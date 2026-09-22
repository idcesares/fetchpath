//! Trusted piece hashes and the verification API that localizes damage.
//!
//! A [`PieceMap`] is only ever built from a consistent `(piece length, piece
//! count, total size)` triple, so a caller can map a failing piece index back
//! to an exact byte range and re-fetch just that range. Verification here says
//! whether staged bytes match digests supplied by the metadata author. It says
//! nothing about who published those bytes.

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::{MAX_PIECE_LENGTH, MetalinkError, ParseLimits};

const READ_BUFFER_BYTES: usize = 64 * 1024;

/// An ordered map of trusted SHA-256 piece digests over one file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PieceMap {
    piece_length: u64,
    total_size: u64,
    hashes: Vec<[u8; 32]>,
}

impl PieceMap {
    /// Builds a piece map, refusing any piece count that cannot describe
    /// `total_size` at `piece_length`.
    pub fn new(
        piece_length: u64,
        total_size: u64,
        hashes: Vec<[u8; 32]>,
        limits: &ParseLimits,
    ) -> Result<Self, MetalinkError> {
        let inconsistent = MetalinkError::InconsistentPieces {
            declared_size: Some(total_size),
            piece_length,
            pieces: hashes.len(),
        };
        if piece_length == 0 || piece_length > MAX_PIECE_LENGTH || total_size == 0 {
            return Err(inconsistent);
        }
        if hashes.is_empty() {
            return Err(inconsistent);
        }
        if hashes.len() > limits.max_pieces {
            return Err(MetalinkError::TooManyNodes {
                kind: "piece hashes",
                limit: limits.max_pieces,
            });
        }
        if total_size.div_ceil(piece_length) != hashes.len() as u64 {
            return Err(inconsistent);
        }
        Ok(Self {
            piece_length,
            total_size,
            hashes,
        })
    }

    pub fn piece_length(&self) -> u64 {
        self.piece_length
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    pub fn piece_count(&self) -> usize {
        self.hashes.len()
    }

    /// The exact `(start, length)` byte range covered by one piece.
    pub fn piece_range(&self, index: usize) -> Option<(u64, u64)> {
        if index >= self.hashes.len() {
            return None;
        }
        let start = index as u64 * self.piece_length;
        let length = self.piece_length.min(self.total_size - start);
        Some((start, length))
    }

    /// The trusted digest for one piece, for a caller that hashes the staged
    /// range itself instead of buffering it.
    pub fn piece_hash(&self, index: usize) -> Option<&[u8; 32]> {
        self.hashes.get(index)
    }

    /// Checks one piece's bytes against its trusted digest. A wrong-length
    /// buffer never passes.
    pub fn verify_piece(&self, index: usize, bytes: &[u8]) -> bool {
        let Some((_, length)) = self.piece_range(index) else {
            return false;
        };
        if bytes.len() as u64 != length {
            return false;
        }
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        digest == self.hashes[index]
    }

    /// Reports exactly which pieces of a staged file fail, so the caller can
    /// re-fetch only those byte ranges.
    pub fn verify_file(&self, path: &Path) -> io::Result<PieceVerification> {
        let mut file = File::open(path)?;
        let mut verifier = self.streaming_verifier();
        let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            verifier.update(&buffer[..read]);
        }
        Ok(verifier.finish())
    }

    /// A verifier that checks each piece the moment its bytes complete, so a
    /// hopeless mirror can be abandoned before the whole file arrives.
    pub fn streaming_verifier(&self) -> StreamingPieceVerifier<'_> {
        StreamingPieceVerifier {
            map: self,
            hasher: Sha256::new(),
            index: 0,
            filled: 0,
            observed_size: 0,
            verified: Vec::new(),
            failed: Vec::new(),
        }
    }
}

/// The result of checking a staged file against a piece map.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PieceVerification {
    /// Piece indices whose bytes matched their trusted digest.
    pub verified_pieces: Vec<usize>,
    /// Piece indices whose bytes did not match, including any piece that the
    /// file was too short to cover.
    pub failed_pieces: Vec<usize>,
    pub observed_size: u64,
    pub expected_size: u64,
}

impl PieceVerification {
    /// True only when every piece matched and the file is exactly the declared
    /// size. Nothing may be published on anything weaker.
    pub fn is_complete(&self) -> bool {
        self.failed_pieces.is_empty() && self.observed_size == self.expected_size
    }
}

/// Verifies pieces from a sequential byte stream.
pub struct StreamingPieceVerifier<'a> {
    map: &'a PieceMap,
    hasher: Sha256,
    index: usize,
    filled: u64,
    observed_size: u64,
    verified: Vec<usize>,
    failed: Vec<usize>,
}

impl StreamingPieceVerifier<'_> {
    /// Feeds the next sequential bytes of the file. Bytes past the declared
    /// size are counted but never fold into a piece digest.
    pub fn update(&mut self, mut bytes: &[u8]) {
        self.observed_size += bytes.len() as u64;
        while !bytes.is_empty() {
            let Some((_, length)) = self.map.piece_range(self.index) else {
                return;
            };
            let remaining = (length - self.filled) as usize;
            let take = remaining.min(bytes.len());
            self.hasher.update(&bytes[..take]);
            self.filled += take as u64;
            bytes = &bytes[take..];
            if self.filled == length {
                let digest: [u8; 32] = std::mem::take(&mut self.hasher).finalize().into();
                if digest == self.map.hashes[self.index] {
                    self.verified.push(self.index);
                } else {
                    self.failed.push(self.index);
                }
                self.index += 1;
                self.filled = 0;
            }
        }
    }

    /// Piece indices that have already failed. A caller may stop a mirror early
    /// once this is non-empty.
    pub fn failed_so_far(&self) -> &[usize] {
        &self.failed
    }

    /// Piece indices that have already matched their trusted digest.
    pub fn verified_so_far(&self) -> &[usize] {
        &self.verified
    }

    /// Finishes the stream. Every piece the stream never covered is reported as
    /// failed rather than quietly assumed good.
    pub fn finish(mut self) -> PieceVerification {
        for index in self.index..self.map.piece_count() {
            self.failed.push(index);
        }
        PieceVerification {
            verified_pieces: self.verified,
            failed_pieces: self.failed,
            observed_size: self.observed_size,
            expected_size: self.map.total_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    fn fixture() -> (Vec<u8>, PieceMap) {
        let body: Vec<u8> = (0..2500_u32).map(|index| (index % 251) as u8).collect();
        let hashes = body.chunks(1024).map(digest).collect();
        let map = PieceMap::new(1024, body.len() as u64, hashes, &ParseLimits::default()).unwrap();
        (body, map)
    }

    #[test]
    fn refuses_a_piece_count_inconsistent_with_the_declared_size() {
        let limits = ParseLimits::default();
        let one = vec![digest(b"x")];
        assert!(PieceMap::new(1024, 2500, one.clone(), &limits).is_err());
        assert!(PieceMap::new(0, 1, one.clone(), &limits).is_err());
        assert!(PieceMap::new(1024, 0, one.clone(), &limits).is_err());
        assert!(PieceMap::new(MAX_PIECE_LENGTH + 1, 1, one, &limits).is_err());
        assert!(PieceMap::new(1024, 1024, Vec::new(), &limits).is_err());
        assert!(
            PieceMap::new(
                1024,
                2500,
                vec![digest(b"a"), digest(b"b"), digest(b"c")],
                &limits
            )
            .is_ok()
        );
    }

    #[test]
    fn refuses_more_piece_hashes_than_the_budget() {
        let limits = ParseLimits {
            max_pieces: 2,
            ..ParseLimits::default()
        };
        assert_eq!(
            PieceMap::new(1, 3, vec![digest(b"a"); 3], &limits),
            Err(MetalinkError::TooManyNodes {
                kind: "piece hashes",
                limit: 2
            })
        );
    }

    #[test]
    fn maps_piece_indices_to_exact_byte_ranges() {
        let (_, map) = fixture();
        assert_eq!(map.piece_count(), 3);
        assert_eq!(map.piece_range(0), Some((0, 1024)));
        assert_eq!(map.piece_range(2), Some((2048, 452)));
        assert_eq!(map.piece_range(3), None);
    }

    #[test]
    fn verify_piece_rejects_wrong_bytes_and_wrong_lengths() {
        let (body, map) = fixture();
        assert!(map.verify_piece(0, &body[..1024]));
        assert!(!map.verify_piece(0, &body[..1023]));
        assert!(!map.verify_piece(0, &body[1024..2048]));
        assert!(!map.verify_piece(9, &body[..1024]));
    }

    #[test]
    fn verify_file_localizes_exactly_the_damaged_pieces() {
        let (mut body, map) = fixture();
        let dir = std::env::temp_dir().join(format!("fetchpath-pieces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("staged.bin");

        std::fs::write(&path, &body).unwrap();
        let clean = map.verify_file(&path).unwrap();
        assert!(clean.is_complete());
        assert_eq!(clean.failed_pieces, Vec::<usize>::new());
        assert_eq!(clean.verified_pieces, vec![0, 1, 2]);

        body[1500] ^= 0xff;
        std::fs::write(&path, &body).unwrap();
        let damaged = map.verify_file(&path).unwrap();
        assert!(!damaged.is_complete());
        assert_eq!(damaged.failed_pieces, vec![1]);
        assert_eq!(damaged.verified_pieces, vec![0, 2]);
        assert_eq!(damaged.observed_size, 2500);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_short_file_fails_the_pieces_it_never_covered() {
        let (body, map) = fixture();
        let mut verifier = map.streaming_verifier();
        verifier.update(&body[..1024]);
        assert_eq!(verifier.verified_so_far(), [0]);
        assert!(verifier.failed_so_far().is_empty());
        let report = verifier.finish();
        assert_eq!(report.failed_pieces, vec![1, 2]);
        assert!(!report.is_complete());
    }

    #[test]
    fn a_long_file_fails_the_size_check_even_when_pieces_match() {
        let (mut body, map) = fixture();
        body.extend_from_slice(b"trailing");
        let mut verifier = map.streaming_verifier();
        for chunk in body.chunks(7) {
            verifier.update(chunk);
        }
        let report = verifier.finish();
        assert!(report.failed_pieces.is_empty());
        assert_eq!(report.observed_size, 2508);
        assert!(!report.is_complete());
    }

    #[test]
    fn streaming_verification_is_chunk_boundary_independent() {
        let (mut body, map) = fixture();
        body[10] ^= 0xff;
        for size in [1, 3, 1024, 2500] {
            let mut verifier = map.streaming_verifier();
            for chunk in body.chunks(size) {
                verifier.update(chunk);
            }
            assert_eq!(
                verifier.finish().failed_pieces,
                vec![0],
                "chunk size {size}"
            );
        }
    }
}
