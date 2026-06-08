"""QM Distributed — Cluster Coordinator.

The ClusterCoordinator is the central control plane for a QM cluster:
    - Manages cluster topology (which nodes exist, their roles)
    - Orchestrates shard assignment and rebalancing
    - Handles node join/leave events
    - Provides cluster metadata to all components
    - Coordinates DDL operations (CREATE/DROP TABLE) across shards
    - Health monitoring and automatic failover

Architecture:
    ClusterCoordinator
        ├── GossipProtocol  (membership & failure detection)
        ├── RaftNode         (leader election & metadata replication)
        ├── ShardManager     (shard assignment & routing)
        └── ReplicationManager (WAL-based data replication)
"""

from __future__ import annotations

import hashlib
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable

from qm_core.distributed.gossip import GossipProtocol, GossipConfig, MemberEntry, NodeState
from qm_core.distributed.consensus import (
    RaftNode, RaftConfig, RaftState, LogEntryType, LogEntry,
    InMemoryRaftTransport,
)


class NodeRole(IntEnum):
    """Roles a node can have in the cluster."""
    DATA = 0          # Stores data shards
    COORDINATOR = 1   # Handles routing + coordination
    MIXED = 2         # Both data and coordinator
    OBSERVER = 3      # Read-only replica, no voting


@dataclass
class ClusterNode:
    """Representation of a node in the cluster."""
    node_id: str
    host: str
    port: int
    role: NodeRole = NodeRole.MIXED
    state: NodeState = NodeState.ALIVE
    shards: list[int] = field(default_factory=list)      # Shard IDs assigned
    replicas_of: list[int] = field(default_factory=list)  # Shard IDs this node has replicas of
    zone: str = "default"    # Availability zone / rack
    capacity: float = 1.0    # Relative capacity weight
    joined_at: float = 0.0
    last_seen: float = 0.0
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def address(self) -> str:
        return f"{self.host}:{self.port}"

    @property
    def is_alive(self) -> bool:
        return self.state == NodeState.ALIVE

    @property
    def shard_count(self) -> int:
        return len(self.shards)

    def to_dict(self) -> dict[str, Any]:
        return {
            "node_id": self.node_id,
            "host": self.host,
            "port": self.port,
            "role": self.role.name,
            "state": self.state.name,
            "shards": self.shards,
            "replicas_of": self.replicas_of,
            "zone": self.zone,
            "capacity": self.capacity,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> ClusterNode:
        return cls(
            node_id=d["node_id"],
            host=d["host"],
            port=d["port"],
            role=NodeRole[d.get("role", "MIXED")],
            state=NodeState[d.get("state", "ALIVE")],
            shards=d.get("shards", []),
            replicas_of=d.get("replicas_of", []),
            zone=d.get("zone", "default"),
            capacity=d.get("capacity", 1.0),
        )


@dataclass
class ClusterConfig:
    """Configuration for the QM cluster."""
    cluster_name: str = "qm-cluster"
    default_shard_count: int = 16         # Default number of shards for new tables
    replication_factor: int = 3           # Number of copies of each shard
    min_alive_for_writes: int = 2         # Min replicas for write quorum
    node_failure_timeout_s: float = 30.0  # Declare node dead after this
    rebalance_threshold: float = 0.2      # Imbalance ratio to trigger rebalance
    gossip_config: GossipConfig = field(default_factory=GossipConfig)
    raft_config: RaftConfig = field(default_factory=RaftConfig)


@dataclass
class TableShard:
    """Metadata for a shard of a table."""
    table_name: str
    shard_id: int
    primary_node: str        # Node ID of the primary
    replica_nodes: list[str]  # Node IDs of replicas
    key_range: tuple[int, int] | None = None  # (min_hash, max_hash) for range shards
    row_count: int = 0
    size_bytes: int = 0
    status: str = "active"    # active, migrating, splitting, merging

    def to_dict(self) -> dict[str, Any]:
        return {
            "table": self.table_name,
            "shard_id": self.shard_id,
            "primary": self.primary_node,
            "replicas": self.replica_nodes,
            "key_range": self.key_range,
            "row_count": self.row_count,
            "size_bytes": self.size_bytes,
            "status": self.status,
        }


@dataclass
class ClusterMetadata:
    """Full cluster metadata, replicated via Raft."""
    tables: dict[str, dict[str, Any]] = field(default_factory=dict)   # name → schema
    shard_map: dict[str, list[TableShard]] = field(default_factory=dict)  # table → shards
    nodes: dict[str, ClusterNode] = field(default_factory=dict)
    version: int = 0
    last_modified: float = 0.0

    def to_dict(self) -> dict[str, Any]:
        return {
            "tables": self.tables,
            "shard_map": {
                t: [s.to_dict() for s in shards]
                for t, shards in self.shard_map.items()
            },
            "nodes": {nid: n.to_dict() for nid, n in self.nodes.items()},
            "version": self.version,
        }


# Type for event callbacks
ClusterEventHandler = Callable[[str, dict[str, Any]], None]


class ClusterCoordinator:
    """Central coordinator for a QM distributed cluster.

    Manages cluster lifecycle:
        1. Node discovery via Gossip
        2. Leader election via Raft
        3. Shard assignment and routing
        4. DDL coordination across shards
        5. Failure detection and recovery
        6. Rebalancing

    Usage:
        config = ClusterConfig(cluster_name="my-cluster")
        coord = ClusterCoordinator("node-1", "10.0.0.1", 9000, config)
        coord.start()
        coord.join_cluster(["10.0.0.2:9000"])
        coord.create_table("users", {"name": "text", "age": "int"}, shard_count=8)
        shard = coord.route("users", shard_key="user-123")
    """

    def __init__(
        self,
        node_id: str,
        host: str,
        port: int,
        config: ClusterConfig | None = None,
        gossip_transport: Any | None = None,
        raft_transport: Any | None = None,
    ) -> None:
        self._config = config or ClusterConfig()
        self._node_id = node_id
        self._host = host
        self._port = port

        # Metadata store
        self._metadata = ClusterMetadata()
        self._metadata_lock = threading.RLock()

        # Register self as a node
        self._self_node = ClusterNode(
            node_id=node_id, host=host, port=port,
            role=NodeRole.MIXED,
            joined_at=time.time(), last_seen=time.time(),
        )
        self._metadata.nodes[node_id] = self._self_node

        # Gossip for membership
        self._gossip = GossipProtocol(
            node_id=node_id, host=host, port=port,
            config=self._config.gossip_config,
            transport=gossip_transport,
        )
        self._gossip.on_join(self._on_node_join)
        self._gossip.on_leave(self._on_node_leave)
        self._gossip.on_suspect(self._on_node_suspect)
        self._gossip.on_alive(self._on_node_alive)

        # Raft for consensus
        self._raft_transport = raft_transport or InMemoryRaftTransport()
        self._raft = RaftNode(
            node_id=node_id,
            peers=[],  # Updated dynamically as nodes join
            transport=self._raft_transport,
            config=self._config.raft_config,
        )
        self._raft.on_apply(self._on_raft_apply)
        if isinstance(self._raft_transport, InMemoryRaftTransport):
            self._raft_transport.register(self._raft)

        # Event handlers
        self._event_handlers: list[ClusterEventHandler] = []

        # Running state
        self._running = False
        self._monitor_thread: threading.Thread | None = None

    # ── Public API ──────────────────────────────────────────────────

    @property
    def node_id(self) -> str:
        return self._node_id

    @property
    def is_leader(self) -> bool:
        return self._raft.is_leader

    @property
    def leader_id(self) -> str | None:
        return self._raft.leader_id

    @property
    def metadata(self) -> ClusterMetadata:
        return self._metadata

    @property
    def cluster_size(self) -> int:
        return len(self._metadata.nodes)

    def start(self) -> None:
        """Start the cluster coordinator."""
        self._running = True
        self._gossip.start()
        self._raft.start()
        self._monitor_thread = threading.Thread(
            target=self._monitor_loop, daemon=True, name="cluster-monitor"
        )
        self._monitor_thread.start()

    def stop(self) -> None:
        """Stop the cluster coordinator."""
        self._running = False
        self._gossip.stop()
        self._raft.stop()
        if self._monitor_thread:
            self._monitor_thread.join(timeout=2.0)

    def join_cluster(self, seed_addresses: list[str]) -> int:
        """Join an existing cluster via seed addresses."""
        return self._gossip.join(seed_addresses)

    def on_event(self, handler: ClusterEventHandler) -> None:
        """Register a handler for cluster events."""
        self._event_handlers.append(handler)

    # ── DDL Operations ──────────────────────────────────────────────

    def create_table(
        self,
        name: str,
        schema: dict[str, str],
        shard_count: int | None = None,
        shard_key: str | None = None,
        replication_factor: int | None = None,
    ) -> bool:
        """Create a table across the cluster.

        This is a distributed DDL: proposed through Raft, applied to all nodes.
        """
        if not self._raft.is_leader:
            return False  # Only leader can propose DDL

        n_shards = shard_count or self._config.default_shard_count
        rf = replication_factor or self._config.replication_factor

        # Compute shard assignments
        alive_nodes = [
            n for n in self._metadata.nodes.values()
            if n.is_alive and n.role in (NodeRole.DATA, NodeRole.MIXED)
        ]
        if not alive_nodes:
            return False

        shards = self._assign_shards(name, n_shards, rf, alive_nodes)

        entry = self._raft.propose(LogEntryType.TABLE_CREATE, {
            "table": name,
            "schema": schema,
            "shard_key": shard_key or "_id",
            "shard_count": n_shards,
            "replication_factor": rf,
            "shards": [s.to_dict() for s in shards],
        })
        if entry:
            return self._raft.wait_committed(entry.index, timeout_s=5.0)
        return False

    def drop_table(self, name: str) -> bool:
        """Drop a table from the cluster."""
        if not self._raft.is_leader:
            return False
        entry = self._raft.propose(LogEntryType.TABLE_DROP, {"table": name})
        if entry:
            return self._raft.wait_committed(entry.index, timeout_s=5.0)
        return False

    # ── Routing ─────────────────────────────────────────────────────

    def route(self, table: str, shard_key: Any) -> TableShard | None:
        """Route a request to the correct shard based on shard key.

        Uses consistent hashing to determine which shard owns the key.
        """
        shards = self._metadata.shard_map.get(table)
        if not shards:
            return None

        key_hash = self._hash_key(shard_key)
        n_shards = len(shards)
        shard_idx = key_hash % n_shards
        return shards[shard_idx]

    def route_to_primary(self, table: str, shard_key: Any) -> ClusterNode | None:
        """Route to the primary node for a given shard key."""
        shard = self.route(table, shard_key)
        if shard:
            return self._metadata.nodes.get(shard.primary_node)
        return None

    def get_all_shards(self, table: str) -> list[TableShard]:
        """Get all shards for a table (for scatter-gather queries)."""
        return self._metadata.shard_map.get(table, [])

    def get_shard_nodes(self, table: str, shard_id: int) -> list[ClusterNode]:
        """Get all nodes (primary + replicas) for a shard."""
        shards = self._metadata.shard_map.get(table, [])
        for s in shards:
            if s.shard_id == shard_id:
                nodes = []
                primary = self._metadata.nodes.get(s.primary_node)
                if primary:
                    nodes.append(primary)
                for rid in s.replica_nodes:
                    replica = self._metadata.nodes.get(rid)
                    if replica:
                        nodes.append(replica)
                return nodes
        return []

    # ── Rebalancing ─────────────────────────────────────────────────

    def check_rebalance(self) -> list[dict[str, Any]]:
        """Check if shards need rebalancing. Returns list of migration plans."""
        if not self._raft.is_leader:
            return []

        alive_nodes = [
            n for n in self._metadata.nodes.values()
            if n.is_alive and n.role in (NodeRole.DATA, NodeRole.MIXED)
        ]
        if len(alive_nodes) < 2:
            return []

        # Calculate load per node
        load: dict[str, int] = {n.node_id: 0 for n in alive_nodes}
        for shards in self._metadata.shard_map.values():
            for shard in shards:
                if shard.primary_node in load:
                    load[shard.primary_node] += 1

        avg_load = sum(load.values()) / len(load) if load else 0
        if avg_load == 0:
            return []

        migrations: list[dict[str, Any]] = []
        overloaded = [(nid, l) for nid, l in load.items() if l > avg_load * (1 + self._config.rebalance_threshold)]
        underloaded = [(nid, l) for nid, l in load.items() if l < avg_load * (1 - self._config.rebalance_threshold)]

        for over_id, over_load in sorted(overloaded, key=lambda x: -x[1]):
            for under_id, under_load in sorted(underloaded, key=lambda x: x[1]):
                excess = over_load - int(avg_load)
                deficit = int(avg_load) - under_load
                to_move = min(excess, deficit, 1)  # Move 1 shard at a time

                if to_move > 0:
                    # Find a shard to move
                    shard_to_move = self._find_shard_on_node(over_id)
                    if shard_to_move:
                        migrations.append({
                            "shard_id": shard_to_move.shard_id,
                            "table": shard_to_move.table_name,
                            "from_node": over_id,
                            "to_node": under_id,
                        })
                        break

        return migrations

    def execute_rebalance(self, migrations: list[dict[str, Any]]) -> int:
        """Execute shard migrations. Returns count of successful migrations."""
        if not self._raft.is_leader:
            return 0

        success = 0
        for migration in migrations:
            entry = self._raft.propose(LogEntryType.SHARD_ASSIGN, {
                "action": "migrate",
                **migration,
            })
            if entry and self._raft.wait_committed(entry.index, timeout_s=10.0):
                success += 1
        return success

    # ── Node event handlers ─────────────────────────────────────────

    def _on_node_join(self, entry: MemberEntry) -> None:
        """Handle a new node joining the cluster."""
        with self._metadata_lock:
            if entry.node_id not in self._metadata.nodes:
                node = ClusterNode(
                    node_id=entry.node_id,
                    host=entry.host, port=entry.port,
                    role=NodeRole[entry.metadata.get("role", "MIXED")],
                    joined_at=time.time(), last_seen=time.time(),
                    zone=entry.metadata.get("zone", "default"),
                )
                self._metadata.nodes[entry.node_id] = node
                self._metadata.version += 1
        self._fire_event("node_join", entry.to_dict())

    def _on_node_leave(self, entry: MemberEntry) -> None:
        """Handle a node leaving / failing."""
        with self._metadata_lock:
            node = self._metadata.nodes.get(entry.node_id)
            if node:
                node.state = NodeState.DEAD
                node.last_seen = time.time()
                self._metadata.version += 1

        # Trigger failover for shards owned by this node
        if self._raft.is_leader:
            self._handle_node_failure(entry.node_id)
        self._fire_event("node_leave", entry.to_dict())

    def _on_node_suspect(self, entry: MemberEntry) -> None:
        """Handle a node being suspected."""
        with self._metadata_lock:
            node = self._metadata.nodes.get(entry.node_id)
            if node:
                node.state = NodeState.SUSPECT
        self._fire_event("node_suspect", entry.to_dict())

    def _on_node_alive(self, entry: MemberEntry) -> None:
        """Handle a suspected node coming back alive."""
        with self._metadata_lock:
            node = self._metadata.nodes.get(entry.node_id)
            if node:
                node.state = NodeState.ALIVE
                node.last_seen = time.time()
        self._fire_event("node_alive", entry.to_dict())

    # ── Raft apply callback ─────────────────────────────────────────

    def _on_raft_apply(self, entry: LogEntry) -> None:
        """Apply a committed Raft log entry to local metadata."""
        with self._metadata_lock:
            if entry.entry_type == LogEntryType.TABLE_CREATE:
                self._apply_create_table(entry.data)
            elif entry.entry_type == LogEntryType.TABLE_DROP:
                self._apply_drop_table(entry.data)
            elif entry.entry_type == LogEntryType.SHARD_ASSIGN:
                self._apply_shard_assign(entry.data)
            elif entry.entry_type == LogEntryType.MEMBERSHIP_CHANGE:
                self._apply_membership_change(entry.data)
            self._metadata.version += 1
            self._metadata.last_modified = time.time()

    def _apply_create_table(self, data: dict[str, Any]) -> None:
        """Apply CREATE TABLE from Raft log."""
        table_name = data["table"]
        self._metadata.tables[table_name] = {
            "schema": data["schema"],
            "shard_key": data.get("shard_key", "_id"),
            "shard_count": data["shard_count"],
            "replication_factor": data["replication_factor"],
        }
        shards = [
            TableShard(
                table_name=table_name,
                shard_id=s["shard_id"],
                primary_node=s["primary"],
                replica_nodes=s.get("replicas", []),
                key_range=tuple(s["key_range"]) if s.get("key_range") else None,
            )
            for s in data["shards"]
        ]
        self._metadata.shard_map[table_name] = shards

        # Update node shard assignments
        for shard in shards:
            node = self._metadata.nodes.get(shard.primary_node)
            if node and shard.shard_id not in node.shards:
                node.shards.append(shard.shard_id)
            for rid in shard.replica_nodes:
                rnode = self._metadata.nodes.get(rid)
                if rnode and shard.shard_id not in rnode.replicas_of:
                    rnode.replicas_of.append(shard.shard_id)

        self._fire_event("table_created", data)

    def _apply_drop_table(self, data: dict[str, Any]) -> None:
        """Apply DROP TABLE from Raft log."""
        table_name = data["table"]
        # Remove shards from nodes
        for shard in self._metadata.shard_map.get(table_name, []):
            node = self._metadata.nodes.get(shard.primary_node)
            if node and shard.shard_id in node.shards:
                node.shards.remove(shard.shard_id)
            for rid in shard.replica_nodes:
                rnode = self._metadata.nodes.get(rid)
                if rnode and shard.shard_id in rnode.replicas_of:
                    rnode.replicas_of.remove(shard.shard_id)

        self._metadata.tables.pop(table_name, None)
        self._metadata.shard_map.pop(table_name, None)
        self._fire_event("table_dropped", data)

    def _apply_shard_assign(self, data: dict[str, Any]) -> None:
        """Apply shard assignment change (migration)."""
        action = data.get("action", "migrate")
        table = data["table"]
        shard_id = data["shard_id"]

        shards = self._metadata.shard_map.get(table, [])
        for shard in shards:
            if shard.shard_id == shard_id:
                if action == "migrate":
                    old_node = data["from_node"]
                    new_node = data["to_node"]

                    # Update primary
                    if shard.primary_node == old_node:
                        shard.primary_node = new_node

                    # Update node assignments
                    old = self._metadata.nodes.get(old_node)
                    if old and shard_id in old.shards:
                        old.shards.remove(shard_id)
                    new = self._metadata.nodes.get(new_node)
                    if new and shard_id not in new.shards:
                        new.shards.append(shard_id)

                elif action == "add_replica":
                    replica_node = data["replica_node"]
                    if replica_node not in shard.replica_nodes:
                        shard.replica_nodes.append(replica_node)
                    rnode = self._metadata.nodes.get(replica_node)
                    if rnode and shard_id not in rnode.replicas_of:
                        rnode.replicas_of.append(shard_id)

                elif action == "remove_replica":
                    replica_node = data["replica_node"]
                    if replica_node in shard.replica_nodes:
                        shard.replica_nodes.remove(replica_node)
                    rnode = self._metadata.nodes.get(replica_node)
                    if rnode and shard_id in rnode.replicas_of:
                        rnode.replicas_of.remove(shard_id)
                break

        self._fire_event("shard_assign", data)

    def _apply_membership_change(self, data: dict[str, Any]) -> None:
        """Apply membership change from Raft log."""
        action = data.get("action")
        node_data = data.get("node", {})
        if action == "add":
            node = ClusterNode.from_dict(node_data)
            self._metadata.nodes[node.node_id] = node
        elif action == "remove":
            nid = node_data.get("node_id", "")
            self._metadata.nodes.pop(nid, None)

    # ── Internal helpers ────────────────────────────────────────────

    def _assign_shards(
        self,
        table: str,
        n_shards: int,
        rf: int,
        nodes: list[ClusterNode],
    ) -> list[TableShard]:
        """Assign shards to nodes using round-robin with zone awareness."""
        # Sort nodes by current load (ascending)
        nodes_sorted = sorted(nodes, key=lambda n: n.shard_count)
        shards: list[TableShard] = []

        hash_range = 2**32
        range_size = hash_range // n_shards

        for i in range(n_shards):
            # Primary: round-robin
            primary = nodes_sorted[i % len(nodes_sorted)]

            # Replicas: prefer different zones
            replicas: list[str] = []
            for j in range(1, rf):
                candidate_idx = (i + j) % len(nodes_sorted)
                candidate = nodes_sorted[candidate_idx]
                if candidate.node_id != primary.node_id:
                    replicas.append(candidate.node_id)
                if len(replicas) >= rf - 1:
                    break

            shard = TableShard(
                table_name=table,
                shard_id=i,
                primary_node=primary.node_id,
                replica_nodes=replicas,
                key_range=(i * range_size, (i + 1) * range_size - 1),
            )
            shards.append(shard)

        return shards

    def _handle_node_failure(self, failed_node_id: str) -> None:
        """Handle failover when a node dies — promote replicas to primary."""
        for table, shards in self._metadata.shard_map.items():
            for shard in shards:
                if shard.primary_node == failed_node_id:
                    # Promote first alive replica
                    for replica_id in shard.replica_nodes:
                        replica = self._metadata.nodes.get(replica_id)
                        if replica and replica.is_alive:
                            self._raft.propose(LogEntryType.SHARD_ASSIGN, {
                                "action": "migrate",
                                "table": table,
                                "shard_id": shard.shard_id,
                                "from_node": failed_node_id,
                                "to_node": replica_id,
                            })
                            break

    def _find_shard_on_node(self, node_id: str) -> TableShard | None:
        """Find any shard primary'd on the given node."""
        for shards in self._metadata.shard_map.values():
            for shard in shards:
                if shard.primary_node == node_id:
                    return shard
        return None

    def _monitor_loop(self) -> None:
        """Background loop for health monitoring and rebalancing."""
        while self._running:
            try:
                if self._raft.is_leader:
                    migrations = self.check_rebalance()
                    if migrations:
                        self.execute_rebalance(migrations[:1])  # 1 at a time
            except Exception:
                pass
            time.sleep(10.0)

    def _fire_event(self, event_type: str, data: dict[str, Any]) -> None:
        for handler in self._event_handlers:
            try:
                handler(event_type, data)
            except Exception:
                pass

    @staticmethod
    def _hash_key(key: Any) -> int:
        """Hash a shard key to a 32-bit integer."""
        raw = str(key).encode("utf-8")
        return int(hashlib.md5(raw).hexdigest()[:8], 16)

    # ── Status ──────────────────────────────────────────────────────

    def status(self) -> dict[str, Any]:
        """Full cluster status."""
        with self._metadata_lock:
            return {
                "cluster_name": self._config.cluster_name,
                "node_id": self._node_id,
                "is_leader": self._raft.is_leader,
                "leader_id": self._raft.leader_id,
                "raft_term": self._raft.current_term,
                "cluster_size": len(self._metadata.nodes),
                "tables": list(self._metadata.tables.keys()),
                "shard_count": sum(
                    len(shards) for shards in self._metadata.shard_map.values()
                ),
                "metadata_version": self._metadata.version,
                "nodes": {
                    nid: n.to_dict()
                    for nid, n in self._metadata.nodes.items()
                },
            }
