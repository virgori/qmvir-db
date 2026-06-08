"""Tests for core_db: TransactionEngine (MVCC), WriteAheadLog, CDCStream, SchemaRegistry.

Replaces old test_core.py MVCC/WAL/Schema sections that depended on deleted qm_core.storage.
"""
from __future__ import annotations

import os
import tempfile
import threading
import time

import pytest

from core_db.transaction_engine.mvcc import (
    IsolationLevel,
    TransactionEngine,
    TxnStatus,
    WriteConflictError,
)
from core_db.wal_cdc.wal import CDCStream, OutboxWriter, WALEntryType, WriteAheadLog
from core_db.schema.registry import (
    ColumnDef,
    ColumnType,
    ConstraintType,
    ForeignKeyDef,
    IndexDef,
    IndexType,
    SchemaRegistry,
    TableSchema,
)


# ─────────────────────────────────────────────────────────────────────
# TransactionEngine (MVCC)
# ─────────────────────────────────────────────────────────────────────

class TestTransactionEngine:
    @pytest.fixture
    def engine(self):
        return TransactionEngine()

    def test_begin_returns_active_txn(self, engine):
        txn = engine.begin()
        assert txn.status == TxnStatus.ACTIVE
        assert txn.txn_id > 0

    def test_insert_and_read(self, engine):
        txn = engine.begin()
        engine.insert(txn, "users", "u1", {"name": "alice", "age": 30})
        row = engine.read(txn, "users", "u1")
        assert row is not None
        assert row["name"] == "alice"
        engine.commit(txn)

    def test_read_own_writes(self, engine):
        txn = engine.begin()
        engine.insert(txn, "users", "u1", {"name": "alice"})
        assert engine.read(txn, "users", "u1") is not None
        engine.commit(txn)

    def test_read_nonexistent_returns_none(self, engine):
        txn = engine.begin()
        assert engine.read(txn, "users", "missing") is None
        engine.commit(txn)

    def test_update_changes_data(self, engine):
        txn = engine.begin()
        engine.insert(txn, "users", "u1", {"name": "alice"})
        engine.commit(txn)

        txn2 = engine.begin()
        engine.update(txn2, "users", "u1", {"name": "Alice"})
        row = engine.read(txn2, "users", "u1")
        assert row["name"] == "Alice"
        engine.commit(txn2)

    def test_delete_removes_data(self, engine):
        txn = engine.begin()
        engine.insert(txn, "users", "u1", {"name": "alice"})
        engine.commit(txn)

        txn2 = engine.begin()
        engine.delete(txn2, "users", "u1")
        assert engine.read(txn2, "users", "u1") is None
        engine.commit(txn2)

    def test_rollback_discards_changes(self, engine):
        txn1 = engine.begin()
        engine.insert(txn1, "users", "u1", {"name": "alice"})
        engine.commit(txn1)

        txn2 = engine.begin()
        engine.update(txn2, "users", "u1", {"name": "bob"})
        engine.rollback(txn2)

        txn3 = engine.begin()
        row = engine.read(txn3, "users", "u1")
        assert row is not None
        assert row["name"] == "alice"
        engine.commit(txn3)

    def test_snapshot_isolation(self, engine):
        """A transaction should see data as of its start time."""
        txn1 = engine.begin()
        engine.insert(txn1, "users", "u1", {"name": "alice"})
        engine.commit(txn1)

        txn_reader = engine.begin(IsolationLevel.SNAPSHOT)
        # Concurrent writer inserts after reader starts
        txn_writer = engine.begin()
        engine.insert(txn_writer, "users", "u2", {"name": "bob"})
        engine.commit(txn_writer)

        # Reader should NOT see u2
        assert engine.read(txn_reader, "users", "u2") is None
        # Reader should see u1
        assert engine.read(txn_reader, "users", "u1") is not None
        engine.commit(txn_reader)

    def test_write_conflict_detection(self, engine):
        txn1 = engine.begin()
        engine.insert(txn1, "users", "u1", {"name": "alice"})
        engine.commit(txn1)

        txn_a = engine.begin()
        txn_b = engine.begin()
        engine.update(txn_a, "users", "u1", {"name": "A"})
        engine.update(txn_b, "users", "u1", {"name": "B"})

        engine.commit(txn_a)
        with pytest.raises(WriteConflictError):
            engine.commit(txn_b)

        txn_check = engine.begin()
        assert engine.read(txn_check, "users", "u1")["name"] == "A"
        engine.commit(txn_check)
        assert engine.active_txn_count == 0

    def test_write_conflict_loser_rollback_is_idempotent(self, engine):
        seed = engine.begin()
        engine.insert(seed, "users", "u1", {"name": "seed"})
        engine.commit(seed)

        winner = engine.begin()
        loser = engine.begin()
        engine.update(winner, "users", "u1", {"name": "winner"})
        engine.update(loser, "users", "u1", {"name": "loser"})
        engine.commit(winner)

        with pytest.raises(WriteConflictError):
            engine.commit(loser)
        engine.rollback(loser)

        reader = engine.begin()
        assert engine.read(reader, "users", "u1")["name"] == "winner"
        engine.commit(reader)
        assert engine.active_txn_count == 0

    def test_write_conflict_after_one_transaction_commits(self, engine):
        seed = engine.begin()
        engine.insert(seed, "users", "u1", {"name": "seed"})
        engine.commit(seed)

        stale = engine.begin()
        writer = engine.begin()
        engine.update(writer, "users", "u1", {"name": "committed"})
        engine.commit(writer)

        engine.update(stale, "users", "u1", {"name": "stale"})
        with pytest.raises(WriteConflictError):
            engine.commit(stale)

        reader = engine.begin()
        assert engine.read(reader, "users", "u1")["name"] == "committed"
        engine.commit(reader)

    def test_no_false_write_conflict_for_independent_rows(self, engine):
        seed = engine.begin()
        engine.insert(seed, "users", "u1", {"name": "one"})
        engine.insert(seed, "users", "u2", {"name": "two"})
        engine.commit(seed)

        txn_a = engine.begin()
        txn_b = engine.begin()
        engine.update(txn_a, "users", "u1", {"name": "A"})
        engine.update(txn_b, "users", "u2", {"name": "B"})
        engine.commit(txn_a)
        engine.commit(txn_b)

        reader = engine.begin()
        assert engine.read(reader, "users", "u1")["name"] == "A"
        assert engine.read(reader, "users", "u2")["name"] == "B"
        engine.commit(reader)

    def test_repeated_conflict_does_not_poison_later_transactions(self, engine):
        seed = engine.begin()
        engine.insert(seed, "users", "u1", {"name": "seed"})
        engine.commit(seed)

        for idx in range(3):
            winner = engine.begin()
            loser = engine.begin()
            engine.update(winner, "users", "u1", {"name": f"winner-{idx}"})
            engine.update(loser, "users", "u1", {"name": f"loser-{idx}"})
            engine.commit(winner)
            with pytest.raises(WriteConflictError):
                engine.commit(loser)
            engine.rollback(loser)

        clean = engine.begin()
        engine.update(clean, "users", "u1", {"name": "clean"})
        engine.commit(clean)

        reader = engine.begin()
        assert engine.read(reader, "users", "u1")["name"] == "clean"
        engine.commit(reader)
        assert engine.active_txn_count == 0

    def test_snapshot_does_not_see_version_committed_after_begin(self, engine):
        seed = engine.begin()
        engine.insert(seed, "users", "u1", {"name": "seed"})
        engine.commit(seed)

        writer = engine.begin()
        engine.update(writer, "users", "u1", {"name": "late"})
        reader = engine.begin(IsolationLevel.SNAPSHOT)
        engine.commit(writer)

        assert engine.read(reader, "users", "u1")["name"] == "seed"
        engine.commit(reader)

    def test_multiple_tables(self, engine):
        txn = engine.begin()
        engine.insert(txn, "users", "u1", {"name": "alice"})
        engine.insert(txn, "orders", "o1", {"total": 100})
        engine.commit(txn)

        txn2 = engine.begin()
        assert engine.read(txn2, "users", "u1")["name"] == "alice"
        assert engine.read(txn2, "orders", "o1")["total"] == 100
        engine.commit(txn2)

    def test_get_wal_entries(self, engine):
        txn = engine.begin()
        engine.insert(txn, "users", "u1", {"name": "alice"})
        engine.commit(txn)
        entries = engine.get_wal_entries()
        assert len(entries) > 0

    def test_committed_status(self, engine):
        txn = engine.begin()
        engine.commit(txn)
        assert txn.status == TxnStatus.COMMITTED

    def test_aborted_status(self, engine):
        txn = engine.begin()
        engine.rollback(txn)
        assert txn.status == TxnStatus.ABORTED

    def test_concurrent_inserts_different_keys(self, engine):
        txn_a = engine.begin()
        txn_b = engine.begin()
        engine.insert(txn_a, "t", "k1", {"v": 1})
        engine.insert(txn_b, "t", "k2", {"v": 2})
        engine.commit(txn_a)
        engine.commit(txn_b)

        txn_c = engine.begin()
        assert engine.read(txn_c, "t", "k1")["v"] == 1
        assert engine.read(txn_c, "t", "k2")["v"] == 2
        engine.commit(txn_c)


# ─────────────────────────────────────────────────────────────────────
# WriteAheadLog
# ─────────────────────────────────────────────────────────────────────

class TestWriteAheadLog:
    @pytest.fixture
    def wal(self, tmp_path):
        return WriteAheadLog(wal_dir=str(tmp_path / "wal"))

    def test_append_returns_entry(self, wal):
        entry = wal.append(WALEntryType.INSERT, txn_id=1, table="users", pk="u1",
                           data={"name": "alice"})
        assert entry is not None
        assert entry.entry_type == WALEntryType.INSERT

    def test_lsn_increments(self, wal):
        e1 = wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k1", data={})
        e2 = wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k2", data={})
        assert e2.lsn > e1.lsn

    def test_current_lsn(self, wal):
        lsn0 = wal.current_lsn
        wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k", data={})
        assert wal.current_lsn > lsn0

    def test_get_entries_since(self, wal):
        wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k1", data={"v": 1})
        lsn_mid = wal.current_lsn
        wal.append(WALEntryType.INSERT, txn_id=2, table="t", pk="k2", data={"v": 2})

        entries = wal.get_entries_since(lsn_mid)
        assert len(entries) >= 1

    def test_checkpoint(self, wal):
        wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k", data={})
        cp = wal.checkpoint(txn_id=1)
        assert cp.entry_type == WALEntryType.CHECKPOINT

    def test_replay(self, wal):
        wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k1", data={"v": 1})
        wal.append(WALEntryType.UPDATE, txn_id=1, table="t", pk="k1", data={"v": 2})
        wal.append(WALEntryType.COMMIT, txn_id=1)

        replayed = []
        count = wal.replay(lambda e: replayed.append(e))
        assert count >= 3
        assert len(replayed) >= 3

    def test_different_entry_types(self, wal):
        wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k", data={})
        wal.append(WALEntryType.UPDATE, txn_id=1, table="t", pk="k", data={"x": 1})
        wal.append(WALEntryType.DELETE, txn_id=1, table="t", pk="k")
        wal.append(WALEntryType.COMMIT, txn_id=1)
        entries = wal.get_entries_since(0)
        assert len(entries) >= 4


class TestCDCStream:
    def test_poll_returns_events(self, tmp_path):
        wal = WriteAheadLog(wal_dir=str(tmp_path / "wal"))
        cdc = CDCStream(wal)
        wal.append(WALEntryType.INSERT, txn_id=1, table="t", pk="k", data={"v": 1})
        wal.append(WALEntryType.COMMIT, txn_id=1)
        events = cdc.poll()
        assert isinstance(events, list)


class TestOutboxWriter:
    def test_write_and_get_unprocessed(self):
        outbox = OutboxWriter()
        entry = outbox.write("User", "u1", "UserCreated", {"name": "alice"})
        assert entry is not None

        unprocessed = outbox.get_unprocessed()
        assert len(unprocessed) >= 1

    def test_mark_processed(self):
        outbox = OutboxWriter()
        entry = outbox.write("Order", "o1", "OrderPlaced", {"total": 100})
        outbox.mark_processed(entry.entry_id)

        unprocessed = outbox.get_unprocessed()
        processed_ids = {e.entry_id for e in unprocessed}
        assert entry.entry_id not in processed_ids


# ─────────────────────────────────────────────────────────────────────
# SchemaRegistry
# ─────────────────────────────────────────────────────────────────────

class TestSchemaRegistry:
    @pytest.fixture
    def registry(self):
        return SchemaRegistry()

    @pytest.fixture
    def user_schema(self):
        return TableSchema(
            name="users",
            columns=[
                ColumnDef(name="id", col_type=ColumnType.UUID, constraints=[ConstraintType.PRIMARY_KEY]),
                ColumnDef(name="name", col_type=ColumnType.TEXT, constraints=[ConstraintType.NOT_NULL]),
                ColumnDef(name="email", col_type=ColumnType.TEXT),
                ColumnDef(name="age", col_type=ColumnType.INT64),
            ],
            multi_tenant=False,
            soft_delete=False,
            audit_fields=False,
        )

    def test_register_and_get(self, registry, user_schema):
        registry.register(user_schema)
        schema = registry.get("users")
        assert schema is not None
        assert schema.name == "users"
        assert len(schema.columns) == 4

    def test_list_tables(self, registry, user_schema):
        registry.register(user_schema)
        tables = registry.list_tables()
        assert "users" in tables

    def test_get_missing_returns_none(self, registry):
        assert registry.get("nonexistent") is None

    def test_validate_data_valid(self, registry, user_schema):
        registry.register(user_schema)
        errors = registry.validate_data("users", {"id": "u1", "name": "alice", "email": "a@b.com", "age": 30})
        assert errors == []

    def test_validate_data_missing_not_null(self, registry, user_schema):
        registry.register(user_schema)
        errors = registry.validate_data("users", {"id": "u1"})
        assert len(errors) > 0  # name is NOT_NULL

    def test_get_primary_key(self, user_schema):
        pks = user_schema.get_primary_key()
        assert "id" in pks

    def test_multiple_schemas(self, registry, user_schema):
        orders_schema = TableSchema(
            name="orders",
            columns=[
                ColumnDef(name="id", col_type=ColumnType.UUID, constraints=[ConstraintType.PRIMARY_KEY]),
                ColumnDef(name="total", col_type=ColumnType.FLOAT64),
            ],
            multi_tenant=False,
            soft_delete=False,
            audit_fields=False,
        )
        registry.register(user_schema)
        registry.register(orders_schema)
        assert len(registry.list_tables()) == 2

    def test_schema_with_index(self, registry):
        schema = TableSchema(
            name="products",
            columns=[
                ColumnDef(name="id", col_type=ColumnType.UUID, constraints=[ConstraintType.PRIMARY_KEY]),
                ColumnDef(name="name", col_type=ColumnType.TEXT),
            ],
            indexes=[IndexDef(name="idx_name", columns=["name"], index_type=IndexType.BTREE)],
            multi_tenant=False,
            soft_delete=False,
            audit_fields=False,
        )
        registry.register(schema)
        s = registry.get("products")
        assert len(s.indexes) == 1
        assert s.indexes[0].name == "idx_name"

    def test_schema_with_foreign_key(self, registry, user_schema):
        registry.register(user_schema)
        orders_schema = TableSchema(
            name="orders",
            columns=[
                ColumnDef(name="id", col_type=ColumnType.UUID, constraints=[ConstraintType.PRIMARY_KEY]),
                ColumnDef(name="user_id", col_type=ColumnType.UUID),
            ],
            foreign_keys=[ForeignKeyDef(name="fk_user", columns=["user_id"], ref_table="users", ref_columns=["id"])],
            multi_tenant=False,
            soft_delete=False,
            audit_fields=False,
        )
        registry.register(orders_schema)
        s = registry.get("orders")
        assert len(s.foreign_keys) == 1
