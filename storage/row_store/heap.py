"""QM Storage — Row Store (Heap-based page layout).

Implements:
  - Fixed-size pages (default 8KB)
  - Tuple header (txn_id, flags, null bitmap)
  - Slot array for tuple offsets
  - Free space management
  - HOT (Heap-Only Tuple) updates
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from typing import Any


PAGE_SIZE = 8192  # 8KB default page size
TUPLE_HEADER_SIZE = 24  # txn_id(8) + flags(4) + null_bitmap(4) + length(4) + version(4)


@dataclass
class TupleHeader:
    """Header for each tuple in the row store."""

    txn_id: int = 0
    flags: int = 0  # bit flags: deleted, hot-updated, etc.
    null_bitmap: int = 0
    data_length: int = 0
    version: int = 1

    FLAG_DELETED = 0x01
    FLAG_HOT_UPDATED = 0x02
    FLAG_LOCKED = 0x04

    @property
    def is_deleted(self) -> bool:
        return bool(self.flags & self.FLAG_DELETED)

    @property
    def is_hot_updated(self) -> bool:
        return bool(self.flags & self.FLAG_HOT_UPDATED)

    def mark_deleted(self) -> None:
        self.flags |= self.FLAG_DELETED

    def pack(self) -> bytes:
        return struct.pack(
            "<QIIiI",
            self.txn_id,
            self.flags,
            self.null_bitmap,
            self.data_length,
            self.version,
        )

    @classmethod
    def unpack(cls, data: bytes) -> TupleHeader:
        txn_id, flags, null_bitmap, data_length, version = struct.unpack("<QIIiI", data[:TUPLE_HEADER_SIZE])
        return cls(
            txn_id=txn_id,
            flags=flags,
            null_bitmap=null_bitmap,
            data_length=data_length,
            version=version,
        )


@dataclass
class Tuple:
    """A row/tuple in the row store."""

    header: TupleHeader
    data: dict[str, Any]
    slot_id: int = 0


@dataclass
class Page:
    """A fixed-size storage page containing tuples."""

    page_id: int
    tuples: list[Tuple] = field(default_factory=list)
    free_space: int = PAGE_SIZE
    is_dirty: bool = False

    def can_fit(self, data_size: int) -> bool:
        return self.free_space >= data_size + TUPLE_HEADER_SIZE + 4  # +4 for slot entry

    def insert_tuple(self, txn_id: int, data: dict[str, Any], data_size: int) -> int:
        """Insert a tuple and return slot_id."""
        header = TupleHeader(txn_id=txn_id, data_length=data_size)
        slot_id = len(self.tuples)
        tup = Tuple(header=header, data=data, slot_id=slot_id)
        self.tuples.append(tup)
        self.free_space -= data_size + TUPLE_HEADER_SIZE + 4
        self.is_dirty = True
        return slot_id

    def get_tuple(self, slot_id: int) -> Tuple | None:
        if 0 <= slot_id < len(self.tuples):
            tup = self.tuples[slot_id]
            if not tup.header.is_deleted:
                return tup
        return None

    def delete_tuple(self, slot_id: int, txn_id: int) -> bool:
        if 0 <= slot_id < len(self.tuples):
            self.tuples[slot_id].header.mark_deleted()
            self.tuples[slot_id].header.txn_id = txn_id
            self.is_dirty = True
            return True
        return False

    @property
    def live_tuple_count(self) -> int:
        return sum(1 for t in self.tuples if not t.header.is_deleted)


class RowStore:
    """Heap-based row store with page management."""

    def __init__(self, page_size: int = PAGE_SIZE) -> None:
        self._page_size = page_size
        self._pages: dict[int, Page] = {}
        self._next_page_id = 0
        # Mapping: (table, pk) -> (page_id, slot_id)
        self._index: dict[tuple[str, str], tuple[int, int]] = {}

    def insert(self, table: str, pk: str, txn_id: int, data: dict[str, Any]) -> tuple[int, int]:
        """Insert a row. Returns (page_id, slot_id)."""
        import json
        data_bytes = json.dumps(data).encode("utf-8")
        data_size = len(data_bytes)

        # Find page with space
        page = self._find_or_create_page(data_size)
        slot_id = page.insert_tuple(txn_id, data, data_size)

        self._index[(table, pk)] = (page.page_id, slot_id)
        return page.page_id, slot_id

    def get(self, table: str, pk: str) -> dict[str, Any] | None:
        """Get a row by table + pk."""
        loc = self._index.get((table, pk))
        if not loc:
            return None

        page_id, slot_id = loc
        page = self._pages.get(page_id)
        if not page:
            return None

        tup = page.get_tuple(slot_id)
        return tup.data if tup else None

    def delete(self, table: str, pk: str, txn_id: int) -> bool:
        """Mark a row as deleted."""
        loc = self._index.get((table, pk))
        if not loc:
            return False

        page_id, slot_id = loc
        page = self._pages.get(page_id)
        if not page:
            return False

        return page.delete_tuple(slot_id, txn_id)

    def _find_or_create_page(self, data_size: int) -> Page:
        """Find a page with enough space or create a new one."""
        for page in self._pages.values():
            if page.can_fit(data_size):
                return page

        page = Page(page_id=self._next_page_id, free_space=self._page_size)
        self._pages[self._next_page_id] = page
        self._next_page_id += 1
        return page

    @property
    def page_count(self) -> int:
        return len(self._pages)

    @property
    def total_tuples(self) -> int:
        return sum(p.live_tuple_count for p in self._pages.values())
