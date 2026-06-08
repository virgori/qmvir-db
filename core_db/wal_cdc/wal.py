"""QM Core DB — Write-Ahead Log and Change Data Capture.

WAL provides:
  - Durability: all changes written to log before heap
  - Recovery: replay log after crash
  - CDC: stream changes to downstream engines

CDC provides:
  - Event stream of all data changes
  - Outbox pattern support
  - Downstream sync to search/vector/analytics
"""

from __future__ import annotations

import json
import os
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable


class WALEntryType(Enum):
    """Types of WAL entries."""

    INSERT = "insert"
    UPDATE = "update"
    DELETE = "delete"
    COMMIT = "commit"
    ROLLBACK = "rollback"
    CHECKPOINT = "checkpoint"


@dataclass
class WALEntry:
    """A single WAL entry."""

    lsn: int  # Log Sequence Number
    entry_type: WALEntryType
    txn_id: int
    table: str | None = None
    pk: str | None = None
    data: dict[str, Any] | None = None
    old_data: dict[str, Any] | None = None
    timestamp: float = field(default_factory=time.time)

    def serialize(self) -> bytes:
        d = {
            "lsn": self.lsn,
            "type": self.entry_type.value,
            "txn_id": self.txn_id,
            "table": self.table,
            "pk": self.pk,
            "data": self.data,
            "old_data": self.old_data,
            "ts": self.timestamp,
        }
        return json.dumps(d).encode("utf-8") + b"\n"

    @classmethod
    def deserialize(cls, raw: bytes) -> WALEntry:
        d = json.loads(raw)
        return cls(
            lsn=d["lsn"],
            entry_type=WALEntryType(d["type"]),
            txn_id=d["txn_id"],
            table=d.get("table"),
            pk=d.get("pk"),
            data=d.get("data"),
            old_data=d.get("old_data"),
            timestamp=d.get("ts", 0.0),
        )


class WriteAheadLog:
    """Append-only write-ahead log for durability and recovery."""

    def __init__(self, wal_dir: str = "data/wal") -> None:
        self.wal_dir = wal_dir
        self._lsn = 0
        self._entries: list[WALEntry] = []
        self._file_handle = None
        os.makedirs(wal_dir, exist_ok=True)

    def append(
        self,
        entry_type: WALEntryType,
        txn_id: int,
        table: str | None = None,
        pk: str | None = None,
        data: dict[str, Any] | None = None,
        old_data: dict[str, Any] | None = None,
    ) -> WALEntry:
        """Append an entry to the WAL."""
        self._lsn += 1
        entry = WALEntry(
            lsn=self._lsn,
            entry_type=entry_type,
            txn_id=txn_id,
            table=table,
            pk=pk,
            data=data,
            old_data=old_data,
        )
        self._entries.append(entry)
        self._flush_entry(entry)
        return entry

    def get_entries_since(self, lsn: int) -> list[WALEntry]:
        """Get all entries after a given LSN."""
        return [e for e in self._entries if e.lsn > lsn]

    def checkpoint(self, txn_id: int = 0) -> WALEntry:
        """Write a checkpoint marker."""
        return self.append(WALEntryType.CHECKPOINT, txn_id)

    def replay(self, callback: Callable[[WALEntry], None]) -> int:
        """Replay all WAL entries through a callback. Returns count."""
        count = 0
        for entry in self._entries:
            callback(entry)
            count += 1
        return count

    @property
    def current_lsn(self) -> int:
        return self._lsn

    def _flush_entry(self, entry: WALEntry) -> None:
        """Persist entry to disk."""
        wal_file = os.path.join(self.wal_dir, "current.wal")
        with open(wal_file, "ab") as f:
            f.write(entry.serialize())


# ─── CDC (Change Data Capture) ─────────────────────────────────────────────

@dataclass
class CDCEvent:
    """A CDC event derived from WAL entries."""

    event_id: int
    operation: str  # insert, update, delete
    table: str
    pk: str
    new_data: dict[str, Any] | None = None
    old_data: dict[str, Any] | None = None
    txn_id: int = 0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        return {
            "event_id": self.event_id,
            "op": self.operation,
            "table": self.table,
            "pk": self.pk,
            "new": self.new_data,
            "old": self.old_data,
            "txn_id": self.txn_id,
            "ts": self.timestamp,
        }


CDCHandler = Callable[[CDCEvent], None]


class CDCStream:
    """Change Data Capture stream from WAL to downstream consumers."""

    def __init__(self, wal: WriteAheadLog) -> None:
        self._wal = wal
        self._last_lsn = 0
        self._handlers: list[CDCHandler] = []
        self._event_counter = 0

    def register_handler(self, handler: CDCHandler) -> None:
        """Register a downstream CDC consumer."""
        self._handlers.append(handler)

    def poll(self) -> list[CDCEvent]:
        """Poll for new CDC events from WAL."""
        new_entries = self._wal.get_entries_since(self._last_lsn)
        events: list[CDCEvent] = []

        for entry in new_entries:
            self._last_lsn = entry.lsn

            if entry.entry_type in (
                WALEntryType.INSERT,
                WALEntryType.UPDATE,
                WALEntryType.DELETE,
            ):
                self._event_counter += 1
                event = CDCEvent(
                    event_id=self._event_counter,
                    operation=entry.entry_type.value,
                    table=entry.table or "",
                    pk=entry.pk or "",
                    new_data=entry.data,
                    old_data=entry.old_data,
                    txn_id=entry.txn_id,
                    timestamp=entry.timestamp,
                )
                events.append(event)

                # Dispatch to handlers
                for handler in self._handlers:
                    handler(event)

        return events


# ─── Outbox Pattern ─────────────────────────────────────────────────────────

@dataclass
class OutboxEntry:
    """Outbox table entry for reliable event delivery."""

    entry_id: int
    aggregate_type: str  # table name
    aggregate_id: str  # pk
    event_type: str  # insert, update, delete
    payload: dict[str, Any]
    created_at: float = field(default_factory=time.time)
    processed: bool = False
    processed_at: float | None = None


class OutboxWriter:
    """Writes events to outbox within the same transaction as data changes."""

    def __init__(self) -> None:
        self._entries: list[OutboxEntry] = []
        self._next_id = 1

    def write(
        self,
        aggregate_type: str,
        aggregate_id: str,
        event_type: str,
        payload: dict[str, Any],
    ) -> OutboxEntry:
        """Write an outbox entry."""
        entry = OutboxEntry(
            entry_id=self._next_id,
            aggregate_type=aggregate_type,
            aggregate_id=aggregate_id,
            event_type=event_type,
            payload=payload,
        )
        self._next_id += 1
        self._entries.append(entry)
        return entry

    def get_unprocessed(self, limit: int = 100) -> list[OutboxEntry]:
        """Get unprocessed outbox entries for downstream sync."""
        return [e for e in self._entries if not e.processed][:limit]

    def mark_processed(self, entry_id: int) -> None:
        """Mark an outbox entry as processed."""
        for e in self._entries:
            if e.entry_id == entry_id:
                e.processed = True
                e.processed_at = time.time()
                break
