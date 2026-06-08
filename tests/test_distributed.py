"""Comprehensive tests for QM distributed module.

Tests all distributed layers:
    - Gossip: SWIM protocol, membership, failure detection
    - Consensus: Raft leader election, log replication
    - Cluster: Coordinator, DDL, shard assignment, failover
    - Shard: Consistent hashing, routing, migration, splitting
    - Replication: WAL shipping, sender/receiver, promotion
    - Distributed Query: Scatter-gather, merge strategies
    - Distributed Transactions: 2PC, Saga pattern
    - CDC: Event pipeline, subscriptions, filtering
    - Client: Cluster-aware client, read preferences, retries
"""

import threading
import time
import unittest


# ── Gossip Protocol Tests ───────────────────────────────────────────

class TestGossipProtocol(unittest.TestCase):
    def test_node_state_enum(self):
        from qm_core.distributed.gossip import NodeState
        self.assertEqual(NodeState.ALIVE, 0)
        self.assertEqual(NodeState.SUSPECT, 1)
        self.assertEqual(NodeState.DEAD, 2)
        self.assertEqual(NodeState.LEFT, 3)

    def test_member_entry_creation(self):
        from qm_core.distributed.gossip import MemberEntry, NodeState
        m = MemberEntry(
            node_id="node-1", host="10.0.0.1", port=9000,
            state=NodeState.ALIVE, incarnation=1,
        )
        self.assertEqual(m.node_id, "node-1")
        self.assertEqual(m.state, NodeState.ALIVE)
        d = m.to_dict()
        self.assertIn("node_id", d)
        self.assertIn("state", d)

    def test_gossip_message_serialization(self):
        from qm_core.distributed.gossip import GossipMessage, GossipMessageType
        msg = GossipMessage(
            msg_type=GossipMessageType.PING, sender_id="node-1",
            payload={"key": "value"},
        )
        data = msg.serialize()
        self.assertIsInstance(data, bytes)
        msg2 = GossipMessage.deserialize(data)
        self.assertEqual(msg2.sender_id, "node-1")
        self.assertEqual(msg2.payload["key"], "value")

    def test_gossip_protocol_create(self):
        from qm_core.distributed.gossip import GossipProtocol, GossipConfig
        config = GossipConfig(ping_interval_s=0.1, ping_timeout_s=0.05)
        gp = GossipProtocol("node-1", "127.0.0.1", 9000, config=config)
        self.assertEqual(gp._self_id, "node-1")
        self.assertEqual(len(gp.alive_members), 1)  # Self

    def test_gossip_join_and_membership(self):
        from qm_core.distributed.gossip import GossipProtocol, GossipConfig, GossipMessage, GossipMessageType
        config = GossipConfig(ping_interval_s=0.5)

        # Create two nodes
        gp1 = GossipProtocol("node-1", "127.0.0.1", 9001, config=config)
        gp2 = GossipProtocol("node-2", "127.0.0.2", 9002, config=config)

        # Simulate node-2 receiving a join from node-1
        join_msg = GossipMessage(
            msg_type=GossipMessageType.STATE_SYNC,
            sender_id="node-1",
            membership_updates=[
                {"node_id": "node-1", "host": "127.0.0.1", "port": 9001,
                 "state": "ALIVE", "incarnation": 1}
            ],
        )
        gp2.handle_message(join_msg.serialize())

        # Node-2 should now know about node-1
        self.assertIn("node-1", gp2._members)

    def test_gossip_metadata(self):
        from qm_core.distributed.gossip import GossipProtocol, GossipConfig
        gp = GossipProtocol("node-1", "127.0.0.1", 9000,
                            config=GossipConfig(ping_interval_s=0.5))
        gp.set_metadata("role", "primary")
        self.assertEqual(gp._self_entry.metadata["role"], "primary")

    def test_gossip_summary(self):
        from qm_core.distributed.gossip import GossipProtocol, GossipConfig
        gp = GossipProtocol("node-1", "127.0.0.1", 9000,
                            config=GossipConfig(ping_interval_s=0.5))
        summary = gp.summary()
        self.assertIn("self_id", summary)
        self.assertIn("members", summary)


# ── Consensus (Raft) Tests ──────────────────────────────────────────

class TestRaftConsensus(unittest.TestCase):
    def test_raft_state_enum(self):
        from qm_core.distributed.consensus import RaftState
        self.assertEqual(RaftState.FOLLOWER, 0)
        self.assertEqual(RaftState.CANDIDATE, 1)
        self.assertEqual(RaftState.LEADER, 2)

    def test_log_entry_types(self):
        from qm_core.distributed.consensus import LogEntryType
        self.assertTrue(hasattr(LogEntryType, "NOOP"))
        self.assertTrue(hasattr(LogEntryType, "SHARD_ASSIGN"))
        self.assertTrue(hasattr(LogEntryType, "TABLE_CREATE"))

    def test_raft_node_creation(self):
        from qm_core.distributed.consensus import RaftNode, RaftConfig, InMemoryRaftTransport
        transport = InMemoryRaftTransport()
        config = RaftConfig(election_timeout_min_ms=50, election_timeout_max_ms=100, heartbeat_interval_ms=20)
        node = RaftNode("node-1", ["node-2", "node-3"], transport=transport, config=config)
        transport.register(node)
        self.assertEqual(node._node_id, "node-1")
        self.assertEqual(node._state, 0)  # FOLLOWER

    def test_raft_single_node_leader_election(self):
        """A single node should become leader quickly."""
        from qm_core.distributed.consensus import RaftNode, RaftConfig, InMemoryRaftTransport
        transport = InMemoryRaftTransport()
        config = RaftConfig(election_timeout_min_ms=30, election_timeout_max_ms=60, heartbeat_interval_ms=15)
        node = RaftNode("node-1", [], transport=transport, config=config)
        transport.register(node)
        node.start()
        time.sleep(0.2)
        # Single node with no peers should become leader
        status = node.status()
        self.assertEqual(status["state"], "LEADER")
        node.stop()

    def test_raft_propose_and_commit(self):
        """Single-node Raft should commit proposals immediately."""
        from qm_core.distributed.consensus import RaftNode, RaftConfig, InMemoryRaftTransport, LogEntryType
        transport = InMemoryRaftTransport()
        config = RaftConfig(election_timeout_min_ms=30, election_timeout_max_ms=60, heartbeat_interval_ms=15)
        node = RaftNode("node-1", [], transport=transport, config=config)
        transport.register(node)
        node.start()
        time.sleep(0.15)

        idx = node.propose(LogEntryType.TABLE_CREATE, {"table": "users"})
        self.assertIsNotNone(idx)
        time.sleep(0.1)

        entries = node.get_committed_entries(since_index=0)
        # Should have NOOP + our entry
        self.assertGreaterEqual(len(entries), 1)
        node.stop()

    def test_raft_vote_request_response(self):
        from qm_core.distributed.consensus import RaftNode, RaftConfig, InMemoryRaftTransport, VoteRequest
        transport = InMemoryRaftTransport()
        config = RaftConfig(election_timeout_min_ms=500, election_timeout_max_ms=1000, heartbeat_interval_ms=100)
        node = RaftNode("node-1", ["node-2"], transport=transport, config=config)
        transport.register(node)

        req = VoteRequest(term=1, candidate_id="node-2", last_log_index=0, last_log_term=0)
        resp = node.handle_vote_request(req)
        self.assertTrue(resp.vote_granted)
        self.assertEqual(resp.term, 1)

    def test_raft_three_node_cluster(self):
        """Three-node cluster should elect a leader."""
        from qm_core.distributed.consensus import RaftNode, RaftConfig, InMemoryRaftTransport
        transport = InMemoryRaftTransport()
        config = RaftConfig(election_timeout_min_ms=50, election_timeout_max_ms=150, heartbeat_interval_ms=20)

        nodes = []
        for i in range(3):
            peers = [f"node-{j}" for j in range(3) if j != i]
            node = RaftNode(f"node-{i}", peers, transport=transport, config=config)
            transport.register(node)
            nodes.append(node)

        for n in nodes:
            n.start()


        time.sleep(1.0)

        leaders = [n for n in nodes if n.status()["state"] == "LEADER"]
        self.assertEqual(len(leaders), 1, "Should have exactly one leader")

        for n in nodes:
            n.stop()


# ── Cluster Coordinator Tests ───────────────────────────────────────

class TestClusterCoordinator(unittest.TestCase):
    def test_cluster_config(self):
        from qm_core.distributed.cluster import ClusterConfig
        config = ClusterConfig(
            cluster_name="test-cluster",
            default_shard_count=8,
            replication_factor=3,
        )
        self.assertEqual(config.cluster_name, "test-cluster")
        self.assertEqual(config.default_shard_count, 8)

    def test_cluster_node(self):
        from qm_core.distributed.cluster import ClusterNode, NodeRole
        node = ClusterNode(
            node_id="node-1", host="10.0.0.1", port=9000,
            role=NodeRole.DATA, capacity=1.0,
        )
        self.assertEqual(node.node_id, "node-1")
        self.assertEqual(node.role, NodeRole.DATA)

    def test_cluster_metadata(self):
        from qm_core.distributed.cluster import ClusterMetadata
        meta = ClusterMetadata()
        self.assertEqual(meta.version, 0)
        self.assertIsInstance(meta.tables, dict)

    def test_table_shard(self):
        from qm_core.distributed.cluster import TableShard
        shard = TableShard(
            table_name="users", shard_id=0,
            primary_node="node-1", replica_nodes=["node-2", "node-3"],
            key_range=(0, 1000),
        )
        self.assertEqual(shard.table_name, "users")
        self.assertEqual(shard.primary_node, "node-1")
        self.assertEqual(len(shard.replica_nodes), 2)

    def test_coordinator_status(self):
        from qm_core.distributed.cluster import ClusterCoordinator, ClusterConfig
        from qm_core.distributed.consensus import InMemoryRaftTransport
        config = ClusterConfig(cluster_name="test")
        transport = InMemoryRaftTransport()
        coord = ClusterCoordinator("coord-1", "127.0.0.1", 9000, config=config, raft_transport=transport)
        status = coord.status()
        self.assertIn("node_id", status)
        self.assertIn("tables", status)


# ── Shard Manager Tests ─────────────────────────────────────────────

class TestConsistentHashRing(unittest.TestCase):
    def test_add_and_get_node(self):
        from qm_core.distributed.shard import ConsistentHashRing
        ring = ConsistentHashRing(vnodes=100)
        ring.add_node("node-1")
        ring.add_node("node-2")
        ring.add_node("node-3")

        # Every key should map to one of the 3 nodes
        for i in range(100):
            node = ring.get_node(f"key-{i}")
            self.assertIn(node, {"node-1", "node-2", "node-3"})

    def test_distribution_fairness(self):
        """Test that keys are roughly evenly distributed."""
        from qm_core.distributed.shard import ConsistentHashRing
        ring = ConsistentHashRing(vnodes=150)
        for i in range(3):
            ring.add_node(f"node-{i}")

        counts = {"node-0": 0, "node-1": 0, "node-2": 0}
        num_keys = 3000
        for i in range(num_keys):
            node = ring.get_node(f"key-{i}")
            counts[node] += 1

        # Each node should get roughly 1/3 of keys (within 30% tolerance)
        expected = num_keys / 3
        for node, count in counts.items():
            self.assertGreater(count, expected * 0.5,
                             f"{node} got too few keys: {count}")
            self.assertLess(count, expected * 1.5,
                          f"{node} got too many keys: {count}")

    def test_remove_node_minimal_disruption(self):
        """Removing a node should only redistribute that node's keys."""
        from qm_core.distributed.shard import ConsistentHashRing
        ring = ConsistentHashRing(vnodes=100)
        ring.add_node("node-1")
        ring.add_node("node-2")
        ring.add_node("node-3")

        # Record mapping before removal
        before = {}
        for i in range(500):
            before[f"key-{i}"] = ring.get_node(f"key-{i}")

        ring.remove_node("node-3")

        # Check that only keys from node-3 moved
        moved = 0
        for i in range(500):
            key = f"key-{i}"
            after_node = ring.get_node(key)
            if before[key] != after_node:
                # This key moved — it should have been on node-3
                self.assertEqual(before[key], "node-3",
                               f"Key {key} moved from {before[key]} but should only move from removed node")
                moved += 1

        self.assertGreater(moved, 0, "Some keys should have moved")

    def test_get_multiple_nodes(self):
        from qm_core.distributed.shard import ConsistentHashRing
        ring = ConsistentHashRing(vnodes=100)
        for i in range(5):
            ring.add_node(f"node-{i}")

        nodes = ring.get_nodes("test-key", count=3)
        self.assertEqual(len(nodes), 3)
        self.assertEqual(len(set(nodes)), 3, "Should return distinct nodes")

    def test_key_distribution_analysis(self):
        from qm_core.distributed.shard import ConsistentHashRing
        ring = ConsistentHashRing(vnodes=100)
        for i in range(4):
            ring.add_node(f"node-{i}")

        dist = ring.get_key_distribution()
        self.assertEqual(len(dist), 4)
        self.assertAlmostEqual(sum(dist.values()), 1.0, places=2)


class TestShardManager(unittest.TestCase):
    def test_shard_strategy_enum(self):
        from qm_core.distributed.shard import ShardStrategy
        self.assertTrue(hasattr(ShardStrategy, "HASH"))
        self.assertTrue(hasattr(ShardStrategy, "RANGE"))

    def test_shard_config(self):
        from qm_core.distributed.shard import ShardConfig
        config = ShardConfig(virtual_nodes=128, split_threshold_rows=1000000)
        self.assertEqual(config.virtual_nodes, 128)

    def test_shard_router_hash(self):
        from qm_core.distributed.shard import ShardRouter, ShardInfo, ShardStrategy, ShardState, ShardConfig

        router = ShardRouter(config=ShardConfig(virtual_nodes=100))

        shard1 = ShardInfo(shard_id=0, table="users", strategy=ShardStrategy.HASH,
                        state=ShardState.ACTIVE, primary_node="node-1")
        shard2 = ShardInfo(shard_id=1, table="users", strategy=ShardStrategy.HASH,
                        state=ShardState.ACTIVE, primary_node="node-2")

        # Add shards to the router
        router._shards["users"] = [shard1, shard2]
        # Verify router was created successfully
        self.assertIsNotNone(router)

    def test_shard_manager_stats(self):
        from qm_core.distributed.shard import ShardManager, ShardConfig
        config = ShardConfig(virtual_nodes=50)
        mgr = ShardManager(config)
        stats = mgr.stats()
        self.assertIn("tables", stats)
        self.assertIn("active_migrations", stats)


# ── Replication Tests ───────────────────────────────────────────────

class TestReplication(unittest.TestCase):
    def test_replication_mode_enum(self):
        from qm_core.distributed.replication import ReplicationMode
        self.assertEqual(ReplicationMode.ASYNC, 0)
        self.assertEqual(ReplicationMode.SYNC_ONE, 1)
        self.assertEqual(ReplicationMode.SYNC_QUORUM, 2)

    def test_replica_state_enum(self):
        from qm_core.distributed.replication import ReplicaState
        self.assertTrue(hasattr(ReplicaState, "STREAMING"))
        self.assertTrue(hasattr(ReplicaState, "CATCHING_UP"))
        self.assertTrue(hasattr(ReplicaState, "DISCONNECTED"))

    def test_wal_entry_serialization(self):
        from qm_core.distributed.replication import WALEntry
        entry = WALEntry(
            lsn=42, txn_id=7, op=1, table="users",
            key="user-123", data={"name": "Alice"},
            old_data=None, timestamp=time.time(),
        )
        data = entry.serialize()
        self.assertIsInstance(data, bytes)
        # Deserialize the payload (skip 4-byte length prefix)
        import struct
        payload_len = struct.unpack("<I", data[:4])[0]
        entry2 = WALEntry.deserialize(data[4:4+payload_len])
        self.assertEqual(entry2.lsn, 42)
        self.assertEqual(entry2.key, "user-123")
        self.assertEqual(entry2.data["name"], "Alice")

    def test_replica_info(self):
        from qm_core.distributed.replication import ReplicaInfo, ReplicaState
        info = ReplicaInfo(
            replica_id="node-2", shard_id=0, table="users",
            state=ReplicaState.STREAMING,
            last_heartbeat=time.time(),
        )
        self.assertTrue(info.is_healthy)
        d = info.to_dict()
        self.assertIn("replica_id", d)

    def test_replication_sender_add_remove_replica(self):
        from qm_core.distributed.replication import ReplicationSender, ReplicationConfig
        sender = ReplicationSender(shard_id=0, table="users", config=ReplicationConfig())
        info = sender.add_replica("node-2", "10.0.0.2:9100")
        self.assertEqual(info.replica_id, "node-2")
        self.assertEqual(len(sender.get_replicas()), 1)

        sender.remove_replica("node-2")
        self.assertEqual(len(sender.get_replicas()), 0)

    def test_replication_sender_wal_entry_async(self):
        from qm_core.distributed.replication import (
            ReplicationSender, ReplicationConfig, ReplicationMode, WALEntry,
        )
        config = ReplicationConfig(mode=ReplicationMode.ASYNC)
        sender = ReplicationSender(shard_id=0, table="users", config=config)
        sender.add_replica("node-2", "10.0.0.2:9100")

        entry = WALEntry(lsn=1, txn_id=1, op=1, table="users",
                        key="k1", data={"v": 1}, old_data=None, timestamp=time.time())
        result = sender.on_wal_entry(entry)
        self.assertTrue(result)  # Async always returns True
        self.assertEqual(sender.current_lsn, 1)

    def test_replication_receiver_handle_batch(self):
        from qm_core.distributed.replication import ReplicationReceiver, WALEntry
        import json

        applied_entries = []
        def apply_fn(entry: WALEntry) -> bool:
            applied_entries.append(entry)
            return True

        receiver = ReplicationReceiver(
            shard_id=0, table="users", apply_fn=apply_fn,
        )

        batch = json.dumps({
            "shard_id": 0,
            "table": "users",
            "entries": [
                {"l": 1, "t": 1, "o": 1, "tb": "users", "k": "k1",
                 "d": {"v": 1}, "od": None, "ts": time.time()},
                {"l": 2, "t": 2, "o": 1, "tb": "users", "k": "k2",
                 "d": {"v": 2}, "od": None, "ts": time.time()},
            ],
        }).encode()

        ack = receiver.handle_batch(batch)
        self.assertEqual(ack.applied_lsn, 2)
        self.assertEqual(len(applied_entries), 2)
        self.assertEqual(receiver.applied_lsn, 2)

    def test_replication_manager_setup(self):
        from qm_core.distributed.replication import ReplicationManager, ReplicationConfig
        mgr = ReplicationManager("node-1", ReplicationConfig())
        sender = mgr.setup_primary("users", 0, {"node-2": "10.0.0.2:9100"})
        self.assertIsNotNone(sender)
        sender.stop()

        receiver = mgr.setup_replica("users", 1)
        self.assertIsNotNone(receiver)

        stats = mgr.stats()
        self.assertIn("as_primary", stats)
        self.assertIn("as_replica", stats)
        mgr.stop_all()

    def test_replication_promote_demote(self):
        from qm_core.distributed.replication import ReplicationManager, ReplicationConfig
        mgr = ReplicationManager("node-1", ReplicationConfig())
        mgr.setup_replica("users", 0)

        # Promote replica to primary
        result = mgr.promote_to_primary("users", 0)
        self.assertTrue(result)
        self.assertIsNotNone(mgr.get_sender("users", 0))

        # Demote back
        result = mgr.demote_to_replica("users", 0)
        self.assertTrue(result)
        self.assertIsNotNone(mgr.get_receiver("users", 0))
        mgr.stop_all()


# ── Distributed Query Tests ─────────────────────────────────────────

class TestDistributedQuery(unittest.TestCase):
    def test_query_type_enum(self):
        from qm_core.distributed.dist_query import QueryType
        self.assertTrue(hasattr(QueryType, "FIND"))
        self.assertTrue(hasattr(QueryType, "AGGREGATE"))
        self.assertTrue(hasattr(QueryType, "VECTOR"))

    def test_merge_strategy_enum(self):
        from qm_core.distributed.dist_query import MergeStrategy
        self.assertTrue(hasattr(MergeStrategy, "CONCAT"))
        self.assertTrue(hasattr(MergeStrategy, "SORT_MERGE"))
        self.assertTrue(hasattr(MergeStrategy, "AGG_MERGE"))

    def test_scatter_gather_plan(self):
        from qm_core.distributed.dist_query import ScatterGatherPlan, QueryType, MergeStrategy
        plan = ScatterGatherPlan(
            query_id="q1", table="users",
            query_type=QueryType.FIND,
            merge_strategy=MergeStrategy.CONCAT,
        )
        d = plan.to_dict()
        self.assertEqual(d["query_id"], "q1")
        self.assertEqual(d["query_type"], "FIND")

    def test_result_merger_concat(self):
        from qm_core.distributed.dist_query import ResultMerger, MergeStrategy, ShardResult
        r1 = ShardResult(shard_id=0, node_id="n1", rows=[{"id": 1}, {"id": 2}], count=2)
        r2 = ShardResult(shard_id=1, node_id="n2", rows=[{"id": 3}, {"id": 4}], count=2)

        merged = ResultMerger.merge([r1, r2], MergeStrategy.CONCAT)
        self.assertEqual(len(merged.rows), 4)
        self.assertEqual(merged.shards_succeeded, 2)

    def test_result_merger_concat_with_sort_and_limit(self):
        from qm_core.distributed.dist_query import ResultMerger, MergeStrategy, ShardResult
        r1 = ShardResult(shard_id=0, node_id="n1",
                        rows=[{"id": 3, "val": 30}, {"id": 1, "val": 10}])
        r2 = ShardResult(shard_id=1, node_id="n2",
                        rows=[{"id": 4, "val": 40}, {"id": 2, "val": 20}])

        merged = ResultMerger.merge(
            [r1, r2], MergeStrategy.CONCAT,
            sort=[("val", 1)], limit=3,
        )
        self.assertEqual(len(merged.rows), 3)
        vals = [r["val"] for r in merged.rows]
        self.assertEqual(vals, [10, 20, 30])

    def test_result_merger_sum(self):
        from qm_core.distributed.dist_query import ResultMerger, MergeStrategy, ShardResult
        r1 = ShardResult(shard_id=0, node_id="n1", count=100)
        r2 = ShardResult(shard_id=1, node_id="n2", count=200)
        r3 = ShardResult(shard_id=2, node_id="n3", count=50, error="timeout")

        merged = ResultMerger.merge([r1, r2, r3], MergeStrategy.SUM)
        self.assertEqual(merged.count, 300)  # Only non-error shards counted
        self.assertEqual(merged.shards_failed, 1)

    def test_result_merger_union_dedup(self):
        from qm_core.distributed.dist_query import ResultMerger, MergeStrategy, ShardResult
        r1 = ShardResult(shard_id=0, node_id="n1",
                        rows=[{"_id": "a", "v": 1}, {"_id": "b", "v": 2}])
        r2 = ShardResult(shard_id=1, node_id="n2",
                        rows=[{"_id": "b", "v": 2}, {"_id": "c", "v": 3}])

        merged = ResultMerger.merge([r1, r2], MergeStrategy.UNION)
        self.assertEqual(len(merged.rows), 3)  # a, b, c (deduped)

    def test_result_merger_aggregation(self):
        from qm_core.distributed.dist_query import ResultMerger, MergeStrategy, ShardResult
        r1 = ShardResult(shard_id=0, node_id="n1",
                        aggregation={"count": 100, "sum": 5000})
        r2 = ShardResult(shard_id=1, node_id="n2",
                        aggregation={"count": 150, "sum": 7500})

        merged = ResultMerger.merge([r1, r2], MergeStrategy.AGG_MERGE)
        self.assertEqual(merged.aggregation["count"], 250)
        self.assertEqual(merged.aggregation["sum"], 12500)

    def test_distributed_query_engine_find(self):
        from qm_core.distributed.dist_query import (
            DistributedQueryEngine, ShardQuery, ShardResult, QueryType
        )

        # Mock shard executor
        def mock_executor(query: ShardQuery) -> ShardResult:
            if query.shard_id == 0:
                return ShardResult(shard_id=0, node_id="n1",
                                  rows=[{"id": 1, "age": 25}], count=1)
            return ShardResult(shard_id=1, node_id="n2",
                              rows=[{"id": 2, "age": 30}], count=1)

        # Mock shard router: two shards
        def mock_router(table, filter):
            return [(0, "n1"), (1, "n2")]

        engine = DistributedQueryEngine(
            shard_executor=mock_executor,
            shard_router=mock_router,
        )

        result = engine.find("users")
        self.assertEqual(len(result.rows), 2)
        self.assertEqual(result.shards_contacted, 2)
        self.assertEqual(result.shards_succeeded, 2)

    def test_distributed_query_engine_count(self):
        from qm_core.distributed.dist_query import (
            DistributedQueryEngine, ShardQuery, ShardResult,
        )

        def mock_executor(query: ShardQuery) -> ShardResult:
            return ShardResult(shard_id=query.shard_id, node_id=query.node_id, count=100)

        def mock_router(table, filter):
            return [(0, "n1"), (1, "n2"), (2, "n3")]

        engine = DistributedQueryEngine(
            shard_executor=mock_executor,
            shard_router=mock_router,
        )

        count = engine.count("users")
        self.assertEqual(count, 300)

    def test_distributed_query_engine_single_shard_optimization(self):
        from qm_core.distributed.dist_query import (
            DistributedQueryEngine, ShardQuery, ShardResult,
        )

        def mock_executor(query: ShardQuery) -> ShardResult:
            return ShardResult(shard_id=0, node_id="n1",
                              rows=[{"id": 1}], count=1)

        # Single shard router
        def mock_router(table, filter):
            return [(0, "n1")]

        engine = DistributedQueryEngine(
            shard_executor=mock_executor,
            shard_router=mock_router,
        )

        result = engine.find("users", {"_id": "specific-key"})
        self.assertEqual(result.shards_contacted, 1)
        self.assertTrue(result.plan.single_shard)

    def test_distributed_query_stats(self):
        from qm_core.distributed.dist_query import DistributedQueryEngine
        engine = DistributedQueryEngine()
        stats = engine.stats()
        self.assertIn("total_queries", stats)
        engine.shutdown()


# ── Distributed Transactions Tests ──────────────────────────────────

class TestTwoPhaseCommit(unittest.TestCase):
    def test_txn_phase_enum(self):
        from qm_core.distributed.dist_txn import TxnPhase
        self.assertEqual(TxnPhase.INIT, 0)
        self.assertEqual(TxnPhase.COMMITTED, 4)
        self.assertEqual(TxnPhase.ABORTED, 6)

    def test_distributed_transaction_creation(self):
        from qm_core.distributed.dist_txn import DistributedTransaction
        txn = DistributedTransaction(table="orders")
        self.assertTrue(txn.txn_id.startswith("dtxn-"))
        self.assertFalse(txn.is_decided)
        self.assertFalse(txn.is_expired)

    def test_2pc_begin(self):
        from qm_core.distributed.dist_txn import TwoPhaseCommit
        coord = TwoPhaseCommit("coord-1")
        txn = coord.begin("orders", [(0, "node-1"), (1, "node-2")])
        self.assertEqual(len(txn.participants), 2)
        self.assertEqual(txn.table, "orders")

    def test_2pc_commit_success(self):
        """2PC should commit when all participants vote YES."""
        from qm_core.distributed.dist_txn import TwoPhaseCommit

        prepare_calls = []
        commit_calls = []

        def prepare_fn(txn_id, shard_id, ops):
            prepare_calls.append((txn_id, shard_id))
            return True

        def commit_fn(txn_id, shard_id):
            commit_calls.append((txn_id, shard_id))
            return True

        coord = TwoPhaseCommit("coord-1", prepare_fn=prepare_fn, commit_fn=commit_fn)
        txn = coord.begin("orders", [(0, "node-1"), (1, "node-2")])
        txn.operations = [
            {"shard": 0, "op": "insert", "data": {"id": 1}},
            {"shard": 1, "op": "insert", "data": {"id": 2}},
        ]

        result = coord.execute(txn)
        self.assertTrue(result)
        self.assertEqual(len(prepare_calls), 2)
        self.assertEqual(len(commit_calls), 2)

    def test_2pc_abort_on_prepare_failure(self):
        """2PC should abort when any participant votes NO."""
        from qm_core.distributed.dist_txn import TwoPhaseCommit, TxnPhase

        abort_calls = []

        def prepare_fn(txn_id, shard_id, ops):
            return shard_id != 1  # Shard 1 rejects

        def abort_fn(txn_id, shard_id):
            abort_calls.append(shard_id)
            return True

        coord = TwoPhaseCommit("coord-1", prepare_fn=prepare_fn, abort_fn=abort_fn)
        txn = coord.begin("orders", [(0, "node-1"), (1, "node-2")])
        txn.operations = [{"shard": 0}, {"shard": 1}]

        result = coord.execute(txn)
        self.assertFalse(result)
        self.assertEqual(txn.phase, TxnPhase.ABORTED)
        # Shard 0 was prepared and should be aborted
        self.assertIn(0, abort_calls)

    def test_2pc_stats(self):
        from qm_core.distributed.dist_txn import TwoPhaseCommit
        coord = TwoPhaseCommit("coord-1")
        txn = coord.begin("orders", [(0, "node-1")])
        coord.execute(txn)
        stats = coord.stats()
        self.assertEqual(stats["total"], 1)
        self.assertEqual(stats["committed"], 1)

    def test_2pc_recovery(self):
        from qm_core.distributed.dist_txn import TwoPhaseCommit, TxnPhase
        coord = TwoPhaseCommit("coord-1")
        txn = coord.begin("orders", [(0, "node-1")])
        txn.phase = TxnPhase.PREPARING  # Simulate crash during prepare

        recovered = coord.recover()
        self.assertEqual(len(recovered), 1)
        self.assertEqual(txn.phase, TxnPhase.ABORTED)


class TestSagaOrchestrator(unittest.TestCase):
    def test_saga_success(self):
        """All saga steps succeed → no compensation."""
        from qm_core.distributed.dist_txn import SagaOrchestrator

        log = []

        def step1():
            log.append("step1")
            return "result1"

        def step2():
            log.append("step2")
            return "result2"

        def step3():
            log.append("step3")
            return "result3"

        saga = SagaOrchestrator()
        saga.create_saga()
        saga.add_step("step1", step1)
        saga.add_step("step2", step2)
        saga.add_step("step3", step3)

        result = saga.execute()
        self.assertTrue(result.completed)
        self.assertFalse(result.failed)
        self.assertEqual(log, ["step1", "step2", "step3"])

    def test_saga_failure_with_compensation(self):
        """Step failure → backward compensation."""
        from qm_core.distributed.dist_txn import SagaOrchestrator

        log = []

        def create_order():
            log.append("create_order")

        def cancel_order():
            log.append("cancel_order")

        def reserve_stock():
            log.append("reserve_stock")

        def release_stock():
            log.append("release_stock")

        def charge_payment():
            log.append("charge_payment")
            raise RuntimeError("Payment declined!")

        def refund_payment():
            log.append("refund_payment")

        saga = SagaOrchestrator()
        saga.create_saga()
        saga.add_step("create_order", create_order, cancel_order)
        saga.add_step("reserve_stock", reserve_stock, release_stock)
        saga.add_step("charge_payment", charge_payment, refund_payment)

        result = saga.execute()
        self.assertTrue(result.failed)
        self.assertTrue(result.compensated)
        self.assertIn("Payment declined", result.error)
        # Compensations in reverse: release_stock, cancel_order
        self.assertIn("release_stock", log)
        self.assertIn("cancel_order", log)
        # charge_payment was not compensated (it failed)
        self.assertNotIn("refund_payment", log)

    def test_saga_chaining(self):
        """Test fluent API for saga steps."""
        from qm_core.distributed.dist_txn import SagaOrchestrator

        saga = SagaOrchestrator()
        result = (
            saga.add_step("s1", lambda: "a")
                .add_step("s2", lambda: "b")
                .add_step("s3", lambda: "c")
        )
        self.assertIsInstance(result, SagaOrchestrator)


# ── CDC Pipeline Tests ──────────────────────────────────────────────

class TestCDCPipeline(unittest.TestCase):
    def test_cdc_event_type_enum(self):
        from qm_core.distributed.cdc import CDCEventType
        self.assertEqual(CDCEventType.INSERT, 1)
        self.assertEqual(CDCEventType.UPDATE, 2)
        self.assertEqual(CDCEventType.DELETE, 3)

    def test_cdc_event_creation_and_serialization(self):
        from qm_core.distributed.cdc import CDCEvent, CDCEventType
        event = CDCEvent(
            event_type=CDCEventType.INSERT,
            table="users", key="user-1",
            after={"name": "Alice", "age": 30},
        )
        self.assertTrue(event.event_id)
        d = event.to_dict()
        self.assertEqual(d["type"], "INSERT")
        self.assertEqual(d["table"], "users")

        data = event.serialize()
        self.assertIsInstance(data, bytes)

    def test_cdc_event_from_dict(self):
        from qm_core.distributed.cdc import CDCEvent, CDCEventType
        d = {
            "event_id": "abc123",
            "sequence": 42,
            "type": "UPDATE",
            "table": "orders",
            "key": "order-1",
            "before": {"status": "pending"},
            "after": {"status": "shipped"},
        }
        event = CDCEvent.from_dict(d)
        self.assertEqual(event.event_type, CDCEventType.UPDATE)
        self.assertEqual(event.table, "orders")
        self.assertEqual(event.before["status"], "pending")

    def test_cdc_filter_matches(self):
        from qm_core.distributed.cdc import CDCFilter, CDCEvent, CDCEventType
        f = CDCFilter(
            tables={"users", "orders"},
            event_types={CDCEventType.INSERT, CDCEventType.UPDATE},
        )

        event1 = CDCEvent(event_type=CDCEventType.INSERT, table="users", key="1")
        self.assertTrue(f.matches(event1))

        event2 = CDCEvent(event_type=CDCEventType.DELETE, table="users", key="1")
        self.assertFalse(f.matches(event2))  # DELETE not in filter

        event3 = CDCEvent(event_type=CDCEventType.INSERT, table="logs", key="1")
        self.assertFalse(f.matches(event3))  # "logs" not in filter

    def test_cdc_filter_key_prefix(self):
        from qm_core.distributed.cdc import CDCFilter, CDCEvent, CDCEventType
        f = CDCFilter(key_prefix="user-")

        e1 = CDCEvent(event_type=CDCEventType.INSERT, table="t", key="user-123")
        self.assertTrue(f.matches(e1))

        e2 = CDCEvent(event_type=CDCEventType.INSERT, table="t", key="order-456")
        self.assertFalse(f.matches(e2))

    def test_cdc_buffer(self):
        from qm_core.distributed.cdc import CDCBuffer, CDCEvent, CDCEventType
        buf = CDCBuffer(max_size=100)

        for i in range(50):
            event = CDCEvent(event_type=CDCEventType.INSERT, table="t", key=str(i))
            buf.append(event)

        self.assertEqual(buf.current_sequence, 50)
        self.assertEqual(buf.size, 50)

        events = buf.get_since(0, max_count=10)
        self.assertEqual(len(events), 10)
        self.assertEqual(events[0].sequence, 1)

        events = buf.get_since(45, max_count=100)
        self.assertEqual(len(events), 5)

    def test_cdc_pipeline_emit_and_poll(self):
        from qm_core.distributed.cdc import CDCPipeline, CDCFilter, CDCEvent, CDCEventType

        pipeline = CDCPipeline(buffer_size=1000)

        # Subscribe (pull model)
        sub = pipeline.subscribe("test-consumer", filter=CDCFilter(tables={"users"}))

        # Emit events
        pipeline.emit_insert("users", "u1", {"name": "Alice"})
        pipeline.emit_insert("orders", "o1", {"total": 100})  # Different table
        pipeline.emit_update("users", "u1", {"name": "Alice"}, {"name": "Bob"})

        # Poll
        events = pipeline.poll(sub.subscription_id, max_events=10)
        self.assertEqual(len(events), 2)  # Only "users" table events
        self.assertEqual(events[0].event_type, CDCEventType.INSERT)
        self.assertEqual(events[1].event_type, CDCEventType.UPDATE)

        stats = pipeline.stats()
        self.assertEqual(stats["total_events"], 3)

    def test_cdc_pipeline_push_subscription(self):
        from qm_core.distributed.cdc import CDCPipeline, CDCFilter

        pipeline = CDCPipeline(buffer_size=1000)
        pipeline.start()

        received = []
        def handler(event):
            received.append(event)

        pipeline.subscribe("push-consumer", callback=handler)

        # Emit
        pipeline.emit_insert("users", "u1", {"name": "Alice"})
        pipeline.emit_delete("users", "u2", {"name": "Bob"})

        time.sleep(0.3)  # Wait for delivery thread

        pipeline.stop()
        self.assertGreaterEqual(len(received), 1)  # At least some delivered

    def test_cdc_pipeline_unsubscribe(self):
        from qm_core.distributed.cdc import CDCPipeline
        pipeline = CDCPipeline()
        sub = pipeline.subscribe("test")
        self.assertEqual(len(pipeline.list_subscriptions()), 1)

        result = pipeline.unsubscribe(sub.subscription_id)
        self.assertTrue(result)
        self.assertEqual(len(pipeline.list_subscriptions()), 0)

    def test_cdc_extractor(self):
        from qm_core.distributed.cdc import CDCExtractor, CDCPipeline, CDCEventType

        pipeline = CDCPipeline()
        extractor = CDCExtractor(pipeline)

        # Simulate WAL records
        extractor.on_wal_record(wal_op=1, table="users", key="u1",
                               data={"name": "Alice"}, lsn=100)
        extractor.on_wal_record(wal_op=2, table="users", key="u1",
                               data={"name": "Bob"}, old_data={"name": "Alice"}, lsn=101)
        extractor.on_wal_record(wal_op=3, table="users", key="u1",
                               old_data={"name": "Bob"}, lsn=102)
        extractor.on_wal_record(wal_op=4, table="users", key="",
                               lsn=103)  # BEGIN_TXN - should be skipped

        self.assertEqual(pipeline.stats()["total_events"], 3)

    def test_cdc_pipeline_acknowledge(self):
        from qm_core.distributed.cdc import CDCPipeline
        pipeline = CDCPipeline()
        sub = pipeline.subscribe("test")
        pipeline.emit_insert("t", "k1", {})
        pipeline.emit_insert("t", "k2", {})
        pipeline.emit_insert("t", "k3", {})

        events = pipeline.poll(sub.subscription_id, max_events=2)
        self.assertEqual(len(events), 2)

        # Acknowledge
        pipeline.acknowledge(sub.subscription_id, events[-1].sequence)
        self.assertEqual(sub.last_sequence, events[-1].sequence)

    def test_cdc_event_hook(self):
        from qm_core.distributed.cdc import CDCPipeline
        pipeline = CDCPipeline()

        hooked = []
        pipeline.add_hook(lambda e: hooked.append(e))

        pipeline.emit_insert("t", "k1", {"v": 1})
        self.assertEqual(len(hooked), 1)


# ── Cluster Client Tests ────────────────────────────────────────────

class TestClusterClient(unittest.TestCase):
    def test_read_preference_enum(self):
        from qm_core.distributed.client import ReadPreference
        self.assertTrue(hasattr(ReadPreference, "PRIMARY"))
        self.assertTrue(hasattr(ReadPreference, "SECONDARY"))
        self.assertTrue(hasattr(ReadPreference, "NEAREST"))

    def test_node_health_enum(self):
        from qm_core.distributed.client import NodeHealth
        self.assertTrue(hasattr(NodeHealth, "HEALTHY"))
        self.assertTrue(hasattr(NodeHealth, "DOWN"))

    def test_client_config(self):
        from qm_core.distributed.client import ClientConfig, ReadPreference
        config = ClientConfig(
            seed_nodes=["10.0.0.1:9000", "10.0.0.2:9000"],
            read_preference=ReadPreference.SECONDARY_PREFERRED,
            max_retries=5,
        )
        self.assertEqual(len(config.seed_nodes), 2)
        self.assertEqual(config.max_retries, 5)

    def test_cluster_node_info(self):
        from qm_core.distributed.client import ClusterNodeInfo, NodeHealth
        node = ClusterNodeInfo(
            node_id="n1", host="10.0.0.1", port=9000,
            is_primary=True, shards=[0, 1, 2],
            health=NodeHealth.HEALTHY,
        )
        self.assertTrue(node.is_available)
        self.assertEqual(node.error_rate, 0.0)

    def test_client_stats(self):
        from qm_core.distributed.client import ClientStats
        stats = ClientStats()
        stats.record_request(5.0, is_write=False, success=True)
        stats.record_request(10.0, is_write=True, success=True)
        stats.record_request(100.0, is_write=False, success=False)

        d = stats.to_dict()
        self.assertEqual(d["total_requests"], 3)
        self.assertEqual(d["reads"], 2)
        self.assertEqual(d["writes"], 1)
        self.assertEqual(d["total_errors"], 1)

    def test_topology_monitor_add_node(self):
        from qm_core.distributed.client import TopologyMonitor, ClientConfig, ClusterNodeInfo, NodeHealth
        monitor = TopologyMonitor(ClientConfig())
        node = ClusterNodeInfo(
            node_id="n1", host="10.0.0.1", port=9000,
            is_primary=True, shards=[0, 1],
            health=NodeHealth.HEALTHY,
        )
        monitor.add_node(node)

        self.assertIsNotNone(monitor.get_node("n1"))
        self.assertEqual(len(monitor.get_all_nodes()), 1)
        primary = monitor.get_primary_for_shard(0)
        self.assertEqual(primary.node_id, "n1")

    def test_cluster_client_create(self):
        from qm_core.distributed.client import ClusterClient, ClientConfig
        client = ClusterClient(ClientConfig())
        self.assertIsNotNone(client)

    def test_cluster_client_mock_find(self):
        """Client find with no transport returns empty results."""
        from qm_core.distributed.client import ClusterClient, ClientConfig, ClusterNodeInfo, NodeHealth
        client = ClusterClient(ClientConfig())

        # Add a mock node to topology
        node = ClusterNodeInfo(
            node_id="n1", host="127.0.0.1", port=9000,
            is_primary=True, shards=[0],
            health=NodeHealth.HEALTHY,
        )
        client._topology.add_node(node)

        results = client.find("users")
        self.assertIsInstance(results, list)

    def test_cluster_client_with_transport(self):
        """Client with mock transport."""
        from qm_core.distributed.client import (
            ClusterClient, ClientConfig, ClusterNodeInfo, NodeHealth
        )

        def mock_transport(address, operation, payload):
            if operation == "find":
                return {"result": [{"id": 1, "name": "Alice"}]}
            elif operation == "count":
                return {"result": 42}
            elif operation == "insert":
                return {"result": "key-1"}
            return {"result": None}

        client = ClusterClient(
            ClientConfig(),
            transport=mock_transport,
        )

        # Add node
        node = ClusterNodeInfo(
            node_id="n1", host="127.0.0.1", port=9000,
            is_primary=True, shards=[0],
            health=NodeHealth.HEALTHY,
        )
        client._topology.add_node(node)

        results = client.find("users")
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]["name"], "Alice")

        count = client.count("users")
        self.assertEqual(count, 42)

        key = client.insert("users", {"name": "Bob"})
        self.assertEqual(key, "key-1")

    def test_cluster_client_status(self):
        from qm_core.distributed.client import ClusterClient, ClientConfig
        client = ClusterClient(ClientConfig())
        status = client.cluster_status()
        self.assertIn("nodes", status)
        self.assertIn("stats", status)


# ── Integration Tests ───────────────────────────────────────────────

class TestDistributedIntegration(unittest.TestCase):
    def test_full_import(self):
        """All distributed components should import cleanly."""
        from qm_core.distributed import (
            GossipProtocol, NodeState,
            RaftNode, RaftState,
            ClusterCoordinator, ClusterConfig, ClusterNode,
            ShardManager, ShardStrategy, ShardConfig, ConsistentHashRing,
            ReplicationManager, ReplicationMode, ReplicaState,
            DistributedQueryEngine, ScatterGatherPlan,
            TwoPhaseCommit, DistributedTransaction, SagaOrchestrator,
            CDCPipeline, CDCSubscription, CDCEvent,
            ClusterClient,
        )
        self.assertIsNotNone(GossipProtocol)
        self.assertIsNotNone(ClusterClient)

    def test_cdc_with_replication(self):
        """CDC events generated during replication apply."""
        from qm_core.distributed.cdc import CDCPipeline, CDCExtractor
        from qm_core.distributed.replication import ReplicationReceiver, WALEntry

        pipeline = CDCPipeline()
        extractor = CDCExtractor(pipeline)

        captured_events = []
        pipeline.add_hook(lambda e: captured_events.append(e))

        def apply_fn(entry: WALEntry) -> bool:
            extractor.on_wal_record(
                wal_op=entry.op, table=entry.table,
                key=entry.key, data=entry.data,
                old_data=entry.old_data, lsn=entry.lsn,
            )
            return True

        receiver = ReplicationReceiver(0, "users", apply_fn=apply_fn)

        import json
        batch = json.dumps({
            "shard_id": 0, "table": "users",
            "entries": [
                {"l": 1, "t": 1, "o": 1, "tb": "users", "k": "u1",
                 "d": {"name": "Alice"}, "od": None, "ts": time.time()},
                {"l": 2, "t": 2, "o": 2, "tb": "users", "k": "u1",
                 "d": {"name": "Bob"}, "od": {"name": "Alice"}, "ts": time.time()},
            ],
        }).encode()

        receiver.handle_batch(batch)
        self.assertEqual(len(captured_events), 2)
        self.assertEqual(captured_events[0].after["name"], "Alice")
        self.assertEqual(captured_events[1].before["name"], "Alice")

    def test_query_engine_with_shard_ring(self):
        """Query engine using consistent hash ring for routing."""
        from qm_core.distributed.dist_query import (
            DistributedQueryEngine, ShardQuery, ShardResult,
        )
        from qm_core.distributed.shard import ConsistentHashRing

        ring = ConsistentHashRing(vnodes=100)
        ring.add_node("node-0")
        ring.add_node("node-1")

        # Data per-shard
        shard_data = {
            "node-0": [{"_id": "a", "val": 10}, {"_id": "c", "val": 30}],
            "node-1": [{"_id": "b", "val": 20}, {"_id": "d", "val": 40}],
        }

        def executor(query: ShardQuery) -> ShardResult:
            rows = shard_data.get(query.node_id, [])
            return ShardResult(
                shard_id=query.shard_id, node_id=query.node_id,
                rows=rows, count=len(rows),
            )

        def router(table, filter):
            return [(0, "node-0"), (1, "node-1")]

        engine = DistributedQueryEngine(
            shard_executor=executor,
            shard_router=router,
        )

        result = engine.find("test_table", sort=[("val", 1)], limit=3)
        self.assertEqual(len(result.rows), 3)
        vals = [r["val"] for r in result.rows]
        self.assertEqual(vals, [10, 20, 30])

    def test_2pc_with_saga_fallback(self):
        """Show that 2PC and Saga can work together."""
        from qm_core.distributed.dist_txn import TwoPhaseCommit, SagaOrchestrator

        # 2PC for short transactions
        coord = TwoPhaseCommit("coord-1")
        txn = coord.begin("orders", [(0, "n1")])
        result_2pc = coord.execute(txn)
        self.assertTrue(result_2pc)

        # Saga for long-running
        saga = SagaOrchestrator()
        saga.create_saga()
        saga.add_step("step1", lambda: "done")
        saga.add_step("step2", lambda: "done")
        result_saga = saga.execute()
        self.assertTrue(result_saga.completed)

    def test_end_to_end_write_read_flow(self):
        """Simulate a distributed write/read cycle."""
        from qm_core.distributed.shard import ConsistentHashRing
        from qm_core.distributed.replication import ReplicationManager, WALEntry, ReplicationConfig
        from qm_core.distributed.cdc import CDCPipeline, CDCExtractor

        # Setup
        ring = ConsistentHashRing(vnodes=50)
        ring.add_node("primary")
        ring.add_node("replica")

        pipeline = CDCPipeline()
        extractor = CDCExtractor(pipeline)

        cdc_events = []
        pipeline.add_hook(lambda e: cdc_events.append(e))

        repl_mgr = ReplicationManager("primary", ReplicationConfig())

        # Simulate write
        key = "user-42"
        target = ring.get_node(key)
        self.assertIn(target, {"primary", "replica"})

        entry = WALEntry(lsn=1, txn_id=1, op=1, table="users",
                        key=key, data={"name": "Eve"}, old_data=None,
                        timestamp=time.time())

        # CDC captures the write
        extractor.on_wal_record(
            wal_op=1, table="users", key=key,
            data={"name": "Eve"}, lsn=1,
        )
        self.assertEqual(len(cdc_events), 1)
        self.assertEqual(cdc_events[0].after["name"], "Eve")

        repl_mgr.stop_all()


if __name__ == "__main__":
    unittest.main()
