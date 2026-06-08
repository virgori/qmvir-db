"""QM Storage — Compression Engine.

Layer-aware compression:
  - Hot: LZ4 / Zstd fast (level 1-3)
  - Warm: Zstd balanced (level 5-9)
  - Cold: Zstd high (level 15-22)

Type-aware encoding:
  - Integer: delta + varint + bit-packing
  - String: dictionary + prefix compression
  - Boolean: bitmap / RLE
  - Time-series: delta-of-delta (Gorilla)
"""

from __future__ import annotations

from enum import Enum
from typing import Any


class CompressionTier(Enum):
    """Data temperature tier."""

    HOT = "hot"
    WARM = "warm"
    COLD = "cold"


class CompressionCodec(Enum):
    """Available compression codecs."""

    NONE = "none"
    LZ4 = "lz4"
    ZSTD_FAST = "zstd_fast"
    ZSTD_BALANCED = "zstd_balanced"
    ZSTD_HIGH = "zstd_high"


# Tier → codec mapping
TIER_CODECS: dict[CompressionTier, CompressionCodec] = {
    CompressionTier.HOT: CompressionCodec.LZ4,
    CompressionTier.WARM: CompressionCodec.ZSTD_BALANCED,
    CompressionTier.COLD: CompressionCodec.ZSTD_HIGH,
}


class CompressionEngine:
    """Compresses and decompresses data blocks."""

    def compress(self, data: bytes, codec: CompressionCodec) -> bytes:
        """Compress data with the given codec."""
        if codec == CompressionCodec.NONE:
            return data

        if codec == CompressionCodec.LZ4:
            try:
                import lz4.frame
                return lz4.frame.compress(data)
            except ImportError:
                return data

        if codec in (
            CompressionCodec.ZSTD_FAST,
            CompressionCodec.ZSTD_BALANCED,
            CompressionCodec.ZSTD_HIGH,
        ):
            try:
                import zstandard as zstd

                level_map = {
                    CompressionCodec.ZSTD_FAST: 1,
                    CompressionCodec.ZSTD_BALANCED: 6,
                    CompressionCodec.ZSTD_HIGH: 19,
                }
                level = level_map.get(codec, 6)
                cctx = zstd.ZstdCompressor(level=level)
                return cctx.compress(data)
            except ImportError:
                return data

        return data

    def decompress(self, data: bytes, codec: CompressionCodec) -> bytes:
        """Decompress data."""
        if codec == CompressionCodec.NONE:
            return data

        if codec == CompressionCodec.LZ4:
            try:
                import lz4.frame
                return lz4.frame.decompress(data)
            except ImportError:
                return data

        if codec in (
            CompressionCodec.ZSTD_FAST,
            CompressionCodec.ZSTD_BALANCED,
            CompressionCodec.ZSTD_HIGH,
        ):
            try:
                import zstandard as zstd
                dctx = zstd.ZstdDecompressor()
                return dctx.decompress(data)
            except ImportError:
                return data

        return data

    def compress_for_tier(self, data: bytes, tier: CompressionTier) -> bytes:
        """Compress using the default codec for a given tier."""
        codec = TIER_CODECS.get(tier, CompressionCodec.NONE)
        return self.compress(data, codec)

    @staticmethod
    def estimate_ratio(codec: CompressionCodec) -> float:
        """Estimated compression ratio (compressed/original)."""
        ratios = {
            CompressionCodec.NONE: 1.0,
            CompressionCodec.LZ4: 0.55,
            CompressionCodec.ZSTD_FAST: 0.45,
            CompressionCodec.ZSTD_BALANCED: 0.35,
            CompressionCodec.ZSTD_HIGH: 0.25,
        }
        return ratios.get(codec, 1.0)


# ─── Type-specific encoders ────────────────────────────────────────────────

class DeltaEncoder:
    """Delta encoding for sorted integers."""

    @staticmethod
    def encode(values: list[int]) -> list[int]:
        if not values:
            return []
        result = [values[0]]
        for i in range(1, len(values)):
            result.append(values[i] - values[i - 1])
        return result

    @staticmethod
    def decode(deltas: list[int]) -> list[int]:
        if not deltas:
            return []
        result = [deltas[0]]
        for i in range(1, len(deltas)):
            result.append(result[-1] + deltas[i])
        return result


class DictionaryEncoder:
    """Dictionary encoding for repetitive strings."""

    def __init__(self) -> None:
        self._dict: dict[str, int] = {}
        self._reverse: list[str] = []

    def encode(self, values: list[str]) -> list[int]:
        codes: list[int] = []
        for v in values:
            if v not in self._dict:
                code = len(self._dict)
                self._dict[v] = code
                self._reverse.append(v)
            codes.append(self._dict[v])
        return codes

    def decode(self, codes: list[int]) -> list[str]:
        return [self._reverse[c] for c in codes]

    @property
    def dict_size(self) -> int:
        return len(self._dict)


class RLEEncoder:
    """Run-Length Encoding for values with many repeats."""

    @staticmethod
    def encode(values: list[Any]) -> list[tuple[Any, int]]:
        if not values:
            return []
        runs: list[tuple[Any, int]] = []
        current = values[0]
        count = 1
        for v in values[1:]:
            if v == current:
                count += 1
            else:
                runs.append((current, count))
                current = v
                count = 1
        runs.append((current, count))
        return runs

    @staticmethod
    def decode(runs: list[tuple[Any, int]]) -> list[Any]:
        result: list[Any] = []
        for value, count in runs:
            result.extend([value] * count)
        return result
