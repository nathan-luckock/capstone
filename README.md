# capstone — a relational database from scratch (Rust)

> CSE 499 senior project. A real disk-based relational database engine with ACID guarantees, written from scratch in Rust.

Not a SQLite/Postgres wrapper. Not a key-value store with SQL on top. A real engine: page manager, buffer pool, B+ tree indexes, WAL + ARIES-style recovery, MVCC for concurrent reads, a hand-written SQL parser, a cost-based query planner, and a query executor.

## Status

Pre-Sprint-1. Bootstrap in progress.

## What's planned

| Layer | Crate | What it does |
|---|---|---|
| CLI | [`rustdb-cli`](crates/rustdb-cli/) | `psql`-style interactive shell |
| Library entry | [`rustdb`](crates/rustdb/) | Top-level DB handle, embeds all layers |
| Execution | [`executor`](crates/executor/) | Seq scan, index scan, hash join, nested-loop join |
| Optimization | [`planner`](crates/planner/) | Cost-based query planner |
| Parsing | [`sql`](crates/sql/) | Hand-written SQL parser (lexer + recursive-descent) |
| Concurrency | [`txn`](crates/txn/) | Transaction manager + MVCC + lock manager |
| Durability | [`wal`](crates/wal/) | Write-ahead log + ARIES recovery |
| Storage | [`storage`](crates/storage/) | Pages, buffer pool, B+ tree |

## Build

```bash
cargo build --workspace
cargo test --workspace
cargo run --bin rustdb        # CLI
```

## Architecture

See [docs/design.md](docs/design.md).

## How AI is used in this project

This project is built with [Claude Code](https://claude.com/claude-code) as a pair programmer. Every commit ships with a `Design notes:` section documenting what was picked and why. See [CLAUDE.md](CLAUDE.md) for the working agreement.

## License

MIT OR Apache-2.0
