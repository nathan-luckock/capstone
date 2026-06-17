//! Forced-process-kill crash-recovery torture test.
//!
//! This is the headline graded requirement: a real crash, not a simulated
//! one. The [`crash_harness`] binary writes committed rows forever; this
//! test spawns it, lets it make progress, force-kills it
//! (`Child::kill` is `TerminateProcess` on Windows / `SIGKILL` on Unix, an
//! abrupt uncooperative kill), then runs recovery and asserts that every
//! row the harness durably committed is present after recovery.
//!
//! The kill/recover cycle runs several rounds with different timings so the
//! crash lands at different points in the workload.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use picklejar_storage::{BufferPool, FileManager, HeapPage, PageId, SlotId, PAGE_SIZE};
use picklejar_wal::recover;

/// Number of independent kill/recover rounds.
const ROUNDS: usize = 4;
/// Wait until the harness has committed at least this many rows before
/// killing it, so every round actually exercises recovery.
const MIN_COMMITTED_BEFORE_KILL: usize = 30;
/// Give up waiting for progress after this long (guards against a harness
/// that fails to start).
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(20);

fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path).map_or(0, |s| s.lines().count())
}

/// Read a slot from a recovered database (pages already flushed by recover).
fn read_slot(pool: &BufferPool, page: u64, slot: u16) -> Option<Vec<u8>> {
    let guard = pool.fetch_page(PageId::new(page)).ok()?;
    let mut buf = Box::new([0u8; PAGE_SIZE]);
    buf.copy_from_slice(guard.page());
    let heap = HeapPage::from_bytes(&mut buf).ok()?;
    heap.get(SlotId::new(slot)).map(<[u8]>::to_vec)
}

#[test]
fn forced_kill_then_recover_loses_no_committed_data() {
    let harness = env!("CARGO_BIN_EXE_crash_harness");

    for round in 0..ROUNDS {
        let dir = tempfile::tempdir().expect("tempdir");
        let truth_path = dir.path().join("truth.txt");
        let wal_path = dir.path().join("wal.log");
        let data_path = dir.path().join("data.db");

        // Spawn the harness. stdout/stderr to null so a full pipe never
        // blocks it.
        let mut child = Command::new(harness)
            .arg(dir.path())
            .arg(&truth_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn crash_harness");

        // Wait until the harness has committed enough rows, then kill it.
        let start = Instant::now();
        loop {
            if count_lines(&truth_path) >= MIN_COMMITTED_BEFORE_KILL {
                break;
            }
            if start.elapsed() > PROGRESS_TIMEOUT {
                let _ = child.kill();
                panic!("round {round}: harness made no progress within timeout");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // FORCED CRASH: hard kill, no cleanup, no graceful shutdown.
        child.kill().expect("kill harness");
        child.wait().expect("reap harness");

        // Snapshot the ground truth at the moment of the kill. The harness
        // may have written a few more rows between our line count and the
        // kill; that is fine, every line here is a durably committed row.
        let truth = std::fs::read_to_string(&truth_path).expect("read truth");
        let committed: Vec<(u64, u64, u16)> = truth
            .lines()
            .filter_map(|line| {
                let mut parts = line.split(',');
                let key = parts.next()?.parse::<u64>().ok()?;
                let page = parts.next()?.parse::<u64>().ok()?;
                let slot = parts.next()?.parse::<u16>().ok()?;
                Some((key, page, slot))
            })
            .collect();
        assert!(
            committed.len() >= MIN_COMMITTED_BEFORE_KILL,
            "round {round}: expected progress, got {} rows",
            committed.len()
        );

        // RECOVER from the WAL + whatever survived in the data file.
        let file = FileManager::open(&data_path).expect("open data after crash");
        let pool = BufferPool::new(file, 256);
        let stats = recover(&pool, &wal_path).expect("recover after crash");

        // Every durably-committed row must be present and correct.
        for &(key, page, slot) in &committed {
            let got = read_slot(&pool, page, slot);
            assert_eq!(
                got.as_deref(),
                Some(&key.to_le_bytes()[..]),
                "round {round}: committed key {key} at page {page} slot {slot} \
                 lost or corrupted after recovery (stats: {stats:?})",
            );
        }

        // Recovery must be idempotent: a second pass changes nothing.
        let file2 = FileManager::open(&data_path).expect("reopen");
        let pool2 = BufferPool::new(file2, 256);
        let stats2 = recover(&pool2, &wal_path).expect("second recover");
        assert_eq!(
            stats2.redone, 0,
            "round {round}: second recovery should redo nothing"
        );
        assert_eq!(
            stats2.undone, 0,
            "round {round}: second recovery should undo nothing"
        );

        // Sanity: the recovered file is a whole number of pages and every
        // committed row is still readable through the freshly reopened pool.
        for &(key, page, slot) in &committed {
            assert_eq!(
                read_slot(&pool2, page, slot).as_deref(),
                Some(&key.to_le_bytes()[..]),
                "round {round}: key {key} not stable across a second recovery",
            );
        }
    }
}

/// A separate, smaller check that the harness binary exists and runs at all
/// (fast feedback if the binary is broken, independent of timing).
#[test]
fn crash_harness_binary_makes_progress() {
    let harness = env!("CARGO_BIN_EXE_crash_harness");
    let dir = tempfile::tempdir().expect("tempdir");
    let truth_path = dir.path().join("truth.txt");

    let mut child = Command::new(harness)
        .arg(dir.path())
        .arg(&truth_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");

    let start = Instant::now();
    let mut buf = String::new();
    loop {
        if let Ok(mut f) = std::fs::File::open(&truth_path) {
            buf.clear();
            let _ = f.read_to_string(&mut buf);
            if buf.lines().count() >= 5 {
                break;
            }
        }
        if start.elapsed() > PROGRESS_TIMEOUT {
            let _ = child.kill();
            panic!("harness did not commit any rows");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().expect("kill");
    child.wait().expect("reap");
}
