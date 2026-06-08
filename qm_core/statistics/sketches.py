"""QM Statistics — Probabilistic Sketches.

Streaming data structures for approximate statistics:
    - HyperLogLog: cardinality estimation (how many distinct values?)
    - Count-Min Sketch: frequency estimation (how often does X appear?)
    - T-Digest: quantile estimation (what's the p99 latency?)

These are essential for:
    - Cardinality estimation in the query planner
    - Frequency-based filtering (MCV — most common values)
    - Histogram bucket boundary selection
    - Adaptive query budgets
"""

from __future__ import annotations

import hashlib
import math
import struct
from dataclasses import dataclass, field
from typing import Any


# ── HyperLogLog ─────────────────────────────────────────────────────


class HyperLogLog:
    """HyperLogLog cardinality estimator.

    Space: O(m) registers where m = 2^p.
    Error: ~1.04 / sqrt(m).

    Standard choices:
        p=10 → 1024 registers → ~3.25% error
        p=14 → 16384 registers → ~0.81% error (Redis default)
        p=16 → 65536 registers → ~0.41% error
    """

    def __init__(self, precision: int = 14) -> None:
        self.p = precision
        self.m = 1 << precision
        self.registers = bytearray(self.m)
        self._alpha = self._compute_alpha(self.m)

    @staticmethod
    def _compute_alpha(m: int) -> float:
        if m == 16:
            return 0.673
        elif m == 32:
            return 0.697
        elif m == 64:
            return 0.709
        else:
            return 0.7213 / (1.0 + 1.079 / m)

    def _hash(self, value: Any) -> int:
        """Hash value to 64-bit integer."""
        h = hashlib.sha256(str(value).encode()).digest()
        return struct.unpack("<Q", h[:8])[0]

    def add(self, value: Any) -> None:
        """Add a value to the sketch."""
        x = self._hash(value)
        # First p bits → register index
        idx = x & (self.m - 1)
        # Remaining bits → count leading zeros
        w = x >> self.p
        rho = self._leading_zeros(w) + 1
        if rho > self.registers[idx]:
            self.registers[idx] = rho

    @staticmethod
    def _leading_zeros(w: int) -> int:
        """Count leading zeros in 64-bit value (after removing p bits)."""
        if w == 0:
            return 50  # Cap at 50
        n = 0
        # Check high bits first
        for shift in (32, 16, 8, 4, 2, 1):
            if w >> shift == 0:
                n += shift
                w <<= shift
            if n > 50:
                return 50
        return n

    def estimate(self) -> int:
        """Estimate cardinality."""
        # Raw HLL estimate
        sum_inv = sum(2.0 ** (-r) for r in self.registers)
        raw_est = self._alpha * self.m * self.m / sum_inv

        # Small range correction
        if raw_est <= 2.5 * self.m:
            zeros = self.registers.count(0)
            if zeros > 0:
                return int(self.m * math.log(self.m / zeros))

        # Large range correction (for 64-bit hashes, not needed below 2^32)
        if raw_est > (1 << 32) / 30.0:
            raw_est = -(1 << 64) * math.log(1.0 - raw_est / (1 << 64))

        return int(raw_est)

    def merge(self, other: "HyperLogLog") -> "HyperLogLog":
        """Merge two HLL sketches (union)."""
        if self.p != other.p:
            raise ValueError("Cannot merge HLLs with different precision")
        result = HyperLogLog(self.p)
        for i in range(self.m):
            result.registers[i] = max(self.registers[i], other.registers[i])
        return result

    def sizeof(self) -> int:
        """Memory usage in bytes."""
        return self.m + 32  # registers + overhead

    def error_rate(self) -> float:
        """Expected relative error."""
        return 1.04 / math.sqrt(self.m)


# ── Count-Min Sketch ────────────────────────────────────────────────


class CountMinSketch:
    """Count-Min Sketch for frequency estimation.

    Space: O(w * d) counters.
    Error: with probability ≥ 1 - δ, estimate ≤ true_count + ε * N
           where ε = e/w, δ = e^(-d), N = total insertions.

    Standard choices:
        width=2048, depth=5 → ε ≈ 0.0013, δ ≈ 0.007 (~99.3% confidence)
    """

    def __init__(self, width: int = 2048, depth: int = 5) -> None:
        self.w = width
        self.d = depth
        self.table = [[0] * width for _ in range(depth)]
        self.total = 0
        # Use different seeds for each hash function
        self._seeds = [i * 0xDEADBEEF + 0xCAFEBABE for i in range(depth)]

    def _hash(self, value: Any, seed: int) -> int:
        h = hashlib.md5((str(seed) + str(value)).encode()).digest()
        return struct.unpack("<I", h[:4])[0] % self.w

    def add(self, value: Any, count: int = 1) -> None:
        """Add value with optional count."""
        self.total += count
        for i in range(self.d):
            j = self._hash(value, self._seeds[i])
            self.table[i][j] += count

    def estimate(self, value: Any) -> int:
        """Estimate frequency of value (always ≥ true count)."""
        return min(
            self.table[i][self._hash(value, self._seeds[i])]
            for i in range(self.d)
        )

    def inner_product(self, other: "CountMinSketch") -> int:
        """Estimate inner product (useful for join size estimation)."""
        if self.w != other.w or self.d != other.d:
            raise ValueError("Incompatible sketches")
        return min(
            sum(self.table[i][j] * other.table[i][j] for j in range(self.w))
            for i in range(self.d)
        )

    def merge(self, other: "CountMinSketch") -> "CountMinSketch":
        """Merge (union) two sketches."""
        if self.w != other.w or self.d != other.d:
            raise ValueError("Incompatible sketches")
        result = CountMinSketch(self.w, self.d)
        result.total = self.total + other.total
        for i in range(self.d):
            for j in range(self.w):
                result.table[i][j] = self.table[i][j] + other.table[i][j]
        return result

    def sizeof(self) -> int:
        """Memory usage in bytes."""
        return self.w * self.d * 8 + 64


# ── T-Digest ────────────────────────────────────────────────────────


@dataclass
class Centroid:
    """A centroid in the t-digest."""
    mean: float
    weight: float

    def merge(self, other: "Centroid") -> "Centroid":
        total_w = self.weight + other.weight
        new_mean = (self.mean * self.weight + other.mean * other.weight) / total_w
        return Centroid(mean=new_mean, weight=total_w)


class TDigest:
    """T-Digest for streaming quantile estimation.

    Provides accurate estimates especially at the tails (p1, p5, p95, p99).
    Compression parameter δ controls accuracy vs. space:
        - δ=100 (default): ~15 bytes/centroid, typical 200-300 centroids
        - δ=200: more centroids, more accurate

    Key property: centroids near q=0 and q=1 are small → tail accuracy.
    """

    def __init__(self, compression: float = 100.0) -> None:
        self.compression = compression
        self.centroids: list[Centroid] = []
        self._buffer: list[float] = []
        self._buffer_limit = max(int(compression * 5), 500)
        self.total_weight: float = 0.0
        self.min_val: float = float('inf')
        self.max_val: float = float('-inf')

    def add(self, value: float, weight: float = 1.0) -> None:
        """Add a value."""
        self._buffer.append(value)
        self.total_weight += weight
        self.min_val = min(self.min_val, value)
        self.max_val = max(self.max_val, value)
        if len(self._buffer) >= self._buffer_limit:
            self._flush()

    def _flush(self) -> None:
        """Merge buffer into centroids."""
        if not self._buffer:
            return
        # Convert buffer to centroids
        new_centroids = [Centroid(mean=v, weight=1.0) for v in self._buffer]
        self._buffer.clear()
        all_centroids = sorted(self.centroids + new_centroids, key=lambda c: c.mean)
        self.centroids = self._compress(all_centroids)

    def _compress(self, sorted_centroids: list[Centroid]) -> list[Centroid]:
        """Compress centroids respecting the scale function."""
        if not sorted_centroids:
            return []
        total_w = sum(c.weight for c in sorted_centroids)
        result: list[Centroid] = [sorted_centroids[0]]
        weight_so_far = sorted_centroids[0].weight

        for c in sorted_centroids[1:]:
            q = weight_so_far / total_w
            # Scale function: k(q) = δ/(2π) * arcsin(2q - 1)
            # Max weight at quantile q: 4 * total_w * q * (1-q) / δ
            max_weight = max(1.0, 4.0 * total_w * q * (1.0 - q) / self.compression)

            if result[-1].weight + c.weight <= max_weight:
                result[-1] = result[-1].merge(c)
            else:
                result.append(c)
            weight_so_far += c.weight

        return result

    def quantile(self, q: float) -> float:
        """Estimate the value at quantile q ∈ [0, 1]."""
        self._flush()
        if not self.centroids:
            return 0.0
        if q <= 0:
            return self.min_val
        if q >= 1:
            return self.max_val

        target_weight = q * self.total_weight
        cumulative = 0.0

        for i, c in enumerate(self.centroids):
            if cumulative + c.weight >= target_weight:
                # Interpolate within the centroid
                if i == 0:
                    return self._interpolate_left(c, target_weight - cumulative)
                prev = self.centroids[i - 1]
                local_q = (target_weight - cumulative) / c.weight
                return prev.mean + (c.mean - prev.mean) * local_q
            cumulative += c.weight

        return self.max_val

    def _interpolate_left(self, c: Centroid, offset: float) -> float:
        """Interpolate near the left tail."""
        if c.weight <= 1:
            return c.mean
        local_q = offset / c.weight
        return self.min_val + (c.mean - self.min_val) * local_q

    def cdf(self, value: float) -> float:
        """Estimate the CDF: P(X ≤ value)."""
        self._flush()
        if not self.centroids:
            return 0.0
        if value <= self.min_val:
            return 0.0
        if value >= self.max_val:
            return 1.0

        cumulative = 0.0
        for i, c in enumerate(self.centroids):
            if c.mean >= value:
                if i == 0:
                    return (value - self.min_val) / (c.mean - self.min_val + 1e-15) * c.weight / (2 * self.total_weight)
                prev = self.centroids[i - 1]
                frac = (value - prev.mean) / (c.mean - prev.mean + 1e-15)
                return (cumulative + frac * c.weight) / self.total_weight
            cumulative += c.weight

        return 1.0

    def percentile(self, p: float) -> float:
        """Estimate the p-th percentile (p ∈ [0, 100])."""
        return self.quantile(p / 100.0)

    def trimmed_mean(self, lo: float = 0.05, hi: float = 0.95) -> float:
        """Trimmed mean between quantiles lo and hi."""
        self._flush()
        if not self.centroids:
            return 0.0
        lo_val = self.quantile(lo)
        hi_val = self.quantile(hi)
        total_w = 0.0
        total_sum = 0.0
        for c in self.centroids:
            if lo_val <= c.mean <= hi_val:
                total_sum += c.mean * c.weight
                total_w += c.weight
        return total_sum / total_w if total_w > 0 else 0.0

    def merge(self, other: "TDigest") -> "TDigest":
        """Merge two t-digests."""
        result = TDigest(self.compression)
        result.total_weight = self.total_weight + other.total_weight
        result.min_val = min(self.min_val, other.min_val)
        result.max_val = max(self.max_val, other.max_val)
        all_centroids = sorted(self.centroids + other.centroids, key=lambda c: c.mean)
        result.centroids = result._compress(all_centroids)
        return result

    def sizeof(self) -> int:
        """Memory usage in bytes."""
        return len(self.centroids) * 16 + 64

    def summary(self) -> dict[str, float]:
        """Quick summary statistics."""
        return {
            "count": self.total_weight,
            "min": self.min_val,
            "max": self.max_val,
            "p50": self.quantile(0.5),
            "p90": self.quantile(0.90),
            "p95": self.quantile(0.95),
            "p99": self.quantile(0.99),
            "p999": self.quantile(0.999),
        }


# ── Bloom Filter ────────────────────────────────────────────────────


class BloomFilter:
    """Standard Bloom filter for set membership.

    False positive rate ≈ (1 - e^(-k*n/m))^k
    Optimal k = m/n * ln(2)
    """

    def __init__(self, expected_items: int = 10000, fp_rate: float = 0.01) -> None:
        self.n = expected_items
        self.fp_rate = fp_rate
        # Optimal size
        self.m = max(64, int(-expected_items * math.log(fp_rate) / (math.log(2) ** 2)))
        self.k = max(1, int(self.m / expected_items * math.log(2)))
        self._bits = bytearray((self.m + 7) // 8)
        self._count = 0

    def _hashes(self, value: Any) -> list[int]:
        """Generate k hash positions using double hashing."""
        h = hashlib.sha256(str(value).encode()).digest()
        h1 = struct.unpack("<Q", h[:8])[0]
        h2 = struct.unpack("<Q", h[8:16])[0]
        return [(h1 + i * h2) % self.m for i in range(self.k)]

    def add(self, value: Any) -> None:
        """Add value to the filter."""
        for pos in self._hashes(value):
            self._bits[pos >> 3] |= (1 << (pos & 7))
        self._count += 1

    def __contains__(self, value: Any) -> bool:
        """Check if value might be in the set."""
        for pos in self._hashes(value):
            if not (self._bits[pos >> 3] & (1 << (pos & 7))):
                return False
        return True

    def sizeof(self) -> int:
        return len(self._bits) + 64

    @property
    def count(self) -> int:
        return self._count


# ── Convenience: Sketch Collection ──────────────────────────────────


@dataclass
class ColumnSketch:
    """All sketches for a single column."""
    hll: HyperLogLog = field(default_factory=lambda: HyperLogLog(14))
    cms: CountMinSketch = field(default_factory=lambda: CountMinSketch(2048, 5))
    digest: TDigest = field(default_factory=lambda: TDigest(100))
    bloom: BloomFilter = field(default_factory=lambda: BloomFilter(10000))
    is_numeric: bool = False

    def add(self, value: Any) -> None:
        """Ingest one value into all sketches."""
        if value is None:
            return
        self.hll.add(value)
        self.cms.add(value)
        self.bloom.add(value)
        if isinstance(value, (int, float)):
            self.digest.add(float(value))
            self.is_numeric = True

    def estimated_distinct(self) -> int:
        return self.hll.estimate()

    def estimated_frequency(self, value: Any) -> int:
        return self.cms.estimate(value)

    def estimated_quantile(self, q: float) -> float:
        return self.digest.quantile(q)

    def may_contain(self, value: Any) -> bool:
        return value in self.bloom

    def sizeof(self) -> int:
        return self.hll.sizeof() + self.cms.sizeof() + self.digest.sizeof() + self.bloom.sizeof()
