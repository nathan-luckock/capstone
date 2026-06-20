//! Counting Bloom filter: a membership set a forgotten memory can leave.
//!
//! ```text
//! cargo run --release --bin countbloomsim
//! ```

use std::process::ExitCode;

use picklejar::countbloom::CountingBloom;

fn main() -> ExitCode {
    println!("\n=============== COUNTING BLOOM FILTER ===============");
    println!("a membership set that supports forgetting\n");

    let mut cb = CountingBloom::with_capacity(10_000, 0.01);
    for i in 0..2000u64 {
        cb.insert(&i.to_be_bytes());
    }
    println!("stored 2000 memories in {} counter cells.", cb.cell_count());

    let forget = [7u64, 42, 1000, 1999];
    for &id in &forget {
        cb.remove(&id.to_be_bytes());
    }
    println!("forgot memories {forget:?} by decrementing their cells.\n");

    let gone = forget
        .iter()
        .filter(|&&id| !cb.contains(&id.to_be_bytes()))
        .count();
    let kept = (0..2000u64)
        .filter(|&i| !forget.contains(&i))
        .filter(|&i| cb.contains(&i.to_be_bytes()))
        .count();

    println!("  forgotten memories now absent: {gone}/{}", forget.len());
    println!(
        "  remaining memories still present: {kept}/{}",
        2000 - forget.len()
    );

    println!("\n==================================================");
    if gone == forget.len() && kept == 2000 - forget.len() {
        println!("VERDICT: forgotten memories left the set cleanly, and every memory that");
        println!("was not forgotten is still present, no false negatives. a plain Bloom");
        println!("filter cannot do this; the counters are what make removal safe.");
    } else {
        println!("VERDICT: unexpected (gone={gone}, kept={kept}).");
        return ExitCode::FAILURE;
    }
    println!("==================================================\n");
    ExitCode::SUCCESS
}
