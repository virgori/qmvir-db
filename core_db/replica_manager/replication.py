"""QM Core DB — Replica Manager.

Manages read replicas and logical replication:
  - Primary → replica streaming
  - LSN tracking per replica
  - Read routing to replicas
  - Lag monitoring
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum


class ReplicaRole(Enum):
    PRIMARY = "primary"
    REPLICA = "replica"
    STANDBY = "standby"


class ReplicaStatus(Enum):
    ACTIVE = "active"
    SYNCING = "syncing"
    LAGGING = "lagging"
    OFFLINE = "offline"


@dataclass
class ReplicaNode:
    """A replica node in the cluster."""

    node_id: str
    role: ReplicaRole
    host: str
    port: int
    status: ReplicaStatus = ReplicaStatus.ACTIVE
    last_applied_lsn: int = 0
    lag_bytes: int = 0
    last_heartbeat: float = field(default_factory=time.time)

    @property
    def is_healthy(self) -> bool:
        return (
            self.status == ReplicaStatus.ACTIVE
            and (time.time() - self.last_heartbeat) < 30.0
        )


class ReplicaManager:
    """Manages replica topology and read routing."""

    def __init__(self) -> None:
        self._nodes: dict[str, ReplicaNode] = {}
        self._primary_id: str | None = None

    def add_node(self, node: ReplicaNode) -> None:
        self._nodes[node.node_id] = node
        if node.role == ReplicaRole.PRIMARY:
            self._primary_id = node.node_id

    def get_primary(self) -> ReplicaNode | None:
        if self._primary_id:
            return self._nodes.get(self._primary_id)
        return None

    def get_read_replica(self) -> ReplicaNode | None:
        """Get the healthiest read replica with least lag."""
        replicas = [
            n for n in self._nodes.values()
            if n.role == ReplicaRole.REPLICA and n.is_healthy
        ]
        if not replicas:
            return self.get_primary()  # fallback to primary
        return min(replicas, key=lambda n: n.lag_bytes)

    def update_lsn(self, node_id: str, lsn: int) -> None:
        """Update applied LSN for a replica."""
        node = self._nodes.get(node_id)
        if node:
            node.last_applied_lsn = lsn
            node.last_heartbeat = time.time()
            primary = self.get_primary()
            if primary:
                node.lag_bytes = max(0, primary.last_applied_lsn - lsn)
                node.status = (
                    ReplicaStatus.ACTIVE if node.lag_bytes < 1000
                    else ReplicaStatus.LAGGING
                )

    def list_nodes(self) -> list[ReplicaNode]:
        return list(self._nodes.values())
