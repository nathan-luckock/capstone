# Sprint Plan — rustdb capstone

Each sprint = ~1 calendar week, ~9-10 hours of work. Sprint label aligns with the proposal timeline. Each sprint maps to a milestone on GitHub.

| Sprint | Weeks | Theme | Definition of done |
|---|---|---|---|
| 0 | (current) | Bootstrap | Workspace, CI, design doc, this file, first issues filed. |
| 1 | W3 | Storage I: pages & file manager | Read/write 8 KiB pages with header + checksum, slotted-page tuple layout, unit tests. |
| 2 | W4 | Storage II: buffer pool & B+ tree | LRU-K buffer pool with RAII pins, B+ tree insert/search/range-scan over the page manager. |
| 3 | W5 | WAL I: log records & write path | Append-only WAL with checksummed records, fsync-on-commit, LSN ordering. |
| 4 | W6 | WAL II: ARIES recovery | Analysis + redo + undo phases. Crash-restart torture test green. |
| 5 | W7 | Transactions & lock manager | Begin/commit/abort, lock manager for DDL + unique indexes. |
| 6 | W8 | MVCC + isolation | Snapshot isolation, xmin/xmax visibility, version chains, isolation level enum. |
| 7 | W9 | SQL parser | Lexer + recursive-descent parser for the target subset. AST + display impl. |
| 8 | W10 | Planner + cost model | Logical plan, predicate pushdown, physical plan, EXPLAIN output. |
| 9 | W11 | Executor | Volcano iterators: SeqScan, IndexScan, Filter, Project, NestedLoopJoin, HashJoin, Aggregate. CLI wired end-to-end. |
| 10 | W12 | Torture test + polish | Multi-hour crash-restart torture test, EXPLAIN polish, isolation-level wiring, bug-fix pass. |
| 11 | W13 | Demo + write-up + SPED talk | Demo script, recorded video, README polish, presentation slides. |

## Sprint 0 — Bootstrap (this week)

Goals: a clean working repository, CI green on `main`, a design doc that a reviewer can read end-to-end, and Sprint 1 issues filed.

Tasks:
- [ ] PR #1: Cargo workspace + 8 crate stubs + CI + CLAUDE.md + design.md + sprint plan
- [ ] Verify `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace` all pass locally
- [ ] Verify CI green on PR #1
- [ ] File Sprint 1 issues (4-6 issues breaking down the page manager + slotted page format)
- [ ] Create GitHub milestone "Sprint 1 — Storage I"

## Out of scope until decided

- **Web UI / "supabase-like" admin interface.** Live decision: build the core engine + CLI first (all must-haves), then add an HTTP API + web UI as Phase 2 polish for demo wow factor. Logged as future scope; not blocking Sprint 0-10.
- **JOIN beyond inner + left.** Right/full outer joins deferred unless time permits.
- **Distributed replication.** Out of scope for capstone.
