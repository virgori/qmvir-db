"""QM Hub — Deterministic LSN Sequencer.

Assigns monotonically increasing Log Sequence Numbers to every
transaction command **before** dispatching to satellites.

This is the backbone of deterministic ordering:
 - Every write is stamped with a unique LSN
 - WAL entries are keyed by LSN
 - Crash recovery replays from the last flushed LSN
 - Satellites can be rebuilt by replaying the LSN stream

The sequencer is single-threaded by design (the Hub is the single
writer that serialises all mutation ordering).

Properties:
 - Monotonically increasing: LSN(n+1) > LSN(n)  always
 - Gap-free: no holes in the sequence
 - Durable: checkpointed to WAL so survives restart
 - Epoch-aware: new epoch on recovery → detects stale satellites
"""

from __future__ import annotations

import struct
import threading
import time
from dataclasses import dataclass, field
from typing import Optional

try:
    import qm_engine as _qm_engine
except ImportError:
    _qm_engine = None


@dataclass(frozen=True, slots=True)
class LSNStamp:
    """Immutable LSN stamp attached to every command."""
    lsn: int            # Monotonic sequence number
    epoch: int          # Recovery epoch (incremented on restart)
    timestamp_ns: int   # Wall-clock nanoseconds (informational)

    def to_bytes(self) -> bytes:
        return struct.pack("<QQQ", self.lsn, self.epoch, self.timestamp_ns)

    @classmethod
    def from_bytes(cls, data: bytes) -> "LSNStamp":
        lsn, epoch, ts = struct.unpack("<QQQ", data[:24])
        return cls(lsn=lsn, epoch=epoch, timestamp_ns=ts)

    def __lt__(self, other: "LSNStamp") -> bool:
        return (self.epoch, self.lsn) < (other.epoch, other.lsn)


class LSNSequencer:
    """Deterministic, monotonic LSN generator.

    Thread-safe but designed to be called from a single Hub thread.
    The lock is a safety net for diagnostic / metrics access.

    Parameters
    ----------
    start_lsn : int
        First LSN to issue (typically loaded from last WAL checkpoint).
    epoch : int
        Current recovery epoch.
    """

    def __init__(self, start_lsn: int = 1, epoch: int = 0):
        self._lsn = start_lsn
        self._epoch = epoch
        self._lock = threading.Lock()
        self._issued_count = 0
        self._last_stamp: Optional[LSNStamp] = None

    # ── Core ───────────────────────────────────────────────────────

    def next(self) -> LSNStamp:
        """Issue the next LSN stamp."""
        with self._lock:
            stamp = LSNStamp(
                lsn=self._lsn,
                epoch=self._epoch,
                timestamp_ns=time.time_ns(),
            )
            self._lsn += 1
            self._issued_count += 1
            self._last_stamp = stamp
            return stamp

    def next_batch(self, count: int) -> list[LSNStamp]:
        """Issue a contiguous batch of LSN stamps."""
        stamps: list[LSNStamp] = []
        with self._lock:
            ts = time.time_ns()
            for _ in range(count):
                s = LSNStamp(lsn=self._lsn, epoch=self._epoch, timestamp_ns=ts)
                self._lsn += 1
                stamps.append(s)
            self._issued_count += count
            if stamps:
                self._last_stamp = stamps[-1]
        return stamps

    def peek(self) -> int:
        """Return the next LSN that will be issued (without advancing)."""
        return self._lsn

    def bump_epoch(self) -> int:
        """Increment epoch (called on recovery / restart)."""
        with self._lock:
            self._epoch += 1
            return self._epoch

    # ── Checkpoint / Recovery ──────────────────────────────────────

    def checkpoint_state(self) -> bytes:
        """Serialize current state for WAL checkpoint."""
        with self._lock:
            return struct.pack("<QQ", self._lsn, self._epoch)

    @classmethod
    def from_checkpoint(cls, data: bytes) -> "LSNSequencer":
        """Restore sequencer from checkpoint bytes."""
        lsn, epoch = struct.unpack("<QQ", data[:16])
        return cls(start_lsn=lsn, epoch=epoch)

    # ── Diagnostics ────────────────────────────────────────────────

    @property
    def current_lsn(self) -> int:
        return self._lsn

    @property
    def epoch(self) -> int:
        return self._epoch

    @property
    def issued_count(self) -> int:
        return self._issued_count

    @property
    def last_stamp(self) -> Optional[LSNStamp]:
        return self._last_stamp

    def stats(self) -> dict:
        return {
            "current_lsn": self._lsn,
            "epoch": self._epoch,
            "issued_count": self._issued_count,
        }
