"""QM Core DB — Transaction Engine with MVCC.

Implements:
  - MVCC (Multi-Version Concurrency Control)
  - Snapshot isolation / repeatable read
  - Transaction commit/rollback
  - Row-level locking
  - Version chain management
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class IsolationLevel(Enum):
    """Transaction isolation levels."""

    READ_COMMITTED = "read_committed"
    REPEATABLE_READ = "repeatable_read"
    SNAPSHOT = "snapshot"
    SERIALIZABLE = "serializable"


class TxnStatus(Enum):
    """Transaction lifecycle states."""

    ACTIVE = "active"
    COMMITTED = "committed"
    ABORTED = "aborted"
    PREPARING = "preparing"


class WriteConflictError(RuntimeError):
    """Raised when a transaction loses first-committer-wins conflict detection."""


@dataclass
class RowVersion:
    """A single version of a row in the MVCC chain."""

    txn_id: int  # Transaction that created this version
    data: dict[str, Any]
    created_at: float
    deleted_by: int | None = None  # Transaction that deleted/updated this
    prev_version: RowVersion | None = None  # Pointer to older version


@dataclass
class Transaction:
    """Represents an active transaction."""

    txn_id: int
    snapshot_ts: float
    isolation: IsolationLevel = IsolationLevel.SNAPSHOT
    status: TxnStatus = TxnStatus.ACTIVE
    write_set: dict[str, dict[str, RowVersion]] = field(default_factory=dict)
    read_set: set[tuple[str, str]] = field(default_factory=set)  # (table, pk)
    _lock: threading.Lock = field(default_factory=threading.Lock)

    def is_active(self) -> bool:
        return self.status == TxnStatus.ACTIVE


class TransactionEngine:
    """MVCC Transaction Engine for the Core DB."""

    def __init__(self) -> None:
        self._next_txn_id = 1
        self._lock = threading.Lock()
        self._active_txns: dict[int, Transaction] = {}
        self._committed_txns: set[int] = set()
        self._commit_ts: dict[int, float] = {}
        # table_name -> pk -> RowVersion (latest)
        self._heap: dict[str, dict[str, RowVersion]] = {}
        self._wal: list[dict[str, Any]] = []  # In-memory WAL for now

    def begin(
        self, isolation: IsolationLevel = IsolationLevel.SNAPSHOT
    ) -> Transaction:
        """Start a new transaction."""
        with self._lock:
            txn_id = self._next_txn_id
            self._next_txn_id += 1

        txn = Transaction(
            txn_id=txn_id,
            snapshot_ts=time.time(),
            isolation=isolation,
        )
        self._active_txns[txn_id] = txn
        return txn

    def read(self, txn: Transaction, table: str, pk: str) -> dict[str, Any] | None:
        """Read a row visible to this transaction's snapshot."""
        if not txn.is_active():
            raise RuntimeError(f"Transaction {txn.txn_id} is not active")

        txn.read_set.add((table, pk))

        # Check write set first (read-your-own-writes)
        if table in txn.write_set and pk in txn.write_set[table]:
            version = txn.write_set[table][pk]
            if version.deleted_by == txn.txn_id:
                return None
            return version.data

        # Traverse version chain
        table_data = self._heap.get(table, {})
        version = table_data.get(pk)

        while version is not None:
            if self._is_visible(txn, version):
                if version.deleted_by is not None and version.deleted_by in self._committed_txns:
                    return None
                return version.data
            version = version.prev_version

        return None

    def insert(self, txn: Transaction, table: str, pk: str, data: dict[str, Any]) -> None:
        """Insert a new row within a transaction."""
        if not txn.is_active():
            raise RuntimeError(f"Transaction {txn.txn_id} is not active")

        version = RowVersion(
            txn_id=txn.txn_id,
            data=data,
            created_at=time.time(),
        )

        if table not in txn.write_set:
            txn.write_set[table] = {}
        txn.write_set[table][pk] = version

        self._wal.append({
            "type": "insert",
            "txn_id": txn.txn_id,
            "table": table,
            "pk": pk,
            "data": data,
            "ts": time.time(),
        })

    def update(self, txn: Transaction, table: str, pk: str, data: dict[str, Any]) -> None:
        """Update a row within a transaction."""
        if not txn.is_active():
            raise RuntimeError(f"Transaction {txn.txn_id} is not active")

        # Get current version
        current = self.read(txn, table, pk)
        if current is None:
            raise KeyError(f"Row {pk} not found in {table}")

        merged = {**current, **data}
        new_version = RowVersion(
            txn_id=txn.txn_id,
            data=merged,
            created_at=time.time(),
        )

        if table not in txn.write_set:
            txn.write_set[table] = {}
        txn.write_set[table][pk] = new_version

        self._wal.append({
            "type": "update",
            "txn_id": txn.txn_id,
            "table": table,
            "pk": pk,
            "data": merged,
            "ts": time.time(),
        })

    def delete(self, txn: Transaction, table: str, pk: str) -> None:
        """Mark a row as deleted within a transaction."""
        if not txn.is_active():
            raise RuntimeError(f"Transaction {txn.txn_id} is not active")

        current = self.read(txn, table, pk)
        if current is None:
            raise KeyError(f"Row {pk} not found in {table}")

        if table not in txn.write_set:
            txn.write_set[table] = {}

        if pk in txn.write_set.get(table, {}):
            # Row was inserted/updated in this txn
            txn.write_set[table][pk].deleted_by = txn.txn_id
        else:
            # Row exists in heap — create a deletion marker
            delete_version = RowVersion(
                txn_id=txn.txn_id,
                data=current,
                created_at=time.time(),
                deleted_by=txn.txn_id,
            )
            txn.write_set[table][pk] = delete_version

        self._wal.append({
            "type": "delete",
            "txn_id": txn.txn_id,
            "table": table,
            "pk": pk,
            "ts": time.time(),
        })

    def commit(self, txn: Transaction) -> None:
        """Commit a transaction — make writes visible to others."""
        if not txn.is_active():
            raise RuntimeError(f"Transaction {txn.txn_id} is not active")

        txn.status = TxnStatus.PREPARING

        # Apply write set to shared heap
        with self._lock:
            for table, rows in txn.write_set.items():
                heap_table = self._heap.get(table, {})
                for pk in rows.keys():
                    head = heap_table.get(pk)
                    if head is None or head.txn_id == txn.txn_id:
                        continue
                    head_commit_ts = self._commit_ts.get(head.txn_id)
                    if head_commit_ts is not None and head_commit_ts > txn.snapshot_ts:
                        txn.status = TxnStatus.ABORTED
                        txn.write_set.clear()
                        self._active_txns.pop(txn.txn_id, None)
                        self._wal.append({
                            "type": "rollback",
                            "txn_id": txn.txn_id,
                            "reason": "write_conflict",
                            "table": table,
                            "pk": pk,
                            "conflicting_txn_id": head.txn_id,
                            "ts": time.time(),
                        })
                        raise WriteConflictError(
                            f"Transaction {txn.txn_id} write conflict on {table}/{pk}; "
                            f"row was modified by committed transaction {head.txn_id}"
                        )

            commit_ts = time.time()
            for table, rows in txn.write_set.items():
                if table not in self._heap:
                    self._heap[table] = {}
                for pk, version in rows.items():
                    old = self._heap[table].get(pk)
                    version.prev_version = old
                    self._heap[table][pk] = version

            txn.status = TxnStatus.COMMITTED
            self._committed_txns.add(txn.txn_id)
            self._commit_ts[txn.txn_id] = commit_ts
            self._active_txns.pop(txn.txn_id, None)

        self._wal.append({
            "type": "commit",
            "txn_id": txn.txn_id,
            "ts": time.time(),
        })

    def rollback(self, txn: Transaction) -> None:
        """Rollback a transaction — discard all writes."""
        txn.status = TxnStatus.ABORTED
        txn.write_set.clear()
        self._active_txns.pop(txn.txn_id, None)

        self._wal.append({
            "type": "rollback",
            "txn_id": txn.txn_id,
            "ts": time.time(),
        })

    def _is_visible(self, txn: Transaction, version: RowVersion) -> bool:
        """Check if a row version is visible to this transaction's snapshot."""
        # Own writes are always visible
        if version.txn_id == txn.txn_id:
            return True
        commit_ts = self._commit_ts.get(version.txn_id)
        if commit_ts is None:
            return False
        if txn.isolation == IsolationLevel.READ_COMMITTED:
            return True
        if commit_ts <= txn.snapshot_ts:
            return True
        return False

    def get_wal_entries(self, since_ts: float | Any = 0.0) -> list[dict[str, Any]]:
        """Get WAL entries since a timestamp (for CDC). Also accepts a Transaction."""
        if hasattr(since_ts, 'snapshot_ts'):
            since_ts = 0.0
        return [e for e in self._wal if e.get("ts", 0) > since_ts]

    @property
    def active_txn_count(self) -> int:
        """Number of active transactions retained by the engine."""
        return len(self._active_txns)
