use fetchpath_metalink::PieceMap;
use sha2::{Digest, Sha256};

/// Domain separation for the piece-map construction. Changing this string
/// changes every piece-map identity, so it is versioned deliberately.
const PIECE_MAP_DOMAIN: &[u8] = b"fetchpath-piecemap-v1";

/// A trusted content identity.
///
/// The variant is part of the identity. A flat digest over the bytes and a
/// digest over a trusted piece map describe the same bytes by different
/// constructions and are never interchangeable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ContentId {
    /// SHA-256 over the whole artifact, as stated by trusted metadata.
    FlatSha256([u8; 32]),
    /// SHA-256 over the canonical encoding of a trusted piece map.
    PieceMapSha256([u8; 32]),
}

impl ContentId {
    pub fn algorithm_label(&self) -> &'static str {
        match self {
            Self::FlatSha256(_) => "sha256",
            Self::PieceMapSha256(_) => "pieces-sha256",
        }
    }

    fn digest(&self) -> &[u8; 32] {
        match self {
            Self::FlatSha256(bytes) | Self::PieceMapSha256(bytes) => bytes,
        }
    }

    pub fn hex(&self) -> String {
        use std::fmt::Write;

        let mut out = String::with_capacity(64);
        for byte in self.digest() {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    pub fn render(&self) -> String {
        format!("{}:{}", self.algorithm_label(), self.hex())
    }

    pub fn parse(text: &str) -> Option<Self> {
        let (label, hex) = text.split_once(':')?;
        let digest = decode_hex32(hex)?;
        match label {
            "sha256" => Some(Self::FlatSha256(digest)),
            "pieces-sha256" => Some(Self::PieceMapSha256(digest)),
            _ => None,
        }
    }

    /// Builds a flat identity from a whole-file digest supplied by trusted
    /// metadata. Returns `None` for anything that is not 64 hex characters.
    pub fn from_expected_sha256(hex: &str) -> Option<Self> {
        decode_hex32(hex).map(Self::FlatSha256)
    }

    /// Builds an identity over a trusted piece map. Piece length, total size,
    /// piece count and the ordered piece hashes all contribute, so any change
    /// to the trusted input produces a different identity.
    pub fn from_piece_map(map: &PieceMap) -> Self {
        let mut digest = Sha256::new();
        digest.update(PIECE_MAP_DOMAIN);
        digest.update(map.piece_length().to_le_bytes());
        digest.update(map.total_size().to_le_bytes());
        digest.update((map.piece_count() as u64).to_le_bytes());
        for index in 0..map.piece_count() {
            let hash = map.piece_hash(index).expect("index below piece count");
            digest.update(hash);
        }
        Self::PieceMapSha256(digest.finalize().into())
    }
}

fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let bytes = hex.as_bytes();
    let mut out = [0_u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        let high = (bytes[index * 2] as char).to_digit(16)?;
        let low = (bytes[index * 2 + 1] as char).to_digit(16)?;
        *slot = ((high << 4) | low) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_metalink::ParseLimits;

    fn map(piece_length: u64, total: u64, hashes: Vec<[u8; 32]>) -> PieceMap {
        PieceMap::new(piece_length, total, hashes, &ParseLimits::default())
            .expect("valid piece map")
    }

    #[test]
    fn a_flat_digest_and_a_piece_map_digest_are_never_the_same_identity() {
        let raw = [7_u8; 32];
        let flat = ContentId::FlatSha256(raw);
        let pieces = ContentId::PieceMapSha256(raw);

        assert_ne!(flat, pieces);
        assert_ne!(flat.render(), pieces.render());
        assert_eq!(flat.algorithm_label(), "sha256");
        assert_eq!(pieces.algorithm_label(), "pieces-sha256");
        assert_eq!(flat.hex(), pieces.hex());
    }

    #[test]
    fn rendering_round_trips_through_parsing() {
        let id = ContentId::FlatSha256([3_u8; 32]);
        assert_eq!(ContentId::parse(&id.render()), Some(id));

        let pieces = ContentId::PieceMapSha256([9_u8; 32]);
        assert_eq!(ContentId::parse(&pieces.render()), Some(pieces));
    }

    #[test]
    fn malformed_identity_text_is_refused_rather_than_guessed() {
        for text in [
            "",
            "sha256:",
            "sha256:zz",
            "md5:0000000000000000000000000000000000000000000000000000000000000000",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "sha256:00000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(ContentId::parse(text), None, "accepted {text:?}");
        }
    }

    #[test]
    fn an_expected_whole_file_digest_becomes_a_flat_identity() {
        let hex = "a".repeat(64);
        let id = ContentId::from_expected_sha256(&hex).expect("valid digest");
        assert_eq!(id, ContentId::FlatSha256([0xaa; 32]));

        assert_eq!(ContentId::from_expected_sha256("a"), None);
        assert_eq!(ContentId::from_expected_sha256(&"g".repeat(64)), None);
    }

    #[test]
    fn piece_map_identity_changes_when_any_trusted_input_changes() {
        let base = ContentId::from_piece_map(&map(4, 8, vec![[1_u8; 32], [2_u8; 32]]));

        let reordered = ContentId::from_piece_map(&map(4, 8, vec![[2_u8; 32], [1_u8; 32]]));
        assert_ne!(base, reordered, "piece order must change identity");

        let different_length = ContentId::from_piece_map(&map(8, 16, vec![[1_u8; 32], [2_u8; 32]]));
        assert_ne!(base, different_length, "piece length must change identity");

        let same = ContentId::from_piece_map(&map(4, 8, vec![[1_u8; 32], [2_u8; 32]]));
        assert_eq!(base, same, "identical trusted input must be stable");
    }
}
