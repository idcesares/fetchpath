use crate::ContentId;

/// Where an entry's bytes came from, decided once at insertion.
///
/// This is the whole basis on which content may leave the machine, so it is
/// never recomputed and never mutated. An entry that touched credentials stays
/// `Credentialed` for as long as it exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Provenance {
    /// Fetched with no cookies, no referer, and a delivering URL carrying no
    /// query string.
    Public,
    /// Anything else.
    Credentialed,
}

impl Provenance {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Credentialed => "credentialed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "public" => Some(Self::Public),
            "credentialed" => Some(Self::Credentialed),
            _ => None,
        }
    }
}

/// How the bytes were proven when they were inserted.
///
/// There is deliberately no `Unverified` variant: content without a trusted
/// digest is not eligible for this cache, so it cannot be represented here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachedVerification {
    PieceHashes,
    FinalHashOnly,
}

impl CachedVerification {
    pub fn label(&self) -> &'static str {
        match self {
            Self::PieceHashes => "piece_hashes",
            Self::FinalHashOnly => "final_hash_only",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "piece_hashes" => Some(Self::PieceHashes),
            "final_hash_only" => Some(Self::FinalHashOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheEntry {
    pub id: ContentId,
    pub bytes: u64,
    pub inserted_at_secs: u64,
    pub last_used_at_secs: u64,
    pub verification: CachedVerification,
    pub provenance: Provenance,
}

impl CacheEntry {
    /// True only for credential-free entries. A trusted digest alone is not
    /// permission to redistribute an object.
    pub fn is_shareable(&self) -> bool {
        matches!(self.provenance, Provenance::Public)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(provenance: Provenance) -> CacheEntry {
        CacheEntry {
            id: ContentId::FlatSha256([1_u8; 32]),
            bytes: 10,
            inserted_at_secs: 100,
            last_used_at_secs: 100,
            verification: CachedVerification::FinalHashOnly,
            provenance,
        }
    }

    #[test]
    fn only_credential_free_entries_are_ever_shareable() {
        assert!(entry(Provenance::Public).is_shareable());
        assert!(!entry(Provenance::Credentialed).is_shareable());
    }

    #[test]
    fn provenance_labels_round_trip_and_reject_anything_else() {
        for value in [Provenance::Public, Provenance::Credentialed] {
            assert_eq!(Provenance::parse(value.label()), Some(value));
        }
        assert_eq!(Provenance::parse("shareable"), None);
        assert_eq!(Provenance::parse(""), None);
    }

    #[test]
    fn verification_labels_round_trip_and_reject_unverified() {
        for value in [
            CachedVerification::PieceHashes,
            CachedVerification::FinalHashOnly,
        ] {
            assert_eq!(CachedVerification::parse(value.label()), Some(value));
        }
        // Unverified content is not eligible for the cache, so it has no label.
        assert_eq!(CachedVerification::parse("unverified"), None);
    }
}
