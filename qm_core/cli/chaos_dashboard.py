"""qmvir dash-chaos - show chaos run status from JSON report."""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any


def _render(report: dict[str, Any]) -> str:
    lines: list[str] = []
    lines.append("QMvir Chaos Dashboard")
    lines.append(f"  Timestamp: {report.get('timestamp', 'N/A')}")
    cfg = report.get("config", {})
    lines.append(f"  Target: {cfg.get('host', '?')}:{cfg.get('port', '?')}  data_dir={cfg.get('data_dir', '?')}")
    lines.append("")

    for item in report.get("results", []):
        name = item.get("scenario", "unknown")
        lines.append(f"- {name}")
        for k, v in item.items():
            if k == "scenario":
                continue
            lines.append(f"    {k}: {v}")
        lines.append("")

    return "\n".join(lines)


def run_dash_chaos(report_file: str, refresh: float = 1.0, json_mode: bool = False) -> int:
    p = Path(report_file)
    if not p.exists():
        print(f"Chaos report not found: {p}")
        return 1

    def _load() -> dict[str, Any]:
        return json.loads(p.read_text())

    if refresh <= 0:
        report = _load()
        if json_mode:
            print(json.dumps(report, indent=2))
        else:
            print(_render(report))
        return 0

    try:
        last_mtime = 0.0
        while True:
            try:
                mtime = p.stat().st_mtime
                if mtime != last_mtime:
                    report = _load()
                    last_mtime = mtime
                    if json_mode:
                        print(json.dumps(report, indent=2))
                    else:
                        print("\033[2J\033[H", end="")
                        print(_render(report))
            except Exception as exc:
                print(f"error reading chaos report: {exc}")
            time.sleep(refresh)
    except KeyboardInterrupt:
        return 0
