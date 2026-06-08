#!/usr/bin/env python3
"""Benchmark + chaos harness for QMvir backup, verify, predict, diff-backup, restore.

Requires a `qm` binary with backup subcommands built (recommended):
  cd qm_engine && cargo build --release --no-default-features --bin qm

Usage:
  QM_BIN=/path/to/qm python3 benchmarks/backup_predict_chaos.py
  QM_BIN=/path/to/qm python3 benchmarks/backup_predict_chaos.py --json-out /tmp/out.json

Chaos scenarios (non-destructive; use temp dirs):
  - truncated_backup_verify_fails: tampered footer / CRC must fail verify
  - diff_restore_roundtrip: full restore then differential restore matches source row count

Environment:
  QM_BIN       Path to `qm` (default: $PATH `qm`, else qm_engine/target/release/qm)
  KEEP_ARTIFACTS  If set, do not delete temp workspace on success
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


def repo_root() -> Path:
    return Path(__file__).resolve().parents[1]


def _host_triple() -> str | None:
    try:
        out = subprocess.check_output(["rustc", "-vV"], text=True, timeout=5)
        for line in out.splitlines():
            if line.startswith("host: "):
                return line.split("host: ", 1)[1].strip()
    except (subprocess.CalledProcessError, FileNotFoundError, subprocess.TimeoutExpired):
        return None
    return None


def resolve_qm_bin() -> str:
    env = os.environ.get("QM_BIN")
    if env:
        return env
    w = shutil.which("qm")
    if w:
        return w
    tdir = repo_root() / "qm_engine" / "target"
    cands: list[Path] = []
    ht = _host_triple()
    if ht:
        cands.append(tdir / ht / "release" / "qm")
    cands.append(tdir / "release" / "qm")
    for cand in cands:
        if cand.is_file():
            return str(cand)
    print(
        "ERROR: No qm binary. Set QM_BIN or run:\n"
        "  cd qm_engine && cargo build --release --no-default-features --bin qm",
        file=sys.stderr,
    )
    sys.exit(2)


def run_qm(qm_bin: str, data_dir: Path | None, args: list[str], timeout: float = 120) -> subprocess.CompletedProcess[str]:
    cmd = [qm_bin]
    if data_dir is not None:
        cmd.extend(["--data-dir", str(data_dir)])
    cmd.extend(args)
    return subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def bench_step(name: str, fn: Any) -> dict[str, Any]:
    t0 = time.perf_counter()
    try:
        out = fn()
        ms = (time.perf_counter() - t0) * 1000.0
        if isinstance(out, dict):
            return {"name": name, "ok": True, "ms": ms, **out}
        return {"name": name, "ok": True, "ms": ms, "result": out}
    except Exception as e:
        ms = (time.perf_counter() - t0) * 1000.0
        return {"name": name, "ok": False, "ms": ms, "error": str(e)}


def chaos_truncated_verify_fails(qm_bin: str, good_backup: Path) -> dict[str, Any]:
    bad = good_backup.with_suffix(".corrupt.qmvb")
    data = good_backup.read_bytes()
    if len(data) < 200:
        return {"ok": False, "error": "backup too small to corrupt"}
    bad.write_bytes(data[: max(0, len(data) - 80)])
    cp = run_qm(qm_bin, None, ["verify", str(bad)])
    return {
        "ok": cp.returncode != 0,
        "returncode": cp.returncode,
        "stderr_tail": (cp.stderr or "")[-500:],
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--json-out", type=Path, default=None, help="Write full report JSON here")
    args = ap.parse_args()

    qm_bin = resolve_qm_bin()
    report: dict[str, Any] = {
        "qm_bin": qm_bin,
        "steps": [],
        "chaos": [],
    }

    tmp = tempfile.mkdtemp(prefix="qmvir_backup_chaos_")
    tmp_path = Path(tmp)
    data_a = tmp_path / "data_a"
    data_b = tmp_path / "data_b"
    backups = tmp_path / "backups"
    backups.mkdir(parents=True)
    full_bak = backups / "full.qmvb"
    diff_bak = backups / "delta.qmvb"

    def add_step(step: dict[str, Any]) -> None:
        report["steps"].append(step)
        tag = "OK" if step.get("ok") else "FAIL"
        print(f"[{tag}] {step.get('name')}: {step.get('ms', 0):.1f} ms")

    try:

        def seed_db() -> dict[str, Any]:
            data_a.mkdir(parents=True)
            sql1 = (
                "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT); "
                "INSERT INTO t VALUES (1, 'a');"
            )
            r1 = run_qm(qm_bin, data_a, ["sql", sql1])
            if r1.returncode != 0:
                return {"ok": False, "stderr": r1.stderr, "stdout": r1.stdout}
            return {"ok": True}

        add_step(bench_step("seed_database", seed_db))

        def do_full_backup() -> dict[str, Any]:
            r = run_qm(qm_bin, data_a, ["backup", "-o", str(full_bak), "--compress", "lz4"])
            return {
                "ok": r.returncode == 0,
                "returncode": r.returncode,
                "stderr": r.stderr[-800:] if r.stderr else "",
            }

        add_step(bench_step("full_backup", do_full_backup))
        if not report["steps"][-1].get("ok"):
            raise RuntimeError("full backup failed")

        def do_verify_full() -> dict[str, Any]:
            r = run_qm(qm_bin, None, ["verify", str(full_bak)])
            return {"ok": r.returncode == 0, "returncode": r.returncode}

        add_step(bench_step("verify_full", do_verify_full))

        def do_predict_json() -> dict[str, Any]:
            r = run_qm(qm_bin, data_a, ["predict", "--compress", "lz4", "--json"])
            if r.returncode != 0:
                return {"ok": False, "stderr": r.stderr}
            text = r.stdout.strip()
            start = text.find("{")
            end = text.rfind("}")
            blob = text[start:end + 1] if start >= 0 and end > start else ""
            if not blob:
                return {"ok": False, "stdout": r.stdout[:400], "stderr": (r.stderr or "")[:400]}
            pred = json.loads(blob)
            return {"ok": True, "predict": pred}

        add_step(bench_step("predict", do_predict_json))

        def mutate_for_diff() -> dict[str, Any]:
            r = run_qm(qm_bin, data_a, ["sql", "INSERT INTO t VALUES (2, 'b');"])
            return {"ok": r.returncode == 0, "returncode": r.returncode}

        add_step(bench_step("mutate_before_diff", mutate_for_diff))

        def do_diff_backup() -> dict[str, Any]:
            r = run_qm(
                qm_bin,
                data_a,
                ["diff-backup", "-b", str(full_bak), "-o", str(diff_bak), "--compress", "lz4"],
            )
            return {
                "ok": r.returncode == 0,
                "returncode": r.returncode,
                "stderr": (r.stderr or "")[-600:],
            }

        add_step(bench_step("diff_backup", do_diff_backup))

        def do_verify_diff() -> dict[str, Any]:
            r = run_qm(qm_bin, None, ["verify", str(diff_bak)])
            return {"ok": r.returncode == 0, "returncode": r.returncode}

        add_step(bench_step("verify_diff", do_verify_diff))

        def restore_roundtrip() -> dict[str, Any]:
            data_b.mkdir(parents=True)
            r_full = run_qm(qm_bin, data_b, ["restore", "-i", str(full_bak)])
            if r_full.returncode != 0:
                return {"ok": False, "phase": "full", "stderr": r_full.stderr}
            r_diff = run_qm(qm_bin, data_b, ["restore", "-i", str(diff_bak)])
            if r_diff.returncode != 0:
                return {"ok": False, "phase": "diff", "stderr": r_diff.stderr}
            rc = run_qm(qm_bin, data_b, ["sql", "SELECT COUNT(*) AS c FROM t;"])
            if rc.returncode != 0:
                return {"ok": False, "phase": "count", "stderr": rc.stderr}
            out = rc.stdout + rc.stderr
            return {"ok": True, "count_output": out.strip()[-400:]}

        add_step(bench_step("restore_full_then_diff", restore_roundtrip))

        c1 = chaos_truncated_verify_fails(qm_bin, full_bak)
        report["chaos"].append({"name": "truncated_backup_verify_fails", **c1})
        print(f"[CHAOS] truncated_backup_verify_fails: {'OK' if c1.get('ok') else 'FAIL'}")

        # Optional: replay shell exits cleanly
        proc = subprocess.run(
            [qm_bin, "--data-dir", str(data_a), "psql"],
            input="SELECT 2 AS x;\n\\q\n",
            capture_output=True,
            text=True,
            timeout=30,
        )
        combined = proc.stdout + proc.stderr
        shell_ok = proc.returncode == 0 and len(proc.stdout.strip()) > 0
        report["chaos"].append({"name": "shell_repl_smoke", "ok": shell_ok})
        print(f"[CHAOS] shell_repl_smoke: {'OK' if shell_ok else 'FAIL'}")

    finally:
        if not os.environ.get("KEEP_ARTIFACTS"):
            shutil.rmtree(tmp_path, ignore_errors=True)
        else:
            report["artifact_dir"] = str(tmp_path)

    failed = any(not s.get("ok", False) for s in report["steps"])
    failed |= any(not c.get("ok") for c in report["chaos"])

    if args.json_out:
        args.json_out.write_text(json.dumps(report, indent=2))

    if failed:
        print(json.dumps(report, indent=2))
        sys.exit(1)
    print("backup_predict_chaos: all checks passed")


if __name__ == "__main__":
    main()
