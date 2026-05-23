//! Volcano/iterator-style query executor.
//!
//! Each physical plan node implements an iterator interface (`open` / `next` /
//! `close`). Supports seq scan, index scan, hash join, and nested-loop join.

#![forbid(unsafe_code)]
