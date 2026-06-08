"""QM Vector Platform — Vector quantization for memory reduction.

Supports:
  - fp16 (half precision)
  - int8 (scalar quantization)
  - Product Quantization (PQ)
  - Scalar Quantization (SQ)
"""

from __future__ import annotations

import numpy as np
from enum import Enum


class QuantizationType(Enum):
    FP32 = "fp32"
    FP16 = "fp16"
    INT8 = "int8"
    PRODUCT = "product"  # Product Quantization
    SCALAR = "scalar"  # Scalar Quantization


class VectorQuantizer:
    """Quantizes vectors to reduce memory footprint."""

    _BYTES_PER_ELEMENT = {
        QuantizationType.FP32: 4,
        QuantizationType.FP16: 2,
        QuantizationType.INT8: 1,
        QuantizationType.PRODUCT: 1,  # approximate code byte per dimension
        QuantizationType.SCALAR: 1,
    }

    _DTYPE_ALIASES = {
        "fp32": QuantizationType.FP32,
        "float32": QuantizationType.FP32,
        "f32": QuantizationType.FP32,
        "fp16": QuantizationType.FP16,
        "float16": QuantizationType.FP16,
        "f16": QuantizationType.FP16,
        "int8": QuantizationType.INT8,
        "uint8": QuantizationType.INT8,
        "i8": QuantizationType.INT8,
        "product": QuantizationType.PRODUCT,
        "pq": QuantizationType.PRODUCT,
        "scalar": QuantizationType.SCALAR,
        "sq": QuantizationType.SCALAR,
    }

    @staticmethod
    def _normalize_dtype(dtype: QuantizationType | str) -> QuantizationType:
        if isinstance(dtype, QuantizationType):
            return dtype
        key = str(dtype).lower().strip()
        if key in VectorQuantizer._DTYPE_ALIASES:
            return VectorQuantizer._DTYPE_ALIASES[key]
        try:
            return QuantizationType(key)
        except ValueError as exc:
            raise ValueError(f"Unsupported quantization dtype: {dtype}") from exc

    @staticmethod
    def _bytes_per_element(dtype: QuantizationType | str) -> int:
        return VectorQuantizer._BYTES_PER_ELEMENT[VectorQuantizer._normalize_dtype(dtype)]

    @staticmethod
    def _validate_vectors(vectors: np.ndarray, label: str) -> np.ndarray:
        arr = np.asarray(vectors)
        if arr.size == 0:
            raise ValueError(f"{label} vectors must not be empty")
        if not np.issubdtype(arr.dtype, np.number):
            raise ValueError(f"{label} vectors must be numeric")
        if not np.all(np.isfinite(arr)):
            raise ValueError(f"{label} vectors must contain only finite values")
        return arr

    @staticmethod
    def to_fp16(vectors: np.ndarray) -> np.ndarray:
        """Convert fp32 to fp16."""
        return VectorQuantizer._validate_vectors(vectors, "fp16 input").astype(np.float16)

    @staticmethod
    def from_fp16(vectors: np.ndarray) -> np.ndarray:
        """Convert fp16 back to fp32."""
        return VectorQuantizer._validate_vectors(vectors, "fp16 encoded").astype(np.float32)

    @staticmethod
    def to_int8(vectors: np.ndarray) -> tuple[np.ndarray, dict[str, float]]:
        """Scalar quantization to int8. Returns (quantized, params)."""
        vectors = VectorQuantizer._validate_vectors(vectors, "int8 input").astype(np.float32, copy=False)
        vmin = float(vectors.min())
        vmax = float(vectors.max())
        scale = (vmax - vmin) / 255.0 if vmax != vmin else 1.0
        offset = vmin
        quantized = np.clip(
            np.round((vectors - offset) / scale), 0, 255
        ).astype(np.uint8)
        return quantized, {"scale": scale, "offset": offset}

    @staticmethod
    def from_int8(
        quantized: np.ndarray, params: dict[str, float] | float = 0.0, offset: float | None = None,
    ) -> np.ndarray:
        """Dequantize int8 back to fp32."""
        if np.asarray(quantized).size == 0:
            raise ValueError("int8 encoded vectors must not be empty")
        if isinstance(params, dict):
            scale = params["scale"]
            offset = params["offset"]
            if not np.isfinite(scale) or not np.isfinite(offset):
                raise ValueError("int8 quantization params must be finite")
            return quantized.astype(np.float32) * scale + offset
        # Legacy: from_int8(quantized, scale, offset)
        if not np.isfinite(params) or not np.isfinite(offset if offset is not None else 0.0):
            raise ValueError("int8 quantization params must be finite")
        return quantized.astype(np.float32) * params + (offset if offset is not None else 0.0)

    @staticmethod
    def memory_usage(n_vectors: int, dimension: int, dtype: QuantizationType | str) -> int:
        """Estimate memory in bytes."""
        if n_vectors < 0 or dimension < 0:
            raise ValueError("n_vectors and dimension must be non-negative")
        return n_vectors * dimension * VectorQuantizer._bytes_per_element(dtype)

    @staticmethod
    def compression_ratio(original: QuantizationType | str, target: QuantizationType | str) -> float:
        """Compute compression ratio."""
        return VectorQuantizer._bytes_per_element(original) / VectorQuantizer._bytes_per_element(target)
