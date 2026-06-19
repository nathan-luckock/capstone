//! An exhaustive model check of the cache-freshness invariant: a query served
//! from the approximate index never returns a row that has been deleted.
//!
//! The sibling of the row-level-security retrieval model (`isolation_model`).
//! Where that one proves the cached index never serves *another tenant's* row,
//! this one proves it never serves a *deleted* row: a memory the agent has
//! forgotten can never resurface through a stale index. Together they cover the
//! cache's whole correctness promise, that it never returns a row it should not.
//!
//! # The mechanism under test
//!
//! The engine invalidates the cached index on every write (insert, update, or
//! delete), so an index-path query always ranks a snapshot no older than the
//! last write. This model proves that invalidation rule is sufficient: across
//! every reachable interleaving of inserts, deletes, index builds, and queries,
//! a query never returns a row that is no longer present.
//!
//! A deliberately buggy variant, where a delete does not invalidate the cache,
//! is caught with a concrete counterexample (a query that serves a deleted row),
//! so the proof is not vacuous. No vector or AI-memory database is known to
//! model-check its index freshness this way.

use std::collections::{HashSet, VecDeque};

/// An abstract state of one table and its cached approximate index.
///
/// Rows are identified by bit position in `present` (a bitmask of the live
/// rows). `cache` is the bitmask the index captured when it was last built, or
/// `None` when no cache is present.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// Bitmask of rows currently present (live) in the table.
    pub present: u8,
    /// The snapshot of `present` the cached index captured, or `None`.
    pub cache: Option<u8>,
    /// Set once a query returns a row that is no longer present.
    pub violated: bool,
}

impl State {
    const fn start() -> Self {
        Self {
            present: 0,
            cache: None,
            violated: false,
        }
    }
}

/// Whether a query in state `s` serves a deleted row: the cache holds a row that
/// is no longer present. With no cache the query reads live rows and cannot.
const fn query_serves_deleted(s: State) -> bool {
    match s.cache {
        // A cached bit that is not in `present` is a deleted row served stale.
        Some(c) => (c & !s.present) != 0,
        None => false,
    }
}

/// Every state reachable from `s` in one step. `correct` selects the engine's
/// rule (every write invalidates the cache) or the buggy rule (a delete leaves
/// the cache in place).
fn successors(s: State, max_rows: u8, correct: bool) -> Vec<State> {
    let mut out = Vec::new();

    for i in 0..max_rows {
        let bit = 1u8 << i;
        // Insert row i if absent. A write always invalidates the cache.
        if s.present & bit == 0 {
            out.push(State {
                present: s.present | bit,
                cache: None,
                ..s
            });
        }
        // Delete row i if present. Correct invalidates the cache; the bug leaves
        // it, so a later query can still serve the deleted row.
        if s.present & bit != 0 {
            out.push(State {
                present: s.present & !bit,
                cache: if correct { None } else { s.cache },
                ..s
            });
        }
    }

    // Build (cache) the approximate index over the currently present rows.
    out.push(State {
        cache: Some(s.present),
        ..s
    });

    // Query: may set the violation flag.
    out.push(State {
        violated: s.violated || query_serves_deleted(s),
        ..s
    });

    out
}

/// Exhaustively check the freshness invariant over the bounded model.
///
/// Returns `None` if no reachable state serves a deleted row (a proof for this
/// bound), or `Some(state)` with the first violating state. `correct` selects the
/// engine's invalidate-on-every-write rule or a buggy rule that skips
/// invalidation on delete, so a test can confirm the check has teeth.
#[must_use]
pub fn check(max_rows: u8, correct: bool) -> Option<State> {
    let mut seen: HashSet<State> = HashSet::new();
    let mut queue: VecDeque<State> = VecDeque::new();
    let start = State::start();
    seen.insert(start);
    queue.push_back(start);
    while let Some(s) = queue.pop_front() {
        if s.violated {
            return Some(s);
        }
        for next in successors(s, max_rows, correct) {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    None
}

/// Distinct reachable states for a bound, for reporting coverage.
#[must_use]
pub fn reachable_states(max_rows: u8, correct: bool) -> usize {
    let mut seen: HashSet<State> = HashSet::new();
    let mut queue: VecDeque<State> = VecDeque::new();
    let start = State::start();
    seen.insert(start);
    queue.push_back(start);
    while let Some(s) = queue.pop_front() {
        for next in successors(s, max_rows, correct) {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    seen.len()
}

#[cfg(test)]
mod tests {
    use super::{check, reachable_states};

    #[test]
    fn freshness_holds_over_every_interleaving() {
        for rows in 1..=5 {
            assert_eq!(
                check(rows, true),
                None,
                "a query served a deleted row at {rows} rows"
            );
        }
        assert!(reachable_states(4, true) > 30);
    }

    #[test]
    fn the_check_has_teeth_a_delete_that_skips_invalidation_is_caught() {
        // If a delete does not invalidate the cache, a query can still serve the
        // deleted row. The check must find that counterexample.
        let counterexample =
            check(2, false).expect("a delete that leaves the cache stale must be caught");
        assert!(counterexample.violated);
    }
}
