"""QM Vector Satellite — Professional vector compute/storage node.

Handles:
 - HNSW index (in-RAM for fast ANN search)
 - DiskANN (vectors on SSD, loaded on demand for re-ranking)
 - XOR-Delta lossless compression for vector storage (PRIMARY)
 - SIMD-accelerated distance computation

Architecture compliance:
 - Lossless First: XOR-Delta is the primary compression path
 - PQ (lossy) is ONLY used as an optional draft/approximation layer
 - All distance computation uses exact vectors (lossless decoded)
"""

from __future__ import annotations

import hashlib
import os
import struct
import numpy as np
from typing import Any, Optional

from qm_core.ipc.ring_buffer import CommandType
from qm_core.hub.lsn_sequencer import LSNStamp
from qm_core.satellite.base import Satellite, SatelliteConfig
from qm_core.compression import XORDeltaCodec, XORDeltaBatchCodec

# Try to use native implementations
try:
    from qm_core.native_adapter import (
        is_native_available,
        get_hnsw_index,
        get_xor_delta_codec,
        get_batch_distance_fn,
    )
    _USE_NATIVE = is_native_available()
except ImportError:
    _USE_NATIVE = False


# ── Vector command sub-types (packed in payload) ─────────────────

class VecOp:
    INSERT  = 1  # Insert vector + metadata
    SEARCH  = 2  # ANN search
    DELETE  = 3  # Remove vector by ID
    UPDATE  = 4  # Update vector
    COMPRESS = 5  # Trigger compression

# ── Wire format helpers ─────────────────────────────────────────

def _pack_vector(vec_id: int, vector: np.ndarray, meta: bytes = b"") -> bytes:
    """Pack a vector for IPC: op(1) + id(8) + dim(4) + float32[] + meta_len(4) + meta."""
    dim = len(vector)
    parts = [
        struct.pack("<BqI", VecOp.INSERT, vec_id, dim),
        vector.astype(np.float32).tobytes(),
        struct.pack("<I", len(meta)),
        meta,
    ]
    return b"".join(parts)


def _pack_search(query: np.ndarray, top_k: int = 10) -> bytes:
    """Pack a search request."""
    dim = len(query)
    return struct.pack("<BIH", VecOp.SEARCH, dim, top_k) + query.astype(np.float32).tobytes()


def _unpack_vec_op(payload: bytes) -> tuple[int, bytes]:
    """Extract (op_code, rest) from payload."""
    return payload[0], payload[1:]


class VectorSatellite(Satellite):
    """Satellite specialized for vector storage and HNSW search.

    On-disk layout (data_dir/):
        vectors/      — raw float32 vectors (or XOR-Delta compressed)
        hnsw_state/   — serialized HNSW graph
        diskann/      — DiskANN SSD index
    """

    def __init__(self, config: SatelliteConfig, ring, dim: int = 128):
        super().__init__(config, ring)
        self._dim = dim

        # In-memory storage (production would use mmap)
        self._vectors: dict[int, np.ndarray] = {}
        self._metadata: dict[int, bytes] = {}

        # Lossless XOR-Delta compression (PRIMARY path)
        if _USE_NATIVE:
            self._xor_codec = get_xor_delta_codec(dim)
        else:
            self._xor_codec = XORDeltaCodec(dim)
        self._compressed_store: dict[int, bytes] = {}  # vec_id → compressed bytes
        self._compression_enabled = True

        # HNSW index - use native Rust implementation if available
        self._hnsw = None
        if _USE_NATIVE:
            self._hnsw = get_hnsw_index(dim=dim, metric="cosine", M=16, ef_construction=200)
        else:
            try:
                from qm_core.index.hnsw import HNSWIndex
                self._hnsw = HNSWIndex(dim=dim)
            except Exception:
                pass

        # DiskANN storage path
        self._disk_path = os.path.join(config.data_dir, "vectors")
        os.makedirs(self._disk_path, exist_ok=True)

    def _execute_command(
        self,
        stamp: LSNStamp,
        cmd: CommandType,
        payload: bytes,
    ) -> bytes:
        if cmd == CommandType.VECTOR_OP:
            return self._handle_vec_op(stamp, payload)
        elif cmd == CommandType.INSERT:
            return self._handle_insert(stamp, payload)
        elif cmd == CommandType.QUERY:
            return self._handle_query(stamp, payload)
        elif cmd == CommandType.DELETE:
            return self._handle_delete(stamp, payload)
        else:
            return b"OK"

    def _handle_vec_op(self, stamp: LSNStamp, payload: bytes) -> bytes:
        op = payload[0]
        rest = payload[1:]

        if op == VecOp.INSERT:
            return self._vec_insert(stamp, rest)
        elif op == VecOp.SEARCH:
            return self._vec_search(rest)
        elif op == VecOp.DELETE:
            vec_id = struct.unpack("<q", rest[:8])[0]
            self._vectors.pop(vec_id, None)
            self._metadata.pop(vec_id, None)
            return b"DELETED"
        else:
            return b"UNKNOWN_VEC_OP"

    def _vec_insert(self, stamp: LSNStamp, data: bytes) -> bytes:
        """Parse and store a vector."""
        vec_id, dim = struct.unpack("<qI", data[:12])
        vec_bytes = data[12 : 12 + dim * 4]
        vector = np.frombuffer(vec_bytes, dtype=np.float32).copy()

        meta_off = 12 + dim * 4
        meta_len = struct.unpack("<I", data[meta_off : meta_off + 4])[0]
        meta = data[meta_off + 4 : meta_off + 4 + meta_len]

        self._vectors[vec_id] = vector
        self._metadata[vec_id] = meta

        # XOR-Delta lossless compression (PRIMARY storage path)
        if self._compression_enabled:
            compressed = self._xor_codec.encode(vector)
            self._compressed_store[vec_id] = compressed

        # Add to HNSW index (uses exact vectors — lossless)
        if self._hnsw is not None:
            self._hnsw.add(vec_id, vector)

        # Write to DiskANN storage (append to file)
        disk_file = os.path.join(self._disk_path, f"{vec_id}.vec")
        with open(disk_file, "wb") as f:
            f.write(vector.tobytes())

        return struct.pack("<q", vec_id)  # return the ID

    def _vec_search(self, data: bytes) -> bytes:
        """ANN search using HNSW."""
        dim, top_k = struct.unpack("<IH", data[:6])
        query = np.frombuffer(data[6 : 6 + dim * 4], dtype=np.float32)

        if self._hnsw is not None and len(self._vectors) > 0:
            hnsw_results = self._hnsw.search(query, top_k=min(top_k, len(self._vectors)))
            # Handle both native (dict) and Python (VectorResult) formats
            if _USE_NATIVE:
                results = [(r["id"], r["distance"]) for r in hnsw_results]
            else:
                results = [(r.id, r.distance) for r in hnsw_results]
        else:
            # Brute-force fallback
            results = self._brute_search(query, top_k)

        # Pack results: count(4) + [id(8) + dist(4)] * count
        parts = [struct.pack("<I", len(results))]
        for vid, dist in results:
            parts.append(struct.pack("<qf", vid, dist))
        return b"".join(parts)

    def _brute_search(self, query: np.ndarray, top_k: int) -> list[tuple[int, float]]:
        """Brute-force search (fallback when HNSW not available).
        
        Uses native batch distance computation if available.
        """
        if not self._vectors:
            return []
        
        vids = list(self._vectors.keys())
        vectors = np.array([self._vectors[vid] for vid in vids], dtype=np.float32)
        
        # Use native batch distance if available
        if _USE_NATIVE and len(vectors) > 100:
            batch_dist = get_batch_distance_fn("cosine")
            dists = batch_dist(query, vectors)
            results = list(zip(vids, dists.tolist()))
        else:
            dists: list[tuple[int, float]] = []
            for vid, vec in self._vectors.items():
                d = float(np.sum((query - vec) ** 2))
                dists.append((vid, d))
            results = dists
        results.sort(key=lambda x: x[1])
        return results[:top_k]

    def _handle_insert(self, stamp: LSNStamp, payload: bytes) -> bytes:
        return self._handle_vec_op(stamp, bytes([VecOp.INSERT]) + payload)

    def _handle_query(self, stamp: LSNStamp, payload: bytes) -> bytes:
        return self._handle_vec_op(stamp, bytes([VecOp.SEARCH]) + payload)

    def _handle_delete(self, stamp: LSNStamp, payload: bytes) -> bytes:
        return self._handle_vec_op(stamp, bytes([VecOp.DELETE]) + payload)

    # ── Direct API (for testing) ────────────────────────────────────

    def insert_vector(self, vec_id: int, vector: np.ndarray, meta: bytes = b"") -> None:
        """Direct insert (bypasses IPC)."""
        self._vectors[vec_id] = vector.astype(np.float32)
        self._metadata[vec_id] = meta
        if self._compression_enabled:
            self._compressed_store[vec_id] = self._xor_codec.encode(vector.astype(np.float32))
        if self._hnsw is not None:
            self._hnsw.add(vec_id, vector)

    def search_vector(self, query: np.ndarray, top_k: int = 10) -> list[tuple[int, float]]:
        """Direct search (bypasses IPC). Uses exact distance (lossless)."""
        if self._hnsw is not None and len(self._vectors) > 0:
            hnsw_results = self._hnsw.search(query, top_k=min(top_k, len(self._vectors)))
            if _USE_NATIVE:
                return [(r["id"], r["distance"]) for r in hnsw_results]
            return [(r.id, r.distance) for r in hnsw_results]
        return self._brute_search(query, top_k)

    def compress_all(self) -> dict[str, Any]:
        """Compress all stored vectors using XOR-Delta batch codec.

        This is the LOSSLESS FIRST approach — no PQ as primary.
        """
        if not self._vectors:
            return {"compressed": 0, "ratio": 0}
        batch = XORDeltaBatchCodec(self._dim)
        vecs = np.array(list(self._vectors.values()), dtype=np.float32)
        compressed = batch.encode_batch(vecs)
        original_bytes = vecs.nbytes
        return {
            "compressed": len(self._vectors),
            "original_bytes": original_bytes,
            "compressed_bytes": len(compressed),
            "ratio": original_bytes / max(len(compressed), 1),
        }

    @property
    def compression_stats(self) -> dict[str, Any]:
        """Return compression statistics."""
        total_raw = sum(v.nbytes for v in self._vectors.values())
        total_compressed = sum(len(c) for c in self._compressed_store.values())
        return {
            "vectors": len(self._vectors),
            "compressed_vectors": len(self._compressed_store),
            "raw_bytes": total_raw,
            "compressed_bytes": total_compressed,
            "ratio": total_raw / max(total_compressed, 1),
        }

    @property
    def vector_count(self) -> int:
        return len(self._vectors)
