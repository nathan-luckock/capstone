//! Measure the engine's detection coverage across the four storage-write fault
//! classes: bit flip, torn write, lost write, and misdirected write. Each is
//! injected into well-formed pages and run through the engine's layered
//! page-integrity check (the payload checksum, the self-identifying page-id
//! guard, then the LSN-versus-log guard). All four are detected in full.
//!
//! ```text
//! cargo run --release --bin faultsim          # 2000 trials per class
//! cargo run --release --bin faultsim -- 50000 # 50k trials per class
//! ```

use std::process::ExitCode;

use picklejar::faults::run_fault_coverage;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let per_class: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2000);

    let cov = run_fault_coverage(0xFA17, per_class);
    println!("storage-fault detection coverage ({per_class} trials per class)\n");
    println!(
        "  bit flip          {:>6.1}%  (payload checksum)",
        cov.bit_flip * 100.0
    );
    println!(
        "  torn write        {:>6.1}%  (payload checksum)",
        cov.torn_write * 100.0
    );
    println!(
        "  lost write        {:>6.1}%  (LSN-versus-log guard)",
        cov.lost_write * 100.0
    );
    println!(
        "  misdirected write {:>6.1}%  (self-identifying page-id guard)",
        cov.misdirected_write * 100.0
    );

    // All four classes must now be caught completely; the page-id guard closed
    // the misdirected-write residual the LSN guard alone left open.
    if cov.bit_flip >= 1.0
        && cov.torn_write >= 1.0
        && cov.lost_write >= 1.0
        && cov.misdirected_write >= 1.0
    {
        println!("\nresult: all four storage-write fault classes fully detected");
        ExitCode::SUCCESS
    } else {
        eprintln!("\nresult: a fault class regressed below full detection");
        ExitCode::FAILURE
    }
}
