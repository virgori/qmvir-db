"""QM Distributed — Cluster coordination, sharding, replication, and distributed query execution.

Architecture:
    ┌──────────────────────────────────────────────────────────────┐
    │                    ClusterCoordinator                        │
    │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │
    │  │ Gossip   │ │ Consensus│ │ Shard    │ │ Replication   │  │
    │  │ Protocol │ │ (Raft)   │ │ Manager  │ │ Manager       │  │
    │  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │
    │  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │
    │  │ DistQuery│ │ Dist TXN │ │ CDC      │ │ ClusterClient │  │
    │  │ Engine   │ │ (2PC)    │ │ Pipeline │ │               │  │
    │  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │
    └──────────────────────────────────────────────────────────────┘
"""

from qm_core.distributed.gossip import GossipProtocol, NodeState
from qm_core.distributed.consensus import RaftNode, RaftState
from qm_core.distributed.cluster import ClusterCoordinator, ClusterConfig, ClusterNode
from qm_core.distributed.shard import ShardManager, ShardStrategy, ShardConfig, ConsistentHashRing
from qm_core.distributed.replication import ReplicationManager, ReplicationMode, ReplicaState
from qm_core.distributed.dist_query import DistributedQueryEngine, ScatterGatherPlan
from qm_core.distributed.dist_txn import TwoPhaseCommit, DistributedTransaction, SagaOrchestrator
from qm_core.distributed.cdc import CDCPipeline, CDCSubscription, CDCEvent
from qm_core.distributed.client import ClusterClient

__all__ = [
    "GossipProtocol", "NodeState",
    "RaftNode", "RaftState",
    "ClusterCoordinator", "ClusterConfig", "ClusterNode",
    "ShardManager", "ShardStrategy", "ShardConfig", "ConsistentHashRing",
    "ReplicationManager", "ReplicationMode", "ReplicaState",
    "DistributedQueryEngine", "ScatterGatherPlan",
    "TwoPhaseCommit", "DistributedTransaction", "SagaOrchestrator",
    "CDCPipeline", "CDCSubscription", "CDCEvent",
    "ClusterClient",
]
