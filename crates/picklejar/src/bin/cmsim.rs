//! Count-Min sketch: estimate per-memory access frequency in fixed space.
//!
//! ```text
//! cargo run --release --bin cmsim
//! ```

use std::process::ExitCode;

use picklejar::cmsketch::CountMinSketch;

fn main() -> ExitCode {
    println!("\n=============== COUNT-MIN SKETCH ===============");
    println!("which memories are hot, in fixed space\n");

    let mut cms = CountMinSketch::with_accuracy(0.0005, 0.01);

    // A skewed access stream: a few hot memories, a long cold tail.
    let hot: [(&str, u64); 3] = [
        ("memory:home", 80_000),
        ("memory:profile", 40_000),
        ("memory:billing", 20_000),
    ];
    for &(k, c) in &hot {
        cms.add(k.as_bytes(), c);
    }
    for i in 0..200_000u64 {
        cms.add(&i.to_be_bytes(), 1);
    }
    println!(
        "recorded {} accesses across a skewed stream.\n",
        cms.total()
    );

    let mut ok = true;
    println!("estimating the hot memories' access counts:");
    for &(k, truth) in &hot {
        let est = cms.estimate(k.as_bytes());
        #[allow(clippy::cast_precision_loss)]
        let over = (est - truth) as f64 / truth as f64 * 100.0;
        println!("  {k:<18} true {truth:>6}  est {est:>6}  (+{over:.2}%)");
        if est < truth {
            ok = false;
        }
    }

    println!("\n==================================================");
    if ok {
        println!("VERDICT: every hot memory was estimated at or just above its true count,");
        println!("never below, in a fixed grid of counters that does not grow with the");
        println!("number of distinct memories. the minimum across rows squeezes out noise.");
    } else {
        println!("VERDICT: an estimate fell below the truth; something is wrong.");
        return ExitCode::FAILURE;
    }
    println!("==================================================\n");
    ExitCode::SUCCESS
}
