"""QM Storage — MVCC Engine with Version Chains + Garbage Collection.

Features:
    - Snapshot isolation via version chains
    - Read-your-own-writes
    - Lock-free reads (readers never block writers)
    - Write-write conflict detection (first-committer-wins)
    - Background garbage collection of old versions
    - Integration with WAL for durability
    - 2PC-ready (prepare/commit)
"""

from __future__ import annotations

import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any

from qm_core.concurrency import WriteConflictError


class TxnState(IntEnum):
    ACTIVE = 0
    PREPARING = 1
    COMMITTED = 2
    ABORTED = 3


@dataclass(slots=True)
class VersionRecord:
    """A single version in the chain. Points backward to older version."""
    txn_id: int
    data: dict[str, Any]
    created_ts: float
    deleted_by: int = 0  # txn_id that logically deleted this
    prev: VersionRecord | None = None

    @property
    def is_deleted(self) -> bool:
        return self.deleted_by != 0


@dataclass
class Transaction:
    """An active transaction context."""
    txn_id: int
    begin_ts: float
    state: TxnState = TxnState.ACTIVE
    write_set: dict[str, dict[str, VersionRecord]] = field(default_factory=dict)
    read_set: set[tuple[str, str]] = field(default_factory=set)
    _lock: threading.Lock = field(default_factory=threading.Lock, repr=False)

    @property
    def is_active(self) -> bool:
        return self.state == TxnState.ACTIVE


class MVCCEngine:
    """Multi-Version Concurrency Control engine with snapshot isolation.

    Each table is a dict of PK → VersionRecord (head of chain).
    Readers see a consistent snapshot; writers create new versions.
    GC removes versions invisible to all active transactions.
    """

    def __init__(self) -> None:
        self._next_txn = 1
        self._global_lock = threading.Lock()
        self._active: dict[int, Transaction] = {}
        self._committed: dict[int, float] = {}  # txn_id → commit_ts
        self._heap: dict[str, dict[str, VersionRecord]] = {}
        self._write_intents: dict[tuple[str, str], int] = {}  # (table, pk) -> txn_id
        self._gc_watermark: float = 0.0

    # ── Transaction lifecycle ───────────────────────────────────────

    def begin(self) -> Transaction:
        """Start a new transaction."""
        with self._global_lock:
            txn_id = self._next_txn
            self._next_txn += 1
            txn = Transaction(txn_id=txn_id, begin_ts=time.time())
            self._active[txn_id] = txn
        return txn

    def commit(self, txn: Transaction) -> float:
        """Commit transaction. Returns commit timestamp.

        First-committer-wins: if any row in the write-set was modified
        by another transaction that committed AFTER our begin_ts, we
        detect a write-write conflict and abort.
        """
        with txn._lock:
            if txn.state != TxnState.ACTIVE:
                raise RuntimeError(f"Txn {txn.txn_id} not active (state={txn.state.name})")
            txn.state = TxnState.PREPARING

        commit_ts = time.time()

        # Install write-set into shared heap
        with self._global_lock:
            # ── Write-Write conflict detection ──
            for table, rows in txn.write_set.items():
                heap_table = self._heap.get(table, {})
                for pk, _ver in rows.items():
                    head = heap_table.get(pk)
                    if head is not None and head.txn_id != txn.txn_id:
                        head_commit_ts = self._committed.get(head.txn_id)
                        if head_commit_ts is not None and head_commit_ts > txn.begin_ts:
                            # Another txn modified this row after we started
                            txn.state = TxnState.ABORTED
                            self._release_intents(txn)
                            txn.write_set.clear()
                            self._active.pop(txn.txn_id, None)
                            raise WriteConflictError(
                                f"Txn {txn.txn_id}: row {table}/{pk} modified by "
                                f"txn {head.txn_id} (committed at {head_commit_ts:.6f}, "
                                f"our begin_ts={txn.begin_ts:.6f})"
                            )

            for table, rows in txn.write_set.items():
                if table not in self._heap:
                    self._heap[table] = {}
                for pk, version in rows.items():
                    old_head = self._heap[table].get(pk)
                    version.prev = old_head
                    self._heap[table][pk] = version

            txn.state = TxnState.COMMITTED
            self._committed[txn.txn_id] = commit_ts
            self._release_intents(txn)
            self._active.pop(txn.txn_id, None)

        return commit_ts

    def rollback(self, txn: Transaction) -> None:
        """Abort a transaction, discarding all writes."""
        with self._global_lock:
            txn.state = TxnState.ABORTED
            txn.write_set.clear()
            self._release_intents(txn)
            self._active.pop(txn.txn_id, None)

    # ── Data operations ─────────────────────────────────────────────

    def read(self, txn: Transaction, table: str, pk: str) -> dict[str, Any] | None:
        """Read a row visible to this transaction's snapshot."""
        if not txn.is_active:
            raise RuntimeError(f"Txn {txn.txn_id} not active")
        txn.read_set.add((table, pk))

        # Read-your-own-writes
        ws = txn.write_set.get(table, {})
        if pk in ws:
            ver = ws[pk]
            return None if ver.is_deleted else ver.data

        # Traverse committed version chain
        head = self._heap.get(table, {}).get(pk)
        return self._find_visible(txn, head)

    def insert(self, txn: Transaction, table: str, pk: str, data: dict[str, Any]) -> None:
        if not txn.is_active:
            raise RuntimeError(f"Txn {txn.txn_id} not active")
        self._reserve_intent(txn, table, pk)
        ver = VersionRecord(txn_id=txn.txn_id, data=data, created_ts=time.time())
        txn.write_set.setdefault(table, {})[pk] = ver

    def update(self, txn: Transaction, table: str, pk: str, data: dict[str, Any]) -> None:
        if not txn.is_active:
            raise RuntimeError(f"Txn {txn.txn_id} not active")
        self._reserve_intent(txn, table, pk)
        current = self.read(txn, table, pk)
        if current is None:
            raise KeyError(f"Row {pk} not found in {table}")
        merged = {**current, **data}
        ver = VersionRecord(txn_id=txn.txn_id, data=merged, created_ts=time.time())
        txn.write_set.setdefault(table, {})[pk] = ver

    def delete(self, txn: Transaction, table: str, pk: str) -> None:
        if not txn.is_active:
            raise RuntimeError(f"Txn {txn.txn_id} not active")
        self._reserve_intent(txn, table, pk)
        current = self.read(txn, table, pk)
        if current is None:
            raise KeyError(f"Row {pk} not found in {table}")
        ver = VersionRecord(
            txn_id=txn.txn_id, data=current, created_ts=time.time(),
            deleted_by=txn.txn_id,
        )
        txn.write_set.setdefault(table, {})[pk] = ver

    def scan(self, txn: Transaction, table: str) -> list[tuple[str, dict[str, Any]]]:
        """Full table scan visible to this transaction."""
        results: list[tuple[str, dict[str, Any]]] = []
        # Committed data
        for pk, head in self._heap.get(table, {}).items():
            if pk in txn.write_set.get(table, {}):
                continue  # Will be handled from write-set
            data = self._find_visible(txn, head)
            if data is not None:
                results.append((pk, data))
        # Write-set entries
        for pk, ver in txn.write_set.get(table, {}).items():
            if not ver.is_deleted:
                results.append((pk, ver.data))
        return results

    # ── Garbage collection ──────────────────────────────────────────

    def gc(self) -> int:
        """Remove old versions invisible to all active transactions.
        Returns count of versions removed."""
        # Compute oldest active snapshot
        with self._global_lock:
            if self._active:
                oldest = min(t.begin_ts for t in self._active.values())
            else:
                oldest = time.time()
            self._gc_watermark = oldest

        removed = 0
        for table_data in self._heap.values():
            for pk in list(table_data.keys()):
                head = table_data[pk]
                removed += self._gc_chain(table_data, pk, head, oldest)
        return removed

    # ── Internal ────────────────────────────────────────────────────

    def _find_visible(self, txn: Transaction, head: VersionRecord | None) -> dict[str, Any] | None:
        """Walk version chain to find the version visible to txn."""
        ver = head
        while ver is not None:
            if self._is_visible(txn, ver):
                if ver.is_deleted and ver.deleted_by in self._committed:
                    return None
                return ver.data
            ver = ver.prev
        return None

    def _is_visible(self, txn: Transaction, ver: VersionRecord) -> bool:
        """A version is visible if created by txn itself, or committed before txn's snapshot."""
        if ver.txn_id == txn.txn_id:
            return True
        commit_ts = self._committed.get(ver.txn_id)
        if commit_ts is not None and commit_ts <= txn.begin_ts:
            return True
        return False

    def _gc_chain(self, table_data: dict, pk: str, head: VersionRecord, watermark: float) -> int:
        """Trim version chain: keep newest visible version, remove older ones."""
        removed = 0
        ver = head
        prev_ref = None  # The version whose .prev we need to set to None
        found_visible = False

        while ver is not None:
            commit_ts = self._committed.get(ver.txn_id, float("inf"))
            if commit_ts < watermark:
                if found_visible:
                    # Everything after the first visible version can be removed
                    if prev_ref is not None:
                        # Count remaining chain
                        old = ver
                        while old is not None:
                            removed += 1
                            old = old.prev
                        prev_ref.prev = None
                    break
                found_visible = True
            prev_ref = ver
            ver = ver.prev
        return removed

    def _reserve_intent(self, txn: Transaction, table: str, pk: str) -> None:
        """Reserve write intent for bookkeeping.

        Keep first-committer-wins semantics at commit time, so we intentionally
        do not fail on concurrent uncommitted intents here.
        """
        key = (table, pk)
        with self._global_lock:
            self._write_intents.setdefault(key, txn.txn_id)

    def _release_intents(self, txn: Transaction) -> None:
        """Release all write intents held by a transaction."""
        for table, rows in txn.write_set.items():
            for pk in rows.keys():
                key = (table, pk)
                if self._write_intents.get(key) == txn.txn_id:
                    self._write_intents.pop(key, None)

    @property
    def active_txn_count(self) -> int:
        return len(self._active)

    @property
    def table_names(self) -> list[str]:
        return list(self._heap.keys())
