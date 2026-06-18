//! A consistent physical backup restores an identical database, frozen at the
//! moment the backup was taken: changes made to the original afterward do not
//! appear in the copy, and the copy opens and serves its data (and its policies)
//! exactly.

use picklejar::{Database, QueryOutcome, Value};
use tempfile::tempdir;

fn count(db: &mut Database, table: &str) -> i64 {
    match db
        .execute(&format!("SELECT count(*) FROM {table}"))
        .expect("count")
    {
        QueryOutcome::Rows { rows, .. } => match rows.first().and_then(|r| r.first()) {
            Some(Value::Int(n)) => *n,
            other => panic!("expected count int, got {other:?}"),
        },
        other => panic!("expected rows, got {other:?}"),
    }
}

#[test]
fn backup_restores_a_consistent_snapshot_frozen_at_backup_time() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("primary.db");
    let dst = dir.path().join("backup/copy.db");

    {
        let mut db = Database::open(&src).expect("open");
        db.execute("CREATE TABLE memories (id INT, tenant TEXT, e VECTOR(2))")
            .unwrap();
        for i in 1..=100i64 {
            db.execute(&format!(
                "INSERT INTO memories VALUES ({i}, 'acme', '[{i}, {i}]')"
            ))
            .unwrap();
        }
        // A policy, so the backup must capture the metadata sidecars too.
        db.execute("GRANT SELECT ON memories TO PUBLIC").unwrap();
        db.execute("CREATE ROLE acme LOGIN").unwrap();
        db.execute("CREATE POLICY tenant ON memories USING ((tenant = current_user()))")
            .unwrap();
        db.execute("ALTER TABLE memories ENABLE ROW LEVEL SECURITY")
            .unwrap();

        let report = db.backup(&dst).expect("backup");
        assert!(report.files >= 2, "at least the heap and WAL are copied");
        assert!(report.bytes > 0);

        // Mutate the original *after* the backup; the copy must not see this.
        db.execute("INSERT INTO memories VALUES (999, 'acme', '[9, 9]')")
            .unwrap();
    }

    // The restored copy is frozen at backup time: 100 rows, no row 999.
    let mut copy = Database::open(&dst).expect("restore opens");
    assert_eq!(count(&mut copy, "memories"), 100, "copy frozen at backup");
    match copy
        .execute("SELECT id FROM memories WHERE id = 999")
        .expect("query")
    {
        QueryOutcome::Rows { rows, .. } => {
            assert!(rows.is_empty(), "post-backup write must not be in the copy");
        }
        other => panic!("expected rows, got {other:?}"),
    }
    // The policy survived the backup: acme sees only its own rows, fenced.
    copy.set_session_user("acme");
    assert_eq!(
        count(&mut copy, "memories"),
        100,
        "acme sees its rows through the restored policy"
    );

    // The original kept going and has the extra row.
    let mut primary = Database::open(&src).expect("reopen primary");
    assert_eq!(
        count(&mut primary, "memories"),
        101,
        "original has the later write"
    );
}
