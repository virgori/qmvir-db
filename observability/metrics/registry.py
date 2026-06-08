"""QM Observability — Metrics collection.

Tracks:
  - Query latency (p50, p95, p99)
  - Throughput (queries/sec, writes/sec)
  - Cache hit rate
  - Index sizes
  - Engine-specific counters
"""

from __future__ import annotations

import time
import threading
from collections import defaultdict
from dataclasses import dataclass, field


@dataclass
class MetricPoint:
    """A single metric data point."""

    name: str
    value: float
    timestamp: float = field(default_factory=time.time)
    labels: dict[str, str] = field(default_factory=dict)


class Counter:
    """Monotonically increasing counter."""

    def __init__(self, name: str) -> None:
        self.name = name
        self._value = 0
        self._lock = threading.Lock()

    def inc(self, amount: int = 1) -> None:
        with self._lock:
            self._value += amount

    @property
    def value(self) -> int:
        return self._value


class Histogram:
    """Simple histogram for latency measurement."""

    def __init__(self, name: str, buckets: list[float] | None = None) -> None:
        self.name = name
        self._buckets = buckets or [0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 5.0]
        self._values: list[float] = []
        self._lock = threading.Lock()

    def observe(self, value: float) -> None:
        with self._lock:
            self._values.append(value)

    def percentile(self, p: float) -> float:
        """Get percentile (e.g., 0.95 for p95)."""
        with self._lock:
            if not self._values:
                return 0.0
            sorted_vals = sorted(self._values)
            idx = int(len(sorted_vals) * p)
            return sorted_vals[min(idx, len(sorted_vals) - 1)]

    @property
    def count(self) -> int:
        return len(self._values)

    @property
    def avg(self) -> float:
        with self._lock:
            return sum(self._values) / len(self._values) if self._values else 0.0


class Gauge:
    """Gauge metric (can go up and down)."""

    def __init__(self, name: str) -> None:
        self.name = name
        self._value: float = 0
        self._lock = threading.Lock()

    def set(self, value: float) -> None:
        with self._lock:
            self._value = value

    def inc(self, amount: float = 1) -> None:
        with self._lock:
            self._value += amount

    def dec(self, amount: float = 1) -> None:
        with self._lock:
            self._value -= amount

    @property
    def value(self) -> float:
        return self._value


class MetricsRegistry:
    """Central registry for all metrics."""

    def __init__(self) -> None:
        self._counters: dict[str, Counter] = {}
        self._histograms: dict[str, Histogram] = {}
        self._gauges: dict[str, Gauge] = {}

    def counter(self, name: str) -> Counter:
        if name not in self._counters:
            self._counters[name] = Counter(name)
        return self._counters[name]

    def histogram(self, name: str) -> Histogram:
        if name not in self._histograms:
            self._histograms[name] = Histogram(name)
        return self._histograms[name]

    def gauge(self, name: str) -> Gauge:
        if name not in self._gauges:
            self._gauges[name] = Gauge(name)
        return self._gauges[name]

    def snapshot(self) -> dict[str, Any]:
        """Get a snapshot of all metrics."""
        from typing import Any

        result: dict[str, Any] = {"counters": {}, "histograms": {}, "gauges": {}}

        for name, c in self._counters.items():
            result["counters"][name] = c.value

        for name, h in self._histograms.items():
            result["histograms"][name] = {
                "count": h.count,
                "avg": h.avg,
                "p50": h.percentile(0.5),
                "p95": h.percentile(0.95),
                "p99": h.percentile(0.99),
            }

        for name, g in self._gauges.items():
            result["gauges"][name] = g.value

        return result


# Global metrics registry
METRICS = MetricsRegistry()
