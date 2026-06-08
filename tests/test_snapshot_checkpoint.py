"""Test snapshot/checkpoint persistence via NativeSqlEngine."""

import os
import tempfile
import pytest
import qm_engine


@pytest.fixture
def data_dir(tmp_path):
    return str(tmp_path / "qm_data")


class TestCheckpoint:
    def test_checkpoint_creates_snapshot_file(self, data_dir):
        """checkpoint() should create native_sql.snap."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE t (id INTEGER, val TEXT)")
        engine.execute("INSERT INTO t (id, val) VALUES (1, 'hello')")
        engine.checkpoint()

        snap_path = os.path.join(data_dir, "native_sql.snap")
        assert os.path.isfile(snap_path)
        assert os.path.getsize(snap_path) > 0

    def test_checkpoint_truncates_wal(self, data_dir):
        """checkpoint() should truncate the WAL file."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE t (id INTEGER, val TEXT)")
        for i in range(20):
            engine.execute(f"INSERT INTO t (id, val) VALUES ({i}, 'v{i}')")

        wal_path = os.path.join(data_dir, "native_sql.wal")
        assert os.path.isfile(wal_path)
        wal_size_before = os.path.getsize(wal_path)
        assert wal_size_before > 0

        engine.checkpoint()

        # After checkpoint, WAL should be truncated (empty or near-empty).
        wal_size_after = os.path.getsize(wal_path)
        assert wal_size_after < wal_size_before

    def test_checkpoint_requires_data_dir(self):
        """checkpoint() on in-memory engine should raise."""
        engine = qm_engine.NativeSqlEngine()
        with pytest.raises(RuntimeError, match="data_dir"):
            engine.checkpoint()

    def test_checkpoint_idempotent(self, data_dir):
        """Multiple checkpoints should not corrupt data."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE t (id INTEGER, val TEXT)")
        engine.execute("INSERT INTO t (id, val) VALUES (1, 'a')")
        engine.checkpoint()
        engine.execute("INSERT INTO t (id, val) VALUES (2, 'b')")
        engine.checkpoint()
        engine.checkpoint()  # No changes — should be safe.

        # Reload and verify.
        engine2 = qm_engine.NativeSqlEngine(data_dir)
        _, rows, _ = engine2.execute("SELECT COUNT(*) FROM t")
        assert rows[0][0] == "2"


class TestPersistence:
    def test_data_survives_restart(self, data_dir):
        """Data should survive engine restart via snapshot + WAL."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE users (id INTEGER, name TEXT)")
        for i in range(50):
            engine.execute(f"INSERT INTO users (id, name) VALUES ({i}, 'user_{i}')")
        del engine

        # Reopen — should replay WAL.
        engine2 = qm_engine.NativeSqlEngine(data_dir)
        _, rows, _ = engine2.execute("SELECT COUNT(*) FROM users")
        assert rows[0][0] == "50"

    def test_data_survives_checkpoint_then_restart(self, data_dir):
        """Checkpoint then restart — snapshot path."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE items (id INTEGER, name TEXT, price TEXT)")
        for i in range(100):
            engine.execute(f"INSERT INTO items (id, name, price) VALUES ({i}, 'item_{i}', '{i * 1.5}')")
        engine.checkpoint()
        del engine

        # Reopen — should load from snapshot.
        engine2 = qm_engine.NativeSqlEngine(data_dir)
        _, rows, _ = engine2.execute("SELECT COUNT(*) FROM items")
        assert rows[0][0] == "100"

        # Verify actual data.
        cols, rows, _ = engine2.execute("SELECT id, name, price FROM items WHERE id = 42")
        assert len(rows) == 1
        # Find the 'name' column index.
        name_idx = cols.index("name")
        assert rows[0][name_idx] == "item_42"

    def test_checkpoint_plus_wal_replay(self, data_dir):
        """Checkpoint, then add more data, restart — should have all data."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE t (id INTEGER, val TEXT)")
        for i in range(10):
            engine.execute(f"INSERT INTO t (id, val) VALUES ({i}, 'before')")
        engine.checkpoint()

        # Add more data after checkpoint (goes to WAL only).
        for i in range(10, 20):
            engine.execute(f"INSERT INTO t (id, val) VALUES ({i}, 'after')")
        del engine

        # Reopen — should load snapshot (10 rows) + replay WAL (10 more).
        engine2 = qm_engine.NativeSqlEngine(data_dir)
        _, rows, _ = engine2.execute("SELECT COUNT(*) FROM t")
        assert rows[0][0] == "20"

    def test_multiple_tables_persist(self, data_dir):
        """Multiple tables should all persist across restarts."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE alpha (id INTEGER, x TEXT)")
        engine.execute("CREATE TABLE beta (id INTEGER, y TEXT)")
        engine.execute("INSERT INTO alpha (id, x) VALUES (1, 'aa')")
        engine.execute("INSERT INTO beta (id, y) VALUES (1, 'bb')")
        engine.checkpoint()
        del engine

        engine2 = qm_engine.NativeSqlEngine(data_dir)
        # Verify both tables exist and have data.
        _, rows_a, _ = engine2.execute("SELECT COUNT(*) FROM alpha")
        _, rows_b, _ = engine2.execute("SELECT COUNT(*) FROM beta")
        assert rows_a[0][0] == "1"
        assert rows_b[0][0] == "1"


class TestSnapshotInfo:
    def test_snapshot_info_in_memory(self):
        """snapshot_info on in-memory engine should show non-persistent status."""
        engine = qm_engine.NativeSqlEngine()
        info = engine.snapshot_info()
        assert info["persistent"] is False
        assert info["table_count"] == 0

    def test_snapshot_info_persistent(self, data_dir):
        """snapshot_info on persistent engine should show file info."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE t (id INTEGER, val TEXT)")
        engine.execute("INSERT INTO t (id, val) VALUES (1, 'x')")
        engine.checkpoint()

        info = engine.snapshot_info()
        assert info["persistent"] is True
        assert info["snapshot_exists"] is True
        assert info["snapshot_size"] > 0
        assert info["table_count"] == 1
        assert info["total_rows"] == 1
        assert info["data_dir"] == data_dir

    def test_snapshot_info_wal_mutations(self, data_dir):
        """snapshot_info should track wal_mutations count."""
        engine = qm_engine.NativeSqlEngine(data_dir)
        engine.execute("CREATE TABLE t (id INTEGER)")
        info1 = engine.snapshot_info()
        mutations1 = info1.get("wal_mutations", 0)

        engine.execute("INSERT INTO t (id) VALUES (1)")
        info2 = engine.snapshot_info()
        mutations2 = info2.get("wal_mutations", 0)
        assert mutations2 > mutations1

        engine.checkpoint()
        info3 = engine.snapshot_info()
        # After checkpoint, mutations should reset.
        assert info3["wal_mutations"] == 0
