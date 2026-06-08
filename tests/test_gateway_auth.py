"""Tests for gateway: QueryPlanner, ACL/Auth, PostgresGateway (Rust).

Replaces old test_phases789.py auth/schema sections + test_final_sprint.py RBAC.
"""
from __future__ import annotations

import pytest

import qm_engine
from gateway.query_router.planner import QueryPlanner
from gateway.auth.acl import (
    APIKeyValidator,
    AuthContext,
    Permission,
    TenantIsolation,
    authorize_request,
)


# ─────────────────────────────────────────────────────────────────────
# QueryPlanner
# ─────────────────────────────────────────────────────────────────────

class TestQueryPlanner:
    @pytest.fixture
    def planner(self):
        return QueryPlanner()

    def test_find_routes_to_core(self, planner):
        plan = planner.plan({"action": "find", "entity": "users", "where": {"id": "u1"}})
        assert plan is not None
        assert plan.primary_engine is not None

    def test_search_routes_to_lexical(self, planner):
        plan = planner.plan({"action": "search", "entity": "articles", "where": {"query": "database"}})
        assert plan is not None

    def test_insert_routes_to_core(self, planner):
        plan = planner.plan({"action": "insert", "entity": "users"})
        assert plan is not None

    def test_aggregate_plan(self, planner):
        plan = planner.plan({"action": "aggregate", "entity": "orders"})
        assert plan is not None

    def test_cache_hint(self, planner):
        plan = planner.plan({"action": "find", "entity": "users", "where": {"id": "u1"}})
        assert hasattr(plan, "use_cache")

    def test_plan_with_limit(self, planner):
        plan = planner.plan({"action": "find", "entity": "users", "limit": 10})
        assert plan is not None

    def test_plan_with_offset(self, planner):
        plan = planner.plan({"action": "find", "entity": "users", "offset": 20, "limit": 10})
        assert plan is not None


# ─────────────────────────────────────────────────────────────────────
# ACL / Auth
# ─────────────────────────────────────────────────────────────────────

class TestAuthContext:
    def test_basic_context(self):
        ctx = AuthContext(user_id="user1", tenant_id="t1")
        assert ctx.user_id == "user1"
        assert ctx.tenant_id == "t1"

    def test_admin_detection(self):
        ctx = AuthContext(user_id="admin", tenant_id="t1", roles=["admin"])
        assert ctx.is_admin is True

    def test_non_admin(self):
        ctx = AuthContext(user_id="user1", tenant_id="t1", roles=["reader"])
        assert ctx.is_admin is False

    def test_service_account(self):
        ctx = AuthContext(user_id="svc", tenant_id="t1", is_service_account=True)
        assert ctx.is_service_account is True


class TestAuthorizeRequest:
    def test_admin_can_do_anything(self):
        ctx = AuthContext(user_id="admin", tenant_id="t1", roles=["admin"])
        assert authorize_request(ctx, "insert", "users") is True

    def test_reader_can_read(self):
        ctx = AuthContext(user_id="user1", tenant_id="t1", roles=["reader"],
                          permissions={Permission.READ})
        assert authorize_request(ctx, "find", "users") is True

    def test_reader_cannot_write(self):
        ctx = AuthContext(user_id="user1", tenant_id="t1", roles=["reader"],
                          permissions={Permission.READ})
        assert authorize_request(ctx, "insert", "users") is False


class TestTenantIsolation:
    def test_apply_filter_none(self):
        iso = TenantIsolation(tenant_id="t1")
        result = iso.apply_filter(None)
        assert "tenant_id" in result
        assert result["tenant_id"] == "t1"

    def test_apply_filter_existing(self):
        iso = TenantIsolation(tenant_id="t1")
        result = iso.apply_filter({"status": "active"})
        assert result["tenant_id"] == "t1"
        assert result["status"] == "active"


class TestAPIKeyValidator:
    def test_register_and_validate(self):
        validator = APIKeyValidator()
        ctx = AuthContext(user_id="svc1", tenant_id="t1", is_service_account=True)
        validator.register_key("api_key_123", ctx)
        result = validator.validate("api_key_123")
        assert result is not None
        assert result.user_id == "svc1"

    def test_validate_invalid_key(self):
        validator = APIKeyValidator()
        assert validator.validate("invalid_key") is None


# ─────────────────────────────────────────────────────────────────────
# PostgresGateway (Rust) — extended tests
# ─────────────────────────────────────────────────────────────────────

class TestPostgresGatewayExtended:
    def test_create_with_unix_socket(self):
        gw = qm_engine.PostgresGateway(port=15490, unix_socket_path="/tmp/qm_test.sock")
        config = gw.get_config()
        assert "/tmp/qm_test.sock" in config
        gw.stop()

    def test_max_connections(self):
        gw = qm_engine.PostgresGateway(port=15489, max_connections=50)
        config = gw.get_config()
        assert "50" in config
        gw.stop()

    def test_custom_host(self):
        gw = qm_engine.PostgresGateway(host="0.0.0.0", port=15488)
        config = gw.get_config()
        assert "0.0.0.0" in config
        gw.stop()


# ─────────────────────────────────────────────────────────────────────
# SqlParser (Rust) — extended tests
# ─────────────────────────────────────────────────────────────────────

class TestSqlParserExtended:
    @pytest.fixture
    def parser(self):
        return qm_engine.SqlParser()

    def test_parse_join(self, parser):
        result = parser.parse("SELECT a.id, b.name FROM a JOIN b ON a.id = b.a_id")
        assert result["query_type"] == "Select"

    def test_parse_group_by(self, parser):
        result = parser.parse("SELECT name, COUNT(*) FROM users GROUP BY name")
        assert result["query_type"] == "Select"

    def test_parse_having(self, parser):
        result = parser.parse("SELECT name, COUNT(*) as c FROM users GROUP BY name HAVING c > 1")
        assert result["query_type"] == "Select"

    def test_parse_order_by(self, parser):
        result = parser.parse("SELECT * FROM users ORDER BY name ASC, age DESC")
        assert result["query_type"] == "Select"

    def test_parse_subquery(self, parser):
        result = parser.parse("SELECT * FROM users WHERE id IN (SELECT user_id FROM orders)")
        assert result["query_type"] == "Select"

    def test_parse_cte(self, parser):
        result = parser.parse("WITH active AS (SELECT * FROM users WHERE status = 'active') SELECT * FROM active")
        assert result["query_type"] in ("Select", "Other")

    def test_parse_alter_table(self, parser):
        result = parser.parse("ALTER TABLE users ADD COLUMN email TEXT")
        assert "query_type" in result

    def test_parse_drop_table(self, parser):
        result = parser.parse("DROP TABLE users")
        assert result["query_type"] == "DropTable"

    def test_parse_create_index(self, parser):
        result = parser.parse("CREATE INDEX idx_name ON users (name)")
        assert result["query_type"] == "CreateIndex"

    def test_parse_begin(self, parser):
        result = parser.parse("BEGIN")
        assert result["query_type"] in ("Begin", "Transaction")

    def test_parse_commit(self, parser):
        result = parser.parse("COMMIT")
        assert result["query_type"] in ("Commit", "Transaction")

    def test_parse_window_function(self, parser):
        result = parser.parse("SELECT name, ROW_NUMBER() OVER (ORDER BY id) FROM users")
        assert result["query_type"] == "Select"

    def test_parse_like(self, parser):
        result = parser.parse("SELECT * FROM users WHERE name LIKE 'a%'")
        assert result["query_type"] == "Select"

    def test_parse_between(self, parser):
        result = parser.parse("SELECT * FROM users WHERE age BETWEEN 20 AND 30")
        assert result["query_type"] == "Select"

    def test_parse_is_null(self, parser):
        result = parser.parse("SELECT * FROM users WHERE email IS NULL")
        assert result["query_type"] == "Select"

    def test_parse_distinct(self, parser):
        result = parser.parse("SELECT DISTINCT name FROM users")
        assert result["query_type"] == "Select"

    def test_parse_create_user(self, parser):
        result = parser.parse("CREATE USER testuser WITH PASSWORD 'pass123'")
        assert "query_type" in result

    def test_parse_grant(self, parser):
        result = parser.parse("GRANT SELECT ON users TO testuser")
        assert "query_type" in result

    def test_parse_show(self, parser):
        result = parser.parse("SHOW TABLES")
        assert "query_type" in result

    def test_parse_explain(self, parser):
        result = parser.parse("EXPLAIN SELECT * FROM users")
        assert "query_type" in result

    def test_parse_vacuum(self, parser):
        result = parser.parse("VACUUM")
        assert "query_type" in result

    def test_parse_analyze(self, parser):
        result = parser.parse("ANALYZE users")
        assert "query_type" in result

    def test_parse_multi_value_insert(self, parser):
        result = parser.parse("INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')")
        assert result["query_type"] == "Insert"

    def test_parse_update_multiple_set(self, parser):
        result = parser.parse("UPDATE users SET name = 'alice', age = 30 WHERE id = 1")
        assert result["query_type"] == "Update"

    def test_parse_left_join(self, parser):
        result = parser.parse("SELECT * FROM a LEFT JOIN b ON a.id = b.a_id")
        assert result["query_type"] == "Select"

    def test_parse_right_join(self, parser):
        result = parser.parse("SELECT * FROM a RIGHT JOIN b ON a.id = b.a_id")
        assert result["query_type"] == "Select"
