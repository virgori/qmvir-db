"""QM IPC — Shared Memory Slab Allocator for Media Blobs.

Large media payloads (images, video, audio) cannot fit through the
64 KB ring-buffer slots.  Instead…

    1. Hub allocates a *slab* from this shared-memory heap.
    2. Producer writes raw bytes directly into the slab (zero-copy).
    3. Hub sends the slab **offset + length** over the ring buffer.
    4. MediaSatellite maps the same region and reads in-place.

Slab geometry (power-of-2 size classes):
    Class 0 :  64 KB   — thumbnails, metadata JSON
    Class 1 :   1 MB   — compressed photos, short audio
    Class 2 :   8 MB   — high-res images, video segments
    Class 3 :  64 MB   — raw video frames, uncompressed media

Each size class maintains a free-list of pre-carved blocks inside
one contiguous mmap region.  Allocation is O(1) pop from free-list;
deallocation is O(1) push.

Thread safety: A per-class spinlock (1-byte atomic in mmap) guards
the free-list head pointer so Hub threads can allocate concurrently.

File layout (one file per size class):
    ┌──────────────────────────────────────────────────┐
    │ MagicHeader (32 B) │ Bitmap (ceil(N/8) B)        │
    ├──────────────────────────────────────────────────┤
    │ Slab[0]             … slot_size bytes …          │
    │ Slab[1]             …                            │
    │  …                                               │
    │ Slab[N-1]                                        │
    └──────────────────────────────────────────────────┘
"""

from __future__ import annotations

import mmap
import os
import struct
import tempfile
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

try:
    import qm_engine as _qm_engine
except ImportError:
    _qm_engine = None

# ── Constants ────────────────────────────────────────────────────────

_META_FMT = "<QQqq"  # magic(8) + slab_size(8) + slab_count(8) + free_count(8)
_META_SIZE = struct.calcsize(_META_FMT)  # 32 bytes
_MAGIC = 0x514D_534C_4142_4846  # "QMSLABHF"

# Default size classes (bytes)
DEFAULT_SIZE_CLASSES: list[int] = [
    64 * 1024,       # 64 KB
    1 * 1024 * 1024, # 1 MB
    8 * 1024 * 1024, # 8 MB
    64 * 1024 * 1024, # 64 MB
]

# Default slab counts per class
DEFAULT_SLAB_COUNTS: list[int] = [256, 64, 16, 4]


# ── Slab Handle ──────────────────────────────────────────────────────

@dataclass(frozen=True, slots=True)
class SlabHandle:
    """Opaque handle returned on allocation.

    Encodes (class_index, slab_index, offset, size) so the receiver
    can locate the slab in shared memory without any IPC round-trip.
    """
    class_idx: int     # size-class index
    slab_idx: int      # index within that class
    offset: int        # byte offset from start of mmap file
    size: int          # usable slab size

    def to_bytes(self) -> bytes:
        """Pack into 32 bytes for ring-buffer payload."""
        return struct.pack("<IIQQ", self.class_idx, self.slab_idx,
                           self.offset, self.size)

    @classmethod
    def from_bytes(cls, data: bytes) -> "SlabHandle":
        ci, si, off, sz = struct.unpack("<IIQQ", data[:24])
        return cls(class_idx=ci, slab_idx=si, offset=off, size=sz)

    def to_bytes_full(self) -> bytes:
        return struct.pack("<IIQQ", self.class_idx, self.slab_idx,
                           self.offset, self.size)


# ── Single Size-Class Pool ──────────────────────────────────────────

class _SlabPool:
    """Manages one mmap-backed pool of identically-sized slabs.

    Parameters
    ----------
    path : str
        File backing the mmap.
    slab_size : int
        Bytes per slab (power of 2).
    slab_count : int
        Number of slabs in this pool.
    create : bool
        True = initialise a new pool.  False = attach to existing.
    """

    def __init__(self, path: str, slab_size: int, slab_count: int,
                 create: bool = True) -> None:
        self._path = path
        self._slab_size = slab_size
        self._slab_count = slab_count
        self._lock = threading.Lock()

        # Bitmap occupies ceil(slab_count / 8) bytes
        self._bitmap_size = (slab_count + 7) // 8
        self._data_offset = _META_SIZE + self._bitmap_size
        # Align data offset to page boundary (4 KB)
        self._data_offset = (self._data_offset + 4095) & ~4095

        total_size = self._data_offset + slab_count * slab_size

        # FIX Bug 1.6: In-memory free-list for O(1) allocate/free.
        # Built from bitmap on attach, maintained in sync.
        self._free_list: list[int] = []

        if create:
            with open(path, "wb") as f:
                f.truncate(total_size)
            self._fd = os.open(path, os.O_RDWR)
            self._mm = mmap.mmap(self._fd, total_size)
            self._write_meta(slab_count)
        else:
            self._fd = os.open(path, os.O_RDWR)
            self._mm = mmap.mmap(self._fd, 0)
            self._validate_meta()

        # Populate free-list from persistent bitmap (handles crash-recovery)
        for idx in range(self._slab_count):
            if not self._bit_get(idx):
                self._free_list.append(idx)

    # ── Metadata ────────────────────────────────────────────────────

    def _write_meta(self, free_count: int) -> None:
        data = struct.pack(_META_FMT, _MAGIC, self._slab_size,
                           self._slab_count, free_count)
        self._mm[0:_META_SIZE] = data

    def _read_free_count(self) -> int:
        _, _, _, fc = struct.unpack(_META_FMT, self._mm[0:_META_SIZE])
        return fc

    def _set_free_count(self, fc: int) -> None:
        struct.pack_into("<q", self._mm, 24, fc)

    def _validate_meta(self) -> None:
        magic, ss, sc, _ = struct.unpack(_META_FMT, self._mm[0:_META_SIZE])
        if magic != _MAGIC:
            raise ValueError("Invalid slab pool magic")
        if ss != self._slab_size or sc != self._slab_count:
            raise ValueError(
                f"Slab pool mismatch: expected {self._slab_size}×{self._slab_count}, "
                f"got {ss}×{sc}"
            )

    # ── Bitmap operations ───────────────────────────────────────────

    def _bit_get(self, idx: int) -> bool:
        """Return True if slab[idx] is allocated."""
        byte_off = _META_SIZE + (idx >> 3)
        bit_mask = 1 << (idx & 7)
        return bool(self._mm[byte_off] & bit_mask)

    def _bit_set(self, idx: int) -> None:
        """Mark slab[idx] as allocated."""
        byte_off = _META_SIZE + (idx >> 3)
        bit_mask = 1 << (idx & 7)
        self._mm[byte_off] = self._mm[byte_off] | bit_mask

    def _bit_clear(self, idx: int) -> None:
        """Mark slab[idx] as free."""
        byte_off = _META_SIZE + (idx >> 3)
        bit_mask = 1 << (idx & 7)
        self._mm[byte_off] = self._mm[byte_off] & (~bit_mask & 0xFF)

    # ── Allocate / Free ─────────────────────────────────────────────

    def allocate(self) -> Optional[int]:
        """Allocate one slab.  O(1) via in-memory free-list."""
        with self._lock:
            if not self._free_list:
                return None
            idx = self._free_list.pop()
            self._bit_set(idx)
            fc = self._read_free_count() - 1
            self._set_free_count(fc)
            return idx

    def free(self, idx: int) -> None:
        """Return a slab to the pool."""
        if idx < 0 or idx >= self._slab_count:
            raise ValueError(f"Slab index {idx} out of range [0, {self._slab_count})")
        with self._lock:
            if not self._bit_get(idx):
                return  # already free — idempotent
            self._bit_clear(idx)
            fc = self._read_free_count() + 1
            self._set_free_count(fc)
            self._free_list.append(idx)

    # ── Data access (zero-copy) ─────────────────────────────────────

    def slab_offset(self, idx: int) -> int:
        """Byte offset of slab[idx] data region within the mmap."""
        return self._data_offset + idx * self._slab_size

    def write(self, idx: int, data: bytes | memoryview, offset: int = 0) -> int:
        """Write data into slab[idx] at internal offset.  Returns bytes written."""
        soff = self.slab_offset(idx)
        nbytes = min(len(data), self._slab_size - offset)
        self._mm[soff + offset : soff + offset + nbytes] = bytes(data[:nbytes])
        return nbytes

    def read(self, idx: int, length: int, offset: int = 0) -> bytes:
        """Read bytes from slab[idx]."""
        soff = self.slab_offset(idx)
        return bytes(self._mm[soff + offset : soff + offset + length])

    def memoryview_of(self, idx: int) -> memoryview:
        """Return a zero-copy memoryview into slab[idx]."""
        soff = self.slab_offset(idx)
        return self._mm[soff : soff + self._slab_size]

    # ── Diagnostics ─────────────────────────────────────────────────

    @property
    def path(self) -> str:
        return self._path

    @property
    def slab_size(self) -> int:
        return self._slab_size

    @property
    def slab_count(self) -> int:
        return self._slab_count

    @property
    def free_count(self) -> int:
        return self._read_free_count()

    @property
    def used_count(self) -> int:
        return self._slab_count - self._read_free_count()

    def stats(self) -> dict:
        return {
            "slab_size": self._slab_size,
            "slab_count": self._slab_count,
            "free": self.free_count,
            "used": self.used_count,
        }

    def close(self) -> None:
        """Unmap and close the file."""
        if self._mm:
            self._mm.close()
            self._mm = None
        if self._fd is not None:
            os.close(self._fd)
            self._fd = None


# ── Multi-Class Media Allocator ──────────────────────────────────────

@dataclass
class MediaAllocatorConfig:
    """Configuration for the media slab allocator."""
    heap_dir: str | None = None
    size_classes: list[int] = field(default_factory=lambda: list(DEFAULT_SIZE_CLASSES))
    slab_counts: list[int] = field(default_factory=lambda: list(DEFAULT_SLAB_COUNTS))


class MediaSlabAllocator:
    """Multi-class shared-memory slab allocator for media blobs.

    Automatically picks the smallest size class that fits the
    requested allocation.

    Usage (Hub side):
        alloc = MediaSlabAllocator(config)
        handle = alloc.allocate(len(image_bytes))
        alloc.write(handle, image_bytes)
        # Send handle.to_bytes() over ring buffer (32 bytes)

    Usage (Satellite side):
        alloc = MediaSlabAllocator.attach(config)
        data  = alloc.read(handle, actual_length)
    """

    def __init__(self, config: MediaAllocatorConfig | None = None,
                 create: bool = True) -> None:
        self._config = config or MediaAllocatorConfig()

        heap_dir = self._config.heap_dir
        if heap_dir is None:
            heap_dir = tempfile.mkdtemp(prefix="qm_media_heap_")
        self._heap_dir = heap_dir
        os.makedirs(heap_dir, exist_ok=True)

        if len(self._config.size_classes) != len(self._config.slab_counts):
            raise ValueError("size_classes and slab_counts must have same length")

        self._pools: list[_SlabPool] = []
        for i, (sz, cnt) in enumerate(zip(self._config.size_classes,
                                           self._config.slab_counts)):
            path = os.path.join(heap_dir, f"slab_class_{i}_{sz}.heap")
            pool = _SlabPool(path=path, slab_size=sz, slab_count=cnt,
                             create=create)
            self._pools.append(pool)

    @classmethod
    def attach(cls, config: MediaAllocatorConfig) -> "MediaSlabAllocator":
        """Attach to an existing media heap (satellite side)."""
        return cls(config, create=False)

    # ── Allocation ──────────────────────────────────────────────────

    def allocate(self, size: int) -> SlabHandle:
        """Allocate the smallest slab that fits *size* bytes.

        Raises RuntimeError if no suitable slab is available.
        """
        for class_idx, pool in enumerate(self._pools):
            if pool.slab_size >= size:
                slab_idx = pool.allocate()
                if slab_idx is not None:
                    return SlabHandle(
                        class_idx=class_idx,
                        slab_idx=slab_idx,
                        offset=pool.slab_offset(slab_idx),
                        size=pool.slab_size,
                    )
        raise RuntimeError(
            f"No slab available for {size} bytes — "
            f"classes exhausted: {[p.stats() for p in self._pools]}"
        )

    def free(self, handle: SlabHandle) -> None:
        """Return a slab to its pool."""
        self._pools[handle.class_idx].free(handle.slab_idx)

    # ── Data access ─────────────────────────────────────────────────

    def write(self, handle: SlabHandle, data: bytes | memoryview,
              offset: int = 0) -> int:
        """Write media bytes into the allocated slab (zero-copy path)."""
        return self._pools[handle.class_idx].write(
            handle.slab_idx, data, offset)

    def read(self, handle: SlabHandle, length: int,
             offset: int = 0) -> bytes:
        """Read media bytes from a slab."""
        return self._pools[handle.class_idx].read(
            handle.slab_idx, length, offset)

    def memoryview_of(self, handle: SlabHandle) -> memoryview:
        """Zero-copy memoryview into the slab."""
        return self._pools[handle.class_idx].memoryview_of(handle.slab_idx)

    def resolve_mref(self, handle: SlabHandle) -> dict:
        """Resolve a SlabHandle into a descriptor dict (for MREF queries).

        Returns metadata about the slab including its location and state.
        """
        pool = self._pools[handle.class_idx]
        return {
            "class_idx": handle.class_idx,
            "slab_idx": handle.slab_idx,
            "offset": handle.offset,
            "slab_size": handle.size,
            "pool_path": pool.path,
            "allocated": pool._bit_get(handle.slab_idx),
        }

    # ── Diagnostics ─────────────────────────────────────────────────

    @property
    def heap_dir(self) -> str:
        return self._heap_dir

    def stats(self) -> list[dict]:
        """Per-class usage stats."""
        return [p.stats() for p in self._pools]

    def total_allocated_bytes(self) -> int:
        """Total bytes currently allocated across all classes."""
        return sum(p.used_count * p.slab_size for p in self._pools)

    def total_capacity_bytes(self) -> int:
        """Total capacity across all classes."""
        return sum(p.slab_count * p.slab_size for p in self._pools)

    def close(self) -> None:
        """Unmap and close all pools."""
        for pool in self._pools:
            pool.close()
        self._pools.clear()
