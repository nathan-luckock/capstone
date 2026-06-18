//! The HNSW index path must be a transparent accelerator: when it is enabled, a
//! `SELECT ... ORDER BY col <op> :q LIMIT k` returns the same answer the exact
//! scan would, and it must never weaken the isolation or permission guarantees.
//! These tests pin both properties by running each query twice (index off, then
//! on) and comparing, and by checking that row-level security and table grants
//! still fence the indexed path.

use picklejar::{Database, QueryOutcome, Value};
use tempfile::tempdir;

/// Run a query and return its rows, failing loudly on anything else.
fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => rows,
        other => panic!("expected rows from `{sql}`, got {other:?}"),
    }
}

/// A store of well-separated embeddings on a line, so the exact nearest-neighbor
/// answer is unambiguous and the approximate path is expected to reproduce it.
fn seeded_store() -> Database {
    let dir = tempdir().expect("tempdir");
    // Leak the dir so the file outlives this function; the OS reclaims it.
    let path = Box::leak(Box::new(dir)).path().join("v.db");
    let mut db = Database::open(&path).expect("open");
    db.execute("CREATE TABLE items (id INT, tag TEXT, e VECTOR(2))")
        .unwrap();
    for i in 0..60i64 {
        let x = f32::from(i16::try_from(i).expect("small"));
        db.execute(&format!(
            "INSERT INTO items VALUES ({i}, 't{}', '[{x}, 0]')",
            i % 3
        ))
        .unwrap();
    }
    db
}

#[test]
fn index_path_matches_exact_path_for_star_and_projection() {
    let mut db = seeded_store();

    // Several KNN shapes the index path accepts. Each must agree with the exact
    // scan for every distance operator we support.
    let queries = [
        "SELECT * FROM items ORDER BY e <-> '[3.2, 0]' LIMIT 5",
        "SELECT id, tag FROM items ORDER BY e <-> '[40.3, 0]' LIMIT 8",
        "SELECT id FROM items ORDER BY e <#> '[10, 0]' LIMIT 4",
        "SELECT id, e FROM items ORDER BY e <+> '[7.1, 0]' LIMIT 6",
    ];

    for q in queries {
        db.set_vector_index(false);
        let exact = rows(&mut db, q);
        db.set_vector_index(true);
        let indexed = rows(&mut db, q);
        assert_eq!(
            indexed, exact,
            "the index path disagreed with the exact path for `{q}`"
        );
    }
}

#[test]
fn unsupported_shapes_fall_through_unchanged() {
    let mut db = seeded_store();
    db.set_vector_index(true);

    // A WHERE clause, a join-like projection, no LIMIT, or a non-vector ORDER BY
    // are all outside the accepted shape; they must still return correct results
    // by falling through to the exact evaluator (identical to index-off).
    let shapes = [
        "SELECT id FROM items WHERE id < 10 ORDER BY e <-> '[2, 0]' LIMIT 3",
        "SELECT id FROM items ORDER BY e <-> '[2, 0]'",
        "SELECT id FROM items ORDER BY id LIMIT 3",
        "SELECT count(*) FROM items",
    ];
    for q in shapes {
        db.set_vector_index(true);
        let on = rows(&mut db, q);
        db.set_vector_index(false);
        let off = rows(&mut db, q);
        assert_eq!(on, off, "fall-through changed the answer for `{q}`");
    }
}

#[test]
fn index_path_preserves_row_level_security() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("rls.db");
    let mut db = Database::open(&path).expect("open");

    db.execute("CREATE TABLE memories (id INT, tenant TEXT, e VECTOR(2))")
        .unwrap();
    // Two tenants, interleaved, with overlapping embedding space so a naive
    // index over all rows would surface the other tenant's vectors.
    for i in 0..20i64 {
        let tenant = if i % 2 == 0 { "orion" } else { "vega" };
        let x = f32::from(i16::try_from(i).expect("small"));
        db.execute(&format!(
            "INSERT INTO memories VALUES ({i}, '{tenant}', '[{x}, 0]')"
        ))
        .unwrap();
    }
    db.execute("GRANT SELECT ON memories TO PUBLIC").unwrap();
    db.execute("CREATE ROLE orion LOGIN").unwrap();
    db.execute("CREATE ROLE vega LOGIN").unwrap();
    db.execute("CREATE POLICY tenant ON memories USING ((tenant = current_user()))")
        .unwrap();
    db.execute("ALTER TABLE memories ENABLE ROW LEVEL SECURITY")
        .unwrap();

    // With the index path ENABLED, orion's nearest-neighbor search must still see
    // only orion's rows. The folded RLS predicate becomes a WHERE, which the
    // index shape rejects, so the query falls through to the fenced exact path.
    db.set_vector_index(true);
    db.set_session_user("orion");
    let hits = rows(
        &mut db,
        "SELECT id, tenant FROM memories ORDER BY e <-> '[9, 0]' LIMIT 100",
    );
    assert!(!hits.is_empty(), "orion should see her own memories");
    for row in &hits {
        assert_eq!(
            row.get(1),
            Some(&Value::Text("orion".to_string())),
            "isolation breach: the indexed path surfaced another tenant's row"
        );
    }
    // Exactly orion's ten even-id rows, nothing leaked.
    assert_eq!(hits.len(), 10, "orion has exactly ten memories");
}

#[test]
fn index_path_respects_table_permissions() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("perm.db");
    let mut db = Database::open(&path).expect("open");

    db.execute("CREATE TABLE secrets (id INT, e VECTOR(2))")
        .unwrap();
    db.execute("INSERT INTO secrets VALUES (1, '[1, 0]'), (2, '[0, 1]')")
        .unwrap();
    db.execute("CREATE ROLE intruder LOGIN").unwrap();

    // intruder was never granted SELECT. Even with the index path on, the query
    // must be denied, because the candidate rows are fetched through the engine's
    // own permission-checked SELECT.
    db.set_vector_index(true);
    db.set_session_user("intruder");
    let denied = db.execute("SELECT id FROM secrets ORDER BY e <-> '[1, 0]' LIMIT 1");
    assert!(
        denied.is_err(),
        "the indexed path served a table the role cannot read"
    );
}
