# rustdb — Design Document

> Working document. Updated as decisions are made. The goal is for any reviewer (and future-you) to be able to reconstruct **why** the engine is shaped the way it is from this single file.

## Goals

A relational database engine built from scratch with:
1. SQL interface (CREATE, INSERT, SELECT/WHERE, plus UPDATE, DELETE, JOIN, GROUP BY).
2. ACID transactions: durability via WAL, atomicity via ARIES-style undo, isolation via MVCC.
3. A cost-based query planner that picks between scan and join strategies using table statistics.
4. A live demo proving crash safety: forced kill mid-write, restart, no data loss.

Non-goals: distributed replication, network protocol compatibility with Postgres, performance parity with mature engines.

---

## High-level architecture

```
        ┌──────────────────────────┐
        │     rustdb-cli (REPL)    │
        └────────────┬─────────────┘
                     │ rustdb::Database::query()
        ┌────────────▼─────────────┐
        │     executor (Volcano)   │
        └────────────┬─────────────┘
                     │ physical plan
        ┌────────────▼─────────────┐
        │    planner (cost-based)  │
        └────────────┬─────────────┘
                     │ logical plan
        ┌────────────▼─────────────┐
        │       sql (parser)       │
        └──────────────────────────┘

           ───── all of the above flow through ─────

        ┌──────────────────────────┐
        │   txn manager + MVCC     │
        └────────────┬─────────────┘
                     │ pin / read / write / log
        ┌────────────▼─────────────┐
        │       buffer pool        │
        └────────────┬─────────────┘
                     │
        ┌────────────▼─────────────┐
        │  page manager + B+ tree  │   ←── disk
        └──────────────────────────┘
        ┌──────────────────────────┐
        │  WAL  +  recovery mgr    │   ←── disk (separate file)
        └──────────────────────────┘
```

---

## Storage layer

### Page size

**Decision: 8 KiB.** Matches Postgres default. Big enough to amortize per-page overhead, small enough that buffer-pool memory ratio is reasonable.

### Slotted-page format (heap tables)

```
┌─────────────────────────────────────────────────────────────────┐
│ page header (24 bytes)                                          │
├─────────────────────────────────────────────────────────────────┤
│ slot directory (grows downward)                                 │
│   slot[0]: (offset u16, length u16)                             │
│   slot[1]: ...                                                  │
│                          ↓                                      │
│                       free space                                │
│                          ↑                                      │
│   tuple data (grows upward)                                     │
└─────────────────────────────────────────────────────────────────┘
```

Page header (24 bytes, little-endian):

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 8 | `lsn: u64` | Last LSN that touched this page. WAL ordering anchor. |
| 8 | 4 | `checksum: u32` | CRC32 of `[12..PAGE_SIZE]`, verified on read. |
| 12 | 2 | `page_type: u16` | Heap / B+ tree internal / B+ tree leaf / overflow. |
| 14 | 2 | `slot_count: u16` | Number of slots (live + tombstoned). |
| 16 | 2 | `free_space_ptr: u16` | Offset where the free region ends (tuples grow up from here). |
| 18 | 2 | `flags: u16` | Bit 0 = dirty (in-memory only), 1 = needs vacuum. |
| 20 | 4 | `reserved` | Zero. Reserved for MVCC chain pointer or similar. |

### B+ tree

Branching factor TBD (target ~128 for 8 KiB pages with u64 keys). Internal node = sorted keys + child page IDs. Leaf node = sorted keys + tuple references. Sibling pointer in leaves for range scans.

### Buffer pool

LRU-K (K=2) replacement. Pin/unpin via RAII `PageGuard`. Pinned pages are evict-immune. Dirty bit set on first write through a guard.

---

## WAL & recovery

### Log record layout

Variable-length records, prefixed with length + type:

```
┌────────────────────────────────────────────────┐
│ length: u32                                    │
│ type: u8         (BEGIN, UPDATE, COMMIT, ABORT,│
│                   CHECKPOINT, CLR)             │
│ lsn: u64                                       │
│ txn_id: u64                                    │
│ prev_lsn: u64    (txn's previous record, for   │
│                   undo chain traversal)        │
│ payload: [u8]    (per-type)                    │
│ checksum: u32                                  │
└────────────────────────────────────────────────┘
```

### Three-phase recovery (ARIES)

1. **Analysis.** Scan from last checkpoint, rebuild the active transaction table + dirty page table.
2. **Redo.** Replay every log record from the earliest dirty-page recovery LSN forward, applying any update whose page LSN < record LSN.
3. **Undo.** For every transaction still active at crash time, walk back via `prev_lsn` and write compensation log records (CLRs).

### Invariant (WAL ordering)

A dirty page cannot be flushed before its corresponding log records are fsync'd. Enforced by the buffer pool's flush path: before write-back, look up the page's LSN, ensure WAL has fsync'd through that LSN.

---

## Transactions + MVCC

Snapshot isolation as the default. Each tuple carries `xmin` (creating txn) and `xmax` (deleting txn). A reader at snapshot S sees tuple T iff `xmin(T) ≤ S` and (`xmax(T)` is null or `xmax(T) > S`).

Lock manager exists primarily for DDL and unique-index enforcement; reads under SI don't take row locks.

---

## SQL parser

Hand-written. Lexer produces a flat token stream; recursive-descent parser produces an AST. Pratt-style precedence for expressions.

Target subset:
- DDL: `CREATE TABLE`, `DROP TABLE`, `CREATE INDEX`.
- DML: `INSERT`, `UPDATE`, `DELETE`.
- Query: `SELECT` with `WHERE`, `GROUP BY`, `ORDER BY`, `LIMIT`, `JOIN` (inner + left).

---

## Planner

1. **Logical plan.** AST → relational algebra tree (Scan, Filter, Project, Join, Aggregate, Sort).
2. **Logical rewrites.** Predicate pushdown, projection pushdown, constant folding.
3. **Physical plan.** Choose between SeqScan vs IndexScan per relation; choose between NestedLoopJoin vs HashJoin per join. Costs from per-table stats (row count, NDV, min/max per column).
4. **`EXPLAIN`** output: pretty-printed plan tree with per-node estimated cost.

---

## Open questions (resolve before the relevant sprint)

- B+ tree fanout: empirical or analytic?
- MVCC garbage collection: epoch-based vs vacuum scan?
- Checkpoint strategy: fuzzy vs sharp?
- Isolation levels above SI: do we ship Serializable (SSI) or stop at SI?

---

## Reference reading (load when relevant)

- Mohan et al., *ARIES: A Transaction Recovery Method Supporting Fine-Granularity Locking and Partial Rollbacks Using Write-Ahead Logging* (1992).
- CMU 15-445 / 15-721 lectures (Pavlo).
- Petrov, *Database Internals*.
- Postgres source — `src/backend/storage/buffer/` and `src/backend/access/transam/xlog.c` as a sanity check on real-world layouts.
