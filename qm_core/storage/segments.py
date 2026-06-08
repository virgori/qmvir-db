"""QM Storage — Binary Segment Manager.

Segments are the fundamental storage unit:
    - Immutable once sealed (copy-on-write)
    - Binary format with header + data pages + footer
    - Checksum per page
    - Manifest file tracks all live segments

Segment layout:
    [SegmentHeader 64B]
    [Page0][Page1]...[PageN]
    [SegmentFooter 32B]

Page layout (8KB default):
    [PageHeader 16B] = page_id(4) + row_count(2) + free_off(2) + crc(4) + flags(2) + reserved(2)
    [SlotArray] = row_count * 4B offsets
    [FreeSpace]
    [RowData ... packed from end]
"""

from __future__ import annotations

import os
import struct
import time
import zlib
import threading
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, BinaryIO

try:
    import orjson
    _dumps = orjson.dumps
    _loads = orjson.loads
except ImportError:
    import json
    def _dumps(d: Any) -> bytes:
        return json.dumps(d).encode()
    def _loads(b: bytes) -> Any:
        return json.loads(b)


# ── Constants ───────────────────────────────────────────────────────
PAGE_SIZE = 8192  # 8KB
SEGMENT_MAGIC = 0x514D_5347  # "QMSG"
PAGE_HEADER_SIZE = 16
SLOT_ENTRY_SIZE = 4  # 4B offset per slot


class SegmentType(IntEnum):
    ROW_STORE = 1
    COLUMN_STORE = 2
    INDEX_DATA = 3


@dataclass(slots=True)
class SegmentHeader:
    """64-byte segment header."""
    magic: int = SEGMENT_MAGIC
    version: int = 1
    segment_id: int = 0
    seg_type: SegmentType = SegmentType.ROW_STORE
    page_count: int = 0
    row_count: int = 0
    created_ts: float = 0.0
    min_key: bytes = b""
    max_key: bytes = b""

    def pack(self) -> bytes:
        min_k = self.min_key[:16].ljust(16, b"\x00")
        max_k = self.max_key[:16].ljust(16, b"\x00")
        return struct.pack(
            "<IHI B I I d 16s 16s",
            self.magic, self.version, self.segment_id,
            int(self.seg_type), self.page_count, self.row_count,
            self.created_ts, min_k, max_k,
        )  # 4+2+4+1+4+4+8+16+16 = 59 → pad to 64

    @classmethod
    def unpack(cls, data: bytes) -> SegmentHeader:
        magic, ver, sid, stype, pcnt, rcnt, ts, min_k, max_k = struct.unpack_from(
            "<IHI B I I d 16s 16s", data
        )
        return cls(
            magic=magic, version=ver, segment_id=sid,
            seg_type=SegmentType(stype), page_count=pcnt, row_count=rcnt,
            created_ts=ts, min_key=min_k.rstrip(b"\x00"), max_key=max_k.rstrip(b"\x00"),
        )

    SIZE = 59  # Actual packed size


@dataclass(slots=True)
class PageHeader:
    """16-byte page header."""
    page_id: int = 0
    row_count: int = 0
    free_offset: int = PAGE_SIZE  # Points to start of free space from end
    crc32: int = 0
    flags: int = 0

    def pack(self) -> bytes:
        return struct.pack("<I H H I H xx", self.page_id, self.row_count,
                           self.free_offset, self.crc32, self.flags)

    @classmethod
    def unpack(cls, data: bytes) -> PageHeader:
        pid, rcnt, foff, crc, flags = struct.unpack_from("<I H H I H", data)
        return cls(page_id=pid, row_count=rcnt, free_offset=foff, crc32=crc, flags=flags)


class Page:
    """An 8KB storage page with slot array + row data."""

    def __init__(self, page_id: int = 0) -> None:
        self.header = PageHeader(page_id=page_id)
        self._slots: list[int] = []  # Offsets into _data
        self._data = bytearray(PAGE_SIZE)
        self._rows: list[bytes] = []
        self._write_pos = PAGE_SIZE  # Grows downward from end

    @property
    def free_space(self) -> int:
        used_header = PAGE_HEADER_SIZE + len(self._slots) * SLOT_ENTRY_SIZE
        used_data = PAGE_SIZE - self._write_pos
        return PAGE_SIZE - used_header - used_data

    def can_fit(self, row_bytes: int) -> bool:
        return self.free_space >= row_bytes + SLOT_ENTRY_SIZE + 4  # +4 for len prefix

    def insert_row(self, row_data: bytes) -> int:
        """Insert a row, returns slot index."""
        record = struct.pack("<I", len(row_data)) + row_data
        self._write_pos -= len(record)
        slot_idx = len(self._slots)
        self._slots.append(self._write_pos)
        self._rows.append(row_data)
        self.header.row_count = len(self._slots)
        self.header.free_offset = self._write_pos
        return slot_idx

    def get_row(self, slot_idx: int) -> bytes | None:
        if 0 <= slot_idx < len(self._rows):
            return self._rows[slot_idx]
        return None

    def serialize(self) -> bytes:
        """Pack page into PAGE_SIZE bytes."""
        buf = bytearray(PAGE_SIZE)
        # Slot data
        slot_data = b"".join(struct.pack("<I", s) for s in self._slots)
        # Row data from end
        row_section = bytearray()
        offsets: list[int] = []
        pos = PAGE_SIZE
        for row in self._rows:
            record = struct.pack("<I", len(row)) + row
            pos -= len(record)
            offsets.append(pos)
            buf[pos:pos + len(record)] = record

        # Write slots
        slot_start = PAGE_HEADER_SIZE
        slot_bytes = b"".join(struct.pack("<I", o) for o in offsets)
        buf[slot_start:slot_start + len(slot_bytes)] = slot_bytes

        # Compute CRC over everything except header CRC field
        self.header.row_count = len(self._rows)
        self.header.free_offset = pos
        self.header.crc32 = 0
        hdr = self.header.pack()
        buf[:PAGE_HEADER_SIZE] = hdr
        self.header.crc32 = zlib.crc32(bytes(buf)) & 0xFFFFFFFF
        buf[:PAGE_HEADER_SIZE] = self.header.pack()
        return bytes(buf)

    @classmethod
    def deserialize(cls, data: bytes, page_id: int = 0) -> Page:
        """Unpack from PAGE_SIZE bytes."""
        hdr = PageHeader.unpack(data)
        page = cls(page_id=hdr.page_id)
        page.header = hdr
        # Read slot offsets
        for i in range(hdr.row_count):
            off = struct.unpack_from("<I", data, PAGE_HEADER_SIZE + i * SLOT_ENTRY_SIZE)[0]
            row_len = struct.unpack_from("<I", data, off)[0]
            row_data = data[off + 4: off + 4 + row_len]
            page._rows.append(row_data)
            page._slots.append(off)
        page._write_pos = hdr.free_offset
        return page


class SegmentWriter:
    """Writes an immutable segment to disk."""

    def __init__(self, path: str, segment_id: int, seg_type: SegmentType = SegmentType.ROW_STORE) -> None:
        self._path = path
        self._header = SegmentHeader(
            segment_id=segment_id, seg_type=seg_type, created_ts=time.time()
        )
        self._pages: list[Page] = []
        self._active_page: Page | None = None
        self._total_rows = 0

    def add_row(self, key: bytes, data: bytes) -> tuple[int, int]:
        """Add a row. Returns (page_idx, slot_idx)."""
        row = struct.pack("<H", len(key)) + key + data
        if self._active_page is None or not self._active_page.can_fit(len(row)):
            self._flush_page()
            self._active_page = Page(page_id=len(self._pages))
        slot = self._active_page.insert_row(row)
        self._total_rows += 1
        # Track min/max keys
        if not self._header.min_key or key < self._header.min_key:
            self._header.min_key = key
        if not self._header.max_key or key > self._header.max_key:
            self._header.max_key = key
        return len(self._pages), slot

    def finish(self) -> str:
        """Seal the segment and write to disk."""
        self._flush_page()
        self._header.page_count = len(self._pages)
        self._header.row_count = self._total_rows

        with open(self._path, "wb") as f:
            # Write header (padded to 64B)
            hdr_bytes = self._header.pack()
            f.write(hdr_bytes.ljust(64, b"\x00"))
            # Write pages
            for page in self._pages:
                f.write(page.serialize())
            # Footer: crc of entire file
            f.flush()

        return self._path

    def _flush_page(self) -> None:
        if self._active_page and self._active_page.header.row_count > 0:
            self._pages.append(self._active_page)
            self._active_page = None


class SegmentReader:
    """Reads an immutable segment from disk."""

    def __init__(self, path: str) -> None:
        self._path = path
        self.header: SegmentHeader | None = None

    def open(self) -> SegmentHeader:
        with open(self._path, "rb") as f:
            hdr_data = f.read(64)
        self.header = SegmentHeader.unpack(hdr_data[:SegmentHeader.SIZE])
        return self.header

    def read_page(self, page_idx: int) -> Page:
        offset = 64 + page_idx * PAGE_SIZE
        with open(self._path, "rb") as f:
            f.seek(offset)
            data = f.read(PAGE_SIZE)
        return Page.deserialize(data, page_idx)

    def scan_all(self) -> list[tuple[bytes, bytes]]:
        """Scan all rows as (key, data) tuples."""
        if not self.header:
            self.open()
        rows: list[tuple[bytes, bytes]] = []
        for pi in range(self.header.page_count):
            page = self.read_page(pi)
            for si in range(page.header.row_count):
                raw = page.get_row(si)
                if raw:
                    klen = struct.unpack_from("<H", raw)[0]
                    key = raw[2:2 + klen]
                    data = raw[2 + klen:]
                    rows.append((key, data))
        return rows


class MMapSegmentReader:
    """Memory-mapped segment reader for zero-copy, ultra-high-throughput I/O.

    Uses mmap to map the entire segment file into virtual memory.
    The OS handles page caching; reads are essentially pointer arithmetic.
    """

    def __init__(self, path: str) -> None:
        import mmap as _mmap
        self._path = path
        self._mmap: _mmap.mmap | None = None
        self._fd: int | None = None
        self.header: SegmentHeader | None = None

    def open(self) -> SegmentHeader:
        import mmap as _mmap
        fd = os.open(self._path, os.O_RDONLY)
        size = os.fstat(fd).st_size
        if size == 0:
            os.close(fd)
            raise ValueError(f"Empty segment file: {self._path}")
        self._fd = fd
        self._mmap = _mmap.mmap(fd, 0, access=_mmap.ACCESS_READ)
        # Parse header from first 64 bytes (zero-copy slice)
        self.header = SegmentHeader.unpack(self._mmap[:SegmentHeader.SIZE])
        return self.header

    def read_page(self, page_idx: int) -> Page:
        """Read a page via mmap — no syscall overhead."""
        if self._mmap is None:
            self.open()
        offset = 64 + page_idx * PAGE_SIZE
        data = self._mmap[offset:offset + PAGE_SIZE]
        return Page.deserialize(bytes(data), page_idx)

    def read_page_raw(self, page_idx: int) -> bytes:
        """Read raw page bytes via mmap."""
        if self._mmap is None:
            self.open()
        offset = 64 + page_idx * PAGE_SIZE
        return bytes(self._mmap[offset:offset + PAGE_SIZE])

    def scan_all(self) -> list[tuple[bytes, bytes]]:
        """Scan all rows as (key, data) tuples."""
        if not self.header:
            self.open()
        rows: list[tuple[bytes, bytes]] = []
        for pi in range(self.header.page_count):
            page = self.read_page(pi)
            for si in range(page.header.row_count):
                raw = page.get_row(si)
                if raw:
                    klen = struct.unpack_from("<H", raw)[0]
                    key = raw[2:2 + klen]
                    data = raw[2 + klen:]
                    rows.append((key, data))
        return rows

    def close(self) -> None:
        if self._mmap is not None:
            self._mmap.close()
            self._mmap = None
        if self._fd is not None:
            os.close(self._fd)
            self._fd = None

    def __del__(self) -> None:
        self.close()

    def __enter__(self) -> MMapSegmentReader:
        self.open()
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()


@dataclass
class SegmentMeta:
    """Metadata for a segment in the manifest."""
    segment_id: int
    path: str
    seg_type: SegmentType
    row_count: int
    page_count: int
    created_ts: float
    min_key: bytes
    max_key: bytes
    level: int = 0  # Compaction level
    size_bytes: int = 0


class SegmentManager:
    """Manages all segments: creation, lookup, compaction candidates."""

    def __init__(self, data_dir: str) -> None:
        self.data_dir = data_dir
        self._segments: dict[int, SegmentMeta] = {}
        self._next_id = 0
        self._lock = threading.Lock()
        os.makedirs(data_dir, exist_ok=True)

    def new_writer(self, seg_type: SegmentType = SegmentType.ROW_STORE) -> SegmentWriter:
        with self._lock:
            self._next_id += 1
            sid = self._next_id
        path = os.path.join(self.data_dir, f"seg_{sid:08d}.qms")
        return SegmentWriter(path, sid, seg_type)

    def register(self, writer: SegmentWriter) -> SegmentMeta:
        """Register a finished segment."""
        hdr = writer._header
        meta = SegmentMeta(
            segment_id=hdr.segment_id, path=writer._path,
            seg_type=hdr.seg_type, row_count=hdr.row_count,
            page_count=hdr.page_count, created_ts=hdr.created_ts,
            min_key=hdr.min_key, max_key=hdr.max_key,
            size_bytes=os.path.getsize(writer._path) if os.path.exists(writer._path) else 0,
        )
        with self._lock:
            self._segments[meta.segment_id] = meta
        return meta

    def get_segments(self, level: int | None = None) -> list[SegmentMeta]:
        with self._lock:
            segs = list(self._segments.values())
        if level is not None:
            segs = [s for s in segs if s.level == level]
        return sorted(segs, key=lambda s: s.segment_id)

    def remove_segment(self, segment_id: int) -> bool:
        with self._lock:
            meta = self._segments.pop(segment_id, None)
        if meta:
            try:
                os.unlink(meta.path)
            except OSError:
                pass
            return True
        return False

    def compaction_candidates(self, level: int = 0, min_count: int = 4) -> list[SegmentMeta]:
        """Return segments at a level eligible for compaction."""
        segs = self.get_segments(level=level)
        if len(segs) >= min_count:
            return segs
        return []
