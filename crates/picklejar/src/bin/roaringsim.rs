//! Roaring bitmap: compact id sets with chunk-wise union and intersection.
//!
//! ```text
//! cargo run --release --bin roaringsim
//! ```

use std::process::ExitCode;

use picklejar::roaring::RoaringBitmap;

fn main() -> ExitCode {
    println!("\n=============== ROARING BITMAP ===============");
    println!("compact id sets that intersect and union chunk by chunk\n");

    // Predicate A: a dense range of ids (rows 0..50000). Predicate B: a sparse
    // scattering. Both are memory-id sets a query plan would combine.
    let mut a = RoaringBitmap::new();
    for i in 0..50_000u32 {
        a.add(i);
    }
    let mut b = RoaringBitmap::new();
    let mut rng = 0xABCDu64;
    for _ in 0..5000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        b.add((rng % 100_000) as u32);
    }
    println!("set A: {} dense ids (0..50000)", a.len());
    println!("set B: {} sparse scattered ids in 0..100000", b.len());

    let union = a.union(&b);
    let inter = a.intersect(&b);
    println!("\n  A union B:     {} ids", union.len());
    println!("  A intersect B: {} ids", inter.len());

    // Verify the intersection: every element is in both, by construction.
    let inter_ok = inter
        .to_vec()
        .iter()
        .all(|&x| a.contains(x) && b.contains(x));
    // Every B id below 50000 should be in the intersection.
    let b_low = b.to_vec().iter().filter(|&&x| x < 50_000).count();

    println!("  intersection members are all in both: {inter_ok}");
    println!(
        "  (B ids below 50000: {b_low}, matching the intersection size {})",
        inter.len()
    );

    println!("\n==================================================");
    if inter_ok && inter.len() == b_low && union.len() >= a.len() {
        println!("VERDICT: the dense set stayed a packed bitset and the sparse one a sorted");
        println!("array, yet union and intersection combined them correctly chunk by chunk.");
        println!("this is how a query plan ANDs and ORs row-id sets without materializing them.");
    } else {
        println!(
            "VERDICT: unexpected (inter_ok={inter_ok}, inter={}, b_low={b_low}).",
            inter.len()
        );
        return ExitCode::FAILURE;
    }
    println!("==================================================\n");
    ExitCode::SUCCESS
}
