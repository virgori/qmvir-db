"""QM Event Bus — Publish/subscribe for database change events.

Supports:
    - Typed events (INSERT, UPDATE, DELETE, SCHEMA_CHANGE, etc.)
    - Topic-based subscriptions (by table, event type, or wildcard)
    - Synchronous and asynchronous delivery
    - Event history for replay
"""

from __future__ import annotations

import time
import threading
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable


class EventType(Enum):
    """Database event types."""
    ROW_INSERTED = "row_inserted"
    ROW_UPDATED = "row_updated"
    ROW_DELETED = "row_deleted"
    TABLE_CREATED = "table_created"
    TABLE_DROPPED = "table_dropped"
    SCHEMA_CHANGED = "schema_changed"
    TXN_COMMITTED = "txn_committed"
    TXN_ROLLED_BACK = "txn_rolled_back"
    CUSTOM = "custom"


@dataclass
class Event:
    """A single database event."""
    event_type: EventType
    table: str = ""
    key: str = ""
    data: dict[str, Any] = field(default_factory=dict)
    old_data: dict[str, Any] | None = None
    timestamp: float = field(default_factory=time.time)
    source: str = ""  # e.g., "txn:42", "trigger:audit_log"
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def topic(self) -> str:
        """Compute the topic for subscription matching."""
        return f"{self.table}.{self.event_type.value}"


# Subscription = (filter_fn, callback)
SubscriptionCallback = Callable[[Event], None]
SubscriptionFilter = Callable[[Event], bool] | None


@dataclass
class _Subscription:
    id: int
    callback: SubscriptionCallback
    filter_fn: SubscriptionFilter = None
    event_types: set[EventType] | None = None
    tables: set[str] | None = None


class EventBus:
    """Pub/sub event bus for database change events.

    Usage:
        bus = EventBus()

        # Subscribe to all inserts on 'articles'
        sub_id = bus.subscribe(
            callback=handle_insert,
            event_types={EventType.ROW_INSERTED},
            tables={"articles"},
        )

        # Publish an event
        bus.publish(Event(
            event_type=EventType.ROW_INSERTED,
            table="articles", key="a1",
            data={"title": "Hello"},
        ))

        # Unsubscribe
        bus.unsubscribe(sub_id)
    """

    def __init__(self, history_size: int = 1000) -> None:
        self._subscriptions: dict[int, _Subscription] = {}
        self._next_id = 1
        self._lock = threading.Lock()
        self._history: list[Event] = []
        self._history_size = history_size
        self._publish_count = 0

    def subscribe(
        self,
        callback: SubscriptionCallback,
        event_types: set[EventType] | None = None,
        tables: set[str] | None = None,
        filter_fn: SubscriptionFilter = None,
    ) -> int:
        """Subscribe to events. Returns a subscription ID."""
        with self._lock:
            sub_id = self._next_id
            self._next_id += 1
            self._subscriptions[sub_id] = _Subscription(
                id=sub_id,
                callback=callback,
                filter_fn=filter_fn,
                event_types=event_types,
                tables={t.lower() for t in tables} if tables else None,
            )
            return sub_id

    def unsubscribe(self, sub_id: int) -> bool:
        """Unsubscribe by ID."""
        with self._lock:
            return self._subscriptions.pop(sub_id, None) is not None

    def publish(self, event: Event) -> int:
        """Publish an event to all matching subscribers. Returns count of notified subscribers."""
        with self._lock:
            self._history.append(event)
            if len(self._history) > self._history_size:
                self._history = self._history[-self._history_size:]
            self._publish_count += 1
            subs = list(self._subscriptions.values())

        notified = 0
        for sub in subs:
            if self._matches(sub, event):
                sub.callback(event)
                notified += 1

        return notified

    def get_history(self, since_ts: float = 0.0, event_type: EventType | None = None) -> list[Event]:
        """Get event history, optionally filtered."""
        events = [e for e in self._history if e.timestamp > since_ts]
        if event_type:
            events = [e for e in events if e.event_type == event_type]
        return events

    @property
    def publish_count(self) -> int:
        return self._publish_count

    @property
    def subscriber_count(self) -> int:
        return len(self._subscriptions)

    @staticmethod
    def _matches(sub: _Subscription, event: Event) -> bool:
        """Check if an event matches a subscription's filters."""
        if sub.event_types and event.event_type not in sub.event_types:
            return False
        if sub.tables and event.table.lower() not in sub.tables:
            return False
        if sub.filter_fn and not sub.filter_fn(event):
            return False
        return True
