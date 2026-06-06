//! WAL sync hook implementation for the buffer pool.
//!
//! Wraps a [`WalWriter`] behind an `Rc<RefCell<>>` and implements
//! [`WalSyncHook`] so the buffer pool can call `fsync_through` before
//! flushing dirty pages without taking a direct dependency on
//! `rustdb-wal`.
//!
//! [`WalSyncHook`]: rustdb_storage::WalSyncHook

use std::cell::RefCell;
use std::rc::Rc;

use rustdb_storage::WalSyncHook;

use crate::error::WalError;
use crate::lsn::Lsn;
use crate::writer::WalWriter;

/// Shared, interior-mutable handle around a [`WalWriter`]. Implements
/// [`WalSyncHook`] so the storage layer can drive WAL ordering without
/// linking against this crate directly.
#[derive(Debug, Clone)]
pub struct WalSyncHandle {
    writer: Rc<RefCell<WalWriter>>,
}

impl WalSyncHandle {
    /// Wrap `writer` in a shared handle. The handle implements
    /// [`WalSyncHook`] and can be installed on a `BufferPool` via
    /// [`BufferPool::with_wal`](rustdb_storage::BufferPool::with_wal).
    #[must_use]
    pub fn new(writer: WalWriter) -> Self {
        Self {
            writer: Rc::new(RefCell::new(writer)),
        }
    }

    /// Borrow the writer mutably. Useful for the transaction manager and
    /// recovery code that need to call `append` / `current_lsn`.
    #[must_use]
    pub fn writer(&self) -> std::cell::RefMut<'_, WalWriter> {
        self.writer.borrow_mut()
    }

    /// Build an `Rc<dyn WalSyncHook>` that points at the same underlying
    /// writer as `self`. Lets the caller install the hook on a
    /// `BufferPool` while keeping a separate `WalSyncHandle` for direct
    /// WAL writes.
    #[must_use]
    pub fn as_hook(&self) -> Rc<dyn WalSyncHook> {
        Rc::new(HookWrapper {
            writer: self.writer.clone(),
        })
    }
}

/// Internal wrapper because `Rc<RefCell<WalWriter>>` cannot itself implement
/// the foreign `WalSyncHook` trait (orphan rule).
#[derive(Debug)]
struct HookWrapper {
    writer: Rc<RefCell<WalWriter>>,
}

impl WalSyncHook for HookWrapper {
    fn fsync_through(&self, page_lsn: u64) -> std::io::Result<()> {
        let mut writer = self.writer.borrow_mut();
        writer
            .fsync_through(Lsn::new(page_lsn))
            .map_err(|e| match e {
                WalError::Io(io) => io,
                other => std::io::Error::other(other.to_string()),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::LogRecord;
    use crate::TxnId;
    use tempfile::TempDir;

    fn fresh_handle() -> (TempDir, WalSyncHandle) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("wal.log");
        let writer = WalWriter::open(&path).expect("open");
        (dir, WalSyncHandle::new(writer))
    }

    #[test]
    fn hook_fsync_through_makes_records_durable() {
        let (dir, handle) = fresh_handle();
        let lsn = handle
            .writer()
            .append(&LogRecord::Begin, TxnId::new(1), Lsn::INVALID)
            .expect("append");
        let hook = handle.as_hook();
        hook.fsync_through(lsn.get()).expect("fsync");
        assert_eq!(handle.writer().durable_through(), lsn);
        // File should have bytes after the fsync.
        let bytes = std::fs::read(dir.path().join("wal.log")).expect("read");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn hook_fsync_through_zero_lsn_is_noop() {
        // Page header LSN starts at 0 for freshly allocated pages. The
        // hook should treat 0 as "no records to wait for" and not error.
        let (_dir, handle) = fresh_handle();
        let hook = handle.as_hook();
        hook.fsync_through(0).expect("noop");
        assert_eq!(handle.writer().durable_through(), Lsn::new(0));
    }

    #[test]
    fn clone_is_shared_handle() {
        let (_dir, h1) = fresh_handle();
        let h2 = h1.clone();
        let lsn = h1
            .writer()
            .append(&LogRecord::Begin, TxnId::new(1), Lsn::INVALID)
            .expect("append");
        // h2's view of the same underlying writer sees the new LSN.
        assert_eq!(h2.writer().current_lsn(), Lsn::new(lsn.get() + 1));
    }
}
