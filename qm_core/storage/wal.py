"""QM Storage — Binary Write-Ahead Log.

Binary format per record:
    [4B magic][4B crc32][8B lsn][8B txn_id][1B op_type][2B table_len][table]
    [2B key_len][key][4B data_len][data][8B timestamp]

Features:
    - CRC32 integrity per record
    - Binary append-only (no JSON overhead)
    - Checkpoint markers with flush
    - Crash recovery via replay
    - Segment rotation by size
    - fsync control (group commit support)
"""

from __future__ import annotations

import os
import struct
import time
import zlib
import threading
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable

try:
    import orjson

    def _pack_data(d: dict | None) -> bytes:
        return orjson.dumps(d) if d else b""

    def _unpack_data(b: bytes) -> dict | None:
        return orjson.loads(b) if b else None
except ImportError:
    import json

    def _pack_data(d: dict | None) -> bytes:
        return json.dumps(d).encode() if d else b""

    def _unpack_data(b: bytes) -> dict | None:
        return json.loads(b) if b else None


# ── WAL record magic ────────────────────────────────────────────────
WAL_MAGIC = 0x514D_574C  # "QMWL"
CHECKPOINT_MAGIC = 0x514D_434B  # "QMCK"

# ── Record header: magic(4) + crc(4) + lsn(8) + txn(8) + op(1) = 25 bytes
RECORD_HEADER_FMT = "<IIQQ B"
RECORD_HEADER_SIZE = struct.calcsize(RECORD_HEADER_FMT)


class WALOp(IntEnum):
    """WAL operation types."""
    INSERT = 1
    UPDATE = 2
    DELETE = 3
    COMMIT = 10
    ROLLBACK = 11
    CHECKPOINT = 20
    BEGIN = 30


@dataclass(slots=True)
class WALRecord:
    """A single binary WAL record."""
    lsn: int
    txn_id: int
    op: WALOp
    table: str = ""
    key: str = ""
    data: dict[str, Any] | None = None
    old_data: dict[str, Any] | None = None
    timestamp: float = 0.0

    def serialize(self) -> bytes:
        """Serialize to binary with CRC32 integrity."""
        table_b = self.table.encode("utf-8")
        key_b = self.key.encode("utf-8")
        data_b = _pack_data(self.data)
        old_b = _pack_data(self.old_data)
        ts_b = struct.pack("<d", self.timestamp)

        # Body = table_len(2) + table + key_len(2) + key +
        #        data_len(4) + data + old_len(4) + old + ts(8)
        body = (
            struct.pack("<H", len(table_b)) + table_b
            + struct.pack("<H", len(key_b)) + key_b
            + struct.pack("<I", len(data_b)) + data_b
            + struct.pack("<I", len(old_b)) + old_b
            + ts_b
        )

        # CRC over: lsn + txn + op + body
        payload = struct.pack("<QQB", self.lsn, self.txn_id, int(self.op)) + body
        crc = zlib.crc32(payload) & 0xFFFFFFFF

        # Full record: magic + crc + payload
        return struct.pack("<II", WAL_MAGIC, crc) + payload

    @classmethod
    def deserialize(cls, buf: bytes, offset: int = 0) -> tuple[WALRecord, int]:
        """Deserialize from binary buffer. Returns (record, bytes_consumed)."""
        pos = offset

        magic, crc = struct.unpack_from("<II", buf, pos)
        pos += 8
        if magic != WAL_MAGIC:
            raise ValueError(f"Bad WAL magic: 0x{magic:08X}")

        payload_start = pos
        lsn, txn_id, op_byte = struct.unpack_from("<QQB", buf, pos)
        pos += 17  # 8+8+1

        table_len, = struct.unpack_from("<H", buf, pos); pos += 2
        table = buf[pos:pos + table_len].decode("utf-8"); pos += table_len

        key_len, = struct.unpack_from("<H", buf, pos); pos += 2
        key = buf[pos:pos + key_len].decode("utf-8"); pos += key_len

        data_len, = struct.unpack_from("<I", buf, pos); pos += 4
        data_b = buf[pos:pos + data_len]; pos += data_len

        old_len, = struct.unpack_from("<I", buf, pos); pos += 4
        old_b = buf[pos:pos + old_len]; pos += old_len

        ts, = struct.unpack_from("<d", buf, pos); pos += 8

        # Verify CRC
        payload = buf[payload_start:pos]
        actual_crc = zlib.crc32(payload) & 0xFFFFFFFF
        if actual_crc != crc:
            raise ValueError(f"CRC mismatch: expected {crc:#x}, got {actual_crc:#x}")

        rec = cls(
            lsn=lsn, txn_id=txn_id, op=WALOp(op_byte),
            table=table, key=key,
            data=_unpack_data(data_b),
            old_data=_unpack_data(old_b),
            timestamp=ts,
        )
        return rec, pos - offset


class WALSegment:
    """A single WAL segment file."""

    def __init__(self, path: str, segment_id: int) -> None:
        self.path = path
        self.segment_id = segment_id
        self._size = 0
        self._fd: int | None = None

    def open_write(self) -> None:
        self._fd = os.open(self.path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
        self._size = os.fstat(self._fd).st_size

    def write(self, data: bytes) -> None:
        if self._fd is None:
            self.open_write()
        os.write(self._fd, data)
        self._size += len(data)

    def sync(self) -> None:
        if self._fd is not None:
            os.fsync(self._fd)

    def close(self) -> None:
        if self._fd is not None:
            os.close(self._fd)
            self._fd = None

    @property
    def size(self) -> int:
        return self._size

    def read_all(self) -> bytes:
        with open(self.path, "rb") as f:
            return f.read()


class WriteAheadLog:
    """Binary WAL with checkpoint, recovery, and segment rotation.

    Usage:
        wal = WriteAheadLog("/data/wal")
        wal.open()
        rec = wal.append(WALOp.INSERT, txn_id=1, table="users", key="u1", data={...})
        wal.checkpoint()
        wal.close()
    """

    MAX_SEGMENT_SIZE = 64 * 1024 * 1024  # 64 MB per segment

    def __init__(
        self,
        wal_dir: str,
        max_segment_size: int = 0,
        fsync_per_write: bool = False,
        group_commit_interval_ms: int = 5,
    ) -> None:
        self.wal_dir = wal_dir
        self._max_seg_size = max_segment_size or self.MAX_SEGMENT_SIZE
        self._fsync_per_write = fsync_per_write
        self._group_commit_interval_s = max(0.001, group_commit_interval_ms / 1000.0)
        self._lsn = 0
        self._segment_id = 0
        self._active_segment: WALSegment | None = None
        self._segments: list[WALSegment] = []
        self._lock = threading.Lock()
        self._checkpoint_lsn = 0
        self._callbacks: list[Callable[[WALRecord], None]] = []
        # Write-combining buffer: batch multiple records into one I/O call
        self._write_buffer: bytearray = bytearray()
        self._write_buffer_limit = 256 * 1024  # Flush every 256KB
        self._buffered_records: list[WALRecord] = []
        self._bg_thread: threading.Thread | None = None
        self._bg_stop = threading.Event()
        self._bg_wakeup = threading.Event()

    def open(self) -> int:
        """Open WAL, replay any existing segments. Returns recovered LSN."""
        os.makedirs(self.wal_dir, exist_ok=True)
        # Discover existing segments
        seg_files = sorted(
            f for f in os.listdir(self.wal_dir) if f.startswith("wal_") and f.endswith(".bin")
        )
        recovered = 0
        for sf in seg_files:
            seg_id = int(sf.split("_")[1].split(".")[0])
            seg = WALSegment(os.path.join(self.wal_dir, sf), seg_id)
            self._segments.append(seg)
            self._segment_id = max(self._segment_id, seg_id)
            # Read and recover LSN
            try:
                buf = seg.read_all()
                pos = 0
                while pos < len(buf):
                    try:
                        rec, consumed = WALRecord.deserialize(buf, pos)
                        self._lsn = max(self._lsn, rec.lsn)
                        recovered += 1
                        pos += consumed
                    except (ValueError, struct.error):
                        break  # Corrupted tail — truncate
            except FileNotFoundError:
                pass

        # Open new active segment
        self._rotate_segment()
        self._start_group_commit_worker()
        return recovered

    def append(
        self,
        op: WALOp,
        txn_id: int,
        table: str = "",
        key: str = "",
        data: dict[str, Any] | None = None,
        old_data: dict[str, Any] | None = None,
    ) -> WALRecord:
        """Append a record to the WAL. Thread-safe.

        Records are buffered and flushed in batches (write-combining)
        for higher throughput. Use group_commit() or checkpoint() to
        guarantee durability.
        """
        with self._lock:
            self._lsn += 1
            rec = WALRecord(
                lsn=self._lsn, txn_id=txn_id, op=op,
                table=table, key=key, data=data, old_data=old_data,
                timestamp=time.time(),
            )
            raw = rec.serialize()

            # Rotate segment if needed
            buf_pending = len(self._write_buffer) + len(raw)
            if self._active_segment and self._active_segment.size + buf_pending > self._max_seg_size:
                self._flush_write_buffer()
                self._rotate_segment()

            # Buffer the write
            self._write_buffer.extend(raw)
            self._buffered_records.append(rec)

            # Flush buffer if over limit or fsync mode
            if self._fsync_per_write or len(self._write_buffer) >= self._write_buffer_limit:
                self._flush_write_buffer()
                if self._fsync_per_write:
                    self._active_segment.sync()
            else:
                self._bg_wakeup.set()

            for cb in self._callbacks:
                cb(rec)

            return rec

    def _flush_write_buffer(self) -> None:
        """Flush write-combining buffer to segment. Must hold lock."""
        if self._write_buffer and self._active_segment:
            self._active_segment.write(bytes(self._write_buffer))
            self._write_buffer.clear()
            self._buffered_records.clear()

    def group_commit(self) -> None:
        """Flush write buffer + fsync the active segment (group commit)."""
        with self._lock:
            self._flush_write_buffer()
            if self._active_segment:
                self._active_segment.sync()

    def checkpoint(self) -> int:
        """Write checkpoint marker, return checkpoint LSN."""
        rec = self.append(WALOp.CHECKPOINT, txn_id=0)
        self._checkpoint_lsn = rec.lsn
        self.group_commit()
        # Write checkpoint file
        ckpt_path = os.path.join(self.wal_dir, "checkpoint")
        with open(ckpt_path, "wb") as f:
            f.write(struct.pack("<Q", self._checkpoint_lsn))
        return self._checkpoint_lsn

    def replay(self, since_lsn: int = 0, callback: Callable[[WALRecord], None] | None = None) -> list[WALRecord]:
        """Replay WAL records since a given LSN."""
        # Flush write buffer so all records are on disk
        with self._lock:
            self._flush_write_buffer()

        records: list[WALRecord] = []
        for seg in self._segments:
            try:
                buf = seg.read_all()
            except FileNotFoundError:
                continue
            pos = 0
            while pos < len(buf):
                try:
                    rec, consumed = WALRecord.deserialize(buf, pos)
                    pos += consumed
                    if rec.lsn > since_lsn:
                        records.append(rec)
                        if callback:
                            callback(rec)
                except (ValueError, struct.error):
                    break
        return records

    def get_checkpoint_lsn(self) -> int:
        """Read last checkpoint LSN from disk."""
        ckpt_path = os.path.join(self.wal_dir, "checkpoint")
        if os.path.exists(ckpt_path):
            with open(ckpt_path, "rb") as f:
                data = f.read(8)
                if len(data) == 8:
                    self._checkpoint_lsn = struct.unpack("<Q", data)[0]
        return self._checkpoint_lsn

    def register_callback(self, cb: Callable[[WALRecord], None]) -> None:
        """Register a CDC-style callback for new records."""
        self._callbacks.append(cb)

    def truncate_before(self, lsn: int) -> int:
        """Remove segments whose max LSN < given LSN. Returns count removed."""
        removed = 0
        keep: list[WALSegment] = []
        for seg in self._segments:
            if seg is self._active_segment:
                keep.append(seg)
                continue
            # Check max LSN in segment
            max_lsn = self._segment_max_lsn(seg)
            if max_lsn < lsn:
                try:
                    os.unlink(seg.path)
                except OSError:
                    pass
                removed += 1
            else:
                keep.append(seg)
        self._segments = keep
        return removed

    def close(self) -> None:
        self._stop_group_commit_worker()
        with self._lock:
            self._flush_write_buffer()
        if self._active_segment:
            self._active_segment.sync()
            self._active_segment.close()
            self._active_segment = None

    @property
    def current_lsn(self) -> int:
        return self._lsn

    def _rotate_segment(self) -> None:
        """Close current segment and open a new one."""
        if self._active_segment:
            self._active_segment.sync()
            self._active_segment.close()
        self._segment_id += 1
        path = os.path.join(self.wal_dir, f"wal_{self._segment_id:08d}.bin")
        seg = WALSegment(path, self._segment_id)
        seg.open_write()
        self._segments.append(seg)
        self._active_segment = seg

    def _segment_max_lsn(self, seg: WALSegment) -> int:
        """Find the max LSN in a segment."""
        max_lsn = 0
        try:
            buf = seg.read_all()
            pos = 0
            while pos < len(buf):
                try:
                    rec, consumed = WALRecord.deserialize(buf, pos)
                    max_lsn = max(max_lsn, rec.lsn)
                    pos += consumed
                except (ValueError, struct.error):
                    break
        except FileNotFoundError:
            pass
        return max_lsn

    def _start_group_commit_worker(self) -> None:
        """Start background group-commit worker for buffered WAL writes."""
        if self._fsync_per_write or self._bg_thread is not None:
            return
        self._bg_stop.clear()
        self._bg_wakeup.clear()
        self._bg_thread = threading.Thread(
            target=self._group_commit_loop,
            name="qm-wal-group-commit",
            daemon=True,
        )
        self._bg_thread.start()

    def _stop_group_commit_worker(self) -> None:
        """Stop background group-commit worker."""
        if self._bg_thread is None:
            return
        self._bg_stop.set()
        self._bg_wakeup.set()
        self._bg_thread.join(timeout=2.0)
        self._bg_thread = None

    def _group_commit_loop(self) -> None:
        """Periodically flush + fsync buffered records to keep latency bounded."""
        while not self._bg_stop.is_set():
            self._bg_wakeup.wait(timeout=self._group_commit_interval_s)
            self._bg_wakeup.clear()
            if self._bg_stop.is_set():
                break
            with self._lock:
                if not self._write_buffer:
                    continue
            self.group_commit()
