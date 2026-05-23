//! Storage layer for the rustdb engine.
//!
//! Provides page-based on-disk storage, a buffer pool with pin/unpin semantics,
//! and a B+ tree index. Everything above this crate (WAL, MVCC, SQL) treats
//! storage as the canonical source of truth for committed data — but mutations
//! flow through the WAL first (see `rustdb-wal`).
//!
//! # Layout (TBD — finalize in docs/design.md)
//!
//! - Page size: likely 8 KiB
//! - Slotted-page format for heap tables
//! - B+ tree pages: separate header layout, see `btree` module
//!
//! # Invariants
//!
//! - A pinned page is never evicted.
//! - All page handles are RAII; dropping a `PageGuard` unpins exactly once.
//! - Writes go through the buffer pool, never directly to disk.

#![forbid(unsafe_code)]
