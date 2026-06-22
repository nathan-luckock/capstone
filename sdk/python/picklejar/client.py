"""A small, typed client for the picklejar AI-memory database.

picklejar speaks the PostgreSQL wire protocol, so this client is a thin,
convenient layer over ``psycopg``: it turns the memory operations an agent
actually wants (store an embedding, recall the nearest, forget one) into the
SQL the engine runs, including the ``pgvector``-style distance operators.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Optional, Sequence

import psycopg

# SQL distance operators, by name. picklejar implements the pgvector set.
_METRICS = {
    "l2": "<->",      # Euclidean
    "cosine": "<=>",  # cosine distance (1 - cosine similarity)
    "inner": "<#>",   # negative inner product
    "l1": "<+>",      # Manhattan / taxicab
}


@dataclass
class Memory:
    """One recalled memory and, for a recall, its distance to the query."""

    id: int
    tenant: str
    content: str
    metadata: Optional[dict]
    distance: Optional[float] = None


def _vector_literal(embedding: Sequence[float]) -> str:
    """Render an embedding as the ``[1,2,3]`` literal the engine parses."""
    return "[" + ",".join(repr(float(x)) for x in embedding) + "]"


class MemoryStore:
    """A connection to a picklejar database, scoped to one memory table.

    Each memory carries a ``tenant`` tag and every query is fenced to one
    tenant, so one agent's recall can never surface another's rows. (For
    isolation enforced by the engine itself rather than this client, connect
    as the tenant role and enable row-level security on the table.)
    """

    def __init__(
        self,
        dsn: Optional[str] = None,
        *,
        host: str = "127.0.0.1",
        port: int = 5433,
        user: str = "postgres",
        dbname: str = "postgres",
        table: str = "memories",
        dim: Optional[int] = None,
    ) -> None:
        self.table = table
        self.dim = dim
        if dsn is not None:
            self._conn = psycopg.connect(dsn, autocommit=True)
        else:
            self._conn = psycopg.connect(
                host=host, port=port, user=user, dbname=dbname, autocommit=True
            )

    def __enter__(self) -> "MemoryStore":
        return self

    def __exit__(self, *_exc) -> None:
        self.close()

    def close(self) -> None:
        """Close the underlying connection."""
        self._conn.close()

    def ensure_schema(self, dim: Optional[int] = None) -> "MemoryStore":
        """Create the memory table if it does not exist.

        ``dim`` is the embedding width; pass it here or to the constructor.
        """
        width = dim if dim is not None else self.dim
        if width is None:
            raise ValueError("embedding dimension required (pass dim=...)")
        self.dim = width
        with self._conn.cursor() as cur:
            cur.execute(
                f"CREATE TABLE IF NOT EXISTS {self.table} ("
                "id SERIAL PRIMARY KEY, "
                "tenant TEXT NOT NULL, "
                "content TEXT, "
                "metadata JSON, "
                f"embedding VECTOR({width}))"
            )
        return self

    def store(
        self,
        tenant: str,
        embedding: Sequence[float],
        content: str = "",
        metadata: Optional[dict] = None,
    ) -> int:
        """Store one memory and return its generated id."""
        with self._conn.cursor() as cur:
            cur.execute(
                f"INSERT INTO {self.table} (tenant, content, metadata, embedding) "
                "VALUES (%s, %s, %s, %s) RETURNING id",
                (
                    tenant,
                    content,
                    json.dumps(metadata) if metadata is not None else None,
                    _vector_literal(embedding),
                ),
            )
            return cur.fetchone()[0]

    def recall(
        self,
        tenant: str,
        query: Sequence[float],
        k: int = 5,
        metric: str = "l2",
    ) -> list[Memory]:
        """Return the ``k`` nearest memories for ``tenant`` to ``query``.

        ``metric`` is one of ``l2``, ``cosine``, ``inner``, ``l1``.
        """
        try:
            op = _METRICS[metric]
        except KeyError:
            raise ValueError(
                f"unknown metric {metric!r}; choose from {sorted(_METRICS)}"
            ) from None
        qv = _vector_literal(query)
        # The engine does not accept a parameter in LIMIT, so the (integer)
        # count is inlined; everything else is a bound parameter.
        limit = int(k)
        with self._conn.cursor() as cur:
            cur.execute(
                f"SELECT id, tenant, content, metadata, embedding {op} %s AS distance "
                f"FROM {self.table} WHERE tenant = %s "
                f"ORDER BY embedding {op} %s LIMIT {limit}",
                (qv, tenant, qv),
            )
            return [
                Memory(
                    id=r[0], tenant=r[1], content=r[2], metadata=r[3], distance=r[4]
                )
                for r in cur.fetchall()
            ]

    def forget(self, tenant: str, id: Optional[int] = None) -> int:
        """Delete one memory (by ``id``) or all of a tenant's memories.

        Returns the number of rows deleted.
        """
        with self._conn.cursor() as cur:
            if id is None:
                cur.execute(f"DELETE FROM {self.table} WHERE tenant = %s", (tenant,))
            else:
                cur.execute(
                    f"DELETE FROM {self.table} WHERE tenant = %s AND id = %s",
                    (tenant, int(id)),
                )
            return cur.rowcount
