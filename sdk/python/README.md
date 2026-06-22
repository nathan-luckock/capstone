# picklejar Python client

A thin, typed client for the [picklejar](../../README.md) AI-memory database.
picklejar speaks the PostgreSQL wire protocol, so this is a small convenience
layer over `psycopg`: it turns the operations an agent wants (store an
embedding, recall the nearest, forget one) into the SQL the engine runs.

## Install

```bash
pip install picklejar          # the client
```

Run a server to talk to (from the repo root, or via Docker):

```bash
cargo run --release --bin picklejar-pg -- --database mem.db --port 5433
# or:  docker run -p 5433:5433 -v picklejar-data:/data picklejar
```

## Use

```python
from picklejar import MemoryStore

# Connect and create the memory table (embedding width = 3 here).
mem = MemoryStore(host="127.0.0.1", port=5433, dim=3).ensure_schema()

# Store memories, each tagged with a tenant.
mem.store("acme", [0.1, 0.2, 0.9], content="the sky is blue", metadata={"src": "doc1"})
mem.store("acme", [0.9, 0.1, 0.1], content="fire is hot")

# Recall the nearest, fenced to that tenant's own rows.
for m in mem.recall("acme", [0.1, 0.2, 0.8], k=5):
    print(m.id, m.content, m.distance)

# Forget one memory, or all of a tenant's.
mem.forget("acme", id=2)
```

`MemoryStore` is also a context manager (`with MemoryStore(...) as mem:`) and
accepts a `dsn="postgresql://..."` string instead of host/port keywords.

## API

| Method | Does |
|---|---|
| `ensure_schema(dim=None)` | create the memory table if absent |
| `store(tenant, embedding, content="", metadata=None) -> int` | insert one memory, return its id |
| `recall(tenant, query, k=5, metric="l2") -> list[Memory]` | the `k` nearest, fenced to `tenant` |
| `forget(tenant, id=None) -> int` | delete one memory, or all of a tenant's |

Distance metrics: `l2`, `cosine`, `inner`, `l1`.

Each query is fenced to one `tenant`. For isolation enforced by the engine
itself (not the client), connect as the tenant role and enable row-level
security on the table; similarity search then runs through the same fence.
