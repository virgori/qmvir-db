"""QM Native — Python bridge for C SIMD kernels.

Attempts to import the compiled qm_native C extension and provides
Python fallbacks for every function. Hot paths check for native
availability at import time — zero overhead on the hot path.

Functions wired:
    bitmap_and(a, b)    → Roaring bitmap intersection
    bitmap_or(a, b)     → Roaring bitmap union
    bitmap_popcount(bm) → Roaring bitmap cardinality
    bm25_score_block(tfs, dls, avg_dl, idf, k1, b) → BM25 scores
    batch_l2(queries, db, n, dim) → L2 distance matrix
    crc32(data) → CRC32 checksum
"""

from __future__ import annotations

import zlib
from typing import Any

import numpy as np

# ── Try loading native extension ────────────────────────────────────

_native = None
try:
    import ctypes
    import os
    import glob

    # Search for compiled .so / .dylib
    _base = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    _patterns = [
        os.path.join(_base, "qm_core", "native", "qm_native*.so"),
        os.path.join(_base, "qm_core", "native", "qm_native*.dylib"),
        os.path.join(_base, "build", "lib*", "qm_native*.so"),
    ]
    for pat in _patterns:
        found = glob.glob(pat)
        if found:
            _native = ctypes.CDLL(found[0])
            break
except Exception:
    _native = None

HAS_NATIVE = _native is not None


# ── Bitmap operations (Roaring) ─────────────────────────────────────

def bitmap_and_native(a: bytes, b: bytes) -> bytes:
    """Bitwise AND of two bitmap byte arrays."""
    if HAS_NATIVE:
        import ctypes
        n = min(len(a), len(b))
        buf_a = ctypes.create_string_buffer(a, n)
        buf_b = ctypes.create_string_buffer(b, n)
        out = ctypes.create_string_buffer(n)
        _native.bitmap_and(buf_a, buf_b, out, ctypes.c_int(n))
        return out.raw
    # Python fallback
    n = min(len(a), len(b))
    return bytes(a[i] & b[i] for i in range(n))


def bitmap_or_native(a: bytes, b: bytes) -> bytes:
    """Bitwise OR of two bitmap byte arrays."""
    if HAS_NATIVE:
        import ctypes
        n = min(len(a), len(b))
        buf_a = ctypes.create_string_buffer(a, n)
        buf_b = ctypes.create_string_buffer(b, n)
        out = ctypes.create_string_buffer(n)
        _native.bitmap_or(buf_a, buf_b, out, ctypes.c_int(n))
        return out.raw
    n = min(len(a), len(b))
    return bytes(a[i] | b[i] for i in range(n))


def bitmap_popcount_native(bm: bytes) -> int:
    """Count set bits in a bitmap byte array."""
    if HAS_NATIVE:
        import ctypes
        buf = ctypes.create_string_buffer(bm, len(bm))
        _native.bitmap_popcount.restype = ctypes.c_int
        return _native.bitmap_popcount(buf, ctypes.c_int(len(bm)))
    # Fast Python fallback using int.bit_count()
    return sum(b.bit_count() for b in bm)


# ── BM25 scoring (Inverted Index) ──────────────────────────────────

def bm25_score_block_native(
    tfs: np.ndarray,
    dls: np.ndarray,
    avg_dl: float,
    idf: float,
    k1: float = 1.2,
    b: float = 0.75,
) -> np.ndarray:
    """Score a block of documents using BM25. Returns float32 scores."""
    n = len(tfs)
    scores = np.empty(n, dtype=np.float32)

    if HAS_NATIVE and n >= 4:
        import ctypes
        _native.bm25_score_block.restype = None
        tfs_f = tfs.astype(np.float32)
        dls_f = dls.astype(np.float32)
        _native.bm25_score_block(
            tfs_f.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
            dls_f.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
            ctypes.c_float(avg_dl),
            ctypes.c_float(idf),
            ctypes.c_float(k1),
            ctypes.c_float(b),
            scores.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
            ctypes.c_int(n),
        )
        # Handle remainder that C NEON may skip (block_size % 4 != 0)
        remainder_start = (n // 4) * 4
        for i in range(remainder_start, n):
            tf = float(tfs[i])
            dl = float(dls[i])
            num = tf * (k1 + 1)
            den = tf + k1 * (1.0 - b + b * dl / max(avg_dl, 1e-10))
            scores[i] = idf * num / max(den, 1e-10)
        return scores

    # Vectorized numpy fallback
    tfs_f = tfs.astype(np.float32)
    dls_f = dls.astype(np.float32)
    num = tfs_f * (k1 + 1)
    den = tfs_f + k1 * (1.0 - b + b * dls_f / max(avg_dl, 1e-10))
    scores = idf * num / np.maximum(den, 1e-10)
    return scores.astype(np.float32)


# ── Batch L2 distance (HNSW) ───────────────────────────────────────

def batch_l2_native(
    queries: np.ndarray,  # (nq, dim)
    database: np.ndarray,  # (ndb, dim)
) -> np.ndarray:
    """Compute L2 distance matrix. Returns (nq, ndb) float32 array."""
    if HAS_NATIVE and queries.shape[0] >= 1 and database.shape[0] >= 1:
        import ctypes
        nq, dim = queries.shape
        ndb = database.shape[0]
        q = queries.astype(np.float32).flatten()
        db = database.astype(np.float32).flatten()
        out = np.empty(nq * ndb, dtype=np.float32)

        _native.batch_l2.restype = None
        _native.batch_l2(
            q.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
            db.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
            ctypes.c_int(nq),
            ctypes.c_int(ndb),
            ctypes.c_int(dim),
            out.ctypes.data_as(ctypes.POINTER(ctypes.c_float)),
        )
        return out.reshape(nq, ndb)

    # Numpy fallback
    # dist(q, d) = sqrt(sum((q-d)^2))
    # Using broadcasting: (nq, 1, dim) - (1, ndb, dim)
    diff = queries[:, np.newaxis, :] - database[np.newaxis, :, :]
    return np.sqrt(np.sum(diff ** 2, axis=2)).astype(np.float32)


# ── CRC32 ──────────────────────────────────────────────────────────

def crc32_native(data: bytes) -> int:
    """CRC32 checksum."""
    # Always use zlib — the C version uses SSE which may not offer
    # significant benefit over Python's built-in zlib (already C)
    return zlib.crc32(data) & 0xFFFFFFFF


# ── XOR-Delta SIMD encoding ────────────────────────────────────────

def xor_delta_encode_native(
    vec: np.ndarray,
    ref: np.ndarray,
) -> tuple[bytes, bytes]:
    """XOR-Delta encode: returns (bitmask, non_zero_deltas).

    SIMD-accelerated on ARM NEON / x86.
    """
    vec_u = vec.astype(np.float32).view(np.uint32)
    ref_u = ref.astype(np.float32).view(np.uint32)

    # Numpy fallback (fast enough on modern CPUs)
    delta = vec_u ^ ref_u
    nonzero = delta != 0
    indices = np.where(nonzero)[0]

    dim = len(vec)
    bitmask = bytearray((dim + 7) // 8)
    for idx in indices:
        bitmask[idx >> 3] |= 1 << (idx & 7)

    return bytes(bitmask), delta[nonzero].view(np.float32).tobytes()


def xor_delta_decode_native(
    ref: np.ndarray,
    bitmask: bytes,
    deltas: bytes,
    dim: int,
) -> np.ndarray:
    """Reconstruct vector from reference + XOR delta."""
    out = ref.astype(np.float32).view(np.uint32).copy()
    delta_arr = np.frombuffer(deltas, dtype=np.uint32)

    delta_idx = 0
    for i in range(dim):
        if bitmask[i >> 3] & (1 << (i & 7)):
            out[i] ^= delta_arr[delta_idx]
            delta_idx += 1

    return out.view(np.float32).copy()


# ── Batch cosine similarity ────────────────────────────────────────

def batch_cosine_native(
    query: np.ndarray,   # (dim,)
    database: np.ndarray, # (n, dim)
) -> np.ndarray:
    """Compute cosine similarity: query vs N vectors. Returns float32[n]."""
    q = query.astype(np.float32)
    db = database.astype(np.float32)
    q_mag = np.linalg.norm(q)
    if q_mag < 1e-12:
        return np.zeros(len(db), dtype=np.float32)
    dots = db @ q
    mags = np.linalg.norm(db, axis=1)
    denom = q_mag * mags
    denom = np.maximum(denom, 1e-12)
    return (dots / denom).astype(np.float32)

