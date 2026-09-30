//! The bytes no lane has claimed yet, as an ordered set of extents.
//!
//! An extent is `[start, end)`. Extents never overlap, and neighbours are not
//! merged, so an extent keeps its start for as long as it exists. The retry
//! budget of a range is keyed by that start.

use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(crate) struct Frontier {
    extents: BTreeMap<u64, u64>,
}

impl Frontier {
    pub(crate) fn new(start: u64, end: u64) -> Self {
        let mut frontier = Self::default();
        frontier.insert(start, end);
        frontier
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.extents.is_empty()
    }

    /// Unclaimed bytes in all.
    pub(crate) fn bytes(&self) -> u64 {
        self.extents.iter().map(|(start, end)| end - start).sum()
    }

    /// The first unclaimed extent.
    pub(crate) fn front(&self) -> Option<(u64, u64)> {
        self.extents
            .first_key_value()
            .map(|(&start, &end)| (start, end))
    }

    /// Adds `[start, end)`. Panics when it overlaps an extent already held,
    /// since that would mean a byte is owned twice.
    pub(crate) fn insert(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        if let Some((_, &before_end)) = self.extents.range(..=start).next_back() {
            assert!(
                before_end <= start,
                "frontier extent overlaps its neighbour"
            );
        }
        if let Some((&after_start, _)) = self.extents.range(start..).next() {
            assert!(after_start >= end, "frontier extent overlaps its neighbour");
        }
        self.extents.insert(start, end);
    }

    /// Takes up to `max` bytes from the front extent.
    pub(crate) fn take_front(&mut self, max: u64) -> Option<(u64, u64)> {
        let (start, end) = self.front()?;
        let taken_end = end.min(start.saturating_add(max.max(1)));
        self.extents.remove(&start);
        if taken_end < end {
            self.extents.insert(taken_end, end);
        }
        Some((start, taken_end))
    }

    /// Takes bytes that begin exactly at `start`, up to `limit_end`, and
    /// returns where they end. `None` when no extent begins at `start`.
    pub(crate) fn take_at(&mut self, start: u64, limit_end: u64) -> Option<u64> {
        let &end = self.extents.get(&start)?;
        let taken_end = end.min(limit_end);
        if taken_end <= start {
            return None;
        }
        self.extents.remove(&start);
        if taken_end < end {
            self.extents.insert(taken_end, end);
        }
        Some(taken_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taking_from_the_front_splits_the_extent_and_loses_nothing() {
        let mut frontier = Frontier::new(100, 1_000);
        let mut covered = Vec::new();
        while let Some((start, end)) = frontier.take_front(300) {
            covered.push((start, end));
        }
        assert_eq!(covered, vec![(100, 400), (400, 700), (700, 1_000)]);
        assert!(frontier.is_empty());
    }

    #[test]
    fn take_at_only_matches_an_extent_that_begins_there() {
        let mut frontier = Frontier::new(0, 100);
        assert_eq!(frontier.take_at(10, 50), None);
        assert_eq!(frontier.take_at(0, 40), Some(40));
        assert_eq!(frontier.front(), Some((40, 100)));
        assert_eq!(frontier.take_at(40, 1_000), Some(100));
        assert!(frontier.is_empty());
    }

    #[test]
    fn a_returned_range_goes_back_in_order() {
        let mut frontier = Frontier::new(500, 900);
        frontier.insert(100, 200);
        assert_eq!(frontier.front(), Some((100, 200)));
        assert_eq!(frontier.bytes(), 500);
    }

    #[test]
    #[should_panic(expected = "overlaps")]
    fn an_overlapping_insert_is_refused() {
        let mut frontier = Frontier::new(0, 100);
        frontier.insert(50, 150);
    }

    /// A pseudo-random walk of claims, splits and returns: every byte is
    /// handed out exactly once in the end.
    #[test]
    fn every_byte_is_claimed_exactly_once() {
        let total = 10_000_u64;
        let mut frontier = Frontier::new(0, total);
        let mut done = vec![0_u8; total as usize];
        let mut seed = 0x2545_f491_u64;
        let mut in_flight: Vec<(u64, u64)> = Vec::new();
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        while !frontier.is_empty() || !in_flight.is_empty() {
            match next() % 3 {
                0 => {
                    if let Some(claim) = frontier.take_front(1 + next() % 900) {
                        in_flight.push(claim);
                    }
                }
                1 if !in_flight.is_empty() => {
                    // A lane finishes part of its claim and fails: the rest returns.
                    let index = (next() as usize) % in_flight.len();
                    let (start, end) = in_flight.swap_remove(index);
                    let cut = start + (next() % (end - start + 1));
                    for byte in start..cut {
                        done[byte as usize] += 1;
                    }
                    frontier.insert(cut, end);
                }
                _ if !in_flight.is_empty() => {
                    let index = (next() as usize) % in_flight.len();
                    let (start, end) = in_flight.swap_remove(index);
                    for byte in start..end {
                        done[byte as usize] += 1;
                    }
                }
                _ => {}
            }
        }
        assert!(done.iter().all(|&count| count == 1));
    }
}
