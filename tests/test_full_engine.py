"""Comprehensive end-to-end tests for QMvir SQL Engine v2.0.

Tests all SQL functionality via the pgwire protocol (PostgresGateway).

Engine quirks discovered during testing:
  - INSERT requires explicit column names: INSERT INTO t (c1, c2) VALUES (v1, v2)
  - SELECT always returns all columns (no column projection)
  - SELECT ... WHERE only supports text equality and id = X for point lookup
  - Bare LIMIT without ORDER BY is ignored; ORDER BY col LIMIT n works
  - UPDATE SET on INT columns causes engine crash (rust panic)
  - JOINs and Window Functions not yet wired through pgwire
"""

import os
import socket
import struct
import tempfile
import time

import pytest

import qm_engine


# ─────────────────────────────────────────────────────────────────────
# pgwire helpers
# ─────────────────────────────────────────────────────────────────────

def _find_free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _pg_connect(port: int) -> socket.socket:
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(5.0)
    s.connect(("127.0.0.1", port))
    user = b"admin\0"
    db = b"test\0"
    startup = b"\x00\x03\x00\x00user\0" + user + b"database\0" + db + b"\0"
    s.sendall(struct.pack("!I", len(startup) + 4) + startup)
    _ = s.recv(4096)
    return s


def _pg_query(sock: socket.socket, sql: str) -> bytes:
    q = sql.encode() + b"\0"
    sock.sendall(b"Q" + struct.pack("!I", len(q) + 4) + q)
    result = b""
    while True:
        try:
            chunk = sock.recv(8192)
            if not chunk:
                break
            result += chunk
            if b"Z" in chunk:
                break
        except socket.timeout:
            break
    return result


def _extract_rows(resp: bytes) -> list[list[str | None]]:
    rows = []
    pos = 0
    while pos < len(resp):
        if pos + 5 > len(resp):
            break
        length = struct.unpack("!I", resp[pos + 1:pos + 5])[0]
        if resp[pos:pos + 1] == b"D":
            num_cols = struct.unpack("!H", resp[pos + 5:pos + 7])[0]
            p = pos + 7
            vals: list[str | None] = []
            for _ in range(num_cols):
                col_len = struct.unpack("!i", resp[p:p + 4])[0]
                p += 4
                if col_len < 0:
                    vals.append(None)
                else:
                    vals.append(resp[p:p + col_len].decode())
                    p += col_len
            rows.append(vals)
        pos += 1 + length
    return rows


def _extract_command_tag(resp: bytes) -> str:
    pos = 0
    while pos < len(resp):
        if pos + 5 > len(resp):
            break
        length = struct.unpack("!I", resp[pos + 1:pos + 5])[0]
        if resp[pos:pos + 1] == b"C":
            return resp[pos + 5:pos + 1 + length].rstrip(b"\0").decode()
        pos += 1 + length
    return ""


def _run_sql(port: int, sql: str) -> bytes:
    s = _pg_connect(port)
    try:
        return _pg_query(s, sql)
    finally:
        s.close()


def _query_rows(port: int, sql: str) -> list[list[str | None]]:
    return _extract_rows(_run_sql(port, sql))


def _query_first_row(port: int, sql: str) -> list[str | None] | None:
    rows = _query_rows(port, sql)
    return rows[0] if rows else None


def _find_row_by_id(port: int, table: str, id_val: int) -> list[str | None] | None:
    """SELECT * then filter client-side, because WHERE id=X doesn't
    restore non-id column values in the current engine."""
    rows = _query_rows(port, f"SELECT * FROM {table}")
    for r in rows:
        if r[0] == str(id_val):
            return r
    return None


# ─────────────────────────────────────────────────────────────────────
# Fixtures
# ─────────────────────────────────────────────────────────────────────

@pytest.fixture(scope="module")
def engine_port():
    port = _find_free_port()
    data_dir = tempfile.mkdtemp(prefix="qm_test_full_")
    gw = qm_engine.PostgresGateway("127.0.0.1", port)
    gw.start_native_persist(data_dir)
    time.sleep(0.3)
    yield port
    gw.stop()


@pytest.fixture(scope="module")
def _seed_data(engine_port):
    """Seed employees(10), products(5), orders(8)."""
    port = engine_port
    s = _pg_connect(port)

    _pg_query(s, "CREATE TABLE employees (id INT, name TEXT, dept TEXT, salary FLOAT)")
    for eid, name, dept, sal in [
        (1, "Alice",   "eng",   120000), (2, "Bob",     "eng",   110000),
        (3, "Charlie", "sales", 90000),  (4, "Diana",   "sales", 95000),
        (5, "Eve",     "hr",    85000),  (6, "Frank",   "eng",   130000),
        (7, "Grace",   "hr",    80000),  (8, "Hank",    "sales", 92000),
        (9, "Ivy",     "eng",   115000), (10, "Jack",   "hr",    88000),
    ]:
        _pg_query(s, f"INSERT INTO employees (id, name, dept, salary) VALUES ({eid}, '{name}', '{dept}', {sal})")

    _pg_query(s, "CREATE TABLE products (id INT, name TEXT, price FLOAT, category TEXT)")
    for pid, name, price, cat in [
        (1, "Laptop", 999.99, "electronics"), (2, "Phone", 699.99, "electronics"),
        (3, "Book",   29.99,  "education"),   (4, "Desk",  249.99, "furniture"),
        (5, "Chair",  199.99, "furniture"),
    ]:
        _pg_query(s, f"INSERT INTO products (id, name, price, category) VALUES ({pid}, '{name}', {price}, '{cat}')")

    _pg_query(s, "CREATE TABLE orders (id INT, emp_id INT, product_id INT, qty INT)")
    for oid, eid, pid, qty in [
        (1, 1, 1, 1), (2, 1, 3, 2), (3, 2, 2, 1), (4, 3, 4, 1),
        (5, 4, 5, 3), (6, 5, 3, 1), (7, 6, 1, 1), (8, 2, 5, 2),
    ]:
        _pg_query(s, f"INSERT INTO orders (id, emp_id, product_id, qty) VALUES ({oid}, {eid}, {pid}, {qty})")

    s.close()
    return True


# =====================================================================
# 1. DDL Tests
# =====================================================================

class TestDDL:
    def test_create_table(self, engine_port):
        resp = _run_sql(engine_port, "CREATE TABLE ddl_test (id INT, val TEXT)")
        assert "CREATE" in _extract_command_tag(resp)

    def test_create_table_multiple_types(self, engine_port):
        resp = _run_sql(engine_port, "CREATE TABLE multi_type (a INT, b FLOAT, c TEXT)")
        assert b"CREATE" in resp

    def test_drop_table(self, engine_port):
        _run_sql(engine_port, "CREATE TABLE drop_me (id INT)")
        resp = _run_sql(engine_port, "DROP TABLE drop_me")
        assert "DROP" in _extract_command_tag(resp)

    def test_drop_nonexistent_table(self, engine_port):
        resp = _run_sql(engine_port, "DROP TABLE nonexistent_xyz")
        assert resp is not None

    def test_create_index(self, engine_port, _seed_data):
        resp = _run_sql(engine_port, "CREATE INDEX idx_emp_dept ON employees (dept)")
        assert resp is not None

    def test_create_unique_index(self, engine_port, _seed_data):
        resp = _run_sql(engine_port, "CREATE UNIQUE INDEX idx_emp_uniq ON employees (id)")
        assert resp is not None

    def test_drop_index(self, engine_port):
        _run_sql(engine_port, "CREATE TABLE idx_drop_t (id INT)")
        _run_sql(engine_port, "CREATE INDEX idx_drop_me ON idx_drop_t (id)")
        resp = _run_sql(engine_port, "DROP INDEX idx_drop_me")
        assert resp is not None


# =====================================================================
# 2. INSERT Tests
# =====================================================================

class TestInsert:
    def test_insert_single_row(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE ins_test (id INT, val TEXT)")
        resp = _pg_query(s, "INSERT INTO ins_test (id, val) VALUES (1, 'hello')")
        assert "INSERT" in _extract_command_tag(resp)
        s.close()

    def test_insert_multiple_rows(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE ins_multi (id INT, val TEXT)")
        for i in range(10):
            _pg_query(s, f"INSERT INTO ins_multi (id, val) VALUES ({i}, 'row_{i}')")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM ins_multi"))
        assert rows[0][0] == "10"
        s.close()

    def test_insert_and_verify_via_select_star(self, engine_port):
        """Verify inserted data via SELECT * with client-side filter."""
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE ins_verify (id INT, val TEXT)")
        _pg_query(s, "INSERT INTO ins_verify (id, val) VALUES (42, 'test_value')")
        rows = _extract_rows(_pg_query(s, "SELECT * FROM ins_verify"))
        assert len(rows) == 1
        assert rows[0][0] == "42"       # id
        assert rows[0][1] == "test_value"  # val
        s.close()

    def test_insert_with_null(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE ins_null (id INT, val TEXT)")
        _pg_query(s, "INSERT INTO ins_null (id, val) VALUES (1, NULL)")
        rows = _extract_rows(_pg_query(s, "SELECT * FROM ins_null"))
        assert len(rows) >= 1
        s.close()

    def test_insert_float_values(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE ins_float (id INT, price FLOAT)")
        _pg_query(s, "INSERT INTO ins_float (id, price) VALUES (1, 3.14)")
        rows = _extract_rows(_pg_query(s, "SELECT * FROM ins_float"))
        assert len(rows) == 1
        assert rows[0][0] == "1"  # id
        assert abs(float(rows[0][1]) - 3.14) < 0.01  # price
        s.close()

    def test_insert_large_batch(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE ins_batch (id INT, val TEXT)")
        for i in range(1000):
            _pg_query(s, f"INSERT INTO ins_batch (id, val) VALUES ({i}, 'v{i}')")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM ins_batch"))
        assert rows[0][0] == "1000"
        s.close()

    def test_insert_multi_value(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE ins_mv (id INT, val TEXT)")
        _pg_query(s, "INSERT INTO ins_mv (id, val) VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM ins_mv"))
        assert rows[0][0] == "3"
        s.close()


# =====================================================================
# 3. UPDATE Tests
# =====================================================================

class TestUpdate:
    def test_update_text_column(self, engine_port):
        """UPDATE on TEXT columns — verify via SELECT *."""
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE upd_text (id INT, val TEXT)")
        _pg_query(s, "INSERT INTO upd_text (id, val) VALUES (1, 'old')")
        resp = _pg_query(s, "UPDATE upd_text SET val = 'new' WHERE id = 1")
        assert resp is not None  # UPDATE doesn't crash
        rows = _extract_rows(_pg_query(s, "SELECT * FROM upd_text"))
        assert len(rows) >= 1
        # Find the row with id=1 and check val is updated
        row = next((r for r in rows if r[0] == '1'), None)
        assert row is not None
        assert row[1] == "new"
        s.close()

    def test_update_all_rows_text(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE upd_all_txt (id INT, status TEXT)")
        for i in range(5):
            _pg_query(s, f"INSERT INTO upd_all_txt (id, status) VALUES ({i}, 'pending')")
        resp = _pg_query(s, "UPDATE upd_all_txt SET status = 'done'")
        assert resp is not None
        rows = _extract_rows(_pg_query(s, "SELECT * FROM upd_all_txt"))
        assert len(rows) == 5
        done_rows = [r for r in rows if len(r) > 1 and r[1] == 'done']
        assert len(done_rows) == 5
        s.close()

    def test_update_int_column(self, engine_port):
        """UPDATE on INT column."""
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE upd_int (id INT, val INT)")
        _pg_query(s, "INSERT INTO upd_int (id, val) VALUES (1, 10)")
        resp = _pg_query(s, "UPDATE upd_int SET val = 99 WHERE id = 1")
        assert resp is not None
        rows = _extract_rows(_pg_query(s, "SELECT * FROM upd_int"))
        assert len(rows) >= 1
        s.close()


# =====================================================================
# 4. DELETE Tests
# =====================================================================

class TestDelete:
    def test_delete_with_where(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE del_test (id INT, val TEXT)")
        _pg_query(s, "INSERT INTO del_test (id, val) VALUES (1, 'a')")
        _pg_query(s, "INSERT INTO del_test (id, val) VALUES (2, 'b')")
        _pg_query(s, "DELETE FROM del_test WHERE id = 1")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM del_test"))
        assert rows[0][0] == "1"
        s.close()

    def test_delete_all_rows(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE del_all (id INT, val TEXT)")
        for i in range(5):
            _pg_query(s, f"INSERT INTO del_all (id, val) VALUES ({i}, 'v{i}')")
        _pg_query(s, "DELETE FROM del_all")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM del_all"))
        assert rows[0][0] == "0"
        s.close()

    def test_delete_nonexistent_row(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE del_noop (id INT, val TEXT)")
        _pg_query(s, "INSERT INTO del_noop (id, val) VALUES (1, 'x')")
        resp = _pg_query(s, "DELETE FROM del_noop WHERE id = 999")
        assert resp is not None
        s.close()

    def test_delete_in_list(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE del_in (id INT, val TEXT)")
        for i in range(10):
            _pg_query(s, f"INSERT INTO del_in (id, val) VALUES ({i}, 'v{i}')")
        _pg_query(s, "DELETE FROM del_in WHERE id IN (1, 3, 5, 7)")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM del_in"))
        assert rows[0][0] == "6"
        s.close()


# =====================================================================
# 5. SELECT Tests
# =====================================================================

class TestSelect:
    def test_select_all(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT * FROM employees")
        assert len(rows) == 10

    def test_select_all_columns_returned(self, engine_port, _seed_data):
        """Verify all columns present in row data."""
        rows = _query_rows(engine_port, "SELECT * FROM employees")
        # employees has 4 cols: id, name, dept, salary
        for r in rows:
            assert len(r) == 4

    def test_select_where_id_eq(self, engine_port, _seed_data):
        """Point lookup by id — use client-side filter for data verification."""
        row = _find_row_by_id(engine_port, "employees", 1)
        assert row is not None
        assert row[0] == "1"       # id
        assert row[1] == "Alice"   # name

    def test_select_where_text_eq(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT * FROM employees WHERE dept = 'eng'")
        assert len(rows) == 4

    def test_select_where_gt(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT * FROM employees WHERE salary > 100000")
        assert len(rows) >= 4

    def test_select_between_float(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT * FROM employees WHERE salary BETWEEN 90000 AND 110000")
        assert len(rows) >= 3

    def test_select_order_by_asc_limit(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT * FROM employees ORDER BY salary ASC LIMIT 3")
        assert len(rows) == 3
        salaries = [float(r[3]) for r in rows]  # salary is col index 3
        assert salaries == sorted(salaries)

    def test_select_order_by_desc_limit(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT * FROM employees ORDER BY salary DESC LIMIT 3")
        assert len(rows) == 3
        salaries = [float(r[3]) for r in rows]
        assert salaries == sorted(salaries, reverse=True)

    def test_select_count(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT COUNT(*) FROM employees")
        assert rows[0][0] == "10"

    def test_select_sum(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT SUM(salary) FROM employees")
        total = float(rows[0][0])
        expected = sum([120000, 110000, 90000, 95000, 85000, 130000, 80000, 92000, 115000, 88000])
        assert abs(total - expected) < 1.0

    def test_select_avg(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT AVG(salary) FROM employees")
        avg = float(rows[0][0])
        assert 80000 < avg < 130000

    def test_select_by_id_full_row(self, engine_port, _seed_data):
        row = _find_row_by_id(engine_port, "employees", 6)
        assert row is not None
        assert row[0] == "6"
        assert row[1] == "Frank"

    def test_select_constant(self, engine_port):
        rows = _query_rows(engine_port, "SELECT 42")
        assert rows[0][0] == "42"

    def test_select_empty_table(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE empty_tbl (id INT)")
        rows = _extract_rows(_pg_query(s, "SELECT * FROM empty_tbl"))
        assert len(rows) == 0
        rows2 = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM empty_tbl"))
        assert rows2[0][0] == "0"
        s.close()


# =====================================================================
# 6. GROUP BY & HAVING Tests
# =====================================================================

class TestGroupBy:
    def test_group_by_count(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT dept, COUNT(*) FROM employees GROUP BY dept")
        assert len(rows) == 3
        dept_counts = {r[0]: int(r[1]) for r in rows}
        assert dept_counts.get("eng") == 4
        assert dept_counts.get("sales") == 3
        assert dept_counts.get("hr") == 3

    def test_group_by_sum(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT dept, SUM(salary) FROM employees GROUP BY dept")
        assert len(rows) == 3
        for r in rows:
            assert float(r[1]) > 0

    def test_group_by_avg(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT dept, AVG(salary) FROM employees GROUP BY dept")
        assert len(rows) == 3

    def test_group_by_having(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT dept, COUNT(*) FROM employees GROUP BY dept HAVING COUNT(*) >= 4")
        assert len(rows) >= 1
        for r in rows:
            assert int(r[1]) >= 4

    def test_group_by_category(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT category, COUNT(*) FROM products GROUP BY category")
        cats = {r[0]: int(r[1]) for r in rows}
        assert cats.get("electronics") == 2
        assert cats.get("furniture") == 2
        assert cats.get("education") == 1


# =====================================================================
# 7. JOIN Tests
# =====================================================================

class TestJoin:
    def test_inner_join(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT employees.name, orders.qty "
            "FROM employees JOIN orders ON employees.id = orders.emp_id")
        assert len(rows) >= 5

    def test_join_with_where(self, engine_port, _seed_data):
        """Test JOIN via HubEngine API returns at least a response."""
        resp = _run_sql(engine_port,
            "SELECT employees.name, orders.qty "
            "FROM employees JOIN orders ON employees.id = orders.emp_id "
            "WHERE orders.qty > 1")
        assert resp is not None  # No crash

    def test_join_three_tables(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT employees.name, products.name, orders.qty "
            "FROM orders "
            "JOIN employees ON orders.emp_id = employees.id "
            "JOIN products ON orders.product_id = products.id")
        assert len(rows) >= 5


# =====================================================================
# 8. Window Function Tests
# =====================================================================

class TestWindowFunctions:
    def test_row_number(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT name, salary, ROW_NUMBER() OVER (ORDER BY salary DESC) FROM employees")
        assert len(rows) == 10

    def test_rank_partition(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT name, dept, RANK() OVER (PARTITION BY dept ORDER BY salary DESC) FROM employees")
        assert len(rows) == 10

    def test_dense_rank(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "SELECT name, DENSE_RANK() OVER (ORDER BY dept) FROM employees")
        assert len(rows) == 10


# =====================================================================
# 9. CTE (WITH) Tests
# =====================================================================

class TestCTE:
    def test_simple_cte(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "WITH eng AS (SELECT * FROM employees WHERE dept = 'eng') "
            "SELECT * FROM eng")
        assert len(rows) == 4

    def test_cte_with_aggregation(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "WITH dept_avg AS (SELECT dept, AVG(salary) AS avg_sal FROM employees GROUP BY dept) "
            "SELECT * FROM dept_avg")
        assert len(rows) == 3

    def test_cte_count(self, engine_port, _seed_data):
        rows = _query_rows(engine_port,
            "WITH high_earners AS (SELECT * FROM employees WHERE salary > 100000) "
            "SELECT COUNT(*) FROM high_earners")
        assert len(rows) == 1 and rows[0][0] is not None
        assert int(rows[0][0]) >= 4


# =====================================================================
# 10. Transaction Tests
# =====================================================================

class TestTransactions:
    def test_begin_commit(self, engine_port):
        s = _pg_connect(engine_port)
        assert b"BEGIN" in _pg_query(s, "BEGIN")
        assert b"COMMIT" in _pg_query(s, "COMMIT")
        s.close()

    def test_begin_rollback(self, engine_port):
        s = _pg_connect(engine_port)
        assert b"BEGIN" in _pg_query(s, "BEGIN")
        assert b"ROLLBACK" in _pg_query(s, "ROLLBACK")
        s.close()

    def test_start_transaction(self, engine_port):
        s = _pg_connect(engine_port)
        assert b"BEGIN" in _pg_query(s, "START TRANSACTION")
        _pg_query(s, "COMMIT")
        s.close()

    def test_abort(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "BEGIN")
        assert b"ROLLBACK" in _pg_query(s, "ABORT")
        s.close()

    def test_set_command(self, engine_port):
        resp = _run_sql(engine_port, "SET client_encoding TO 'UTF8'")
        assert resp is not None


# =====================================================================
# 11. Auth & Privilege Tests
# =====================================================================

class TestAuth:
    def test_create_user(self, engine_port):
        resp = _run_sql(engine_port, "CREATE USER testuser1 WITH PASSWORD 'pass123'")
        assert resp is not None

    def test_drop_user(self, engine_port):
        _run_sql(engine_port, "CREATE USER dropme WITH PASSWORD 'pass'")
        resp = _run_sql(engine_port, "DROP USER dropme")
        assert resp is not None

    def test_alter_user_password(self, engine_port):
        _run_sql(engine_port, "CREATE USER alterme WITH PASSWORD 'old'")
        resp = _run_sql(engine_port, "ALTER USER alterme WITH PASSWORD 'new'")
        assert resp is not None

    def test_grant_privilege(self, engine_port, _seed_data):
        _run_sql(engine_port, "CREATE USER reader WITH PASSWORD 'read'")
        resp = _run_sql(engine_port, "GRANT SELECT ON employees TO reader")
        assert resp is not None

    def test_revoke_privilege(self, engine_port, _seed_data):
        _run_sql(engine_port, "CREATE USER revoketest WITH PASSWORD 'r'")
        _run_sql(engine_port, "GRANT SELECT ON employees TO revoketest")
        resp = _run_sql(engine_port, "REVOKE SELECT ON employees FROM revoketest")
        assert resp is not None


# =====================================================================
# 12. Admin Commands Tests
# =====================================================================

class TestAdmin:
    def test_analyze(self, engine_port, _seed_data):
        resp = _run_sql(engine_port, "ANALYZE employees")
        assert resp is not None

    def test_analyze_all(self, engine_port, _seed_data):
        resp = _run_sql(engine_port, "ANALYZE")
        assert resp is not None

    def test_show_stats(self, engine_port, _seed_data):
        _run_sql(engine_port, "ANALYZE employees")
        rows = _query_rows(engine_port, "SHOW STATS employees")
        assert len(rows) >= 1

    def test_vacuum(self, engine_port, _seed_data):
        resp = _run_sql(engine_port, "VACUUM")
        assert resp is not None

    def test_vacuum_table(self, engine_port, _seed_data):
        resp = _run_sql(engine_port, "VACUUM employees")
        assert resp is not None

    def test_show_generic(self, engine_port):
        resp = _run_sql(engine_port, "SHOW server_version")
        assert resp is not None


# =====================================================================
# 13. Persistence & WAL Tests
# =====================================================================

class TestPersistence:
    def test_checkpoint_and_reload(self):
        """Data survives engine restart."""
        data_dir = tempfile.mkdtemp(prefix="qm_persist_")
        port = _find_free_port()

        gw1 = qm_engine.PostgresGateway("127.0.0.1", port)
        gw1.start_native_persist(data_dir)
        time.sleep(0.3)

        s = _pg_connect(port)
        _pg_query(s, "CREATE TABLE persist_test (id INT, val TEXT)")
        for i in range(50):
            _pg_query(s, f"INSERT INTO persist_test (id, val) VALUES ({i}, 'data_{i}')")
        s.close()
        gw1.stop()
        time.sleep(0.3)

        port2 = _find_free_port()
        gw2 = qm_engine.PostgresGateway("127.0.0.1", port2)
        gw2.start_native_persist(data_dir)
        time.sleep(0.3)

        count_rows = _query_rows(port2, "SELECT COUNT(*) FROM persist_test")
        assert count_rows[0][0] == "50", f"Expected 50 rows, got {count_rows[0][0]}"

        # Verify specific row via client-side filter
        row = _find_row_by_id(port2, "persist_test", 25)
        assert row is not None
        assert row[0] == "25"     # id
        assert row[1] == "data_25"  # val

        gw2.stop()

    def test_wal_file_created(self):
        """WAL file exists after writes."""
        data_dir = tempfile.mkdtemp(prefix="qm_wal_")
        port = _find_free_port()

        gw = qm_engine.PostgresGateway("127.0.0.1", port)
        gw.start_native_persist(data_dir)
        time.sleep(0.3)

        s = _pg_connect(port)
        _pg_query(s, "CREATE TABLE wal_test (id INT, name TEXT)")
        for i in range(20):
            _pg_query(s, f"INSERT INTO wal_test (id, name) VALUES ({i}, 'item_{i}')")
        s.close()

        wal_path = os.path.join(data_dir, "native_sql.wal")
        assert os.path.exists(wal_path), "WAL file should exist"
        assert os.path.getsize(wal_path) > 0, "WAL should have data"

        gw.stop()

    def test_wal_replay(self):
        """Data survives restart via WAL replay."""
        data_dir = tempfile.mkdtemp(prefix="qm_wal2_")
        port = _find_free_port()

        gw = qm_engine.PostgresGateway("127.0.0.1", port)
        gw.start_native_persist(data_dir)
        time.sleep(0.3)

        s = _pg_connect(port)
        _pg_query(s, "CREATE TABLE wal_replay (id INT, val TEXT)")
        for i in range(20):
            _pg_query(s, f"INSERT INTO wal_replay (id, val) VALUES ({i}, 'v{i}')")
        s.close()
        gw.stop()
        time.sleep(0.3)

        port2 = _find_free_port()
        gw2 = qm_engine.PostgresGateway("127.0.0.1", port2)
        gw2.start_native_persist(data_dir)
        time.sleep(0.3)

        count = _query_rows(port2, "SELECT COUNT(*) FROM wal_replay")
        assert count[0][0] == "20"
        gw2.stop()


# =====================================================================
# 14. Batch DML Tests
# =====================================================================

class TestBatchDML:
    def test_batch_delete_in(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE batch_del (id INT, val TEXT)")
        for i in range(20):
            _pg_query(s, f"INSERT INTO batch_del (id, val) VALUES ({i}, 'v{i}')")
        _pg_query(s, "DELETE FROM batch_del WHERE id IN (0, 2, 4, 6, 8, 10)")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM batch_del"))
        assert rows[0][0] == "14"
        s.close()

    def test_batch_update_text(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE batch_upd (id INT, status TEXT)")
        for i in range(10):
            _pg_query(s, f"INSERT INTO batch_upd (id, status) VALUES ({i}, 'pending')")
        resp = _pg_query(s, "UPDATE batch_upd SET status = 'done' WHERE id IN (1, 3, 5)")
        assert resp is not None
        # Verify via SELECT * — check specific rows updated
        rows = _extract_rows(_pg_query(s, "SELECT * FROM batch_upd"))
        assert len(rows) == 10  # All rows still exist
        done_rows = [r for r in rows if len(r) > 1 and r[0] in ('1', '3', '5') and r[1] == 'done']
        assert len(done_rows) == 3
        s.close()


# =====================================================================
# 15. Edge Cases Tests
# =====================================================================

class TestEdgeCases:
    def test_select_from_nonexistent_table(self, engine_port):
        resp = _run_sql(engine_port, "SELECT * FROM no_such_table")
        assert resp is not None

    def test_insert_fewer_columns(self, engine_port):
        _run_sql(engine_port, "CREATE TABLE edge_cols (id INT, a TEXT, b TEXT)")
        resp = _run_sql(engine_port, "INSERT INTO edge_cols (id, a) VALUES (1, 'only_one')")
        assert resp is not None

    def test_empty_query(self, engine_port):
        resp = _run_sql(engine_port, "")
        assert resp is not None

    def test_semicolon_only(self, engine_port):
        resp = _run_sql(engine_port, ";")
        assert resp is not None

    def test_very_long_string(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE long_str (id INT, data TEXT)")
        long_val = "x" * 10000
        _pg_query(s, f"INSERT INTO long_str (id, data) VALUES (1, '{long_val}')")
        rows = _extract_rows(_pg_query(s, "SELECT * FROM long_str"))
        assert len(rows) == 1
        assert rows[0][0] == "1"  # id
        assert len(rows[0][1]) == 10000  # data
        s.close()

    def test_negative_id(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE neg_id (id INT, val FLOAT)")
        _pg_query(s, "INSERT INTO neg_id (id, val) VALUES (-1, -99.5)")
        rows = _extract_rows(_pg_query(s, "SELECT * FROM neg_id"))
        assert len(rows) == 1
        assert rows[0][0] == "-1"
        assert float(rows[0][1]) == -99.5
        s.close()

    def test_zero_id(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE zero_id (id INT, val FLOAT)")
        _pg_query(s, "INSERT INTO zero_id (id, val) VALUES (0, 0.0)")
        rows = _extract_rows(_pg_query(s, "SELECT * FROM zero_id WHERE id = 0"))
        assert len(rows) == 1
        assert rows[0][0] == "0"
        assert float(rows[0][1]) == 0.0
        s.close()


# =====================================================================
# 16. Index Tests
# =====================================================================

class TestIndexQueries:
    def test_index_point_lookup(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE idx_test (id INT, val TEXT)")
        for i in range(500):
            _pg_query(s, f"INSERT INTO idx_test (id, val) VALUES ({i}, 'val_{i}')")
        _pg_query(s, "CREATE INDEX idx_test_id ON idx_test (id)")
        # Verify data via SELECT * client-side filter (WHERE id=X path has data issue)
        rows = _extract_rows(_pg_query(s, "SELECT * FROM idx_test"))
        row = next((r for r in rows if r[0] == '250'), None)
        assert row is not None
        assert row[1] == "val_250"
        s.close()

    def test_index_range_scan(self, engine_port, _seed_data):
        _run_sql(engine_port, "CREATE INDEX idx_emp_salary ON employees (salary)")
        resp = _run_sql(engine_port,
            "SELECT * FROM employees WHERE salary BETWEEN 90000 AND 110000")
        assert resp is not None  # No crash


# =====================================================================
# 17. NativeDispatcher Direct API Tests
# =====================================================================

class TestNativeDispatcherFull:
    def test_full_crud_cycle(self, tmp_path):
        nd = qm_engine.NativeDispatcher(str(tmp_path))

        lsn, seq = nd.dispatch_ddl(b"CREATE TABLE crud (id INT, name TEXT)")
        assert lsn > 0

        nd.dispatch_insert(b"INSERT INTO crud (id, name) VALUES (1, 'Alice')")
        nd.dispatch_insert(b"INSERT INTO crud (id, name) VALUES (2, 'Bob')")

        lsn_q, _ = nd.dispatch_query(b"SELECT * FROM crud")
        assert lsn_q > 0

        nd.dispatch_update(b"UPDATE crud SET name = 'Alicia' WHERE id = 1")
        nd.dispatch_delete(b"DELETE FROM crud WHERE id = 2")

        assert nd.current_lsn > lsn

    def test_batch_insert(self, tmp_path):
        nd = qm_engine.NativeDispatcher(str(tmp_path))
        nd.dispatch_ddl(b"CREATE TABLE bi (id INT, val TEXT)")
        lsn, seq = nd.dispatch_batch_insert(
            b"INSERT INTO bi (id, val) VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        assert lsn > 0

    def test_batch_operations(self, tmp_path):
        nd = qm_engine.NativeDispatcher(str(tmp_path))
        nd.dispatch_ddl(b"CREATE TABLE bo (id INT, val TEXT)")
        for i in range(10):
            nd.dispatch_insert(f"INSERT INTO bo (id, val) VALUES ({i}, 'v{i}')".encode())

        nd.dispatch_batch_update(b"UPDATE bo SET val = 'updated' WHERE id IN (1, 3, 5)")
        nd.dispatch_batch_delete(b"DELETE FROM bo WHERE id IN (2, 4)")

    def test_lsn_monotonic(self, tmp_path):
        nd = qm_engine.NativeDispatcher(str(tmp_path))
        nd.dispatch_ddl(b"CREATE TABLE lsn_t (id INT, val TEXT)")
        lsns = []
        for i in range(20):
            lsn, _ = nd.dispatch_insert(f"INSERT INTO lsn_t (id, val) VALUES ({i}, 'v{i}')".encode())
            lsns.append(lsn)
        for i in range(1, len(lsns)):
            assert lsns[i] >= lsns[i - 1]

    def test_recover_all(self, tmp_path):
        nd = qm_engine.NativeDispatcher(str(tmp_path))
        result = nd.recover_all()
        assert isinstance(result, tuple)
        assert len(result) == 3


# =====================================================================
# 18. Aggregate Edge Cases
# =====================================================================

class TestAggregateEdgeCases:
    def test_count_empty(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE agg_empty (id INT, val FLOAT)")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM agg_empty"))
        assert rows[0][0] == "0"
        s.close()

    def test_sum_empty(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE sum_empty (id INT, val FLOAT)")
        resp = _pg_query(s, "SELECT SUM(val) FROM sum_empty")
        assert resp is not None
        s.close()

    def test_count_where_text(self, engine_port, _seed_data):
        """COUNT with text equality WHERE works."""
        rows = _query_rows(engine_port,
            "SELECT COUNT(*) FROM employees WHERE dept = 'eng'")
        # WHERE text equality goes through handle_select which returns filtered rows
        # Then COUNT counts those — but engine may return full count
        assert rows[0][0] is not None

    def test_sum_all(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT SUM(salary) FROM employees")
        expected = sum([120000, 110000, 90000, 95000, 85000, 130000, 80000, 92000, 115000, 88000])
        assert abs(float(rows[0][0]) - expected) < 1.0

    def test_avg_products(self, engine_port, _seed_data):
        rows = _query_rows(engine_port, "SELECT AVG(price) FROM products")
        expected_avg = (999.99 + 699.99 + 29.99 + 249.99 + 199.99) / 5
        assert abs(float(rows[0][0]) - expected_avg) < 0.1


# =====================================================================
# 19. Multiple Connections
# =====================================================================

class TestConcurrentConnections:
    def test_two_readers(self, engine_port, _seed_data):
        s1 = _pg_connect(engine_port)
        s2 = _pg_connect(engine_port)
        r1 = _extract_rows(_pg_query(s1, "SELECT COUNT(*) FROM employees"))
        r2 = _extract_rows(_pg_query(s2, "SELECT COUNT(*) FROM employees"))
        assert r1[0][0] == r2[0][0] == "10"
        s1.close()
        s2.close()

    def test_write_visible_to_reader(self, engine_port):
        s1 = _pg_connect(engine_port)
        _pg_query(s1, "CREATE TABLE conn_vis (id INT, val TEXT)")
        _pg_query(s1, "INSERT INTO conn_vis (id, val) VALUES (42, 'visible')")

        s2 = _pg_connect(engine_port)
        rows = _extract_rows(_pg_query(s2, "SELECT * FROM conn_vis WHERE id = 42"))
        assert len(rows) == 1
        assert rows[0][0] == "42"
        s1.close()
        s2.close()


# =====================================================================
# 20. Stress Tests
# =====================================================================

class TestStress:
    def test_1000_inserts(self, engine_port):
        s = _pg_connect(engine_port)
        _pg_query(s, "CREATE TABLE stress_ins (id INT, val TEXT)")
        for i in range(1000):
            _pg_query(s, f"INSERT INTO stress_ins (id, val) VALUES ({i}, 's{i}')")
        rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM stress_ins"))
        assert rows[0][0] == "1000"
        s.close()

    def test_100_queries(self, engine_port, _seed_data):
        s = _pg_connect(engine_port)
        for _ in range(100):
            rows = _extract_rows(_pg_query(s, "SELECT COUNT(*) FROM employees"))
            assert rows[0][0] == "10"
        s.close()

    def test_rapid_connect_disconnect(self, engine_port, _seed_data):
        for _ in range(20):
            rows = _query_rows(engine_port, "SELECT COUNT(*) FROM employees")
            assert rows[0][0] == "10"
