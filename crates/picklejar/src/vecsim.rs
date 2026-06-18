//! Engine-level deterministic simulation for the vector memory layer.
//!
//! The storage-level simulator in `picklejar-wal` (the `dst` binary) proves that
//! committed rows survive a crash. This simulator runs one level up, through the
//! real [`Database`], and proves the two properties the AI memory layer must hold
//! *together*:
//!
//! - **Durability.** Every committed embedding is present and byte-for-byte
//!   intact after a crash and reopen; every rolled-back embedding is gone.
//! - **Isolation.** After recovery, each tenant still sees exactly its own
//!   embeddings and never another tenant's, enforced by row-level security in the
//!   engine rather than by application code.
//!
//! Every run is driven entirely by one `u64` seed, so any failure replays
//! exactly. The on-disk location is process-unique (it does not affect the
//! simulated logic, only where the bytes live), and is removed when the run ends.
//!
//! The crash model is the engine's real reopen path: the [`Database`] is dropped
//! and reopened, which runs WAL recovery. This is the same model the engine's own
//! in-process recovery tests use. It is complementary to, not a replacement for,
//! the stricter fault-disk model behind the storage-level `dst` simulator.

use std::path::Path;

use crate::{ast, Database, QueryOutcome, Value};

/// `SplitMix64`: a small, fast, fully deterministic PRNG, seeded once per run.
struct Rng(u64);

impl Rng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n` must be non-zero).
    fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// What one simulated run built and verified.
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    /// Number of tenants in the run.
    pub tenants: usize,
    /// Committed embeddings that had to survive the crash.
    pub committed: usize,
    /// Rolled-back embeddings that had to stay gone.
    pub rolled_back: usize,
}

/// Run one seeded crash-and-recover simulation of the vector memory layer.
///
/// Returns the verified [`Outcome`], or an error string naming the first
/// violated invariant (a missing or altered embedding, or a tenant seeing a row
/// that is not its own).
///
/// # Errors
///
/// Returns `Err` if the temporary working directory cannot be created, if a
/// setup statement fails, or if any durability or isolation invariant is
/// violated after recovery.
pub fn run_seed(seed: u64) -> Result<Outcome, String> {
    let dir = std::env::temp_dir().join(format!("pj-vecsim-{}-{seed}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create_dir_all: {e}"))?;
    let result = simulate(seed, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// One committed embedding the oracle expects to find, keyed by its tenant.
type TenantRows = Vec<Vec<(i64, Vec<f32>)>>;

#[allow(clippy::too_many_lines)]
fn simulate(seed: u64, dir: &Path) -> Result<Outcome, String> {
    let mut rng = Rng::new(seed);
    let dim = usize::try_from(2 + rng.below(6)).expect("small");
    let tenants = usize::try_from(2 + rng.below(3)).expect("small");
    let ops = 20 + rng.below(80);

    let path = dir.join("mem.db");
    let mut db = Database::open(&path).map_err(|e| format!("open: {e}"))?;

    // Schema: a multi-tenant embedding table fenced by a per-tenant RLS policy.
    exec(
        &mut db,
        &format!("CREATE TABLE memories (id INT, tenant TEXT, e VECTOR({dim}))"),
    )?;
    exec(&mut db, "GRANT SELECT ON memories TO PUBLIC")?;
    for t in 0..tenants {
        exec(&mut db, &format!("CREATE ROLE t{t} LOGIN"))?;
    }
    exec(
        &mut db,
        "CREATE POLICY tenant ON memories USING ((tenant = current_user()))",
    )?;
    exec(&mut db, "ALTER TABLE memories ENABLE ROW LEVEL SECURITY")?;

    // Workload: random embeddings across tenants, a quarter of them rolled back.
    let mut oracle: TenantRows = vec![Vec::new(); tenants];
    let mut committed = 0usize;
    let mut rolled_back = 0usize;
    for i in 0..ops {
        let t = usize::try_from(rng.below(tenants as u64)).expect("small");
        let id = i64::try_from(i).expect("op count fits i64") + 1;
        let embedding = random_vector(&mut rng, dim);
        let stmt = format!(
            "INSERT INTO memories VALUES ({id}, 't{t}', '{}')",
            ast::format_vector(&embedding)
        );
        if rng.below(4) == 0 {
            // A rolled-back transaction: its row must never come back.
            exec(&mut db, "BEGIN")?;
            exec(&mut db, &stmt)?;
            exec(&mut db, "ROLLBACK")?;
            rolled_back += 1;
        } else {
            exec(&mut db, &stmt)?;
            oracle[t].push((id, embedding));
            committed += 1;
        }
    }

    // The crash: drop the engine and reopen, which runs WAL recovery.
    drop(db);
    let mut db = Database::open(&path).map_err(|e| format!("reopen: {e}"))?;

    // Durability, from the superuser's unfenced view: exactly the committed rows
    // survive, no rolled-back row reappears.
    let total = rows(&mut db, "SELECT id FROM memories")?.len();
    if total != committed {
        return Err(format!(
            "seed {seed}: durability: expected {committed} committed rows, found {total}"
        ));
    }

    // A zero-vector probe of the right width for the nearest-neighbor checks.
    let probe = ast::format_vector(&vec![0.0f32; dim]);

    // Isolation plus durability, per tenant: each tenant sees exactly its own
    // committed embeddings, intact, and nothing belonging to anyone else.
    for (t, want) in oracle.iter().enumerate() {
        db.set_session_user(&format!("t{t}"));
        let got = rows(&mut db, "SELECT id, e FROM memories ORDER BY id")?;
        let expected: Vec<Vec<Value>> = want
            .iter()
            .map(|(id, v)| vec![Value::Int(*id), Value::Vector(v.clone())])
            .collect();
        if got != expected {
            return Err(format!(
                "seed {seed}: tenant t{t}: expected {} rows, found {} (isolation or durability violated)",
                expected.len(),
                got.len()
            ));
        }
        // A nearest-neighbor query must also stay inside the tenant's own rows:
        // every row a tenant's KNN ranks must carry that tenant's own label.
        let knn = rows(
            &mut db,
            &format!("SELECT tenant FROM memories ORDER BY e <-> '{probe}' LIMIT 1000"),
        )?;
        let mine = Value::Text(format!("t{t}"));
        if knn.iter().any(|row| row.first() != Some(&mine)) {
            return Err(format!(
                "seed {seed}: tenant t{t}: a KNN result leaked another tenant's row"
            ));
        }
    }

    Ok(Outcome {
        tenants,
        committed,
        rolled_back,
    })
}

/// A random embedding of `dim` integer-valued components in `[-1000, 1000]`.
/// Integer-valued `f32`s in that range format and parse back exactly, so the
/// stored embedding round-trips the SQL literal path with no float-formatting
/// ambiguity to muddy the durability check.
fn random_vector(rng: &mut Rng, dim: usize) -> Vec<f32> {
    (0..dim)
        .map(|_| {
            let raw = i16::try_from(rng.below(2001)).expect("0..=2000 fits i16");
            f32::from(raw - 1000)
        })
        .collect()
}

/// Run a non-query statement, mapping any failure to a descriptive string.
fn exec(db: &mut Database, sql: &str) -> Result<(), String> {
    db.execute(sql)
        .map(|_| ())
        .map_err(|e| format!("exec `{sql}`: {e}"))
}

/// Run a query and return its rows.
fn rows(db: &mut Database, sql: &str) -> Result<Vec<Vec<Value>>, String> {
    match db.execute(sql).map_err(|e| format!("exec `{sql}`: {e}"))? {
        QueryOutcome::Rows { rows, .. } => Ok(rows),
        other => Err(format!("expected rows from `{sql}`, got {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::run_seed;

    #[test]
    fn a_sweep_of_seeds_holds_durability_and_isolation() {
        // A small sweep keeps the routine test suite fast; the `vecsim` binary
        // sweeps tens of thousands of seeds on demand in release.
        for seed in 0..8 {
            run_seed(seed).unwrap_or_else(|e| panic!("seed {seed} failed: {e}"));
        }
    }

    #[test]
    fn the_same_seed_is_reproducible() {
        let a = run_seed(7).expect("seed 7");
        let b = run_seed(7).expect("seed 7 again");
        assert_eq!(a.committed, b.committed);
        assert_eq!(a.rolled_back, b.rolled_back);
        assert_eq!(a.tenants, b.tenants);
    }
}
