"""QM Storage — Page Buffer Pool with LRU Eviction.

Features:
    - Fixed-size buffer pool (configurable page count)
    - LRU eviction with dirty-page write-back
    - Pin/unpin semantics for concurrency safety
    - Read-ahead / prefetch support
    - Page-level locking
    - Dirty page tracking for checkpoint
"""

from __future__ import annotations

import threading
import time
from collections import OrderedDict
from dataclasses import dataclass, field
from typing import Any, Callable


@dataclass(slots=True)
class BufferFrame:
    """A frame in the buffer pool holding one page."""
    page_id: int = -1
    segment_id: int = -1
    data: bytes = b""
    is_dirty: bool = False
    pin_count: int = 0
    last_access: float = 0.0
    access_count: int = 0

    @property
    def is_pinned(self) -> bool:
        return self.pin_count > 0


class BufferPool:
    """LRU buffer pool for page-level I/O.

    The page_reader callback reads a page from storage:
        page_reader(segment_id, page_id) -> bytes

    The page_writer callback writes a dirty page back:
        page_writer(segment_id, page_id, data) -> None
    """

    def __init__(
        self,
        capacity: int = 1024,
        page_reader: Callable[[int, int], bytes] | None = None,
        page_writer: Callable[[int, int, bytes], None] | None = None,
    ) -> None:
        self._capacity = capacity
        self._reader = page_reader
        self._writer = page_writer
        self._frames: OrderedDict[tuple[int, int], BufferFrame] = OrderedDict()
        self._lock = threading.Lock()
        self._stats = {"hits": 0, "misses": 0, "evictions": 0, "writes": 0}

    def fetch_page(self, segment_id: int, page_id: int) -> bytes:
        """Fetch a page, loading from disk if needed. Auto-pins the page."""
        key = (segment_id, page_id)
        with self._lock:
            frame = self._frames.get(key)
            if frame is not None:
                frame.pin_count += 1
                frame.last_access = time.time()
                frame.access_count += 1
                self._frames.move_to_end(key)
                self._stats["hits"] += 1
                return frame.data
            # Cache miss — load from storage
            self._stats["misses"] += 1

        # Read outside lock to avoid holding it during I/O
        data = self._read_page(segment_id, page_id)

        with self._lock:
            # Double-check
            if key in self._frames:
                frame = self._frames[key]
                frame.pin_count += 1
                return frame.data
            # Evict if needed
            self._evict_if_needed()
            frame = BufferFrame(
                page_id=page_id, segment_id=segment_id,
                data=data, pin_count=1,
                last_access=time.time(), access_count=1,
            )
            self._frames[key] = frame
            return data

    def unpin(self, segment_id: int, page_id: int, dirty: bool = False) -> None:
        """Unpin a page. Mark dirty if modified."""
        key = (segment_id, page_id)
        with self._lock:
            frame = self._frames.get(key)
            if frame and frame.pin_count > 0:
                frame.pin_count -= 1
                if dirty:
                    frame.is_dirty = True

    def mark_dirty(self, segment_id: int, page_id: int, new_data: bytes | None = None) -> None:
        """Mark a page as dirty, optionally updating its data."""
        key = (segment_id, page_id)
        with self._lock:
            frame = self._frames.get(key)
            if frame:
                frame.is_dirty = True
                if new_data is not None:
                    frame.data = new_data

    def flush_dirty(self) -> int:
        """Write all dirty pages back to storage. Returns count flushed."""
        dirty: list[tuple[int, int, bytes]] = []
        with self._lock:
            for (seg_id, pg_id), frame in self._frames.items():
                if frame.is_dirty:
                    dirty.append((seg_id, pg_id, frame.data))
                    frame.is_dirty = False
        for seg_id, pg_id, data in dirty:
            self._write_page(seg_id, pg_id, data)
            self._stats["writes"] += 1
        return len(dirty)

    def prefetch(self, pages: list[tuple[int, int]]) -> None:
        """Prefetch pages into buffer pool (read-ahead)."""
        for seg_id, pg_id in pages:
            key = (seg_id, pg_id)
            with self._lock:
                if key in self._frames:
                    continue
            # Load in background
            try:
                data = self._read_page(seg_id, pg_id)
                with self._lock:
                    if key not in self._frames:
                        self._evict_if_needed()
                        self._frames[key] = BufferFrame(
                            page_id=pg_id, segment_id=seg_id,
                            data=data, last_access=time.time(),
                        )
            except Exception:
                pass

    def invalidate(self, segment_id: int, page_id: int) -> None:
        """Remove a page from the pool, writing back if dirty."""
        key = (segment_id, page_id)
        with self._lock:
            frame = self._frames.pop(key, None)
        if frame and frame.is_dirty:
            self._write_page(segment_id, page_id, frame.data)

    def clear(self) -> int:
        """Flush all dirty pages and clear the pool."""
        count = self.flush_dirty()
        with self._lock:
            self._frames.clear()
        return count

    @property
    def size(self) -> int:
        return len(self._frames)

    @property
    def stats(self) -> dict[str, int]:
        return dict(self._stats)

    @property
    def dirty_count(self) -> int:
        with self._lock:
            return sum(1 for f in self._frames.values() if f.is_dirty)

    def _evict_if_needed(self) -> None:
        """Evict unpinned pages using clock-sweep until under capacity.

        DEADLOCK FIX: dirty pages are collected and written OUTSIDE the lock.
        The caller must hold self._lock. This method may temporarily release
        and re-acquire it to flush dirty pages without blocking readers.
        """
        while len(self._frames) >= self._capacity:
            # --- Clock-sweep: find an unpinned victim ---
            victim_key = None
            victim_dirty: tuple[int, int, bytes] | None = None

            for key in list(self._frames.keys()):
                frame = self._frames[key]
                if not frame.is_pinned:
                    if frame.access_count > 0:
                        # Give a second chance
                        frame.access_count = 0
                        self._frames.move_to_end(key)
                        continue
                    victim_key = key
                    if frame.is_dirty:
                        victim_dirty = (frame.segment_id, frame.page_id, frame.data)
                    break

            if victim_key is None:
                break  # All pages pinned — cannot evict

            # Remove from pool first
            del self._frames[victim_key]
            self._stats["evictions"] += 1

            # Flush dirty page OUTSIDE the lock to prevent deadlock
            if victim_dirty:
                self._lock.release()
                try:
                    self._write_page(*victim_dirty)
                    self._stats["writes"] += 1
                finally:
                    self._lock.acquire()

    def _read_page(self, segment_id: int, page_id: int) -> bytes:
        if self._reader:
            return self._reader(segment_id, page_id)
        return b"\x00" * 8192

    def _write_page(self, segment_id: int, page_id: int, data: bytes) -> None:
        if self._writer:
            self._writer(segment_id, page_id, data)
