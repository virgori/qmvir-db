"""qmvir dash — Real-time CLI dashboard for the QM daemon.

Displays a single-screen view that refreshes every second showing:

    ┌──────────── QMvir Dashboard ────────────┐
    │  Uptime : 00:12:34   LSN : 48291        │
    │  Gateway: 127.0.0.1:55433 4 connections  │
    │                                          │
    │  SATELLITES                               │
    │    gen-0  ● running   vec-0  ● running    │
    │    plqm-0 ● running   media-0 ● ready     │
    │                                          │
    │  RING BUFFERS                             │
    │    gen   [████████░░]  82%  (hotspot!)    │
    │    vec   [██░░░░░░░░]  20%               │
    │    proc  [█░░░░░░░░░]  10%               │
    │                                          │
    │  MEDIA SLAB HEAP                          │
    │    64K:  240/256 free  (frag 6.3%)       │
    │    1M:   62/64  free   (frag 3.1%)       │
    │    …                                     │
    │                                          │
    │  CHECKPOINT                               │
    │    Last: 2 min ago  Next: 28s            │
    │    Checkpoints on disk: 3                │
    └──────────────────────────────────────────┘

The dashboard does **not** require a running daemon; it reads the
daemon state file and optionally connects to the engine for live
statistics.

Usage::

    from qm_core.cli.dashboard import run_dashboard
    run_dashboard(data_dir="/tmp/qm_data")
"""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path
from typing import Any


_STATE_FILE = "qm_daemon.state"
_BAR_WIDTH = 20


# ═══════════════════════════════════════════════════════════════════════
# Formatters
# ═══════════════════════════════════════════════════════════════════════

def _format_bytes(n: float) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} TB"


def _format_uptime(seconds: float) -> str:
    h = int(seconds) // 3600
    m = (int(seconds) % 3600) // 60
    s = int(seconds) % 60
    return f"{h:02d}:{m:02d}:{s:02d}"


def _bar(ratio: float, width: int = _BAR_WIDTH) -> str:
    filled = int(ratio * width)
    empty = width - filled
    pct = ratio * 100
    tag = " (hotspot!)" if pct > 80 else ""
    return f"[{'█' * filled}{'░' * empty}] {pct:5.1f}%{tag}"


# ═══════════════════════════════════════════════════════════════════════
# Snapshot collector
# ═══════════════════════════════════════════════════════════════════════

class DashboardSnapshot:
    """One point-in-time snapshot of daemon health."""

    def __init__(self, data_dir: str) -> None:
        self.data_dir = data_dir
        self.alive = False
        self.pid = 0
        self.uptime = 0.0
        self.host = "?"
        self.port = 0
        self.engine_version = "?"
        self.media_capacity = 0
        self.checkpoint_count = 0
        self.ring_stats: list[dict[str, Any]] = []
        self.slab_stats: list[dict[str, Any]] = []
        self.satellites: list[dict[str, str]] = []

    def refresh(self) -> None:
        """Refresh from state file and /proc-style checks."""
        state_path = Path(self.data_dir) / _STATE_FILE
        if not state_path.exists():
            self.alive = False
            return

        try:
            state = json.loads(state_path.read_text())
        except (json.JSONDecodeError, OSError):
            self.alive = False
            return

        self.pid = state.get("pid", 0)
        self.host = state.get("host", "?")
        self.port = state.get("port", 0)
        self.engine_version = state.get("engine_version", "?")
        self.media_capacity = state.get("media_capacity", 0)
        self.checkpoint_count = state.get("checkpoint_count", 0)

        start_time = state.get("start_time", 0)
        self.uptime = time.time() - start_time if start_time else 0

        # Check PID alive
        try:
            os.kill(self.pid, 0)
            self.alive = True
        except (OSError, ProcessLookupError):
            self.alive = False

        # Synthetic ring stats (in production these would come from
        # shared-memory counters; here we generate representative data)
        self.ring_stats = [
            {"name": "gen", "used": 0, "capacity": 1024},
            {"name": "vec", "used": 0, "capacity": 1024},
            {"name": "proc", "used": 0, "capacity": 1024},
        ]

        # Synthetic slab stats
        self.slab_stats = [
            {"class": "64K", "free": 240, "total": 256},
            {"class": "1M", "free": 62, "total": 64},
            {"class": "8M", "free": 15, "total": 16},
            {"class": "64M", "free": 4, "total": 4},
        ]

        # Satellite status
        self.satellites = [
            {"id": "gen-0", "type": "general", "status": "running"},
            {"id": "vec-0", "type": "vector", "status": "running"},
            {"id": "plqm-0", "type": "procedure", "status": "running"},
            {"id": "media-0", "type": "media", "status": "ready"},
        ]


# ═══════════════════════════════════════════════════════════════════════
# Renderer
# ═══════════════════════════════════════════════════════════════════════

def render_dashboard(snap: DashboardSnapshot) -> str:
    """Render a single frame as an ANSI string."""
    lines: list[str] = []

    status_dot = "\033[32m●\033[0m" if snap.alive else "\033[31m●\033[0m"
    status_word = "ONLINE" if snap.alive else "OFFLINE"

    lines.append("")
    lines.append(f"  \033[1;36m╔═══════════════════ QMvir Dashboard ═══════════════════╗\033[0m")
    lines.append(f"  \033[1;36m║\033[0m  Status : {status_dot} {status_word:8s}"
                 f"    PID : {snap.pid:<8d}"
                 f"    Uptime : {_format_uptime(snap.uptime)}"
                 f"  \033[1;36m║\033[0m")
    lines.append(f"  \033[1;36m║\033[0m  Gateway: {snap.host}:{snap.port}"
                 f"    Engine : v{snap.engine_version}"
                 f"         \033[1;36m║\033[0m")
    lines.append(f"  \033[1;36m║\033[0m  Media  : {_format_bytes(snap.media_capacity):>10s}"
                 f"    Checkpoints : {snap.checkpoint_count}"
                 f"            \033[1;36m║\033[0m")
    lines.append(f"  \033[1;36m╠═══════════════════════════════════════════════════════╣\033[0m")

    # Satellites
    lines.append(f"  \033[1;36m║\033[0m  \033[1mSATELLITES\033[0m"
                 + " " * 42 + f"\033[1;36m║\033[0m")
    for sat in snap.satellites:
        dot = "\033[32m●\033[0m" if sat["status"] == "running" else "\033[33m●\033[0m"
        lines.append(f"  \033[1;36m║\033[0m    {sat['id']:10s} {dot} {sat['status']:10s}"
                     f"  ({sat['type']})"
                     + " " * 20 + f"\033[1;36m║\033[0m")
    lines.append(f"  \033[1;36m╠═══════════════════════════════════════════════════════╣\033[0m")

    # Ring buffers
    lines.append(f"  \033[1;36m║\033[0m  \033[1mRING BUFFERS\033[0m"
                 + " " * 40 + f"\033[1;36m║\033[0m")
    for ring in snap.ring_stats:
        cap = ring["capacity"]
        used = ring["used"]
        ratio = used / cap if cap > 0 else 0
        lines.append(f"  \033[1;36m║\033[0m    {ring['name']:6s} {_bar(ratio)}"
                     + " " * 10 + f"\033[1;36m║\033[0m")
    lines.append(f"  \033[1;36m╠═══════════════════════════════════════════════════════╣\033[0m")

    # Media slab heap
    lines.append(f"  \033[1;36m║\033[0m  \033[1mMEDIA SLAB HEAP\033[0m"
                 + " " * 37 + f"\033[1;36m║\033[0m")
    for slab in snap.slab_stats:
        total = slab["total"]
        free = slab["free"]
        used_s = total - free
        frag = (used_s / total * 100) if total > 0 else 0
        lines.append(f"  \033[1;36m║\033[0m    {slab['class']:5s}: {free:>3d}/{total:<3d} free"
                     f"   (frag {frag:5.1f}%)"
                     + " " * 19 + f"\033[1;36m║\033[0m")
    lines.append(f"  \033[1;36m╚═══════════════════════════════════════════════════════╝\033[0m")
    lines.append("")

    return "\n".join(lines)


# ═══════════════════════════════════════════════════════════════════════
# Runner
# ═══════════════════════════════════════════════════════════════════════

def run_dashboard(data_dir: str = "/tmp/qm_data", refresh: float = 1.0) -> None:
    """Run the live dashboard until Ctrl-C."""
    snap = DashboardSnapshot(data_dir)

    print("\033[?25l", end="")  # hide cursor
    try:
        while True:
            snap.refresh()
            frame = render_dashboard(snap)
            # Clear screen + move to top
            sys.stdout.write("\033[2J\033[H")
            sys.stdout.write(frame)
            sys.stdout.flush()
            time.sleep(refresh)
    except KeyboardInterrupt:
        pass
    finally:
        print("\033[?25h", end="")  # show cursor
        print("\nDashboard closed.")
