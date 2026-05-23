//! Write-ahead log (WAL) and crash recovery.
//!
//! Implements an ARIES-style log: every page mutation produces a log record
//! with a monotonically increasing LSN, the record is fsync'd before the
//! corresponding dirty page can be flushed (WAL ordering invariant), and on
//! restart a three-phase recovery (analysis → redo → undo) restores the
//! database to a consistent committed state.
//!
//! # Invariant
//!
//! No dirty page is flushed to disk before its log record is durable on disk.

#![forbid(unsafe_code)]
