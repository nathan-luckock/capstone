//! Counting Bloom filter: a membership set a forgotten memory can leave.
//!
//! A plain Bloom filter can never remove an item, because clearing a bit might
//! erase a bit another item depends on. That is a problem for a memory store
//! where items are forgotten as well as added: the dedup set would drift, holding
//! ghosts of memories that no longer exist. A counting Bloom filter replaces each
//! bit with a small counter, incremented on insert and decremented on remove, so
//! a memory can be taken back out as cleanly as it went in.
//!
//! The cost is a few bytes per cell instead of a bit, and one rule the caller
//! must keep: only remove an item that was actually inserted. Removing a phantom
//! decrements counters it never raised and can introduce a false negative.

use crate::authmem::sha256;

/// Two independent 64-bit hashes for double hashing.
fn base_hashes(key: &[u8]) -> (u64, u64) {
    let d = sha256::hash(key);
    let h1 = u64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]);
    let h2 = u64::from_be_bytes([d[8], d[9], d[10], d[11], d[12], d[13], d[14], d[15]]);
    (h1, h2 | 1)
}

/// A counting Bloom filter over byte-string keys.
#[derive(Clone, Debug)]
pub struct CountingBloom {
    counters: Vec<u8>,
    cells: u64,
    hashes: u32,
}

impl CountingBloom {
    /// Size a filter for `expected` items at a target `fp_rate`.
    ///
    /// # Panics
    /// Panics if `fp_rate` is not in `(0, 1)` or `expected` is zero.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn with_capacity(expected: usize, fp_rate: f64) -> Self {
        assert!(expected > 0, "expected must be positive");
        assert!(fp_rate > 0.0 && fp_rate < 1.0, "fp_rate must be in (0, 1)");
        let n = expected as f64;
        let ln2 = std::f64::consts::LN_2;
        let cells = (-n * fp_rate.ln() / (ln2 * ln2)).ceil().max(64.0) as u64;
        let hashes = ((cells as f64 / n) * ln2).round().clamp(1.0, 32.0) as u32;
        Self {
            counters: vec![0u8; cells as usize],
            cells,
            hashes,
        }
    }

    #[allow(clippy::cast_possible_truncation)] // reduced mod cells, always fits
    fn cell(&self, i: u32, h1: u64, h2: u64) -> usize {
        (h1.wrapping_add(u64::from(i).wrapping_mul(h2)) % self.cells) as usize
    }

    /// Add a memory to the set, saturating counters at 255.
    pub fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = base_hashes(key);
        for i in 0..self.hashes {
            let c = self.cell(i, h1, h2);
            self.counters[c] = self.counters[c].saturating_add(1);
        }
    }

    /// Remove a memory previously inserted. Only call for items that were added.
    pub fn remove(&mut self, key: &[u8]) {
        let (h1, h2) = base_hashes(key);
        for i in 0..self.hashes {
            let c = self.cell(i, h1, h2);
            self.counters[c] = self.counters[c].saturating_sub(1);
        }
    }

    /// Whether `key` is (probably) in the set. A `false` is definitive.
    #[must_use]
    pub fn contains(&self, key: &[u8]) -> bool {
        let (h1, h2) = base_hashes(key);
        (0..self.hashes).all(|i| self.counters[self.cell(i, h1, h2)] > 0)
    }

    /// The number of counter cells.
    #[must_use]
    pub const fn cell_count(&self) -> u64 {
        self.cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_remove_makes_a_memory_absent() {
        let mut cb = CountingBloom::with_capacity(1000, 0.01);
        cb.insert(b"memory-7");
        assert!(cb.contains(b"memory-7"));
        cb.remove(b"memory-7");
        assert!(!cb.contains(b"memory-7"));
    }

    #[test]
    fn removing_one_memory_leaves_the_others() {
        let mut cb = CountingBloom::with_capacity(10_000, 0.01);
        for i in 0..1000u64 {
            cb.insert(&i.to_be_bytes());
        }
        cb.remove(&500u64.to_be_bytes());
        assert!(!cb.contains(&500u64.to_be_bytes()));
        // Every other inserted memory is still present (no false negatives).
        for i in (0..1000u64).filter(|&i| i != 500) {
            assert!(cb.contains(&i.to_be_bytes()), "{i} should remain");
        }
    }

    #[test]
    fn duplicate_inserts_need_matching_removes() {
        // Counting reflects multiplicity: two inserts, one remove, still present.
        let mut cb = CountingBloom::with_capacity(100, 0.01);
        cb.insert(b"m");
        cb.insert(b"m");
        cb.remove(b"m");
        assert!(cb.contains(b"m"), "still present after one of two removes");
        cb.remove(b"m");
        assert!(!cb.contains(b"m"), "absent after the second remove");
    }

    #[test]
    fn no_false_negatives_after_a_batch() {
        let mut cb = CountingBloom::with_capacity(5000, 0.01);
        for i in 0..5000u64 {
            cb.insert(&i.to_be_bytes());
        }
        for i in 0..5000u64 {
            assert!(cb.contains(&i.to_be_bytes()));
        }
    }
}
