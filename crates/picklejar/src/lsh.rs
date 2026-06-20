//! Hyperplane LSH: a cheap prefilter that lands similar embeddings in one bucket.
//!
//! Exact nearest-neighbor search scans every vector; the HNSW index avoids that
//! with a graph. Locality-sensitive hashing offers a third, complementary tool: a
//! hash that similar vectors tend to share. Each of `b` random hyperplanes splits
//! space in two, and a vector's code is the `b` sign bits of which side it lands
//! on. Because two vectors agree on a given bit with probability proportional to
//! the angle between them, near vectors get codes a small Hamming distance apart
//! and collide in the same bucket, while far ones scatter. Bucketing by code
//! turns a similarity search into a hash lookup over a handful of candidates.

use std::collections::HashMap;

/// A bank of random hyperplanes that hashes a vector to a `b`-bit code.
#[derive(Clone, Debug)]
pub struct Lsh {
    planes: Vec<Vec<f32>>,
}

struct Rng(u64);
impl Rng {
    #[allow(clippy::cast_precision_loss)]
    fn signed(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        let u = (x >> 40) as f32 / 16_777_216.0;
        u.mul_add(2.0, -1.0)
    }
}

impl Lsh {
    /// A hash with `bits` random hyperplanes over `dims`-dimensional vectors
    /// (`bits` at most 64).
    ///
    /// # Panics
    /// Panics if `bits` is zero or above 64.
    #[must_use]
    pub fn new(dims: usize, bits: u32, seed: u64) -> Self {
        assert!((1..=64).contains(&bits), "bits in 1..=64");
        let mut rng = Rng(seed | 1);
        let planes = (0..bits)
            .map(|_| (0..dims).map(|_| rng.signed()).collect())
            .collect();
        Self { planes }
    }

    /// The LSH code of a vector: one sign bit per hyperplane.
    #[must_use]
    pub fn code(&self, v: &[f32]) -> u64 {
        let mut bits = 0u64;
        for (i, plane) in self.planes.iter().enumerate() {
            let dot: f32 = plane.iter().zip(v).map(|(a, b)| a * b).sum();
            if dot >= 0.0 {
                bits |= 1u64 << i;
            }
        }
        bits
    }
}

/// The Hamming distance between two codes (number of differing bits).
#[must_use]
pub const fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// An LSH-bucketed index mapping codes to the ids that hash to them.
#[derive(Clone, Debug)]
pub struct LshIndex {
    lsh: Lsh,
    buckets: HashMap<u64, Vec<u64>>,
}

impl LshIndex {
    /// A new index over the given hash.
    #[must_use]
    pub fn new(lsh: Lsh) -> Self {
        Self {
            lsh,
            buckets: HashMap::new(),
        }
    }

    /// Index a vector under its code.
    pub fn insert(&mut self, id: u64, vector: &[f32]) {
        let code = self.lsh.code(vector);
        self.buckets.entry(code).or_default().push(id);
    }

    /// Candidate ids that share a bucket with `query`.
    #[must_use]
    pub fn candidates(&self, query: &[f32]) -> &[u64] {
        let code = self.lsh.code(query);
        self.buckets.get(&code).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn near_vectors_share_more_bits_than_far_ones() {
        let lsh = Lsh::new(16, 64, 0xABCD);
        let base: Vec<f32> = (0..16).map(|i| (i as f32 * 0.3).sin()).collect();
        let near: Vec<f32> = base.iter().map(|x| x + 0.01).collect();
        let far: Vec<f32> = base.iter().map(|x| -x).collect(); // opposite direction

        let cb = lsh.code(&base);
        let h_near = hamming(cb, lsh.code(&near));
        let h_far = hamming(cb, lsh.code(&far));
        assert!(
            h_near < h_far,
            "near {h_near} should share more bits than far {h_far}"
        );
        assert!(
            h_near <= 3,
            "a tiny perturbation should rarely flip bits, was {h_near}"
        );
    }

    #[test]
    fn the_code_is_deterministic() {
        let lsh = Lsh::new(8, 32, 7);
        let v = [0.1, -0.2, 0.3, 0.4, -0.5, 0.6, 0.7, -0.8];
        assert_eq!(lsh.code(&v), lsh.code(&v));
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn a_cluster_lands_in_one_bucket() {
        let lsh = Lsh::new(8, 12, 99);
        let mut idx = LshIndex::new(lsh);
        // A tight cluster around a center.
        let center = [1.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        for id in 0..10u64 {
            let jitter = id as f32 * 0.001;
            let v: Vec<f32> = center.iter().map(|x| x + jitter).collect();
            idx.insert(id, &v);
        }
        // A query near the center retrieves the cluster members.
        let cand = idx.candidates(&center);
        assert!(
            cand.len() >= 8,
            "most of the cluster should share the bucket, got {}",
            cand.len()
        );
    }
}
