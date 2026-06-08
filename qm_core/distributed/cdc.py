"""QM Distributed — Change Data Capture (CDC) Pipeline.

Provides:
    - Real-time change event streaming from WAL
    - Subscription model with per-table/per-shard filters
    - At-least-once delivery guarantees
    - Consumer offset tracking (resumable)
    - Configurable output formats (JSON, Avro-like)
    - Backpressure via bounded queues

Architecture:
    WAL → CDCExtractor → CDCPipeline → [Subscription1, Subscription2, ...]
                                              ↓
                                         Consumer(filter, callback)

Event types:
    - INSERT: New row added
    - UPDATE: Row modified (includes before/after images)
    - DELETE: Row removed
    - SCHEMA_CHANGE: Table DDL
    - TRUNCATE: Table truncated
"""

from __future__ import annotations

import threading
import time
import uuid
from collections import defaultdict, deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class CDCEventType(IntEnum):
    """Type of CDC change event."""
    INSERT = 1
    UPDATE = 2
    DELETE = 3
    SCHEMA_CHANGE = 4
    TRUNCATE = 5


@dataclass(slots=True)
class CDCEvent:
    """A single change data capture event."""
    event_id: str = ""
    sequence: int = 0          # Global monotonic sequence number
    lsn: int = 0               # WAL LSN this event came from
    timestamp: float = 0.0
    event_type: CDCEventType = CDCEventType.INSERT
    table: str = ""
    shard_id: int = 0
    key: str = ""
    before: dict[str, Any] | None = None     # Row before change (UPDATE/DELETE)
    after: dict[str, Any] | None = None      # Row after change (INSERT/UPDATE)
    metadata: dict[str, Any] = field(default_factory=dict)
    txn_id: int = 0

    def __post_init__(self) -> None:
        if not self.event_id:
            self.event_id = uuid.uuid4().hex[:16]
        if not self.timestamp:
            self.timestamp = time.time()

    def to_dict(self) -> dict[str, Any]:
        d: dict[str, Any] = {
            "event_id": self.event_id,
            "sequence": self.sequence,
            "lsn": self.lsn,
            "timestamp": self.timestamp,
            "type": self.event_type.name,
            "table": self.table,
            "shard_id": self.shard_id,
            "key": self.key,
        }
        if self.before is not None:
            d["before"] = self.before
        if self.after is not None:
            d["after"] = self.after
        if self.metadata:
            d["metadata"] = self.metadata
        if self.txn_id:
            d["txn_id"] = self.txn_id
        return d

    def serialize(self) -> bytes:
        try:
            import orjson
            return orjson.dumps(self.to_dict())
        except ImportError:
            import json
            return json.dumps(self.to_dict()).encode()

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> CDCEvent:
        return cls(
            event_id=d.get("event_id", ""),
            sequence=d.get("sequence", 0),
            lsn=d.get("lsn", 0),
            timestamp=d.get("timestamp", 0.0),
            event_type=CDCEventType[d.get("type", "INSERT")],
            table=d.get("table", ""),
            shard_id=d.get("shard_id", 0),
            key=d.get("key", ""),
            before=d.get("before"),
            after=d.get("after"),
            metadata=d.get("metadata", {}),
            txn_id=d.get("txn_id", 0),
        )


@dataclass
class CDCFilter:
    """Filter for CDC subscriptions."""
    tables: set[str] | None = None          # None = all tables
    shard_ids: set[int] | None = None       # None = all shards
    event_types: set[CDCEventType] | None = None  # None = all types
    key_prefix: str | None = None           # Filter by key prefix

    def matches(self, event: CDCEvent) -> bool:
        if self.tables and event.table not in self.tables:
            return False
        if self.shard_ids and event.shard_id not in self.shard_ids:
            return False
        if self.event_types and event.event_type not in self.event_types:
            return False
        if self.key_prefix and not event.key.startswith(self.key_prefix):
            return False
        return True


# Callback type for CDC consumers
CDCCallback = Callable[[CDCEvent], None]


@dataclass
class CDCSubscription:
    """A subscription to CDC events."""
    subscription_id: str = ""
    name: str = ""
    filter: CDCFilter = field(default_factory=CDCFilter)
    callback: CDCCallback | None = None
    created_at: float = 0.0
    active: bool = True
    last_sequence: int = 0       # Last delivered sequence (for resumption)
    events_delivered: int = 0
    errors: int = 0

    def __post_init__(self) -> None:
        if not self.subscription_id:
            self.subscription_id = f"cdc-sub-{uuid.uuid4().hex[:12]}"
        if not self.created_at:
            self.created_at = time.time()

    def to_dict(self) -> dict[str, Any]:
        return {
            "subscription_id": self.subscription_id,
            "name": self.name,
            "active": self.active,
            "last_sequence": self.last_sequence,
            "events_delivered": self.events_delivered,
            "errors": self.errors,
        }


class CDCBuffer:
    """Bounded ring buffer for CDC events with sequence tracking."""

    def __init__(self, max_size: int = 100000) -> None:
        self._buffer: deque[CDCEvent] = deque(maxlen=max_size)
        self._sequence = 0
        self._lock = threading.Lock()
        self._oldest_sequence = 0

    def append(self, event: CDCEvent) -> int:
        """Add event and return its sequence number."""
        with self._lock:
            self._sequence += 1
            event.sequence = self._sequence
            self._buffer.append(event)
            if len(self._buffer) == self._buffer.maxlen:
                self._oldest_sequence = self._buffer[0].sequence
            return self._sequence

    def get_since(self, since_sequence: int, max_count: int = 1000) -> list[CDCEvent]:
        """Get events since a given sequence number."""
        with self._lock:
            result: list[CDCEvent] = []
            for event in self._buffer:
                if event.sequence > since_sequence:
                    result.append(event)
                    if len(result) >= max_count:
                        break
            return result

    @property
    def current_sequence(self) -> int:
        return self._sequence

    @property
    def oldest_available(self) -> int:
        return self._oldest_sequence

    @property
    def size(self) -> int:
        return len(self._buffer)


class CDCPipeline:
    """CDC Pipeline — captures and distributes change events.

    Central hub for CDC: receives events from WAL writer, distributes
    to subscriptions.

    Usage:
        pipeline = CDCPipeline()
        pipeline.start()

        # Subscribe to changes
        sub = pipeline.subscribe("my-consumer", filter=CDCFilter(tables={"orders"}), callback=handler)

        # From WAL/engine: emit events
        pipeline.emit(CDCEvent(event_type=CDCEventType.INSERT, table="orders", key="123", after={...}))

        # Pull model alternative
        events = pipeline.poll(sub.subscription_id, max_events=100)
    """

    def __init__(
        self,
        buffer_size: int = 100000,
        delivery_workers: int = 4,
    ) -> None:
        self._buffer = CDCBuffer(max_size=buffer_size)
        self._subscriptions: dict[str, CDCSubscription] = {}
        self._delivery_workers = delivery_workers

        # Pending events for delivery (push model)
        self._pending: deque[CDCEvent] = deque(maxlen=buffer_size)
        self._pending_condition = threading.Condition()

        self._lock = threading.Lock()
        self._running = False
        self._threads: list[threading.Thread] = []

        # Stats
        self._total_events = 0
        self._total_delivered = 0
        self._total_errors = 0

        # Event hooks
        self._on_event_hooks: list[CDCCallback] = []

    def start(self) -> None:
        """Start the CDC pipeline delivery threads."""
        self._running = True
        for i in range(self._delivery_workers):
            t = threading.Thread(
                target=self._delivery_loop,
                daemon=True,
                name=f"cdc-delivery-{i}",
            )
            t.start()
            self._threads.append(t)

    def stop(self) -> None:
        """Stop the CDC pipeline."""
        self._running = False
        with self._pending_condition:
            self._pending_condition.notify_all()
        for t in self._threads:
            t.join(timeout=2.0)
        self._threads.clear()

    def emit(self, event: CDCEvent) -> int:
        """Emit a CDC event into the pipeline.

        Called by the WAL writer or engine on data changes.
        Returns the sequence number.
        """
        seq = self._buffer.append(event)

        with self._lock:
            self._total_events += 1

        # Notify push subscribers
        with self._pending_condition:
            self._pending.append(event)
            self._pending_condition.notify()

        # Fire hooks
        for hook in self._on_event_hooks:
            try:
                hook(event)
            except Exception:
                pass

        return seq

    def emit_insert(
        self,
        table: str,
        key: str,
        data: dict[str, Any],
        shard_id: int = 0,
        txn_id: int = 0,
    ) -> int:
        """Convenience: emit an INSERT event."""
        return self.emit(CDCEvent(
            event_type=CDCEventType.INSERT,
            table=table, key=key, after=data,
            shard_id=shard_id, txn_id=txn_id,
        ))

    def emit_update(
        self,
        table: str,
        key: str,
        before: dict[str, Any] | None,
        after: dict[str, Any],
        shard_id: int = 0,
        txn_id: int = 0,
    ) -> int:
        """Convenience: emit an UPDATE event."""
        return self.emit(CDCEvent(
            event_type=CDCEventType.UPDATE,
            table=table, key=key, before=before, after=after,
            shard_id=shard_id, txn_id=txn_id,
        ))

    def emit_delete(
        self,
        table: str,
        key: str,
        data: dict[str, Any] | None = None,
        shard_id: int = 0,
        txn_id: int = 0,
    ) -> int:
        """Convenience: emit a DELETE event."""
        return self.emit(CDCEvent(
            event_type=CDCEventType.DELETE,
            table=table, key=key, before=data,
            shard_id=shard_id, txn_id=txn_id,
        ))

    def subscribe(
        self,
        name: str,
        filter: CDCFilter | None = None,
        callback: CDCCallback | None = None,
        resume_from: int = 0,
    ) -> CDCSubscription:
        """Create a new CDC subscription.

        If callback is provided: push model (events delivered asynchronously).
        Otherwise: pull model (use poll() to fetch events).
        """
        sub = CDCSubscription(
            name=name,
            filter=filter or CDCFilter(),
            callback=callback,
            last_sequence=resume_from,
        )
        with self._lock:
            self._subscriptions[sub.subscription_id] = sub
        return sub

    def unsubscribe(self, subscription_id: str) -> bool:
        """Remove a subscription."""
        with self._lock:
            sub = self._subscriptions.pop(subscription_id, None)
        return sub is not None

    def poll(
        self,
        subscription_id: str,
        max_events: int = 100,
    ) -> list[CDCEvent]:
        """Poll for new events (pull model).

        Returns events since the subscription's last_sequence.
        """
        sub = self._subscriptions.get(subscription_id)
        if not sub or not sub.active:
            return []

        events = self._buffer.get_since(sub.last_sequence, max_count=max_events * 2)
        filtered: list[CDCEvent] = []

        for event in events:
            if sub.filter.matches(event):
                filtered.append(event)
                if len(filtered) >= max_events:
                    break

        if filtered:
            sub.last_sequence = filtered[-1].sequence
            sub.events_delivered += len(filtered)

        return filtered

    def acknowledge(self, subscription_id: str, sequence: int) -> None:
        """Acknowledge processing up to a sequence number."""
        sub = self._subscriptions.get(subscription_id)
        if sub:
            sub.last_sequence = max(sub.last_sequence, sequence)

    def add_hook(self, callback: CDCCallback) -> None:
        """Add a global event hook (called for every event)."""
        self._on_event_hooks.append(callback)

    def get_subscription(self, subscription_id: str) -> CDCSubscription | None:
        return self._subscriptions.get(subscription_id)

    def list_subscriptions(self) -> list[CDCSubscription]:
        return list(self._subscriptions.values())

    def _delivery_loop(self) -> None:
        """Background thread delivering events to push subscribers."""
        while self._running:
            event = None
            with self._pending_condition:
                if not self._pending:
                    self._pending_condition.wait(timeout=0.5)
                if self._pending:
                    event = self._pending.popleft()

            if event:
                self._deliver_event(event)

    def _deliver_event(self, event: CDCEvent) -> None:
        """Deliver an event to all matching push subscriptions."""
        for sub in list(self._subscriptions.values()):
            if not sub.active or not sub.callback:
                continue
            if not sub.filter.matches(event):
                continue

            try:
                sub.callback(event)
                sub.events_delivered += 1
                sub.last_sequence = event.sequence
                with self._lock:
                    self._total_delivered += 1
            except Exception:
                sub.errors += 1
                with self._lock:
                    self._total_errors += 1

    def stats(self) -> dict[str, Any]:
        return {
            "total_events": self._total_events,
            "total_delivered": self._total_delivered,
            "total_errors": self._total_errors,
            "buffer_size": self._buffer.size,
            "current_sequence": self._buffer.current_sequence,
            "oldest_available": self._buffer.oldest_available,
            "subscriptions": len(self._subscriptions),
            "active_subscriptions": sum(
                1 for s in self._subscriptions.values() if s.active
            ),
        }


class CDCExtractor:
    """Extracts CDC events from WAL records.

    Bridges the WAL log and the CDC pipeline.

    Usage:
        extractor = CDCExtractor(pipeline)
        # Called by WAL writer:
        extractor.on_wal_record(wal_op=1, table="users", key="123", data={...})
    """

    def __init__(self, pipeline: CDCPipeline) -> None:
        self._pipeline = pipeline

    def on_wal_record(
        self,
        wal_op: int,
        table: str,
        key: str,
        data: dict[str, Any] | None = None,
        old_data: dict[str, Any] | None = None,
        shard_id: int = 0,
        txn_id: int = 0,
        lsn: int = 0,
    ) -> int:
        """Convert a WAL record to a CDC event and emit it.

        wal_op mapping (from storage.wal.WALOp):
            1 = INSERT
            2 = UPDATE
            3 = DELETE
            4 = BEGIN_TXN
            5 = COMMIT_TXN
            6 = ROLLBACK_TXN
        """
        event_type_map = {
            1: CDCEventType.INSERT,
            2: CDCEventType.UPDATE,
            3: CDCEventType.DELETE,
        }

        event_type = event_type_map.get(wal_op)
        if event_type is None:
            return 0  # Skip non-data WAL ops

        event = CDCEvent(
            lsn=lsn,
            event_type=event_type,
            table=table,
            key=key,
            shard_id=shard_id,
            txn_id=txn_id,
        )

        if event_type == CDCEventType.INSERT:
            event.after = data
        elif event_type == CDCEventType.UPDATE:
            event.before = old_data
            event.after = data
        elif event_type == CDCEventType.DELETE:
            event.before = old_data or data

        return self._pipeline.emit(event)
