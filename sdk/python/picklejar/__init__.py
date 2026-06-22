"""picklejar: a Python client for the picklejar AI-memory database.

```python
from picklejar import MemoryStore

mem = MemoryStore(dim=3).ensure_schema()
mem.store("acme", [0.1, 0.2, 0.9], content="the sky is blue")
hits = mem.recall("acme", [0.1, 0.2, 0.8], k=5)
```
"""

from .client import Memory, MemoryStore

__all__ = ["Memory", "MemoryStore"]
__version__ = "0.1.0"
