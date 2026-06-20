//! Token-bucket rate limiting: fair, burst-tolerant per-tenant request limits.
//!
//! A shared memory node has to protect itself from any one tenant flooding it
//! with requests, but a hard "N per second" cap is both too strict (it forbids a
//! brief, harmless burst) and awkward to reason about. A token bucket fits the
//! shape better: tokens drip into a bucket at a steady rate up to a cap, and each
//! request spends one. A tenant that has been quiet accumulates a burst it can
//! spend at once, while sustained traffic is held to the drip rate. Everything
//! here is driven by an explicit logical clock, so the behavior is exactly
//! reproducible and testable.

/// A single token bucket.
#[derive(Clone, Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_rate: f64,
    tokens: f64,
    last: u64,
}

impl TokenBucket {
    /// A bucket that holds up to `capacity` tokens and refills `refill_rate`
    /// tokens per tick, starting full.
    ///
    /// # Panics
    /// Panics if `capacity` or `refill_rate` is not positive.
    #[must_use]
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        assert!(
            capacity > 0.0 && refill_rate > 0.0,
            "capacity and rate must be positive"
        );
        Self {
            capacity,
            refill_rate,
            tokens: capacity,
            last: 0,
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn refill(&mut self, now: u64) {
        if now > self.last {
            let elapsed = (now - self.last) as f64;
            self.tokens = elapsed
                .mul_add(self.refill_rate, self.tokens)
                .min(self.capacity);
            self.last = now;
        }
    }

    /// Try to spend `cost` tokens at logical time `now`. Returns whether the
    /// request is allowed.
    pub fn try_acquire(&mut self, now: u64, cost: f64) -> bool {
        self.refill(now);
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }

    /// The tokens available as of time `now`.
    pub fn available(&mut self, now: u64) -> f64 {
        self.refill(now);
        self.tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_up_to_capacity_is_allowed_then_throttled() {
        let mut b = TokenBucket::new(10.0, 1.0);
        // Ten immediate requests at t=0 succeed, the eleventh is throttled.
        for _ in 0..10 {
            assert!(b.try_acquire(0, 1.0));
        }
        assert!(!b.try_acquire(0, 1.0), "burst beyond capacity is denied");
    }

    #[test]
    fn tokens_refill_over_time() {
        let mut b = TokenBucket::new(10.0, 1.0);
        for _ in 0..10 {
            b.try_acquire(0, 1.0);
        }
        assert!(!b.try_acquire(0, 1.0));
        // After 5 ticks, 5 tokens are back.
        assert!(b.try_acquire(5, 5.0), "five tokens should have refilled");
        assert!(!b.try_acquire(5, 1.0), "but no more than five");
    }

    #[test]
    fn refill_never_exceeds_capacity() {
        let mut b = TokenBucket::new(10.0, 1.0);
        b.try_acquire(0, 10.0); // empty it
                                // Idle for a long time: tokens cap at capacity, not 1000.
        assert!((b.available(1000) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn sustained_rate_is_held_to_the_drip() {
        let mut b = TokenBucket::new(5.0, 1.0);
        // Drain the initial burst.
        for _ in 0..5 {
            assert!(b.try_acquire(0, 1.0));
        }
        // Then exactly one request per tick is sustainable.
        let mut allowed = 0;
        for t in 1..=100u64 {
            if b.try_acquire(t, 1.0) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, 100, "one per tick at the drip rate");
    }
}
