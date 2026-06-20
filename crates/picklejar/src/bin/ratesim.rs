//! Token-bucket rate limiting: a tenant bursts, gets throttled, and recovers.
//!
//! ```text
//! cargo run --release --bin ratesim
//! ```

use std::process::ExitCode;

use picklejar::ratelimit::TokenBucket;

fn main() -> ExitCode {
    println!("\n=============== TOKEN-BUCKET RATE LIMIT ===============");
    println!("a brief burst is fine; a sustained flood is held to the drip rate\n");

    // 20-request burst capacity, refilling 5 requests per tick.
    let mut bucket = TokenBucket::new(20.0, 5.0);

    // A quiet tenant suddenly fires 30 requests at once (t=0).
    let burst_ok = (0..30).filter(|_| bucket.try_acquire(0, 1.0)).count();
    println!(
        "t=0: tenant fires 30 requests at once -> {burst_ok} allowed, {} throttled",
        30 - burst_ok
    );
    println!("     (the 20-token burst is spent; the rest are shed)\n");

    // Then it settles into a steady 5/tick, which is exactly sustainable.
    let mut steady_allowed = 0;
    for t in 1..=10u64 {
        steady_allowed += (0..5).filter(|_| bucket.try_acquire(t, 1.0)).count();
    }
    println!("t=1..10: tenant sends 5/tick (the drip rate) -> {steady_allowed}/50 allowed");

    // A greedy tenant asking for 10/tick gets held to 5.
    let mut greedy_allowed = 0;
    let mut greedy = TokenBucket::new(20.0, 5.0);
    for _ in 0..20 {
        greedy.try_acquire(0, 1.0); // drain the burst first
    }
    for t in 1..=10u64 {
        greedy_allowed += (0..10).filter(|_| greedy.try_acquire(t, 1.0)).count();
    }
    println!("greedy tenant asking 10/tick over 10 ticks -> {greedy_allowed}/100 allowed (held to ~5/tick)");

    println!("\n==================================================");
    if burst_ok == 20 && steady_allowed == 50 && greedy_allowed == 50 {
        println!("VERDICT: the burst was capped at 20, the drip-rate traffic all passed, and");
        println!("the greedy tenant was held to the 5/tick refill. one tenant cannot starve");
        println!("the shared node, yet a quiet tenant keeps its burst headroom.");
    } else {
        println!("VERDICT: unexpected (burst={burst_ok}, steady={steady_allowed}, greedy={greedy_allowed}).");
        return ExitCode::FAILURE;
    }
    println!("==================================================\n");
    ExitCode::SUCCESS
}
