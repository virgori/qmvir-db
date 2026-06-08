"""QM Observability — Slow query log.

Logs queries exceeding a latency threshold for analysis.
"""

from __future__ import annotations

import time
from collections import deque
from dataclasses import dataclass, field
from typing import Any


@dataclass
class SlowQueryEntry:
    """A slow query log entry."""

    query_id: str
    engine: str  # core_db, search, vector, analytics
    query: dict[str, Any]
    duration_ms: float
    timestamp: float = field(default_factory=time.time)
    plan: dict[str, Any] | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.query_id,
            "engine": self.engine,
            "query": self.query,
            "duration_ms": self.duration_ms,
            "ts": self.timestamp,
            "plan": self.plan,
        }


class SlowQueryLog:
    """Collects and stores slow queries for analysis."""

    def __init__(
        self,
        threshold_ms: float = 100.0,
        max_entries: int = 10_000,
    ) -> None:
        self.threshold_ms = threshold_ms
        self._entries: deque[SlowQueryEntry] = deque(maxlen=max_entries)
        self._total_slow: int = 0

    def record(
        self,
        query_id: str,
        engine: str,
        query: dict[str, Any],
        duration_ms: float,
        plan: dict[str, Any] | None = None,
    ) -> bool:
        """Record a query if it exceeds the threshold. Returns True if slow."""
        if duration_ms >= self.threshold_ms:
            entry = SlowQueryEntry(
                query_id=query_id,
                engine=engine,
                query=query,
                duration_ms=duration_ms,
                plan=plan,
            )
            self._entries.append(entry)
            self._total_slow += 1
            return True
        return False

    def get_recent(self, limit: int = 50) -> list[SlowQueryEntry]:
        """Get recent slow queries."""
        entries = list(self._entries)
        return entries[-limit:]

    def get_slowest(self, limit: int = 10) -> list[SlowQueryEntry]:
        """Get the slowest queries."""
        return sorted(self._entries, key=lambda e: e.duration_ms, reverse=True)[:limit]

    @property
    def total_slow_queries(self) -> int:
        return self._total_slow
