"""Tests for core_db.transaction_engine.mvcc — MVCC transaction engine."""

from __future__ import annotations

import pytest

from core_db.transaction_engine.mvcc import (
    IsolationLevel,
    TransactionEngine,
    TxnStatus,
)


@pytest.fixture
def engine() -> TransactionEngine:
    return TransactionEngine()


class TestTransactionEngine:
    def test_begin_and_commit(self, engine: TransactionEngine) -> None:
        txn = engine.begin()
        assert txn.status == TxnStatus.ACTIVE

        engine.insert(txn, "articles", "a1", {"title": "Hello"})
        engine.commit(txn)
        assert txn.status == TxnStatus.COMMITTED

    def test_read_own_writes(self, engine: TransactionEngine) -> None:
        txn = engine.begin()
        engine.insert(txn, "articles", "a1", {"title": "Hello"})
        row = engine.read(txn, "articles", "a1")
        assert row is not None
        assert row["title"] == "Hello"
        engine.commit(txn)

    def test_snapshot_isolation(self, engine: TransactionEngine) -> None:
        # txn1 inserts, commits
        txn1 = engine.begin()
        engine.insert(txn1, "articles", "a1", {"title": "Original"})
        engine.commit(txn1)

        # txn2 reads, sees committed value
        txn2 = engine.begin()
        row = engine.read(txn2, "articles", "a1")
        assert row is not None
        assert row["title"] == "Original"

        # txn3 updates after txn2 started — txn2 should not see update
        txn3 = engine.begin()
        engine.update(txn3, "articles", "a1", {"title": "Updated"})
        engine.commit(txn3)

        row2 = engine.read(txn2, "articles", "a1")
        assert row2 is not None
        assert row2["title"] == "Original"  # snapshot isolation
        engine.commit(txn2)

    def test_rollback(self, engine: TransactionEngine) -> None:
        txn = engine.begin()
        engine.insert(txn, "articles", "a1", {"title": "Temp"})
        engine.rollback(txn)
        assert txn.status == TxnStatus.ABORTED

        txn2 = engine.begin()
        row = engine.read(txn2, "articles", "a1")
        assert row is None
        engine.commit(txn2)

    def test_delete(self, engine: TransactionEngine) -> None:
        txn1 = engine.begin()
        engine.insert(txn1, "articles", "a1", {"title": "To delete"})
        engine.commit(txn1)

        txn2 = engine.begin()
        engine.delete(txn2, "articles", "a1")
        engine.commit(txn2)

        txn3 = engine.begin()
        assert engine.read(txn3, "articles", "a1") is None
        engine.commit(txn3)

    def test_wal_entries_generated(self, engine: TransactionEngine) -> None:
        txn = engine.begin()
        engine.insert(txn, "articles", "a1", {"title": "Hello"})
        engine.commit(txn)
        entries = engine.get_wal_entries(txn)
        assert len(entries) >= 1
