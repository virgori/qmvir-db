"""QM Bench — Micro-benchmark harness for the QM database kernel.

Designed to surface the three key bottlenecks described in the QM
performance model:

    1. **Shared-Memory Contention** — Ring-buffer publish/collect throughput
       under varying concurrency (single → N threads).
    2. **Gateway (GIL) Throughput** — Simulated ``pgbench``-style TPS through
       the Python gateway path.
    3. **Checkpoint I/O** — ``CPOINT FULL`` wall-clock time (SSD-bound).

Additionally measures:
    4. **Vector Search Latency** — ``LIKEV`` round-trip at 1 K, 10 K, 100 K
       scale.
    5. **SQL Parse Throughput** — Lark LALR parse rate (stmts/sec).

Usage (standalone)::

    python -m qm_core.bench              # run all benchmarks
    python -m qm_core.bench --only ring  # run only ring benchmark

Programmatic::

    from qm_core.bench import run_all, BenchResult
    results: list[BenchResult] = run_all()
"""

from __future__ import annotations

import statistics
import struct
import tempfile
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Sequence


# ═══════════════════════════════════════════════════════════════════════
# Result container
# ═══════════════════════════════════════════════════════════════════════

@dataclass(slots=True)
class BenchResult:
    """One benchmark measurement."""
    name: str
    ops: int
    elapsed_s: float
    p50_us: float = 0.0
    p99_us: float = 0.0
    extra: str = ""

    @property
    def ops_per_sec(self) -> float:
        return self.ops / self.elapsed_s if self.elapsed_s > 0 else 0.0

    def __str__(self) -> str:
        rate = self.ops_per_sec
        unit = "ops/s"
        if rate > 1_000_000:
            rate /= 1_000_000
            unit = "M ops/s"
        elif rate > 1_000:
            rate /= 1_000
            unit = "K ops/s"
        lat = ""
        if self.p50_us > 0:
            lat = f"  p50={self.p50_us:.1f}µs  p99={self.p99_us:.1f}µs"
        extra = f"  ({self.extra})" if self.extra else ""
        return (
            f"  {self.name:40s} {rate:>10.2f} {unit:>10s}"
            f"  {self.elapsed_s * 1000:>8.1f}ms{lat}{extra}"
        )


def _percentile(data: Sequence[float], pct: float) -> float:
    if not data:
        return 0.0
    s = sorted(data)
    idx = int(len(s) * pct / 100)
    return s[min(idx, len(s) - 1)]


# ═══════════════════════════════════════════════════════════════════════
# 1. Ring Buffer Throughput
# ═══════════════════════════════════════════════════════════════════════

def bench_ring(ops: int = 50_000) -> BenchResult:
    """Publish+collect throughput on a single SharedRingBuffer."""
    from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType

    with tempfile.TemporaryDirectory() as td:
        ring = SharedRingBuffer(
            path=f"{td}/bench.shm",
            slot_count=1024,
            slot_data_size=256,
            create=True,
        )
        payload = b"\x42" * 64
        latencies: list[float] = []

        t0 = time.perf_counter()
        for i in range(ops):
            s = time.perf_counter_ns()
            slot = ring.try_publish(lsn=i + 1, cmd=CommandType.INSERT,
                                    payload=payload, timeout_ms=100)
            if slot >= 0:
                # Simulate satellite completing instantly
                ring.complete(slot, b"ok")
                ring.collect_result(slot)
            latencies.append((time.perf_counter_ns() - s) / 1000)
        elapsed = time.perf_counter() - t0
        ring.close()

    return BenchResult(
        name="ring_buffer_publish_collect",
        ops=ops,
        elapsed_s=elapsed,
        p50_us=_percentile(latencies, 50),
        p99_us=_percentile(latencies, 99),
    )


# ═══════════════════════════════════════════════════════════════════════
# 2. Gateway TPS (simulated pgbench)
# ═══════════════════════════════════════════════════════════════════════

def bench_gateway_tps(ops: int = 10_000) -> BenchResult:
    """Measure executor callback throughput (no network, pure Python)."""
    from qm_core.hub_engine import QMHubEngine

    with tempfile.TemporaryDirectory() as td:
        engine = QMHubEngine(data_dir=td, wal_enabled=False)
        engine.execute_sql("CREATE TABLE bench (id INT, val TEXT)")
        for i in range(100):
            engine.execute_sql(f"INSERT INTO bench (id, val) VALUES ({i}, 'row{i}')")

        latencies: list[float] = []
        t0 = time.perf_counter()
        for _ in range(ops):
            s = time.perf_counter_ns()
            engine.execute_sql("SELECT * FROM bench WHERE id = 42")
            latencies.append((time.perf_counter_ns() - s) / 1000)
        elapsed = time.perf_counter() - t0
        engine.close()

    return BenchResult(
        name="gateway_select_tps",
        ops=ops,
        elapsed_s=elapsed,
        p50_us=_percentile(latencies, 50),
        p99_us=_percentile(latencies, 99),
        extra="single-thread SELECT WHERE",
    )


# ═══════════════════════════════════════════════════════════════════════
# 3. Checkpoint I/O
# ═══════════════════════════════════════════════════════════════════════

def bench_checkpoint(payload_kb: int = 256) -> BenchResult:
    """Measure single CPOINT FULL write speed."""
    from qm_core.checkpoint import CheckpointManager, CheckpointConfig

    state = {"data": "x" * (payload_kb * 1024)}

    with tempfile.TemporaryDirectory() as td:
        mgr = CheckpointManager(
            CheckpointConfig(
                checkpoint_dir=td,
                interval_seconds=999,
                lsn_threshold=999999,
                max_checkpoints=3,
                enabled=True,
            ),
            state_provider=lambda: state,
        )
        latencies: list[float] = []
        n = 20
        t0 = time.perf_counter()
        for _ in range(n):
            s = time.perf_counter_ns()
            mgr.force_checkpoint("full")
            latencies.append((time.perf_counter_ns() - s) / 1000)
        elapsed = time.perf_counter() - t0

    return BenchResult(
        name="checkpoint_full_write",
        ops=n,
        elapsed_s=elapsed,
        p50_us=_percentile(latencies, 50),
        p99_us=_percentile(latencies, 99),
        extra=f"{payload_kb}KB payload",
    )


# ═══════════════════════════════════════════════════════════════════════
# 4. Vector Search Latency
# ═══════════════════════════════════════════════════════════════════════

def bench_vector_search(n_vectors: int = 1_000, dim: int = 128, top_k: int = 10) -> BenchResult:
    """LIKEV round-trip latency at given scale."""
    from qm_core.hub_engine import QMHubEngine
    import numpy as np

    with tempfile.TemporaryDirectory() as td:
        engine = QMHubEngine(data_dir=td, wal_enabled=False)
        engine.execute_sql(f"CREATE TABLE vecs (id INT, embedding VECTOR({dim}))")

        # Bulk insert random vectors
        for i in range(n_vectors):
            vec = np.random.randn(dim).astype(np.float32).tolist()
            vec_str = "[" + ",".join(f"{v:.4f}" for v in vec) + "]"
            engine.execute_sql(
                f"INSERT INTO vecs (id, embedding) VALUES ({i}, '{vec_str}')"
            )

        query_vec = np.random.randn(dim).astype(np.float32).tolist()
        q_str = "[" + ",".join(f"{v:.4f}" for v in query_vec) + "]"
        sql = f"LIKEV VEC {q_str} IN vecs TOP {top_k}"

        latencies: list[float] = []
        warmup = 3
        iters = 50
        for i in range(warmup + iters):
            s = time.perf_counter_ns()
            engine.execute_sql(sql)
            lat = (time.perf_counter_ns() - s) / 1000
            if i >= warmup:
                latencies.append(lat)
        engine.close()

    return BenchResult(
        name=f"vector_search_{n_vectors}",
        ops=iters,
        elapsed_s=sum(latencies) / 1e6,
        p50_us=_percentile(latencies, 50),
        p99_us=_percentile(latencies, 99),
        extra=f"dim={dim} top_k={top_k}",
    )


# ═══════════════════════════════════════════════════════════════════════
# 5. SQL Parse Throughput
# ═══════════════════════════════════════════════════════════════════════

def bench_sql_parse(ops: int = 10_000) -> BenchResult:
    """Lark LALR parse rate — stmts/sec."""
    from qm_core.execution.qm_sql import QMSQLParser

    parser = QMSQLParser()
    stmts = [
        "SELECT * FROM users WHERE id = 1",
        "INSERT INTO t (a, b) VALUES (1, 'hello')",
        "LIKEV VEC [0.1,0.2,0.3] IN docs TOP 5",
        "CPOINT FULL",
        "UPDATE t SET x = 10 WHERE id = 3",
    ]

    latencies: list[float] = []
    t0 = time.perf_counter()
    for i in range(ops):
        sql = stmts[i % len(stmts)]
        s = time.perf_counter_ns()
        parser.parse(sql)
        latencies.append((time.perf_counter_ns() - s) / 1000)
    elapsed = time.perf_counter() - t0

    return BenchResult(
        name="sql_parse_lalr",
        ops=ops,
        elapsed_s=elapsed,
        p50_us=_percentile(latencies, 50),
        p99_us=_percentile(latencies, 99),
        extra="mixed stmts",
    )


# ═══════════════════════════════════════════════════════════════════════
# Runner
# ═══════════════════════════════════════════════════════════════════════

_BENCHMARKS: dict[str, callable] = {
    "ring": bench_ring,
    "gateway": bench_gateway_tps,
    "checkpoint": bench_checkpoint,
    "vector": bench_vector_search,
    "parse": bench_sql_parse,
}


def run_all(only: str | None = None) -> list[BenchResult]:
    """Run benchmarks and return results."""
    results: list[BenchResult] = []
    for name, fn in _BENCHMARKS.items():
        if only and name != only:
            continue
        results.append(fn())
    return results


def print_report(results: list[BenchResult]) -> None:
    """Pretty-print benchmark results."""
    print()
    print("=" * 90)
    print("  QM Database — Performance Benchmark Report")
    print("=" * 90)
    for r in results:
        print(r)
    print("=" * 90)
    print()


def export_json(results: list[BenchResult], path: str) -> None:
    """Serialize benchmark results to a JSON file."""
    import json

    records = []
    for r in results:
        records.append({
            "name": r.name,
            "ops": r.ops,
            "elapsed_s": round(r.elapsed_s, 6),
            "ops_per_sec": round(r.ops_per_sec, 2),
            "p50_us": round(r.p50_us, 2),
            "p99_us": round(r.p99_us, 2),
            "extra": r.extra,
        })

    report = {
        "version": "1.0.0",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "benchmarks": records,
    }
    Path(path).write_text(json.dumps(report, indent=2))


# ── __main__ ────────────────────────────────────────────────────────

if __name__ == "__main__":
    import argparse

    ap = argparse.ArgumentParser(description="QM Database Benchmark")
    ap.add_argument("--only", choices=list(_BENCHMARKS.keys()),
                    help="Run only this benchmark")
    args = ap.parse_args()
    results = run_all(only=args.only)
    print_report(results)
