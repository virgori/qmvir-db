"""QM Pipelines — Outbox consumer.

Reads outbox entries from core DB and dispatches to downstream consumers:
  - Search indexer
  - Vector indexer
  - Analytics loader
  - Cache invalidator
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any, Callable

from core_db.wal_cdc.wal import OutboxWriter, OutboxEntry


@dataclass
class ConsumerState:
    """Track consumer processing state."""

    consumer_name: str
    last_processed_id: int = 0
    total_processed: int = 0
    last_error: str | None = None
    last_run_at: float = 0.0


OutboxHandler = Callable[[OutboxEntry], bool]


class OutboxConsumer:
    """Consumes outbox entries and dispatches to registered handlers."""

    def __init__(self, outbox: OutboxWriter) -> None:
        self._outbox = outbox
        self._handlers: dict[str, OutboxHandler] = {}
        self._states: dict[str, ConsumerState] = {}

    def register_handler(self, name: str, handler: OutboxHandler) -> None:
        """Register a named consumer handler."""
        self._handlers[name] = handler
        self._states[name] = ConsumerState(consumer_name=name)

    def poll_and_dispatch(self, batch_size: int = 100) -> int:
        """Poll outbox for new entries and dispatch to all handlers."""
        entries = self._outbox.get_unprocessed(limit=batch_size)
        dispatched = 0

        for entry in entries:
            all_ok = True
            for name, handler in self._handlers.items():
                state = self._states[name]
                try:
                    ok = handler(entry)
                    if ok:
                        state.total_processed += 1
                        state.last_processed_id = entry.entry_id
                    else:
                        all_ok = False
                except Exception as exc:
                    state.last_error = str(exc)
                    all_ok = False

                state.last_run_at = time.time()

            if all_ok:
                self._outbox.mark_processed(entry.entry_id)
                dispatched += 1

        return dispatched

    def get_consumer_states(self) -> dict[str, ConsumerState]:
        return dict(self._states)
