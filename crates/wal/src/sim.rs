//! Deterministic simulation testing (DST) for crash recovery.
//!
//! Every run is driven entirely by a single `u64` seed, so any failure is
//! reproduced exactly by replaying that seed. This turns crash recovery from
//! "we kill a process and hope" into "we explore thousands of randomized crash
//! scenarios, reproducibly, in milliseconds."
//!
//! # The model
//!
//! The data file is a [`FaultDisk`]: an in-memory block device that models
//! durability explicitly. A `write_page` only stages bytes; they become durable
//! only on `fsync`. A simulated crash keeps the durable image and throws away
//! everything written-but-not-fsynced. This is stricter than dropping a real
//! buffer pool in a test, where the OS page cache would still hand back
//! un-fsynced writes and hide durability bugs.
//!
//! The WAL is a real (fsynced) file, because the engine relies on the OS to
//! make log records durable on commit; that is the one thing a crash must not
//! lose.
//!
//! # The invariant
//!
//! For a seed, the simulator builds a random workload of inserting
//! transactions (committed, aborted, or left in-flight at the crash), flushing
//! the page cache at random points so the durable/lost split varies. It records
//! an oracle: exactly which rows a correct database must hold after recovery.
//! Then it crashes, runs ARIES recovery over the durable image plus the WAL,
//! and checks the recovered database against the oracle:
//!
//! - every committed row is present and intact, and
//! - every aborted or in-flight row is absent (rolled back).
//!
//! A single mismatch fails the run and reports the seed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rustdb_storage::{
    BufferPool, Disk, HeapPage, Page, PageId, Result as StorageResult, SlotId, StorageError,
    PAGE_SIZE,
};

use crate::hook::WalSyncHandle;
use crate::recovery::recover;
use crate::workload::MiniHeap;
use crate::writer::WalWriter;

/// A small, fast, fully deterministic PRNG (`SplitMix64`). Seeded once per run.
#[derive(Debug)]
pub struct Rng(u64);

impl Rng {
    /// Seed the generator.
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..n` (`n` must be non-zero).
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    /// True with probability `pct`/100.
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// The durable and pending state behind a [`FaultDisk`].
#[derive(Debug)]
struct DiskState {
    /// Pages that survived the last `fsync`, indexed by page id.
    durable: Vec<Page>,
    /// Pages written since the last `fsync` (lost on a crash).
    pending: HashMap<u64, Page>,
    /// Total pages allocated this session (`>= durable.len()`).
    allocated: u64,
}

/// An in-memory block device that models durability so a crash can be simulated
/// deterministically: only `fsync`-ed writes survive.
///
/// Cloning shares the same underlying state (like a file handle), so the
/// simulator keeps a handle after moving one into the buffer pool.
#[derive(Debug, Clone)]
pub struct FaultDisk {
    state: Rc<RefCell<DiskState>>,
}

impl FaultDisk {
    /// A fresh, empty device.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            state: Rc::new(RefCell::new(DiskState {
                durable: Vec::new(),
                pending: HashMap::new(),
                allocated: 0,
            })),
        }
    }

    /// A device seeded with `pages` as its durable content and nothing pending,
    /// as a crash leaves it.
    #[must_use]
    pub fn from_durable(pages: Vec<Page>) -> Self {
        let allocated = pages.len() as u64;
        Self {
            state: Rc::new(RefCell::new(DiskState {
                durable: pages,
                pending: HashMap::new(),
                allocated,
            })),
        }
    }

    /// Snapshot the durable image: exactly the pages that would survive a crash
    /// right now (every `fsync`-ed write, none of the pending ones).
    #[must_use]
    pub fn crash(&self) -> Vec<Page> {
        self.state.borrow().durable.clone()
    }
}

impl Disk for FaultDisk {
    fn allocate_page(&mut self) -> StorageResult<PageId> {
        let mut s = self.state.borrow_mut();
        let id = s.allocated;
        s.allocated += 1;
        Ok(PageId::new(id))
    }

    fn read_page(&mut self, id: PageId, buf: &mut Page) -> StorageResult<()> {
        let s = self.state.borrow();
        if id.get() >= s.allocated {
            return Err(StorageError::PageOutOfBounds {
                requested: id.get(),
                page_count: s.allocated,
            });
        }
        let index = usize::try_from(id.get()).unwrap_or(usize::MAX);
        if let Some(page) = s.pending.get(&id.get()) {
            buf.copy_from_slice(page);
        } else if let Some(page) = s.durable.get(index) {
            buf.copy_from_slice(page);
        } else {
            // Allocated this session but never written and not yet durable.
            buf.fill(0);
        }
        Ok(())
    }

    fn write_page(&mut self, id: PageId, buf: &Page) -> StorageResult<()> {
        let mut s = self.state.borrow_mut();
        if id.get() >= s.allocated {
            return Err(StorageError::PageOutOfBounds {
                requested: id.get(),
                page_count: s.allocated,
            });
        }
        s.pending.insert(id.get(), *buf);
        Ok(())
    }

    fn fsync(&mut self) -> StorageResult<()> {
        let mut s = self.state.borrow_mut();
        let allocated = usize::try_from(s.allocated).unwrap_or(usize::MAX);
        while s.durable.len() < allocated {
            s.durable.push([0u8; PAGE_SIZE]);
        }
        let pending: Vec<(u64, Page)> = s.pending.drain().collect();
        for (id, page) in pending {
            let index = usize::try_from(id).unwrap_or(usize::MAX);
            s.durable[index] = page;
        }
        Ok(())
    }

    fn page_count(&self) -> u64 {
        self.state.borrow().allocated
    }
}

/// What a recovered database should look like at one slot.
struct Expected {
    page: PageId,
    slot: SlotId,
    /// `Some(bytes)` if the row was committed (must survive); `None` if it was
    /// aborted or in-flight at the crash (must be rolled back).
    row: Option<Vec<u8>>,
}

/// The result of one successful simulation run, for reporting.
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    /// Rows committed before the crash (and so required to survive).
    pub committed: usize,
    /// Rows aborted or in-flight at the crash (and so required to be gone).
    pub rolled_back: usize,
    /// Winners reported by recovery.
    pub winners: usize,
    /// Records redone by recovery.
    pub redone: usize,
    /// Records undone by recovery.
    pub undone: usize,
}

/// Run one deterministic crash-recovery simulation for `seed`.
///
/// Returns the run's [`Outcome`] on success, or an `Err` describing the first
/// broken invariant (the message includes the seed so it can be replayed).
///
/// # Errors
///
/// Returns an error string if any committed row is missing or corrupt after
/// recovery, any rolled-back row survives, recovery itself fails, or the
/// underlying WAL/storage errors.
#[allow(clippy::too_many_lines)]
pub fn run_seed(seed: u64) -> std::result::Result<Outcome, String> {
    let mut rng = Rng::new(seed);

    // The WAL is a real, fsynced file; the data file is the fault disk.
    let dir = tempfile::tempdir().map_err(|e| format!("seed {seed}: tempdir failed: {e}"))?;
    let wal_path = dir.path().join("wal.log");
    let disk = FaultDisk::empty();

    let mut expected: Vec<Expected> = Vec::new();
    let mut committed = 0usize;
    let mut committed_txns = 0usize;
    let mut rolled_back = 0usize;

    // --- Workload phase ---
    {
        let writer =
            WalWriter::open(&wal_path).map_err(|e| format!("seed {seed}: open wal: {e}"))?;
        let wal = WalSyncHandle::new(writer);
        // A small pool forces eviction, so pages move to the disk (and are lost
        // on crash unless a flush has fsynced them) mid-workload.
        let pool = BufferPool::with_wal(disk.clone(), 4, wal.as_hook());
        let heap = MiniHeap::create(&pool, wal.clone())
            .map_err(|e| format!("seed {seed}: create heap: {e}"))?;

        let txn_count = rng.below(30) + 5;
        let mut next_key: u64 = 0;
        for t in 0..txn_count {
            let mut txn = heap
                .begin()
                .map_err(|e| format!("seed {seed}: begin: {e}"))?;
            let inserts = rng.below(5) + 1;
            let mut rows: Vec<(PageId, SlotId, Vec<u8>)> = Vec::new();
            for _ in 0..inserts {
                let len = usize::try_from(rng.below(112) + 8).unwrap_or(8);
                let mut tuple = vec![0u8; len];
                tuple[..8.min(len)].copy_from_slice(&next_key.to_le_bytes()[..8.min(len)]);
                next_key += 1;
                let (page, slot) = heap
                    .insert(&mut txn, &tuple)
                    .map_err(|e| format!("seed {seed}: insert: {e}"))?;
                rows.push((page, slot, tuple));
            }

            // Decide this transaction's fate. The last transaction may be left
            // in-flight to simulate a crash mid-transaction.
            let last = t == txn_count - 1;
            let fate = rng.below(100);
            if fate < 70 {
                heap.commit(&mut txn)
                    .map_err(|e| format!("seed {seed}: commit: {e}"))?;
                committed_txns += 1;
                for (page, slot, tuple) in rows {
                    committed += 1;
                    expected.push(Expected {
                        page,
                        slot,
                        row: Some(tuple),
                    });
                }
            } else if fate < 90 || !last {
                heap.abort(&mut txn)
                    .map_err(|e| format!("seed {seed}: abort: {e}"))?;
                for (page, slot, _) in rows {
                    rolled_back += 1;
                    expected.push(Expected {
                        page,
                        slot,
                        row: None,
                    });
                }
            } else {
                // Leave in-flight: never commit or abort.
                for (page, slot, _) in rows {
                    rolled_back += 1;
                    expected.push(Expected {
                        page,
                        slot,
                        row: None,
                    });
                }
            }

            // Randomly flush, fsyncing the disk so some pages become durable
            // while others remain only in the (about to be lost) pool.
            if rng.chance(25) {
                pool.flush_all()
                    .map_err(|e| format!("seed {seed}: flush: {e}"))?;
            }
        }

        // CRASH: drop the pool (losing dirty in-memory pages) and keep only the
        // disk's durable image. The WAL file on disk survives intact.
        drop(heap);
        drop(pool);
        drop(wal);
    }

    let durable = disk.crash();

    // --- Recovery phase ---
    let recovered = FaultDisk::from_durable(durable);
    let pool = BufferPool::new(recovered, 16);
    let stats =
        recover(&pool, &wal_path).map_err(|e| format!("seed {seed}: recovery failed: {e}"))?;

    if stats.winners != committed_txns {
        return Err(format!(
            "seed {seed}: recovery reported {} winners but {committed_txns} transactions committed",
            stats.winners
        ));
    }

    // --- Verification phase ---
    for e in &expected {
        let got = read_slot(&pool, e.page, e.slot)
            .map_err(|err| format!("seed {seed}: read {:?}/{:?}: {err}", e.page, e.slot))?;
        match (&e.row, got) {
            (Some(want), Some(have)) if *want == have => {}
            (Some(want), have) => {
                return Err(format!(
                    "seed {seed}: committed row at {:?}/{:?} corrupt: wanted {} bytes, got {:?}",
                    e.page,
                    e.slot,
                    want.len(),
                    have.map(|h| h.len()),
                ));
            }
            (None, None) => {}
            (None, Some(_)) => {
                return Err(format!(
                    "seed {seed}: rolled-back row at {:?}/{:?} survived recovery",
                    e.page, e.slot
                ));
            }
        }
    }

    Ok(Outcome {
        committed,
        rolled_back,
        winners: stats.winners,
        redone: stats.redone,
        undone: stats.undone,
    })
}

/// Read a slot's bytes through `pool` (used to verify the recovered database).
fn read_slot(pool: &BufferPool, page: PageId, slot: SlotId) -> StorageResult<Option<Vec<u8>>> {
    let guard = pool.fetch_page(page)?;
    let mut buf = Box::new([0u8; PAGE_SIZE]);
    buf.copy_from_slice(guard.page());
    let heap = HeapPage::from_bytes(&mut buf)?;
    Ok(heap.get(slot).map(<[u8]>::to_vec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_disk_loses_unfsynced_writes() {
        let mut disk = FaultDisk::empty();
        let id = disk.allocate_page().unwrap();
        let mut page = [7u8; PAGE_SIZE];
        disk.write_page(id, &page).unwrap();
        // Not fsynced yet: a crash snapshot has no durable pages.
        assert!(disk.crash().is_empty());
        disk.fsync().unwrap();
        // Now it survives.
        assert_eq!(disk.crash().len(), 1);
        // A further write without fsync is lost on crash.
        page.fill(9);
        disk.write_page(id, &page).unwrap();
        let durable = disk.crash();
        assert_eq!(durable[0][0], 7, "crash keeps the last fsynced bytes");
    }

    #[test]
    fn a_handful_of_seeds_recover_correctly() {
        for seed in 0..16u64 {
            run_seed(seed).unwrap_or_else(|e| panic!("{e}"));
        }
    }
}
