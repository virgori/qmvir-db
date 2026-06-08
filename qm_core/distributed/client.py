"""QM Distributed — Cluster-Aware Client.

Provides:
    - Topology-aware connection to QM cluster
    - Automatic shard routing (reads and writes)
    - Read preference support (primary, secondary, nearest)
    - Connection pooling per node
    - Automatic retry with exponential backoff
    - Health-based node selection

Architecture:
    ClusterClient
        ├── TopologyMonitor (background thread discovers cluster state)
        ├── ConnectionPool (per-node connection management)
        └── ShardRouter (routes queries to correct node/shard)

    Client API mirrors QMEngine but transparently handles distribution.
"""

from __future__ import annotations

import random
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class ReadPreference(IntEnum):
    """Where to route read queries."""
    PRIMARY = 0            # Always read from primary
    PRIMARY_PREFERRED = 1  # Prefer primary, fall back to secondary
    SECONDARY = 2          # Always read from secondary
    SECONDARY_PREFERRED = 3  # Prefer secondary, fall back to primary
    NEAREST = 4            # Lowest latency node


class NodeHealth(IntEnum):
    UNKNOWN = 0
    HEALTHY = 1
    DEGRADED = 2
    UNHEALTHY = 3
    DOWN = 4


@dataclass
class ClusterNodeInfo:
    """Client-side view of a cluster node."""
    node_id: str
    host: str
    port: int
    is_primary: bool = False
    shards: list[int] = field(default_factory=list)
    health: NodeHealth = NodeHealth.UNKNOWN
    latency_ms: float = 0.0
    last_check: float = 0.0
    error_count: int = 0
    success_count: int = 0

    @property
    def is_available(self) -> bool:
        return self.health in (NodeHealth.HEALTHY, NodeHealth.DEGRADED)

    @property
    def error_rate(self) -> float:
        total = self.error_count + self.success_count
        return self.error_count / total if total > 0 else 0.0


@dataclass
class ClientConfig:
    """Client configuration."""
    seed_nodes: list[str] = field(default_factory=list)  # ["host:port", ...]
    read_preference: ReadPreference = ReadPreference.PRIMARY_PREFERRED
    connect_timeout_s: float = 5.0
    request_timeout_s: float = 30.0
    max_retries: int = 3
    retry_backoff_base_s: float = 0.1
    topology_refresh_interval_s: float = 10.0
    max_connections_per_node: int = 10
    health_check_interval_s: float = 5.0


@dataclass
class ClientStats:
    """Client-side statistics."""
    total_requests: int = 0
    total_errors: int = 0
    total_retries: int = 0
    reads: int = 0
    writes: int = 0
    avg_latency_ms: float = 0.0
    p99_latency_ms: float = 0.0
    _latencies: list[float] = field(default_factory=list)

    def record_request(self, latency_ms: float, is_write: bool, success: bool) -> None:
        self.total_requests += 1
        if is_write:
            self.writes += 1
        else:
            self.reads += 1
        if not success:
            self.total_errors += 1
        self._latencies.append(latency_ms)
        if len(self._latencies) > 10000:
            self._latencies = self._latencies[-5000:]
        if self._latencies:
            self.avg_latency_ms = sum(self._latencies) / len(self._latencies)
            sorted_lat = sorted(self._latencies)
            idx_99 = int(len(sorted_lat) * 0.99)
            self.p99_latency_ms = sorted_lat[min(idx_99, len(sorted_lat) - 1)]

    def to_dict(self) -> dict[str, Any]:
        return {
            "total_requests": self.total_requests,
            "total_errors": self.total_errors,
            "total_retries": self.total_retries,
            "reads": self.reads,
            "writes": self.writes,
            "avg_latency_ms": round(self.avg_latency_ms, 2),
            "p99_latency_ms": round(self.p99_latency_ms, 2),
            "error_rate": round(
                self.total_errors / max(1, self.total_requests) * 100, 2
            ),
        }


# Transport: (node_address, operation, payload) → response
ClientTransport = Callable[[str, str, dict[str, Any]], dict[str, Any]]


class TopologyMonitor:
    """Monitors cluster topology and node health.

    Periodically refreshes the cluster state and checks node health.
    """

    def __init__(
        self,
        config: ClientConfig,
        transport: ClientTransport | None = None,
    ) -> None:
        self._config = config
        self._transport = transport
        self._nodes: dict[str, ClusterNodeInfo] = {}
        self._shard_map: dict[int, list[str]] = {}  # shard_id → [node_ids]
        self._primary_map: dict[int, str] = {}       # shard_id → primary_node_id
        self._lock = threading.Lock()
        self._running = False
        self._thread: threading.Thread | None = None
        self._initialized = threading.Event()

    def start(self) -> None:
        """Start topology monitoring."""
        self._running = True
        self._refresh_topology()  # Initial refresh
        self._initialized.set()
        self._thread = threading.Thread(
            target=self._monitor_loop, daemon=True,
            name="topology-monitor",
        )
        self._thread.start()

    def stop(self) -> None:
        self._running = False
        if self._thread:
            self._thread.join(timeout=2.0)

    def wait_ready(self, timeout: float = 10.0) -> bool:
        """Wait until initial topology is discovered."""
        return self._initialized.wait(timeout=timeout)

    def get_node(self, node_id: str) -> ClusterNodeInfo | None:
        return self._nodes.get(node_id)

    def get_all_nodes(self) -> list[ClusterNodeInfo]:
        return list(self._nodes.values())

    def get_primary_for_shard(self, shard_id: int) -> ClusterNodeInfo | None:
        node_id = self._primary_map.get(shard_id)
        if node_id:
            return self._nodes.get(node_id)
        return None

    def get_replicas_for_shard(self, shard_id: int) -> list[ClusterNodeInfo]:
        node_ids = self._shard_map.get(shard_id, [])
        primary = self._primary_map.get(shard_id)
        return [
            self._nodes[nid] for nid in node_ids
            if nid != primary and nid in self._nodes
        ]

    def get_nodes_for_shard(self, shard_id: int) -> list[ClusterNodeInfo]:
        node_ids = self._shard_map.get(shard_id, [])
        return [self._nodes[nid] for nid in node_ids if nid in self._nodes]

    def add_node(self, node: ClusterNodeInfo) -> None:
        """Manually add a node (used by configure/seed)."""
        with self._lock:
            self._nodes[node.node_id] = node
            for shard_id in node.shards:
                if shard_id not in self._shard_map:
                    self._shard_map[shard_id] = []
                if node.node_id not in self._shard_map[shard_id]:
                    self._shard_map[shard_id].append(node.node_id)
                if node.is_primary:
                    self._primary_map[shard_id] = node.node_id

    def _monitor_loop(self) -> None:
        while self._running:
            try:
                self._refresh_topology()
                self._health_check()
            except Exception:
                pass
            time.sleep(self._config.topology_refresh_interval_s)

    def _refresh_topology(self) -> None:
        """Discover or refresh cluster topology."""
        if not self._transport:
            return

        for seed in self._config.seed_nodes:
            try:
                resp = self._transport(seed, "topology", {})
                if resp and "nodes" in resp:
                    with self._lock:
                        for node_data in resp["nodes"]:
                            nid = node_data["node_id"]
                            existing = self._nodes.get(nid)
                            node = ClusterNodeInfo(
                                node_id=nid,
                                host=node_data.get("host", ""),
                                port=node_data.get("port", 0),
                                is_primary=node_data.get("is_primary", False),
                                shards=node_data.get("shards", []),
                            )
                            if existing:
                                node.health = existing.health
                                node.latency_ms = existing.latency_ms
                            self._nodes[nid] = node

                            for sid in node.shards:
                                if sid not in self._shard_map:
                                    self._shard_map[sid] = []
                                if nid not in self._shard_map[sid]:
                                    self._shard_map[sid].append(nid)
                                if node.is_primary:
                                    self._primary_map[sid] = nid
                    break  # Got topology from one seed
            except Exception:
                continue

    def _health_check(self) -> None:
        """Ping all known nodes."""
        for node in list(self._nodes.values()):
            start = time.monotonic()
            try:
                if self._transport:
                    addr = f"{node.host}:{node.port}"
                    resp = self._transport(addr, "ping", {})
                    if resp and resp.get("ok"):
                        node.health = NodeHealth.HEALTHY
                        node.latency_ms = (time.monotonic() - start) * 1000
                    else:
                        node.health = NodeHealth.DEGRADED
                else:
                    node.health = NodeHealth.HEALTHY
            except Exception:
                node.health = NodeHealth.UNHEALTHY
                node.error_count += 1
            node.last_check = time.time()


class ClusterClient:
    """Cluster-aware client for QM distributed database.

    Provides a high-level API that transparently handles shard routing,
    read preferences, retries, and failover.

    Usage:
        client = ClusterClient(ClientConfig(
            seed_nodes=["10.0.0.1:9000", "10.0.0.2:9000"],
            read_preference=ReadPreference.PRIMARY_PREFERRED,
        ))
        client.start()

        # Reads — routed based on read preference
        results = client.find("users", {"age": {"$gte": 18}}, limit=100)

        # Writes — always routed to primary
        client.insert("users", {"name": "Alice", "age": 30})
        client.update("users", {"name": "Alice"}, {"$set": {"age": 31}})

        client.stop()
    """

    def __init__(
        self,
        config: ClientConfig | None = None,
        transport: ClientTransport | None = None,
        shard_key_fn: Callable[[str, dict[str, Any]], int] | None = None,
    ) -> None:
        self._config = config or ClientConfig()
        self._transport = transport
        self._shard_key_fn = shard_key_fn

        self._topology = TopologyMonitor(self._config, transport)
        self._stats = ClientStats()
        self._lock = threading.Lock()
        self._started = False

    def start(self) -> None:
        """Start the client and discover topology."""
        self._topology.start()
        self._started = True

    def stop(self) -> None:
        """Stop the client."""
        self._topology.stop()
        self._started = False

    # ── Read Operations ─────────────────────────────

    def find(
        self,
        table: str,
        filter: dict[str, Any] | None = None,
        projection: list[str] | None = None,
        sort: list[tuple[str, int]] | None = None,
        limit: int | None = None,
        offset: int | None = None,
    ) -> list[dict[str, Any]]:
        """Find documents, automatically routing to correct shard(s)."""
        shard_id = self._resolve_shard(table, filter)
        node = self._select_read_node(shard_id)

        return self._execute_with_retry("find", node, {
            "table": table,
            "filter": filter,
            "projection": projection,
            "sort": sort,
            "limit": limit,
            "offset": offset,
        })

    def find_one(
        self,
        table: str,
        filter: dict[str, Any] | None = None,
    ) -> dict[str, Any] | None:
        """Find a single document."""
        results = self.find(table, filter, limit=1)
        return results[0] if results else None

    def count(
        self,
        table: str,
        filter: dict[str, Any] | None = None,
    ) -> int:
        """Count documents."""
        shard_id = self._resolve_shard(table, filter)
        node = self._select_read_node(shard_id)

        result = self._execute_with_retry("count", node, {
            "table": table,
            "filter": filter,
        })
        return result if isinstance(result, int) else 0

    def aggregate(
        self,
        table: str,
        pipeline: list[dict[str, Any]],
    ) -> dict[str, Any]:
        """Execute an aggregation pipeline."""
        node = self._select_read_node(None)  # Scatter to all shards via coordinator
        return self._execute_with_retry("aggregate", node, {
            "table": table,
            "pipeline": pipeline,
        })

    def search(
        self,
        table: str,
        query: str,
        limit: int = 10,
    ) -> list[dict[str, Any]]:
        """Full-text search."""
        node = self._select_read_node(None)
        return self._execute_with_retry("search", node, {
            "table": table,
            "query": query,
            "limit": limit,
        })

    # ── Write Operations ────────────────────────────

    def insert(
        self,
        table: str,
        document: dict[str, Any],
    ) -> str:
        """Insert a document. Returns the document key."""
        shard_id = self._resolve_shard(table, document)
        node = self._select_write_node(shard_id)

        return self._execute_with_retry("insert", node, {
            "table": table,
            "document": document,
        }, is_write=True)

    def insert_many(
        self,
        table: str,
        documents: list[dict[str, Any]],
    ) -> list[str]:
        """Insert multiple documents."""
        # Group by shard
        shard_groups: dict[int, list[dict[str, Any]]] = {}
        for doc in documents:
            sid = self._resolve_shard(table, doc)
            if sid is None:
                sid = 0
            if sid not in shard_groups:
                shard_groups[sid] = []
            shard_groups[sid].append(doc)

        results: list[str] = []
        for sid, docs in shard_groups.items():
            node = self._select_write_node(sid)
            batch_result = self._execute_with_retry("insert_many", node, {
                "table": table,
                "documents": docs,
            }, is_write=True)
            if isinstance(batch_result, list):
                results.extend(batch_result)

        return results

    def update(
        self,
        table: str,
        filter: dict[str, Any],
        update: dict[str, Any],
    ) -> int:
        """Update documents. Returns count of updated docs."""
        shard_id = self._resolve_shard(table, filter)
        node = self._select_write_node(shard_id)

        result = self._execute_with_retry("update", node, {
            "table": table,
            "filter": filter,
            "update": update,
        }, is_write=True)
        return result if isinstance(result, int) else 0

    def delete(
        self,
        table: str,
        filter: dict[str, Any],
    ) -> int:
        """Delete documents. Returns count of deleted docs."""
        shard_id = self._resolve_shard(table, filter)
        node = self._select_write_node(shard_id)

        result = self._execute_with_retry("delete", node, {
            "table": table,
            "filter": filter,
        }, is_write=True)
        return result if isinstance(result, int) else 0

    # ── Admin Operations ────────────────────────────

    def create_table(
        self,
        table: str,
        schema: dict[str, Any] | None = None,
        shard_count: int | None = None,
    ) -> bool:
        """Create a table (DDL — goes through coordinator)."""
        node = self._get_coordinator_node()
        return self._execute_with_retry("create_table", node, {
            "table": table,
            "schema": schema,
            "shard_count": shard_count,
        }, is_write=True)

    def drop_table(self, table: str) -> bool:
        """Drop a table."""
        node = self._get_coordinator_node()
        return self._execute_with_retry("drop_table", node, {
            "table": table,
        }, is_write=True)

    def cluster_status(self) -> dict[str, Any]:
        """Get cluster status."""
        nodes = self._topology.get_all_nodes()
        return {
            "nodes": [
                {
                    "node_id": n.node_id,
                    "host": n.host,
                    "port": n.port,
                    "is_primary": n.is_primary,
                    "health": n.health.name,
                    "shards": n.shards,
                    "latency_ms": round(n.latency_ms, 2),
                }
                for n in nodes
            ],
            "stats": self._stats.to_dict(),
        }

    @property
    def stats(self) -> ClientStats:
        return self._stats

    # ── Internal ────────────────────────────────────

    def _resolve_shard(
        self,
        table: str,
        key_or_filter: dict[str, Any] | None,
    ) -> int | None:
        """Resolve which shard to target.

        Returns None if all shards should be queried (scatter).
        """
        if self._shard_key_fn and key_or_filter:
            return self._shard_key_fn(table, key_or_filter)
        return None  # Scatter to all shards

    def _select_read_node(self, shard_id: int | None) -> ClusterNodeInfo | None:
        """Select a node for reading based on read preference."""
        if shard_id is None:
            # Scatter query → send to coordinator/any available node
            nodes = [
                n for n in self._topology.get_all_nodes()
                if n.is_available
            ]
            return min(nodes, key=lambda n: n.latency_ms) if nodes else None

        pref = self._config.read_preference
        primary = self._topology.get_primary_for_shard(shard_id)
        replicas = self._topology.get_replicas_for_shard(shard_id)
        available_replicas = [r for r in replicas if r.is_available]

        if pref == ReadPreference.PRIMARY:
            return primary
        elif pref == ReadPreference.SECONDARY:
            return random.choice(available_replicas) if available_replicas else None
        elif pref == ReadPreference.PRIMARY_PREFERRED:
            if primary and primary.is_available:
                return primary
            return random.choice(available_replicas) if available_replicas else None
        elif pref == ReadPreference.SECONDARY_PREFERRED:
            if available_replicas:
                return random.choice(available_replicas)
            return primary if primary and primary.is_available else None
        elif pref == ReadPreference.NEAREST:
            all_nodes = self._topology.get_nodes_for_shard(shard_id)
            available = [n for n in all_nodes if n.is_available]
            return min(available, key=lambda n: n.latency_ms) if available else None

        return primary

    def _select_write_node(self, shard_id: int | None) -> ClusterNodeInfo | None:
        """Select a node for writing (always primary)."""
        if shard_id is not None:
            return self._topology.get_primary_for_shard(shard_id)
        # If no shard, use coordinator/any primary
        nodes = [n for n in self._topology.get_all_nodes() if n.is_primary and n.is_available]
        return nodes[0] if nodes else None

    def _get_coordinator_node(self) -> ClusterNodeInfo | None:
        """Get the coordinator node (leader)."""
        for n in self._topology.get_all_nodes():
            if n.is_primary and n.is_available:
                return n
        return None

    def _execute_with_retry(
        self,
        operation: str,
        node: ClusterNodeInfo | None,
        payload: dict[str, Any],
        is_write: bool = False,
    ) -> Any:
        """Execute an operation with retry logic."""
        last_error: Exception | None = None

        for attempt in range(self._config.max_retries + 1):
            if node is None:
                # Try any available node
                nodes = [n for n in self._topology.get_all_nodes() if n.is_available]
                if not nodes:
                    last_error = ConnectionError("No available nodes")
                    continue
                node = nodes[attempt % len(nodes)]

            start = time.monotonic()
            try:
                result = self._send_request(node, operation, payload)
                latency = (time.monotonic() - start) * 1000
                node.success_count += 1
                self._stats.record_request(latency, is_write, True)
                return result
            except Exception as e:
                latency = (time.monotonic() - start) * 1000
                node.error_count += 1
                self._stats.record_request(latency, is_write, False)
                self._stats.total_retries += 1
                last_error = e

                # Exponential backoff
                if attempt < self._config.max_retries:
                    backoff = self._config.retry_backoff_base_s * (2 ** attempt)
                    time.sleep(backoff)
                    # Try a different node on retry
                    node = None

        raise last_error or RuntimeError("All retries exhausted")

    def _send_request(
        self,
        node: ClusterNodeInfo,
        operation: str,
        payload: dict[str, Any],
    ) -> Any:
        """Send a request to a node."""
        if self._transport:
            address = f"{node.host}:{node.port}"
            response = self._transport(address, operation, payload)
            if response is None:
                raise ConnectionError(f"No response from {address}")
            if "error" in response:
                raise RuntimeError(response["error"])
            return response.get("result")
        else:
            # No transport = mock mode, return empty results
            return [] if operation in ("find", "search") else None
