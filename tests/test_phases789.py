"""Tests for QM Phase 7/8/9 — Wire & Harden, Ultra I/O, Query Features.

Covers:
    1. Concurrency primitives (RWLock, WaitDiePolicy)
    2. MVCC write-write conflict detection
    3. Buffer pool deadlock-free eviction
    4. SQL parsing (SELECT, INSERT, UPDATE, DELETE, JOIN, GROUP BY, etc.)
    5. Volcano JOIN operators (Hash, Merge, NestedLoop)
    6. Window functions (ROW_NUMBER, RANK, SUM OVER)
    7. CTE + DISTINCT + UNION
    8. Schema constraints (NOT NULL, UNIQUE, PK, CHECK)
    9. MMapSegmentReader
    10. WAL write-combining buffer
    11. Native bridge fallbacks
    12. execute_sql() end-to-end
"""

from __future__ import annotations

import os
import tempfile
import threading
import time

import pytest

# ── 1. Concurrency Primitives ──────────────────────────────────────

from qm_core.concurrency import (
    RWLock, LatchCoupling, WaitDiePolicy, TransactionAbortError,
    WriteConflictError, BackgroundWorker,
)


class TestRWLock:
    def test_multiple_readers(self):
        lock = RWLock()
        results = []

        def reader(idx):
            with lock.read():
                results.append(f"r{idx}_start")
                time.sleep(0.01)
                results.append(f"r{idx}_end")

        threads = [threading.Thread(target=reader, args=(i,)) for i in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=5)
        # All readers should run (possibly concurrently)
        assert len(results) == 8

    def test_writer_exclusive(self):
        lock = RWLock()
        counter = [0]

        def writer():
            with lock.write():
                v = counter[0]
                time.sleep(0.005)
                counter[0] = v + 1

        threads = [threading.Thread(target=writer) for _ in range(10)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)
        assert counter[0] == 10

    def test_reader_writer_interleave(self):
        lock = RWLock()
        data = []

        def writer():
            with lock.write():
                data.append("write")

        def reader():
            with lock.read():
                data.append("read")

        t1 = threading.Thread(target=writer)
        t2 = threading.Thread(target=reader)
        t3 = threading.Thread(target=writer)
        t1.start(); t1.join(timeout=5)
        t2.start(); t2.join(timeout=5)
        t3.start(); t3.join(timeout=5)
        assert data == ["write", "read", "write"]


class TestWaitDiePolicy:
    def test_older_waits(self):
        policy = WaitDiePolicy()
        # Older txn (ts=1) vs younger holder (ts=5) → older should wait
        should_wait = policy.should_wait(1, 5)
        assert should_wait

    def test_younger_dies(self):
        policy = WaitDiePolicy()
        # Younger txn (ts=5) vs older holder (ts=1) → younger should die (abort)
        should_wait = policy.should_wait(5, 1)
        assert not should_wait  # younger should NOT wait → should abort


class TestBackgroundWorker:
    def test_runs_task(self):
        counter = [0]

        def increment():
            counter[0] += 1

        w = BackgroundWorker(name="test-worker", func=increment, interval_s=0.01)
        w.start()
        time.sleep(0.08)
        w.stop()
        assert counter[0] >= 3


# ── 2. MVCC W-W Conflict ───────────────────────────────────────────

from qm_core.storage.mvcc import MVCCEngine


class TestMVCCConflict:
    def test_first_committer_wins(self):
        mvcc = MVCCEngine()
        t1 = mvcc.begin()
        t2 = mvcc.begin()
        mvcc.insert(t1, "tbl", "x", {"val": "t1"})
        mvcc.insert(t2, "tbl", "x", {"val": "t2"})
        mvcc.commit(t1)  # t1 commits first → succeeds
        with pytest.raises(WriteConflictError):
            mvcc.commit(t2)  # t2 tries to commit same key → conflict

    def test_no_conflict_different_keys(self):
        mvcc = MVCCEngine()
        t1 = mvcc.begin()
        t2 = mvcc.begin()
        mvcc.insert(t1, "tbl", "a", {"v": 1})
        mvcc.insert(t2, "tbl", "b", {"v": 2})
        mvcc.commit(t1)
        mvcc.commit(t2)  # No conflict — different keys

    def test_read_snapshot_isolation(self):
        mvcc = MVCCEngine()
        t1 = mvcc.begin()
        mvcc.insert(t1, "tbl", "k", {"v": "v1"})
        mvcc.commit(t1)

        t2 = mvcc.begin()
        t3 = mvcc.begin()
        mvcc.insert(t3, "tbl", "k", {"v": "v3"})
        mvcc.commit(t3)
        # t2 should still see v1 (snapshot isolation)
        row = mvcc.read(t2, "tbl", "k")
        assert row is not None
        assert row["v"] == "v1"


# ── 3. Buffer Pool ─────────────────────────────────────────────────

from qm_core.storage.buffer_pool import BufferPool


class TestBufferPoolEviction:
    def test_eviction_no_deadlock(self):
        """Ensure eviction works without hanging (deadlock-free)."""
        pages_written: dict[tuple[int, int], bytes] = {}

        def reader(seg_id: int, page_id: int) -> bytes:
            return pages_written.get((seg_id, page_id), b"\x00" * 8192)

        def writer(seg_id: int, page_id: int, data: bytes) -> None:
            pages_written[(seg_id, page_id)] = data

        pool = BufferPool(capacity=4, page_reader=reader, page_writer=writer)
        # Fill pool beyond capacity to trigger eviction
        for i in range(10):
            data = pool.fetch_page(0, i)
            pool.unpin(0, i)
        # If we get here without hanging, the deadlock fix works
        assert pool._stats["evictions"] > 0


# ── 4. SQL Parser ──────────────────────────────────────────────────

from qm_core.execution.sql_parser import (
    SQLParser, SelectStmt, InsertStmt, UpdateStmt, DeleteStmt,
    CreateTableStmt, ColumnRef, Literal, BinaryOp, FunctionCall,
    AliasedExpr, StarExpr, SQLSyntaxError,
)


class TestSQLParser:
    def test_simple_select(self):
        stmt = SQLParser("SELECT name, age FROM users").parse()
        assert isinstance(stmt, SelectStmt)
        assert stmt.from_table == "users"
        assert len(stmt.columns) == 2
        assert isinstance(stmt.columns[0].expr, ColumnRef)
        assert stmt.columns[0].expr.column == "name"

    def test_select_star(self):
        stmt = SQLParser("SELECT * FROM items").parse()
        assert isinstance(stmt, SelectStmt)
        assert isinstance(stmt.columns[0].expr, StarExpr)

    def test_select_where(self):
        stmt = SQLParser("SELECT id FROM users WHERE age > 18").parse()
        assert stmt.where is not None
        assert isinstance(stmt.where, BinaryOp)
        assert stmt.where.op == ">"

    def test_select_order_by_limit(self):
        stmt = SQLParser("SELECT id FROM t ORDER BY id DESC LIMIT 10 OFFSET 5").parse()
        assert stmt.limit == 10
        assert stmt.offset == 5
        assert len(stmt.order_by) == 1
        assert stmt.order_by[0][1] is False  # DESC

    def test_select_group_by_having(self):
        stmt = SQLParser("SELECT dept, COUNT(id) FROM emp GROUP BY dept HAVING COUNT(id) > 5").parse()
        assert len(stmt.group_by) == 1
        assert stmt.having is not None

    def test_select_join(self):
        stmt = SQLParser("SELECT o.id FROM orders o JOIN users u ON o.uid = u.id").parse()
        assert len(stmt.joins) == 1
        assert stmt.joins[0].join_type == "INNER"
        assert stmt.joins[0].table == "users"

    def test_select_left_join(self):
        stmt = SQLParser("SELECT * FROM a LEFT JOIN b ON a.id = b.a_id").parse()
        assert stmt.joins[0].join_type == "LEFT"

    def test_select_distinct(self):
        stmt = SQLParser("SELECT DISTINCT name FROM users").parse()
        assert stmt.distinct is True

    def test_insert(self):
        stmt = SQLParser("INSERT INTO users (name, age) VALUES ('Alice', 30)").parse()
        assert isinstance(stmt, InsertStmt)
        assert stmt.table == "users"
        assert stmt.columns == ["name", "age"]
        assert len(stmt.values) == 1
        assert len(stmt.values[0]) == 2

    def test_update(self):
        stmt = SQLParser("UPDATE users SET age = 31 WHERE name = 'Alice'").parse()
        assert isinstance(stmt, UpdateStmt)
        assert stmt.table == "users"
        assert len(stmt.assignments) == 1
        assert stmt.where is not None

    def test_delete(self):
        stmt = SQLParser("DELETE FROM users WHERE id = 1").parse()
        assert isinstance(stmt, DeleteStmt)
        assert stmt.table == "users"
        assert stmt.where is not None

    def test_create_table(self):
        stmt = SQLParser("""
            CREATE TABLE products (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                price FLOAT
            )
        """).parse()
        assert isinstance(stmt, CreateTableStmt)
        assert stmt.table == "products"
        assert len(stmt.columns) == 3
        assert stmt.columns[0].primary_key is True
        assert stmt.columns[1].nullable is False

    def test_syntax_error(self):
        with pytest.raises(SQLSyntaxError):
            SQLParser("SELECTX foo FROM bar").parse()

    def test_select_with_cte(self):
        sql = "WITH active AS (SELECT id, name FROM users WHERE active = 1) SELECT * FROM active"
        stmt = SQLParser(sql).parse()
        assert isinstance(stmt, SelectStmt)
        assert len(stmt.ctes) == 1
        assert stmt.ctes[0].name == "active"

    def test_select_between(self):
        stmt = SQLParser("SELECT id FROM t WHERE age BETWEEN 18 AND 65").parse()
        assert stmt.where is not None

    def test_select_in_list(self):
        stmt = SQLParser("SELECT id FROM t WHERE status IN ('active', 'pending')").parse()
        assert stmt.where is not None

    def test_select_like(self):
        stmt = SQLParser("SELECT id FROM t WHERE name LIKE 'Al%'").parse()
        assert stmt.where is not None

    def test_select_is_null(self):
        stmt = SQLParser("SELECT id FROM t WHERE email IS NULL").parse()
        assert stmt.where is not None

    def test_alias(self):
        stmt = SQLParser("SELECT name AS user_name FROM users").parse()
        assert stmt.columns[0].alias == "user_name"


# ── 5. JOIN Operators ──────────────────────────────────────────────

from qm_core.execution.join import (
    ScanOperator, FilterOperator, ProjectOperator, LimitOperator,
    HashJoin, MergeJoin, NestedLoopJoin, SortOperator,
    HashAggregateOperator, JoinType,
)


class TestJoinOperators:
    @pytest.fixture
    def users(self):
        return [
            {"id": 1, "name": "Alice", "dept": "eng"},
            {"id": 2, "name": "Bob", "dept": "eng"},
            {"id": 3, "name": "Charlie", "dept": "sales"},
        ]

    @pytest.fixture
    def orders(self):
        return [
            {"oid": 101, "user_id": 1, "amount": 100},
            {"oid": 102, "user_id": 1, "amount": 200},
            {"oid": 103, "user_id": 2, "amount": 50},
            {"oid": 104, "user_id": 99, "amount": 10},  # no matching user
        ]

    def test_scan_operator(self, users):
        op = ScanOperator(users)
        assert list(op) == users

    def test_filter_operator(self, users):
        op = ScanOperator(users)
        filt = FilterOperator(op, lambda r: r["dept"] == "eng")
        result = list(filt)
        assert len(result) == 2
        assert all(r["dept"] == "eng" for r in result)

    def test_project_operator(self, users):
        op = ScanOperator(users)
        proj = ProjectOperator(op, ["name"])
        result = list(proj)
        assert result == [{"name": "Alice"}, {"name": "Bob"}, {"name": "Charlie"}]

    def test_limit_operator(self, users):
        op = ScanOperator(users)
        lim = LimitOperator(op, 2)
        result = list(lim)
        assert len(result) == 2

    def test_limit_with_offset(self, users):
        op = ScanOperator(users)
        lim = LimitOperator(op, 1, offset=1)
        result = list(lim)
        assert len(result) == 1
        assert result[0]["name"] == "Bob"

    def test_sort_operator(self, users):
        op = ScanOperator(users)
        sort = SortOperator(op, [("name", False)])  # DESC
        result = list(sort)
        assert result[0]["name"] == "Charlie"
        assert result[-1]["name"] == "Alice"

    def test_hash_join_inner(self, users, orders):
        left = ScanOperator(orders)
        right = ScanOperator(users)
        join = HashJoin(left, right, "user_id", "id", JoinType.INNER)
        result = list(join)
        assert len(result) == 3  # oid 101,102,103 match users 1,2
        assert all("name" in r for r in result)

    def test_hash_join_left(self, users, orders):
        left = ScanOperator(orders)
        right = ScanOperator(users)
        join = HashJoin(left, right, "user_id", "id", JoinType.LEFT)
        result = list(join)
        assert len(result) == 4  # All orders, even oid=104 with no user match

    def test_hash_join_right(self, users, orders):
        left = ScanOperator(orders)
        right = ScanOperator(users)
        join = HashJoin(left, right, "user_id", "id", JoinType.RIGHT)
        result = list(join)
        # Charlie (id=3) has no orders → should appear once with NULL order fields
        charlie_rows = [r for r in result if r.get("name") == "Charlie"]
        assert len(charlie_rows) == 1

    def test_nested_loop_join(self, users, orders):
        left = ScanOperator(orders)
        right = ScanOperator(users)
        join = NestedLoopJoin(left, right, 
                              predicate=lambda row: row["user_id"] == row["id"],
                              join_type=JoinType.INNER)
        result = list(join)
        assert len(result) == 3

    def test_merge_join(self):
        left = ScanOperator([
            {"k": 1, "v": "a"},
            {"k": 2, "v": "b"},
            {"k": 3, "v": "c"},
        ])
        right = ScanOperator([
            {"k": 2, "v": "x"},
            {"k": 3, "v": "y"},
            {"k": 4, "v": "z"},
        ])
        join = MergeJoin(left, right, "k", "k")
        result = list(join)
        assert len(result) == 2  # k=2 and k=3

    def test_hash_aggregate(self, users):
        op = ScanOperator(users)
        agg = HashAggregateOperator(op, ["dept"], [("COUNT", "id", "cnt")])
        result = list(agg)
        by_dept = {r["dept"]: r["cnt"] for r in result}
        assert by_dept["eng"] == 2
        assert by_dept["sales"] == 1


# ── 6. Window Functions ───────────────────────────────────────────

from qm_core.execution.window import (
    WindowOperator, WindowSpec, CTEOperator, DistinctOperator, UnionOperator,
)


class TestWindowFunctions:
    @pytest.fixture
    def rows(self):
        return [
            {"dept": "eng", "name": "Alice", "salary": 100},
            {"dept": "eng", "name": "Bob", "salary": 120},
            {"dept": "sales", "name": "Charlie", "salary": 90},
            {"dept": "sales", "name": "Diana", "salary": 110},
        ]

    def test_row_number(self, rows):
        spec = WindowSpec(partition_by=["dept"], order_by=[("salary", True)])
        op = ScanOperator(rows)
        win = WindowOperator(op, [("ROW_NUMBER", None, "rn", spec)])
        result = list(win)
        assert len(result) == 4
        # Within each dept, row_number should be 1 and 2
        eng_rns = sorted(r["rn"] for r in result if r["dept"] == "eng")
        assert eng_rns == [1, 2]

    def test_rank(self, rows):
        spec = WindowSpec(partition_by=["dept"], order_by=[("salary", True)])
        op = ScanOperator(rows)
        win = WindowOperator(op, [("RANK", None, "rank", spec)])
        result = list(win)
        assert all(r["rank"] >= 1 for r in result)

    def test_sum_over(self, rows):
        spec = WindowSpec(partition_by=["dept"], order_by=[("salary", True)])
        op = ScanOperator(rows)
        win = WindowOperator(op, [("SUM", "salary", "running_total", spec)])
        result = list(win)
        eng_rows = sorted((r for r in result if r["dept"] == "eng"), key=lambda r: r["salary"])
        assert eng_rows[0]["running_total"] == 100  # first row
        assert eng_rows[1]["running_total"] == 220  # 100 + 120

    def test_distinct_operator(self):
        op = ScanOperator([{"a": 1}, {"a": 2}, {"a": 1}, {"a": 3}, {"a": 2}])
        distinct = DistinctOperator(op)
        result = list(distinct)
        assert len(result) == 3

    def test_cte_operator(self):
        base = [{"id": 1, "x": 10}, {"id": 2, "x": 20}]

        def main_factory(materialized):
            return ScanOperator(materialized["my_cte"])

        cte = CTEOperator(
            cte_defs={"my_cte": ScanOperator(base)},
            main_query=main_factory,
        )
        result = list(cte)
        assert result == base

    def test_union_all(self):
        left = ScanOperator([{"a": 1}, {"a": 2}])
        right = ScanOperator([{"a": 2}, {"a": 3}])
        union = UnionOperator(left, right, distinct=False)
        result = list(union)
        assert len(result) == 4

    def test_union_distinct(self):
        left = ScanOperator([{"a": 1}, {"a": 2}])
        right = ScanOperator([{"a": 2}, {"a": 3}])
        union = UnionOperator(left, right, distinct=True)
        result = list(union)
        assert len(result) == 3


# ── 7. Schema & Constraints ───────────────────────────────────────

from qm_core.schema import (
    Catalog, TableDef, ColumnSchema, DataType, ConstraintChecker,
    ConstraintViolation, validate_type,
)


class TestSchema:
    def test_validate_type(self):
        assert validate_type(42, DataType.INTEGER) is True
        assert validate_type("hello", DataType.TEXT) is True
        assert validate_type(3.14, DataType.FLOAT) is True
        assert validate_type("not_int", DataType.INTEGER) is False

    def test_not_null(self):
        cols = [ColumnSchema("name", DataType.TEXT, nullable=False)]
        tdef = TableDef("users", cols)
        checker = ConstraintChecker(tdef)
        with pytest.raises(ConstraintViolation):
            checker.validate_insert({"name": None})

    def test_unique(self):
        cols = [ColumnSchema("email", DataType.TEXT, unique=True)]
        tdef = TableDef("users", cols)
        checker = ConstraintChecker(tdef)
        checker.validate_insert({"email": "a@b.com"})
        checker.register_row({"email": "a@b.com"})
        with pytest.raises(ConstraintViolation):
            checker.validate_insert({"email": "a@b.com"})

    def test_primary_key(self):
        cols = [ColumnSchema("id", DataType.INTEGER, primary_key=True)]
        tdef = TableDef("items", cols)
        checker = ConstraintChecker(tdef)
        checker.validate_insert({"id": 1})
        checker.register_row({"id": 1})
        with pytest.raises(ConstraintViolation):
            checker.validate_insert({"id": 1})  # duplicate PK

    def test_default_value(self):
        cols = [ColumnSchema("status", DataType.TEXT, default="active")]
        tdef = TableDef("items", cols)
        checker = ConstraintChecker(tdef)
        row = {"name": "test"}
        row = checker.apply_defaults(row)
        assert row["status"] == "active"

    def test_catalog(self):
        catalog = Catalog()
        cols = [ColumnSchema("id", DataType.INTEGER)]
        tdef = TableDef("test_table", cols)
        catalog.create_table(tdef)
        assert catalog.get_table("test_table") is tdef
        catalog.drop_table("test_table")
        assert catalog.get_table("test_table") is None


# ── 8. MMapSegmentReader ──────────────────────────────────────────

from qm_core.storage.segments import SegmentManager


class TestMMapSegmentReader:
    def test_mmap_reader_import(self):
        """MMapSegmentReader should be importable."""
        from qm_core.storage.segments import MMapSegmentReader
        assert MMapSegmentReader is not None

    def test_segment_write_read(self):
        """SegmentManager write via SegmentWriter then read."""
        with tempfile.TemporaryDirectory() as tmpdir:
            mgr = SegmentManager(data_dir=tmpdir)
            writer = mgr.new_writer()
            key = b"test_key"
            data = b"hello world payload"
            writer.add_row(key, data)
            writer.finish()
            meta = mgr.register(writer)
            # Verify file was created
            assert os.path.exists(meta.path)


# ── 9. WAL Write-Combining ────────────────────────────────────────

from qm_core.storage.wal import WriteAheadLog, WALOp


class TestWALWriteCombining:
    def test_buffered_writes(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            wal = WriteAheadLog(wal_dir=tmpdir, fsync_per_write=False)
            wal.open()
            # Write multiple small records — should be buffered
            for i in range(50):
                wal.append(WALOp.INSERT, txn_id=i, table=f"table_{i}", key=str(i), data={"val": i})
            # Force flush
            wal.group_commit()
            wal.close()

    def test_wal_append_and_replay(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            wal = WriteAheadLog(wal_dir=tmpdir, fsync_per_write=False)
            wal.open()
            wal.append(WALOp.INSERT, txn_id=1, table="t", key="k", data={"v": "v"})
            wal.group_commit()
            records = wal.replay()
            assert len(records) >= 1
            wal.close()


# ── 10. Native Bridge ─────────────────────────────────────────────

from qm_core.native.bridge import (
    bitmap_and_native, bitmap_or_native, bitmap_popcount_native,
    bm25_score_block_native, batch_l2_native, crc32_native,
)


class TestNativeBridge:
    def test_bitmap_and(self):
        a = bytes([0xFF, 0x0F, 0xAA])
        b = bytes([0xF0, 0xFF, 0x55])
        result = bitmap_and_native(a, b)
        assert result == bytes([0xF0, 0x0F, 0x00])

    def test_bitmap_or(self):
        a = bytes([0xF0, 0x0F])
        b = bytes([0x0F, 0xF0])
        result = bitmap_or_native(a, b)
        assert result == bytes([0xFF, 0xFF])

    def test_bitmap_popcount(self):
        data = bytes([0xFF, 0x00, 0x0F])
        count = bitmap_popcount_native(data)
        assert count == 12  # 8 + 0 + 4

    def test_crc32(self):
        data = b"hello world"
        crc = crc32_native(data)
        assert isinstance(crc, int)
        assert crc != 0

    def test_batch_l2(self):
        import numpy as np
        query = np.array([[1.0, 0.0, 0.0, 0.0]], dtype=np.float32)
        vecs = np.array([[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]], dtype=np.float32)
        result = batch_l2_native(query, vecs)
        assert result.shape == (1, 2)
        assert abs(result[0, 0]) < 1e-6  # same vector → distance 0
        assert result[0, 1] > 0  # different vector → distance > 0


# ── 11. B+Tree Thread-Safety ──────────────────────────────────────

from qm_core.index.btree import BPlusTree


class TestBTreeThreadSafety:
    def test_concurrent_inserts(self):
        tree = BPlusTree(order=32)
        errors = []

        def inserter(start, count):
            try:
                for i in range(start, start + count):
                    tree.insert(i, f"val_{i}")
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=inserter, args=(i * 100, 100)) for i in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)
        assert not errors
        # All keys should be findable
        for i in range(400):
            assert tree.get(i) == f"val_{i}"

    def test_concurrent_read_write(self):
        tree = BPlusTree(order=32)
        for i in range(100):
            tree.insert(i, f"v{i}")

        results = []
        errors = []

        def reader():
            try:
                for i in range(100):
                    tree.get(i)
                results.append("ok")
            except Exception as e:
                errors.append(e)

        def writer():
            try:
                for i in range(100, 200):
                    tree.insert(i, f"v{i}")
            except Exception as e:
                errors.append(e)

        threads = [threading.Thread(target=reader) for _ in range(3)]
        threads.append(threading.Thread(target=writer))
        for t in threads:
            t.start()
        for t in threads:
            t.join(timeout=10)
        assert not errors
        assert len(results) == 3


# ── 12. Roaring Bitmap popcount fix ───────────────────────────────

from qm_core.index.roaring import RoaringBitmap


class TestRoaringPopcount:
    def test_popcount_correct(self):
        bm = RoaringBitmap()
        for i in range(1000):
            bm.add(i)
        assert len(bm) == 1000

    def test_contains(self):
        bm = RoaringBitmap()
        bm.add(42)
        bm.add(100)
        assert bm.contains(42)
        assert bm.contains(100)
        assert not bm.contains(43)


# ── 13. End-to-End SQL via Engine ─────────────────────────────────

from qm_core.engine import QMEngine


class TestExecuteSQL:
    @pytest.fixture
    def engine(self, tmp_path):
        eng = QMEngine(str(tmp_path))
        eng.create_table("users", schema={"name": "text", "age": "int", "dept": "text"})
        eng.insert("users", {"name": "Alice", "age": 30, "dept": "eng"})
        eng.insert("users", {"name": "Bob", "age": 25, "dept": "eng"})
        eng.insert("users", {"name": "Charlie", "age": 35, "dept": "sales"})
        eng.insert("users", {"name": "Diana", "age": 28, "dept": "sales"})
        return eng

    def test_select_all(self, engine):
        result = engine.execute_sql("SELECT * FROM users")
        assert len(result) == 4

    def test_select_where(self, engine):
        result = engine.execute_sql("SELECT name FROM users WHERE age > 28")
        names = {r["name"] for r in result}
        assert "Alice" in names
        assert "Charlie" in names
        assert "Bob" not in names

    def test_select_order_by(self, engine):
        result = engine.execute_sql("SELECT name FROM users ORDER BY age ASC")
        names = [r["name"] for r in result]
        assert names[0] == "Bob"  # youngest
        assert names[-1] == "Charlie"  # oldest

    def test_select_limit(self, engine):
        result = engine.execute_sql("SELECT name FROM users LIMIT 2")
        assert len(result) == 2

    def test_insert_sql(self, engine):
        result = engine.execute_sql("INSERT INTO users (name, age, dept) VALUES ('Eve', 22, 'hr')")
        assert result[0]["inserted"] == 1
        # Verify
        all_rows = engine.execute_sql("SELECT * FROM users")
        assert len(all_rows) == 5

    def test_update_sql(self, engine):
        result = engine.execute_sql("UPDATE users SET age = 31 WHERE name = 'Alice'")
        assert result[0]["updated"] == 1
        # Verify
        alice = engine.execute_sql("SELECT age FROM users WHERE name = 'Alice'")
        assert alice[0]["age"] == 31

    def test_delete_sql(self, engine):
        result = engine.execute_sql("DELETE FROM users WHERE name = 'Bob'")
        assert result[0]["deleted"] == 1
        all_rows = engine.execute_sql("SELECT * FROM users")
        assert len(all_rows) == 3

    def test_select_distinct(self, engine):
        result = engine.execute_sql("SELECT DISTINCT dept FROM users")
        depts = {r["dept"] for r in result}
        assert depts == {"eng", "sales"}

    def test_group_by(self, engine):
        result = engine.execute_sql("SELECT dept, COUNT(name) FROM users GROUP BY dept")
        by_dept = {r["dept"]: r.get("count_name", r.get("COUNT_name")) for r in result}
        assert by_dept["eng"] == 2
        assert by_dept["sales"] == 2

    def test_create_table_sql(self, engine):
        result = engine.execute_sql("""
            CREATE TABLE products (
                id INTEGER PRIMARY KEY,
                name TEXT,
                price FLOAT
            )
        """)
        assert result[0]["created"] == "products"

    def test_join_sql(self, engine):
        engine.create_table("orders", schema={"user_name": "text", "amount": "int"})
        engine.insert("orders", {"user_name": "Alice", "amount": 100})
        engine.insert("orders", {"user_name": "Bob", "amount": 200})
        result = engine.execute_sql(
            "SELECT u.name, o.amount FROM users u JOIN orders o ON u.name = o.user_name"
        )
        assert len(result) >= 2

    def test_point_query_sql_by_primary_key(self, engine):
        engine.create_table("tickets", schema={"id": "int", "title": "text"}, primary_key="id")
        engine.insert("tickets", {"id": 11, "title": "hello"})
        engine.insert("tickets", {"id": 12, "title": "world"})

        r = engine.execute_sql("SELECT title FROM tickets WHERE id = 12 LIMIT 1")
        assert len(r) == 1
        assert r[0]["title"] == "world"

    def test_syntax_error(self, engine):
        with pytest.raises(ValueError, match="SQL syntax error"):
            engine.execute_sql("SELECTX foo FROM bar")

    def test_between(self, engine):
        result = engine.execute_sql("SELECT name FROM users WHERE age BETWEEN 25 AND 30")
        names = {r["name"] for r in result}
        assert "Bob" in names
        assert "Alice" in names
        assert "Charlie" not in names

    def test_like(self, engine):
        result = engine.execute_sql("SELECT name FROM users WHERE name LIKE 'A%'")
        names = {r["name"] for r in result}
        assert names == {"Alice"}

    def test_is_null(self, engine):
        engine.insert("users", {"name": "NullAge", "dept": "test"})
        result = engine.execute_sql("SELECT name FROM users WHERE age IS NULL")
        names = {r["name"] for r in result}
        assert "NullAge" in names

    def test_in_list(self, engine):
        result = engine.execute_sql("SELECT name FROM users WHERE dept IN ('eng')")
        names = {r["name"] for r in result}
        assert names == {"Alice", "Bob"}


# ── 14. HNSW Thread-Safety ────────────────────────────────────────

from qm_core.index.hnsw import HNSWIndex


class TestHNSWThreadSafety:
    def test_concurrent_add_search(self):
        import numpy as np
        hnsw = HNSWIndex(dim=4, M=16)
        errors = []

        def adder():
            try:
                import random
                for i in range(50):
                    vec = np.array([random.random() for _ in range(4)], dtype=np.float32)
                    hnsw.add(i + 1000, vec)
            except Exception as e:
                errors.append(e)

        def searcher():
            try:
                for _ in range(20):
                    hnsw.search(np.array([0.5, 0.5, 0.5, 0.5], dtype=np.float32), top_k=3)
            except Exception as e:
                errors.append(e)

        t1 = threading.Thread(target=adder)
        t2 = threading.Thread(target=searcher)
        t1.start(); t2.start()
        t1.join(timeout=10); t2.join(timeout=10)
        assert not errors


# ── 15. Inverted Index Thread-Safety ──────────────────────────────

from qm_core.index.inverted import InvertedIndex


class TestInvertedThreadSafety:
    def test_concurrent_add_search(self):
        idx = InvertedIndex()
        errors = []

        def adder():
            try:
                for i in range(30):
                    idx.add_document(i, {"content": f"hello world test document {i}"})
            except Exception as e:
                errors.append(e)

        def searcher():
            try:
                for _ in range(10):
                    idx.search_daat("hello", top_k=5)
            except Exception as e:
                errors.append(e)

        t1 = threading.Thread(target=adder)
        t2 = threading.Thread(target=searcher)
        t1.start(); t2.start()
        t1.join(timeout=10); t2.join(timeout=10)
        assert not errors
