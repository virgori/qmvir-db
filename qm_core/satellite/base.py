"""QM Satellite — Base class for all satellite processes.

A Satellite attaches to the Hub's shared memory ring buffer,
polls for READY slots, executes commands, and marks slots DONE/ERROR.

Each satellite type (Vector, General, Analytics) overrides the
``_execute_command()`` method to provide domain-specific logic.

Lifecycle:
    1. Attach to shared memory ring buffer (consumer side)
    2. Maintain local WAL (Sat_WAL) for crash recovery
    3. Poll for READY slots → execute → mark DONE
    4. Report page hashes to Hub for Merkle audit
"""

from __future__ import annotations

import hashlib
import os
import struct
import threading
import time
from abc import ABC, abstractmethod
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import Any, Optional

try:
    import qm_engine as _qm_engine
except ImportError:
    _qm_engine = None

from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType, SlotState, SlotHeader
from qm_core.hub.lsn_sequencer import LSNStamp


@dataclass
class SatelliteConfig:
    """Configuration for a satellite process."""
    satellite_id: str = "sat-0"
    poll_interval_us: int = 100     # microseconds between polls
    data_dir: str = "/tmp/qm_sat"   # local storage directory
    wal_enabled: bool = True        # local WAL for crash recovery
    max_batch: int = 64             # max commands per poll cycle
    num_workers: int = 4            # number of worker threads for concurrent execution
    concurrent: bool = False        # enable concurrent execution


class Satellite(ABC):
    """Abstract base for satellite compute/storage nodes.

    Parameters
    ----------
    config : SatelliteConfig
        Satellite configuration.
    ring : SharedRingBuffer
        Shared memory ring (consumer side).
    """

    def __init__(self, config: SatelliteConfig, ring: SharedRingBuffer):
        self._config = config
        self._ring = ring
        self._running = False
        self._thread: Optional[threading.Thread] = None

        # Local WAL
        self._wal_path = os.path.join(config.data_dir, f"{config.satellite_id}_wal.bin")
        self._wal_file = None
        self._wal_lock = threading.Lock()

        # Stats
        self._processed = 0
        self._errors = 0
        self._last_lsn = 0
        self._stats_lock = threading.Lock()

        # Page hashes for Merkle audit reporting
        self._page_hashes: dict[str, bytes] = {}
        self._hash_lock = threading.Lock()
        
        # Thread pool for concurrent execution
        self._executor: Optional[ThreadPoolExecutor] = None
        if config.concurrent:
            self._executor = ThreadPoolExecutor(
                max_workers=config.num_workers,
                thread_name_prefix=f"sat-worker-{config.satellite_id}",
            )
        
        # Table-level locks for write ordering
        self._table_locks: dict[str, threading.Lock] = {}
        self._table_locks_lock = threading.Lock()

        os.makedirs(config.data_dir, exist_ok=True)
    
    def _get_table_lock(self, table: str) -> threading.Lock:
        """Get or create a lock for a specific table."""
        with self._table_locks_lock:
            if table not in self._table_locks:
                self._table_locks[table] = threading.Lock()
            return self._table_locks[table]

    # ── Abstract ────────────────────────────────────────────────────

    @abstractmethod
    def _execute_command(
        self,
        stamp: LSNStamp,
        cmd: CommandType,
        payload: bytes,
    ) -> bytes:
        """Execute a single command. Returns result payload bytes.

        Subclasses implement domain-specific logic here.
        Raise an exception to signal ERROR to the Hub.
        """
        ...

    # ── Lifecycle ───────────────────────────────────────────────────

    def start(self) -> None:
        """Start the satellite polling loop in a daemon thread."""
        if self._running:
            return
        self._running = True
        if self._config.wal_enabled:
            self._wal_file = open(self._wal_path, "ab")
        self._thread = threading.Thread(
            target=self._poll_loop,
            name=f"sat-{self._config.satellite_id}",
            daemon=True,
        )
        self._thread.start()

    def stop(self) -> None:
        """Stop the polling loop."""
        self._running = False
        if self._thread:
            self._thread.join(timeout=2.0)
            self._thread = None
        if self._wal_file:
            self._wal_file.close()
            self._wal_file = None
        if self._executor:
            self._executor.shutdown(wait=True)
            self._executor = None

    def _poll_loop(self) -> None:
        """Main poll loop — scans ring buffer for READY slots."""
        interval_s = self._config.poll_interval_us / 1_000_000.0
        while self._running:
            batch_count = 0
            while batch_count < self._config.max_batch:
                result = self._ring.consume()
                if result is None:
                    break
                slot_idx, hdr, data_mv = result
                
                if self._executor and self._config.concurrent:
                    # Submit to thread pool for concurrent execution
                    self._executor.submit(
                        self._handle_slot, slot_idx, hdr, bytes(data_mv)
                    )
                else:
                    # Sequential execution
                    self._handle_slot(slot_idx, hdr, bytes(data_mv))
                batch_count += 1
            if batch_count == 0:
                time.sleep(interval_s)

    def _handle_slot(
        self,
        slot_idx: int,
        hdr: SlotHeader,
        raw_data: bytes,
    ) -> None:
        """Process a single slot — extract LSN stamp, execute, mark done."""
        try:
            # Extract LSN stamp from wire payload (first 24 bytes)
            stamp = LSNStamp.from_bytes(raw_data[:24])
            payload = raw_data[24:]

            # Local WAL (before execution) - thread-safe
            self._wal_write(stamp, hdr.cmd, payload)

            # Execute
            result = self._execute_command(stamp, hdr.cmd, payload)

            # Update page hash for Merkle audit - thread-safe
            page_key = f"{self._config.satellite_id}:{stamp.lsn}"
            with self._hash_lock:
                self._page_hashes[page_key] = hashlib.sha256(result or payload).digest()

            # Mark DONE
            self._ring.complete(slot_idx, result_payload=result)
            with self._stats_lock:
                self._processed += 1
                self._last_lsn = stamp.lsn

        except Exception as exc:
            error_msg = str(exc).encode("utf-8")[:1024]
            self._ring.fail(slot_idx, error_payload=error_msg)
            with self._stats_lock:
                self._errors += 1

    # ── Local WAL ───────────────────────────────────────────────────

    def _wal_write(self, stamp: LSNStamp, cmd: CommandType, payload: bytes) -> None:
        if self._wal_file is None:
            return
        entry = struct.pack("<QQB I", stamp.lsn, stamp.epoch, int(cmd), len(payload))
        with self._wal_lock:
            self._wal_file.write(entry + payload)
            self._wal_file.flush()

    # ── Merkle audit ────────────────────────────────────────────────

    def report_hashes(self) -> dict[str, bytes]:
        """Return all page hashes for Hub Merkle audit."""
        return dict(self._page_hashes)

    # ── One-shot execution (for testing / synchronous mode) ────────

    def process_one(self) -> bool:
        """Process exactly one slot (blocking). Returns True if processed."""
        result = self._ring.consume()
        if result is None:
            return False
        slot_idx, hdr, data_mv = result
        self._handle_slot(slot_idx, hdr, bytes(data_mv))
        return True

    # ── Diagnostics ─────────────────────────────────────────────────

    def stats(self) -> dict:
        return {
            "satellite_id": self._config.satellite_id,
            "processed": self._processed,
            "errors": self._errors,
            "last_lsn": self._last_lsn,
            "page_hashes": len(self._page_hashes),
            "running": self._running,
        }
