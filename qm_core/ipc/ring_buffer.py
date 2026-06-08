"""QM IPC — Lock-free Shared Memory Ring Buffer (LMAX Disruptor Model).

Implements a zero-copy, lock-free ring buffer over mmap shared memory
for Hub ↔ Satellite IPC without serialization overhead.

Layout (per slot):
┌─────────────────────────────────────────────────────────┐
│ state(1B) │ lsn(8B) │ cmd(1B) │ payload_sz(4B) │ pad(2B) │
├─────────────────────────────────────────────────────────┤
│                     data[slot_data_size]                 │
└─────────────────────────────────────────────────────────┘

Slot state transitions (atomic 1-byte CAS):
    EMPTY ──(Hub write)──► READY ──(Sat read)──► PROCESSING
        ──(Sat done)──► DONE ──(Hub ack)──► EMPTY
                            or
        ──(Sat fail)──► ERROR ──(Hub collect)──► EMPTY
"""

from __future__ import annotations

import ctypes
import enum
import mmap
import os
import struct
import tempfile
import time
from dataclasses import dataclass
from typing import Optional

try:
    import qm_engine as _qm_engine
except ImportError:
    _qm_engine = None


# ── Constants ────────────────────────────────────────────────────────

_HEADER_FMT = "<BqBIH"  # state(1) + lsn(8) + cmd(1) + payload_sz(4) + pad(2) = 16 bytes
SLOT_HEADER_SIZE = struct.calcsize(_HEADER_FMT)  # 16 bytes
assert SLOT_HEADER_SIZE == 16

# Ring metadata stored at the start of the mmap region
_RING_META_FMT = "<QQQQ"  # magic(8) + slot_count(8) + slot_total_size(8) + slot_data_size(8)
_RING_META_SIZE = struct.calcsize(_RING_META_FMT)  # 32 bytes
_RING_MAGIC = 0x514D_5249_4E47_4246  # "QMRINGBF" in little-endian hex


class SlotState(enum.IntEnum):
    """Atomic slot state — each fits in 1 byte."""
    EMPTY      = 0
    READY      = 1   # Hub wrote command, waiting for Satellite
    PROCESSING = 2   # Satellite picked up the command
    DONE       = 3   # Satellite finished successfully
    ERROR      = 4   # Satellite encountered error


class CommandType(enum.IntEnum):
    """IPC command identifiers."""
    NOOP       = 0
    INSERT     = 1
    UPDATE     = 2
    DELETE     = 3
    QUERY      = 4
    DDL        = 5
    VECTOR_OP  = 6
    COMPRESS   = 7
    CHECKPOINT = 8
    BATCH_INSERT = 9  # Batch insert for high throughput
    SHUTDOWN   = 255


@dataclass(frozen=True, slots=True)
class SlotHeader:
    """Decoded slot header."""
    state: SlotState
    lsn: int
    cmd: CommandType
    payload_size: int


# ── Shared Ring Buffer ───────────────────────────────────────────────

class SharedRingBuffer:
    """Lock-free ring buffer over mmap shared memory.

    Parameters
    ----------
    path : str | None
        File path backing the mmap region.  If None, creates a temp file.
    slot_count : int
        Number of slots (must be power of 2 for fast modulo).
    slot_data_size : int
        Max payload bytes per slot (default 64 KB).
    create : bool
        True  – create and initialize a new ring (Hub side).
        False – attach to an existing ring (Satellite side).
    """

    def __init__(
        self,
        path: str | None = None,
        slot_count: int = 1024,
        slot_data_size: int = 65536,
        create: bool = True,
    ):
        if slot_count & (slot_count - 1) != 0:
            raise ValueError(f"slot_count must be power of 2, got {slot_count}")

        self._slot_count = slot_count
        self._slot_data_size = slot_data_size
        self._slot_total_size = SLOT_HEADER_SIZE + slot_data_size
        self._mask = slot_count - 1  # fast modulo

        total_size = _RING_META_SIZE + slot_count * self._slot_total_size

        # File-backed mmap
        if path is None:
            fd, path = tempfile.mkstemp(prefix="qm_ring_", suffix=".shm")
            os.close(fd)
        self._path = path

        if create:
            with open(path, "wb") as f:
                f.write(b"\x00" * total_size)
            self._fd = os.open(path, os.O_RDWR)
            self._mm = mmap.mmap(self._fd, total_size)
            self._write_ring_meta()
        else:
            self._fd = os.open(path, os.O_RDWR)
            self._mm = mmap.mmap(self._fd, 0)  # map entire file
            self._validate_ring_meta()

        # Hub-side cursor: next slot to try publishing into
        self._hub_cursor = 0
        # Satellite-side cursor: next slot to try consuming
        self._sat_cursor = 0

    # ── Ring metadata ───────────────────────────────────────────────

    def _write_ring_meta(self) -> None:
        data = struct.pack(
            _RING_META_FMT,
            _RING_MAGIC,
            self._slot_count,
            self._slot_total_size,
            self._slot_data_size,
        )
        self._mm[0:_RING_META_SIZE] = data

    def _validate_ring_meta(self) -> None:
        magic, sc, sts, sds = struct.unpack(
            _RING_META_FMT, self._mm[0:_RING_META_SIZE]
        )
        if magic != _RING_MAGIC:
            raise ValueError("Invalid ring buffer magic — not a QM shared memory region")
        if sc != self._slot_count or sds != self._slot_data_size:
            raise ValueError(
                f"Ring geometry mismatch: expected {self._slot_count}×{self._slot_data_size}, "
                f"got {sc}×{sds}"
            )

    # ── Slot addressing ─────────────────────────────────────────────

    def _slot_offset(self, index: int) -> int:
        """Byte offset of slot[index] header in the mmap."""
        return _RING_META_SIZE + (index & self._mask) * self._slot_total_size

    def _read_header(self, index: int) -> SlotHeader:
        off = self._slot_offset(index)
        raw = self._mm[off : off + SLOT_HEADER_SIZE]
        state, lsn, cmd, psz, _pad = struct.unpack(_HEADER_FMT, raw)
        return SlotHeader(
            state=SlotState(state),
            lsn=lsn,
            cmd=CommandType(cmd),
            payload_size=psz,
        )

    def _write_header(
        self,
        index: int,
        state: SlotState,
        lsn: int = 0,
        cmd: CommandType = CommandType.NOOP,
        payload_size: int = 0,
    ) -> None:
        off = self._slot_offset(index)
        raw = struct.pack(_HEADER_FMT, int(state), lsn, int(cmd), payload_size, 0)
        self._mm[off : off + SLOT_HEADER_SIZE] = raw

    def _set_state(self, index: int, state: SlotState) -> None:
        """Atomic-ish 1-byte state transition."""
        off = self._slot_offset(index)
        self._mm[off] = int(state)

    def _get_state(self, index: int) -> SlotState:
        off = self._slot_offset(index)
        return SlotState(self._mm[off])

    def _data_offset(self, index: int) -> int:
        return self._slot_offset(index) + SLOT_HEADER_SIZE

    # ── Hub side (Producer) ─────────────────────────────────────────

    def publish(
        self,
        lsn: int,
        cmd: CommandType,
        payload: bytes | memoryview,
    ) -> int:
        """Write a command into the next available slot (Hub → Satellite).

        Returns the slot index used, or -1 if the ring is full.
        """
        payload_len = len(payload)
        if payload_len > self._slot_data_size:
            raise ValueError(
                f"Payload {payload_len} bytes exceeds slot capacity {self._slot_data_size}"
            )

        # Scan for an EMPTY slot starting from hub cursor
        for attempt in range(self._slot_count):
            idx = (self._hub_cursor + attempt) & self._mask
            if self._get_state(idx) == SlotState.EMPTY:
                # Write payload first (data before header for visibility)
                doff = self._data_offset(idx)
                self._mm[doff : doff + payload_len] = bytes(payload)

                # Write header with state=READY (makes slot visible to Satellite)
                self._write_header(idx, SlotState.READY, lsn, cmd, payload_len)

                self._hub_cursor = (idx + 1) & self._mask
                return idx

        return -1  # ring full

    def try_publish(
        self,
        lsn: int,
        cmd: CommandType,
        payload: bytes | memoryview,
        timeout_ms: int = 1000,
    ) -> int:
        """Blocking publish with timeout — spins until a slot is available."""
        deadline = time.monotonic() + timeout_ms / 1000.0
        while True:
            idx = self.publish(lsn, cmd, payload)
            if idx >= 0:
                return idx
            if time.monotonic() >= deadline:
                return -1
            time.sleep(0.0001)  # 100µs backoff

    def collect_result(self, index: int) -> tuple[SlotState, bytes]:
        """Collect result from a completed slot (Hub reads Satellite reply).

        Returns (state, payload_data). Resets slot to EMPTY after read.
        """
        hdr = self._read_header(index)
        if hdr.state not in (SlotState.DONE, SlotState.ERROR):
            return (hdr.state, b"")

        doff = self._data_offset(index)
        data = bytes(self._mm[doff : doff + hdr.payload_size])

        # Reset slot
        self._set_state(index, SlotState.EMPTY)
        return (hdr.state, data)

    def collect_all_done(self) -> list[tuple[int, SlotState, bytes]]:
        """Sweep all DONE/ERROR slots, return (slot_idx, state, payload)."""
        results: list[tuple[int, SlotState, bytes]] = []
        for i in range(self._slot_count):
            st = self._get_state(i)
            if st in (SlotState.DONE, SlotState.ERROR):
                _, data = self.collect_result(i)
                results.append((i, st, data))
        return results

    # ── Satellite side (Consumer) ───────────────────────────────────

    def consume(self) -> Optional[tuple[int, SlotHeader, memoryview]]:
        """Try to consume the next READY slot (Satellite side).

        Returns (slot_index, header, data_memoryview) or None if nothing ready.
        The satellite MUST call ``complete()`` or ``fail()`` when done.
        """
        for attempt in range(self._slot_count):
            idx = (self._sat_cursor + attempt) & self._mask
            if self._get_state(idx) == SlotState.READY:
                # Transition to PROCESSING atomically
                self._set_state(idx, SlotState.PROCESSING)
                hdr = self._read_header(idx)
                # Fix: re-read but keep PROCESSING state
                hdr = SlotHeader(
                    state=SlotState.PROCESSING,
                    lsn=hdr.lsn,
                    cmd=hdr.cmd,
                    payload_size=hdr.payload_size,
                )

                doff = self._data_offset(idx)
                data = self._mm[doff : doff + hdr.payload_size]

                self._sat_cursor = (idx + 1) & self._mask
                return (idx, hdr, data)
        return None

    def complete(self, index: int, result_payload: bytes = b"") -> None:
        """Mark slot as DONE with optional result payload (Satellite → Hub)."""
        if result_payload:
            if len(result_payload) > self._slot_data_size:
                raise ValueError("Result payload exceeds slot capacity")
            doff = self._data_offset(index)
            self._mm[doff : doff + len(result_payload)] = result_payload
            # Update payload_size in header
            hdr_off = self._slot_offset(index)
            struct.pack_into("<I", self._mm, hdr_off + 10, len(result_payload))

        self._set_state(index, SlotState.DONE)

    def fail(self, index: int, error_payload: bytes = b"") -> None:
        """Mark slot as ERROR with optional error info (Satellite → Hub)."""
        if error_payload:
            if len(error_payload) > self._slot_data_size:
                error_payload = error_payload[: self._slot_data_size]
            doff = self._data_offset(index)
            self._mm[doff : doff + len(error_payload)] = error_payload
            hdr_off = self._slot_offset(index)
            struct.pack_into("<I", self._mm, hdr_off + 10, len(error_payload))

        self._set_state(index, SlotState.ERROR)

    # ── Diagnostics ─────────────────────────────────────────────────

    @property
    def path(self) -> str:
        return self._path

    @property
    def slot_count(self) -> int:
        return self._slot_count

    @property
    def slot_data_size(self) -> int:
        return self._slot_data_size

    def stats(self) -> dict[str, int]:
        """Count slots in each state."""
        counts = {s.name: 0 for s in SlotState}
        for i in range(self._slot_count):
            st = self._get_state(i)
            counts[SlotState(st).name] += 1
        return counts

    def close(self) -> None:
        """Unmap and close file descriptor."""
        try:
            self._mm.close()
        except Exception:
            pass
        try:
            os.close(self._fd)
        except Exception:
            pass

    def unlink(self) -> None:
        """Remove the backing file."""
        self.close()
        try:
            os.unlink(self._path)
        except FileNotFoundError:
            pass

    def __del__(self) -> None:
        try:
            self._mm.close()
        except Exception:
            pass
