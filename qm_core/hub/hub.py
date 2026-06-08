"""QM Hub — Central Control Plane.

Orchestrates the entire transaction flow:
 1. Client SQL → Parser → AST → Optimizer → Logical Plan
 2. Assign deterministic LSN via sequencer
 3. Write to Hub WAL (durability)
 4. Dispatch command to satellite via Shared Memory ring buffer
 5. Collect result → update Merkle auditor → respond to client

The Hub is the single source of truth for ordering. It never stores
raw data itself — that lives on satellites.
"""

from __future__ import annotations

import hashlib
import os
import struct
import time
from dataclasses import dataclass, field
from typing import Any, Optional

import msgpack

try:
    import qm_engine as _qm_engine
except ImportError:
    _qm_engine = None

from qm_core.hub.lsn_sequencer import LSNSequencer, LSNStamp
from qm_core.hub.merkle_auditor import MerkleAuditor
from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType, SlotState


@dataclass(slots=True)
class CommandEnvelope:
    """A command ready for dispatch."""
    stamp: LSNStamp
    cmd: CommandType
    table: str
    payload: bytes
    slot_index: int = -1  # assigned after publish


@dataclass(slots=True)
class CommandResult:
    """Result returned from a satellite."""
    lsn: int
    success: bool
    data: bytes
    latency_us: int = 0

    def _batch_count(self) -> int:
        try:
            payload = msgpack.unpackb(self.data, raw=False)
        except Exception:
            return 1
        if isinstance(payload, dict):
            if isinstance(payload.get("row_ids"), list):
                return len(payload["row_ids"])
            if isinstance(payload.get("count"), int):
                return max(0, payload["count"])
        return 1

    def __len__(self) -> int:
        return self._batch_count()

    def __iter__(self):
        for _ in range(self._batch_count()):
            yield self


class Hub:
    """QM Control Plane — the brain of the database.

    Parameters
    ----------
    ring : SharedRingBuffer
        The shared memory ring for IPC.
    wal_path : str | None
        Path for Hub-side WAL. None = in-memory only.
    start_lsn : int
        Starting LSN (from checkpoint).
    epoch : int
        Recovery epoch.
    """

    def __init__(
        self,
        ring: SharedRingBuffer,
        wal_path: str | None = None,
        start_lsn: int = 1,
        epoch: int = 0,
    ):
        self._ring = ring
        self._sequencer = LSNSequencer(start_lsn=start_lsn, epoch=epoch)
        self._auditor = MerkleAuditor()

        # Hub WAL — lightweight append-only log of LSN assignments
        self._wal_path = wal_path
        self._wal_file = None
        if wal_path:
            self._wal_file = open(wal_path, "ab")
        self._wal_group_commit_ms = max(0, int(os.getenv("QM_WAL_GROUP_COMMIT_MS", "8")))
        self._wal_pending = 0
        self._wal_last_flush_ns = time.time_ns()

        # In-flight commands tracking: lsn → (envelope, submit_time_ns)
        self._inflight: dict[int, tuple[CommandEnvelope, int]] = {}

        # Stats
        self._dispatched_count = 0
        self._completed_count = 0
        self._error_count = 0

    # ── Transaction dispatch ────────────────────────────────────────

    def dispatch(
        self,
        cmd: CommandType,
        table: str,
        payload: bytes,
        timeout_ms: int = 1000,
    ) -> CommandEnvelope:
        """Assign LSN, write WAL, dispatch to satellite via ring buffer.

        Returns the envelope (with slot_index set). Raises if ring is full.
        """
        # 1. Assign deterministic LSN
        stamp = self._sequencer.next()

        # 2. Write to Hub WAL for durability
        self._wal_append(stamp, cmd, table, payload)

        # 3. Build envelope
        envelope = CommandEnvelope(
            stamp=stamp,
            cmd=cmd,
            table=table,
            payload=payload,
        )

        # 4. Publish to ring buffer
        # Prefix payload with LSN stamp (24 bytes) for satellite correlation
        wire_payload = stamp.to_bytes() + payload
        slot = self._ring.try_publish(
            lsn=stamp.lsn,
            cmd=cmd,
            payload=wire_payload,
            timeout_ms=timeout_ms,
        )
        if slot < 0:
            raise RuntimeError(
                f"Ring buffer full — could not dispatch LSN {stamp.lsn} "
                f"within {timeout_ms}ms"
            )

        envelope.slot_index = slot
        self._inflight[stamp.lsn] = (envelope, time.time_ns())
        self._dispatched_count += 1
        return envelope

    def collect(self, envelope: CommandEnvelope) -> CommandResult:
        """Block until the satellite finishes the command and return result."""
        start = time.time_ns()
        while True:
            state, data = self._ring.collect_result(envelope.slot_index)
            if state == SlotState.DONE:
                lat = (time.time_ns() - start) // 1000
                self._inflight.pop(envelope.stamp.lsn, None)
                self._completed_count += 1

                # Update Merkle auditor with data hash (if write op)
                if envelope.cmd in (CommandType.INSERT, CommandType.UPDATE, CommandType.DELETE):
                    page_hash = hashlib.sha256(data if data else envelope.payload).digest()
                    key = f"{envelope.table}:{envelope.stamp.lsn}"
                    self._auditor.update_leaf(key, page_hash)

                return CommandResult(
                    lsn=envelope.stamp.lsn,
                    success=True,
                    data=data,
                    latency_us=lat,
                )
            elif state == SlotState.ERROR:
                lat = (time.time_ns() - start) // 1000
                self._inflight.pop(envelope.stamp.lsn, None)
                self._error_count += 1
                return CommandResult(
                    lsn=envelope.stamp.lsn,
                    success=False,
                    data=data,
                    latency_us=lat,
                )
            # Still PROCESSING — spin
            time.sleep(0.0001)

    def dispatch_sync(
        self,
        cmd: CommandType,
        table: str,
        payload: bytes,
        timeout_ms: int = 5000,
    ) -> CommandResult:
        """Convenience: dispatch + collect in one call."""
        env = self.dispatch(cmd, table, payload, timeout_ms=timeout_ms)
        return self.collect(env)

    # ── Batch dispatch ──────────────────────────────────────────────

    def dispatch_batch(
        self,
        commands: list[tuple[CommandType, str, bytes]],
        timeout_ms: int = 2000,
    ) -> list[CommandEnvelope]:
        """Dispatch a batch of commands, assigning contiguous LSNs."""
        stamps = self._sequencer.next_batch(len(commands))
        envelopes: list[CommandEnvelope] = []

        for stamp, (cmd, table, payload) in zip(stamps, commands):
            self._wal_append(stamp, cmd, table, payload)
            wire_payload = stamp.to_bytes() + payload
            slot = self._ring.try_publish(
                lsn=stamp.lsn, cmd=cmd, payload=wire_payload,
                timeout_ms=timeout_ms,
            )
            if slot < 0:
                raise RuntimeError(f"Ring full at batch LSN {stamp.lsn}")

            env = CommandEnvelope(stamp=stamp, cmd=cmd, table=table,
                                 payload=payload, slot_index=slot)
            envelopes.append(env)
            self._inflight[stamp.lsn] = (env, time.time_ns())
            self._dispatched_count += 1

        return envelopes

    def collect_batch(self, envelopes: list[CommandEnvelope]) -> list[CommandResult]:
        """Collect results for a batch of commands."""
        return [self.collect(env) for env in envelopes]

    # ── Hub WAL ─────────────────────────────────────────────────────

    def _wal_append(
        self,
        stamp: LSNStamp,
        cmd: CommandType,
        table: str,
        payload: bytes,
    ) -> None:
        """Append a Hub WAL entry (lightweight — just LSN + cmd + table)."""
        if self._wal_file is None:
            return
        tb = table.encode("utf-8")
        entry = struct.pack(
            "<QQB H",
            stamp.lsn,
            stamp.epoch,
            int(cmd),
            len(tb),
        )
        self._wal_file.write(entry + tb)
        self._wal_pending += 1

        now_ns = time.time_ns()
        should_flush = self._wal_group_commit_ms <= 0
        if not should_flush:
            elapsed_ms = (now_ns - self._wal_last_flush_ns) / 1_000_000.0
            if elapsed_ms >= self._wal_group_commit_ms or self._wal_pending >= 256:
                should_flush = True

        if should_flush:
            self._wal_file.flush()
            self._wal_pending = 0
            self._wal_last_flush_ns = now_ns

    # ── Merkle audit ────────────────────────────────────────────────

    def merkle_root(self) -> bytes:
        """Current Merkle root of all audited data."""
        return self._auditor.root()

    def audit_satellite(self, satellite_leaves: dict[str, bytes]) -> list[str]:
        """Cross-check satellite-reported page hashes. Returns divergent keys."""
        return self._auditor.divergent_leaves(satellite_leaves)

    # ── Recovery ────────────────────────────────────────────────────

    def checkpoint(self) -> bytes:
        """Serialize Hub state for crash recovery."""
        seq_data = self._sequencer.checkpoint_state()
        merkle_data = self._auditor.checkpoint()
        return struct.pack("<I", len(seq_data)) + seq_data + merkle_data

    @classmethod
    def from_checkpoint(cls, data: bytes, ring: SharedRingBuffer) -> "Hub":
        """Restore Hub from checkpoint."""
        (seq_len,) = struct.unpack("<I", data[:4])
        seq_data = data[4 : 4 + seq_len]
        merkle_data = data[4 + seq_len :]

        seq = LSNSequencer.from_checkpoint(seq_data)
        seq.bump_epoch()  # new epoch on recovery

        hub = cls(ring=ring, start_lsn=seq.current_lsn, epoch=seq.epoch)
        hub._auditor = MerkleAuditor.from_checkpoint(merkle_data)
        return hub

    # ── Diagnostics ─────────────────────────────────────────────────

    @property
    def sequencer(self) -> LSNSequencer:
        return self._sequencer

    @property
    def auditor(self) -> MerkleAuditor:
        return self._auditor

    def stats(self) -> dict:
        return {
            "lsn": self._sequencer.stats(),
            "merkle": self._auditor.stats(),
            "ring": self._ring.stats(),
            "dispatched": self._dispatched_count,
            "completed": self._completed_count,
            "errors": self._error_count,
            "inflight": len(self._inflight),
        }

    def close(self) -> None:
        if self._wal_file:
            if self._wal_pending > 0:
                self._wal_file.flush()
                self._wal_pending = 0
            self._wal_file.close()
            self._wal_file = None
