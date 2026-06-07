//! Crash-test harness: a process that writes committed rows forever and is
//! meant to be force-killed.
//!
//! Used by the torture test (`tests/torture.rs`). It drives a [`MiniHeap`]
//! workload against a database directory, committing one row per iteration,
//! and records each durably-committed row to a ground-truth file *after*
//! the commit is on disk. The test kills this process at an arbitrary
//! point, runs recovery, and checks that every ground-truth row survived.
//!
//! Usage: `crash_harness <db_dir> <truth_file>`
//!
//! The harness never exits on its own. It is designed to die by
//! `TerminateProcess` (Windows) / `SIGKILL`, which is the whole point:
//! proving the database survives an abrupt, uncooperative crash.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use rustdb_storage::{BufferPool, FileManager};
use rustdb_wal::{MiniHeap, WalSyncHandle, WalWriter};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: crash_harness <db_dir> <truth_file>");
        std::process::exit(2);
    }
    let db_dir = Path::new(&args[1]);
    let truth_path = Path::new(&args[2]);
    let data_path = db_dir.join("data.db");
    let wal_path = db_dir.join("wal.log");

    let writer = WalWriter::open(&wal_path).expect("open wal");
    let wal = WalSyncHandle::new(writer);
    let file = FileManager::open(&data_path).expect("open data");
    // A roomy pool so most pages stay dirty (unflushed) until a periodic
    // flush, giving recovery a realistic mix of on-disk and lost pages.
    let pool = BufferPool::with_wal(file, 256, wal.as_hook());
    // `pool` already holds its own hook handle (as_hook clones the inner Rc),
    // so move `wal` into the table; we do not need it again here.
    let heap = MiniHeap::create(&pool, wal).expect("create table");

    let mut truth = OpenOptions::new()
        .create(true)
        .append(true)
        .open(truth_path)
        .expect("open truth file");

    let mut key: u64 = 0;
    loop {
        // One committed row per iteration.
        let mut txn = heap.begin().expect("begin");
        let key_bytes = key.to_le_bytes();
        let (page, slot) = heap.insert(&mut txn, &key_bytes).expect("insert");
        heap.commit(&mut txn).expect("commit");

        // Record ground truth ONLY after the commit is durable. If we are
        // killed before this line, the row is still committed in the DB but
        // simply will not be checked, which keeps the test conservative.
        writeln!(truth, "{key},{},{}", page.get(), slot.get()).expect("write truth");
        truth.flush().expect("flush truth");
        truth.sync_all().expect("sync truth");

        // Periodically flush the pool so the database file holds a moving
        // mix of durable and not-yet-durable pages when the kill lands.
        if key % 64 == 0 {
            pool.flush_all().expect("flush pool");
        }

        key += 1;
    }
}
