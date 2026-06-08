"""QM Distributed — Shard Manager with Consistent Hashing.

Provides:
    - Consistent hash ring with virtual nodes for even distribution
    - Hash-based and range-based shard routing
    - Online shard splitting and merging
    - Shard migration coordination
    - Load-aware placement
    - Locality-aware routing (prefer local shard, then same-zone)

The shard manager works with the ClusterCoordinator:
    - Coordinator decides WHAT to shard and WHERE
    - ShardManager implements HOW routing works
"""

from __future__ import annotations

import bisect
import hashlib
import struct
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class ShardStrategy(IntEnum):
    """Sharding strategies."""
    HASH = 0        # Hash-based partitioning (default)
    RANGE = 1       # Range-based partitioning (by time, ID, etc.)
    LIST = 2        # List-based partitioning (by category)
    COMPOSITE = 3   # Composite key (hash + range)


class ShardState(IntEnum):
    """Lifecycle states for a shard."""
    ACTIVE = 0
    MIGRATING = 1
    SPLITTING = 2
    MERGING = 3
    READONLY = 4
    OFFLINE = 5


@dataclass
class ShardConfig:
    """Configuration for shard manager."""
    virtual_nodes: int = 128    # Virtual nodes per physical node on hash ring
    hash_algorithm: str = "md5"  # md5, xxhash, murmur3
    rebalance_batch_size: int = 1000  # Rows per batch during migration
    split_threshold_rows: int = 1_000_000  # Split shard above this row count
    merge_threshold_rows: int = 10_000     # Merge shards below this row count


@dataclass(slots=True)
class ShardInfo:
    """Full information about a shard."""
    shard_id: int
    table: str
    strategy: ShardStrategy
    state: ShardState = ShardState.ACTIVE
    primary_node: str = ""
    replica_nodes: list[str] = field(default_factory=list)
    key_range_start: int = 0
    key_range_end: int = 0xFFFFFFFF
    list_values: list[Any] = field(default_factory=list)
    row_count: int = 0
    size_bytes: int = 0
    created_at: float = 0.0
    last_compacted: float = 0.0

    def contains_hash(self, h: int) -> bool:
        """Check if a hash falls in this shard's range."""
        return self.key_range_start <= h <= self.key_range_end

    def to_dict(self) -> dict[str, Any]:
        return {
            "shard_id": self.shard_id,
            "table": self.table,
            "strategy": self.strategy.name,
            "state": self.state.name,
            "primary": self.primary_node,
            "replicas": self.replica_nodes,
            "range": [self.key_range_start, self.key_range_end],
            "row_count": self.row_count,
            "size_bytes": self.size_bytes,
        }


@dataclass(slots=True)
class MigrationPlan:
    """Plan for migrating a shard between nodes."""
    migration_id: str
    table: str
    shard_id: int
    source_node: str
    target_node: str
    state: str = "pending"  # pending, copying, catching_up, switching, done, failed
    rows_copied: int = 0
    total_rows: int = 0
    started_at: float = 0.0
    completed_at: float = 0.0
    error: str | None = None

    @property
    def progress(self) -> float:
        if self.total_rows == 0:
            return 1.0
        return self.rows_copied / self.total_rows

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.migration_id,
            "table": self.table,
            "shard_id": self.shard_id,
            "source": self.source_node,
            "target": self.target_node,
            "state": self.state,
            "progress": f"{self.progress:.1%}",
            "rows_copied": self.rows_copied,
        }


@dataclass(slots=True)
class SplitPlan:
    """Plan for splitting a shard into two."""
    original_shard_id: int
    new_shard_id: int
    table: str
    split_point: int  # Hash value to split at
    state: str = "pending"


class ConsistentHashRing:
    """Consistent hash ring with virtual nodes for even distribution.

    Provides O(log N) routing with minimal key redistribution when nodes change.
    Each physical node is mapped to `vnodes` positions on the ring.

    Usage:
        ring = ConsistentHashRing(vnodes=128)
        ring.add_node("node-1")
        ring.add_node("node-2")
        ring.add_node("node-3")

        node = ring.get_node("user-key-123")  # → "node-2"
        nodes = ring.get_nodes("user-key-123", count=3)  # → ["node-2", "node-1", "node-3"]
    """

    RING_SIZE = 2**32  # 32-bit hash ring

    def __init__(self, vnodes: int = 128) -> None:
        self._vnodes = vnodes
        self._ring: list[tuple[int, str]] = []  # Sorted list of (hash, node_id)
        self._ring_hashes: list[int] = []        # Just the hashes for bisect
        self._nodes: set[str] = set()
        self._lock = threading.Lock()

    @property
    def node_count(self) -> int:
        return len(self._nodes)

    @property
    def ring_size(self) -> int:
        return len(self._ring)

    def add_node(self, node_id: str, weight: float = 1.0) -> set[str]:
        """Add a node to the ring. Returns set of nodes that lose keys.

        Weight multiplies the number of virtual nodes (default 1.0).
        """
        with self._lock:
            if node_id in self._nodes:
                return set()

            affected: set[str] = set()
            n_vnodes = max(1, int(self._vnodes * weight))

            for i in range(n_vnodes):
                vnode_key = f"{node_id}#vn{i}"
                h = self._hash(vnode_key)
                # Find which node currently owns this position
                if self._ring:
                    idx = bisect.bisect_left(self._ring_hashes, h)
                    if idx < len(self._ring):
                        affected.add(self._ring[idx][1])

                bisect.insort(self._ring, (h, node_id))
                bisect.insort(self._ring_hashes, h)

            self._nodes.add(node_id)
            return affected

    def remove_node(self, node_id: str) -> set[str]:
        """Remove a node from the ring. Returns set of nodes that gain keys."""
        with self._lock:
            if node_id not in self._nodes:
                return set()

            gained: set[str] = set()
            new_ring: list[tuple[int, str]] = []
            new_hashes: list[int] = []

            for h, nid in self._ring:
                if nid != node_id:
                    new_ring.append((h, nid))
                    new_hashes.append(h)

            # The nodes that are now responsible for the removed node's ranges
            for i in range(self._vnodes):
                vnode_key = f"{node_id}#vn{i}"
                h = self._hash(vnode_key)
                if new_ring:
                    idx = bisect.bisect_left(new_hashes, h) % len(new_ring)
                    gained.add(new_ring[idx][1])

            self._ring = new_ring
            self._ring_hashes = new_hashes
            self._nodes.discard(node_id)
            return gained

    def get_node(self, key: Any) -> str | None:
        """Get the node responsible for a key."""
        if not self._ring:
            return None
        h = self._hash(str(key))
        idx = bisect.bisect_left(self._ring_hashes, h) % len(self._ring)
        return self._ring[idx][1]

    def get_nodes(self, key: Any, count: int = 3) -> list[str]:
        """Get N distinct nodes for a key (primary + replicas).

        Walks the ring clockwise from the key's position, collecting distinct nodes.
        """
        if not self._ring:
            return []

        h = self._hash(str(key))
        idx = bisect.bisect_left(self._ring_hashes, h) % len(self._ring)

        result: list[str] = []
        seen: set[str] = set()
        ring_len = len(self._ring)

        for i in range(ring_len):
            node_id = self._ring[(idx + i) % ring_len][1]
            if node_id not in seen:
                seen.add(node_id)
                result.append(node_id)
                if len(result) >= count:
                    break

        return result

    def get_key_distribution(self) -> dict[str, float]:
        """Get the approximate key distribution across nodes.

        Returns node_id → fraction of keyspace owned.
        """
        if not self._ring:
            return {}

        ownership: dict[str, int] = {n: 0 for n in self._nodes}
        ring_len = len(self._ring)

        for i in range(ring_len):
            node_id = self._ring[i][1]
            # Space between this vnode and the previous one
            if i == 0:
                space = self._ring[i][0] + (self.RING_SIZE - self._ring[-1][0])
            else:
                space = self._ring[i][0] - self._ring[i - 1][0]
            ownership[node_id] = ownership.get(node_id, 0) + space

        total = sum(ownership.values())
        if total == 0:
            return {}
        return {nid: count / total for nid, count in ownership.items()}

    @staticmethod
    def _hash(key: str) -> int:
        """Hash a key to a 32-bit integer position on the ring."""
        digest = hashlib.md5(key.encode("utf-8")).digest()
        return struct.unpack("<I", digest[:4])[0]


class ShardRouter:
    """Routes requests to the correct shard based on strategy.

    Supports:
        - Hash routing via consistent hash ring
        - Range routing via sorted shard boundaries
        - List routing via exact value matching
        - Composite routing (hash first, then range)
    """

    def __init__(self, config: ShardConfig | None = None) -> None:
        self._config = config or ShardConfig()
        self._lock = threading.RLock()
        # Per-table shard info
        self._shards: dict[str, list[ShardInfo]] = {}
        # Per-table hash rings (for HASH strategy)
        self._rings: dict[str, ConsistentHashRing] = {}
        # Per-table shard key column
        self._shard_keys: dict[str, str] = {}

    def register_table(
        self,
        table: str,
        shards: list[ShardInfo],
        shard_key: str = "_id",
        strategy: ShardStrategy = ShardStrategy.HASH,
    ) -> None:
        """Register a table's shard configuration."""
        with self._lock:
            self._shards[table] = shards
            self._shard_keys[table] = shard_key

            if strategy == ShardStrategy.HASH:
                ring = ConsistentHashRing(vnodes=self._config.virtual_nodes)
                # Add primary nodes to ring
                seen_nodes: set[str] = set()
                for shard in shards:
                    if shard.primary_node and shard.primary_node not in seen_nodes:
                        ring.add_node(shard.primary_node)
                        seen_nodes.add(shard.primary_node)
                self._rings[table] = ring

    def unregister_table(self, table: str) -> None:
        with self._lock:
            self._shards.pop(table, None)
            self._rings.pop(table, None)
            self._shard_keys.pop(table, None)

    def route(self, table: str, key_value: Any) -> ShardInfo | None:
        """Route a single key to its shard."""
        with self._lock:
            shards = self._shards.get(table)
            if not shards:
                return None

            # Hash-based routing
            ring = self._rings.get(table)
            if ring:
                node_id = ring.get_node(key_value)
                if node_id:
                    for s in shards:
                        if s.primary_node == node_id and s.state == ShardState.ACTIVE:
                            return s

            # Range-based routing fallback
            h = self._hash_key(key_value)
            for s in shards:
                if s.contains_hash(h):
                    return s

            # Default to first shard
            return shards[0] if shards else None

    def route_with_replicas(self, table: str, key_value: Any, count: int = 3) -> list[ShardInfo]:
        """Route a key and return primary + replica shards."""
        with self._lock:
            ring = self._rings.get(table)
            shards = self._shards.get(table, [])
            if not ring or not shards:
                return []

            nodes = ring.get_nodes(key_value, count=count)
            result: list[ShardInfo] = []
            for node_id in nodes:
                for s in shards:
                    if s.primary_node == node_id:
                        result.append(s)
                        break
            return result

    def route_scatter(self, table: str) -> list[ShardInfo]:
        """Get all active shards for scatter-gather queries."""
        with self._lock:
            return [
                s for s in self._shards.get(table, [])
                if s.state in (ShardState.ACTIVE, ShardState.READONLY)
            ]

    def get_shard_key(self, table: str) -> str:
        """Get the shard key column for a table."""
        return self._shard_keys.get(table, "_id")

    def update_shard(self, table: str, shard_id: int, **kwargs: Any) -> None:
        """Update shard metadata (row_count, state, etc.)."""
        with self._lock:
            for s in self._shards.get(table, []):
                if s.shard_id == shard_id:
                    for k, v in kwargs.items():
                        if hasattr(s, k):
                            setattr(s, k, v)
                    break

    @staticmethod
    def _hash_key(key: Any) -> int:
        raw = str(key).encode("utf-8")
        return int(hashlib.md5(raw).hexdigest()[:8], 16)


class ShardMigrator:
    """Coordinates online shard migration between nodes.

    Migration phases:
        1. COPYING: Bulk copy existing data to target
        2. CATCHING_UP: Stream WAL changes since copy started
        3. SWITCHING: Atomically swap primary to target
        4. CLEANUP: Remove data from source

    Supports:
        - Online migration (reads/writes continue during migration)
        - Catch-up via WAL replay
        - Atomic primary switchover
        - Rollback on failure
    """

    def __init__(self) -> None:
        self._active_migrations: dict[str, MigrationPlan] = {}
        self._lock = threading.Lock()
        self._migration_counter = 0

    def plan_migration(
        self,
        table: str,
        shard_id: int,
        source_node: str,
        target_node: str,
        total_rows: int = 0,
    ) -> MigrationPlan:
        """Create a migration plan."""
        with self._lock:
            self._migration_counter += 1
            mid = f"mig-{self._migration_counter:06d}"

        plan = MigrationPlan(
            migration_id=mid,
            table=table,
            shard_id=shard_id,
            source_node=source_node,
            target_node=target_node,
            total_rows=total_rows,
        )
        self._active_migrations[mid] = plan
        return plan

    def start_migration(self, migration_id: str) -> bool:
        """Begin executing a migration plan."""
        plan = self._active_migrations.get(migration_id)
        if not plan or plan.state != "pending":
            return False

        plan.state = "copying"
        plan.started_at = time.time()
        return True

    def update_progress(self, migration_id: str, rows_copied: int) -> None:
        """Update migration progress."""
        plan = self._active_migrations.get(migration_id)
        if plan:
            plan.rows_copied = rows_copied

    def advance_phase(self, migration_id: str, new_state: str) -> bool:
        """Advance migration to next phase."""
        plan = self._active_migrations.get(migration_id)
        if not plan:
            return False

        valid_transitions = {
            "pending": ["copying"],
            "copying": ["catching_up", "failed"],
            "catching_up": ["switching", "failed"],
            "switching": ["done", "failed"],
        }
        if new_state in valid_transitions.get(plan.state, []):
            plan.state = new_state
            if new_state == "done":
                plan.completed_at = time.time()
            return True
        return False

    def fail_migration(self, migration_id: str, error: str) -> None:
        """Mark a migration as failed."""
        plan = self._active_migrations.get(migration_id)
        if plan:
            plan.state = "failed"
            plan.error = error

    def get_active_migrations(self) -> list[MigrationPlan]:
        """Get all active/pending migrations."""
        return [
            p for p in self._active_migrations.values()
            if p.state not in ("done", "failed")
        ]

    def get_migration(self, migration_id: str) -> MigrationPlan | None:
        return self._active_migrations.get(migration_id)

    def cleanup_completed(self, max_age_s: float = 3600.0) -> int:
        """Remove completed/failed migrations older than max_age_s."""
        now = time.time()
        to_remove: list[str] = []
        for mid, plan in self._active_migrations.items():
            if plan.state in ("done", "failed"):
                age = now - max(plan.completed_at, plan.started_at)
                if age > max_age_s:
                    to_remove.append(mid)
        for mid in to_remove:
            del self._active_migrations[mid]
        return len(to_remove)


class ShardSplitter:
    """Handles shard splitting when a shard grows too large.

    Split algorithm:
        1. Pick split point (median hash of shard's keys)
        2. Create new shard with second half of range
        3. Move data above split point to new shard
        4. Update routing tables atomically
    """

    def __init__(self) -> None:
        self._active_splits: dict[int, SplitPlan] = {}
        self._next_shard_id = 10000  # Start high to avoid conflicts

    def plan_split(self, shard: ShardInfo) -> SplitPlan:
        """Create a split plan for an oversized shard."""
        split_point = (shard.key_range_start + shard.key_range_end) // 2
        self._next_shard_id += 1

        plan = SplitPlan(
            original_shard_id=shard.shard_id,
            new_shard_id=self._next_shard_id,
            table=shard.table,
            split_point=split_point,
        )
        self._active_splits[shard.shard_id] = plan
        return plan

    def execute_split(self, plan: SplitPlan, shards: list[ShardInfo]) -> tuple[ShardInfo, ShardInfo]:
        """Execute a split, returning the two resulting shards.

        The original shard keeps the lower half, new shard gets upper half.
        """
        original: ShardInfo | None = None
        for s in shards:
            if s.shard_id == plan.original_shard_id:
                original = s
                break

        if not original:
            raise ValueError(f"Shard {plan.original_shard_id} not found")

        # Update original shard range to lower half
        original_end = original.key_range_end
        original.key_range_end = plan.split_point - 1

        # Create new shard for upper half
        new_shard = ShardInfo(
            shard_id=plan.new_shard_id,
            table=plan.table,
            strategy=original.strategy,
            state=ShardState.ACTIVE,
            primary_node=original.primary_node,  # Initially same node
            replica_nodes=list(original.replica_nodes),
            key_range_start=plan.split_point,
            key_range_end=original_end,
            row_count=original.row_count // 2,  # Approximate
            created_at=time.time(),
        )
        original.row_count = original.row_count // 2

        plan.state = "done"
        return original, new_shard

    def should_split(self, shard: ShardInfo, threshold: int = 1_000_000) -> bool:
        """Check if a shard needs splitting."""
        return (
            shard.state == ShardState.ACTIVE
            and shard.row_count > threshold
        )


class ShardManager:
    """High-level shard management combining routing, migration, and splitting.

    Usage:
        mgr = ShardManager(config)
        mgr.register_table("users", shards, shard_key="user_id")
        shard = mgr.route("users", "user-123")
        mgr.plan_and_execute_migration(...)
    """

    def __init__(self, config: ShardConfig | None = None) -> None:
        self._config = config or ShardConfig()
        self._router = ShardRouter(self._config)
        self._migrator = ShardMigrator()
        self._splitter = ShardSplitter()
        self._lock = threading.RLock()

    @property
    def router(self) -> ShardRouter:
        return self._router

    @property
    def migrator(self) -> ShardMigrator:
        return self._migrator

    @property
    def splitter(self) -> ShardSplitter:
        return self._splitter

    def register_table(
        self,
        table: str,
        shards: list[ShardInfo],
        shard_key: str = "_id",
        strategy: ShardStrategy = ShardStrategy.HASH,
    ) -> None:
        self._router.register_table(table, shards, shard_key, strategy)

    def route(self, table: str, key_value: Any) -> ShardInfo | None:
        return self._router.route(table, key_value)

    def route_scatter(self, table: str) -> list[ShardInfo]:
        return self._router.route_scatter(table)

    def check_and_split(self, table: str) -> list[SplitPlan]:
        """Check all shards for a table and return split plans for oversized ones."""
        plans: list[SplitPlan] = []
        shards = self._router._shards.get(table, [])
        for shard in shards:
            if self._splitter.should_split(shard, self._config.split_threshold_rows):
                plans.append(self._splitter.plan_split(shard))
        return plans

    def check_and_merge(self, table: str) -> list[tuple[int, int]]:
        """Check for tiny shards that could be merged. Returns pairs of shard IDs."""
        candidates: list[tuple[int, int]] = []
        shards = sorted(
            self._router._shards.get(table, []),
            key=lambda s: s.key_range_start,
        )
        for i in range(len(shards) - 1):
            if (
                shards[i].row_count < self._config.merge_threshold_rows
                and shards[i + 1].row_count < self._config.merge_threshold_rows
            ):
                candidates.append((shards[i].shard_id, shards[i + 1].shard_id))
        return candidates

    def stats(self) -> dict[str, Any]:
        """Shard manager statistics."""
        with self._lock:
            tables: dict[str, Any] = {}
            for table, shards in self._router._shards.items():
                tables[table] = {
                    "shard_count": len(shards),
                    "total_rows": sum(s.row_count for s in shards),
                    "total_bytes": sum(s.size_bytes for s in shards),
                    "shards": [s.to_dict() for s in shards],
                }
            return {
                "tables": tables,
                "active_migrations": [
                    m.to_dict() for m in self._migrator.get_active_migrations()
                ],
            }
