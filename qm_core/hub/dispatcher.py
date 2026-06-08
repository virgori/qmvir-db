"""QM Hub Dispatcher — Gateway between Engine API and IPC ring.

The HubDispatcher translates high-level engine operations into
Hub-dispatched IPC commands. This is the single choke-point where
all data mutations and queries cross the process boundary.

Architecture:

    QMEngine (facade)
        │
        ▼
    HubDispatcher  (this module)
        │
        ├── hub_gen.dispatch() → ring_gen → GeneralSatellite
        ├── hub_vec.dispatch() → ring_vec → VectorSatellite
        └── hub_proc.dispatch() → ring_proc → ProcedureSatellite

Each satellite type has its own dedicated ring buffer, ensuring
commands are never misrouted. The Hub only assigns LSNs and
coordinates — no data computation.
"""

from __future__ import annotations

import struct
import tempfile
from dataclasses import dataclass
from typing import Any, Optional

import msgpack

try:
    import qm_engine as _qm_engine
except ImportError:
    _qm_engine = None

from qm_core.hub.hub import Hub, CommandResult
from qm_core.hub.lsn_sequencer import LSNSequencer, LSNStamp
from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType


@dataclass
class DispatcherConfig:
    """Configuration for the Hub Dispatcher."""
    ring_dir: str | None = None       # Directory for ring buffer files
    slot_count: int = 1024
    slot_data_size: int = 65536       # 64KB per slot
    wal_path: str | None = None       # Hub WAL path (None = in-memory)
    timeout_ms: int = 5000            # Default dispatch timeout
    # Legacy compat
    ring_path: str | None = None      # DEPRECATED — use ring_dir


class HubDispatcher:
    """Translates engine API calls to Hub IPC commands.

    Uses separate ring buffers per satellite type to prevent
    command misrouting. Each ring is consumed by exactly one
    satellite.

    Parameters
    ----------
    config : DispatcherConfig
        Dispatcher settings.
    """

    def __init__(self, config: DispatcherConfig | None = None) -> None:
        self._config = config or DispatcherConfig()

        # Ring directory
        ring_dir = self._config.ring_dir
        if ring_dir is None:
            ring_dir = tempfile.mkdtemp(prefix="qm_rings_")
        self._ring_dir = ring_dir

        import os
        os.makedirs(ring_dir, exist_ok=True)

        # Per-satellite ring buffers
        self._ring_gen = SharedRingBuffer(
            path=f"{ring_dir}/gen_ring.shm",
            slot_count=self._config.slot_count,
            slot_data_size=self._config.slot_data_size,
            create=True,
        )
        self._ring_vec = SharedRingBuffer(
            path=f"{ring_dir}/vec_ring.shm",
            slot_count=self._config.slot_count,
            slot_data_size=self._config.slot_data_size,
            create=True,
        )
        self._ring_proc = SharedRingBuffer(
            path=f"{ring_dir}/proc_ring.shm",
            slot_count=self._config.slot_count,
            slot_data_size=self._config.slot_data_size,
            create=True,
        )

        # Per-satellite Hubs (shared LSN sequencer for global ordering)
        self._sequencer = LSNSequencer(start_lsn=1, epoch=0)

        self._hub_gen = Hub(
            ring=self._ring_gen,
            wal_path=self._config.wal_path,
            start_lsn=1,
        )
        self._hub_vec = Hub(ring=self._ring_vec)
        self._hub_proc = Hub(ring=self._ring_proc)

        # Share the same sequencer across all hubs for global LSN ordering
        self._hub_vec._sequencer = self._hub_gen._sequencer
        self._hub_proc._sequencer = self._hub_gen._sequencer

    @property
    def hub(self) -> Hub:
        """Primary hub (general satellite)."""
        return self._hub_gen

    @property
    def hub_gen(self) -> Hub:
        return self._hub_gen

    @property
    def hub_vec(self) -> Hub:
        return self._hub_vec

    @property
    def hub_proc(self) -> Hub:
        return self._hub_proc

    @property
    def ring_gen(self) -> SharedRingBuffer:
        return self._ring_gen

    @property
    def ring_vec(self) -> SharedRingBuffer:
        return self._ring_vec

    @property
    def ring_proc(self) -> SharedRingBuffer:
        return self._ring_proc

    # ── DDL Operations ──────────────────────────────────────────────

    def create_table(self, table: str, schema: dict[str, str]) -> CommandResult:
        """Dispatch CREATE TABLE to general satellite via Hub."""
        payload = msgpack.packb({
            "table": table,
            "action": "create",
            "schema": schema,
        })
        return self._hub_gen.dispatch_sync(
            CommandType.DDL, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    def drop_table(self, table: str) -> CommandResult:
        """Dispatch DROP TABLE to general satellite via Hub."""
        payload = msgpack.packb({
            "table": table,
            "action": "drop",
        })
        return self._hub_gen.dispatch_sync(
            CommandType.DDL, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    # ── DML Operations ──────────────────────────────────────────────

    def insert(self, table: str, row: dict[str, Any]) -> CommandResult:
        """Dispatch INSERT to general satellite via Hub."""
        payload = msgpack.packb({
            "table": table,
            "row": row,
        })
        return self._hub_gen.dispatch_sync(
            CommandType.INSERT, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    def insert_batch(self, table: str, rows: list[dict[str, Any]]) -> CommandResult:
        """Dispatch BATCH INSERT to general satellite via Hub.
        
        Sends all rows in a single IPC command for high throughput.
        Much faster than individual inserts for bulk data loading.
        """
        payload = msgpack.packb({
            "table": table,
            "rows": rows,
        })
        return self._hub_gen.dispatch_sync(
            CommandType.BATCH_INSERT, table, payload,
            timeout_ms=self._config.timeout_ms * max(1, len(rows) // 100),
        )

    def update(self, table: str, row_id: int, updates: dict[str, Any]) -> CommandResult:
        """Dispatch UPDATE to general satellite via Hub."""
        payload = msgpack.packb({
            "table": table,
            "row_id": row_id,
            "updates": updates,
        })
        return self._hub_gen.dispatch_sync(
            CommandType.UPDATE, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    def delete(self, table: str, row_id: int) -> CommandResult:
        """Dispatch DELETE to general satellite via Hub."""
        payload = msgpack.packb({
            "table": table,
            "row_id": row_id,
        })
        return self._hub_gen.dispatch_sync(
            CommandType.DELETE, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    def query(self, table: str, predicates: list[dict] | None = None) -> CommandResult:
        """Dispatch QUERY to general satellite via Hub."""
        payload = msgpack.packb({
            "table": table,
            "predicates": predicates or [],
        })
        return self._hub_gen.dispatch_sync(
            CommandType.QUERY, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    # ── Vector Operations (→ vector ring) ───────────────────────────

    def vector_insert(self, table: str, vec_id: int, vector_bytes: bytes,
                      dim: int, meta: bytes = b"") -> CommandResult:
        """Dispatch vector insert to vector satellite via Hub."""
        payload = (
            struct.pack("<BqI", 1, vec_id, dim)
            + vector_bytes
            + struct.pack("<I", len(meta))
            + meta
        )
        return self._hub_vec.dispatch_sync(
            CommandType.VECTOR_OP, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    def vector_search(self, table: str, query_bytes: bytes,
                      dim: int, top_k: int = 10) -> CommandResult:
        """Dispatch vector search to vector satellite via Hub."""
        payload = struct.pack("<BIH", 2, dim, top_k) + query_bytes
        return self._hub_vec.dispatch_sync(
            CommandType.VECTOR_OP, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    def vector_delete(self, table: str, vec_id: int) -> CommandResult:
        """Dispatch vector delete to vector satellite via Hub."""
        payload = struct.pack("<Bq", 3, vec_id)
        return self._hub_vec.dispatch_sync(
            CommandType.VECTOR_OP, table, payload,
            timeout_ms=self._config.timeout_ms,
        )

    # ── Procedure Execution (→ procedure ring) ──────────────────────

    def call_procedure(self, name: str, args: dict[str, Any]) -> CommandResult:
        """Dispatch CALL to procedure satellite via Hub."""
        payload = msgpack.packb({
            "name": name,
            "args": args,
        })
        return self._hub_proc.dispatch_sync(
            CommandType.DDL, "_plqm", payload,
            timeout_ms=self._config.timeout_ms,
        )

    # ── Batch Operations ────────────────────────────────────────────

    def insert_batch_individual(self, table: str, rows: list[dict[str, Any]]) -> list[CommandResult]:
        """Dispatch batch of inserts as individual commands — contiguous LSNs.

        Unlike insert_batch() which sends all rows in a single IPC slot,
        this method sends each row as a separate INSERT command, which
        yields per-row LSN tracking and individual error reporting.
        """
        commands = []
        for row in rows:
            payload = msgpack.packb({"table": table, "row": row})
            commands.append((CommandType.INSERT, table, payload))

        envelopes = self._hub_gen.dispatch_batch(commands, timeout_ms=self._config.timeout_ms)
        return self._hub_gen.collect_batch(envelopes)

    # ── Diagnostics ─────────────────────────────────────────────────

    def stats(self) -> dict:
        return {
            "hub_gen": self._hub_gen.stats(),
            "hub_vec": self._hub_vec.stats(),
            "hub_proc": self._hub_proc.stats(),
            "ring_dir": self._ring_dir,
        }

    def close(self) -> None:
        """Shut down the dispatcher."""
        self._hub_gen.close()
        self._hub_vec.close()
        self._hub_proc.close()

    @property
    def current_lsn(self) -> int:
        return self._hub_gen.sequencer.current_lsn

    @property
    def merkle_root(self) -> bytes:
        return self._hub_gen.merkle_root()
