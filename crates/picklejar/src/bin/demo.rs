//! A live, narrated walk through picklejar's headline features, driven against
//! the real engine. The scorecard proves the numbers; this shows the behavior.
//!
//! ```text
//! cargo run --release --bin demo
//! ```

use std::process::ExitCode;

use picklejar::{Database, QueryOutcome, Value};

/// Open a throwaway database under the OS temp directory (a standalone binary
/// cannot use the dev-only tempfile crate).
fn open() -> Database {
    let base = std::env::temp_dir().join(format!("pj_demo_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&base);
    Database::open(&base).expect("open")
}

/// Run a statement, panicking with context on error.
fn run(db: &mut Database, sql: &str) {
    db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}"));
}

/// The first cell of a query's first row as a display string, or `<none>` for an
/// empty result.
fn scalar(db: &mut Database, sql: &str) -> String {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => rows.first().map_or_else(
            || "<none>".to_string(),
            |row| match &row[0] {
                Value::Text(s) => s.clone(),
                other => other.to_string(),
            },
        ),
        other => format!("{other:?}"),
    }
}

/// Number of rows a query returns.
fn count(db: &mut Database, sql: &str) -> usize {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => rows.len(),
        other => panic!("expected rows, got {other:?}"),
    }
}

fn heading(n: u8, title: &str) {
    println!("\n{n}. {title}");
}

// A demo is one linear narration of several features; splitting it into helpers
// would scatter the story without making it clearer.
#[allow(clippy::too_many_lines)]
fn main() -> ExitCode {
    println!("\n=============== PICKLEJAR LIVE DEMO ===============");
    println!("the real engine, walking its headline behaviors\n");

    // 1. Valid-time travel: memory recalls what was true, and when.
    heading(1, "Valid-time travel: read the past as it was.");
    let mut db = open();
    run(
        &mut db,
        "CREATE TABLE prices (sku TEXT, price INT, valid_from TIMESTAMP, valid_to TIMESTAMP)",
    );
    run(
        &mut db,
        "INSERT INTO prices VALUES \
         ('A', 100, TIMESTAMP '2020-01-01 00:00:00', TIMESTAMP '2020-06-01 00:00:00')",
    );
    run(
        &mut db,
        "INSERT INTO prices VALUES ('A', 150, TIMESTAMP '2020-06-01 00:00:00', NULL)",
    );
    println!("   SKU 'A' cost 100 from Jan, then 150 from June onward.");
    run(&mut db, "SET valid_time = TIMESTAMP '2020-03-01 00:00:00'");
    println!(
        "   SET valid_time = '2020-03-01'  ->  price = {}   (March: the old price)",
        scalar(&mut db, "SELECT price FROM prices")
    );
    run(&mut db, "SET valid_time = TIMESTAMP '2020-09-01 00:00:00'");
    println!(
        "   SET valid_time = '2020-09-01'  ->  price = {}   (September: the new price)",
        scalar(&mut db, "SELECT price FROM prices")
    );
    run(&mut db, "RESET valid_time");

    // 2. Transaction-time travel: replay what the database itself knew.
    heading(2, "Transaction-time travel: read as of a past write point.");
    run(&mut db, "CREATE TABLE notes (id INT, body TEXT)");
    run(&mut db, "INSERT INTO notes VALUES (1, 'first thought')");
    let point = db.current_txid();
    run(
        &mut db,
        "UPDATE notes SET body = 'revised thought' WHERE id = 1",
    );
    println!(
        "   now            ->  '{}'",
        scalar(&mut db, "SELECT body FROM notes WHERE id = 1")
    );
    run(&mut db, &format!("SET transaction_time = {point}"));
    println!(
        "   as of point {point:>3}  ->  '{}'   (before the revision)",
        scalar(&mut db, "SELECT body FROM notes WHERE id = 1")
    );
    run(&mut db, "RESET transaction_time");

    // 3. Contradiction detection: a conflicting memory is caught at write time.
    heading(
        3,
        "Contradiction detection: a conflicting fact is rejected.",
    );
    run(
        &mut db,
        "CREATE TABLE facts (subject TEXT, attribute TEXT, value TEXT)",
    );
    let assert = "ON CONFLICT (subject, attribute) DO ASSERT";
    run(
        &mut db,
        &format!("INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue') {assert}"),
    );
    println!("   assert ada.favorite_color = blue   ->  stored");
    run(
        &mut db,
        &format!("INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue') {assert}"),
    );
    println!("   assert ada.favorite_color = blue   ->  idempotent (a known fact, allowed)");
    let conflict = db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'favorite_color', 'red') {assert}"
    ));
    match conflict {
        Err(e) => println!("   assert ada.favorite_color = red    ->  REJECTED: {e}"),
        Ok(_) => println!("   assert ada.favorite_color = red    ->  (unexpectedly accepted)"),
    }

    // 4. Tenant isolation: enforced by the engine, not the application.
    heading(
        4,
        "Tenant isolation: the engine fences each tenant to its own rows.",
    );
    run(&mut db, "CREATE TABLE memories (tenant TEXT, secret TEXT)");
    run(&mut db, "GRANT SELECT, INSERT ON memories TO PUBLIC");
    run(
        &mut db,
        "CREATE POLICY tenant ON memories USING ((tenant = current_user))",
    );
    run(&mut db, "ALTER TABLE memories ENABLE ROW LEVEL SECURITY");
    run(&mut db, "CREATE ROLE acme LOGIN");
    run(&mut db, "CREATE ROLE globex LOGIN");
    db.set_session_user("acme");
    run(
        &mut db,
        "INSERT INTO memories VALUES ('acme', 'the acme launch codes')",
    );
    println!(
        "   acme inserts a secret, then reads      ->  {} row(s) visible",
        count(&mut db, "SELECT secret FROM memories")
    );
    db.set_session_user("globex");
    println!(
        "   globex queries the very same table     ->  {} row(s) visible   (isolation holds)",
        count(&mut db, "SELECT secret FROM memories")
    );

    println!("\n==================================================");
    println!("Every line above ran against the real engine. For the proof");
    println!("behind the behavior, run:  cargo run --release --bin scorecard");
    println!("==================================================\n");
    ExitCode::SUCCESS
}
