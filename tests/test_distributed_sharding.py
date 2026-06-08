"""Tests for distributed/sharding features via Rust qm_engine API.

Tests ShardRing, ShardManager — the Rust shard routing layer.
Also tests NativeDispatcher batch operations and HubEngine SQL execution.
Replaces old test_distributed.py.
"""
from __future__ import annotations

import json
import tempfile

import pytest

import qm_engine


# ─────────────────────────────────────────────────────────────────────
# ShardRing — consistent hash ring
# ─────────────────────────────────────────────────────────────────────

class TestShardRing:
    @pytest.fixture
    def ring(self):
        return qm_engine.ShardRing(num_shards=4, vnodes_per_shard=64)

    def test_create(self, ring):
        assert ring.num_shards() == 4

    def test_shard_for_id_deterministic(self, ring):
        shard1 = ring.shard_for_id(123)
        shard2 = ring.shard_for_id(123)
        assert shard1 == shard2

    def test_different_ids_may_differ(self, ring):
        shards = {ring.shard_for_id(i) for i in range(100)}
        # With 4 shards, we should hit multiple
        assert len(shards) > 1

    def test_get_shard(self, ring):
        shard = ring.get_shard(0)
        assert shard is not None

    def test_get_replicas(self, ring):
        replicas = ring.get_replicas(1, 2)
        assert isinstance(replicas, list)
        assert len(replicas) >= 1

    def test_add_shard(self, ring):
        old_count = ring.num_shards()
        ring.add_shard(old_count)
        assert ring.num_shards() == old_count + 1

    def test_remove_shard(self, ring):
        new_id = ring.num_shards()
        ring.add_shard(new_id)
        old_count = ring.num_shards()
        ring.remove_shard(new_id)
        assert ring.num_shards() == old_count - 1

    def test_distribution_stats(self, ring):
        keys = list(range(1000))
        stats = ring.distribution_stats(keys)
        assert isinstance(stats, tuple)

    def test_distribution_fairness(self, ring):
        """Keys should be roughly evenly distributed across shards."""
        counts = {}
        for i in range(1000):
            shard = ring.shard_for_id(i)
            counts[shard] = counts.get(shard, 0) + 1
        # Each shard should get at least 10% of keys (250 ideal for 4 shards)
        for count in counts.values():
            assert count >= 50, f"Unfair distribution: {counts}"

    def test_single_shard(self):
        ring = qm_engine.ShardRing(num_shards=1, vnodes_per_shard=32)
        assert ring.num_shards() == 1
        assert ring.shard_for_id(0) == 0


# ─────────────────────────────────────────────────────────────────────
# ShardManager — routing with replication
# ─────────────────────────────────────────────────────────────────────

class TestShardManager:
    @pytest.fixture
    def mgr(self):
        return qm_engine.ShardManager(num_shards=4, vnodes_per_shard=64, replication_factor=2)

    def test_create(self, mgr):
        assert mgr.num_shards() == 4

    def test_route_returns_shard(self, mgr):
        result = mgr.route(42)
        assert isinstance(result, list)
        assert len(result) > 0
        for shard in result:
            assert 0 <= shard < 4

    def test_route_batch(self, mgr):
        keys = list(range(10))
        results = mgr.route_batch(keys)
        assert isinstance(results, dict)
        # All keys should be routed somewhere
        total_keys = sum(len(v) for v in results.values())
        assert total_keys == 10

    def test_record_insert(self, mgr):
        mgr.record_insert(0)
        stats = mgr.balance_stats()
        assert isinstance(stats, tuple)

    def test_balance_stats(self, mgr):
        for i in range(100):
            shards = mgr.route(i)
            for s in shards:
                mgr.record_insert(s)
        stats = mgr.balance_stats()
        assert isinstance(stats, tuple)

    def test_add_and_remove_shard(self, mgr):
        new_id = mgr.num_shards()
        mgr.add_shard(new_id)
        assert mgr.num_shards() == 5
        mgr.remove_shard(new_id)
        assert mgr.num_shards() == 4

    def test_route_deterministic(self, mgr):
        r1 = mgr.route(999)
        r2 = mgr.route(999)
        assert r1 == r2


# ─────────────────────────────────────────────────────────────────────
# NativeDispatcher — batch operations
# ─────────────────────────────────────────────────────────────────────

class TestNativeDispatcherBatch:
    @pytest.fixture
    def nd(self, tmp_path):
        nd = qm_engine.NativeDispatcher(str(tmp_path))
        nd.dispatch_ddl(b"CREATE TABLE items (id INT, name TEXT, qty INT)")
        return nd

    def test_dispatch_batch_insert(self, nd):
        result = nd.dispatch_batch_insert(
            b"INSERT INTO items VALUES (1, 'apple', 10)"
        )
        assert isinstance(result, tuple)

    def test_dispatch_batch_update(self, nd):
        nd.dispatch_insert(b"INSERT INTO items VALUES (1, 'apple', 10)")
        result = nd.dispatch_batch_update(
            b"UPDATE items SET qty = 15 WHERE id = 1"
        )
        assert isinstance(result, tuple)

    def test_dispatch_batch_delete(self, nd):
        nd.dispatch_insert(b"INSERT INTO items VALUES (1, 'apple', 10)")
        result = nd.dispatch_batch_delete(
            b"DELETE FROM items WHERE id = 1"
        )
        assert isinstance(result, tuple)

    def test_dispatch_vector_op(self, nd):
        result = nd.dispatch_vector_op(b"VECTOR_SEARCH items LIMIT 5")
        assert isinstance(result, tuple)

    def test_lsn_monotonic_after_batch(self, nd):
        lsn0 = nd.current_lsn
        nd.dispatch_batch_insert(
            b"INSERT INTO items VALUES (10, 'x', 1)"
        )
        assert nd.current_lsn > lsn0

    def test_recover_all(self, nd):
        result = nd.recover_all()
        assert isinstance(result, tuple)

    def test_drain_all(self, nd):
        result = nd.drain_all()
        assert isinstance(result, tuple)


# ─────────────────────────────────────────────────────────────────────
# HubEngine — SQL execution
# ─────────────────────────────────────────────────────────────────────

class TestHubEngineSQL:
    @pytest.fixture
    def hub(self, tmp_path):
        h = qm_engine.HubEngine(str(tmp_path))
        h.start()
        return h

    def test_execute_sql_create_table(self, hub):
        result = hub.execute_sql("CREATE TABLE t (id INT, name TEXT)")
        assert result is not None

    def test_execute_sql_insert(self, hub):
        hub.execute_sql("CREATE TABLE t (id INT, name TEXT)")
        result = hub.execute_sql("INSERT INTO t VALUES (1, 'alice')")
        assert result is not None

    def test_execute_sql_select(self, hub):
        hub.execute_sql("CREATE TABLE t (id INT, name TEXT)")
        hub.execute_sql("INSERT INTO t VALUES (1, 'alice')")
        result = hub.execute_sql("SELECT * FROM t")
        assert result is not None
        payload = json.loads(result)
        assert payload["columns"] == ["id", "name"]
        assert payload["rows"] == [["1", "alice"]]

    def test_hash_join_multiple_rows(self, hub):
        build = json.dumps([
            {"id": 1, "name": "alice"},
            {"id": 2, "name": "bob"},
            {"id": 3, "name": "carol"},
        ])
        probe = json.dumps([
            {"id": 1, "score": 90},
            {"id": 2, "score": 85},
            {"id": 4, "score": 70},
        ])
        result = json.loads(hub.execute_hash_join_bytes(
            build.encode(), probe.encode(), "id", "id"
        ))
        assert result["affected_rows"] == 2  # id=1 and id=2 match

    def test_hash_join_empty_build(self, hub):
        build = json.dumps([])
        probe = json.dumps([{"id": 1}])
        result = json.loads(hub.execute_hash_join_bytes(
            build.encode(), probe.encode(), "id", "id"
        ))
        assert result["affected_rows"] == 0
        assert result["rows"] == []

    def test_hash_join_empty_probe(self, hub):
        build = json.dumps([{"id": 1}])
        probe = json.dumps([])
        result = json.loads(hub.execute_hash_join_bytes(
            build.encode(), probe.encode(), "id", "id"
        ))
        assert result["affected_rows"] == 0


# ─────────────────────────────────────────────────────────────────────
# ReplicaManager (Python)
# ─────────────────────────────────────────────────────────────────────

class TestReplicaManager:
    @pytest.fixture
    def mgr(self):
        from core_db.replica_manager.replication import ReplicaManager
        return ReplicaManager()

    def test_add_and_list_nodes(self, mgr):
        from core_db.replica_manager.replication import ReplicaNode, ReplicaRole
        node = ReplicaNode(node_id="n1", role=ReplicaRole.PRIMARY, host="127.0.0.1", port=5432)
        mgr.add_node(node)
        nodes = mgr.list_nodes()
        assert len(nodes) == 1
        assert nodes[0].node_id == "n1"

    def test_get_primary(self, mgr):
        from core_db.replica_manager.replication import ReplicaNode, ReplicaRole
        mgr.add_node(ReplicaNode("n1", ReplicaRole.PRIMARY, "127.0.0.1", 5432))
        mgr.add_node(ReplicaNode("n2", ReplicaRole.REPLICA, "127.0.0.2", 5433))
        primary = mgr.get_primary()
        assert primary is not None
        assert primary.node_id == "n1"

    def test_get_read_replica(self, mgr):
        from core_db.replica_manager.replication import ReplicaNode, ReplicaRole
        mgr.add_node(ReplicaNode("n1", ReplicaRole.PRIMARY, "127.0.0.1", 5432))
        mgr.add_node(ReplicaNode("n2", ReplicaRole.REPLICA, "127.0.0.2", 5433))
        replica = mgr.get_read_replica()
        assert replica is not None
        assert replica.role == ReplicaRole.REPLICA

    def test_update_lsn(self, mgr):
        from core_db.replica_manager.replication import ReplicaNode, ReplicaRole
        mgr.add_node(ReplicaNode("n1", ReplicaRole.PRIMARY, "127.0.0.1", 5432))
        mgr.update_lsn("n1", 42)
        nodes = mgr.list_nodes()
        assert nodes[0].last_applied_lsn == 42

    def test_node_is_healthy(self):
        from core_db.replica_manager.replication import ReplicaNode, ReplicaRole, ReplicaStatus
        node = ReplicaNode("n1", ReplicaRole.PRIMARY, "127.0.0.1", 5432, status=ReplicaStatus.ACTIVE)
        assert node.is_healthy is True
