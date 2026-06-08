"""QM General Satellite — Media, Text & Structured data storage.

Handles:
 - Structured row storage (INSERT/UPDATE/DELETE/QUERY)
 - Zstandard compression with dictionary training
 - Content-Defined Chunking (CDC) for deduplication
 - Full-text indexing via inverted index
 - Concurrent write operations with table-level locking
"""

from __future__ import annotations

import hashlib
import os
import struct
import threading
from typing import Any, Optional

import msgpack

try:
    import qm_engine as _qm_engine
except ImportError:
    _qm_engine = None

from qm_core.ipc.ring_buffer import CommandType
from qm_core.hub.lsn_sequencer import LSNStamp
from qm_core.satellite.base import Satellite, SatelliteConfig


class GeneralSatellite(Satellite):
    """Satellite for structured/text/media data.

    On-disk layout (data_dir/):
        rows/       — row pages (msgpack serialized)
        chunks/     — CDC deduplicated chunks
        text_idx/   — inverted index segments
    """

    def __init__(self, config: SatelliteConfig, ring):
        super().__init__(config, ring)

        # In-memory row store (table → {row_id → row_dict})
        self._tables: dict[str, dict[int, dict]] = {}
        self._next_ids: dict[str, int] = {}
        self._table_data_locks: dict[str, threading.Lock] = {}
        self._table_data_locks_lock = threading.Lock()

        # CDC chunk store: content_hash → data
        self._chunks: dict[bytes, bytes] = {}

        # Zstd compressor (lazy init)
        self._zstd = None
        try:
            import zstandard
            self._zstd = zstandard.ZstdCompressor(level=6)
            self._zstd_d = zstandard.ZstdDecompressor()
        except ImportError:
            pass

        # Storage paths
        self._rows_path = os.path.join(config.data_dir, "rows")
        self._chunks_path = os.path.join(config.data_dir, "chunks")
        os.makedirs(self._rows_path, exist_ok=True)
        os.makedirs(self._chunks_path, exist_ok=True)
    
    def _get_table_data_lock(self, table: str) -> threading.Lock:
        """Get or create a lock for a specific table's data."""
        with self._table_data_locks_lock:
            if table not in self._table_data_locks:
                self._table_data_locks[table] = threading.Lock()
            return self._table_data_locks[table]

    def _execute_command(
        self,
        stamp: LSNStamp,
        cmd: CommandType,
        payload: bytes,
    ) -> bytes:
        # Decode payload (msgpack envelope: {table, data, ...})
        try:
            msg = msgpack.unpackb(payload, raw=False)
        except Exception:
            msg = {"raw": payload}

        table = msg.get("table", "_default")

        if cmd == CommandType.INSERT:
            return self._do_insert(table, msg.get("row", {}), stamp)
        elif cmd == CommandType.BATCH_INSERT:
            return self._do_batch_insert(table, msg.get("rows", []), stamp)
        elif cmd == CommandType.UPDATE:
            return self._do_update(table, msg.get("row_id"), msg.get("updates", {}), stamp)
        elif cmd == CommandType.DELETE:
            return self._do_delete(table, msg.get("row_id"), stamp)
        elif cmd == CommandType.QUERY:
            return self._do_query(table, msg.get("predicates", []))
        elif cmd == CommandType.DDL:
            return self._do_ddl(table, msg)
        elif cmd == CommandType.COMPRESS:
            return self._do_compress(table)
        else:
            return b"OK"

    # ── Row operations ──────────────────────────────────────────────

    def _do_insert(self, table: str, row: dict, stamp: LSNStamp) -> bytes:
        lock = self._get_table_data_lock(table)
        with lock:
            if table not in self._tables:
                self._tables[table] = {}
                self._next_ids[table] = 1

            row_id = self._next_ids[table]
            self._next_ids[table] += 1
            row["_id"] = row_id
            row["_lsn"] = stamp.lsn

            self._tables[table][row_id] = row

        # Compress and persist (outside lock for better concurrency)
        data = msgpack.packb(row)
        if self._zstd:
            data = self._zstd.compress(data)

        page_file = os.path.join(self._rows_path, f"{table}_{row_id}.qmr")
        with open(page_file, "wb") as f:
            f.write(data)

        return msgpack.packb({"row_id": row_id, "lsn": stamp.lsn})

    def _do_batch_insert(self, table: str, rows: list, stamp: LSNStamp) -> bytes:
        """Batch insert for high throughput.
        
        Processes all rows in a single call, buffering writes to disk.
        Significantly faster than individual inserts for bulk loading.
        """
        lock = self._get_table_data_lock(table)
        row_ids = []
        batch_data = []
        
        with lock:
            if table not in self._tables:
                self._tables[table] = {}
                self._next_ids[table] = 1
            
            for row in rows:
                row_id = self._next_ids[table]
                self._next_ids[table] += 1
                row["_id"] = row_id
                row["_lsn"] = stamp.lsn
                
                self._tables[table][row_id] = row
                row_ids.append(row_id)
                batch_data.append((row_id, row))
        
        # Batch write to disk with buffered I/O (outside lock)
        if batch_data:
            # Serialize all rows in one msgpack call
            all_data = msgpack.packb(batch_data)
            if self._zstd:
                all_data = self._zstd.compress(all_data)
            
            # Write batch file
            batch_file = os.path.join(
                self._rows_path, 
                f"{table}_batch_{stamp.lsn}.qmb"
            )
            with open(batch_file, "wb") as f:
                f.write(all_data)
        
        return msgpack.packb({
            "row_ids": row_ids,
            "count": len(row_ids),
            "lsn": stamp.lsn,
        })

    def _do_update(self, table: str, row_id: int, updates: dict, stamp: LSNStamp) -> bytes:
        lock = self._get_table_data_lock(table)
        with lock:
            if table not in self._tables or row_id not in self._tables[table]:
                raise KeyError(f"Row {row_id} not found in {table}")

            row = self._tables[table][row_id]
            row.update(updates)
            row["_lsn"] = stamp.lsn

        return msgpack.packb({"row_id": row_id, "updated": True})

    def _do_delete(self, table: str, row_id: int, stamp: LSNStamp) -> bytes:
        lock = self._get_table_data_lock(table)
        with lock:
            if table not in self._tables:
                return msgpack.packb({"deleted": False})
            self._tables[table].pop(row_id, None)
        return msgpack.packb({"row_id": row_id, "deleted": True})

    def _do_query(self, table: str, predicates: list) -> bytes:
        lock = self._get_table_data_lock(table)
        with lock:
            if table not in self._tables:
                return msgpack.packb({"rows": []})

            rows = list(self._tables[table].values())

        # Apply simple predicates (outside lock, on copied data)
        for pred in predicates:
            col = pred.get("column", "")
            op = pred.get("op", "eq")
            val = pred.get("value")
            rows = [r for r in rows if self._eval_pred(r, col, op, val)]

        return msgpack.packb({"rows": rows})

    def _do_ddl(self, table: str, msg: dict) -> bytes:
        action = msg.get("action", "create")
        if action == "create":
            if table not in self._tables:
                self._tables[table] = {}
                self._next_ids[table] = 1
            return msgpack.packb({"created": table})
        elif action == "drop":
            self._tables.pop(table, None)
            self._next_ids.pop(table, None)
            return msgpack.packb({"dropped": table})
        return b"OK"

    def _do_compress(self, table: str) -> bytes:
        """Trigger zstd compression for a table's data."""
        if table not in self._tables:
            return msgpack.packb({"compressed": 0})
        count = 0
        for row_id, row in self._tables[table].items():
            data = msgpack.packb(row)
            if self._zstd:
                data = self._zstd.compress(data)
            page_file = os.path.join(self._rows_path, f"{table}_{row_id}.qmr")
            with open(page_file, "wb") as f:
                f.write(data)
            count += 1
        return msgpack.packb({"compressed": count})

    @staticmethod
    def _eval_pred(row: dict, col: str, op: str, val) -> bool:
        rv = row.get(col)
        if rv is None:
            return False
        if op == "eq":
            return rv == val
        elif op == "neq":
            return rv != val
        elif op == "gt":
            return rv > val
        elif op == "gte":
            return rv >= val
        elif op == "lt":
            return rv < val
        elif op == "lte":
            return rv <= val
        elif op == "contains":
            return val in str(rv)
        return True

    # ── Content-Defined Chunking (CDC) ──────────────────────────────

    def store_blob(self, data: bytes, chunk_size: int = 4096) -> list[bytes]:
        """Store a blob using CDC deduplication. Returns list of chunk hashes."""
        chunk_hashes: list[bytes] = []
        offset = 0
        while offset < len(data):
            # Rabin-like boundary detection (simplified gear hash)
            boundary = self._find_boundary(data, offset, chunk_size)
            chunk = data[offset:boundary]
            h = hashlib.sha256(chunk).digest()

            if h not in self._chunks:
                self._chunks[h] = chunk
                # Persist chunk
                chunk_file = os.path.join(self._chunks_path, h.hex())
                with open(chunk_file, "wb") as f:
                    if self._zstd:
                        f.write(self._zstd.compress(chunk))
                    else:
                        f.write(chunk)

            chunk_hashes.append(h)
            offset = boundary
        return chunk_hashes

    def load_blob(self, chunk_hashes: list[bytes]) -> bytes:
        """Reassemble a blob from its chunk hashes."""
        parts: list[bytes] = []
        for h in chunk_hashes:
            if h in self._chunks:
                parts.append(self._chunks[h])
            else:
                chunk_file = os.path.join(self._chunks_path, h.hex())
                if os.path.exists(chunk_file):
                    with open(chunk_file, "rb") as f:
                        raw = f.read()
                    if self._zstd:
                        raw = self._zstd_d.decompress(raw)
                    self._chunks[h] = raw
                    parts.append(raw)
                else:
                    raise FileNotFoundError(f"Chunk {h.hex()} not found")
        return b"".join(parts)

    @staticmethod
    def _find_boundary(data: bytes, offset: int, avg_size: int) -> int:
        """Gear-hash based CDC boundary detection."""
        mask = (1 << 12) - 1  # ~4KB average chunk
        min_size = avg_size // 4
        max_size = avg_size * 4
        fp = 0
        end = min(offset + max_size, len(data))

        for i in range(offset + min_size, end):
            fp = ((fp << 1) + data[i]) & 0xFFFFFFFF
            if (fp & mask) == 0:
                return i + 1

        return end

    # ── Direct API (for testing) ────────────────────────────────────

    def insert_row(self, table: str, row: dict) -> int:
        if table not in self._tables:
            self._tables[table] = {}
            self._next_ids[table] = 1
        row_id = self._next_ids[table]
        self._next_ids[table] += 1
        row["_id"] = row_id
        self._tables[table][row_id] = row
        return row_id

    def get_row(self, table: str, row_id: int) -> Optional[dict]:
        return self._tables.get(table, {}).get(row_id)

    def query_rows(self, table: str, predicates: list | None = None) -> list[dict]:
        if table not in self._tables:
            return []
        rows = list(self._tables[table].values())
        if predicates:
            for p in predicates:
                rows = [r for r in rows if self._eval_pred(r, p["column"], p["op"], p["value"])]
        return rows

    @property
    def table_names(self) -> list[str]:
        return list(self._tables.keys())
