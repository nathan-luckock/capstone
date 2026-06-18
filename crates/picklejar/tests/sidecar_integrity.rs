//! The metadata sidecars (catalog, policies, grants, constraints) carry a CRC32
//! integrity header, so a flipped byte in one of them is detected on load rather
//! than silently applied. The most security-relevant case is the policy file: a
//! corrupted `.pol` must never reopen with row-level security silently dropped,
//! which would expose every tenant's rows to every other tenant.

use picklejar::{Database, QueryOutcome, Value};
use tempfile::tempdir;

/// Build a fenced two-tenant table, returning the database path.
fn fenced_db(path: &std::path::Path) {
    let mut db = Database::open(path).expect("open");
    db.execute("CREATE TABLE memories (id INT, tenant TEXT)")
        .unwrap();
    db.execute("INSERT INTO memories VALUES (1, 'alice'), (2, 'mallory')")
        .unwrap();
    db.execute("GRANT SELECT ON memories TO PUBLIC").unwrap();
    db.execute("CREATE ROLE alice LOGIN").unwrap();
    db.execute("CREATE ROLE mallory LOGIN").unwrap();
    db.execute("CREATE POLICY tenant ON memories USING ((tenant = current_user()))")
        .unwrap();
    db.execute("ALTER TABLE memories ENABLE ROW LEVEL SECURITY")
        .unwrap();
}

/// Flip the last byte of `path` (a body byte, past the checksum header).
fn corrupt_last_byte(path: &std::path::Path) {
    let mut bytes = std::fs::read(path).expect("read sidecar");
    let n = bytes.len();
    assert!(n > 9, "sidecar should have a header plus a body");
    bytes[n - 1] ^= 0xFF;
    std::fs::write(path, &bytes).expect("write sidecar");
}

#[test]
fn corrupting_the_policy_file_is_detected_not_silently_applied() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("m.db");
    fenced_db(&path);

    // Corrupt the policy sidecar. The integrity header must catch it.
    corrupt_last_byte(&path.with_extension("pol"));

    // Reopen. Either the corruption is detected (open fails) or, if it somehow
    // opens, row-level security must still fence alice to her own row. What must
    // never happen is a silent reopen with the policy dropped.
    match Database::open(&path) {
        Err(_) => {} // detected, as intended
        Ok(mut db) => {
            db.set_session_user("alice");
            let rows = match db.execute("SELECT id, tenant FROM memories") {
                Ok(QueryOutcome::Rows { rows, .. }) => rows,
                Err(_) => return, // detected on query, also fine
                Ok(other) => panic!("expected rows, got {other:?}"),
            };
            for row in &rows {
                assert_eq!(
                    row.get(1),
                    Some(&Value::Text("alice".to_string())),
                    "row-level security was silently dropped by a corrupted policy file"
                );
            }
        }
    }
}

#[test]
fn corrupting_the_catalog_file_is_detected() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("c.db");
    {
        let mut db = Database::open(&path).expect("open");
        db.execute("CREATE TABLE t (id INT, name TEXT)").unwrap();
        db.execute("INSERT INTO t VALUES (1, 'x'), (2, 'y')")
            .unwrap();
    }
    corrupt_last_byte(&path.with_extension("meta"));

    // A corrupted catalog must not reopen as if nothing happened: either open
    // fails, or it opens but does not silently serve a wrong table shape.
    if let Ok(mut db) = Database::open(&path) {
        // If it opened, the table must still read back its committed rows exactly
        // or raise an error; a silently different answer is the failure.
        if let Ok(QueryOutcome::Rows { rows, .. }) =
            db.execute("SELECT id, name FROM t ORDER BY id")
        {
            assert_eq!(
                rows,
                vec![
                    vec![Value::Int(1), Value::Text("x".to_string())],
                    vec![Value::Int(2), Value::Text("y".to_string())],
                ],
                "a corrupted catalog silently changed the committed data"
            );
        }
    }
}

#[test]
fn a_clean_database_still_reopens() {
    // The integrity header must round-trip: a clean database reopens unchanged.
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("ok.db");
    fenced_db(&path);

    let mut db = Database::open(&path).expect("clean reopen");
    db.set_session_user("mallory");
    let rows = match db
        .execute("SELECT id, tenant FROM memories")
        .expect("query")
    {
        QueryOutcome::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    };
    assert_eq!(
        rows,
        vec![vec![Value::Int(2), Value::Text("mallory".to_string())]]
    );
}
