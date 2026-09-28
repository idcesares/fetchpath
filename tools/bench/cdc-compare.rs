//! How much of a new version a client already holds in an old one (FP-023).
//!
//! Content-defined chunking (FastCDC with normalized chunking, gear hash,
//! 16 KiB minimum, 64 KiB average, 256 KiB maximum) cuts both files where
//! their content says, not at fixed offsets, so an insertion shifts only the
//! chunks around it. Reports, as one JSON line, the bytes of NEW found as
//! chunks of OLD, the chunking rate, and the chunk list a client would need
//! (a 32-byte hash and a length per chunk). No dependencies:
//!
//!     rustc -O tools/bench/cdc-compare.rs -o work/cdc/cdc-compare.exe
//!     work/cdc/cdc-compare.exe OLD NEW

use std::collections::HashSet;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::Instant;

const MIN: usize = 16 * 1024;
const AVG: usize = 64 * 1024;
const MAX: usize = 256 * 1024;
// Stricter before the average, looser after it (normalized chunking, level 1).
const MASK_SMALL: u64 = (1 << 17) - 1;
const MASK_LARGE: u64 = (1 << 15) - 1;

fn gear() -> [u64; 256] {
    // A fixed pseudo-random table, so every run cuts the same way.
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut table = [0_u64; 256];
    for entry in &mut table {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *entry = state;
    }
    table
}

fn cut(data: &[u8], gear: &[u64; 256]) -> usize {
    if data.len() <= MIN {
        return data.len();
    }
    let end = data.len().min(MAX);
    let middle = end.min(AVG);
    let mut hash = 0_u64;
    for (index, byte) in data.iter().enumerate().take(end).skip(MIN) {
        hash = (hash << 1).wrapping_add(gear[*byte as usize]);
        let mask = if index < middle { MASK_SMALL } else { MASK_LARGE };
        if hash & mask == 0 {
            return index + 1;
        }
    }
    end
}

/// Chunks as (128-bit identity, length). Two SipHash keys, shared by both
/// files, give the identity; a collision in this estimate is negligible.
fn chunks(data: &[u8], gear: &[u64; 256], keys: &[RandomState; 2]) -> Vec<(u128, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let length = cut(&data[at..], gear);
        let piece = &data[at..at + length];
        let mut first = keys[0].build_hasher();
        first.write(piece);
        let mut second = keys[1].build_hasher();
        second.write(piece);
        out.push((((first.finish() as u128) << 64) | second.finish() as u128, length));
        at += length;
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, old, new] = args.as_slice() else {
        eprintln!("usage: cdc-compare OLD NEW");
        std::process::exit(2);
    };
    let old = std::fs::read(old).expect("read OLD");
    let new = std::fs::read(new).expect("read NEW");
    let gear = gear();
    let keys = [RandomState::new(), RandomState::new()];
    let started = Instant::now();
    let old_chunks = chunks(&old, &gear, &keys);
    let new_chunks = chunks(&new, &gear, &keys);
    let seconds = started.elapsed().as_secs_f64();
    let held: HashSet<u128> = old_chunks.iter().map(|(id, _)| *id).collect();
    let reused: usize = new_chunks
        .iter()
        .filter(|(id, _)| held.contains(id))
        .map(|(_, length)| length)
        .sum();
    let metadata = new_chunks.len() * (32 + 8);
    println!(
        "{{\"oldBytes\":{},\"newBytes\":{},\"newChunks\":{},\"reusedBytes\":{},\"reusedShare\":{:.4},\"metadataBytes\":{},\"netSavedBytes\":{},\"chunkingMBps\":{:.1}}}",
        old.len(),
        new.len(),
        new_chunks.len(),
        reused,
        reused as f64 / new.len().max(1) as f64,
        metadata,
        reused as i64 - metadata as i64,
        (old.len() + new.len()) as f64 / 1e6 / seconds.max(1e-9)
    );
}
