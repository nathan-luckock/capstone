//! Private aggregates: SUM and AVG over salaries no server can read.
//!
//! ```text
//! cargo run --release --bin homoaggsim
//! ```

use std::process::ExitCode;

use picklejar::homoagg::SharedColumn;

fn main() -> ExitCode {
    println!("\n=============== PRIVATE AGGREGATES ===============");
    println!("SUM and AVG over values no single server can see\n");

    // A sensitive column: 200 salaries.
    let salaries: Vec<i64> = (0..200).map(|i| 60_000 + (i * 911 % 90_000)).collect();
    let plaintext_sum: i64 = salaries.iter().sum();
    let n_servers = 3;
    let col = SharedColumn::share(&salaries, n_servers, 0xC0FF_EE11);

    println!(
        "shared {} salaries across {n_servers} non-colluding servers.",
        salaries.len()
    );
    println!("each server's stored shares are uniform noise. row 0 looks like:");
    for s in 0..n_servers {
        println!("  server {s}: {}", col.share_at(s, 0));
    }
    println!("  (the real salary is {}, held by no one)\n", salaries[0]);

    // Compute SUM and AVG: each server sums its own shares; the client adds them.
    let rows: Vec<usize> = (0..salaries.len()).collect();
    let partials: Vec<i64> = (0..n_servers)
        .map(|s| col.server_partial(s, &rows))
        .collect();
    let private_sum = col.sum(&rows);
    let avg = col.avg(&rows).unwrap_or(0.0);

    println!("each server returns one partial sum (still meaningless alone):");
    for (s, p) in partials.iter().enumerate() {
        println!("  server {s} partial: {p}");
    }
    println!("\nclient adds the partials:");
    println!("  SUM   = {private_sum}   (true sum {plaintext_sum})");
    println!("  COUNT = {}", col.count(&rows));
    println!("  AVG   = {avg:.0}");

    println!("\n==================================================");
    if private_sum == plaintext_sum {
        println!("VERDICT: the exact total and average came out, yet no server ever held a");
        println!("single salary. additive shares are individually uniform noise but sum back");
        println!("to the truth, so the sum of the shares is the share of the sum.");
    } else {
        println!("VERDICT: aggregate mismatch ({private_sum} vs {plaintext_sum}).");
        return ExitCode::FAILURE;
    }
    println!("==================================================\n");
    ExitCode::SUCCESS
}
