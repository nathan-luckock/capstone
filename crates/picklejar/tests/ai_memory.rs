//! End-to-end walk of the AI memory layer through the public `Database` API:
//! durable embeddings, nearest-neighbor recall, the vector functions, and
//! engine-enforced per-tenant isolation that survives a crash. This reads as the
//! demo script for the memory layer and doubles as an executable specification.

use picklejar::{Database, QueryOutcome, Value};
use tempfile::tempdir;

/// Run a query and return its rows, failing loudly on anything else.
fn rows(db: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    match db.execute(sql).unwrap_or_else(|e| panic!("`{sql}`: {e}")) {
        QueryOutcome::Rows { rows, .. } => rows,
        other => panic!("expected rows from `{sql}`, got {other:?}"),
    }
}

/// The id column of each returned row, in order.
fn ids(rows: &[Vec<Value>]) -> Vec<i64> {
    rows.iter()
        .map(|r| match r.first() {
            Some(Value::Int(n)) => *n,
            other => panic!("expected an INT id, got {other:?}"),
        })
        .collect()
}

#[test]
fn ai_memory_layer_end_to_end() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("memory.db");
    let mut db = Database::open(&path).expect("open");

    // A memory store for autonomous agents: each row is a remembered fact, owned
    // by a tenant (the agent), with a 4-dimensional embedding.
    db.execute("CREATE TABLE memories (id INT, tenant TEXT, fact TEXT, embedding VECTOR(4))")
        .unwrap();
    db.execute(
        "INSERT INTO memories VALUES \
         (1, 'orion', 'docking sequence nominal', '[1, 0, 0, 0]'), \
         (2, 'orion', 'thermal load rising',      '[0, 1, 0, 0]'), \
         (3, 'orion', 'comms window at 0400',     '[0, 0, 1, 0]'), \
         (4, 'vega',  'battery cell B degraded',  '[1, 1, 0, 0]'), \
         (5, 'vega',  'reaction wheel vibration', '[0, 0, 0, 1]')",
    )
    .unwrap();

    // Nearest-neighbor recall: the memory closest to a query embedding. With no
    // isolation yet, the globally nearest to [1,0,0,0] is orion's row 1.
    let nearest = rows(
        &mut db,
        "SELECT id FROM memories ORDER BY embedding <-> '[1, 0, 0, 0]' LIMIT 1",
    );
    assert_eq!(ids(&nearest), [1]);

    // The vector functions report shape and magnitude.
    let shape = rows(
        &mut db,
        "SELECT vector_dims(embedding), l2_norm(embedding) FROM memories WHERE id = 4",
    );
    assert_eq!(shape[0][0], Value::Int(4));
    match shape[0][1] {
        // |[1,1,0,0]| = sqrt(2).
        Value::Float(x) => assert!((x - 2f64.sqrt()).abs() < 1e-9, "l2_norm was {x}"),
        ref other => panic!("expected float, got {other:?}"),
    }

    // Turn on engine-enforced isolation: each agent can see only its own
    // memories. The policy lives in the engine, not the application.
    db.execute("GRANT SELECT ON memories TO PUBLIC").unwrap();
    db.execute("CREATE ROLE orion LOGIN").unwrap();
    db.execute("CREATE ROLE vega LOGIN").unwrap();
    db.execute("CREATE POLICY tenant ON memories USING ((tenant = current_user()))")
        .unwrap();
    db.execute("ALTER TABLE memories ENABLE ROW LEVEL SECURITY")
        .unwrap();

    // orion's nearest-neighbor search to vega's vector [1,1,0,0] still ranks only
    // orion's rows: vega's globally-nearest row 4 is invisible to orion.
    db.set_session_user("orion");
    let orion_knn = rows(
        &mut db,
        "SELECT id FROM memories ORDER BY embedding <-> '[1, 1, 0, 0]' LIMIT 5",
    );
    assert_eq!(orion_knn.len(), 3, "orion has exactly three memories");
    for id in ids(&orion_knn) {
        assert!(
            (1..=3).contains(&id),
            "orion ranked a row that is not hers: {id}"
        );
    }

    // vega likewise sees only her two memories.
    db.set_session_user("vega");
    let vega_ids = ids(&rows(&mut db, "SELECT id FROM memories ORDER BY id"));
    assert_eq!(vega_ids, [4, 5]);

    // The memory layer is durable: drop the engine (a crash) and reopen. Schema,
    // rows, embeddings, roles, and the isolation policy all return.
    drop(db);
    let mut db = Database::open(&path).expect("reopen after crash");

    db.set_session_user("orion");
    let after = rows(
        &mut db,
        "SELECT id, embedding FROM memories ORDER BY embedding <-> '[0, 1, 0, 0]'",
    );
    // orion still sees exactly her three rows, nearest to [0,1,0,0] first (row 2).
    assert_eq!(ids(&after), [2, 1, 3]);
    assert_eq!(after[0][1], Value::Vector(vec![0.0, 1.0, 0.0, 0.0]));

    // Isolation survives the crash too: vega's data is still invisible to orion.
    let leak = rows(&mut db, "SELECT id FROM memories WHERE tenant = 'vega'");
    assert!(
        leak.is_empty(),
        "another tenant's memory leaked after recovery"
    );
}
