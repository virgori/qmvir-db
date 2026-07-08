#!/usr/bin/env python3
"""QMvir Daemon — The heart of the QM Database System.

Bootstraps, manages, and monitors the entire Hub-Satellite database.

Lifecycle:
    1. Auth       — Initialize RBAC User Catalog.
    2. Bootstrap  — Create shared memory regions (Ring Buffers ×3 + Media Heap).
    3. Recover    — Load latest checkpoint, replay WAL tail if needed.
    4. Spawn      — Start satellite processes (General, Vector, Procedure, Media).
    5. Gateway    — Open PostgreSQL wire-protocol listener.
    6. Monitor    — Heartbeat health checks + auto-restart dead satellites.
    7. Checkpoint — Periodic RAM→SSD state persistence.
    8. Shutdown   — Graceful drain, final checkpoint, cleanup.

Usage:
    qmvir start                             # Start QM daemon
    qmvir start --port 55433                # Custom port
    qmvir start --data-dir /ssd/qm_data     # Custom data dir
    qmvir sql -u admin -p secret            # Interactive SQL shell
    qmvir dash                              # Real-time dashboard
    qmvir bench                             # Run benchmarks
    qmvir status                            # Show running status
    qmvir stop                              # Graceful shutdown
    qmvir version                           # Print version info
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import signal
import socket
import sys
import threading
import time
from logging.handlers import RotatingFileHandler
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any, Optional

# ── QM imports ──────────────────────────────────────────────────────

_RUNTIME_IMPORT_ERROR: Exception | None = None

try:
    from qm_core.hub_engine import QMHubEngine
    from qm_core.hub.dispatcher import HubDispatcher, DispatcherConfig
    from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig
    from qm_core.checkpoint import CheckpointManager, CheckpointConfig
    from qm_core.satellite.worker import SatelliteCluster, WorkerSpec
    from qm_core.auth import UserCatalog, AuthSession, AuthError, Role, check_permission
    from gateway.api_postgres.server import QMPostgresServer
    from gateway.api_postgres.hub_executor import make_hub_executor
except Exception as exc:  # pragma: no cover - allows parser/help/check to run without full deps
    _RUNTIME_IMPORT_ERROR = exc

    class _QMHubEngineStub:
        VERSION = "unavailable"

    class _RoleStub(Enum):
        ADMIN = 3

    class _AuthErrorStub(Exception):
        pass

    class _UserCatalogStub:
        def list_users(self) -> list[Any]:
            return []

    QMHubEngine = _QMHubEngineStub  # type: ignore[assignment]
    HubDispatcher = DispatcherConfig = Any  # type: ignore[assignment]
    MediaSlabAllocator = MediaAllocatorConfig = Any  # type: ignore[assignment]
    CheckpointManager = CheckpointConfig = Any  # type: ignore[assignment]
    SatelliteCluster = WorkerSpec = Any  # type: ignore[assignment]
    UserCatalog = _UserCatalogStub  # type: ignore[assignment]
    AuthSession = Any  # type: ignore[assignment]
    AuthError = _AuthErrorStub  # type: ignore[assignment]
    Role = _RoleStub  # type: ignore[assignment]
    check_permission = lambda *_args, **_kwargs: True  # type: ignore[assignment]
    QMPostgresServer = Any  # type: ignore[assignment]
    make_hub_executor = None  # type: ignore[assignment]

try:
    from qm_engine import PostgresGateway as RustPostgresGateway
except Exception:
    RustPostgresGateway = None

logger = logging.getLogger("qm.daemon")

__version__ = "6.2.7"


# ── i18n ────────────────────────────────────────────────────────────

_I18N = {
    "en": {
        "daemon_not_running": "QM daemon is not running",
        "sending_sigterm": "Sending SIGTERM to QM daemon (pid={pid})",
        "process_not_found": "Process not found — cleaning up PID file",
        "json_written": "JSON report written to {path}",
        "bench_done": "Benchmark completed: {n} result(s)",
        "starting_server": "Starting QM server...",
        "server_ready": "QM server ready on {host}:{port}",
        "stopping_server": "Stopping QM server...",
    },
    "vi": {
        "daemon_not_running": "QM daemon chưa chạy",
        "sending_sigterm": "Gửi SIGTERM đến QM daemon (pid={pid})",
        "process_not_found": "Không tìm thấy tiến trình — dọn dẹp file PID",
        "json_written": "Báo cáo JSON đã ghi vào {path}",
        "bench_done": "Đo kiểm hoàn tất: {n} kết quả",
        "starting_server": "Đang khởi động QM server...",
        "server_ready": "QM server sẵn sàng tại {host}:{port}",
        "stopping_server": "Đang dừng QM server...",
    },
    "zh": {
        "daemon_not_running": "QM 守护进程未运行",
        "sending_sigterm": "正在向 QM 守护进程发送 SIGTERM（pid={pid}）",
        "process_not_found": "未找到进程 — 清理 PID 文件",
        "json_written": "JSON 报告已写入 {path}",
        "bench_done": "基准测试完成：{n} 个结果",
        "starting_server": "正在启动 QM 服务器...",
        "server_ready": "QM 服务器已就绪 {host}:{port}",
        "stopping_server": "正在停止 QM 服务器...",
    },
    "zht": {
        "daemon_not_running": "QM 守護程序未運行",
        "sending_sigterm": "正在向 QM 守護程序發送 SIGTERM（pid={pid}）",
        "process_not_found": "未找到程序 — 清理 PID 文件",
        "json_written": "JSON 報告已寫入 {path}",
        "bench_done": "基準測試完成：{n} 個結果",
        "starting_server": "正在啟動 QM 伺服器...",
        "server_ready": "QM 伺服器已就緒 {host}:{port}",
        "stopping_server": "正在停止 QM 伺服器...",
    },
}


def _msg(lang: str, key: str, **kwargs) -> str:
    """Get a translated message."""
    msgs = _I18N.get(lang, _I18N["en"])
    template = msgs.get(key, _I18N["en"].get(key, key))
    return template.format(**kwargs) if kwargs else template


# ── Configuration ───────────────────────────────────────────────────

@dataclass
class QMDaemonConfig:
    """Full daemon configuration."""
    data_dir: str = "/tmp/qm_data"
    host: str = "127.0.0.1"
    port: int = 55433
    unix_socket_path: Optional[str] = None

    # Ring buffer geometry
    ring_slot_count: int = 1024
    ring_slot_data_size: int = 65536  # 64 KB

    # Media slab allocator
    media_size_classes: list[int] = field(default_factory=lambda: [
        64 * 1024,         # 64 KB — thumbnails
        1 * 1024 * 1024,   # 1 MB  — photos
        8 * 1024 * 1024,   # 8 MB  — video segments
        64 * 1024 * 1024,  # 64 MB — raw frames
    ])
    media_slab_counts: list[int] = field(default_factory=lambda: [256, 64, 16, 4])

    # Checkpoint
    checkpoint_interval: float = 30.0
    checkpoint_lsn_threshold: int = 1000
    checkpoint_max_keep: int = 5

    # Satellites
    vector_dim: int = 128
    satellite_poll_us: int = 100

    # Monitor
    heartbeat_interval: float = 5.0       # seconds between health checks
    max_restart_attempts: int = 3          # per satellite before giving up

    # Process isolation: True = spawn OS processes, False = in-process threads
    process_isolation: bool = False


# ── Daemon State ────────────────────────────────────────────────────

_PID_FILE = "qm_daemon.pid"
_STATE_FILE = "qm_daemon.state"


class QMDaemon:
    """Main daemon orchestrating the QM database system.

    Manages:
        - QMHubEngine (control plane)
        - MediaSlabAllocator (large blob zero-copy)
        - SatelliteCluster (worker processes when process_isolation=True)
        - CheckpointManager (periodic RAM→SSD)
        - QMPostgresServer (PostgreSQL wire-protocol gateway)
        - Health monitor (heartbeat + auto-restart)
    """

    def __init__(self, config: QMDaemonConfig | None = None) -> None:
        if _RUNTIME_IMPORT_ERROR is not None:
            raise RuntimeError(
                f"QM runtime dependencies are missing: {_RUNTIME_IMPORT_ERROR}"
            )
        self._config = config or QMDaemonConfig()
        self._data_dir = Path(self._config.data_dir)
        self._data_dir.mkdir(parents=True, exist_ok=True)

        # Components (initialized in start())
        self._engine: Optional[QMHubEngine] = None
        self._media_alloc: Optional[MediaSlabAllocator] = None
        self._checkpoint_mgr: Optional[CheckpointManager] = None
        self._cluster: Optional[SatelliteCluster] = None
        self._server: Optional[Any] = None
        self._server_backend: str = "python"
        self._monitor_thread: Optional[threading.Thread] = None
        self._stop_event = threading.Event()
        self._restart_counts: dict[str, int] = {}
        self._start_time: float = 0.0
        self._ready: bool = False
        self._ready_error: str | None = None
        self._user_catalog = UserCatalog()

    # ── Bootstrap ───────────────────────────────────────────────────

    def _bootstrap(self) -> None:
        """Phase 1: Initialize all shared memory regions and engine."""
        logger.info("=== QM Daemon Bootstrap ===")
        logger.info("Data directory: %s", self._data_dir)
        logger.info("[0/4] RBAC User Catalog initialized (%d users)",
                     len(self._user_catalog.list_users()))

        # 1. Hub Engine (creates ring buffers + in-process satellites)
        logger.info("[1/4] Initializing Hub Engine...")
        self._engine = QMHubEngine(
            data_dir=str(self._data_dir),
            wal_enabled=True,
        )
        logger.info("  Hub Engine v%s ready", QMHubEngine.VERSION)

        # 2. Media Slab Allocator
        logger.info("[2/4] Initializing Media Heap...")
        heap_dir = str(self._data_dir / "media_heap")
        self._media_alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=heap_dir,
            size_classes=self._config.media_size_classes,
            slab_counts=self._config.media_slab_counts,
        ))
        logger.info("  Media Heap: %d classes, %s total capacity",
                     len(self._config.media_size_classes),
                     _format_bytes(self._media_alloc.total_capacity_bytes()))

        # 3. Checkpoint Manager
        logger.info("[3/4] Initializing Checkpoint Manager...")
        ckpt_dir = str(self._data_dir / "checkpoints")
        self._checkpoint_mgr = CheckpointManager(
            CheckpointConfig(
                checkpoint_dir=ckpt_dir,
                interval_seconds=self._config.checkpoint_interval,
                lsn_threshold=self._config.checkpoint_lsn_threshold,
                max_checkpoints=self._config.checkpoint_max_keep,
                enabled=True,
            ),
            state_provider=self._get_checkpoint_state,
        )

        # 4. Recovery — scan .qmck files for latest checkpoint
        logger.info("[4/4] Scanning for .qmck recovery files...")
        recovered = self._checkpoint_mgr.recover_state()
        if recovered:
            logger.info("  Recovered from checkpoint: LSN=%d epoch=%d mode=%s",
                         recovered.get("lsn", 0), recovered.get("epoch", 0),
                         recovered.get("mode", "full"))
        else:
            logger.info("  No .qmck checkpoint found — fresh start")

    def _get_checkpoint_state(self) -> dict[str, Any]:
        """Provide current state for checkpointing."""
        if self._engine is None:
            return {}

        hub = self._engine._dispatcher.hub
        seq = hub._sequencer

        table_meta = {}
        for name, meta in self._engine._tables.items():
            table_meta[name] = {
                "schema": meta.schema,
                "primary_key": meta.primary_key,
                "vector_dim": meta.vector_dim,
                "row_count": meta.row_count,
            }

        return {
            "lsn": seq.current_lsn,
            "epoch": seq.epoch,
            "table_meta": table_meta,
            "merkle_root": hub._auditor.root(),
        }

    # ── Spawn ───────────────────────────────────────────────────────

    def _spawn_satellites(self) -> None:
        """Phase 2: Start satellite workers (process isolation mode)."""
        if not self._config.process_isolation:
            logger.info("Process isolation disabled — using in-process satellites")
            return

        logger.info("Spawning satellite worker processes...")
        self._cluster = SatelliteCluster()
        ring_dir = str(self._data_dir / "rings")

        specs = [
            ("gen-0", "general", f"{ring_dir}/gen_ring.shm"),
            ("vec-0", "vector", f"{ring_dir}/vec_ring.shm"),
            ("plqm-0", "procedure", f"{ring_dir}/proc_ring.shm"),
            ("media-0", "media", f"{ring_dir}/media_ring.shm"),
        ]
        for sid, stype, rpath in specs:
            self._cluster.add(
                sid, stype, ring_path=rpath,
                slot_count=self._config.ring_slot_count,
                slot_data_size=self._config.ring_slot_data_size,
                dim=self._config.vector_dim,
                poll_interval_us=self._config.satellite_poll_us,
            )
        self._cluster.start_all()
        logger.info("  %d satellite workers spawned (gen, vec, proc, media)",
                     len(specs))

    # ── Gateway ─────────────────────────────────────────────────────

    async def _start_gateway(self) -> None:
        """Phase 3: Open PostgreSQL wire-protocol gateway."""
        executor = make_hub_executor(
            self._engine,
            media_allocator=self._media_alloc,
            checkpoint_manager=self._checkpoint_mgr,
        )

        def rust_executor(sql: str) -> tuple[list[str], list[int], list[list[Optional[str]]]]:
            cols, oids, rows = executor(sql)
            out_rows: list[list[Optional[str]]] = []
            for row in rows:
                out_row: list[Optional[str]] = []
                for v in row:
                    out_row.append(None if v is None else str(v))
                out_rows.append(out_row)
            return cols, oids, out_rows

        rust_only = os.environ.get("QM_RUST_ONLY", "1") == "1"

        # Prefer Rust gateway for hot wire-protocol path.
        if RustPostgresGateway is not None:
            try:
                gw = RustPostgresGateway(
                    host=self._config.host,
                    port=self._config.port,
                    max_connections=1000,
                    unix_socket_path=self._config.unix_socket_path,
                )
                # Native mode removes Python SQL execution from the hot path.
                native_dir = str(self._data_dir / "native_engine")
                gw.start_native_persist(native_dir)
                self._server = gw
                self._server_backend = "rust"
                if self._config.unix_socket_path:
                    logger.info(
                        "PostgreSQL gateway (rust) listening on %s:%d + unix://%s",
                        self._config.host,
                        self._config.port,
                        self._config.unix_socket_path,
                    )
                else:
                    logger.info("PostgreSQL gateway (rust) listening on %s:%d",
                                self._config.host, self._config.port)
                return
            except Exception:
                if rust_only:
                    logger.exception("Rust gateway start failed in QM_RUST_ONLY=1 mode")
                    raise
                logger.exception("Rust gateway start failed, falling back to python gateway")

        if rust_only:
            raise RuntimeError(
                "QM_RUST_ONLY=1 but RustPostgresGateway is unavailable; refusing Python fallback"
            )

        self._server = QMPostgresServer(
            executor=executor,
            host=self._config.host,
            port=self._config.port,
        )
        await self._server.start()
        self._server_backend = "python"
        logger.info("PostgreSQL gateway (python) listening on %s:%d",
                     self._config.host, self._config.port)

    # ── Health Monitor ──────────────────────────────────────────────

    def _start_monitor(self) -> None:
        """Phase 4: Start heartbeat monitor thread."""
        self._monitor_thread = threading.Thread(
            target=self._monitor_loop,
            name="qm-monitor",
            daemon=True,
        )
        self._monitor_thread.start()
        logger.info("Health monitor started (interval=%.1fs)",
                     self._config.heartbeat_interval)

    def _monitor_loop(self) -> None:
        """Periodic health check and auto-restart for satellites."""
        while not self._stop_event.is_set():
            self._stop_event.wait(timeout=self._config.heartbeat_interval)
            if self._stop_event.is_set():
                break

            # Check in-process satellites
            if self._engine:
                for name, sat in [
                    ("gen-0", self._engine._gen_sat),
                    ("vec-0", self._engine._vec_sat),
                    ("plqm-0", self._engine._proc_sat),
                ]:
                    if not sat._running:
                        count = self._restart_counts.get(name, 0)
                        if count < self._config.max_restart_attempts:
                            logger.warning(
                                "Satellite %s not running — restarting (%d/%d)",
                                name, count + 1, self._config.max_restart_attempts,
                            )
                            sat.start()
                            self._restart_counts[name] = count + 1
                        else:
                            logger.error(
                                "Satellite %s exceeded restart limit — giving up",
                                name,
                            )

            # Check OS-process workers
            if self._cluster:
                for worker in self._cluster._workers.values():
                    if not worker.is_alive():
                        name = worker.spec.satellite_id
                        count = self._restart_counts.get(name, 0)
                        if count < self._config.max_restart_attempts:
                            logger.warning(
                                "Worker %s (pid=%s) dead — restarting (%d/%d)",
                                name, worker.pid,
                                count + 1, self._config.max_restart_attempts,
                            )
                            worker.restart()
                            self._restart_counts[name] = count + 1

    # ── Main Start ──────────────────────────────────────────────────

    async def start(self) -> None:
        """Start the QM daemon — the full lifecycle."""
        self._start_time = time.time()
        self._stop_event.clear()
        self._ready = False
        self._ready_error = None

        # Phase 1: Bootstrap
        self._bootstrap()

        # Phase 2: Spawn satellite workers
        self._spawn_satellites()

        # Phase 3: Start auto-checkpoint
        self._checkpoint_mgr.start()

        # Phase 4: Start health monitor
        self._start_monitor()

        # Phase 5: Open gateway
        await self._start_gateway()

        # Readiness gate: do not report ready until gateway accepts connections.
        self._wait_gateway_ready(timeout_s=15.0)
        self._ready = True

        # Write PID file
        pid_path = self._data_dir / _PID_FILE
        pid_path.write_text(str(os.getpid()))

        # Write state file
        self._write_state()

        elapsed = time.time() - self._start_time
        logger.info("=== QM Daemon Ready (%.2fs) ===", elapsed)
        logger.info("  Engine:     v%s", QMHubEngine.VERSION)
        logger.info("  Gateway:    %s:%d (PostgreSQL wire protocol)",
                     self._config.host, self._config.port)
        logger.info("  Media Heap: %s",
                     _format_bytes(self._media_alloc.total_capacity_bytes()))
        logger.info("  Checkpoint: every %.0fs or %d mutations",
                     self._config.checkpoint_interval,
                     self._config.checkpoint_lsn_threshold)
        logger.info("  PID:        %d", os.getpid())

    async def stop(self) -> None:
        """Graceful shutdown."""
        logger.info("=== QM Daemon Shutting Down ===")
        self._stop_event.set()
        self._ready = False

        # 1. Close gateway
        if self._server:
            if self._server_backend == "rust":
                self._server.stop()
            else:
                await self._server.stop()
            logger.info("  Gateway closed")

        # 2. Final checkpoint
        if self._checkpoint_mgr:
            try:
                self._checkpoint_mgr.force_checkpoint("full")
                logger.info("  Final checkpoint written")
            except Exception:
                logger.exception("  Final checkpoint failed")
            self._checkpoint_mgr.stop()

        # 3. Stop satellites
        if self._cluster:
            self._cluster.stop_all()
            logger.info("  Satellite workers stopped")

        # 4. Close engine
        if self._engine:
            self._engine.close()
            logger.info("  Engine closed")

        # 5. Close media allocator
        if self._media_alloc:
            self._media_alloc.close()
            logger.info("  Media heap closed")

        # 6. Cleanup PID file
        pid_path = self._data_dir / _PID_FILE
        if pid_path.exists():
            pid_path.unlink()

        logger.info("=== QM Daemon Stopped ===")

    def _write_state(self) -> None:
        """Write daemon state to file for status queries."""
        state = {
            "pid": os.getpid(),
            "start_time": self._start_time,
            "data_dir": str(self._data_dir),
            "host": self._config.host,
            "port": self._config.port,
            "unix_socket_path": self._config.unix_socket_path,
            "engine_version": QMHubEngine.VERSION,
            "media_capacity": self._media_alloc.total_capacity_bytes()
                if self._media_alloc else 0,
            "checkpoint_count": self._checkpoint_mgr.checkpoint_count
                if self._checkpoint_mgr else 0,
            "ready": self._ready,
            "ready_error": self._ready_error,
        }
        state_path = self._data_dir / _STATE_FILE
        state_path.write_text(json.dumps(state, indent=2))

    def _wait_gateway_ready(self, timeout_s: float = 15.0) -> None:
        """Block until gateway is actually reachable on configured endpoint."""
        deadline = time.time() + timeout_s
        last_err: Exception | None = None
        while time.time() < deadline:
            try:
                if self._config.unix_socket_path:
                    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    sock.settimeout(0.5)
                    sock.connect(self._config.unix_socket_path)
                    sock.close()
                    return
                sock = socket.create_connection((self._config.host, self._config.port), timeout=0.5)
                sock.close()
                return
            except Exception as exc:  # pragma: no cover - timing-sensitive
                last_err = exc
                time.sleep(0.1)
        self._ready_error = str(last_err) if last_err else "gateway readiness timeout"
        raise RuntimeError(f"Gateway not ready within {timeout_s:.1f}s: {self._ready_error}")

    # ── Status ──────────────────────────────────────────────────────

    def status(self) -> dict[str, Any]:
        """Return daemon status."""
        uptime = time.time() - self._start_time if self._start_time else 0
        return {
            "running": not self._stop_event.is_set(),
            "pid": os.getpid(),
            "uptime_seconds": uptime,
            "engine_version": QMHubEngine.VERSION,
            "gateway": {
                "host": self._config.host,
                "port": self._config.port,
                "backend": self._server_backend,
                "connections": self._server.connection_count if self._server else 0,
                "is_listening": self._server.is_running if self._server else False,
            },
            "ready": self._ready,
            "ready_error": self._ready_error,
            "media_heap": self._media_alloc.stats() if self._media_alloc else [],
            "checkpoint": self._checkpoint_mgr.stats() if self._checkpoint_mgr else {},
            "tables": list(self._engine._tables.keys()) if self._engine else [],
            "users": len(self._user_catalog.list_users()),
            "auth_roles": [r.name for r in Role],
        }

    @property
    def user_catalog(self) -> UserCatalog:
        return self._user_catalog


# ── Utilities ───────────────────────────────────────────────────────

def _format_bytes(n: int) -> str:
    """Human-readable byte count."""
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} TB"


class _PrefixFilter(logging.Filter):
    """Keep log records whose logger name matches any configured prefix."""

    def __init__(self, prefixes: tuple[str, ...]) -> None:
        super().__init__()
        self._prefixes = prefixes

    def filter(self, record: logging.LogRecord) -> bool:
        name = record.name
        return any(name.startswith(p) for p in self._prefixes)


def _setup_layered_file_logging(data_dir: str, level_name: str) -> None:
    """Write daemon logs into per-layer rotating files under <data-dir>/logs."""
    logs_dir = Path(data_dir) / "logs"
    logs_dir.mkdir(parents=True, exist_ok=True)

    level = getattr(logging, level_name.upper(), logging.INFO)
    formatter = logging.Formatter(
        fmt="%(asctime)s [%(name)s] %(levelname)s %(message)s",
        datefmt="%H:%M:%S",
    )

    root = logging.getLogger()
    existing_paths = {
        getattr(h, "baseFilename", "") for h in root.handlers if hasattr(h, "baseFilename")
    }

    def _add_handler(file_name: str, prefixes: tuple[str, ...]) -> None:
        path = str((logs_dir / file_name).resolve())
        if path in existing_paths:
            return
        handler = RotatingFileHandler(path, maxBytes=4 * 1024 * 1024, backupCount=5)
        handler.setLevel(level)
        handler.setFormatter(formatter)
        handler.addFilter(_PrefixFilter(prefixes))
        root.addHandler(handler)

    _add_handler("gateway-rust.log", ("qm.daemon", "qm.checkpoint", "gateway."))
    _add_handler("satellite-python.log", ("qm.worker", "qm.satellite"))
    _add_handler("satellite-vir.log", ("qm.vir", "vir."))


# ── CLI Entry Point ─────────────────────────────────────────────────

async def _async_main(config: QMDaemonConfig) -> None:
    """Async entry point for the daemon."""
    daemon = QMDaemon(config)

    loop = asyncio.get_event_loop()
    stop_future = loop.create_future()

    def _handle_signal() -> None:
        if not stop_future.done():
            stop_future.set_result(None)

    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, _handle_signal)

    await daemon.start()
    try:
        await stop_future
    finally:
        await daemon.stop()


def _show_status(data_dir: str) -> None:
    """Print status from the state file."""
    state_path = Path(data_dir) / _STATE_FILE
    if not state_path.exists():
        print("QM daemon is not running (no state file)")
        sys.exit(1)

    state = json.loads(state_path.read_text())
    pid = state.get("pid", 0)

    # Check if PID is still alive
    try:
        os.kill(pid, 0)
        alive = True
    except (OSError, ProcessLookupError):
        alive = False

    print(f"QM Daemon Status")
    print(f"  PID:      {pid} ({'running' if alive else 'dead'})")
    print(f"  Data Dir: {state.get('data_dir', 'N/A')}")
    print(f"  Gateway:  {state.get('host', '?')}:{state.get('port', '?')}")
    if state.get("unix_socket_path"):
        print(f"  UDS:      {state.get('unix_socket_path')}")
    print(f"  Ready:    {state.get('ready', False)}")
    if state.get("ready_error"):
        print(f"  ReadyErr: {state.get('ready_error')}")
    print(f"  Engine:   {state.get('engine_version', '?')}")
    print(f"  Media:    {_format_bytes(state.get('media_capacity', 0))}")


def _resolve_unix_socket_path(spec: Optional[str], port: int) -> Optional[str]:
    """Resolve CLI unix socket option into an absolute socket path.

    - None: disabled
    - "auto": PostgreSQL-compatible `/tmp/.s.PGSQL.<port>`
    - directory path: `<dir>/.s.PGSQL.<port>`
    - file path: used as-is
    """
    if spec is None:
        return None

    if spec == "auto":
        return f"/tmp/.s.PGSQL.{port}"

    p = Path(spec)
    if p.exists() and p.is_dir():
        return str((p / f".s.PGSQL.{port}").resolve())

    # Heuristic: directory path if explicit trailing slash or no suffix.
    if spec.endswith("/"):
        d = Path(spec)
        d.mkdir(parents=True, exist_ok=True)
        return str((d / f".s.PGSQL.{port}").resolve())

    if p.suffix == "":
        d = p
        d.mkdir(parents=True, exist_ok=True)
        return str((d / f".s.PGSQL.{port}").resolve())

    # Treat as explicit socket file path and ensure parent dir exists.
    p.parent.mkdir(parents=True, exist_ok=True)

    return str(p)


def _run_cli_check(data_dir: str) -> int:
    """Run quick CLI diagnostics to catch common operator errors."""
    issues: list[str] = []
    data_path = Path(data_dir)
    if not data_path.exists():
        issues.append(f"data-dir does not exist: {data_path}")

    pid_path = data_path / _PID_FILE
    state_path = data_path / _STATE_FILE

    if pid_path.exists() and not state_path.exists():
        issues.append("pid file exists but state file missing")

    if state_path.exists():
        try:
            state = json.loads(state_path.read_text())
            pid = int(state.get("pid", 0))
            if pid > 0:
                try:
                    os.kill(pid, 0)
                except OSError:
                    issues.append(f"stale state file: pid {pid} is not running")
            uds = state.get("unix_socket_path")
            if uds:
                uds_parent = Path(str(uds)).parent
                if not uds_parent.exists():
                    issues.append(f"uds parent dir missing: {uds_parent}")
            if state.get("ready") is False:
                issues.append(f"daemon not ready: {state.get('ready_error') or 'unknown reason'}")
        except Exception as exc:
            issues.append(f"invalid state file: {exc}")

    if issues:
        print("CLI check: FAIL")
        for issue in issues:
            print(f"  - {issue}")
        return 1

    print("CLI check: OK")
    print(f"  data-dir: {data_path}")
    print("  parser:   OK")
    print("  state:    consistent")
    return 0


def _resolve_chaos_report_path(data_dir: str | None, report: str) -> str:
    p = Path(report)
    if p.is_absolute() or data_dir is None:
        return str(p)
    return str((Path(data_dir) / p).resolve())


def _build_parser() -> argparse.ArgumentParser:
    """Build the subcommand-based CLI parser."""
    parser = argparse.ArgumentParser(
        prog="qmvir",
        description="QMvir — Hybrid AI-Native Database",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  qmvir start
    qmvir start --port 55433 --data-dir /ssd/qm_data
  qmvir sql -u admin -p secret
  qmvir dash
    qmvir dash-chaos
    qmvir logs --layer gateway-rust
  qmvir bench
  qmvir status
  qmvir stop
        """,
    )
    # Global options
    parser.add_argument("--data-dir", default="/tmp/qm_data", help="Data directory")
    parser.add_argument("--log-level", default="INFO",
                        choices=["DEBUG", "INFO", "WARNING", "ERROR"])
    parser.add_argument(
        "--lang",
        default=os.environ.get("QM_LANG", "en"),
        choices=["vi", "en", "zh", "zht"],
        help="Display language: vi (Vietnamese), en (English), zh (Simplified Chinese), zht (Traditional Chinese). "
             "Can also be set via QM_LANG environment variable. (default: en)",
    )

    sub = parser.add_subparsers(dest="command", help="Available commands")

    # ── start ───────────────────────────────────────────────────────
    sp_start = sub.add_parser("start", help="Start the QM daemon")
    sp_start.add_argument("--host", default="127.0.0.1", help="Bind address")
    sp_start.add_argument("--port", type=int, default=55433, help="Listen port")
    sp_start.add_argument(
        "--unix-socket",
        nargs="?",
        const="auto",
        default=None,
        help="Enable UDS listener (optional path). Default path when omitted: /tmp/.s.PGSQL.<port>",
    )
    sp_start.add_argument("--checkpoint-interval", type=float, default=30.0,
                          help="Auto-checkpoint interval (seconds)")
    sp_start.add_argument("--vector-dim", type=int, default=128,
                          help="Default vector dimension")
    sp_start.add_argument("--process-isolation", action="store_true",
                          help="Spawn satellites as OS processes")

    # ── stop ────────────────────────────────────────────────────────
    sub.add_parser("stop", help="Stop the daemon")

    # ── status ──────────────────────────────────────────────────────
    sub.add_parser("status", help="Show daemon status")

    # ── version ─────────────────────────────────────────────────────
    sub.add_parser("version", help="Print version information")

    # ── check ───────────────────────────────────────────────────────
    sub.add_parser("check", help="Run CLI diagnostics (state, pid, socket paths)")

    # ── sql ─────────────────────────────────────────────────────────
    sp_sql = sub.add_parser("sql", help="Interactive SQL shell (REPL)")
    sp_sql.add_argument("-u", "--user", default="admin",
                        help="Username for RBAC authentication")
    sp_sql.add_argument("-p", "--password", default="",
                        help="Password (omit for interactive prompt)")
    sp_sql.add_argument("--host", default=None,
                        help="Gateway host (default: discover from daemon state)")
    sp_sql.add_argument("--port", type=int, default=None,
                        help="Gateway port (default: discover from daemon state)")
    sp_sql.add_argument("--daemon", choices=["auto", "on", "off"], default="auto",
                        help="SQL mode: auto-discover daemon, force daemon, or local-only")
    sp_sql.add_argument("--local", action="store_true",
                        help="Use embedded local engine instead of daemon gateway")

    # ── dash ────────────────────────────────────────────────────────
    sp_dash = sub.add_parser("dash", help="Real-time monitoring dashboard")
    sp_dash.add_argument("--refresh", type=float, default=1.0,
                         help="Refresh interval in seconds")

    # ── dash-chaos ──────────────────────────────────────────────────
    sp_dash_chaos = sub.add_parser("dash-chaos", help="Show chaos run dashboard")
    sp_dash_chaos.add_argument(
        "--report",
        default="/Users/gengyang/Desktop/AI/QM/benchmarks/QMVIR_CHAOS_REPORT.json",
        help="Path to chaos JSON report",
    )
    sp_dash_chaos.add_argument(
        "--data-dir",
        dest="dash_data_dir",
        default=None,
        help="Resolve relative --report against this data directory",
    )
    sp_dash_chaos.add_argument("--refresh", type=float, default=1.0,
                               help="Refresh interval; use 0 for one-shot")
    sp_dash_chaos.add_argument("--json", action="store_true",
                               help="Print raw JSON")

    # ── logs ───────────────────────────────────────────────────────
    sp_logs = sub.add_parser("logs", help="View layered daemon logs")
    sp_logs.add_argument(
        "--layer",
        choices=["gateway-rust", "satellite-python", "satellite-vir", "all"],
        default="all",
        help="Filter by logical log layer",
    )
    sp_logs.add_argument("--tail", type=int, default=200,
                         help="Show last N lines")
    sp_logs.add_argument("--follow", action="store_true",
                         help="Follow log output")
    sp_logs.add_argument(
        "--data-dir",
        dest="logs_data_dir",
        default=None,
        help="Override data directory for logs lookup",
    )

    # ── bench ───────────────────────────────────────────────────────
    sp_bench = sub.add_parser("bench", help="Run performance benchmarks")
    sp_bench.add_argument("--only",
                          choices=["ring", "gateway", "checkpoint", "vector", "parse"],
                          help="Run only the named benchmark")
    sp_bench.add_argument("--json", dest="bench_json", metavar="FILE",
                          help="Write results to a JSON report file")

    # Legacy flags for backward compatibility
    parser.add_argument("--start", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--status", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--stop", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--host", default=None, help=argparse.SUPPRESS)
    parser.add_argument("--port", type=int, default=None, help=argparse.SUPPRESS)
    parser.add_argument("--unix-socket", default=None, help=argparse.SUPPRESS)
    parser.add_argument("--checkpoint-interval", type=float, default=30.0,
                        help=argparse.SUPPRESS)
    parser.add_argument("--vector-dim", type=int, default=128,
                        help=argparse.SUPPRESS)
    parser.add_argument("--process-isolation", action="store_true",
                        help=argparse.SUPPRESS)

    return parser


def main() -> None:
    """CLI entry point — ``qm-server`` or ``python qm_app.py``."""
    parser = _build_parser()
    args = parser.parse_args()
    lang = getattr(args, "lang", "en")

    # Logging setup
    logging.basicConfig(
        level=getattr(logging, args.log_level),
        format="%(asctime)s [%(name)s] %(levelname)s %(message)s",
        datefmt="%H:%M:%S",
    )

    # Resolve command: prefer subcommand, fall back to legacy flags
    cmd = args.command
    if cmd is None:
        if getattr(args, "start", False):
            cmd = "start"
        elif getattr(args, "status", False):
            cmd = "status"
        elif getattr(args, "stop", False):
            cmd = "stop"

    if cmd == "version":
        print(f"QM Database v{__version__}")
        print(f"  Engine:  v{QMHubEngine.VERSION}")
        print(f"  Kernel:  v{__import__('qm_core').__version__}")
        print(f"  Python:  {sys.version.split()[0]}")
        return

    if cmd == "status":
        _show_status(args.data_dir)
        return

    if cmd == "stop":
        pid_path = Path(args.data_dir) / _PID_FILE
        if not pid_path.exists():
            print(_msg(lang, "daemon_not_running"))
            sys.exit(1)
        pid = int(pid_path.read_text().strip())
        print(_msg(lang, "sending_sigterm", pid=pid))
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            print(_msg(lang, "process_not_found"))
            pid_path.unlink(missing_ok=True)
        return

    if cmd == "check":
        rc = _run_cli_check(args.data_dir)
        if rc != 0:
            sys.exit(rc)
        return

    if cmd == "start":
        port = args.port or 55433
        _setup_layered_file_logging(args.data_dir, args.log_level)
        config = QMDaemonConfig(
            data_dir=args.data_dir,
            host=args.host or "127.0.0.1",
            port=port,
            unix_socket_path=_resolve_unix_socket_path(args.unix_socket, port),
            checkpoint_interval=args.checkpoint_interval,
            vector_dim=args.vector_dim,
            process_isolation=args.process_isolation,
        )
        asyncio.run(_async_main(config))
        return

    if cmd == "sql":
        from qm_core.cli.sql_shell import run_sql_shell
        run_sql_shell(
            data_dir=args.data_dir,
            username=args.user,
            password=args.password,
            host=args.host,
            port=args.port,
            local_mode=args.local,
            daemon_mode=args.daemon,
        )
        return

    if cmd == "dash":
        from qm_core.cli.dashboard import run_dashboard
        run_dashboard(data_dir=args.data_dir, refresh=args.refresh)
        return

    if cmd == "dash-chaos":
        from qm_core.cli.chaos_dashboard import run_dash_chaos
        report_file = _resolve_chaos_report_path(args.dash_data_dir, args.report)
        rc = run_dash_chaos(report_file=report_file, refresh=args.refresh, json_mode=args.json)
        if rc != 0:
            sys.exit(rc)
        return

    if cmd == "logs":
        from qm_core.cli.logs import run_logs
        target_data_dir = args.logs_data_dir or args.data_dir
        rc = run_logs(data_dir=target_data_dir, layer=args.layer, tail=args.tail, follow=args.follow)
        if rc != 0:
            sys.exit(rc)
        return

    if cmd == "bench":
        from qm_core.bench import run_all, print_report, export_json
        results = run_all(only=getattr(args, "only", None))
        print_report(results)
        json_path = getattr(args, "bench_json", None)
        if json_path:
            export_json(results, json_path)
            print(_msg(lang, "json_written", path=json_path))
        return

    parser.print_help()


if __name__ == "__main__":
    main()
