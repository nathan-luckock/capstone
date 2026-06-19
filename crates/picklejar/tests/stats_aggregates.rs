//! Statistical aggregates: population and sample variance and standard deviation,
//! checked against textbook values, including the NULL edge cases and GROUP BY.

use picklejar::{Database, QueryOutcome, Value};
use tempfile::tempdir;

fn open() -> Database {
    let dir = tempdir().expect("tempdir");
    let path = Box::leak(Box::new(dir)).path().join("s.db");
    Database::open(&path).expect("open")
}

/// The single scalar of a one-row, one-column result.
fn scalar(db: &mut Database, sql: &str) -> Value {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => rows[0][0].clone(),
        other => panic!("expected rows from `{sql}`, got {other:?}"),
    }
}

/// The float value of a one-row, one-column result.
fn float(db: &mut Database, sql: &str) -> f64 {
    match scalar(db, sql) {
        Value::Float(x) => x,
        other => panic!("expected a float from `{sql}`, got {other:?}"),
    }
}

#[test]
fn population_and_sample_match_textbook_values() {
    let mut db = open();
    db.execute("CREATE TABLE t (x INT)").unwrap();
    // The classic set: mean 5, population variance 4, population stddev 2.
    for x in [2, 4, 4, 4, 5, 5, 7, 9] {
        db.execute(&format!("INSERT INTO t VALUES ({x})")).unwrap();
    }

    assert!((float(&mut db, "SELECT VAR_POP(x) FROM t") - 4.0).abs() < 1e-9);
    assert!((float(&mut db, "SELECT STDDEV_POP(x) FROM t") - 2.0).abs() < 1e-9);
    // Sample divides by n - 1 = 7: 32 / 7.
    assert!((float(&mut db, "SELECT VARIANCE(x) FROM t") - 32.0 / 7.0).abs() < 1e-9);
    assert!((float(&mut db, "SELECT VAR_SAMP(x) FROM t") - 32.0 / 7.0).abs() < 1e-9);
    let want = (32.0_f64 / 7.0).sqrt();
    assert!((float(&mut db, "SELECT STDDEV(x) FROM t") - want).abs() < 1e-9);
    assert!((float(&mut db, "SELECT STDDEV_SAMP(x) FROM t") - want).abs() < 1e-9);
}

#[test]
fn null_edge_cases_follow_sql() {
    let mut db = open();
    db.execute("CREATE TABLE t (x INT)").unwrap();

    // No rows: every variant is NULL.
    assert_eq!(scalar(&mut db, "SELECT VAR_POP(x) FROM t"), Value::Null);
    assert_eq!(scalar(&mut db, "SELECT STDDEV_SAMP(x) FROM t"), Value::Null);

    // One row: population variance is 0, but the sample variants are NULL (no
    // n - 1 to divide by).
    db.execute("INSERT INTO t VALUES (42)").unwrap();
    assert!((float(&mut db, "SELECT VAR_POP(x) FROM t")).abs() < 1e-12);
    assert!((float(&mut db, "SELECT STDDEV_POP(x) FROM t")).abs() < 1e-12);
    assert_eq!(scalar(&mut db, "SELECT VARIANCE(x) FROM t"), Value::Null);
    assert_eq!(scalar(&mut db, "SELECT STDDEV(x) FROM t"), Value::Null);

    // NULL inputs are skipped, like SUM and AVG.
    db.execute("INSERT INTO t VALUES (NULL)").unwrap();
    db.execute("INSERT INTO t VALUES (44)").unwrap();
    // Two non-null values 42, 44: population variance 1, stddev 1.
    assert!((float(&mut db, "SELECT VAR_POP(x) FROM t") - 1.0).abs() < 1e-9);
}

#[test]
fn stddev_groups_and_floats() {
    let mut db = open();
    db.execute("CREATE TABLE m (g TEXT, x FLOAT)").unwrap();
    // Group a: {1.0, 3.0} -> pop variance 1.0. Group b: {10.0} -> 0.0.
    db.execute("INSERT INTO m VALUES ('a', 1.0)").unwrap();
    db.execute("INSERT INTO m VALUES ('a', 3.0)").unwrap();
    db.execute("INSERT INTO m VALUES ('b', 10.0)").unwrap();

    let rows = match db
        .execute("SELECT g, VAR_POP(x) FROM m GROUP BY g ORDER BY g")
        .unwrap()
    {
        QueryOutcome::Rows { rows, .. } => rows,
        other => panic!("expected rows, got {other:?}"),
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("a".into()));
    match rows[0][1] {
        Value::Float(v) => assert!((v - 1.0).abs() < 1e-9),
        ref other => panic!("expected float, got {other:?}"),
    }
    match rows[1][1] {
        Value::Float(v) => assert!(v.abs() < 1e-12),
        ref other => panic!("expected float, got {other:?}"),
    }
}
