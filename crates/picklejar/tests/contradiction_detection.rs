//! Contradiction detection for AI-memory facts: `INSERT ... ON CONFLICT (key)
//! DO ASSERT`. Re-asserting an identical fact under the same key is idempotent;
//! asserting a different value for that key is rejected as a contradiction. These
//! tests pin both halves, the first assertion, composite keys, the intra-statement
//! cases, and that a target is required.

use picklejar::{Database, QueryOutcome, Value};
use tempfile::tempdir;

fn open() -> Database {
    let dir = tempdir().expect("tempdir");
    let path = Box::leak(Box::new(dir)).path().join("m.db");
    let mut db = Database::open(&path).expect("open");
    // A subject/attribute/value memory table: at most one value per (subject,
    // attribute) is the fact, and the pair is the conflict key. DO ASSERT itself
    // enforces single-fact-per-key, so no separate unique constraint is needed.
    db.execute("CREATE TABLE facts (subject TEXT, attribute TEXT, value TEXT)")
        .unwrap();
    db
}

fn count(db: &mut Database, sql: &str) -> usize {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => rows.len(),
        other => panic!("expected rows from `{sql}`, got {other:?}"),
    }
}

fn one_text(db: &mut Database, sql: &str) -> String {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => match &rows[0][0] {
            Value::Text(s) => s.clone(),
            other => panic!("expected text, got {other:?}"),
        },
        other => panic!("expected rows, got {other:?}"),
    }
}

const ASSERT: &str = "ON CONFLICT (subject, attribute) DO ASSERT";

#[test]
fn first_assertion_inserts_the_fact() {
    let mut db = open();
    db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue') {ASSERT}"
    ))
    .expect("first assertion inserts");
    assert_eq!(count(&mut db, "SELECT * FROM facts"), 1);
    assert_eq!(
        one_text(&mut db, "SELECT value FROM facts WHERE subject = 'ada'"),
        "blue"
    );
}

#[test]
fn re_asserting_the_identical_fact_is_idempotent() {
    let mut db = open();
    db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue') {ASSERT}"
    ))
    .unwrap();
    // The same fact again: allowed, and it does not duplicate the row.
    db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue') {ASSERT}"
    ))
    .expect("re-asserting the identical fact is allowed");
    assert_eq!(count(&mut db, "SELECT * FROM facts"), 1);
}

#[test]
fn asserting_a_conflicting_value_is_a_contradiction() {
    let mut db = open();
    db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue') {ASSERT}"
    ))
    .unwrap();
    // A different value for the same fact: rejected.
    let err = db
        .execute(&format!(
            "INSERT INTO facts VALUES ('ada', 'favorite_color', 'red') {ASSERT}"
        ))
        .expect_err("a conflicting value must be a contradiction");
    let msg = err.to_string();
    assert!(
        msg.contains("contradiction"),
        "error should be a contradiction, got: {msg}"
    );
    // The stored fact is unchanged: the contradiction did not overwrite it.
    assert_eq!(
        one_text(&mut db, "SELECT value FROM facts WHERE subject = 'ada'"),
        "blue"
    );
    assert_eq!(count(&mut db, "SELECT * FROM facts"), 1);
}

#[test]
fn distinct_facts_coexist() {
    let mut db = open();
    db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue') {ASSERT}"
    ))
    .unwrap();
    // A different subject, and a different attribute of the same subject: both
    // are distinct facts, not contradictions.
    db.execute(&format!(
        "INSERT INTO facts VALUES ('grace', 'favorite_color', 'green') {ASSERT}"
    ))
    .expect("different subject is a distinct fact");
    db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'city', 'london') {ASSERT}"
    ))
    .expect("different attribute is a distinct fact");
    assert_eq!(count(&mut db, "SELECT * FROM facts"), 3);
}

#[test]
fn intra_statement_conflicting_assertions_are_caught() {
    let mut db = open();
    // Two rows in one statement that assert different values for the same fact.
    let err = db
        .execute(&format!(
            "INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue'), ('ada', 'favorite_color', 'red') {ASSERT}"
        ))
        .expect_err("a self-contradicting batch must be rejected");
    assert!(err.to_string().contains("contradiction"));
    // The whole statement was rejected: nothing was written.
    assert_eq!(count(&mut db, "SELECT * FROM facts"), 0);
}

#[test]
fn intra_statement_identical_assertions_collapse() {
    let mut db = open();
    // The same fact twice in one statement: allowed, stored once.
    db.execute(&format!(
        "INSERT INTO facts VALUES ('ada', 'favorite_color', 'blue'), ('ada', 'favorite_color', 'blue') {ASSERT}"
    ))
    .expect("an idempotent batch is allowed");
    assert_eq!(count(&mut db, "SELECT * FROM facts"), 1);
}

#[test]
fn do_assert_requires_a_conflict_target() {
    let mut db = open();
    let err = db
        .execute("INSERT INTO facts VALUES ('ada', 'c', 'blue') ON CONFLICT DO ASSERT")
        .expect_err("DO ASSERT without a target is rejected");
    assert!(err.to_string().contains("requires a conflict target"));
}
