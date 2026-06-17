//! End-to-end tests that the buffer pool's WAL ordering hook calls
//! through to the real `WalWriter` and that pages are not flushed before
//! their corresponding WAL records become durable.

use std::cell::RefCell;
use std::rc::Rc;

use picklejar_storage::{
    header::PageHeader, BufferPool, FileManager, PageId, WalSyncHook, PAGE_SIZE,
};
use picklejar_wal::{LogRecord, Lsn, TxnId, WalSyncHandle, WalWriter};

/// Test double that records every `fsync_through` call. Lets tests
/// assert WAL ordering happened with the expected LSN.
#[derive(Debug, Default)]
struct RecordingHook {
    calls: RefCell<Vec<u64>>,
}

impl RecordingHook {
    fn calls(&self) -> Vec<u64> {
        self.calls.borrow().clone()
    }
}

impl WalSyncHook for RecordingHook {
    fn fsync_through(&self, page_lsn: u64) -> std::io::Result<()> {
        self.calls.borrow_mut().push(page_lsn);
        Ok(())
    }
}

#[test]
fn wal_hook_fires_on_flush_page_with_dirty_lsn() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = FileManager::open(dir.path().join("data.db")).expect("open");
    let hook: Rc<RecordingHook> = Rc::new(RecordingHook::default());
    let pool = BufferPool::with_wal(file, 4, hook.clone());

    let id;
    {
        let (new_id, mut g) = pool.new_page().expect("new");
        id = new_id;
        // Write a real header with a non-zero LSN so the WAL hook receives it.
        let mut header = PageHeader::new_heap();
        header.lsn = 42;
        header.write(g.page_mut());
    }
    // No calls yet because no flush happened.
    assert!(hook.calls().is_empty(), "hook called before flush");

    pool.flush_page(id).expect("flush");
    let calls = hook.calls();
    assert_eq!(calls, vec![42], "hook should fire with page LSN 42");
}

#[test]
fn wal_hook_fires_on_eviction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = FileManager::open(dir.path().join("data.db")).expect("open");
    let hook: Rc<RecordingHook> = Rc::new(RecordingHook::default());
    let pool = BufferPool::with_wal(file, 2, hook.clone());

    let mut ids = Vec::new();
    for lsn in 10u64..=14 {
        let (id, mut g) = pool.new_page().expect("new");
        let mut h = PageHeader::new_heap();
        h.lsn = lsn;
        h.write(g.page_mut());
        ids.push((id, lsn));
    }

    // Pool size is 2 and we allocated 5 pages, so 3 evictions occurred. Each
    // eviction must have called the hook with the evicted page's LSN.
    let calls = hook.calls();
    assert_eq!(
        calls.len(),
        3,
        "expected 3 eviction-triggered hook calls, got {calls:?}",
    );
    // Every call must be one of the LSNs we wrote (the exact eviction order
    // depends on LRU-K tie-breaking, which is implementation detail).
    let written: std::collections::HashSet<u64> = ids.iter().map(|&(_, lsn)| lsn).collect();
    for &lsn in &calls {
        assert!(
            written.contains(&lsn),
            "hook called with unexpected LSN {lsn}",
        );
    }
}

#[test]
fn wal_hook_skipped_when_page_lsn_is_zero() {
    // Freshly allocated pages have an LSN of 0 (the "never been logged"
    // sentinel). The buffer pool skips the hook in that case because
    // there are no WAL records to wait for.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = FileManager::open(dir.path().join("data.db")).expect("open");
    let hook: Rc<RecordingHook> = Rc::new(RecordingHook::default());
    let pool = BufferPool::with_wal(file, 4, hook.clone());

    let id;
    {
        let (new_id, mut g) = pool.new_page().expect("new");
        // Touch a byte but do NOT write a non-zero LSN.
        g.page_mut()[100] = 0x77;
        id = new_id;
    }
    pool.flush_page(id).expect("flush");
    assert!(
        hook.calls().is_empty(),
        "page with LSN=0 should not trigger WAL fsync",
    );
}

#[test]
fn real_wal_writer_round_trip_through_buffer_pool() {
    // Wires the actual WalWriter (not a test double) through the buffer
    // pool. Asserts that:
    // 1. The hook causes the WAL to fsync at the right LSN.
    // 2. The on-disk WAL bytes are present after the flush.
    let dir = tempfile::tempdir().expect("tempdir");
    let wal_path = dir.path().join("wal.log");
    let writer = WalWriter::open(&wal_path).expect("wal open");
    let handle = WalSyncHandle::new(writer);

    // Append a few records and remember the highest LSN.
    let mut last_lsn = Lsn::INVALID;
    for i in 1u64..=5 {
        let lsn = handle
            .writer()
            .append(&LogRecord::Begin, TxnId::new(i), Lsn::INVALID)
            .expect("append");
        last_lsn = lsn;
    }

    let file = FileManager::open(dir.path().join("data.db")).expect("data open");
    let pool = BufferPool::with_wal(file, 4, handle.as_hook());

    // Create a page whose header LSN matches the last appended WAL LSN.
    let id = {
        let (new_id, mut g) = pool.new_page().expect("new");
        let mut h = PageHeader::new_heap();
        h.lsn = last_lsn.get();
        h.write(g.page_mut());
        new_id
    };

    // Before flush, durable_through should be 0 (nothing fsynced yet).
    assert_eq!(handle.writer().durable_through(), Lsn::new(0));

    // Flushing the dirty page must drive the WAL fsync first.
    pool.flush_page(id).expect("flush");
    assert_eq!(
        handle.writer().durable_through(),
        last_lsn,
        "WAL should be durable through last_lsn after page flush",
    );

    // WAL file should now have bytes on disk.
    let wal_bytes = std::fs::read(&wal_path).expect("wal read");
    assert!(!wal_bytes.is_empty(), "WAL file empty after flush");
}

#[test]
fn pool_without_wal_does_not_panic_on_flush() {
    // The existing BufferPool::new constructor should still work; tests
    // that don't care about WAL ordering use it.
    let dir = tempfile::tempdir().expect("tempdir");
    let file = FileManager::open(dir.path().join("data.db")).expect("open");
    let pool = BufferPool::new(file, 4);
    let (id, mut g) = pool.new_page().expect("new");
    g.page_mut()[0] = 0xAB;
    drop(g);
    pool.flush_page(id).expect("flush");
    assert_eq!(pool.fetch_page(id).expect("read").page()[0], 0xAB);
    // Touch unused imports / consts so the compiler keeps them.
    let _ = (PageId::new(0), PAGE_SIZE);
}
