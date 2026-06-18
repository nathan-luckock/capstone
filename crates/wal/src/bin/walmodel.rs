//! Exhaustively model-check the write-ahead-logging ordering invariant and
//! report. The complement to the `dst` binary's random crash sweep: this proves,
//! over every reachable interleaving of a bounded model, that a page change is
//! never durable ahead of its log record.
//!
//! ```text
//! cargo run --release --bin walmodel        # sweep bounds 1..=8
//! cargo run --release --bin walmodel -- 10  # sweep bounds 1..=10
//! ```

use std::process::ExitCode;

use picklejar_wal::model::{check, reachable_states};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let max: u8 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);

    println!("model-checking the WAL ordering invariant (no page durable ahead of its log)...");
    for bound in 1..=max {
        if let Some(cx) = check(bound, true) {
            eprintln!("VIOLATION at bound {bound}: {cx:?}");
            return ExitCode::FAILURE;
        }
        println!(
            "  bound {bound}: invariant holds over {} reachable states",
            reachable_states(bound, true)
        );
    }

    // Confirm the check has teeth: a buggy engine that flushes a page before its
    // log is durable must be caught, or the proof above would be vacuous.
    if let Some(cx) = check(3, false) {
        println!("  teeth check: a too-early page flush is caught ({cx:?})");
    } else {
        eprintln!("teeth check failed: a known-buggy flush was not caught");
        return ExitCode::FAILURE;
    }

    println!("result: WAL ordering invariant proved over every interleaving up to bound {max}");
    ExitCode::SUCCESS
}
