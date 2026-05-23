//! Transaction manager, lock manager, and MVCC.
//!
//! Coordinates concurrent transactions over the storage + WAL layers.
//! Provides snapshot isolation as the baseline; isolation level configuration
//! is a nice-to-have.

#![forbid(unsafe_code)]
