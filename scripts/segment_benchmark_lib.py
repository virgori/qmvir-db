#!/usr/bin/env python3
"""Shared schema + helpers for multi-segment QMVir benchmarks."""

from __future__ import annotations

import json
import os
import platform
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable

# Override via env, e.g. QM_DEPLOYMENT=docker_bridge QDRANT_DEPLOYMENT=docker_bridge
DEFAULT_COMPETITOR_DEPLOYMENT: dict[str, str] = {
    "postgresql": "native_host",
    "duckdb": "in_process_embedded",
    "qdrant": "docker_bridge",
}

WORKLOAD_DOCKER_SENSITIVITY: dict[str, str] = {
    "vector.l2_top10_hnsw": "low",
    "vector.l2_top50_hnsw": "low",
    "vector.cosine_top10_hnsw": "low",
    "text.fts_plainto_tsquery": "low",
    "text.bm25_persistent_vs_fts": "low",
    "json.path_filter": "low",
    "olap.group_by_count": "low",
    "olap.group_by_sum": "low",
    "vector.insert_autocommit": "high",
    "vector.update_autocommit": "high",
    "vector.batch_insert_multivalue": "high",
    "json.insert_autocommit": "high",
    "olap.full_scan_count": "medium",
    "olap.filter_scan_count": "medium",
}


def benchmark_deployment_metadata(competitor: str) -> dict[str, Any]:
    """Record how QM and the competitor were run — required for publishable results."""
    slug = competitor_slug(competitor)
    qm_dep = os.environ.get("QM_DEPLOYMENT", "native_host")
    env_key = f"{slug.upper()}_DEPLOYMENT"
    comp_dep = os.environ.get(env_key, DEFAULT_COMPETITOR_DEPLOYMENT.get(slug, "unknown"))
    host = os.environ.get("BENCHMARK_HOST", platform.node())

    if qm_dep == "native_host" and comp_dep == "native_host":
        fairness_mode = "all_native_host"
    elif "docker" in qm_dep and "docker" in comp_dep:
        fairness_mode = "all_container"
    elif comp_dep == "in_process_embedded":
        fairness_mode = "qm_native_vs_embedded_competitor"
    else:
        fairness_mode = "mixed"

    disclosure = (
        f"QM ({qm_dep}) vs {competitor} ({comp_dep}) on host {host}. "
        "CPU-bound workloads (HNSW, FTS, JSON index) are usually within ~0–5% "
        "across native/Docker on the same machine. I/O-bound workloads "
        "(INSERT, UPDATE, WAL fsync, bulk load) may skew 10–30%+ when one side "
        "uses container storage/network."
    )

    return {
        "benchmark_host": host,
        "qm_deployment": qm_dep,
        "competitor": competitor,
        "competitor_deployment": comp_dep,
        "fairness_mode": fairness_mode,
        "disclosure": disclosure,
        "publish_ready": fairness_mode in ("all_native_host", "all_container"),
        "workload_docker_sensitivity_hint": WORKLOAD_DOCKER_SENSITIVITY,
    }


def attach_deployment(payload: dict[str, Any], competitor: str) -> dict[str, Any]:
    meta = benchmark_deployment_metadata(competitor)
    payload["deployment"] = meta
    env = payload.setdefault("environment", {})
    env.update(
        {
            "os": platform.platform(),
            "python": platform.python_version(),
            "benchmark_host": meta["benchmark_host"],
        }
    )
    notes = payload.setdefault("notes", [])
    if isinstance(notes, list) and meta["disclosure"] not in notes:
        notes.append(meta["disclosure"])
    return payload


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def competitor_slug(name: str) -> str:
    return name.lower().replace(" ", "_").replace("-", "_")


def bench_pair(
    name: str,
    iterations: int,
    qm_fn: Callable[[], Any],
    competitor_fn: Callable[[], Any],
    *,
    competitor_name: str,
) -> dict[str, Any]:
    for _ in range(min(10, iterations)):
        qm_fn()
        competitor_fn()

    qm_samples: list[float] = []
    comp_samples: list[float] = []
    for _ in range(iterations):
        t0 = time.perf_counter()
        qm_fn()
        qm_samples.append((time.perf_counter() - t0) * 1000.0)
        t0 = time.perf_counter()
        competitor_fn()
        comp_samples.append((time.perf_counter() - t0) * 1000.0)

    qm_p50 = percentile(qm_samples, 50)
    comp_p50 = percentile(comp_samples, 50)
    if qm_p50 < comp_p50:
        winner = "QM"
    elif comp_p50 < qm_p50:
        winner = competitor_name
    else:
        winner = "tie"

    slug = competitor_slug(competitor_name)
    row: dict[str, Any] = {
        "workload": name,
        "iterations": iterations,
        "qm": {
            "p50_ms": qm_p50,
            "p95_ms": percentile(qm_samples, 95),
            "throughput_ops_sec": iterations / (sum(qm_samples) / 1000.0) if qm_samples else 0.0,
        },
        slug: {
            "p50_ms": comp_p50,
            "p95_ms": percentile(comp_samples, 95),
            "throughput_ops_sec": iterations / (sum(comp_samples) / 1000.0) if comp_samples else 0.0,
        },
        "competitor": competitor_name,
        "competitor_key": slug,
        "winner": winner,
    }
    # Legacy alias for scripts that expect postgresql key
    if slug == "postgresql":
        row["postgresql"] = row[slug]
    return row


def bench_throughput_pair(
    name: str,
    *,
    qm_rows_per_sec: float,
    competitor_rows_per_sec: float,
    competitor_name: str,
    rows: int,
) -> dict[str, Any]:
    slug = competitor_slug(competitor_name)
    winner = (
        "QM"
        if qm_rows_per_sec > competitor_rows_per_sec
        else competitor_name
        if competitor_rows_per_sec > qm_rows_per_sec
        else "tie"
    )
    row = {
        "workload": name,
        "rows": rows,
        "qm": {"rows_per_sec": qm_rows_per_sec},
        slug: {"rows_per_sec": competitor_rows_per_sec},
        "competitor": competitor_name,
        "competitor_key": slug,
        "winner": winner,
    }
    if slug == "postgresql":
        row["postgresql"] = row[slug]
    return row


@dataclass
class SegmentResult:
    segment: str
    competitor: str
    comparison: list[dict[str, Any]] = field(default_factory=list)
    notes: list[str] = field(default_factory=list)
    rows: int | None = None
    dim: int | None = None
    extra: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        slug = competitor_slug(self.competitor)
        wins = sum(1 for c in self.comparison if c.get("winner") == "QM")
        comp_wins = sum(1 for c in self.comparison if c.get("winner") == self.competitor)
        payload: dict[str, Any] = {
            "segment": self.segment,
            "competitor": self.competitor,
            "competitor_key": slug,
            "environment": {
                "os": platform.platform(),
                "python": platform.python_version(),
            },
            "comparison": self.comparison,
            "qm_wins": wins,
            f"{slug}_wins": comp_wins,
            "total": len(self.comparison),
            "notes": list(self.notes),
            **self.extra,
        }
        attach_deployment(payload, self.competitor)
        if self.rows is not None:
            payload["rows"] = self.rows
        if self.dim is not None:
            payload["dim"] = self.dim
        return payload


def write_segment_result(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def print_segment_summary(payload: dict[str, Any]) -> None:
    competitor = payload.get("competitor", "competitor")
    slug = payload.get("competitor_key") or competitor_slug(competitor)
    rows = payload.get("rows", "?")
    dep = payload.get("deployment", {})
    if dep:
        print(
            "deployment: QM=%s %s=%s [%s]"
            % (
                dep.get("qm_deployment", "?"),
                competitor,
                dep.get("competitor_deployment", "?"),
                dep.get("fairness_mode", "?"),
            )
        )
    print(
        "QM %s/%s @ %s rows [%s]"
        % (payload.get("qm_wins"), payload.get("total"), rows, competitor)
    )
    for c in payload.get("comparison", []):
        if c.get("skipped"):
            print("  %-36s SKIP %s" % (c.get("workload", "?"), c.get("reason", "")))
            continue
        qm = c.get("qm", {})
        comp = c.get(slug) or c.get(competitor_slug(competitor), {})
        if "p50_ms" in qm:
            print(
                "  %-36s QM=%.3fms %s=%.3fms %s"
                % (
                    c["workload"],
                    qm["p50_ms"],
                    competitor,
                    comp.get("p50_ms", 0),
                    c.get("winner", "?"),
                )
            )
        elif "rows_per_sec" in qm:
            print(
                "  %-36s QM=%.0f/s %s=%.0f/s %s"
                % (
                    c["workload"],
                    qm.get("rows_per_sec", 0),
                    competitor,
                    comp.get("rows_per_sec", 0),
                    c.get("winner", "?"),
                )
            )


SEGMENT_BENCHMARKS: dict[str, dict[str, str]] = {
    "oltp": {
        "postgresql": "compare_postgres_native_sql.py",
        "description": "Autocommit insert/update/delete, group commit",
    },
    "vector": {
        "postgresql_pgvector": "compare_postgres_vector_bench.py",
        "qdrant": "compare_qdrant_vector_bench.py",
        "description": "INSERT/UPDATE/KNN, batch load",
    },
    "search": {
        "postgresql": "compare_postgres_search_bench.py",
        "description": "FTS, trigram, JSON path",
    },
    "olap": {
        "duckdb": "compare_duckdb_olap_bench.py",
        "description": "GROUP BY, aggregation, scan",
    },
}
