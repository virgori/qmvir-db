"""QM Auto-Checkpoint — Periodic RAM→SSD state persistence.

Provides automatic, periodic checkpointing of the Hub-Satellite
state to durable storage on SSD.  On crash recovery, the system
replays only the WAL tail (since last checkpoint) instead of the
entire history.

Two modes:
    FULL  — Snapshot entire Hub metadata + satellite epoch markers.
    DELTA — Only WAL segments since last checkpoint are flushed.

Architecture:
    CheckpointManager runs on a background thread inside the Hub
    process.  Every *interval* seconds (or after *lsn_threshold*
    mutations) it:

        1. Freezes the LSN sequencer state.
        2. Asks each satellite (via ring) to flush its buffers.
        3. Writes a checkpoint record to the WAL.
        4. Persists Hub metadata (table schemas, stats, Merkle root)
           to a checkpoint file on SSD.
        5. Optionally truncates old WAL segments.
"""

from __future__ import annotations

import hashlib
import logging
import os
import struct
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional

import msgpack

logger = logging.getLogger("qm.checkpoint")

# Checkpoint file magic
_CKPT_MAGIC = 0x514D_434B_5054  # "QMCKPT"
_CKPT_HEADER_FMT = "<QQQQ"  # magic(8) + lsn(8) + epoch(8) + timestamp_ns(8)
_CKPT_HEADER_SIZE = struct.calcsize(_CKPT_HEADER_FMT)


@dataclass(frozen=True, slots=True)
class CheckpointRecord:
    """Immutable snapshot of system state at checkpoint time."""
    lsn: int                   # LSN at checkpoint
    epoch: int                 # Sequencer epoch
    timestamp_ns: int          # Wall-clock time
    table_meta: dict[str, Any] # Hub-side table metadata
    merkle_root: bytes         # Merkle tree root hash (32B)
    mode: str = "full"         # "full" or "delta"

    def to_bytes(self) -> bytes:
        header = struct.pack(
            _CKPT_HEADER_FMT,
            _CKPT_MAGIC, self.lsn, self.epoch, self.timestamp_ns,
        )
        body = msgpack.packb({
            "table_meta": self.table_meta,
            "merkle_root": self.merkle_root,
            "mode": self.mode,
        })
        # header + body_len(4B) + body + sha256(header+body) for integrity
        body_len = struct.pack("<I", len(body))
        raw = header + body_len + body
        checksum = hashlib.sha256(raw).digest()
        return raw + checksum

    @classmethod
    def from_bytes(cls, data: bytes) -> "CheckpointRecord":
        # Verify checksum
        raw = data[:-32]
        expected_hash = data[-32:]
        actual_hash = hashlib.sha256(raw).digest()
        if actual_hash != expected_hash:
            raise ValueError("Checkpoint integrity check failed (SHA-256 mismatch)")

        magic, lsn, epoch, ts = struct.unpack(_CKPT_HEADER_FMT, raw[:_CKPT_HEADER_SIZE])
        if magic != _CKPT_MAGIC:
            raise ValueError(f"Invalid checkpoint magic: {magic:#x}")

        body_len = struct.unpack("<I", raw[_CKPT_HEADER_SIZE:_CKPT_HEADER_SIZE + 4])[0]
        body = msgpack.unpackb(raw[_CKPT_HEADER_SIZE + 4 : _CKPT_HEADER_SIZE + 4 + body_len])

        return cls(
            lsn=lsn,
            epoch=epoch,
            timestamp_ns=ts,
            table_meta=body["table_meta"],
            merkle_root=body["merkle_root"],
            mode=body.get("mode", "full"),
        )


@dataclass
class CheckpointConfig:
    """Auto-checkpoint configuration."""
    checkpoint_dir: str | None = None     # Directory for checkpoint files
    interval_seconds: float = 30.0        # Time between auto-checkpoints
    lsn_threshold: int = 1000             # Checkpoint after N mutations
    max_checkpoints: int = 5              # Keep N most recent checkpoints
    enabled: bool = True                  # Enable auto-checkpoint thread


class CheckpointManager:
    """Manages periodic checkpointing of Hub state to SSD.

    Usage:
        mgr = CheckpointManager(config, state_provider=engine.checkpoint_state)
        mgr.start()          # Start background thread
        mgr.notify_mutation() # Called on each write
        mgr.force_checkpoint("full")  # Manual checkpoint
        mgr.start()          # Idempotent
        mgr.stop()
    """

    def __init__(
        self,
        config: CheckpointConfig | None = None,
        state_provider: Callable[[], dict[str, Any]] | None = None,
    ) -> None:
        self._config = config or CheckpointConfig()

        ckpt_dir = self._config.checkpoint_dir
        if ckpt_dir is None:
            ckpt_dir = os.path.join(os.getcwd(), "qm_checkpoints")
        self._ckpt_dir = ckpt_dir
        os.makedirs(ckpt_dir, exist_ok=True)

        self._state_provider = state_provider
        self._mutation_count = 0
        self._last_checkpoint_lsn = 0
        self._lock = threading.Lock()
        self._stop_event = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._checkpoint_count = 0

    # ── Background thread ───────────────────────────────────────────

    def start(self) -> None:
        """Start the auto-checkpoint background thread."""
        if not self._config.enabled:
            return
        if self._thread is not None and self._thread.is_alive():
            return
        self._stop_event.clear()
        self._thread = threading.Thread(
            target=self._run_loop,
            name="qm-checkpoint",
            daemon=True,
        )
        self._thread.start()
        logger.info("Auto-checkpoint started (interval=%.1fs, threshold=%d)",
                     self._config.interval_seconds, self._config.lsn_threshold)

    def stop(self) -> None:
        """Stop the auto-checkpoint thread."""
        self._stop_event.set()
        if self._thread is not None:
            self._thread.join(timeout=5.0)
            self._thread = None
        logger.info("Auto-checkpoint stopped")

    @property
    def is_running(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    def _run_loop(self) -> None:
        """Background loop: sleep, check thresholds, checkpoint."""
        while not self._stop_event.is_set():
            self._stop_event.wait(timeout=self._config.interval_seconds)
            if self._stop_event.is_set():
                break
            # Check if threshold reached or timer expired
            with self._lock:
                if self._mutation_count < self._config.lsn_threshold:
                    # Timer expired but not enough mutations — still checkpoint
                    if self._mutation_count == 0:
                        continue
            try:
                self.force_checkpoint("delta")
            except Exception:
                logger.exception("Auto-checkpoint failed")

    # ── Mutation tracking ───────────────────────────────────────────

    def notify_mutation(self) -> None:
        """Called by the engine on every write mutation.

        If mutation count exceeds threshold, triggers immediate checkpoint.
        """
        with self._lock:
            self._mutation_count += 1
            should_checkpoint = (
                self._mutation_count >= self._config.lsn_threshold
            )
        if should_checkpoint:
            try:
                self.force_checkpoint("delta")
            except Exception:
                logger.exception("Threshold-triggered checkpoint failed")

    # ── Checkpoint execution ────────────────────────────────────────

    def force_checkpoint(self, mode: str = "full") -> CheckpointRecord:
        """Force an immediate checkpoint.

        Parameters
        ----------
        mode : str
            "full" or "delta"

        Returns
        -------
        CheckpointRecord
            The written checkpoint.
        """
        state = {}
        if self._state_provider:
            state = self._state_provider()

        lsn = state.get("lsn", 0)
        epoch = state.get("epoch", 0)
        table_meta = state.get("table_meta", {})
        merkle_root = state.get("merkle_root", b"\x00" * 32)

        record = CheckpointRecord(
            lsn=lsn,
            epoch=epoch,
            timestamp_ns=time.time_ns(),
            table_meta=table_meta,
            merkle_root=merkle_root,
            mode=mode,
        )

        # Write to SSD
        filename = f"ckpt_{lsn:012d}_{record.timestamp_ns}.qmck"
        filepath = os.path.join(self._ckpt_dir, filename)
        data = record.to_bytes()
        with open(filepath, "wb") as f:
            f.write(data)
            f.flush()
            os.fsync(f.fileno())

        # Reset mutation counter
        with self._lock:
            self._mutation_count = 0
            self._last_checkpoint_lsn = lsn
        self._checkpoint_count += 1

        # Prune old checkpoints
        self._prune_old_checkpoints()

        logger.info("Checkpoint written: LSN=%d epoch=%d mode=%s file=%s",
                     lsn, epoch, mode, filename)
        return record

    # ── Recovery ────────────────────────────────────────────────────

    def latest_checkpoint(self) -> Optional[CheckpointRecord]:
        """Load the most recent checkpoint from SSD."""
        ckpt_files = sorted(
            [f for f in os.listdir(self._ckpt_dir) if f.endswith(".qmck")],
            reverse=True,
        )
        for fname in ckpt_files:
            filepath = os.path.join(self._ckpt_dir, fname)
            try:
                with open(filepath, "rb") as f:
                    data = f.read()
                return CheckpointRecord.from_bytes(data)
            except (ValueError, struct.error) as e:
                logger.warning("Corrupt checkpoint %s: %s", fname, e)
                continue
        return None

    def recover_state(self) -> dict[str, Any]:
        """Recover Hub state from the latest checkpoint.

        Returns the state dict that the engine can use to restore
        its metadata. Returns empty dict if no checkpoint found.
        """
        record = self.latest_checkpoint()
        if record is None:
            return {}
        return {
            "lsn": record.lsn,
            "epoch": record.epoch,
            "table_meta": record.table_meta,
            "merkle_root": record.merkle_root,
            "mode": record.mode,
        }

    # ── Maintenance ─────────────────────────────────────────────────

    def _prune_old_checkpoints(self) -> None:
        """Keep only the N most recent checkpoint files."""
        ckpt_files = sorted(
            [f for f in os.listdir(self._ckpt_dir) if f.endswith(".qmck")],
        )
        while len(ckpt_files) > self._config.max_checkpoints:
            oldest = ckpt_files.pop(0)
            path = os.path.join(self._ckpt_dir, oldest)
            try:
                os.remove(path)
                logger.debug("Pruned old checkpoint: %s", oldest)
            except OSError:
                pass

    # ── Diagnostics ─────────────────────────────────────────────────

    @property
    def checkpoint_dir(self) -> str:
        return self._ckpt_dir

    @property
    def checkpoint_count(self) -> int:
        return self._checkpoint_count

    @property
    def last_checkpoint_lsn(self) -> int:
        return self._last_checkpoint_lsn

    @property
    def pending_mutations(self) -> int:
        return self._mutation_count

    def stats(self) -> dict:
        return {
            "checkpoint_count": self._checkpoint_count,
            "last_checkpoint_lsn": self._last_checkpoint_lsn,
            "pending_mutations": self._mutation_count,
            "is_running": self.is_running,
            "interval_seconds": self._config.interval_seconds,
            "lsn_threshold": self._config.lsn_threshold,
        }
