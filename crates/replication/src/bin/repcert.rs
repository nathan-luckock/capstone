//! repcert: a regenerable, content-hashed certificate of the replication
//! guarantees, in the spirit of the engine's `vecert` / `attest`.
//!
//! Each check is deterministic, so re-running from the same commit reproduces
//! the same verdict and the same hash.
//!
//! ```text
//! cargo run --release --bin repcert
//! ```

use std::fmt::Write as _;

use picklejar::antientropy::MerkleSet;
use picklejar_replication::{run_seed, Cluster};

/// Thousands of random partition-and-crash schedules all converge.
fn check_convergence(seeds: u64) -> bool {
    (0..seeds).all(|s| run_seed(s, 5, 200).converged)
}

/// Both sides of a partition keep accepting writes, and the cluster reconciles
/// to a single value on heal.
fn check_available_under_partition() -> bool {
    let mut c = Cluster::new(3, 3, 2, 2);
    c.set_partitions(&[0, 1, 1]);
    let left = c.write(0, 5, b"left").accepted();
    let right = c.write(1, 5, b"right").accepted();
    c.heal();
    let _ = c.anti_entropy();
    left && right && c.fully_converged() && c.node(0).get(5) == c.node(2).get(5)
}

/// A crashed node rejoins and anti-entropy catches it up on what it missed.
#[allow(clippy::cast_possible_truncation)]
fn check_crash_and_rejoin() -> bool {
    let mut c = Cluster::new(5, 3, 2, 2);
    for k in 0..30u64 {
        c.write((k % 5) as usize, k, &k.to_le_bytes());
    }
    let _ = c.anti_entropy();
    c.crash(2);
    for k in 30..60u64 {
        c.write(0, k, &k.to_le_bytes());
    }
    c.restart(2);
    let _ = c.anti_entropy();
    c.fully_converged() && c.node(2).get(45) == Some(45u64.to_le_bytes().as_slice())
}

/// Repairing a one-key divergence ships about one slot, not the whole store.
fn check_repair_is_diff_sized() -> bool {
    let mut c = Cluster::new(2, 2, 1, 1);
    for k in 0..200u64 {
        c.write(0, k, &k.to_le_bytes());
    }
    let _ = c.anti_entropy();
    c.set_partitions(&[0, 1]);
    c.write(1, 5, b"changed");
    c.heal();
    let transfers = c.anti_entropy();
    c.fully_converged() && transfers <= 2
}

fn main() {
    let seeds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let checks: Vec<(String, bool)> = vec![
        (
            format!("convergence under partition + crash ({seeds} seeds, 5 nodes)"),
            check_convergence(seeds),
        ),
        (
            "availability under partition, reconciled on heal".to_string(),
            check_available_under_partition(),
        ),
        (
            "crash and rejoin caught up by anti-entropy".to_string(),
            check_crash_and_rejoin(),
        ),
        (
            "repair ships only the diverged keys".to_string(),
            check_repair_is_diff_sized(),
        ),
    ];

    // A content hash over the check names and outcomes, regenerable from source.
    let entries: Vec<(u64, Vec<u8>)> = checks
        .iter()
        .enumerate()
        .map(|(i, (name, ok))| {
            let mut bytes = name.clone().into_bytes();
            bytes.push(u8::from(*ok));
            (u64::try_from(i).unwrap_or(0), bytes)
        })
        .collect();
    let root = MerkleSet::from_entries(10, &entries).root();
    let hash = root.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });

    println!("============ PICKLEJAR REPLICATION CERTIFICATE ============");
    let passed = checks.iter().filter(|(_, ok)| *ok).count();
    for (name, ok) in &checks {
        println!("  [{}] {name}", if *ok { "PASS" } else { "FAIL" });
    }
    println!();
    println!("certificate hash: {}", &hash[..16]);
    println!("VERDICT: {passed}/{} checks passed", checks.len());
    println!("==========================================================");

    if passed != checks.len() {
        std::process::exit(1);
    }
}
