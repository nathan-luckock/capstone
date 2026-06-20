//! Count-Min sketch: estimate how often each memory is accessed, in fixed space.
//!
//! A node wants to know which memories are hot, to cache or tier them, but a hash
//! map of exact counters grows with the number of distinct memories. A Count-Min
//! sketch keeps a small fixed grid of counters and adds each access to one cell
//! per row, chosen by an independent hash. To read a count back, it takes the
//! minimum across the rows: collisions can only push a counter up, so the minimum
//! is the tightest estimate and is never below the truth. Frequent items are
//! estimated tightly; the error is bounded by a small fraction of the total
//! traffic.

use crate::authmem::sha256;

/// Two independent 64-bit hashes for double hashing.
fn base_hashes(key: &[u8]) -> (u64, u64) {
    let d = sha256::hash(key);
    let h1 = u64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]);
    let h2 = u64::from_be_bytes([d[8], d[9], d[10], d[11], d[12], d[13], d[14], d[15]]);
    (h1, h2 | 1)
}

/// A Count-Min sketch over byte-string keys.
#[derive(Clone, Debug)]
pub struct CountMinSketch {
    rows: usize,
    width: u64,
    counters: Vec<u64>,
    total: u64,
}

impl CountMinSketch {
    /// Size a sketch for error `epsilon` (as a fraction of total count) at
    /// confidence `1 - delta`.
    ///
    /// # Panics
    /// Panics if `epsilon` or `delta` is not in `(0, 1)`.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    pub fn with_accuracy(epsilon: f64, delta: f64) -> Self {
        assert!(epsilon > 0.0 && epsilon < 1.0, "epsilon in (0, 1)");
        assert!(delta > 0.0 && delta < 1.0, "delta in (0, 1)");
        let width = (std::f64::consts::E / epsilon).ceil().max(2.0) as u64;
        let rows = (1.0 / delta).ln().ceil().max(1.0) as usize;
        Self {
            rows,
            width,
            counters: vec![0u64; rows * width as usize],
            total: 0,
        }
    }

    #[allow(clippy::cast_possible_truncation)] // col reduced mod width
    const fn col(&self, row: usize, h1: u64, h2: u64) -> usize {
        let probe = h1.wrapping_add((row as u64).wrapping_mul(h2)) % self.width;
        row * self.width as usize + probe as usize
    }

    /// Record `count` accesses of `key`.
    pub fn add(&mut self, key: &[u8], count: u64) {
        let (h1, h2) = base_hashes(key);
        for row in 0..self.rows {
            let cell = self.col(row, h1, h2);
            self.counters[cell] += count;
        }
        self.total += count;
    }

    /// Estimate the number of accesses of `key`. Never below the true count.
    #[must_use]
    pub fn estimate(&self, key: &[u8]) -> u64 {
        let (h1, h2) = base_hashes(key);
        (0..self.rows)
            .map(|row| self.counters[self.col(row, h1, h2)])
            .min()
            .unwrap_or(0)
    }

    /// The total number of accesses recorded.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_underestimates() {
        let mut cms = CountMinSketch::with_accuracy(0.001, 0.01);
        // Record known frequencies.
        for i in 0..1000u64 {
            cms.add(&i.to_be_bytes(), i % 7 + 1);
        }
        for i in 0..1000u64 {
            let truth = i % 7 + 1;
            assert!(
                cms.estimate(&i.to_be_bytes()) >= truth,
                "{i}: estimate must not be below {truth}"
            );
        }
    }

    #[test]
    fn heavy_hitters_are_estimated_tightly() {
        let mut cms = CountMinSketch::with_accuracy(0.001, 0.01);
        // A skewed stream: one very hot memory, lots of cold noise.
        cms.add(b"hot", 100_000);
        for i in 0..50_000u64 {
            cms.add(&i.to_be_bytes(), 1);
        }
        let est = cms.estimate(b"hot");
        // Error bounded by epsilon * total.
        #[allow(clippy::cast_precision_loss)]
        let bound = 0.001 * cms.total() as f64;
        assert!(est >= 100_000, "must not underestimate");
        #[allow(clippy::cast_precision_loss)]
        let over = est as f64 - 100_000.0;
        assert!(
            over <= bound,
            "overestimate {over} should be within {bound}"
        );
    }

    #[test]
    fn an_unseen_key_estimates_low() {
        let mut cms = CountMinSketch::with_accuracy(0.01, 0.01);
        for i in 0..100u64 {
            cms.add(&i.to_be_bytes(), 1);
        }
        // A never-added key reads as a small overcount at most.
        assert!(cms.estimate(b"never-added") <= 2);
    }
}
