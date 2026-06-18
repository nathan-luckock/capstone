//! Deterministic simulation of the self-healing erasure-coded store under
//! sustained orbital radiation, to show the operational envelope: scrub often
//! enough relative to the orbit's upset rate and the store loses nothing, at a
//! fraction of the mass of redundant hardware.
//!
//! Each simulated day injects that day's expected single-event-upset dose (from
//! the orbit model, over the store's physical footprint) as corrupted shards,
//! spread across the blobs. Every `scrub` days the store is read and healed: a
//! blob with at most `m` corrupt shards is reconstructed from parity, and only a
//! blob that accumulated more than `m` corrupt shards between two scrubs is a real
//! loss. A loss is always detected, never served as a silently wrong answer.
//!
//! ```text
//! cargo run --release --bin resilientsim                       # 10 years LEO, daily scrub
//! cargo run --release --bin resilientsim -- 3650 1 leo 10 4    # days scrub orbit k m
//! ```

use std::collections::HashSet;
use std::process::ExitCode;

use picklejar::radiation::{expected_upsets_per_day, Orbit};
use picklejar_storage::resilient::ResilientStore;

/// A deterministic PRNG so a run replays exactly.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// A deterministic embedding blob for `key`.
fn blob(key: u64) -> Vec<u8> {
    let len = 384 + usize::try_from(key % 9).expect("0..9") * 64;
    (0..len)
        .map(|i| u8::try_from((i as u64 ^ key.wrapping_mul(2_654_435_761)) & 0xFF).expect("masked"))
        .collect()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let days: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3650);
    let scrub: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let orbit = match args.get(3).map(String::as_str) {
        Some("geo" | "GEO") => Orbit::Geo,
        _ => Orbit::Leo,
    };
    let k: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(10);
    let m: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(4);
    let blobs: u64 = 256;

    let Ok(mut store) = ResilientStore::new(k, m) else {
        eprintln!("invalid shape: k={k}, m={m}");
        return ExitCode::FAILURE;
    };
    for key in 0..blobs {
        if store.put(key, &blob(key)).is_err() {
            eprintln!("encode failed");
            return ExitCode::FAILURE;
        }
    }

    let footprint = store.stored_bytes();
    let per_day = expected_upsets_per_day(footprint, orbit);
    let shards = u64::try_from(store.shards_per_blob()).expect("small");

    #[allow(clippy::cast_precision_loss)]
    let years = days as f64 / 365.0;
    println!(
        "simulating {days} orbit-days ({years:.1} years) at {} with scrub every {scrub} day(s)",
        orbit.name()
    );
    println!(
        "  store: {blobs} blobs, k={k}+m={m} shards, {footprint} bytes resident; \
         dose ~{per_day:.4} upsets/day"
    );

    let mut rng = Rng(0x0DD_BA11);
    let mut lost: HashSet<u64> = HashSet::new();
    let mut injected = 0usize;
    let mut silently_wrong = 0usize;
    let mut acc = 0.0f64;

    for day in 0..days {
        acc += per_day;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let today = acc.floor() as u64;
        #[allow(clippy::cast_precision_loss)]
        {
            acc -= today as f64;
        }
        for _ in 0..today {
            if lost.len() as u64 >= blobs {
                break;
            }
            let mut key = rng.below(blobs);
            while lost.contains(&key) {
                key = rng.below(blobs);
            }
            let shard = usize::try_from(rng.below(shards)).expect("small");
            store.corrupt_shard(key, shard, b"single-event upset");
            injected += 1;
        }

        if (day + 1) % scrub == 0 {
            for key in 0..blobs {
                if lost.contains(&key) {
                    continue;
                }
                match store.get(key) {
                    Ok(got) if got == blob(key) => {}
                    Ok(_) => silently_wrong += 1,
                    Err(_) => {
                        lost.insert(key);
                    }
                }
            }
        }
    }

    let repaired = store.fault_log().iter().filter(|e| e.repaired).count();
    #[allow(clippy::cast_precision_loss)]
    let overhead = m as f64 / k as f64 * 100.0;
    println!(
        "  {injected} upsets injected, {repaired} shard repairs from parity, \
         {} blobs lost, {silently_wrong} served silently wrong",
        lost.len()
    );

    if silently_wrong > 0 {
        eprintln!("FAIL: the store served a silently wrong answer, which must never happen");
        return ExitCode::FAILURE;
    }
    if lost.is_empty() {
        println!(
            "result: zero data loss over {years:.1} years at +{overhead:.0}% storage overhead"
        );
    } else {
        println!(
            "result: {} blobs lost (dose outran the scrub cadence); every loss was detected, \
             none served wrong. Scrub more often or raise m.",
            lost.len()
        );
    }
    ExitCode::SUCCESS
}
