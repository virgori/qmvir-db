#!/usr/bin/env python3
"""Automated stress + chaos harness for QMvir.

Scenarios:
1. hard_kill_under_write_load
2. memory_pressure_under_write_load
3. restart_recovery_smoke
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import signal
import subprocess
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

try:
    import psycopg2
except ImportError as exc:  # pragma: no cover
    raise SystemExit("Install psycopg2-binary first: pip install psycopg2-binary") from exc


@dataclass
class ChaosConfig:
    qmvir_bin: str
    data_dir: str
    host: str
    port: int
    user: str
    password: str
    dbname: str
    duration_s: int
    clients: int
    output_json: str


def _qmvir_cmd(cfg: ChaosConfig, *extra: str) -> list[str]:
    base = shlex.split(cfg.qmvir_bin)
    return [*base, *extra]


def _connect(cfg: ChaosConfig):
    return psycopg2.connect(
        host=cfg.host,
        port=cfg.port,
        user=cfg.user,
        password=cfg.password,
        dbname=cfg.dbname,
        connect_timeout=5,
    )


def _start_daemon(cfg: ChaosConfig) -> subprocess.Popen[str]:
    env = os.environ.copy()
    qm_root = "/Users/gengyang/Desktop/AI/QM"
    existing = env.get("PYTHONPATH", "")
    env["PYTHONPATH"] = qm_root if not existing else f"{qm_root}:{existing}"
    cmd = _qmvir_cmd(
        cfg,
        "--data-dir",
        cfg.data_dir,
        "start",
        "--host",
        cfg.host,
        "--port",
        str(cfg.port),
    )
    p = subprocess.Popen(
        cmd,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env=env,
        cwd="/Users/gengyang/Desktop/AI",
    )

    # Readiness gate: wait until this daemon process reports ready and SQL endpoint accepts.
    state_path = Path(cfg.data_dir) / "qm_daemon.state"
    deadline = time.time() + 20.0
    while time.time() < deadline:
        try:
            if state_path.exists():
                state = json.loads(state_path.read_text())
                state_pid = int(state.get("pid") or 0)
                if state_pid == p.pid and state.get("ready"):
                    if _is_reachable(cfg):
                        return p
        except Exception:
            pass
        if p.poll() is not None:
            break
        time.sleep(0.2)

    # Fall back to legacy fixed wait for diagnostics instead of hard-fail here.
    time.sleep(1.0)
    return p


def _stop_daemon(cfg: ChaosConfig) -> None:
    subprocess.run(_qmvir_cmd(cfg, "--data-dir", cfg.data_dir, "stop"), check=False)
    _wait_stopped(cfg, timeout_s=12.0)


def _wait_stopped(cfg: ChaosConfig, timeout_s: float = 10.0) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        pid = _load_pid(cfg.data_dir)
        if pid <= 0 and not _is_reachable(cfg):
            return True
        if pid > 0:
            try:
                os.kill(pid, 0)
            except OSError:
                if not _is_reachable(cfg):
                    return True
        time.sleep(0.2)
    return False


def _ensure_table(cfg: ChaosConfig) -> None:
    conn = _connect(cfg)
    conn.autocommit = True
    cur = conn.cursor()
    try:
        cur.execute("CREATE TABLE chaos_write (id INTEGER PRIMARY KEY, payload TEXT)")
    except Exception:
        cur.execute("DELETE FROM chaos_write WHERE 1=1")
    cur.close()
    conn.close()


def _is_reachable(cfg: ChaosConfig) -> bool:
    try:
        conn = _connect(cfg)
        conn.close()
        return True
    except Exception:
        return False


def _ensure_running(cfg: ChaosConfig) -> subprocess.Popen[str] | None:
    if _is_reachable(cfg):
        return None
    return _start_daemon(cfg)


def _writer_worker(cfg: ChaosConfig, stop: threading.Event, offset: int, stats: dict[str, int]) -> None:
    conn = _connect(cfg)
    conn.autocommit = True
    cur = conn.cursor()
    i = offset
    while not stop.is_set():
        try:
            cur.execute(
                "INSERT INTO chaos_write (id, payload) VALUES (%s, %s)",
                (i, f"msg-{i}"),
            )
            stats["ok"] += 1
        except Exception:
            stats["err"] += 1
        i += 1
    cur.close()
    conn.close()


def _run_write_load(cfg: ChaosConfig, seconds: float) -> dict[str, int]:
    stop = threading.Event()
    stats = {"ok": 0, "err": 0}
    threads: list[threading.Thread] = []
    for c in range(cfg.clients):
        t = threading.Thread(target=_writer_worker, args=(cfg, stop, c * 1_000_000, stats), daemon=True)
        t.start()
        threads.append(t)
    time.sleep(seconds)
    stop.set()
    for t in threads:
        t.join(timeout=2)
    return stats


def _load_pid(data_dir: str) -> int:
    pid_file = Path(data_dir) / "qm_daemon.pid"
    if not pid_file.exists():
        return 0
    try:
        return int(pid_file.read_text().strip())
    except Exception:
        return 0


def scenario_hard_kill(cfg: ChaosConfig) -> dict[str, Any]:
    starter = _ensure_running(cfg)
    _ensure_table(cfg)
    stop = threading.Event()
    stats = {"ok": 0, "err": 0}
    threads = [threading.Thread(target=_writer_worker, args=(cfg, stop, i * 1_000_000, stats), daemon=True) for i in range(cfg.clients)]
    for t in threads:
        t.start()

    time.sleep(max(1.0, cfg.duration_s / 2))
    pid = _load_pid(cfg.data_dir)
    kill_ok = False
    if pid > 0:
        try:
            os.kill(pid, signal.SIGKILL)
            kill_ok = True
        except OSError:
            kill_ok = False

    stop.set()
    for t in threads:
        t.join(timeout=2)

    t0 = time.perf_counter()
    daemon_proc = _start_daemon(cfg)
    recovery_s = time.perf_counter() - t0

    smoke_ok = False
    try:
        conn = _connect(cfg)
        conn.autocommit = True
        cur = conn.cursor()
        cur.execute("SELECT COUNT(*) FROM chaos_write")
        cur.fetchall()
        smoke_ok = True
        cur.close()
        conn.close()
    except Exception:
        smoke_ok = False

    # Keep daemon alive for following scenarios in this run.

    return {
        "scenario": "hard_kill_under_write_load",
        "kill_sent": kill_ok,
        "writes_ok": stats["ok"],
        "writes_err": stats["err"],
        "recovery_seconds": recovery_s,
        "recovery_smoke_ok": smoke_ok,
    }


def scenario_memory_pressure(cfg: ChaosConfig) -> dict[str, Any]:
    starter = _ensure_running(cfg)
    _ensure_table(cfg)
    pressure: list[bytes] = []
    chunk = 8 * 1024 * 1024
    target_chunks = 20  # ~160MB soft pressure

    t0 = time.perf_counter()
    try:
        for _ in range(target_chunks):
            pressure.append(b"x" * chunk)
            time.sleep(0.02)
        stats = _run_write_load(cfg, seconds=max(1, cfg.duration_s // 2))
        ok = True
    except MemoryError:
        stats = {"ok": 0, "err": 1}
        ok = False
    finally:
        pressure.clear()
    if starter is not None:
        _stop_daemon(cfg)
        try:
            starter.terminate()
        except Exception:
            pass

    elapsed = time.perf_counter() - t0
    return {
        "scenario": "memory_pressure_under_write_load",
        "ok": ok,
        "elapsed_seconds": elapsed,
        "writes_ok": stats["ok"],
        "writes_err": stats["err"],
    }


def scenario_restart_smoke(cfg: ChaosConfig) -> dict[str, Any]:
    t0 = time.perf_counter()
    _stop_daemon(cfg)
    p = _start_daemon(cfg)
    restart_s = time.perf_counter() - t0

    query_ok = False
    last_error: str | None = None
    deadline = time.time() + 30.0
    while time.time() < deadline and not query_ok:
        try:
            conn = _connect(cfg)
            conn.autocommit = True
            cur = conn.cursor()
            # QM SQL parser requires FROM in SELECT shape.
            cur.execute("SELECT COUNT(*) FROM chaos_write")
            cur.fetchall()
            query_ok = True
            cur.close()
            conn.close()
            last_error = None
        except Exception as exc:
            last_error = str(exc)
            time.sleep(0.4)

    _stop_daemon(cfg)
    try:
        p.terminate()
    except Exception:
        pass

    return {
        "scenario": "restart_recovery_smoke",
        "restart_seconds": restart_s,
        "query_ok": query_ok,
        "query_last_error": last_error,
    }


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="QMvir stress/chaos harness")
    p.add_argument("--qmvir-bin", default="/Users/gengyang/Desktop/AI/.venv/bin/qmvir")
    p.add_argument("--data-dir", default="/tmp/qm_data_chaos")
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=56543)
    p.add_argument("--user", default="admin")
    p.add_argument("--password", default="admin")
    p.add_argument("--dbname", default="qm")
    p.add_argument("--duration-s", type=int, default=20)
    p.add_argument("--clients", type=int, default=8)
    p.add_argument("--output-json", default="/Users/gengyang/Desktop/AI/QM/benchmarks/QMVIR_CHAOS_REPORT.json")
    return p.parse_args()


def main() -> None:
    args = parse_args()
    cfg = ChaosConfig(
        qmvir_bin=args.qmvir_bin,
        data_dir=args.data_dir,
        host=args.host,
        port=args.port,
        user=args.user,
        password=args.password,
        dbname=args.dbname,
        duration_s=args.duration_s,
        clients=args.clients,
        output_json=args.output_json,
    )

    report = {
        "timestamp": datetime_now_iso(),
        "config": {
            "data_dir": cfg.data_dir,
            "host": cfg.host,
            "port": cfg.port,
            "duration_s": cfg.duration_s,
            "clients": cfg.clients,
        },
        "results": [
            scenario_hard_kill(cfg),
            scenario_memory_pressure(cfg),
            scenario_restart_smoke(cfg),
        ],
    }

    Path(cfg.output_json).write_text(json.dumps(report, indent=2))
    print(f"Chaos report: {cfg.output_json}")


def datetime_now_iso() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime())


if __name__ == "__main__":
    main()
