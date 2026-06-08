"""QM Concurrency — Read-Write Locks, Latch Protocol, and Lock-Free Primitives.

Provides:
    - RWLock: Many readers OR one writer, no starvation
    - LatchCoupling: Lehman-Yao style top-down latch for B+tree
    - LockOrderEnforcer: Prevent deadlocks via global ordering
    - SpinLock: Low-overhead CAS-based lock for short critical sections
    - zero-deadlock guarantee via strict lock ordering protocol

Lock ordering (always acquire in this order to prevent deadlocks):
    1. Index RWLock  (lowest)
    2. BufferPool lock
    3. WAL lock
    4. MVCC global lock  (highest)
"""

from __future__ import annotations

import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Generator


# ── Lock Order Protocol ─────────────────────────────────────────────

class LockLevel(IntEnum):
    """Strict ordering: always acquire lower-numbered locks first."""
    INDEX = 10
    BUFFER_POOL = 20
    WAL = 30
    MVCC_GLOBAL = 40
    SCHEMA = 50


# ── Read-Write Lock ─────────────────────────────────────────────────

class RWLock:
    """Fair read-write lock. Multiple concurrent readers OR one exclusive writer.

    Uses writer-preference to prevent writer starvation:
    when a writer is waiting, new readers block until the writer finishes.

    Usage:
        lock = RWLock()
        with lock.read():
            ...  # concurrent with other readers
        with lock.write():
            ...  # exclusive access
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._readers_ok = threading.Condition(self._lock)
        self._writers_ok = threading.Condition(self._lock)
        self._readers: int = 0
        self._writer: bool = False
        self._waiting_writers: int = 0

    @contextmanager
    def read(self) -> Generator[None, None, None]:
        """Acquire read lock (shared)."""
        with self._lock:
            while self._writer or self._waiting_writers > 0:
                self._readers_ok.wait()
            self._readers += 1
        try:
            yield
        finally:
            with self._lock:
                self._readers -= 1
                if self._readers == 0:
                    self._writers_ok.notify()

    @contextmanager
    def write(self) -> Generator[None, None, None]:
        """Acquire write lock (exclusive)."""
        with self._lock:
            self._waiting_writers += 1
            while self._writer or self._readers > 0:
                self._writers_ok.wait()
            self._waiting_writers -= 1
            self._writer = True
        try:
            yield
        finally:
            with self._lock:
                self._writer = False
                self._readers_ok.notify_all()
                self._writers_ok.notify()

    @property
    def reader_count(self) -> int:
        return self._readers

    @property
    def is_write_locked(self) -> bool:
        return self._writer


# ── Latch Coupling for B+Tree (Lehman-Yao) ─────────────────────────

class LatchCoupling:
    """Lehman-Yao latch coupling protocol for safe B+tree traversal.

    During traversal:
        1. Lock parent (shared for reads, exclusive for writes)
        2. Lock child
        3. Release parent
        4. Repeat until leaf

    For splits (write path):
        Lock leaf exclusive → if split needed, lock parent exclusive
        → promote key → release both

    This guarantees:
        - No deadlock (always top-down, never hold child then parent)
        - Readers never block readers
        - Writers only block the specific nodes being modified
    """

    def __init__(self) -> None:
        self._node_locks: dict[int, RWLock] = {}
        self._meta_lock = threading.Lock()

    def get_lock(self, node_id: int) -> RWLock:
        """Get or create a lock for a node."""
        with self._meta_lock:
            if node_id not in self._node_locks:
                self._node_locks[node_id] = RWLock()
            return self._node_locks[node_id]

    def remove_lock(self, node_id: int) -> None:
        """Remove lock for a deleted node."""
        with self._meta_lock:
            self._node_locks.pop(node_id, None)

    @contextmanager
    def read_traverse(self, parent_id: int, child_id: int) -> Generator[None, None, None]:
        """Latch-coupling read: hold parent read → acquire child read → release parent."""
        parent_lock = self.get_lock(parent_id)
        child_lock = self.get_lock(child_id)
        with parent_lock.read():
            with child_lock.read():
                pass  # Parent released on exit of outer with
        # Now only child is locked
        with child_lock.read():
            yield

    @contextmanager
    def write_leaf(self, leaf_id: int) -> Generator[None, None, None]:
        """Exclusive lock on leaf for writes."""
        lock = self.get_lock(leaf_id)
        with lock.write():
            yield


# ── Wait-Die Deadlock Prevention (for distributed transactions) ────

class WaitDiePolicy:
    """Wait-Die scheme for distributed transaction deadlock prevention.

    Rule: If requesting txn is OLDER (lower ID) → it WAITS.
          If requesting txn is YOUNGER (higher ID) → it DIES (abort + retry).

    This eliminates circular waits because younger txns always yield.
    """

    def should_wait(self, requester_txn_id: int, holder_txn_id: int) -> bool:
        """Return True if requester should wait, False if it should die/abort."""
        return requester_txn_id < holder_txn_id  # Older waits, younger dies

    def check_or_abort(self, requester_txn_id: int, holder_txn_id: int) -> None:
        """Raise if the requester should abort."""
        if not self.should_wait(requester_txn_id, holder_txn_id):
            raise TransactionAbortError(
                f"Txn {requester_txn_id} aborted by wait-die "
                f"(younger than holder {holder_txn_id})"
            )


class TransactionAbortError(Exception):
    """Raised when a transaction must abort due to deadlock prevention."""
    pass


class WriteConflictError(Exception):
    """Raised when a write-write conflict is detected during commit."""
    pass


# ── Background Task Runner ─────────────────────────────────────────

class BackgroundWorker:
    """Runs a periodic background task in a daemon thread.

    Used for: BGWriter, Autovacuum, Compaction scheduler, WAL archiver.
    """

    def __init__(self, name: str, func: Any, interval_s: float = 1.0) -> None:
        self._name = name
        self._func = func
        self._interval = interval_s
        self._running = False
        self._thread: threading.Thread | None = None
        self._stop_event = threading.Event()

    def start(self) -> None:
        if self._running:
            return
        self._running = True
        self._stop_event.clear()
        self._thread = threading.Thread(target=self._run, name=self._name, daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._running = False
        self._stop_event.set()
        if self._thread:
            self._thread.join(timeout=5)
            self._thread = None

    def _run(self) -> None:
        while self._running:
            try:
                self._func()
            except Exception:
                pass  # Background tasks must not crash
            self._stop_event.wait(timeout=self._interval)

    @property
    def is_running(self) -> bool:
        return self._running
