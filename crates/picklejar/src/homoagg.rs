//! Private aggregates: SUM, COUNT, and AVG over values no server can read.
//!
//! A fleet operator wants the total or average of a sensitive column (salaries,
//! latencies, balances) without any single storage node ever seeing an
//! individual value. Additive secret sharing makes that exact tradeoff. Each
//! value is split into `n` shares that are individually uniform and reveal
//! nothing, but sum (with wraparound) back to the value. Spread one share per
//! node across `n` non-colluding nodes, and an aggregate is computed by each node
//! summing its own shares and the client adding the partial sums. The scheme is
//! additively homomorphic: the sum of the shares is the share of the sum, so the
//! total comes out exactly while every individual value stays hidden.
//!
//! Honest scope: this is secure aggregation by additive secret sharing over the
//! ring of 64-bit integers, secure against any `n - 1` colluding nodes. It is the
//! aggregate, not arbitrary computation; novel here as an AI-memory feature.

/// A small deterministic generator for reproducible shares (a real split draws
/// from entropy).
struct Rng(u64);
impl Rng {
    #[allow(clippy::cast_possible_wrap)] // a full-range i64 share is intended
    fn next_i64(&mut self) -> i64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x as i64
    }
}

/// Split `value` into `n` additive shares that wrap-sum back to it. The first
/// `n - 1` shares are uniform and independent of the value.
///
/// # Panics
/// Panics if `n == 0`.
#[must_use]
pub fn split(value: i64, n: usize, seed: u64) -> Vec<i64> {
    assert!(n >= 1, "need at least one share");
    let mut rng = Rng(seed | 1);
    let mut shares = Vec::with_capacity(n);
    let mut acc = 0i64;
    for _ in 0..n - 1 {
        let s = rng.next_i64();
        acc = acc.wrapping_add(s);
        shares.push(s);
    }
    // The last share absorbs the value so the shares wrap-sum to it.
    shares.push(value.wrapping_sub(acc));
    shares
}

/// Reconstruct a value from its shares (wrapping sum).
#[must_use]
pub fn reconstruct(shares: &[i64]) -> i64 {
    shares.iter().fold(0i64, |a, &s| a.wrapping_add(s))
}

/// A column secret-shared across `n` servers: `servers[s][row]` is server `s`'s
/// share of row `row`.
#[derive(Clone, Debug)]
pub struct SharedColumn {
    servers: Vec<Vec<i64>>,
}

impl SharedColumn {
    /// Share `values` across `n` servers.
    ///
    /// # Panics
    /// Panics if `n == 0`.
    #[must_use]
    pub fn share(values: &[i64], n: usize, seed: u64) -> Self {
        assert!(n >= 1, "need at least one server");
        let mut servers = vec![Vec::with_capacity(values.len()); n];
        for (row, &v) in values.iter().enumerate() {
            let parts = split(
                v,
                n,
                seed.wrapping_add(row as u64).wrapping_mul(0x9E37_79B9),
            );
            for (s, part) in parts.into_iter().enumerate() {
                servers[s].push(part);
            }
        }
        Self { servers }
    }

    /// The number of servers.
    #[must_use]
    pub fn server_count(&self) -> usize {
        self.servers.len()
    }

    /// What server `s` holds for `row` (uniform; reveals nothing alone).
    #[must_use]
    pub fn share_at(&self, server: usize, row: usize) -> i64 {
        self.servers[server][row]
    }

    /// Server `s`'s partial sum over the given rows. Each server runs this on its
    /// own shares and returns only the partial.
    #[must_use]
    pub fn server_partial(&self, server: usize, rows: &[usize]) -> i64 {
        rows.iter()
            .fold(0i64, |a, &r| a.wrapping_add(self.servers[server][r]))
    }

    /// The SUM over `rows`, reconstructed from every server's partial.
    #[must_use]
    pub fn sum(&self, rows: &[usize]) -> i64 {
        (0..self.servers.len()).fold(0i64, |a, s| a.wrapping_add(self.server_partial(s, rows)))
    }

    /// The COUNT over `rows` (public; the row identities are not the secret).
    #[must_use]
    pub const fn count(&self, rows: &[usize]) -> usize {
        let _ = self;
        rows.len()
    }

    /// The AVG over `rows`, or `None` for an empty set.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn avg(&self, rows: &[usize]) -> Option<f64> {
        if rows.is_empty() {
            return None;
        }
        Some(self.sum(rows) as f64 / rows.len() as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_reconstruct_the_value() {
        for &v in &[0i64, 1, -1, 42, 1_000_000, i64::MAX, i64::MIN] {
            for n in 1..=5 {
                assert_eq!(reconstruct(&split(v, n, 7)), v, "v={v} n={n}");
            }
        }
    }

    #[test]
    fn a_single_server_share_is_independent_of_the_value() {
        // For a fixed seed, server 0's share is the same regardless of the value,
        // so server 0 learns nothing about it.
        assert_eq!(split(100, 3, 99)[0], split(-7777, 3, 99)[0]);
    }

    #[test]
    fn sum_matches_the_plaintext() {
        let values: Vec<i64> = (1..=1000).map(|i| i * 37 - 500).collect();
        let col = SharedColumn::share(&values, 4, 0xABCD);
        let rows: Vec<usize> = (0..values.len()).collect();
        let plaintext: i64 = values.iter().fold(0i64, |a, &v| a.wrapping_add(v));
        assert_eq!(col.sum(&rows), plaintext);
    }

    #[test]
    fn sum_over_a_subset_matches() {
        let values: Vec<i64> = vec![10, 20, 30, 40, 50];
        let col = SharedColumn::share(&values, 3, 1);
        let rows = vec![0, 2, 4]; // 10 + 30 + 50 = 90
        assert_eq!(col.sum(&rows), 90);
        assert_eq!(col.count(&rows), 3);
        assert!((col.avg(&rows).unwrap() - 30.0).abs() < 1e-9);
    }

    #[test]
    fn no_single_server_holds_the_value() {
        let values = vec![123_456i64];
        let col = SharedColumn::share(&values, 3, 5);
        for s in 0..col.server_count() {
            assert_ne!(
                col.share_at(s, 0),
                123_456,
                "no server holds the cleartext value"
            );
        }
        // But together they reconstruct it.
        let shares: Vec<i64> = (0..col.server_count())
            .map(|s| col.share_at(s, 0))
            .collect();
        assert_eq!(reconstruct(&shares), 123_456);
    }
}
