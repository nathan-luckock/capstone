---
name: db-debug
description: Use when inspecting low-level database files (pages, WAL segments, B+ tree nodes), debugging buffer-pool / pin-unpin bugs, hunting memory leaks, or analyzing crash logs. Triggers on any binary on-disk format inspection or pointer-lifetime issue in this Rust database engine.
---

# Database Low-Level Debugging Engine

This skill applies to the CSE 499 Rust database engine. It encodes the debugging muscle memory that's slow to re-derive each session.

## 1. Byte & hex inspection of on-disk files

Whenever a `.db` data file, WAL segment, B+ tree page, or any binary on-disk artifact is involved, **dump the bytes first, reason second**. Don't guess at layout — read it.

- Page-level dump (first 4KB):
  ```bash
  xxd -l 4096 path/to/file.db
  ```
- Stream-friendly hex with offsets:
  ```bash
  hexdump -C path/to/wal-000001.log | head -40
  ```
- On Windows (no xxd): use `Format-Hex` in PowerShell:
  ```powershell
  Format-Hex -Path .\data\page-0.bin -Count 4096
  ```

**Always annotate the first 24 bytes against the page header struct.** Map the literal bytes to:
- Page LSN (u64) — for recovery ordering
- Page type / flags (u8/u16) — leaf vs internal, transaction-header flags
- Slot directory count + free-space pointer (u16 each) — for slotted pages
- Checksum (u32) if present — verify before trusting any other field

If the dump doesn't match the struct, the struct is wrong, the writer is wrong, or endianness flipped somewhere. Don't move on until the bytes line up.

## 2. Buffer pool & pointer lifetime

The buffer pool is where most bugs in this engine will live: pin/unpin mismatches, double-frees, dangling page references, eviction-of-pinned-page invariant violations.

**Invariants to assert in every code path:**
- Every `pin()` has a matching `unpin()` on every return path (including error paths and panics — use RAII `PageGuard` types, not bare pin/unpin calls).
- A pinned page is never evicted. If you can't prove this from the call graph, add a debug-assert in the evictor.
- The dirty bit propagates: any write through a page handle marks the frame dirty before unpin.

**Tooling:**
- `cargo build` with `RUSTFLAGS="-Z sanitizer=address"` (nightly) for ASan-style detection of dangling refs. Pin/unpin bugs often surface as use-after-free.
- For deterministic leak detection, `cargo test --features=leak-detect` with a custom `Drop` impl that panics if a `PageGuard` is dropped while still pinned.
- `RUST_LOG=rustdb::buffer=trace` to see pin/unpin call sequences during a failing test.

## 3. WAL & crash-recovery debugging

When the torture test fails or recovery produces wrong data:

1. **Hex-dump the WAL** up to the crash point — confirm log records have valid LSNs and prev-LSN pointers (no torn writes).
2. **Replay only the redo phase first, then undo** — log the per-LSN action; cross-reference against expected page state.
3. **Diff the recovered DB file against a clean shutdown snapshot** with `cmp -l` or a Python script that prints first differing offset.
4. If a CLR (compensation log record) is missing, the abort path didn't fsync — that's almost always the bug.

## 4. Reporting findings

When reporting a low-level bug:
- Quote the exact bytes (offset + hex).
- Show the struct definition you expected.
- Name the invariant that was violated.

Don't write "WAL is corrupted." Write "WAL segment 000003 at offset 0x1A40 shows page LSN 0x0000_0000_0000_07F2, but page 42's in-memory header has LSN 0x0000_0000_0000_0801 — page was modified after the WAL record was written, violating WAL ordering."
