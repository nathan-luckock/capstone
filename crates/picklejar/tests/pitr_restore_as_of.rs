//! Logical point-in-time restore: `restore_as_of` rebuilds a fresh database
//! holding the state the source had as of a past transaction point, read through
//! the transaction-time-travel path and re-materialized through the normal write
//! path. These tests pin that the restored database holds exactly the as-of-point
//! data, that it is a clean writable database (fresh ids, working index and
//! constraints), and that schema, indexes, and views carry over.

use picklejar::{Database, QueryOutcome, Value};
use tempfile::tempdir;

/// Run a query and return its rows.
fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => rows,
        other => panic!("expected rows from `{sql}`, got {other:?}"),
    }
}

#[test]
fn restore_holds_the_data_as_of_the_point() {
    let dir = tempdir().expect("tempdir");
    let src_path = dir.path().join("src.db");
    let dst_path = dir.path().join("dst.db");

    let mut src = Database::open(&src_path).expect("open src");
    src.execute("CREATE TABLE m (id INT PRIMARY KEY, v TEXT)")
        .unwrap();
    src.execute("INSERT INTO m VALUES (1, 'a')").unwrap();
    src.execute("INSERT INTO m VALUES (2, 'b')").unwrap();
    let point = src.current_txid();
    // Changes after the point: an update, a delete, and a new row.
    src.execute("UPDATE m SET v = 'a2' WHERE id = 1").unwrap();
    src.execute("DELETE FROM m WHERE id = 2").unwrap();
    src.execute("INSERT INTO m VALUES (3, 'c')").unwrap();

    let report = src.restore_as_of(&dst_path, point).expect("restore");
    assert_eq!(report.tables, 1);
    assert_eq!(report.rows, 2, "the two rows that existed at the point");

    // The restored database has exactly the as-of-point state.
    let mut dst = Database::open(&dst_path).expect("open dst");
    let got = rows(&mut dst, "SELECT id, v FROM m ORDER BY id");
    assert_eq!(
        got,
        vec![
            vec![Value::Int(1), Value::Text("a".into())],
            vec![Value::Int(2), Value::Text("b".into())],
        ],
        "restore reflects the data as of the point, not the later changes"
    );
}

#[test]
fn the_restored_database_is_clean_and_writable() {
    let dir = tempdir().expect("tempdir");
    let src_path = dir.path().join("s.db");
    let dst_path = dir.path().join("d.db");

    let mut src = Database::open(&src_path).expect("open");
    src.execute("CREATE TABLE m (id INT PRIMARY KEY, v TEXT)")
        .unwrap();
    src.execute("INSERT INTO m VALUES (1, 'a')").unwrap();
    let point = src.current_txid();
    src.execute("INSERT INTO m VALUES (2, 'b')").unwrap();
    src.restore_as_of(&dst_path, point).expect("restore");

    let mut dst = Database::open(&dst_path).expect("open dst");
    // The primary-key index works: a duplicate id is rejected.
    assert!(
        dst.execute("INSERT INTO m VALUES (1, 'dup')").is_err(),
        "the rebuilt primary-key index must reject a duplicate"
    );
    // A genuinely new row inserts and reads back.
    dst.execute("INSERT INTO m VALUES (5, 'e')").unwrap();
    let got = rows(&mut dst, "SELECT v FROM m WHERE id = 5");
    assert_eq!(got, vec![vec![Value::Text("e".into())]]);
}

#[test]
fn restore_carries_schema_indexes_and_views() {
    let dir = tempdir().expect("tempdir");
    let src_path = dir.path().join("s2.db");
    let dst_path = dir.path().join("d2.db");

    let mut src = Database::open(&src_path).expect("open");
    src.execute("CREATE TABLE items (id INT PRIMARY KEY, sku TEXT, qty INT)")
        .unwrap();
    src.execute("CREATE INDEX items_sku ON items (sku)")
        .unwrap();
    src.execute("CREATE VIEW big AS SELECT id, sku FROM items WHERE qty > 10")
        .unwrap();
    src.execute("INSERT INTO items VALUES (1, 'x', 5)").unwrap();
    src.execute("INSERT INTO items VALUES (2, 'y', 50)")
        .unwrap();
    let point = src.current_txid();
    src.execute("INSERT INTO items VALUES (3, 'z', 99)")
        .unwrap();

    src.restore_as_of(&dst_path, point).expect("restore");
    let mut dst = Database::open(&dst_path).expect("open dst");

    // The view exists and reads the as-of-point data through it.
    let view_rows = rows(&mut dst, "SELECT id FROM big ORDER BY id");
    assert_eq!(
        view_rows,
        vec![vec![Value::Int(2)]],
        "only the qty>10 row that existed at the point"
    );
    // The secondary index is usable (an equality lookup on sku).
    let by_sku = rows(&mut dst, "SELECT qty FROM items WHERE sku = 'x'");
    assert_eq!(by_sku, vec![vec![Value::Int(5)]]);
}

#[test]
fn restoring_to_the_current_point_reproduces_everything() {
    let dir = tempdir().expect("tempdir");
    let src_path = dir.path().join("s3.db");
    let dst_path = dir.path().join("d3.db");

    let mut src = Database::open(&src_path).expect("open");
    src.execute("CREATE TABLE m (id INT PRIMARY KEY, v TEXT)")
        .unwrap();
    src.execute("INSERT INTO m VALUES (1, 'a')").unwrap();
    src.execute("INSERT INTO m VALUES (2, 'b')").unwrap();
    // The present point captures all committed work.
    let point = src.current_txid();

    let report = src.restore_as_of(&dst_path, point).expect("restore");
    assert_eq!(report.rows, 2);
    let mut dst = Database::open(&dst_path).expect("open dst");
    assert_eq!(rows(&mut dst, "SELECT id FROM m ORDER BY id").len(), 2);
}
