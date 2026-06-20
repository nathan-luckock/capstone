//! Roaring-style compressed bitmap: compact sets of memory ids with fast set ops.
//!
//! Query plans and tag indexes constantly manipulate sets of row ids: the rows
//! matching one predicate intersected with another, unioned with a third. A plain
//! bitset is fast but wastes space when ids are sparse; a hash set is compact when
//! sparse but slow to intersect. A roaring bitmap takes the best of both by
//! splitting the 32-bit id space into 16-bit chunks and storing each chunk in
//! whichever container is smaller: a sorted array of the present low bits when the
//! chunk is sparse, or a dense bitset when it is full. Set operations run chunk by
//! chunk, so a union or intersection only touches the chunks that actually hold
//! data.

use std::collections::BTreeMap;

/// Above this many entries, a chunk switches from a sorted array to a bitset.
const ARRAY_MAX: usize = 4096;
/// 65536 bits per chunk = 1024 u64 words.
const WORDS: usize = 1024;

#[derive(Clone, Debug)]
enum Container {
    Array(Vec<u16>),
    Bits(Vec<u64>),
}

impl Container {
    fn add(&mut self, lo: u16) {
        match self {
            Self::Array(v) => {
                if let Err(pos) = v.binary_search(&lo) {
                    v.insert(pos, lo);
                    if v.len() > ARRAY_MAX {
                        let mut bits = vec![0u64; WORDS];
                        for &x in v.iter() {
                            bits[x as usize / 64] |= 1u64 << (x % 64);
                        }
                        *self = Self::Bits(bits);
                    }
                }
            }
            Self::Bits(b) => b[lo as usize / 64] |= 1u64 << (lo % 64),
        }
    }

    fn contains(&self, lo: u16) -> bool {
        match self {
            Self::Array(v) => v.binary_search(&lo).is_ok(),
            Self::Bits(b) => b[lo as usize / 64] & (1u64 << (lo % 64)) != 0,
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Array(v) => v.len(),
            Self::Bits(b) => b.iter().map(|w| w.count_ones() as usize).sum(),
        }
    }

    #[allow(clippy::cast_possible_truncation)] // bit index < 65536 fits u16
    fn values(&self) -> Vec<u16> {
        match self {
            Self::Array(v) => v.clone(),
            Self::Bits(b) => {
                let mut out = Vec::new();
                for (w, &word) in b.iter().enumerate() {
                    let mut bits = word;
                    while bits != 0 {
                        let bit = bits.trailing_zeros();
                        out.push((w * 64 + bit as usize) as u16);
                        bits &= bits - 1;
                    }
                }
                out
            }
        }
    }
}

/// A compressed bitmap over 32-bit ids.
#[derive(Clone, Debug, Default)]
pub struct RoaringBitmap {
    chunks: BTreeMap<u16, Container>,
}

#[allow(clippy::cast_possible_truncation)] // hi/lo splits are exact by construction
const fn split(x: u32) -> (u16, u16) {
    ((x >> 16) as u16, (x & 0xffff) as u16)
}

impl RoaringBitmap {
    /// An empty bitmap.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an id.
    pub fn add(&mut self, id: u32) {
        let (hi, lo) = split(id);
        self.chunks
            .entry(hi)
            .or_insert_with(|| Container::Array(Vec::new()))
            .add(lo);
    }

    /// Whether `id` is present.
    #[must_use]
    pub fn contains(&self, id: u32) -> bool {
        let (hi, lo) = split(id);
        self.chunks.get(&hi).is_some_and(|c| c.contains(lo))
    }

    /// The number of ids in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.chunks.values().map(Container::len).sum()
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.values().all(|c| c.len() == 0)
    }

    /// The union of two bitmaps.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let mut out = self.clone();
        for (hi, c) in &other.chunks {
            for lo in c.values() {
                out.add((u32::from(*hi) << 16) | u32::from(lo));
            }
        }
        out
    }

    /// The intersection of two bitmaps.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let mut out = Self::new();
        for (hi, c) in &self.chunks {
            if let Some(oc) = other.chunks.get(hi) {
                for lo in c.values() {
                    if oc.contains(lo) {
                        out.add((u32::from(*hi) << 16) | u32::from(lo));
                    }
                }
            }
        }
        out
    }

    /// All ids, ascending.
    #[must_use]
    pub fn to_vec(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.len());
        for (hi, c) in &self.chunks {
            for lo in c.values() {
                out.push((u32::from(*hi) << 16) | u32::from(lo));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn add_and_contains() {
        let mut b = RoaringBitmap::new();
        b.add(1);
        b.add(70_000);
        b.add(1_000_000);
        assert!(b.contains(1) && b.contains(70_000) && b.contains(1_000_000));
        assert!(!b.contains(2));
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn dense_chunk_becomes_a_bitset() {
        let mut b = RoaringBitmap::new();
        // Fill one chunk densely to force the array->bitset switch.
        for i in 0..10_000u32 {
            b.add(i);
        }
        assert_eq!(b.len(), 10_000);
        for i in 0..10_000u32 {
            assert!(b.contains(i));
        }
    }

    #[test]
    fn set_ops_match_a_naive_set() {
        let mut a = RoaringBitmap::new();
        let mut b = RoaringBitmap::new();
        let mut sa = HashSet::new();
        let mut sb = HashSet::new();
        let mut rng = 0x1234u64;
        for _ in 0..20_000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let x = (rng % 200_000) as u32;
            if rng & 1 == 0 {
                a.add(x);
                sa.insert(x);
            } else {
                b.add(x);
                sb.insert(x);
            }
        }
        let union: HashSet<u32> = a.union(&b).to_vec().into_iter().collect();
        let expect_u: HashSet<u32> = sa.union(&sb).copied().collect();
        assert_eq!(union, expect_u, "union must match");

        let inter: HashSet<u32> = a.intersect(&b).to_vec().into_iter().collect();
        let expect_i: HashSet<u32> = sa.intersection(&sb).copied().collect();
        assert_eq!(inter, expect_i, "intersection must match");
    }

    #[test]
    fn to_vec_is_sorted() {
        let mut b = RoaringBitmap::new();
        for x in [500_000u32, 3, 70_000, 1, 70_001] {
            b.add(x);
        }
        assert_eq!(b.to_vec(), vec![1, 3, 70_000, 70_001, 500_000]);
    }
}
