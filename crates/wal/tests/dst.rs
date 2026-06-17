//! Deterministic simulation testing: sweep many seeds and assert every one
//! recovers to a consistent committed state.
//!
//! Each seed is a fully reproducible crash-and-recover scenario (random
//! workload, random durable/lost split, a crash, then ARIES recovery over the
//! surviving durable image plus the WAL). A failure prints its seed, which
//! `cargo run --bin dst -- --seed <n>` replays exactly.
//!
//! This is the regression guard for a real bug the simulator found: an aborted
//! transaction whose in-memory rollback was lost in the crash could resurrect
//! its row, because recovery skips undo for a transaction that already logged
//! `Abort`. The fix makes `abort` log its rollback as CLRs so redo reproduces
//! it. See [`rustdb_wal::sim`].

use rustdb_wal::run_seed;

#[test]
fn many_seeds_recover_consistently() {
    // A broad sweep, kept quick enough for CI. The `dst` binary runs far more
    // for deeper exploration.
    let seeds = 0..512u64;
    for seed in seeds {
        if let Err(e) = run_seed(seed) {
            panic!("deterministic simulation failed: {e}\nreproduce: cargo run --bin dst -- --seed {seed}");
        }
    }
}

#[test]
fn specific_seeds_that_exercise_aborts_and_in_flight_txns() {
    // A handful of named seeds, so a regression in one is obvious in isolation.
    for seed in [2u64, 7, 13, 42, 99, 256, 1000, 9999] {
        run_seed(seed).unwrap_or_else(|e| panic!("seed {seed} regressed: {e}"));
    }
}
