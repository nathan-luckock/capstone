//! Populate a database with a realistic multi-tenant AI-memory corpus: several
//! tenants, each fenced to its own rows by row-level security, each carrying a
//! cluster of embeddings so that nearest-neighbor recall is meaningful.
//!
//! ```text
//! cargo run --release --bin memload -- mydb.db            # 8 tenants, 500 each, 16 dims
//! cargo run --release --bin memload -- mydb.db 4 1000 32  # 4 tenants, 1000 each, 32 dims
//! ```
//!
//! Afterward the file is a ready-to-poke memory store. Open it with the CLI,
//! `SET ROLE tenant_3`, and every query is fenced to that tenant's own memories.

use std::fmt::Write as _;
use std::process::ExitCode;
use std::time::Instant;

use picklejar::Database;

/// A tiny deterministic xorshift generator, so a given invocation always builds
/// the same corpus.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A float in `[0, 1)`.
    #[allow(clippy::cast_precision_loss)]
    fn unit(&mut self) -> f32 {
        // Top 24 bits give a uniform value with no precision surprises.
        (self.next() >> 40) as f32 / 16_777_216.0
    }
}

/// A vector literal of `dims` values, each near `center` so that one tenant's
/// memories form a recognizable cluster.
fn vector_literal(rng: &mut Rng, dims: usize, center: f32) -> String {
    let mut s = String::with_capacity(dims * 8);
    s.push('[');
    for d in 0..dims {
        if d > 0 {
            s.push(',');
        }
        // center +/- a small spread, clamped into the unit range.
        let jitter = (rng.unit() - 0.5) * 0.2;
        let v = (center + jitter).clamp(0.0, 1.0);
        let _ = write!(s, "{v:.4}");
    }
    s.push(']');
    s
}

#[allow(clippy::cast_precision_loss)]
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: memload <db_path> [tenants] [per_tenant] [dims]");
        return ExitCode::FAILURE;
    };
    let tenants: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8);
    let per_tenant: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(500);
    let dims: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);

    let mut db = match Database::open(std::path::Path::new(path)) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("could not open {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    // A real multi-tenant memory store: an embedding per memory, and a policy
    // that fences each tenant to rows whose tenant column is the current user.
    let setup = [
        format!(
            "CREATE TABLE memories (id SERIAL PRIMARY KEY, tenant TEXT, body TEXT, embedding VECTOR({dims}))"
        ),
        "GRANT SELECT, INSERT ON memories TO PUBLIC".to_string(),
        "CREATE POLICY tenant_fence ON memories USING ((tenant = current_user))".to_string(),
        "ALTER TABLE memories ENABLE ROW LEVEL SECURITY".to_string(),
    ];
    for stmt in &setup {
        if let Err(e) = db.execute(stmt) {
            eprintln!("setup failed on `{stmt}`: {e}");
            return ExitCode::FAILURE;
        }
    }

    // Create every tenant role first, while still the bootstrap superuser. A
    // tenant role cannot create the next one, so all role DDL must precede the
    // first session switch.
    for t in 0..tenants {
        if let Err(e) = db.execute(&format!("CREATE ROLE tenant_{t} LOGIN")) {
            eprintln!("could not create role tenant_{t}: {e}");
            return ExitCode::FAILURE;
        }
    }

    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    let start = Instant::now();
    let mut rows = 0usize;

    for t in 0..tenants {
        let tenant = format!("tenant_{t}");
        // Each tenant logs in as its own role, so the inserts pass the fence.
        db.set_session_user(&tenant);

        // A stable cluster center for this tenant: evenly spaced across [0, 1).
        let center = (t as f32 + 0.5) / tenants as f32;

        if let Err(e) = db.execute("BEGIN") {
            eprintln!("begin failed: {e}");
            return ExitCode::FAILURE;
        }
        for i in 0..per_tenant {
            let body = format!("memory {i} for {tenant}");
            let vec = vector_literal(&mut rng, dims, center);
            let sql =
                format!("INSERT INTO memories (tenant, body, embedding) VALUES ('{tenant}', '{body}', '{vec}')");
            if let Err(e) = db.execute(&sql) {
                eprintln!("insert failed for {tenant} row {i}: {e}");
                return ExitCode::FAILURE;
            }
            rows += 1;
        }
        if let Err(e) = db.execute("COMMIT") {
            eprintln!("commit failed: {e}");
            return ExitCode::FAILURE;
        }
    }

    // Drop back to the bootstrap superuser before reporting.
    db.set_session_user("picklejar");
    let elapsed = start.elapsed().as_secs_f64();
    let rate = if elapsed > 0.0 {
        rows as f64 / elapsed
    } else {
        0.0
    };

    println!("loaded {rows} memories across {tenants} tenants ({dims}-dim embeddings) into {path}");
    println!("  {rows} durable rows in {elapsed:.1}s ({rate:.0} rows/sec)");
    println!();
    println!("try it:");
    println!("  cargo run --release --bin picklejar -- --database {path}");
    println!("  picklejar> SET ROLE tenant_3;");
    println!(
        "  picklejar> SELECT body FROM memories ORDER BY embedding <-> '[0.4, 0.4, ...]' LIMIT 5;"
    );
    ExitCode::SUCCESS
}
