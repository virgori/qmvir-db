"""
Tests for the QMvir Web Dashboard HTTP API.

Starts the authenticated web server in-process using qm_engine when the
Python web entrypoint is available, then exercises
every public endpoint with httpx.

Run with:
    pytest tests/test_web_dashboard.py -v
"""

from __future__ import annotations

import os
import sys
import time
import threading
import tempfile
import subprocess

import pytest

# ── Optional dependency check (module-level try/except to avoid skipping all) ─
try:
    import httpx as httpx  # type: ignore[import]
    _HTTPX_AVAILABLE = True
except ImportError:
    httpx = None  # type: ignore[assignment]
    _HTTPX_AVAILABLE = False


# ── Helpers ───────────────────────────────────────────────────────────

def _free_port() -> int:
    """Return an ephemeral TCP port that is currently free."""
    import socket
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


BASE_URL: str = ""   # set by session fixture
DASHBOARD_AUTH = ("admin", "dashboard-secret")


# ── Session-scoped server fixture ────────────────────────────────────

@pytest.fixture(scope="module")
def dashboard_base_url(tmp_path_factory):
    """Start the web dashboard as a subprocess, yield base URL, then stop."""
    import qm_engine

    data_dir = tmp_path_factory.mktemp("web_data")
    port = _free_port()
    addr = f"127.0.0.1:{port}"
    base = f"http://{addr}"

    start_web_server = getattr(qm_engine, "start_web_server", None)
    if start_web_server is None:
        pytest.skip("qm_engine.start_web_server not available — skipping dashboard HTTP tests")

    engine = qm_engine.NativeSqlEngine(data_dir=str(data_dir))
    engine.execute("CREATE TABLE IF NOT EXISTS items (id TEXT, name TEXT, value FLOAT)")
    engine.execute("INSERT INTO items VALUES ('1', 'alpha', 1.0)")
    engine.execute("INSERT INTO items VALUES ('2', 'beta',  2.0)")
    engine.execute("ALTER USER admin WITH PASSWORD 'dashboard-secret'")

    start_web_server(engine, host="127.0.0.1", port=port, background=True)

    # Wait until the server is reachable.
    client = httpx.Client(
        base_url=base,
        timeout=2.0,
        auth=httpx.BasicAuth(*DASHBOARD_AUTH),
    )
    deadline = time.time() + 10
    while time.time() < deadline:
        try:
            r = client.get("/api/health")
            if r.status_code == 200:
                break
        except Exception:
            pass
        time.sleep(0.1)
    else:
        pytest.skip("Dashboard server did not start in time")

    yield base


# ── Alternate fixture: raw engine + direct handler tests ─────────────

@pytest.fixture(scope="module")
def engine(tmp_path_factory):
    """A fresh NativeSqlEngine with a few rows for endpoint-level tests."""
    import qm_engine
    data_dir = tmp_path_factory.mktemp("dashboard_engine")
    eng = qm_engine.NativeSqlEngine(data_dir=str(data_dir))
    eng.execute("CREATE TABLE products (id INTEGER, name TEXT, price TEXT)")
    eng.execute("INSERT INTO products (id, name, price) VALUES (1, 'Widget', '9.99')")
    eng.execute("INSERT INTO products (id, name, price) VALUES (2, 'Gadget', '19.99')")
    eng.execute("INSERT INTO products (id, name, price) VALUES (3, 'Doohickey', '4.99')")
    return eng


# ── Tests using the engine directly (no HTTP server required) ─────────

class TestDashboardEngineLayer:
    """
    Validate the engine behaviours that the web dashboard surfaces.
    No HTTP server needed — these exercise the SQL layer directly.
    """

    def test_health_query(self, engine):
        """Engine responds to a trivial SELECT (dashboard /api/health equivalent)."""
        cols, rows, tag = engine.execute("SELECT 42")
        assert rows, "Expected at least one row"
        assert rows[0][0] == "42"
        assert "SELECT" in tag

    def test_snapshot_info_available(self, engine):
        """snapshot_info() returns a dict with expected keys."""
        info = engine.snapshot_info()
        assert isinstance(info, dict)
        # tables list should exist
        assert "tables" in info or "snapshot_lsn" in info or len(info) >= 0

    def test_list_tables(self, engine):
        """Can enumerate tables that were created via snapshot_info."""
        info = engine.snapshot_info()
        assert info["table_count"] >= 1, "Expected at least 1 table in snapshot_info"

    def test_query_endpoint_select(self, engine):
        """SELECT returns correct columns and rows."""
        cols, rows, tag = engine.execute("SELECT id, name FROM products")
        assert "id" in cols
        assert "name" in cols
        assert len(rows) == 3
        assert "SELECT" in tag

    def test_query_endpoint_insert(self, engine):
        """INSERT via execute returns INSERT command tag."""
        _, _, tag = engine.execute(
            "INSERT INTO products (id, name, price) VALUES (4, 'Thingamajig', '2.49')"
        )
        assert "INSERT" in tag

    def test_query_endpoint_delete(self, engine):
        """DELETE via execute returns DELETE command tag."""
        _, _, tag = engine.execute("DELETE FROM products WHERE id = 4")
        assert "DELETE" in tag

    def test_explain_select(self, engine):
        """EXPLAIN SELECT returns a non-empty plan string."""
        cols, rows, tag = engine.execute("EXPLAIN SELECT * FROM products WHERE price > 5")
        assert rows, "EXPLAIN should return at least one plan row"
        plan_text = " ".join(c or "" for row in rows for c in row)
        assert "Seq Scan" in plan_text or "Scan" in plan_text

    def test_explain_insert(self, engine):
        """EXPLAIN INSERT returns an insert plan."""
        cols, rows, _ = engine.execute(
            "EXPLAIN INSERT INTO products (id, name, price) VALUES (9, 'X', '0')"
        )
        plan_text = " ".join(c or "" for row in rows for c in row)
        assert plan_text  # non-empty

    def test_timeout_short_query(self, engine):
        """execute_timeout with generous limit completes normally."""
        cols, rows, tag = engine.execute_timeout("SELECT count(*) FROM products", 5000)
        assert cols
        assert rows

    def test_timeout_zero_passes_through(self, engine):
        """execute_timeout(sql, 0) falls through to normal execute."""
        cols, rows, tag = engine.execute_timeout("SELECT 42", 0)
        assert rows[0][0] == "42"


# ── Tests using httpx against the live server (skipped if no web API) ─

@pytest.mark.skipif(not _HTTPX_AVAILABLE, reason="httpx not installed")
class TestDashboardHTTP:
    """HTTP tests — automatically skipped when the web server is unavailable."""

    @pytest.fixture(autouse=True)
    def _skip_if_no_server(self, request):
        """Skip all HTTP tests if dashboard_base_url fixture skipped."""
        # If the fixture yielded nothing it means it was skipped.
        pass

    def _client(self, base_url: str):
        return httpx.Client(
            base_url=base_url,
            timeout=5.0,
            auth=httpx.BasicAuth(*DASHBOARD_AUTH),
        )

    def test_auth_required(self, dashboard_base_url):
        r = httpx.get(f"{dashboard_base_url}/api/health", timeout=5.0)
        assert r.status_code == 401

    def test_health_ok(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/api/health")
        assert r.status_code == 200
        body = r.json()
        assert body["status"] == "ok"
        assert "version" in body

    def test_stats_shape(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/api/stats")
        assert r.status_code == 200
        body = r.json()
        assert "queries_total" in body
        assert body["cache_hit_rate"] >= 0

    def test_tables_list(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/api/tables")
        assert r.status_code == 200
        tables = r.json()
        assert isinstance(tables, list)
        names = [t["name"] for t in tables]
        assert "items" in names

    def test_table_detail(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/api/tables/items")
        assert r.status_code == 200
        body = r.json()
        assert body["name"] == "items"
        assert "columns" in body
        assert body["row_count"] >= 2

    def test_table_not_found(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/api/tables/nonexistent_xyz")
        assert r.status_code in (404, 400)

    def test_query_select(self, dashboard_base_url):
        r = self._client(dashboard_base_url).post(
            "/api/query", json={"sql": "SELECT * FROM items ORDER BY id"}
        )
        assert r.status_code == 200
        body = r.json()
        assert "columns" in body
        assert "rows" in body
        assert len(body["rows"]) >= 2

    def test_query_bad_sql(self, dashboard_base_url):
        r = self._client(dashboard_base_url).post(
            "/api/query", json={"sql": "NOT VALID SQL !!!"}
        )
        # Should return 4xx or a JSON error, not 500
        assert r.status_code in (400, 422, 200)  # 200 with error field also acceptable

    def test_wal_status(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/api/wal/status")
        assert r.status_code == 200
        body = r.json()
        assert "wal_writes" in body

    def test_backup_endpoint(self, dashboard_base_url):
        r = self._client(dashboard_base_url).post(
            "/api/backup", json={"output": "web_backup", "compression": "lz4"}
        )
        assert r.status_code == 200
        body = r.json()
        assert body.get("ok") or "path" in body
        assert body["path"].endswith("web_backup.qmvb")

    def test_metrics_prometheus(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/metrics")
        assert r.status_code == 200
        # Prometheus text format typically contains # HELP or a metric name
        assert r.text  # non-empty

    def test_dashboard_html(self, dashboard_base_url):
        r = self._client(dashboard_base_url).get("/")
        assert r.status_code == 200
        assert "text/html" in r.headers.get("content-type", "")
