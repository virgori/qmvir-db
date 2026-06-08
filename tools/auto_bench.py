#!/usr/bin/env python3
"""Automated Benchmark Script for the QM Database (qmvir).

Executes a full benchmark cycle — from low-level IPC to high-level SQL —
and produces both a human-readable console report and a machine-readable
JSON file.

Workflow
--------
1. Start the QM daemon in the background (``qmvir start``).
2. Run the integrated benchmark suite (``qm_core.bench``).
3. Stop the daemon gracefully (``qmvir stop``).
4. Emit ``bench_report.json`` and a KPI summary table.

The script can also run **without** a live daemon by invoking the bench
module directly (the benchmarks create temporary engines internally).

Usage::

    python tools/auto_bench.py                      # full suite
    python tools/auto_bench.py --only vector         # single benchmark
    python tools/auto_bench.py --no-server           # skip daemon start/stop
    python tools/auto_bench.py --json results.json   # custom output path
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

# Ensure the repo root is on sys.path so ``import qm_core`` works when
# running the script directly (not via ``pip install -e .``).
_REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(_REPO_ROOT))


# ═══════════════════════════════════════════════════════════════════════
# KPI table
# ═══════════════════════════════════════════════════════════════════════

_KPI_MAP: dict[str, dict[str, str]] = {
    "ring_buffer_publish_collect": {
        "group": "Ring Buffer IPC",
        "metric": "Throughput (ops/s)",
        "significance": "Khả năng xử lý lệnh không khóa (Lock-free).",
    },
    "gateway_select_tps": {
        "group": "SQL Gateway",
        "metric": "TPS (Transactions/sec)",
        "significance": "Hiệu năng lớp giao thức PostgreSQL và Parser.",
    },
    "vector_search_1000": {
        "group": "Vector Search",
        "metric": "Latency p99 (µs)",
        "significance": "Độ trễ tìm kiếm ANN tối đa.",
    },
    "checkpoint_full_write": {
        "group": "Checkpoint I/O",
        "metric": "Write speed (ops/s)",
        "significance": "Tốc độ ghi trạng thái SSD.",
    },
    "sql_parse_lalr": {
        "group": "SQL Parse",
        "metric": "Parse rate (stmts/s)",
        "significance": "Throughput Lark LALR parser.",
    },
}


def _print_kpi_table(results: list) -> None:
    """Print a summarized KPI table from BenchResult objects."""
    print()
    print("┌─────────────────────────┬───────────────────┬──────────────┬──────────────┐")
    print("│ Nhóm đo kiểm            │ Chỉ số            │ ops/s        │ p99 (µs)     │")
    print("├─────────────────────────┼───────────────────┼──────────────┼──────────────┤")
    for r in results:
        kpi = _KPI_MAP.get(r.name, {})
        group = kpi.get("group", r.name)[:23]
        metric = kpi.get("metric", "—")[:17]
        ops_s = f"{r.ops_per_sec:,.0f}"
        p99 = f"{r.p99_us:,.1f}" if r.p99_us > 0 else "—"
        print(f"│ {group:23s} │ {metric:17s} │ {ops_s:>12s} │ {p99:>12s} │")
    print("└─────────────────────────┴───────────────────┴──────────────┴──────────────┘")
    print()


# ═══════════════════════════════════════════════════════════════════════
# Server lifecycle helpers
# ═══════════════════════════════════════════════════════════════════════

def _find_qmvir() -> str:
    """Return the path to the qmvir CLI (or fallback to python -m qm_app)."""
    # Prefer installed entry-point
    from shutil import which
    exe = which("qmvir") or which("qm-server")
    if exe:
        return exe
    # Fallback: run as module
    return ""


def _start_server(data_dir: str, timeout: float = 5.0) -> subprocess.Popen | None:
    """Start ``qmvir start`` in the background and wait for the state file."""
    exe = _find_qmvir()
    if exe:
        cmd = [exe, "start", "--data-dir", data_dir]
    else:
        cmd = [sys.executable, str(_REPO_ROOT / "qm_app.py"),
               "start", "--data-dir", data_dir]

    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    # Wait for state file to appear (indicates daemon is ready)
    state_path = Path(data_dir) / "qm_daemon.state"
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if state_path.exists():
            return proc
        time.sleep(0.25)
    return proc


def _stop_server(data_dir: str, proc: subprocess.Popen | None) -> None:
    """Gracefully stop the daemon."""
    pid_path = Path(data_dir) / "qm_daemon.pid"
    if pid_path.exists():
        try:
            pid = int(pid_path.read_text().strip())
            os.kill(pid, signal.SIGTERM)
        except (OSError, ValueError):
            pass

    if proc is not None:
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.terminate()


# ═══════════════════════════════════════════════════════════════════════
# Main runner
# ═══════════════════════════════════════════════════════════════════════

def run_auto_benchmark(
    *,
    only: str | None = None,
    json_path: str = "bench_report.json",
    data_dir: str = "/tmp/qm_bench_data",
    use_server: bool = True,
) -> list:
    """Run the full automated benchmark cycle.

    Returns a list of ``BenchResult`` objects.
    """
    from qm_core.bench import run_all, print_report, export_json

    print(f"=== QMvir Automated Benchmark v1.0.0 ===")
    print(f"  Data dir  : {data_dir}")
    print(f"  Filter    : {only or 'all'}")
    print(f"  JSON out  : {json_path}")
    print()

    proc = None
    if use_server:
        print("[1/3] Khởi động QM Server...")
        proc = _start_server(data_dir)
        time.sleep(1)
    else:
        print("[1/3] Bỏ qua khởi động server (--no-server)")

    # ── Run benchmarks ──────────────────────────────────────────────
    print("[2/3] Thực thi các kịch bản đo kiểm...")
    results = run_all(only=only)
    print_report(results)

    # KPI summary
    _print_kpi_table(results)

    # JSON export
    export_json(results, json_path)
    print(f"[*] JSON report: {json_path}")

    # ── Cleanup ─────────────────────────────────────────────────────
    if use_server:
        print("[3/3] Đang dừng hệ thống...")
        _stop_server(data_dir, proc)
    else:
        print("[3/3] Bỏ qua dừng server")

    print(f"\n=== HOÀN TẤT. {len(results)} benchmark(s) đã chạy xong ===")
    return results


# ═══════════════════════════════════════════════════════════════════════
# CLI
# ═══════════════════════════════════════════════════════════════════════

def _build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(
        prog="auto_bench",
        description="QMvir Automated Benchmark — đo kiểm hiệu năng toàn diện",
    )
    ap.add_argument(
        "--only",
        choices=["ring", "gateway", "checkpoint", "vector", "parse"],
        help="Chỉ chạy benchmark được chỉ định",
    )
    ap.add_argument(
        "--json",
        default="bench_report.json",
        help="Đường dẫn file JSON báo cáo (default: bench_report.json)",
    )
    ap.add_argument(
        "--data-dir",
        default="/tmp/qm_bench_data",
        help="Thư mục dữ liệu cho daemon",
    )
    ap.add_argument(
        "--no-server",
        action="store_true",
        help="Bỏ qua khởi động/dừng daemon (chạy benchmark trực tiếp)",
    )
    return ap


def main() -> None:
    ap = _build_parser()
    args = ap.parse_args()
    run_auto_benchmark(
        only=args.only,
        json_path=args.json,
        data_dir=args.data_dir,
        use_server=not args.no_server,
    )


if __name__ == "__main__":
    main()
