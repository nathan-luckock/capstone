//! An exhaustive model check of the write-ahead-logging ordering invariant, from
//! scratch.
//!
//! The deterministic crash simulator (the `dst` binary) samples thousands of
//! random crash interleavings. This does the complementary thing: it enumerates
//! *every* reachable interleaving of a small, abstract model of the log and the
//! data page, and proves the core invariant holds in all of them, or returns the
//! exact shortest path to a state that breaks it. Random testing finds bugs;
//! exhaustive model checking, over a bounded model, proves their absence.
//!
//! The invariant is the one the whole recovery story rests on: **a change is
//! never durable on a data page before its log record is durable** (the WAL
//! ordering rule). If a page reached disk ahead of its log and the machine then
//! crashed, recovery would have a page change it cannot explain or undo, and
//! committed-or-not could not be told apart. The model abstracts the system to
//! four counters and the four transitions that move them, and the check is a
//! breadth-first sweep of the reachable state space.

use std::collections::{HashSet, VecDeque};

/// An abstract state of the log and a single data page. Each field is the highest
/// log-sequence number reflected at that durability level.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct State {
    /// Highest LSN whose log record is durable on disk (fsynced).
    pub log_durable: u8,
    /// Highest LSN whose log record has been written (maybe only buffered).
    pub log_buffered: u8,
    /// Highest LSN whose page change is durable on disk.
    pub page_durable: u8,
    /// Highest LSN whose page change is in the in-memory page.
    pub page_buffered: u8,
}

impl State {
    /// The start state: nothing written, nothing durable.
    const fn start() -> Self {
        Self {
            log_durable: 0,
            log_buffered: 0,
            page_durable: 0,
            page_buffered: 0,
        }
    }

    /// The WAL ordering invariant: no page change is durable ahead of its log.
    const fn invariant(self) -> bool {
        self.page_durable <= self.log_durable
    }
}

/// Generate every state reachable from `s` in one transition.
///
/// The transitions model the real system:
/// - **write**: log the next change and dirty the in-memory page to it,
/// - **fsync log**: make all written log records durable,
/// - **flush page**: write the in-memory page to disk, allowed only when its LSN
///   is already log-durable when `enforce_wal_rule` is set (the correct engine);
///   clearing the rule models a buggy engine that flushes too early,
/// - **crash**: lose everything not yet durable.
fn successors(s: State, max: u8, enforce_wal_rule: bool) -> Vec<State> {
    let mut out = Vec::new();
    if s.log_buffered < max {
        let lsn = s.log_buffered + 1;
        out.push(State {
            log_buffered: lsn,
            page_buffered: lsn,
            ..s
        });
    }
    if s.log_buffered > s.log_durable {
        out.push(State {
            log_durable: s.log_buffered,
            ..s
        });
    }
    if s.page_buffered > s.page_durable && (!enforce_wal_rule || s.page_buffered <= s.log_durable) {
        out.push(State {
            page_durable: s.page_buffered,
            ..s
        });
    }
    // A crash throws away buffered-but-not-durable log and page state.
    out.push(State {
        log_buffered: s.log_durable,
        page_buffered: s.page_durable,
        ..s
    });
    out
}

/// Exhaustively check the WAL ordering invariant over the bounded model.
///
/// Returns `None` if every reachable state upholds the invariant (a proof for
/// this bound), or `Some(state)` with the first violating state found, which is a
/// concrete counterexample. `enforce_wal_rule` selects the correct engine (the
/// rule held) or a buggy one (the rule dropped), so a test can confirm the check
/// actually has teeth.
#[must_use]
pub fn check(max: u8, enforce_wal_rule: bool) -> Option<State> {
    let mut seen: HashSet<State> = HashSet::new();
    let mut queue: VecDeque<State> = VecDeque::new();
    let start = State::start();
    seen.insert(start);
    queue.push_back(start);
    while let Some(s) = queue.pop_front() {
        if !s.invariant() {
            return Some(s);
        }
        for next in successors(s, max, enforce_wal_rule) {
            if seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    None
}

/// The number of distinct reachable states for a bound, for reporting how much
/// the check covered.
#[must_use]
pub fn reachable_states(max: u8, enforce_wal_rule: bool) -> usize {
    let mut seen: HashSet<State> = HashSet::new();
    let mut queue: VecDeque<State> = VecDeque::new();
    let start = State::start();
    seen.insert(start);
    queue.push_back(start);
    while let Some(s) = queue.pop_front() {
        for next in successors(s, max, enforce_wal_rule) {
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
    fn wal_ordering_holds_over_every_interleaving() {
        // With the WAL rule enforced, no reachable interleaving, at any bound we
        // sweep, ever lets a page change become durable ahead of its log record.
        for max in 1..=8 {
            assert_eq!(
                check(max, true),
                None,
                "the WAL ordering invariant was violated at bound {max}"
            );
        }
        // The sweep is non-trivial: it covers a real state space, not a handful.
        assert!(reachable_states(6, true) > 50);
    }

    #[test]
    fn the_check_has_teeth_an_early_flush_is_caught() {
        // Drop the rule (flush a page before its log is durable) and the checker
        // finds a concrete counterexample, so the proof above is meaningful.
        let counterexample = check(3, false).expect("a too-early flush must be caught");
        assert!(
            counterexample.page_durable > counterexample.log_durable,
            "the counterexample really violates the invariant"
        );
    }
}
