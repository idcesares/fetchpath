//! The encrypted stream a share link serves (FP-117, framing of
//! docs/development/SHARE-RELAY.md).
//!
//! `"FPS1"`, version 1, chunk size log2, a 16-byte salt: the 22-byte header,
//! sent in the clear and bound as associated data of every chunk. The key is
//! HKDF-SHA256 over the link's fragment key with the header's salt, 39 bytes:
//! an AES-256-GCM key and a 7-byte nonce prefix. A chunk's nonce is the
//! prefix, its index as u32 big-endian and a last-chunk byte, so truncation,
//! reordering and a swapped header fail authentication.
//!
//! The plain stream is `name length (u16 BE) || name || file bytes`, so the
//! file name travels only inside the ciphertext.

use aes_gcm::aead::{Aead, KeyInit, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key};
use hkdf::Hkdf;
use sha2_11::Sha256;
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 4] = b"FPS1";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 22;
pub const TAG_LEN: usize = 16;
/// The chunk size the engine writes (64 KiB, as measured in FP-116).
pub const CHUNK_LOG2: u8 = 16;
/// What a reader accepts from the unauthenticated header.
pub const MIN_CHUNK_LOG2: u8 = 16;
pub const MAX_CHUNK_LOG2: u8 = 22;
const INFO: &[u8] = b"fetchpath-share-v1";

pub struct Frame {
    cipher: Aes256Gcm,
    prefix: [u8; 7],
    header: [u8; HEADER_LEN],
    chunk: usize,
}

impl Frame {
    /// `None` for a chunk size outside what readers accept.
    pub fn new(key: &[u8; 32], salt: [u8; 16], chunk_log2: u8) -> Option<Self> {
        if !(MIN_CHUNK_LOG2..=MAX_CHUNK_LOG2).contains(&chunk_log2) {
            return None;
        }
        let mut header = [0_u8; HEADER_LEN];
        header[..4].copy_from_slice(MAGIC);
        header[4] = VERSION;
        header[5] = chunk_log2;
        header[6..].copy_from_slice(&salt);
        let mut okm = Zeroizing::new([0_u8; 39]);
        Hkdf::<Sha256>::new(Some(&salt), key)
            .expand(INFO, okm.as_mut_slice())
            .expect("39 bytes is a valid HKDF-SHA256 output length");
        let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(
            <[u8; 32]>::try_from(&okm[..32]).expect("fixed-size slice"),
        ));
        let prefix = <[u8; 7]>::try_from(&okm[32..]).expect("fixed-size slice");
        Some(Self {
            cipher,
            prefix,
            header,
            chunk: 1 << chunk_log2,
        })
    }

    pub fn header(&self) -> &[u8; HEADER_LEN] {
        &self.header
    }

    pub fn chunk_size(&self) -> usize {
        self.chunk
    }

    fn nonce(&self, index: u32, last: bool) -> Nonce<Aes256Gcm> {
        let mut bytes = [0_u8; 12];
        bytes[..7].copy_from_slice(&self.prefix);
        bytes[7..11].copy_from_slice(&index.to_be_bytes());
        bytes[11] = u8::from(last);
        Nonce::<Aes256Gcm>::from(bytes)
    }

    /// One chunk: `ciphertext || tag`.
    pub fn seal(&self, index: u32, last: bool, plain: &[u8]) -> Vec<u8> {
        debug_assert!(plain.len() <= self.chunk);
        self.cipher
            .encrypt(
                &self.nonce(index, last),
                Payload {
                    msg: plain,
                    aad: &self.header,
                },
            )
            .expect("AES-GCM encrypts a chunk-sized message")
    }

    /// The plain chunk, or `None` when it does not authenticate.
    pub fn open(&self, index: u32, last: bool, sealed: &[u8]) -> Option<Vec<u8>> {
        self.cipher
            .decrypt(
                &self.nonce(index, last),
                Payload {
                    msg: sealed,
                    aad: &self.header,
                },
            )
            .ok()
    }
}

/// Where each byte of the encrypted stream is, from the plain length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Layout {
    pub plain_len: u64,
    chunk: u64,
}

impl Layout {
    /// `None` when the stream would need more chunks than an index holds.
    pub fn new(plain_len: u64, chunk_log2: u8) -> Option<Self> {
        let layout = Self {
            plain_len,
            chunk: 1 << chunk_log2,
        };
        (layout.chunks() <= u64::from(u32::MAX)).then_some(layout)
    }

    /// An empty stream is one empty chunk, never a header alone.
    pub fn chunks(&self) -> u64 {
        self.plain_len.div_ceil(self.chunk).max(1)
    }

    pub fn cipher_len(&self) -> u64 {
        HEADER_LEN as u64 + self.plain_len + TAG_LEN as u64 * self.chunks()
    }

    /// The plain bytes of chunk `index`.
    pub fn plain_range(&self, index: u64) -> (u64, u64) {
        let start = index * self.chunk;
        (start, self.plain_len.min(start + self.chunk))
    }

    /// Where chunk `index` starts in the encrypted stream.
    pub fn cipher_start(&self, index: u64) -> u64 {
        HEADER_LEN as u64 + index * (self.chunk + TAG_LEN as u64)
    }

    /// The chunk holding encrypted byte `offset`, for `offset` past the
    /// header and inside the stream.
    pub fn chunk_at(&self, offset: u64) -> u64 {
        (offset - HEADER_LEN as u64) / (self.chunk + TAG_LEN as u64)
    }

    pub fn is_last(&self, index: u64) -> bool {
        index + 1 == self.chunks()
    }
}

/// The plain stream's first bytes: the name, length first.
pub fn meta(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let mut meta = Vec::with_capacity(2 + bytes.len());
    meta.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    meta.extend_from_slice(bytes);
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7; 32];
    const SALT: [u8; 16] = [9; 16];

    /// The whole stream for `plain`, as the engine writes it.
    pub(crate) fn encode(plain: &[u8]) -> Vec<u8> {
        let frame = Frame::new(&KEY, SALT, MIN_CHUNK_LOG2).unwrap();
        let layout = Layout::new(plain.len() as u64, MIN_CHUNK_LOG2).unwrap();
        let mut out = frame.header().to_vec();
        for index in 0..layout.chunks() {
            let (from, to) = layout.plain_range(index);
            out.extend(frame.seal(
                index as u32,
                layout.is_last(index),
                &plain[from as usize..to as usize],
            ));
        }
        assert_eq!(out.len() as u64, layout.cipher_len());
        out
    }

    /// What a reader must do: refuse anything but a stream that ends
    /// exactly after one chunk that authenticates as last.
    fn decode(stream: &[u8]) -> Option<Vec<u8>> {
        let (header, mut rest) = stream.split_at_checked(HEADER_LEN)?;
        if &header[..4] != MAGIC || header[4] != VERSION {
            return None;
        }
        let log2 = header[5];
        let frame = Frame::new(&KEY, header[6..].try_into().ok()?, log2)?;
        let full = (1_usize << log2) + TAG_LEN;
        let mut out = Vec::new();
        for index in 0_u32.. {
            let last = rest.len() <= full;
            let (sealed, tail) = rest.split_at(rest.len().min(full));
            out.extend(frame.open(index, last, sealed)?);
            if last {
                return tail.is_empty().then_some(out);
            }
            rest = tail;
        }
        None
    }

    fn sample(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn streams_of_every_edge_length_round_trip() {
        let chunk = 1 << MIN_CHUNK_LOG2;
        for len in [0, 1, chunk - 1, chunk, chunk + 1, 3 * chunk] {
            let plain = sample(len);
            assert_eq!(decode(&encode(&plain)), Some(plain), "length {len}");
        }
    }

    #[test]
    fn an_empty_stream_is_one_empty_chunk_not_a_header_alone() {
        let stream = encode(&[]);
        assert_eq!(stream.len(), HEADER_LEN + TAG_LEN);
        assert_eq!(decode(&stream[..HEADER_LEN]), None);
    }

    #[test]
    fn truncation_trailing_bytes_and_reordering_fail() {
        let chunk = 1 << MIN_CHUNK_LOG2;
        let stream = encode(&sample(3 * chunk));
        let full = chunk + TAG_LEN;
        // Cut at a chunk boundary: the new final chunk was sealed as not last.
        assert_eq!(decode(&stream[..HEADER_LEN + 2 * full]), None);
        // Cut inside a chunk.
        assert_eq!(decode(&stream[..stream.len() - 1]), None);
        // Bytes after the last chunk.
        let mut longer = stream.clone();
        longer.push(0);
        assert_eq!(decode(&longer), None);
        // Chunks 0 and 1 swapped.
        let mut swapped = stream[..HEADER_LEN].to_vec();
        swapped.extend(&stream[HEADER_LEN + full..HEADER_LEN + 2 * full]);
        swapped.extend(&stream[HEADER_LEN..HEADER_LEN + full]);
        swapped.extend(&stream[HEADER_LEN + 2 * full..]);
        assert_eq!(decode(&swapped), None);
    }

    #[test]
    fn a_changed_header_fails_every_chunk() {
        let mut stream = encode(&sample(100));
        stream[5] = 17;
        assert_eq!(decode(&stream), None);
    }

    #[test]
    fn chunk_sizes_outside_the_accepted_range_are_refused() {
        assert!(Frame::new(&KEY, SALT, MIN_CHUNK_LOG2 - 1).is_none());
        assert!(Frame::new(&KEY, SALT, MAX_CHUNK_LOG2 + 1).is_none());
    }

    #[test]
    fn the_layout_matches_what_is_written() {
        let chunk = 1 << MIN_CHUNK_LOG2;
        for len in [0_u64, 1, chunk, chunk + 1] {
            let layout = Layout::new(len, MIN_CHUNK_LOG2).unwrap();
            for index in 0..layout.chunks() {
                assert_eq!(layout.chunk_at(layout.cipher_start(index)), index);
            }
            assert_eq!(
                layout.cipher_len(),
                encode(&sample(len as usize)).len() as u64
            );
        }
    }

    /// Fixed vectors the page script is checked against
    /// (`apps/cli/src/share/page.test.mjs`).
    #[test]
    fn the_shared_vectors_are_what_the_engine_writes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/share-vectors.json");
        let text = std::fs::read_to_string(&path).unwrap();
        let vectors: serde_json::Value = serde_json::from_str(&text).unwrap();
        for case in vectors["cases"].as_array().unwrap() {
            let len = case["plainLen"].as_u64().unwrap() as usize;
            let stream = encode(&sample(len));
            let digest = {
                use sha2::{Digest, Sha256};
                format!("{:x}", Sha256::digest(&stream))
            };
            assert_eq!(case["cipherSha256"].as_str().unwrap(), digest, "{case}");
        }
    }
}
