#!/usr/bin/env python3
"""Shared helpers for the publish benchmark suite."""

from __future__ import annotations

import json
import os
import platform
import resource
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Callable


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((pct / 100.0) * (len(ordered) - 1))))
    return ordered[idx]


def max_rss_mb() -> float:
    rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    if sys.platform == "darwin":
        return rss / (1024.0 * 1024.0)
    return rss / 1024.0


def command_version(cmd: list[str]) -> str | None:
    try:
        return subprocess.check_output(cmd, stderr=subprocess.STDOUT, text=True).strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def environment_info(build_mode: str = "release", feature_flags: str = "default") -> dict[str, Any]:
    return {
        "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "os": platform.platform(),
        "python": platform.python_version(),
        "cpu": platform.processor() or platform.machine(),
        "rustc_version": command_version(["rustc", "--version"]),
        "build_mode": build_mode,
        "feature_flags": feature_flags,
        "max_rss_mb_start": max_rss_mb(),
    }


def run_subprocess_json(
    cmd: list[str],
    *,
    env: dict[str, str] | None = None,
    cwd: Path | None = None,
    timeout_s: float | None = None,
) -> tuple[int, dict[str, Any] | None, str]:
    merged = os.environ.copy()
    if env:
        merged.update(env)
    proc = subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        env=merged,
        capture_output=True,
        text=True,
        timeout=timeout_s,
    )
    stdout = proc.stdout or ""
    stderr = proc.stderr or ""
    combined = stdout + ("\n" + stderr if stderr else "")
    payload: dict[str, Any] | None = None
    for line in reversed(stdout.splitlines()):
        line = line.strip()
        if line.startswith("{") and line.endswith("}"):
            try:
                payload = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
    if payload is None and stdout.strip():
        try:
            payload = json.loads(stdout)
        except json.JSONDecodeError:
            payload = None
    return proc.returncode, payload, combined


def median_across_runs(runs: list[dict[str, Any]], path: tuple[str, ...]) -> float | None:
    values: list[float] = []
    for run in runs:
        node: Any = run
        for key in path:
            if not isinstance(node, dict) or key not in node:
                node = None
                break
            node = node[key]
        if isinstance(node, (int, float)):
            values.append(float(node))
    if not values:
        return None
    return statistics.median(values)


def aggregate_comparison_runs(runs: list[dict[str, Any]]) -> dict[str, Any]:
    """Median p50 per workload across multiple search/oltp comparison runs."""
    by_workload: dict[str, list[dict[str, Any]]] = {}
    for run in runs:
        for row in run.get("comparison", run.get("results", [])):
            name = row.get("workload") or row.get("name")
            if not name:
                continue
            by_workload.setdefault(name, []).append(row)

    aggregated: list[dict[str, Any]] = []
    qm_wins = 0
    pg_wins = 0
    for name, rows in sorted(by_workload.items()):
        if rows[0].get("skipped"):
            aggregated.append(rows[0])
            continue
        qm_p50s = [r["qm"]["p50_ms"] for r in rows if r.get("qm", {}).get("p50_ms") is not None]
        pg_p50s = [
            r["postgresql"]["p50_ms"]
            for r in rows
            if r.get("postgresql", {}).get("p50_ms") is not None
        ]
        if not qm_p50s:
            qm_p50s = [r.get("p50_ms") for r in rows if r.get("p50_ms") is not None]  # type: ignore[misc]
        qm_med = statistics.median(qm_p50s) if qm_p50s else None
        pg_med = statistics.median(pg_p50s) if pg_p50s else None
        winner = "QM" if qm_med is not None and pg_med is not None and qm_med < pg_med else (
            "PostgreSQL" if qm_med is not None and pg_med is not None and pg_med < qm_med else "tie"
        )
        if winner == "QM":
            qm_wins += 1
        elif winner == "PostgreSQL":
            pg_wins += 1
        entry: dict[str, Any] = {
            "workload": name,
            "runs": len(rows),
            "qm_median_p50_ms": qm_med,
            "postgresql_median_p50_ms": pg_med,
            "winner": winner,
        }
        if qm_med and pg_med and qm_med > 0:
            entry["qm_ops_ratio_vs_postgresql"] = pg_med / qm_med
        aggregated.append(entry)

    return {
        "runs": len(runs),
        "comparison": aggregated,
        "qm_wins": qm_wins,
        "postgresql_wins": pg_wins,
        "total": len([r for r in aggregated if not r.get("skipped")]),
    }


def bench_latency(name: str, iterations: int, fn: Callable[[], Any], warmup: int = 10) -> dict[str, Any]:
    for _ in range(min(warmup, iterations)):
        fn()
    latencies_ms: list[float] = []
    start = time.perf_counter()
    for _ in range(iterations):
        op_start = time.perf_counter()
        fn()
        latencies_ms.append((time.perf_counter() - op_start) * 1000.0)
    elapsed = time.perf_counter() - start
    return {
        "name": name,
        "iterations": iterations,
        "p50_ms": percentile(latencies_ms, 50),
        "p95_ms": percentile(latencies_ms, 95),
        "p99_ms": percentile(latencies_ms, 99),
        "mean_ms": statistics.fmean(latencies_ms) if latencies_ms else 0.0,
        "throughput_ops_sec": iterations / elapsed if elapsed > 0 else 0.0,
    }


def render_markdown(report: dict[str, Any]) -> str:
    env = report.get("environment", {})
    summary = report.get("summary", {})
    lines = [
        f"# QM Publish Benchmark — {report.get('release_tag', 'unreleased')}",
        "",
        f"Generated: {env.get('timestamp_utc', 'unknown')}",
        f"Mode: **{report.get('mode', 'full')}** | Runs: **{report.get('runs', 1)}**",
        "",
        "## Environment",
        "",
        f"- OS: {env.get('os', 'unknown')}",
        f"- Python: {env.get('python', 'unknown')}",
        f"- Rust: {env.get('rustc_version', 'unknown')}",
        f"- Build: {env.get('build_mode', 'unknown')} ({env.get('feature_flags', 'default')})",
        "",
        "## Summary",
        "",
    ]

    if summary.get("oltp"):
        o = summary["oltp"]
        lines.append(
            f"- **OLTP vs PostgreSQL:** QM {o.get('qm_wins', 0)}/{o.get('total', 0)} wins "
            f"(median of {report.get('runs', 1)} runs)"
        )
    if summary.get("search"):
        s = summary["search"]
        lines.append(
            f"- **Search vs PostgreSQL:** QM {s.get('qm_wins', 0)}/{s.get('total', 0)} wins"
        )
    for scale in summary.get("search_scale", []):
        lines.append(
            f"- **Search @ {scale.get('rows', '?')} rows:** QM {scale.get('qm_wins', 0)}/{scale.get('total', 0)} wins"
        )
    if summary.get("recovery"):
        r = summary["recovery"]
        lines.append(
            f"- **Crash recovery:** reopen+query p50 {r.get('reopen_query_p50_ms', 'n/a')} ms, "
            f"row count ok={r.get('row_count_ok', False)}"
        )
    if summary.get("concurrent_oltp"):
        c = summary["concurrent_oltp"]
        lines.append(
            f"- **Concurrent OLTP:** {c.get('throughput_ops_sec', 0):.0f} ops/s "
            f"({c.get('threads', '?')} threads)"
        )
    if summary.get("mixed_workload"):
        m = summary["mixed_workload"]
        lines.append(
            f"- **Mixed workload:** {m.get('throughput_ops_sec', 0):.0f} ops/s "
            f"({m.get('read_ratio', 0.7):.0%} reads)"
        )
    if summary.get("release_gate"):
        g = summary["release_gate"]
        lines.append(
            f"- **P0 release gate:** {g.get('passed', 0)}/{g.get('total', 0)} passed "
            f"(ok={g.get('ok', False)})"
        )

    lines.extend(["", "## OLTP vs PostgreSQL (median p50)", "", "| Workload | QM p50 | PG p50 | Winner |", "|---|---:|---:|---|"])
    oltp = report.get("sections", {}).get("oltp_vs_postgres", {})
    for row in oltp.get("comparison", []):
        if row.get("skipped"):
            continue
        qm = row.get("qm_median_p50_ms", row.get("qm", {}).get("p50_ms"))
        pg = row.get("postgresql_median_p50_ms", row.get("postgresql", {}).get("p50_ms"))
        winner = row.get("winner", "?")
        name = row.get("workload") or row.get("name")
        qm_s = f"{qm:.3f} ms" if isinstance(qm, (int, float)) else "n/a"
        pg_s = f"{pg:.3f} ms" if isinstance(pg, (int, float)) else "n/a"
        lines.append(f"| {name} | {qm_s} | {pg_s} | {winner} |")

    lines.extend(["", "## Search vs PostgreSQL (median p50)", "", "| Workload | QM p50 | PG p50 | PG/QM | Winner |", "|---|---:|---:|---:|---|"])
    search = report.get("sections", {}).get("search_vs_postgres", {})
    for row in search.get("comparison", []):
        if row.get("skipped"):
            continue
        qm = row.get("qm_median_p50_ms", row.get("qm", {}).get("p50_ms"))
        pg = row.get("postgresql_median_p50_ms", row.get("postgresql", {}).get("p50_ms"))
        winner = row.get("winner", "?")
        name = row.get("workload")
        ratio = row.get("qm_ops_ratio_vs_postgresql")
        ratio_s = f"{ratio:.2f}x" if isinstance(ratio, (int, float)) else "n/a"
        qm_s = f"{qm:.3f} ms" if isinstance(qm, (int, float)) else "n/a"
        pg_s = f"{pg:.3f} ms" if isinstance(pg, (int, float)) else "n/a"
        lines.append(f"| {name} | {qm_s} | {pg_s} | {ratio_s} | {winner} |")

    lines.extend(
        [
            "",
            "## P0 Release Gate",
            "",
        ]
    )
    gate = report.get("sections", {}).get("release_gate_realdata", {})
    if gate:
        for name, section in gate.get("sections", {}).items():
            ok = section.get("ok", section.get("skipped"))
            status = "PASS" if ok else ("SKIP" if section.get("skipped") else "FAIL")
            lines.append(f"- **{name}:** {status}")
        lines.append("")
        bulk = gate.get("sections", {}).get("bulk_ingest", {})
        for prof in bulk.get("profiles", []):
            rps = prof.get("ingest_rows_per_sec", 0)
            lines.append(
                f"  - bulk {prof.get('rows_target', '?')}: "
                f"{rps:.0f} rows/s ingest, index {prof.get('index_build_elapsed_s', 0):.1f}s"
            )
    else:
        lines.append("- (not run)")

    lines.extend(
        [
            "",
            "## Disclosure",
            "",
            "- QM persistent WAL benches use engine-native sync policies; see per-section metadata.",
            "- PostgreSQL uses `synchronous_commit=on` unless noted in section output.",
            "- Vector search: QM uses exact sort unless HNSW wired; PG uses HNSW when pgvector is installed.",
            "- FTS: QM inverted GIN (BMW); PostgreSQL `to_tsvector` GIN — not BM25 on PG side.",
            "",
        ]
    )
    return "\n".join(lines) + "\n"
