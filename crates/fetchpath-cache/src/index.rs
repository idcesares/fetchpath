use crate::{CacheEntry, CachedVerification, ContentId, Provenance};
use std::collections::{BTreeMap, BTreeSet};

const INDEX_HEADER: &str = "fetchpath-cache-index";
const INDEX_VERSION: u32 = 1;

/// The accounted contents of the store.
///
/// Decoding is all-or-nothing on purpose. A partially readable index would
/// under-report the accounted total, which would quietly let the store grow
/// past its quota, so a malformed index is refused and the caller rebuilds it
/// from the directory instead.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CacheIndex {
    entries: BTreeMap<ContentId, CacheEntry>,
}

impl CacheIndex {
    pub fn insert(&mut self, entry: CacheEntry) {
        self.entries.insert(entry.id, entry);
    }

    pub fn remove(&mut self, id: &ContentId) -> Option<CacheEntry> {
        self.entries.remove(id)
    }

    pub fn get(&self, id: &ContentId) -> Option<&CacheEntry> {
        self.entries.get(id)
    }

    pub fn touch(&mut self, id: &ContentId, now_secs: u64) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.last_used_at_secs = now_secs;
        }
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    pub fn entries(&self) -> Vec<CacheEntry> {
        self.entries.values().cloned().collect()
    }

    /// The eviction candidate: oldest last-used time, with the content id
    /// breaking ties so the choice is deterministic across runs.
    pub fn least_recently_used(&self, skip: &BTreeSet<ContentId>) -> Option<ContentId> {
        self.entries
            .values()
            .filter(|entry| !skip.contains(&entry.id))
            .min_by_key(|entry| (entry.last_used_at_secs, entry.id))
            .map(|entry| entry.id)
    }

    pub fn encode(&self) -> String {
        let mut out = format!("{INDEX_HEADER} {INDEX_VERSION}\n");
        for entry in self.entries.values() {
            out.push_str(&format!(
                "{} {} {} {} {} {}\n",
                entry.id.render(),
                entry.bytes,
                entry.inserted_at_secs,
                entry.last_used_at_secs,
                entry.verification.label(),
                entry.provenance.label(),
            ));
        }
        out
    }

    pub fn decode(text: &str) -> Option<Self> {
        // Every record ends in a newline, so a truncated final line is
        // detectable rather than silently dropped.
        if !text.ends_with('\n') {
            return None;
        }

        let mut lines = text.lines();
        let header = lines.next()?;
        let (name, version) = header.split_once(' ')?;
        if name != INDEX_HEADER || version.parse::<u32>().ok()? != INDEX_VERSION {
            return None;
        }

        let mut index = Self::default();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let mut fields = line.split(' ');
            let id = ContentId::parse(fields.next()?)?;
            let bytes = fields.next()?.parse::<u64>().ok()?;
            let inserted_at_secs = fields.next()?.parse::<u64>().ok()?;
            let last_used_at_secs = fields.next()?.parse::<u64>().ok()?;
            let verification = CachedVerification::parse(fields.next()?)?;
            let provenance = Provenance::parse(fields.next()?)?;
            if fields.next().is_some() {
                return None;
            }
            index.insert(CacheEntry {
                id,
                bytes,
                inserted_at_secs,
                last_used_at_secs,
                verification,
                provenance,
            });
        }
        Some(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: u8, bytes: u64, last_used: u64) -> CacheEntry {
        CacheEntry {
            id: ContentId::FlatSha256([tag; 32]),
            bytes,
            inserted_at_secs: 1,
            last_used_at_secs: last_used,
            verification: CachedVerification::PieceHashes,
            provenance: Provenance::Public,
        }
    }

    #[test]
    fn an_index_round_trips_through_its_encoding() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));

        let decoded = CacheIndex::decode(&index.encode()).expect("decodes");
        assert_eq!(decoded.entries(), index.entries());
        assert_eq!(decoded.total_bytes(), 30);
    }

    #[test]
    fn a_malformed_index_is_refused_so_the_caller_can_rebuild_it() {
        for text in [
            "",
            "garbage",
            "fetchpath-cache-index 99\n",
            "fetchpath-cache-index 1\nsha256:zz 10 1 1 piece_hashes public\n",
            "fetchpath-cache-index 1\nsha256:aa 10 1 1 unverified public\n",
        ] {
            assert!(CacheIndex::decode(text).is_none(), "accepted {text:?}");
        }
    }

    #[test]
    fn a_truncated_final_line_does_not_silently_lose_bytes() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));
        let encoded = index.encode();
        let truncated = &encoded[..encoded.len() - 5];

        assert!(
            CacheIndex::decode(truncated).is_none(),
            "a truncated index must be refused, not partially trusted"
        );
    }

    #[test]
    fn touching_an_entry_updates_only_its_last_used_time() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.touch(&ContentId::FlatSha256([1; 32]), 500);

        let stored = index.get(&ContentId::FlatSha256([1; 32])).expect("present");
        assert_eq!(stored.last_used_at_secs, 500);
        assert_eq!(stored.inserted_at_secs, 1);
        assert_eq!(stored.bytes, 10);
    }

    #[test]
    fn least_recently_used_skips_the_entries_it_is_told_to_skip() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));
        index.insert(entry(3, 30, 300));

        let none_skipped = BTreeSet::new();
        assert_eq!(
            index.least_recently_used(&none_skipped),
            Some(ContentId::FlatSha256([1; 32]))
        );

        let mut skip = BTreeSet::new();
        skip.insert(ContentId::FlatSha256([1; 32]));
        assert_eq!(
            index.least_recently_used(&skip),
            Some(ContentId::FlatSha256([2; 32]))
        );

        skip.insert(ContentId::FlatSha256([2; 32]));
        skip.insert(ContentId::FlatSha256([3; 32]));
        assert_eq!(index.least_recently_used(&skip), None);
    }

    #[test]
    fn removing_an_entry_returns_it_and_reduces_the_accounted_total() {
        let mut index = CacheIndex::default();
        index.insert(entry(1, 10, 100));
        index.insert(entry(2, 20, 200));

        let removed = index
            .remove(&ContentId::FlatSha256([1; 32]))
            .expect("present");
        assert_eq!(removed.bytes, 10);
        assert_eq!(index.total_bytes(), 20);
        assert_eq!(index.remove(&ContentId::FlatSha256([1; 32])), None);
    }
}
