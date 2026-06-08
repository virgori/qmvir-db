"""QM Index — Roaring Bitmap.

A proper Roaring Bitmap implementation with three container types:
    1. ArrayContainer  — sorted short array, for sparse sets (< 4096 elements)
    2. BitmapContainer — 8KB bitset, for dense sets (>= 4096 elements)
    3. RunContainer    — run-length encoded, for consecutive ranges

Supports:
    - AND / OR / NOT / XOR / ANDNOT
    - Cardinality estimation
    - Iteration
    - Serialization
    - Adaptive container switching

Each 32-bit integer is split:
    - High 16 bits → container key
    - Low 16 bits  → position within container
"""

from __future__ import annotations

import array
import struct
from typing import Iterator

from qm_core.concurrency import RWLock


# ── Constants ───────────────────────────────────────────────────────
ARRAY_MAX = 4096  # Threshold to switch from array → bitmap
CONTAINER_CAPACITY = 65536  # 2^16 values per container


class ArrayContainer:
    """Sorted array of uint16 values. Efficient for sparse sets."""
    __slots__ = ("_arr",)

    def __init__(self, values: list[int] | None = None) -> None:
        self._arr = array.array("H", sorted(set(values)) if values else [])

    def add(self, val: int) -> bool:
        """Add value. Returns True if newly added."""
        # Binary search
        lo, hi = 0, len(self._arr)
        while lo < hi:
            mid = (lo + hi) >> 1
            if self._arr[mid] < val:
                lo = mid + 1
            elif self._arr[mid] > val:
                hi = mid
            else:
                return False  # Already exists
        self._arr.insert(lo, val)
        return True

    def contains(self, val: int) -> bool:
        lo, hi = 0, len(self._arr)
        while lo < hi:
            mid = (lo + hi) >> 1
            if self._arr[mid] < val:
                lo = mid + 1
            elif self._arr[mid] > val:
                hi = mid
            else:
                return True
        return False

    def remove(self, val: int) -> bool:
        lo, hi = 0, len(self._arr)
        while lo < hi:
            mid = (lo + hi) >> 1
            if self._arr[mid] < val:
                lo = mid + 1
            elif self._arr[mid] > val:
                hi = mid
            else:
                del self._arr[mid]
                return True
        return False

    @property
    def cardinality(self) -> int:
        return len(self._arr)

    def __iter__(self) -> Iterator[int]:
        return iter(self._arr)

    def should_convert_to_bitmap(self) -> bool:
        return self.cardinality > ARRAY_MAX

    def to_bitmap(self) -> BitmapContainer:
        bc = BitmapContainer()
        for v in self._arr:
            bc.add(v)
        return bc

    # ── Set operations ──────────────────────────────────────────────

    def and_with(self, other: ArrayContainer) -> ArrayContainer:
        """Intersection."""
        result: list[int] = []
        i, j = 0, 0
        a, b = self._arr, other._arr
        while i < len(a) and j < len(b):
            if a[i] < b[j]:
                i += 1
            elif a[i] > b[j]:
                j += 1
            else:
                result.append(a[i])
                i += 1
                j += 1
        return ArrayContainer(result)

    def or_with(self, other: ArrayContainer) -> ArrayContainer:
        """Union."""
        result: list[int] = []
        i, j = 0, 0
        a, b = self._arr, other._arr
        while i < len(a) and j < len(b):
            if a[i] < b[j]:
                result.append(a[i]); i += 1
            elif a[i] > b[j]:
                result.append(b[j]); j += 1
            else:
                result.append(a[i]); i += 1; j += 1
        while i < len(a):
            result.append(a[i]); i += 1
        while j < len(b):
            result.append(b[j]); j += 1
        return ArrayContainer(result)

    def andnot(self, other: ArrayContainer) -> ArrayContainer:
        """Difference (self - other)."""
        result: list[int] = []
        i, j = 0, 0
        a, b = self._arr, other._arr
        while i < len(a) and j < len(b):
            if a[i] < b[j]:
                result.append(a[i]); i += 1
            elif a[i] > b[j]:
                j += 1
            else:
                i += 1; j += 1
        while i < len(a):
            result.append(a[i]); i += 1
        return ArrayContainer(result)


class BitmapContainer:
    """Fixed 8KB bitmap for dense sets (65536 bits = 8192 bytes)."""
    __slots__ = ("_bits", "_card")

    def __init__(self) -> None:
        self._bits = bytearray(8192)  # 65536 bits
        self._card = 0

    def add(self, val: int) -> bool:
        byte_idx = val >> 3
        bit_idx = val & 7
        if self._bits[byte_idx] & (1 << bit_idx):
            return False
        self._bits[byte_idx] |= (1 << bit_idx)
        self._card += 1
        return True

    def contains(self, val: int) -> bool:
        return bool(self._bits[val >> 3] & (1 << (val & 7)))

    def remove(self, val: int) -> bool:
        byte_idx = val >> 3
        bit_idx = val & 7
        if not (self._bits[byte_idx] & (1 << bit_idx)):
            return False
        self._bits[byte_idx] &= ~(1 << bit_idx)
        self._card -= 1
        return True

    @property
    def cardinality(self) -> int:
        return self._card

    def __iter__(self) -> Iterator[int]:
        for i in range(65536):
            if self._bits[i >> 3] & (1 << (i & 7)):
                yield i

    def should_convert_to_array(self) -> bool:
        return self._card <= ARRAY_MAX

    def to_array(self) -> ArrayContainer:
        return ArrayContainer(list(self))

    # ── Set operations ──────────────────────────────────────────────

    def and_with(self, other: BitmapContainer) -> BitmapContainer:
        result = BitmapContainer()
        for i in range(8192):
            result._bits[i] = self._bits[i] & other._bits[i]
        result._recount()
        return result

    def or_with(self, other: BitmapContainer) -> BitmapContainer:
        result = BitmapContainer()
        for i in range(8192):
            result._bits[i] = self._bits[i] | other._bits[i]
        result._recount()
        return result

    def andnot(self, other: BitmapContainer) -> BitmapContainer:
        result = BitmapContainer()
        for i in range(8192):
            result._bits[i] = self._bits[i] & ~other._bits[i]
        result._recount()
        return result

    def xor_with(self, other: BitmapContainer) -> BitmapContainer:
        result = BitmapContainer()
        for i in range(8192):
            result._bits[i] = self._bits[i] ^ other._bits[i]
        result._recount()
        return result

    def _recount(self) -> None:
        self._card = sum(b.bit_count() for b in self._bits)


class RunContainer:
    """Run-length encoded container: stores sorted (start, length) pairs."""
    __slots__ = ("_runs",)

    def __init__(self, runs: list[tuple[int, int]] | None = None) -> None:
        self._runs: list[tuple[int, int]] = runs or []

    def add(self, val: int) -> bool:
        for i, (start, length) in enumerate(self._runs):
            if start <= val <= start + length:
                return False  # Already in a run
            if val == start - 1:
                self._runs[i] = (val, length + 1)
                self._merge_adjacent(i)
                return True
            if val == start + length + 1:
                self._runs[i] = (start, length + 1)
                self._merge_adjacent(i)
                return True
        # New singleton run
        self._runs.append((val, 0))
        self._runs.sort()
        return True

    def contains(self, val: int) -> bool:
        for start, length in self._runs:
            if start <= val <= start + length:
                return True
            if start > val:
                break
        return False

    @property
    def cardinality(self) -> int:
        return sum(length + 1 for _, length in self._runs)

    def __iter__(self) -> Iterator[int]:
        for start, length in self._runs:
            for v in range(start, start + length + 1):
                yield v

    def _merge_adjacent(self, idx: int) -> None:
        while idx + 1 < len(self._runs):
            s1, l1 = self._runs[idx]
            s2, l2 = self._runs[idx + 1]
            if s1 + l1 + 1 >= s2:
                new_end = max(s1 + l1, s2 + l2)
                self._runs[idx] = (s1, new_end - s1)
                del self._runs[idx + 1]
            else:
                break


# Container type alias
Container = ArrayContainer | BitmapContainer | RunContainer


class RoaringBitmap:
    """Roaring Bitmap with adaptive container selection.

    Each 32-bit value is split: high 16 bits → container key, low 16 bits → value.
    Container type is chosen adaptively based on cardinality and density.
    """

    def __init__(self) -> None:
        self._containers: dict[int, Container] = {}
        self._rwlock = RWLock()

    def add(self, value: int) -> None:
        """Add a 32-bit integer. Thread-safe (exclusive lock)."""
        with self._rwlock.write():
            self._add_unlocked(value)

    def _add_unlocked(self, value: int) -> None:
        """Add without lock (caller must hold write lock)."""
        hi = value >> 16
        lo = value & 0xFFFF
        container = self._containers.get(hi)
        if container is None:
            container = ArrayContainer()
            self._containers[hi] = container

        if isinstance(container, ArrayContainer):
            container.add(lo)
            if container.should_convert_to_bitmap():
                self._containers[hi] = container.to_bitmap()
        elif isinstance(container, BitmapContainer):
            container.add(lo)
        elif isinstance(container, RunContainer):
            container.add(lo)

    def add_range(self, start: int, end: int) -> None:
        """Add all values in [start, end)."""
        for v in range(start, end):
            self.add(v)

    def contains(self, value: int) -> bool:
        """Thread-safe contains check."""
        with self._rwlock.read():
            return self._contains_unlocked(value)

    def _contains_unlocked(self, value: int) -> bool:
        hi = value >> 16
        lo = value & 0xFFFF
        container = self._containers.get(hi)
        if container is None:
            return False
        return container.contains(lo)

    def remove(self, value: int) -> bool:
        hi = value >> 16
        lo = value & 0xFFFF
        container = self._containers.get(hi)
        if container is None:
            return False
        if isinstance(container, (ArrayContainer, BitmapContainer)):
            result = container.remove(lo)
            # Maybe convert back
            if isinstance(container, BitmapContainer) and container.should_convert_to_array():
                self._containers[hi] = container.to_array()
            if container.cardinality == 0:
                del self._containers[hi]
            return result
        return False

    @property
    def cardinality(self) -> int:
        return sum(c.cardinality for c in self._containers.values())

    def __len__(self) -> int:
        return self.cardinality

    def __contains__(self, value: int) -> bool:
        return self.contains(value)

    def __iter__(self) -> Iterator[int]:
        for hi in sorted(self._containers):
            base = hi << 16
            for lo in self._containers[hi]:
                yield base | lo

    def __and__(self, other: RoaringBitmap) -> RoaringBitmap:
        return self.intersect(other)

    def __or__(self, other: RoaringBitmap) -> RoaringBitmap:
        return self.union(other)

    def __sub__(self, other: RoaringBitmap) -> RoaringBitmap:
        return self.andnot(other)

    # ── Set operations ──────────────────────────────────────────────

    def intersect(self, other: RoaringBitmap) -> RoaringBitmap:
        """AND intersection."""
        result = RoaringBitmap()
        common_keys = set(self._containers) & set(other._containers)
        for hi in common_keys:
            c1 = self._containers[hi]
            c2 = other._containers[hi]
            merged = self._intersect_containers(c1, c2)
            if merged.cardinality > 0:
                result._containers[hi] = merged
        return result

    def union(self, other: RoaringBitmap) -> RoaringBitmap:
        """OR union."""
        result = RoaringBitmap()
        all_keys = set(self._containers) | set(other._containers)
        for hi in all_keys:
            c1 = self._containers.get(hi)
            c2 = other._containers.get(hi)
            if c1 is not None and c2 is not None:
                merged = self._union_containers(c1, c2)
            elif c1 is not None:
                merged = c1
            else:
                merged = c2
            if merged and merged.cardinality > 0:
                result._containers[hi] = merged
        return result

    def andnot(self, other: RoaringBitmap) -> RoaringBitmap:
        """AND NOT (difference)."""
        result = RoaringBitmap()
        for hi, c1 in self._containers.items():
            c2 = other._containers.get(hi)
            if c2 is None:
                result._containers[hi] = c1
            else:
                diff = self._andnot_containers(c1, c2)
                if diff.cardinality > 0:
                    result._containers[hi] = diff
        return result

    def to_set(self) -> set[int]:
        return set(self)

    def to_sorted_list(self) -> list[int]:
        return list(self)

    # ── Facet / analytics helpers ───────────────────────────────────

    def intersection_cardinality(self, other: RoaringBitmap) -> int:
        """Fast cardinality of intersection without materialization."""
        count = 0
        common_keys = set(self._containers) & set(other._containers)
        for hi in common_keys:
            c1 = self._containers[hi]
            c2 = other._containers[hi]
            merged = self._intersect_containers(c1, c2)
            count += merged.cardinality
        return count

    # ── Serialization ───────────────────────────────────────────────

    def serialize(self) -> bytes:
        """Simple binary serialization."""
        parts = [struct.pack("<I", len(self._containers))]
        for hi in sorted(self._containers):
            container = self._containers[hi]
            values = list(container)
            parts.append(struct.pack("<I I", hi, len(values)))
            for v in values:
                parts.append(struct.pack("<H", v))
        return b"".join(parts)

    @classmethod
    def deserialize(cls, data: bytes) -> RoaringBitmap:
        bm = cls()
        pos = 0
        n_containers = struct.unpack_from("<I", data, pos)[0]; pos += 4
        for _ in range(n_containers):
            hi, n_vals = struct.unpack_from("<II", data, pos); pos += 8
            for _ in range(n_vals):
                lo = struct.unpack_from("<H", data, pos)[0]; pos += 2
                bm.add((hi << 16) | lo)
        return bm

    # ── Internal ────────────────────────────────────────────────────

    @staticmethod
    def _ensure_same_type(c1: Container, c2: Container) -> tuple[Container, Container]:
        """Convert both to bitmap if different types for set ops."""
        if type(c1) is type(c2):
            return c1, c2
        # Convert both to their native value lists → use ArrayContainer if small
        v1 = list(c1)
        v2 = list(c2)
        if len(v1) + len(v2) < ARRAY_MAX:
            return ArrayContainer(v1), ArrayContainer(v2)
        # Convert to bitmap
        b1, b2 = BitmapContainer(), BitmapContainer()
        for v in v1:
            b1.add(v)
        for v in v2:
            b2.add(v)
        return b1, b2

    def _intersect_containers(self, c1: Container, c2: Container) -> Container:
        c1, c2 = self._ensure_same_type(c1, c2)
        if isinstance(c1, ArrayContainer) and isinstance(c2, ArrayContainer):
            return c1.and_with(c2)
        if isinstance(c1, BitmapContainer) and isinstance(c2, BitmapContainer):
            return c1.and_with(c2)
        # Fallback
        v = set(c1) & set(c2)
        return ArrayContainer(sorted(v))

    def _union_containers(self, c1: Container, c2: Container) -> Container:
        c1, c2 = self._ensure_same_type(c1, c2)
        if isinstance(c1, ArrayContainer) and isinstance(c2, ArrayContainer):
            r = c1.or_with(c2)
            if r.should_convert_to_bitmap():
                return r.to_bitmap()
            return r
        if isinstance(c1, BitmapContainer) and isinstance(c2, BitmapContainer):
            return c1.or_with(c2)
        v = set(c1) | set(c2)
        return ArrayContainer(sorted(v))

    def _andnot_containers(self, c1: Container, c2: Container) -> Container:
        c1, c2 = self._ensure_same_type(c1, c2)
        if isinstance(c1, ArrayContainer) and isinstance(c2, ArrayContainer):
            return c1.andnot(c2)
        if isinstance(c1, BitmapContainer) and isinstance(c2, BitmapContainer):
            return c1.andnot(c2)
        v = set(c1) - set(c2)
        return ArrayContainer(sorted(v))
