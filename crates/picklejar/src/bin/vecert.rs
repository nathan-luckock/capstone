//! Generate and print the AI memory layer reliability certificate.
//!
//! Runs the memory layer's reliability invariants deterministically and prints a
//! reproducible, content-hashed report (see [`picklejar::certify`]). Exits 0 when
//! every invariant holds, non-zero otherwise, so it doubles as a release gate.
//!
//! ```text
//! cargo run --release --bin vecert
//! ```

use std::process::ExitCode;

use picklejar::certify::Certificate;

fn main() -> ExitCode {
    let cert = Certificate::generate();
    print!("{}", cert.render());
    if cert.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
