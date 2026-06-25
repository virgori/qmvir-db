"""Test Backup & Migration Suite — PyO3 bindings."""

import os
import tempfile
import pytest
import qm_engine


@pytest.fixture
def engine_with_data():
    """Create a NativeSqlEngine with sample tables."""
    engine = qm_engine.NativeSqlEngine()
    engine.execute("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)")
    for i in range(100):
        engine.execute(f"INSERT INTO users (id, name, age) VALUES ({i}, 'user_{i}', {20 + i})")
    engine.execute("CREATE TABLE orders (id INTEGER, user_id INTEGER, amount TEXT)")
    for i in range(50):
        engine.execute(f"INSERT INTO orders (id, user_id, amount) VALUES ({i}, {i % 100}, '99.99')")
    return engine


@pytest.fixture
def backup_path(tmp_path):
    return str(tmp_path / "test.qmvb")


class TestBackup:
    def test_backup_creates_file(self, engine_with_data, backup_path):
        result = qm_engine.backup(engine_with_data, backup_path)
        assert os.path.isfile(backup_path)
        assert result["tables_backed_up"] == 2
        assert result["total_rows"] == 150
        assert result["compressed_size"] > 0
        assert result["path"].endswith(".qmvb")

    def test_backup_lz4_compression(self, engine_with_data, backup_path):
        result = qm_engine.backup(engine_with_data, backup_path, compression="lz4")
        assert result["compressed_size"] <= result["original_size"]

    def test_backup_no_compression(self, engine_with_data, backup_path):
        result = qm_engine.backup(engine_with_data, backup_path, compression="none")
        assert result["compressed_size"] > 0

    def test_backup_single_table(self, engine_with_data, backup_path):
        result = qm_engine.backup(engine_with_data, backup_path, tables=["users"])
        assert result["tables_backed_up"] == 1
        assert result["total_rows"] == 100

    def test_backup_invalid_compression(self, engine_with_data, backup_path):
        with pytest.raises(RuntimeError, match="Unknown compression"):
            qm_engine.backup(engine_with_data, backup_path, compression="brotli")


class TestVerify:
    def test_verify_valid_backup(self, engine_with_data, backup_path):
        qm_engine.backup(engine_with_data, backup_path)
        result = qm_engine.backup_verify(backup_path)
        assert result["ok"] is True
        assert result["header_valid"] is True
        assert result["footer_valid"] is True
        assert result["crc_match"] is True
        assert result["tables"] == 2
        assert result["rows"] == 150

    def test_verify_nonexistent_file(self):
        with pytest.raises(RuntimeError):
            qm_engine.backup_verify("/tmp/nonexistent.qmvb")

    def test_verify_corrupted_file(self, tmp_path):
        bad = str(tmp_path / "bad.qmvb")
        with open(bad, "wb") as f:
            f.write(b"\x00" * 200)
        result = qm_engine.backup_verify(bad)
        assert result["ok"] is False


class TestBackupInfo:
    def test_info_basic(self, engine_with_data, backup_path):
        qm_engine.backup(engine_with_data, backup_path)
        info = qm_engine.backup_info(backup_path)
        assert info["format_version"] == 1
        assert info["table_count"] == 2
        assert info["total_rows"] == 150
        assert info["file_size"] > 0
        assert len(info["tables"]) == 2

    def test_info_table_details(self, engine_with_data, backup_path):
        qm_engine.backup(engine_with_data, backup_path)
        info = qm_engine.backup_info(backup_path)
        table_names = {t["name"] for t in info["tables"]}
        assert "users" in table_names
        assert "orders" in table_names
        users = next(t for t in info["tables"] if t["name"] == "users")
        assert users["row_count"] == 100


class TestRestore:
    def test_restore_roundtrip(self, engine_with_data, backup_path):
        """Backup → restore into new engine → verify data matches."""
        qm_engine.backup(engine_with_data, backup_path)

        new_engine = qm_engine.NativeSqlEngine()
        result = qm_engine.backup_restore(new_engine, backup_path)
        assert result["tables_restored"] == 2
        assert result["total_rows"] == 150

        # Verify users table data
        cols, rows, tag = new_engine.execute("SELECT id, name, age FROM users ORDER BY id LIMIT 5")
        assert len(rows) == 5
        assert rows[0][1] == "user_0"

        # Verify orders table data
        cols, rows, tag = new_engine.execute("SELECT id, user_id, amount FROM orders ORDER BY id LIMIT 3")
        assert len(rows) == 3
        assert rows[0][2] == "99.99"

    def test_restore_with_table_filter(self, engine_with_data, backup_path):
        """Restore only specific tables."""
        qm_engine.backup(engine_with_data, backup_path)

        new_engine = qm_engine.NativeSqlEngine()
        result = qm_engine.backup_restore(new_engine, backup_path, tables=["users"])
        assert result["tables_restored"] == 1
        assert result["total_rows"] == 100

        # users should exist
        cols, rows, tag = new_engine.execute("SELECT COUNT(*) FROM users")
        assert rows[0][0] == "100"

        # orders should NOT exist
        with pytest.raises(RuntimeError):
            new_engine.execute("SELECT * FROM orders")

    def test_restore_drop_existing(self, engine_with_data, backup_path):
        """Restoring with drop_existing replaces existing tables."""
        qm_engine.backup(engine_with_data, backup_path)

        # Create engine with some pre-existing data.
        target = qm_engine.NativeSqlEngine()
        target.execute("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER)")
        target.execute("INSERT INTO users (id, name, age) VALUES (9999, 'old_user', 99)")

        result = qm_engine.backup_restore(target, backup_path, drop_existing=True)
        assert result["tables_restored"] == 2

        # Should have the restored data, not the old data.
        cols, rows, tag = target.execute("SELECT COUNT(*) FROM users")
        assert rows[0][0] == "100"

    def test_restore_without_drop_existing(self, engine_with_data, backup_path):
        """Restoring without drop_existing merges into existing tables."""
        qm_engine.backup(engine_with_data, backup_path)

        target = qm_engine.NativeSqlEngine()
        result = qm_engine.backup_restore(target, backup_path, drop_existing=False)
        assert result["tables_restored"] == 2
        assert result["total_rows"] == 150

    def test_restore_corrupted_file(self, tmp_path):
        """Restoring from a corrupted file should fail."""
        bad = str(tmp_path / "bad.qmvb")
        with open(bad, "wb") as f:
            f.write(b"\x00" * 200)

        engine = qm_engine.NativeSqlEngine()
        with pytest.raises(RuntimeError):
            qm_engine.backup_restore(engine, bad)

    def test_restore_nonexistent_file(self):
        """Restoring from a nonexistent file should fail."""
        engine = qm_engine.NativeSqlEngine()
        with pytest.raises(RuntimeError):
            qm_engine.backup_restore(engine, "/tmp/nonexistent.qmvb")

    def test_restore_zstd_backup(self, engine_with_data, backup_path):
        """Backup with zstd and restore."""
        qm_engine.backup(engine_with_data, backup_path, compression="zstd")

        new_engine = qm_engine.NativeSqlEngine()
        result = qm_engine.backup_restore(new_engine, backup_path)
        assert result["tables_restored"] == 2
        assert result["total_rows"] == 150

    def test_restore_no_compression_backup(self, engine_with_data, backup_path):
        """Backup with no compression and restore."""
        qm_engine.backup(engine_with_data, backup_path, compression="none")

        new_engine = qm_engine.NativeSqlEngine()
        result = qm_engine.backup_restore(new_engine, backup_path)
        assert result["tables_restored"] == 2
        assert result["total_rows"] == 150


class TestVectorBackup:
    def test_vector_column_roundtrip(self, tmp_path):
        """VECTOR columns survive .qmvb backup → restore with correct schema."""
        engine = qm_engine.NativeSqlEngine()
        engine.execute("CREATE TABLE docs (id INTEGER, embedding VECTOR(4))")
        engine.execute(
            "INSERT INTO docs (id, embedding) VALUES (1, '[0.1,0.2,0.3,0.4]')"
        )
        path = str(tmp_path / "vec.qmvb")
        qm_engine.backup(engine, path, compression="lz4")

        restored = qm_engine.NativeSqlEngine()
        result = qm_engine.backup_restore(restored, path, drop_existing=True)
        assert result["tables_restored"] == 1
        assert result["total_rows"] == 1

        info = qm_engine.backup_info(path)
        assert info["format_version"] == 1

        cols, rows, _ = restored.execute(
            "SELECT id, embedding FROM docs ORDER BY id"
        )
        assert len(rows) == 1
        assert rows[0][0] == "1"
