//! Authenticated nearest-neighbor against a node you do not trust.
//!
//! ```text
//! cargo run --release --bin authknn
//! ```
//!
//! Memories are stored durably in the engine, committed to a single 32-byte
//! root, and a nearest-neighbor query is answered with a proof. A thin client
//! that holds only the root verifies the answer, then a simulated malicious
//! server tries four ways to cheat and is caught every time.

use std::process::ExitCode;

use picklejar::authmem::{authenticated_knn, commit, verify_knn, MemoryRecord};
use picklejar::{Database, QueryOutcome, Value};

/// Pull the committed memories back out of the engine as records to commit to.
fn load_records(db: &mut Database) -> Vec<MemoryRecord> {
    let rows = match db.execute("SELECT id, tenant, embedding FROM memories") {
        Ok(QueryOutcome::Rows { rows, .. }) => rows,
        other => panic!("unexpected query result: {other:?}"),
    };
    rows.into_iter()
        .map(|r| {
            let rowid = match &r[0] {
                Value::Int(i) => u64::try_from(*i).unwrap_or(0),
                other => panic!("bad id: {other:?}"),
            };
            let tenant = match &r[1] {
                Value::Text(t) => t.clone(),
                other => panic!("bad tenant: {other:?}"),
            };
            let vector = match &r[2] {
                Value::Vector(v) => v.clone(),
                other => panic!("bad embedding: {other:?}"),
            };
            MemoryRecord {
                rowid,
                tenant,
                vector,
            }
        })
        .collect()
}

fn open() -> Database {
    let base = std::env::temp_dir().join(format!("pj_authknn_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&base);
    Database::open(&base).expect("open")
}

#[allow(clippy::too_many_lines)]
fn main() -> ExitCode {
    println!("\n=============== AUTHENTICATED KNN ===============");
    println!("verifiable nearest-neighbor for a node you cannot trust\n");

    // Store memories durably for two tenants.
    let mut db = open();
    db.execute("CREATE TABLE memories (id INT PRIMARY KEY, tenant TEXT, embedding VECTOR(2))")
        .expect("create");
    let seed = [
        (1, "acme", "[0.0, 0.0]"),
        (2, "acme", "[1.0, 1.0]"),
        (3, "acme", "[5.0, 5.0]"),
        (4, "globex", "[0.1, 0.1]"),
        (5, "acme", "[2.0, 2.0]"),
        (6, "globex", "[9.0, 9.0]"),
    ];
    for (id, tenant, vec) in seed {
        db.execute(&format!(
            "INSERT INTO memories VALUES ({id}, '{tenant}', '{vec}')"
        ))
        .expect("insert");
    }

    // Commit the engine's committed state to one 32-byte root. This is all a
    // thin client needs to pin.
    let records = load_records(&mut db);
    let root = commit(&records);
    println!("committed {} memories across 2 tenants", records.len());
    println!("pinned root: {}", root.hex());
    println!("(a client holds only this 32-byte root, and never trusts the server again)\n");

    // The honest server answers acme's nearest-neighbor query with proofs.
    let query = [0.2_f32, 0.2];
    let k = 3;
    let (served_root, hits) = authenticated_knn(&records, "acme", &query, k);
    assert_eq!(served_root, root, "server commits to the same state");

    println!("acme asks for its {k} nearest memories to [0.2, 0.2]:");
    for (rank, hit) in hits.iter().enumerate() {
        println!(
            "  {}. row {}  vector {:?}  distance {:.3}  (proof: {} hashes)",
            rank + 1,
            hit.record.rowid,
            hit.record.vector,
            hit.distance,
            hit.proof.siblings.len()
        );
    }
    match verify_knn(root, "acme", &query, &hits, k) {
        Ok(()) => println!("  client verification: PASS (authentic, correctly scored, in order)\n"),
        Err(e) => {
            println!("  client verification unexpectedly FAILED: {e}");
            return ExitCode::FAILURE;
        }
    }

    // Now a malicious or corrupted server tries to cheat, four ways. The client,
    // holding only the root, catches each one.
    println!("a malicious server now tries to cheat. the client holds only the root:\n");
    let mut caught = 0;

    // 1. Fabricate: alter a vector the server claims is committed.
    {
        let mut tampered = hits.clone();
        tampered[0].record.vector[0] = 99.0;
        report(
            "fabricate a memory (alter a committed vector)",
            &mut caught,
            verify_knn(root, "acme", &query, &tampered, k),
        );
    }

    // 2. Mis-score: claim a result is closer than it is.
    {
        let mut tampered = hits.clone();
        tampered[1].distance = 0.0;
        report(
            "lie about a distance (claim a far memory is near)",
            &mut caught,
            verify_knn(root, "acme", &query, &tampered, k),
        );
    }

    // 3. Cross-tenant leak: substitute globex's real, committed row, with a
    // genuine inclusion proof. The proof passes; the tenant check does not.
    {
        let (_, globex_hits) = authenticated_knn(&records, "globex", &query, k);
        let mut tampered = hits.clone();
        tampered[2] = globex_hits[0].clone();
        report(
            "leak another tenant's row (with a valid proof)",
            &mut caught,
            verify_knn(root, "acme", &query, &tampered, k),
        );
    }

    // 4. Reorder: present the results out of distance order. This is the last
    // use of the honest hits, so it takes them by value.
    {
        let mut tampered = hits;
        tampered.swap(0, 2);
        report(
            "reorder the results",
            &mut caught,
            verify_knn(root, "acme", &query, &tampered, k),
        );
    }

    println!("\n================================================");
    if caught == 4 {
        println!("VERDICT: all 4 attacks caught. the server proved its answer; the");
        println!("client trusted the math, not the machine.");
    } else {
        println!("VERDICT: only {caught}/4 attacks caught. something is wrong.");
        return ExitCode::FAILURE;
    }
    println!("note: this proves soundness (every served answer is real, scored, and");
    println!("ordered). completeness (no closer memory hidden) is the open frontier.");
    println!("================================================\n");
    ExitCode::SUCCESS
}

/// Print whether an attack was caught, and tally it.
fn report(attack: &str, caught: &mut u32, result: Result<(), picklejar::authmem::VerifyError>) {
    match result {
        Err(e) => {
            *caught += 1;
            println!("  [CAUGHT] {attack}");
            println!("           reason: {e}");
        }
        Ok(()) => println!("  [MISSED] {attack} -- verification wrongly passed"),
    }
}
