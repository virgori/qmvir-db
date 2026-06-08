"""Tests for Final Sprint modules: Media Allocator, QM-SQL Parser,
Auto-Checkpoint, and QM Daemon.
"""

from __future__ import annotations

import os
import struct
import sys
import tempfile
import time
from pathlib import Path
import threading

import pytest


# ═══════════════════════════════════════════════════════════════════════
# 1. Media Slab Allocator Tests
# ═══════════════════════════════════════════════════════════════════════

class TestSlabPool:
    """Unit tests for the underlying _SlabPool."""

    def test_create_and_allocate(self, tmp_path):
        from qm_core.ipc.media_allocator import _SlabPool

        pool = _SlabPool(
            path=str(tmp_path / "test.heap"),
            slab_size=4096,
            slab_count=8,
            create=True,
        )
        assert pool.free_count == 8
        assert pool.used_count == 0

        idx = pool.allocate()
        assert idx is not None
        assert pool.free_count == 7
        assert pool.used_count == 1

        pool.free(idx)
        assert pool.free_count == 8
        pool.close()

    def test_exhaust_pool(self, tmp_path):
        from qm_core.ipc.media_allocator import _SlabPool

        pool = _SlabPool(str(tmp_path / "x.heap"), slab_size=1024, slab_count=4)
        indices = []
        for _ in range(4):
            idx = pool.allocate()
            assert idx is not None
            indices.append(idx)

        # Pool should be exhausted
        assert pool.allocate() is None
        assert pool.free_count == 0

        # Free one and re-allocate
        pool.free(indices[0])
        idx = pool.allocate()
        assert idx is not None
        pool.close()

    def test_write_and_read(self, tmp_path):
        from qm_core.ipc.media_allocator import _SlabPool

        pool = _SlabPool(str(tmp_path / "rw.heap"), slab_size=4096, slab_count=2)
        idx = pool.allocate()

        data = b"Hello QM Media!" * 100
        written = pool.write(idx, data)
        assert written == len(data)

        read_back = pool.read(idx, len(data))
        assert read_back == data
        pool.close()

    def test_memoryview_zero_copy(self, tmp_path):
        from qm_core.ipc.media_allocator import _SlabPool

        pool = _SlabPool(str(tmp_path / "mv.heap"), slab_size=4096, slab_count=2)
        idx = pool.allocate()

        pool.write(idx, b"\xDE\xAD\xBE\xEF")
        mv = pool.memoryview_of(idx)
        assert bytes(mv[:4]) == b"\xDE\xAD\xBE\xEF"
        pool.close()

    def test_free_idempotent(self, tmp_path):
        from qm_core.ipc.media_allocator import _SlabPool

        pool = _SlabPool(str(tmp_path / "idem.heap"), slab_size=1024, slab_count=4)
        idx = pool.allocate()
        pool.free(idx)
        pool.free(idx)  # should not double-count
        assert pool.free_count == 4
        pool.close()

    def test_free_invalid_index_raises(self, tmp_path):
        from qm_core.ipc.media_allocator import _SlabPool

        pool = _SlabPool(str(tmp_path / "inv.heap"), slab_size=1024, slab_count=4)
        with pytest.raises(ValueError):
            pool.free(-1)
        with pytest.raises(ValueError):
            pool.free(100)
        pool.close()

    def test_attach_existing(self, tmp_path):
        from qm_core.ipc.media_allocator import _SlabPool

        path = str(tmp_path / "attach.heap")
        pool1 = _SlabPool(path, slab_size=4096, slab_count=4, create=True)
        idx = pool1.allocate()
        pool1.write(idx, b"shared data")
        pool1.close()

        pool2 = _SlabPool(path, slab_size=4096, slab_count=4, create=False)
        read_back = pool2.read(idx, 11)
        assert read_back == b"shared data"
        pool2.close()


class TestMediaSlabAllocator:
    """Tests for the multi-class MediaSlabAllocator."""

    def test_basic_allocate_and_free(self, tmp_path):
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[1024, 4096],
            slab_counts=[4, 2],
        ))

        handle = alloc.allocate(500)
        assert handle.class_idx == 0  # fits in 1024
        assert handle.size == 1024

        handle2 = alloc.allocate(2000)
        assert handle2.class_idx == 1  # needs 4096

        alloc.free(handle)
        alloc.free(handle2)
        alloc.close()

    def test_write_read_round_trip(self, tmp_path):
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[4096],
            slab_counts=[4],
        ))

        data = os.urandom(3000)
        handle = alloc.allocate(len(data))
        alloc.write(handle, data)

        read_back = alloc.read(handle, len(data))
        assert read_back == data
        alloc.free(handle)
        alloc.close()

    def test_zero_copy_memoryview(self, tmp_path):
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[4096],
            slab_counts=[2],
        ))

        handle = alloc.allocate(100)
        alloc.write(handle, b"zero-copy test")
        mv = alloc.memoryview_of(handle)
        assert bytes(mv[:14]) == b"zero-copy test"
        alloc.close()

    def test_handle_serialization(self):
        from qm_core.ipc.media_allocator import SlabHandle

        original = SlabHandle(class_idx=1, slab_idx=5, offset=8192, size=4096)
        raw = original.to_bytes()
        restored = SlabHandle.from_bytes(raw)
        assert restored.class_idx == original.class_idx
        assert restored.slab_idx == original.slab_idx
        assert restored.offset == original.offset
        assert restored.size == original.size

    def test_stats(self, tmp_path):
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[1024, 4096],
            slab_counts=[4, 2],
        ))

        assert alloc.total_capacity_bytes() == 4 * 1024 + 2 * 4096
        assert alloc.total_allocated_bytes() == 0

        h = alloc.allocate(500)
        assert alloc.total_allocated_bytes() == 1024

        stats = alloc.stats()
        assert len(stats) == 2
        assert stats[0]["used"] == 1
        assert stats[1]["used"] == 0
        alloc.free(h)
        alloc.close()

    def test_no_slab_available_raises(self, tmp_path):
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[1024],
            slab_counts=[1],
        ))

        alloc.allocate(500)  # uses the only slab
        with pytest.raises(RuntimeError):
            alloc.allocate(500)
        alloc.close()

    def test_oversized_raises(self, tmp_path):
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[1024],
            slab_counts=[4],
        ))

        with pytest.raises(RuntimeError):
            alloc.allocate(2000)  # no class large enough
        alloc.close()

    def test_attach_mode(self, tmp_path):
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        cfg = MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[4096],
            slab_counts=[4],
        )

        alloc1 = MediaSlabAllocator(cfg, create=True)
        h = alloc1.allocate(100)
        alloc1.write(h, b"shared media blob")

        # Satellite attaches
        alloc2 = MediaSlabAllocator.attach(cfg)
        data = alloc2.read(h, 17)
        assert data == b"shared media blob"

        alloc1.close()
        alloc2.close()

    def test_resolve_mref(self, tmp_path):
        """resolve_mref returns slab descriptor dict for MREF queries."""
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[4096],
            slab_counts=[4],
        ))

        handle = alloc.allocate(100)
        alloc.write(handle, b"mref test data")

        info = alloc.resolve_mref(handle)
        assert info["class_idx"] == handle.class_idx
        assert info["slab_idx"] == handle.slab_idx
        assert info["offset"] == handle.offset
        assert info["slab_size"] == handle.size
        assert info["allocated"] is True
        assert "pool_path" in info

        alloc.free(handle)
        info2 = alloc.resolve_mref(handle)
        assert info2["allocated"] is False

        alloc.close()


# ═══════════════════════════════════════════════════════════════════════
# 2. QM-SQL Parser Tests
# ═══════════════════════════════════════════════════════════════════════

class TestQMSQLParser:
    """Tests for the Lark-based QM-SQL parser."""

    @pytest.fixture
    def parser(self):
        from qm_core.execution.qm_sql import QMSQLParser
        return QMSQLParser()

    def test_search_vector_basic(self, parser):
        from qm_core.execution.qm_sql import SearchVectorStmt
        r = parser.parse("SEARCH VECTOR [0.1, 0.5, 0.9] IN photos TOP 10;")
        assert isinstance(r, SearchVectorStmt)
        assert r.vector == [0.1, 0.5, 0.9]
        assert r.table == "photos"
        assert r.top_k == 10
        assert r.metric == "cosine"

    def test_search_vector_with_metric(self, parser):
        from qm_core.execution.qm_sql import SearchVectorStmt
        r = parser.parse("SEARCH VECTOR [1.0, 2.0] IN embeddings TOP 5 METRIC l2;")
        assert isinstance(r, SearchVectorStmt)
        assert r.metric == "l2"

    def test_set_compression(self, parser):
        from qm_core.execution.qm_sql import SetCompressionStmt
        r = parser.parse("SET COMPRESSION 'XOR_DELTA' ON my_table;")
        assert isinstance(r, SetCompressionStmt)
        assert r.codec == "XOR_DELTA"
        assert r.table == "my_table"

    def test_link_media(self, parser):
        from qm_core.execution.qm_sql import LinkMediaStmt
        r = parser.parse("LINK MEDIA '/data/photo.jpg' TO users ROW_ID 42;")
        assert isinstance(r, LinkMediaStmt)
        assert r.path == "/data/photo.jpg"
        assert r.table == "users"
        assert r.row_id == 42

    def test_unlink_media(self, parser):
        from qm_core.execution.qm_sql import UnlinkMediaStmt
        r = parser.parse("UNLINK MEDIA FROM users ROW_ID 42;")
        assert isinstance(r, UnlinkMediaStmt)
        assert r.table == "users"
        assert r.row_id == 42

    def test_show_slabs(self, parser):
        from qm_core.execution.qm_sql import ShowSlabsStmt
        r = parser.parse("SHOW SLABS;")
        assert isinstance(r, ShowSlabsStmt)

    def test_checkpoint_full(self, parser):
        from qm_core.execution.qm_sql import CheckpointStmt
        r = parser.parse("CHECKPOINT FULL;")
        assert isinstance(r, CheckpointStmt)
        assert r.mode == "full"

    def test_checkpoint_delta(self, parser):
        from qm_core.execution.qm_sql import CheckpointStmt
        r = parser.parse("CHECKPOINT DELTA;")
        assert isinstance(r, CheckpointStmt)
        assert r.mode == "delta"

    def test_checkpoint_no_mode(self, parser):
        from qm_core.execution.qm_sql import CheckpointStmt
        r = parser.parse("CHECKPOINT;")
        assert isinstance(r, CheckpointStmt)
        assert r.mode == "full"

    def test_select_basic(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT name, age FROM users;")
        assert isinstance(r, SelectStmt)
        assert r.from_table == "users"
        assert len(r.columns) == 2

    def test_select_where(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT * FROM users WHERE age > 25;")
        assert isinstance(r, SelectStmt)
        assert r.where is not None

    def test_select_distinct(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT DISTINCT name FROM users;")
        assert isinstance(r, SelectStmt)
        assert r.distinct is True

    def test_select_order_limit(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT * FROM users ORDER BY age DESC LIMIT 10;")
        assert isinstance(r, SelectStmt)
        assert r.limit == 10
        assert len(r.order_by) == 1

    def test_insert(self, parser):
        from qm_core.execution.sql_parser import InsertStmt
        r = parser.parse("INSERT INTO users (name, age) VALUES ('Alice', 30);")
        assert isinstance(r, InsertStmt)
        assert r.table == "users"
        assert r.columns == ["name", "age"]

    def test_update(self, parser):
        from qm_core.execution.sql_parser import UpdateStmt
        r = parser.parse("UPDATE users SET age = 31 WHERE name = 'Alice';")
        assert isinstance(r, UpdateStmt)
        assert r.table == "users"
        assert r.where is not None

    def test_delete(self, parser):
        from qm_core.execution.sql_parser import DeleteStmt
        r = parser.parse("DELETE FROM users WHERE age < 18;")
        assert isinstance(r, DeleteStmt)
        assert r.where is not None

    def test_create_table(self, parser):
        from qm_core.execution.sql_parser import CreateTableStmt
        r = parser.parse("CREATE TABLE users (name TEXT, age INT);")
        assert isinstance(r, CreateTableStmt)
        assert r.table == "users"
        assert len(r.columns) == 2

    def test_between_expr(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT * FROM t WHERE x BETWEEN 1 AND 10;")
        assert isinstance(r, SelectStmt)
        assert r.where is not None

    def test_in_expr(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT * FROM t WHERE x IN (1, 2, 3);")
        assert isinstance(r, SelectStmt)

    def test_like_expr(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT * FROM t WHERE name LIKE 'A%';")
        assert isinstance(r, SelectStmt)

    def test_is_null(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT * FROM t WHERE x IS NULL;")
        assert isinstance(r, SelectStmt)

    def test_syntax_error_raises(self, parser):
        from qm_core.execution.sql_parser import SQLSyntaxError
        with pytest.raises(SQLSyntaxError):
            parser.parse("INVALID GARBAGE HERE;")

    def test_join(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT * FROM a INNER JOIN b ON a.id = b.id;")
        assert isinstance(r, SelectStmt)
        assert len(r.joins) == 1

    def test_group_by_having(self, parser):
        from qm_core.execution.sql_parser import SelectStmt
        r = parser.parse("SELECT dept, COUNT(id) FROM emp GROUP BY dept HAVING COUNT(id) > 5;")
        assert isinstance(r, SelectStmt)
        assert r.group_by is not None
        assert r.having is not None

    def test_case_insensitive(self, parser):
        from qm_core.execution.qm_sql import SearchVectorStmt
        r = parser.parse("search vector [0.1] in photos top 5;")
        assert isinstance(r, SearchVectorStmt)

    # ── Short-keyword (hybrid) tests ────────────────────────────────

    def test_likev_basic(self, parser):
        """LIKEV VEC [...] IN table TOP k — short form of SEARCH VECTOR."""
        from qm_core.execution.qm_sql import SearchVectorStmt
        r = parser.parse("LIKEV VEC [0.1, 0.5, 0.9] IN photos TOP 10;")
        assert isinstance(r, SearchVectorStmt)
        assert r.vector == [0.1, 0.5, 0.9]
        assert r.table == "photos"
        assert r.top_k == 10
        assert r.metric == "cosine"

    def test_likev_with_metric(self, parser):
        from qm_core.execution.qm_sql import SearchVectorStmt
        r = parser.parse("LIKEV VEC [1.0] IN emb TOP 3 METRIC l2;")
        assert isinstance(r, SearchVectorStmt)
        assert r.metric == "l2"

    def test_likev_with_where(self, parser):
        from qm_core.execution.qm_sql import SearchVectorStmt
        r = parser.parse("LIKEV VEC [0.5] IN photos TOP 5 WHERE active = 1;")
        assert isinstance(r, SearchVectorStmt)
        assert r.where is not None

    def test_cpoint_full(self, parser):
        """CPOINT FULL — short form of CHECKPOINT FULL."""
        from qm_core.execution.qm_sql import CheckpointStmt
        r = parser.parse("CPOINT FULL;")
        assert isinstance(r, CheckpointStmt)
        assert r.mode == "full"

    def test_cpoint_delta(self, parser):
        from qm_core.execution.qm_sql import CheckpointStmt
        r = parser.parse("CPOINT DELTA;")
        assert isinstance(r, CheckpointStmt)
        assert r.mode == "delta"

    def test_cpoint_no_mode(self, parser):
        from qm_core.execution.qm_sql import CheckpointStmt
        r = parser.parse("CPOINT;")
        assert isinstance(r, CheckpointStmt)
        assert r.mode == "full"

    def test_slabs_short(self, parser):
        """SLABS — short form of SHOW SLABS."""
        from qm_core.execution.qm_sql import ShowSlabsStmt
        r = parser.parse("SLABS;")
        assert isinstance(r, ShowSlabsStmt)

    def test_mref(self, parser):
        """MREF table row_id — retrieve SlabHandle pointer."""
        from qm_core.execution.qm_sql import MrefStmt
        r = parser.parse("MREF photos 42;")
        assert isinstance(r, MrefStmt)
        assert r.table == "photos"
        assert r.row_id == 42

    def test_dist_column(self, parser):
        """SELECT DIST, name FROM results — DIST as virtual column."""
        from qm_core.execution.sql_parser import SelectStmt
        from qm_core.execution.qm_sql import DistColumn
        r = parser.parse("SELECT DIST, name FROM results;")
        assert isinstance(r, SelectStmt)
        # Find DistColumn in columns
        dist_found = False
        for col in r.columns:
            expr = col.expr if hasattr(col, 'expr') else col
            if isinstance(expr, DistColumn):
                dist_found = True
                break
        assert dist_found, f"DistColumn not found in {r.columns}"

    def test_link_short_form(self, parser):
        """LINK '/path' TO table ROW <id> — without MEDIA keyword."""
        from qm_core.execution.qm_sql import LinkMediaStmt
        r = parser.parse("LINK '/data/photo.jpg' TO users ROW 42;")
        assert isinstance(r, LinkMediaStmt)
        assert r.path == "/data/photo.jpg"
        assert r.row_id == 42

    def test_unlink_short_form(self, parser):
        """UNLINK FROM table ROW <id> — without MEDIA keyword."""
        from qm_core.execution.qm_sql import UnlinkMediaStmt
        r = parser.parse("UNLINK FROM users ROW 42;")
        assert isinstance(r, UnlinkMediaStmt)
        assert r.table == "users"
        assert r.row_id == 42

    def test_link_row_id_keyword(self, parser):
        """Both ROW and ROW_ID keywords work."""
        from qm_core.execution.qm_sql import LinkMediaStmt
        r1 = parser.parse("LINK MEDIA '/x' TO t ROW_ID 1;")
        r2 = parser.parse("LINK '/x' TO t ROW 1;")
        assert isinstance(r1, LinkMediaStmt)
        assert isinstance(r2, LinkMediaStmt)
        assert r1.row_id == r2.row_id


# ═══════════════════════════════════════════════════════════════════════
# 3. Auto-Checkpoint Tests
# ═══════════════════════════════════════════════════════════════════════

class TestCheckpointRecord:
    """Tests for CheckpointRecord serialization."""

    def test_round_trip(self):
        from qm_core.checkpoint import CheckpointRecord

        record = CheckpointRecord(
            lsn=42,
            epoch=3,
            timestamp_ns=1234567890,
            table_meta={"users": {"schema": {"name": "text"}}},
            merkle_root=b"\xAB" * 32,
            mode="full",
        )
        data = record.to_bytes()
        restored = CheckpointRecord.from_bytes(data)

        assert restored.lsn == 42
        assert restored.epoch == 3
        assert restored.timestamp_ns == 1234567890
        assert restored.table_meta == {"users": {"schema": {"name": "text"}}}
        assert restored.merkle_root == b"\xAB" * 32
        assert restored.mode == "full"

    def test_integrity_check(self):
        from qm_core.checkpoint import CheckpointRecord

        record = CheckpointRecord(
            lsn=1, epoch=0, timestamp_ns=0,
            table_meta={}, merkle_root=b"\x00" * 32,
        )
        data = bytearray(record.to_bytes())
        # Corrupt one byte
        data[10] ^= 0xFF
        with pytest.raises(ValueError, match="SHA-256 mismatch"):
            CheckpointRecord.from_bytes(bytes(data))


class TestCheckpointManager:
    """Tests for CheckpointManager lifecycle."""

    def test_force_checkpoint(self, tmp_path):
        from qm_core.checkpoint import CheckpointManager, CheckpointConfig

        state = {"lsn": 100, "epoch": 1, "table_meta": {"t": {}},
                 "merkle_root": b"\x00" * 32}

        mgr = CheckpointManager(
            CheckpointConfig(
                checkpoint_dir=str(tmp_path / "ckpt"),
                enabled=False,
            ),
            state_provider=lambda: state,
        )

        record = mgr.force_checkpoint("full")
        assert record.lsn == 100
        assert record.epoch == 1
        assert mgr.checkpoint_count == 1

    def test_recover_latest(self, tmp_path):
        from qm_core.checkpoint import CheckpointManager, CheckpointConfig

        ckpt_dir = str(tmp_path / "ckpt")
        state = {"lsn": 50, "epoch": 0, "table_meta": {},
                 "merkle_root": b"\x00" * 32}

        mgr = CheckpointManager(
            CheckpointConfig(checkpoint_dir=ckpt_dir, enabled=False),
            state_provider=lambda: state,
        )

        mgr.force_checkpoint("full")

        state["lsn"] = 100
        mgr.force_checkpoint("delta")

        # Latest should be LSN 100
        recovered = mgr.recover_state()
        assert recovered["lsn"] == 100

    def test_prune_old_checkpoints(self, tmp_path):
        from qm_core.checkpoint import CheckpointManager, CheckpointConfig

        ckpt_dir = str(tmp_path / "ckpt")
        i = 0

        def state_fn():
            nonlocal i
            i += 1
            return {"lsn": i, "epoch": 0, "table_meta": {},
                    "merkle_root": b"\x00" * 32}

        mgr = CheckpointManager(
            CheckpointConfig(
                checkpoint_dir=ckpt_dir,
                max_checkpoints=3,
                enabled=False,
            ),
            state_provider=state_fn,
        )

        for _ in range(6):
            mgr.force_checkpoint("full")

        files = [f for f in os.listdir(ckpt_dir) if f.endswith(".qmck")]
        assert len(files) <= 3

    def test_background_thread(self, tmp_path):
        from qm_core.checkpoint import CheckpointManager, CheckpointConfig

        state = {"lsn": 1, "epoch": 0, "table_meta": {},
                 "merkle_root": b"\x00" * 32}

        mgr = CheckpointManager(
            CheckpointConfig(
                checkpoint_dir=str(tmp_path / "ckpt"),
                interval_seconds=0.1,
                lsn_threshold=5,
                enabled=True,
            ),
            state_provider=lambda: state,
        )

        mgr.start()
        assert mgr.is_running
        # Trigger threshold
        for _ in range(5):
            mgr.notify_mutation()
        time.sleep(0.3)
        mgr.stop()
        assert mgr.checkpoint_count >= 1

    def test_stats(self, tmp_path):
        from qm_core.checkpoint import CheckpointManager, CheckpointConfig

        mgr = CheckpointManager(
            CheckpointConfig(
                checkpoint_dir=str(tmp_path / "ckpt"),
                enabled=False,
            ),
            state_provider=lambda: {"lsn": 0, "epoch": 0,
                                     "table_meta": {},
                                     "merkle_root": b"\x00" * 32},
        )
        s = mgr.stats()
        assert "checkpoint_count" in s
        assert "pending_mutations" in s
        assert s["is_running"] is False


# ═══════════════════════════════════════════════════════════════════════
# 4. QM Daemon Tests
# ═══════════════════════════════════════════════════════════════════════

class TestQMDaemon:
    """Tests for QM Daemon bootstrap and lifecycle."""

    def test_daemon_config_defaults(self):
        from qm_app import QMDaemonConfig
        cfg = QMDaemonConfig()
        assert cfg.port == 55433
        assert cfg.host == "127.0.0.1"
        assert len(cfg.media_size_classes) == 4
        assert cfg.checkpoint_interval == 30.0

    def test_daemon_bootstrap(self, tmp_path):
        from qm_app import QMDaemon, QMDaemonConfig

        config = QMDaemonConfig(
            data_dir=str(tmp_path / "qm"),
            media_size_classes=[1024, 4096],
            media_slab_counts=[4, 2],
        )
        daemon = QMDaemon(config)
        daemon._bootstrap()

        # Engine should be ready
        assert daemon._engine is not None
        assert daemon._engine.VERSION == "2.0.0-hub"

        # Media allocator should be ready
        assert daemon._media_alloc is not None
        assert daemon._media_alloc.total_capacity_bytes() > 0

        # Checkpoint manager should be ready
        assert daemon._checkpoint_mgr is not None

        # Cleanup
        daemon._engine.close()
        daemon._media_alloc.close()

    def test_daemon_status(self, tmp_path):
        from qm_app import QMDaemon, QMDaemonConfig

        config = QMDaemonConfig(
            data_dir=str(tmp_path / "qm"),
            media_size_classes=[1024],
            media_slab_counts=[2],
        )
        daemon = QMDaemon(config)
        daemon._bootstrap()

        status = daemon.status()
        assert "engine_version" in status
        assert "gateway" in status
        assert "media_heap" in status
        assert "checkpoint" in status

        daemon._engine.close()
        daemon._media_alloc.close()

    def test_format_bytes(self):
        from qm_app import _format_bytes
        assert "KB" in _format_bytes(2048)
        assert "MB" in _format_bytes(2 * 1024 * 1024)
        assert "B" in _format_bytes(100)

    def test_checkpoint_state_provider(self, tmp_path):
        from qm_app import QMDaemon, QMDaemonConfig

        config = QMDaemonConfig(
            data_dir=str(tmp_path / "qm"),
            media_size_classes=[1024],
            media_slab_counts=[2],
        )
        daemon = QMDaemon(config)
        daemon._bootstrap()

        state = daemon._get_checkpoint_state()
        assert "lsn" in state
        assert "epoch" in state
        assert "table_meta" in state
        assert "merkle_root" in state

        daemon._engine.close()
        daemon._media_alloc.close()


# ═══════════════════════════════════════════════════════════════════════
# 5. Integration: Media Allocator + Ring Buffer IPC
# ═══════════════════════════════════════════════════════════════════════

class TestMediaIPCIntegration:
    """Test sending slab handles over ring buffer."""

    def test_handle_through_ring(self, tmp_path):
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.ipc.media_allocator import MediaSlabAllocator, MediaAllocatorConfig, SlabHandle

        ring = SharedRingBuffer(
            path=str(tmp_path / "ring.shm"),
            slot_count=16,
            slot_data_size=4096,
        )

        alloc = MediaSlabAllocator(MediaAllocatorConfig(
            heap_dir=str(tmp_path / "heap"),
            size_classes=[4096],
            slab_counts=[4],
        ))

        # Hub side: allocate, write, send handle
        data = b"media payload " * 200
        handle = alloc.allocate(len(data))
        alloc.write(handle, data)

        # Send handle bytes over ring buffer
        handle_bytes = handle.to_bytes()
        slot = ring.publish(lsn=1, cmd=CommandType.INSERT, payload=handle_bytes)
        assert slot >= 0

        # Satellite side: consume, decode handle, read media
        result = ring.consume()
        assert result is not None
        _, hdr, payload_mv = result
        received_handle = SlabHandle.from_bytes(bytes(payload_mv))
        assert received_handle.class_idx == handle.class_idx
        assert received_handle.slab_idx == handle.slab_idx

        read_back = alloc.read(received_handle, len(data))
        assert read_back == data

        alloc.close()


# ═══════════════════════════════════════════════════════════════════════
# 6. Import Smoke Tests
# ═══════════════════════════════════════════════════════════════════════

class TestImports:
    """Verify all new modules import correctly."""

    def test_import_media_allocator(self):
        from qm_core.ipc import MediaSlabAllocator, MediaAllocatorConfig, SlabHandle
        assert MediaSlabAllocator is not None
        assert SlabHandle is not None

    def test_import_qm_sql(self):
        from qm_core.execution.qm_sql import (
            QMSQLParser, SearchVectorStmt, SetCompressionStmt,
            LinkMediaStmt, UnlinkMediaStmt, ShowSlabsStmt, CheckpointStmt,
            MrefStmt, DistColumn,
        )
        assert QMSQLParser is not None
        assert MrefStmt is not None
        assert DistColumn is not None

    def test_import_checkpoint(self):
        from qm_core.checkpoint import (
            CheckpointManager, CheckpointConfig, CheckpointRecord,
        )
        assert CheckpointManager is not None

    def test_import_daemon(self):
        from qm_app import QMDaemon, QMDaemonConfig
        assert QMDaemon is not None


# ═══════════════════════════════════════════════════════════════════════
# 7. RBAC / Auth Tests
# ═══════════════════════════════════════════════════════════════════════

class TestRBACAuth:
    """Test qm_core.auth — password hashing, user catalog, ACL."""

    def test_hash_and_verify(self):
        from qm_core.auth import _hash_password, _verify_password
        salt, dk = _hash_password("s3cret")
        assert len(salt) == 16
        assert len(dk) == 32
        assert _verify_password("s3cret", salt, dk) is True
        assert _verify_password("wrong", salt, dk) is False

    def test_user_catalog_bootstrap(self):
        from qm_core.auth import UserCatalog, Role
        cat = UserCatalog()
        users = cat.list_users()
        assert len(users) == 1
        assert users[0].username == "admin"
        assert users[0].role == Role.ADMIN

    def test_create_and_authenticate(self):
        from qm_core.auth import UserCatalog, Role, AuthSession
        cat = UserCatalog()
        cat.create_user("alice", "pw123", Role.WRITER)
        session = cat.authenticate("alice", "pw123")
        assert isinstance(session, AuthSession)
        assert session.username == "alice"
        assert session.role == Role.WRITER
        assert session.can_write is True
        assert session.is_admin is False

    def test_authenticate_invalid(self):
        from qm_core.auth import UserCatalog, AuthError
        cat = UserCatalog()
        with pytest.raises(AuthError) as exc:
            cat.authenticate("admin", "wrongpass")
        assert exc.value.pg_code == "28P01"

    def test_authenticate_nonexistent(self):
        from qm_core.auth import UserCatalog, AuthError
        cat = UserCatalog()
        with pytest.raises(AuthError) as exc:
            cat.authenticate("nobody", "pass")
        assert exc.value.pg_code == "28P01"

    def test_duplicate_user(self):
        from qm_core.auth import UserCatalog, Role, AuthError
        cat = UserCatalog()
        with pytest.raises(AuthError) as exc:
            cat.create_user("admin", "x", Role.READER)
        assert exc.value.pg_code == "42710"

    def test_drop_user(self):
        from qm_core.auth import UserCatalog, Role
        cat = UserCatalog()
        cat.create_user("bob", "pw", Role.READER)
        assert len(cat.list_users()) == 2
        cat.drop_user("bob")
        assert len(cat.list_users()) == 1

    def test_alter_role(self):
        from qm_core.auth import UserCatalog, Role
        cat = UserCatalog()
        cat.create_user("carol", "pw", Role.READER)
        cat.alter_role("carol", Role.ADMIN)
        rec = cat.get_user("carol")
        assert rec.role == Role.ADMIN

    def test_serialize_and_restore(self):
        from qm_core.auth import UserCatalog, Role
        cat = UserCatalog()
        cat.create_user("dave", "pw", Role.WRITER)
        data = cat.to_dict()
        cat2 = UserCatalog.__new__(UserCatalog)
        cat2._users = {}
        cat2.load_from_dict(data)
        assert cat2.get_user("dave") is not None
        assert cat2.get_user("dave").role == Role.WRITER

    def test_check_permission_admin(self):
        from qm_core.auth import UserCatalog, Role, AuthSession, check_permission
        from qm_core.execution.qm_sql import CheckpointStmt
        session = AuthSession(username="admin", role=Role.ADMIN, token="tok")
        # Should NOT raise
        check_permission(session, CheckpointStmt(mode="full"))

    def test_check_permission_reader_denied_write(self):
        from qm_core.auth import AuthSession, Role, AuthError, check_permission
        from qm_core.execution.qm_sql import QMSQLParser
        parser = QMSQLParser()
        ast = parser.parse("INSERT INTO t (a) VALUES (1)")
        session = AuthSession(username="reader1", role=Role.READER, token="tok")
        with pytest.raises(AuthError) as exc:
            check_permission(session, ast)
        assert exc.value.pg_code == "42501"

    def test_check_permission_writer_select_ok(self):
        from qm_core.auth import AuthSession, Role, check_permission
        from qm_core.execution.qm_sql import QMSQLParser
        parser = QMSQLParser()
        ast = parser.parse("SELECT * FROM t")
        session = AuthSession(username="w1", role=Role.WRITER, token="tok")
        check_permission(session, ast)  # should not raise

    def test_check_permission_reader_likev_ok(self):
        from qm_core.auth import AuthSession, Role, check_permission
        from qm_core.execution.qm_sql import QMSQLParser
        parser = QMSQLParser()
        ast = parser.parse("LIKEV VEC [0.1,0.2] IN docs TOP 5")
        session = AuthSession(username="r1", role=Role.READER, token="tok")
        check_permission(session, ast)  # should not raise

    def test_check_permission_reader_denied_cpoint(self):
        from qm_core.auth import AuthSession, Role, AuthError, check_permission
        from qm_core.execution.qm_sql import CheckpointStmt
        session = AuthSession(username="r1", role=Role.READER, token="tok")
        with pytest.raises(AuthError):
            check_permission(session, CheckpointStmt(mode="full"))

    def test_roles_ordering(self):
        from qm_core.auth import Role
        assert Role.READER < Role.WRITER < Role.ADMIN


# ═══════════════════════════════════════════════════════════════════════
# 8. Daemon Auth Integration Tests
# ═══════════════════════════════════════════════════════════════════════

class TestDaemonAuth:
    """Test qm_app.py auth integration."""

    def test_daemon_has_user_catalog(self):
        from qm_app import QMDaemon, QMDaemonConfig
        cfg = QMDaemonConfig(data_dir="/tmp/qm_test_auth")
        d = QMDaemon(cfg)
        assert d.user_catalog is not None
        assert len(d.user_catalog.list_users()) >= 1

    def test_daemon_status_includes_auth(self):
        from qm_app import QMDaemon, QMDaemonConfig
        cfg = QMDaemonConfig(data_dir="/tmp/qm_test_auth2")
        d = QMDaemon(cfg)
        st = d.status()
        assert "users" in st
        assert "auth_roles" in st
        assert st["users"] >= 1

    def test_daemon_version(self):
        from qm_app import __version__
        assert __version__ == "5.4.0"


# ═══════════════════════════════════════════════════════════════════════
# 9. CLI Parser Tests
# ═══════════════════════════════════════════════════════════════════════

class TestCLIParser:
    """Test the subcommand-based CLI parser."""

    def test_build_parser(self):
        from qm_app import _build_parser
        p = _build_parser()
        assert p.prog == "qmvir"

    def test_start_subcommand(self):
        from qm_app import _build_parser
        p = _build_parser()
        args = p.parse_args(["start", "--port", "9999"])
        assert args.command == "start"
        assert args.port == 9999

    def test_start_subcommand_unix_socket_auto(self):
        from qm_app import _build_parser
        p = _build_parser()
        args = p.parse_args(["start", "--port", "55433", "--unix-socket"])
        assert args.command == "start"
        assert args.unix_socket == "auto"

    def test_unix_socket_path_resolver(self):
        from qm_app import _resolve_unix_socket_path
        assert _resolve_unix_socket_path(None, 55433) is None
        assert _resolve_unix_socket_path("auto", 55433) == "/tmp/.s.PGSQL.55433"
        assert _resolve_unix_socket_path("/tmp/custom.sock", 55433) == "/tmp/custom.sock"

    def test_unix_socket_path_resolver_directory(self, tmp_path):
        from qm_app import _resolve_unix_socket_path
        out = _resolve_unix_socket_path(str(tmp_path / "pgsockdir"), 55433)
        assert out is not None
        assert out.endswith("/.s.PGSQL.55433")

    def test_version_subcommand(self):
        from qm_app import _build_parser
        p = _build_parser()
        args = p.parse_args(["version"])
        assert args.command == "version"

    def test_legacy_flag_start(self):
        from qm_app import _build_parser
        p = _build_parser()
        args = p.parse_args(["--start", "--port", "5555"])
        assert args.start is True

    def test_status_subcommand(self):
        from qm_app import _build_parser
        p = _build_parser()
        args = p.parse_args(["status"])
        assert args.command == "status"

    def test_check_subcommand(self):
        from qm_app import _build_parser
        p = _build_parser()
        args = p.parse_args(["check"])
        assert args.command == "check"


# ═══════════════════════════════════════════════════════════════════════
# 10. Benchmark Module Tests
# ═══════════════════════════════════════════════════════════════════════

class TestBenchmark:
    """Test qm_core.bench — verify BenchResult and parse benchmark."""

    def test_bench_result_format(self):
        from qm_core.bench import BenchResult
        r = BenchResult(name="test_op", ops=1000, elapsed_s=0.1)
        assert r.ops_per_sec == pytest.approx(10_000.0)
        s = str(r)
        assert "test_op" in s

    def test_bench_sql_parse(self):
        from qm_core.bench import bench_sql_parse
        r = bench_sql_parse(ops=100)
        assert r.ops == 100
        assert r.elapsed_s > 0
        assert r.p50_us > 0

    def test_percentile(self):
        from qm_core.bench import _percentile
        data = list(range(100))
        assert _percentile(data, 50) == 50
        assert _percentile(data, 99) == 99
        assert _percentile([], 50) == 0.0


# ═══════════════════════════════════════════════════════════════════════
# 11. Packaging Smoke Tests
# ═══════════════════════════════════════════════════════════════════════

class TestPackaging:
    """Verify pyproject.toml entry points and Dockerfile exist."""

    def test_pyproject_entry_points(self):
        import tomllib
        with open("pyproject.toml", "rb") as f:
            cfg = tomllib.load(f)
        scripts = cfg.get("project", {}).get("scripts", {})
        assert "qm-server" in scripts
        assert scripts["qm-server"] == "qm_app:main"

    def test_pyproject_version(self):
        import tomllib
        with open("pyproject.toml", "rb") as f:
            cfg = tomllib.load(f)
        assert cfg["project"]["version"] == "5.4.0"

    def test_dockerfile_exists(self):
        assert os.path.isfile("Dockerfile")

    def test_import_auth(self):
        from qm_core.auth import (
            UserCatalog, UserRecord, AuthSession, AuthError,
            Role, check_permission,
        )
        assert UserCatalog is not None
        assert Role.ADMIN.value == 3

    def test_import_bench(self):
        from qm_core.bench import BenchResult, run_all, bench_sql_parse
        assert BenchResult is not None

    def test_qmvir_entry_point(self):
        import tomllib
        with open("pyproject.toml", "rb") as f:
            cfg = tomllib.load(f)
        scripts = cfg.get("project", {}).get("scripts", {})
        assert "qmvir" in scripts
        assert scripts["qmvir"] == "qm_app:main"

    def test_qmvir_project_name(self):
        import tomllib
        with open("pyproject.toml", "rb") as f:
            cfg = tomllib.load(f)
        assert cfg["project"]["name"] == "qmvir"


# ═══════════════════════════════════════════════════════════════════════
# 12. QMvir CLI — Parser, SQL Shell, Dashboard
# ═══════════════════════════════════════════════════════════════════════

class TestQMVirCLI:
    """Verify the qmvir CLI parser exposes all six subcommands."""

    def test_parser_prog_name(self):
        from qm_app import _build_parser
        parser = _build_parser()
        assert parser.prog == "qmvir"

    def test_subcommand_start(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["start"])
        assert args.command == "start"

    def test_subcommand_stop(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["stop"])
        assert args.command == "stop"

    def test_subcommand_status(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["status"])
        assert args.command == "status"

    def test_subcommand_version(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["version"])
        assert args.command == "version"

    def test_subcommand_sql(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["sql", "-u", "root", "-p", "pw"])
        assert args.command == "sql"
        assert args.user == "root"
        assert args.password == "pw"

    def test_subcommand_sql_defaults(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["sql"])
        assert args.command == "sql"
        assert args.user == "admin"
        assert args.password == ""

    def test_subcommand_dash(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["dash", "--refresh", "2.5"])
        assert args.command == "dash"
        assert args.refresh == 2.5

    def test_subcommand_dash_default(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["dash"])
        assert args.command == "dash"
        assert args.refresh == 1.0

    def test_subcommand_dash_chaos(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["dash-chaos", "--refresh", "0"])
        assert args.command == "dash-chaos"
        assert args.refresh == 0.0

    def test_subcommand_dash_chaos_with_data_dir(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args([
            "dash-chaos",
            "--data-dir",
            "/tmp/qm_chaos_run",
            "--report",
            "QMVIR_CHAOS_REPORT.json",
        ])
        assert args.command == "dash-chaos"
        assert args.dash_data_dir == "/tmp/qm_chaos_run"

    def test_subcommand_logs(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["logs", "--layer", "gateway-rust", "--tail", "50"])
        assert args.command == "logs"
        assert args.layer == "gateway-rust"
        assert args.tail == 50

    def test_subcommand_logs_with_data_dir_override(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args([
            "logs",
            "--layer",
            "all",
            "--data-dir",
            "/tmp/qm_chaos_run",
        ])
        assert args.command == "logs"
        assert args.logs_data_dir == "/tmp/qm_chaos_run"

    def test_subcommand_bench(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["bench"])
        assert args.command == "bench"

    def test_epilog_mentions_qmvir(self):
        from qm_app import _build_parser
        parser = _build_parser()
        assert "qmvir" in parser.epilog


class TestSQLShell:
    """Test the interactive SQL shell components."""

    def test_import_sql_shell(self):
        from qm_core.cli.sql_shell import SQLShell, run_sql_shell
        assert SQLShell is not None
        assert callable(run_sql_shell)

    def test_keyword_completer(self):
        from qm_core.cli.sql_shell import qm_completer, _QM_KEYWORDS
        assert len(_QM_KEYWORDS) > 30
        # Should have core QM keywords
        kw_lower = [k.lower() for k in _QM_KEYWORDS]
        assert "select" in kw_lower
        assert "create" in kw_lower
        assert "insert" in kw_lower
        assert "vector" in kw_lower
        assert "search" in kw_lower

    def test_format_table_basic(self):
        from qm_core.cli.sql_shell import format_table
        out = format_table(["id", "name"], [[1, "alice"], [2, "bob"]])
        assert "id" in out
        assert "name" in out
        assert "alice" in out
        assert "bob" in out

    def test_format_table_empty(self):
        from qm_core.cli.sql_shell import format_table
        out = format_table(["col"], [])
        assert "col" in out

    def test_format_table_alignment(self):
        from qm_core.cli.sql_shell import format_table
        out = format_table(["x"], [[12345]])
        lines = out.strip().split("\n")
        # header + separator + data = at least 3 lines
        assert len(lines) >= 3

    def test_history_file_path(self):
        from qm_core.cli.sql_shell import HISTORY_FILE
        assert ".qmvir_history" in str(HISTORY_FILE)

    def test_slash_commands_help(self, capsys):
        """Verify SQLShell._handle_slash prints help for /h."""
        from qm_core.cli.sql_shell import SQLShell
        from unittest.mock import MagicMock
        engine = MagicMock()
        session = MagicMock()
        catalog = MagicMock()
        shell = SQLShell(engine, session, catalog)
        result = shell._handle_slash("/h")
        assert result is True
        captured = capsys.readouterr().out
        assert "/d" in captured
        assert "/q" in captured

    def test_slash_d_tables(self, capsys):
        """Verify /d lists tables from engine."""
        from qm_core.cli.sql_shell import SQLShell
        from unittest.mock import MagicMock
        engine = MagicMock()
        # Mock table metadata with row_count attribute
        meta_users = MagicMock()
        meta_users.row_count = 10
        meta_items = MagicMock()
        meta_items.row_count = 5
        engine._tables = {"users": meta_users, "items": meta_items}
        session = MagicMock()
        catalog = MagicMock()
        shell = SQLShell(engine, session, catalog)
        result = shell._handle_slash("/d")
        assert result is True
        captured = capsys.readouterr().out
        assert "users" in captured
        assert "items" in captured

    def test_slash_u_users(self, capsys):
        """Verify /u lists users from catalog."""
        from qm_core.cli.sql_shell import SQLShell
        from unittest.mock import MagicMock
        engine = MagicMock()
        session = MagicMock()
        catalog = MagicMock()
        # list_users returns UserRecord objects with .username and .role
        user1 = MagicMock()
        user1.username = "admin"
        user1.role = MagicMock()
        user1.role.name = "ADMIN"
        user2 = MagicMock()
        user2.username = "reader"
        user2.role = MagicMock()
        user2.role.name = "READER"
        catalog.list_users.return_value = [user1, user2]
        shell = SQLShell(engine, session, catalog)
        result = shell._handle_slash("/u")
        assert result is True
        captured = capsys.readouterr().out
        assert "admin" in captured
        assert "reader" in captured

    def test_slash_q_raises_eof(self):
        """Verify /q raises EOFError to signal quit."""
        from qm_core.cli.sql_shell import SQLShell
        from unittest.mock import MagicMock
        import pytest
        shell = SQLShell(MagicMock(), MagicMock(), MagicMock())
        with pytest.raises(EOFError):
            shell._handle_slash("/q")


class TestDashboard:
    """Test the CLI dashboard snapshot and renderer."""

    def test_import_dashboard(self):
        from qm_core.cli.dashboard import (
            DashboardSnapshot, render_dashboard, run_dashboard,
        )
        assert DashboardSnapshot is not None
        assert callable(render_dashboard)
        assert callable(run_dashboard)

    def test_snapshot_no_state(self):
        """Snapshot from non-existent data dir stays offline."""
        from qm_core.cli.dashboard import DashboardSnapshot
        snap = DashboardSnapshot("/tmp/_qm_nonexistent_test_dir_")
        snap.refresh()
        assert snap.alive is False
        assert snap.pid == 0

    def test_snapshot_with_state_file(self):
        """Snapshot reads from a valid state file."""
        import json, tempfile
        from qm_core.cli.dashboard import DashboardSnapshot
        with tempfile.TemporaryDirectory() as d:
            state = {
                "pid": os.getpid(),  # current PID is alive
                "start_time": time.time() - 60,
                "host": "127.0.0.1",
                "port": 55433,
                "engine_version": "0.4.0",
                "media_capacity": 1024,
                "checkpoint_count": 3,
            }
            (Path(d) / "qm_daemon.state").write_text(json.dumps(state))
            snap = DashboardSnapshot(d)
            snap.refresh()
            assert snap.alive is True
            assert snap.pid == os.getpid()
            assert snap.port == 55433
            assert snap.checkpoint_count == 3
            assert snap.uptime > 50

    def test_render_dashboard_offline(self):
        """Render produces ANSI frame even when daemon is offline."""
        from qm_core.cli.dashboard import DashboardSnapshot, render_dashboard
        snap = DashboardSnapshot("/tmp/_qm_nonexistent_test_dir_")
        snap.refresh()
        frame = render_dashboard(snap)
        assert "QMvir Dashboard" in frame
        assert "OFFLINE" in frame

    def test_render_dashboard_online(self):
        """Render produces ANSI frame with ONLINE status."""
        import json, tempfile
        from qm_core.cli.dashboard import DashboardSnapshot, render_dashboard
        with tempfile.TemporaryDirectory() as d:
            state = {
                "pid": os.getpid(),
                "start_time": time.time() - 10,
                "host": "127.0.0.1",
                "port": 55433,
                "engine_version": "0.4.0",
                "media_capacity": 0,
                "checkpoint_count": 0,
            }
            (Path(d) / "qm_daemon.state").write_text(json.dumps(state))
            snap = DashboardSnapshot(d)
            snap.refresh()
            frame = render_dashboard(snap)
            assert "QMvir Dashboard" in frame
            assert "ONLINE" in frame
            assert "SATELLITES" in frame
            assert "RING BUFFERS" in frame
            assert "MEDIA SLAB HEAP" in frame

    def test_format_uptime(self):
        from qm_core.cli.dashboard import _format_uptime
        assert _format_uptime(0) == "00:00:00"
        assert _format_uptime(3661) == "01:01:01"
        assert _format_uptime(90061) == "25:01:01"

    def test_bar_rendering(self):
        from qm_core.cli.dashboard import _bar
        b = _bar(0.0, width=10)
        assert "0.0%" in b
        b = _bar(1.0, width=10)
        assert "100.0%" in b
        b = _bar(0.85, width=10)
        assert "hotspot!" in b
        b = _bar(0.5, width=10)
        assert "hotspot!" not in b

    def test_format_bytes(self):
        from qm_core.cli.dashboard import _format_bytes
        assert "B" in _format_bytes(100)
        assert "KB" in _format_bytes(2048)
        assert "MB" in _format_bytes(5 * 1024 * 1024)


# ═══════════════════════════════════════════════════════════════════════
# 13. Automated Benchmark & JSON Export
# ═══════════════════════════════════════════════════════════════════════

class TestBenchExportJSON:
    """Verify bench.export_json and the --only filter."""

    def test_export_json_creates_file(self):
        import json
        from qm_core.bench import BenchResult, export_json
        results = [
            BenchResult(name="fake_bench", ops=100, elapsed_s=0.01,
                        p50_us=5.0, p99_us=15.0, extra="test"),
        ]
        with tempfile.TemporaryDirectory() as td:
            path = os.path.join(td, "report.json")
            export_json(results, path)
            assert os.path.isfile(path)
            data = json.loads(Path(path).read_text())
            assert data["version"] == "1.0.0"
            assert len(data["benchmarks"]) == 1
            assert data["benchmarks"][0]["name"] == "fake_bench"
            assert data["benchmarks"][0]["ops_per_sec"] == 10000.0

    def test_export_json_multiple(self):
        import json
        from qm_core.bench import BenchResult, export_json
        results = [
            BenchResult(name="a", ops=50, elapsed_s=0.05),
            BenchResult(name="b", ops=200, elapsed_s=0.1),
        ]
        with tempfile.TemporaryDirectory() as td:
            path = os.path.join(td, "multi.json")
            export_json(results, path)
            data = json.loads(Path(path).read_text())
            assert len(data["benchmarks"]) == 2
            assert data["benchmarks"][1]["ops_per_sec"] == 2000.0

    def test_run_all_only_parse(self):
        from qm_core.bench import run_all
        results = run_all(only="parse")
        assert len(results) == 1
        assert results[0].name == "sql_parse_lalr"

    def test_run_all_only_unknown_returns_empty(self):
        from qm_core.bench import run_all
        results = run_all(only="nonexistent")
        assert results == []

    def test_bench_subcommand_only_flag(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["bench", "--only", "vector"])
        assert args.command == "bench"
        assert args.only == "vector"

    def test_bench_subcommand_json_flag(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args(["bench", "--json", "out.json"])
        assert args.command == "bench"
        assert args.bench_json == "out.json"

    def test_bench_subcommand_combined_flags(self):
        from qm_app import _build_parser
        args = _build_parser().parse_args([
            "bench", "--only", "ring", "--json", "/tmp/r.json",
        ])
        assert args.only == "ring"
        assert args.bench_json == "/tmp/r.json"


class TestAutoBenchScript:
    """Verify tools/auto_bench.py imports and helpers."""

    def test_import_auto_bench(self):
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools"))
        from auto_bench import run_auto_benchmark, _print_kpi_table, _KPI_MAP
        assert callable(run_auto_benchmark)
        assert callable(_print_kpi_table)
        assert len(_KPI_MAP) >= 5

    def test_kpi_map_keys(self):
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools"))
        from auto_bench import _KPI_MAP
        expected = {
            "ring_buffer_publish_collect",
            "gateway_select_tps",
            "vector_search_1000",
            "checkpoint_full_write",
            "sql_parse_lalr",
        }
        assert expected == set(_KPI_MAP.keys())

    def test_auto_bench_no_server_parse_only(self):
        """Run auto_bench with --no-server --only parse for a fast smoke test."""
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools"))
        from auto_bench import run_auto_benchmark
        with tempfile.TemporaryDirectory() as td:
            json_path = os.path.join(td, "test_report.json")
            results = run_auto_benchmark(
                only="parse",
                json_path=json_path,
                data_dir=td,
                use_server=False,
            )
            assert len(results) == 1
            assert results[0].name == "sql_parse_lalr"
            assert os.path.isfile(json_path)

    def test_auto_bench_parser(self):
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "tools"))
        from auto_bench import _build_parser
        args = _build_parser().parse_args([
            "--only", "gateway", "--no-server", "--json", "x.json",
        ])
        assert args.only == "gateway"
        assert args.no_server is True
        assert args.json == "x.json"
