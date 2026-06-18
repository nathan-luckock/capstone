//! Deterministic simulation runner for the vector memory layer.
//!
//! Each seed drives one fully reproducible crash-and-recover scenario through
//! the real engine (see [`picklejar::vecsim`]), proving that committed
//! embeddings survive intact (durability) and that each tenant sees only its own
//! after recovery (isolation). This binary sweeps a range of seeds and reports
//! the first that breaks an invariant, so it can be replayed exactly.
//!
//! ```text
//! cargo run --release --bin vecsim                 # 1000 seeds from 0
//! cargo run --release --bin vecsim -- 100000       # 100k seeds from 0
//! cargo run --release --bin vecsim -- --seed 42    # replay one seed, verbose
//! ```

use std::process::ExitCode;

use picklejar::vecsim::run_seed;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.get(1).map(String::as_str) == Some("--seed") {
        let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        return match run_seed(seed) {
            Ok(o) => {
                println!(
                    "seed {seed}: OK  tenants={} committed={} rolled_back={}",
                    o.tenants, o.committed, o.rolled_back
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("FAIL {e}");
                ExitCode::FAILURE
            }
        };
    }

    let count: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    println!("running {count} deterministic vector durability+isolation simulations...");
    let mut committed_total = 0u64;
    for seed in 0..count {
        match run_seed(seed) {
            Ok(o) => committed_total += o.committed as u64,
            Err(e) => {
                eprintln!("FAIL {e}");
                eprintln!("reproduce with: cargo run --bin vecsim -- --seed {seed}");
                return ExitCode::FAILURE;
            }
        }
        if count >= 1000 && (seed + 1) % (count / 10).max(1) == 0 {
            println!("  {}/{count} seeds passed", seed + 1);
        }
    }
    println!(
        "all {count} seeds recovered with isolation intact ({committed_total} embeddings verified)"
    );
    ExitCode::SUCCESS
}
