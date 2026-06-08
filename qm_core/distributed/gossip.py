"""QM Distributed — Gossip Protocol for Cluster Membership.

SWIM-based (Scalable Weakly-consistent Infection-style Membership) protocol:
    - Periodic ping/ack heartbeats between nodes
    - Indirect probing when direct ping fails
    - Suspicion mechanism before declaring node dead
    - Piggyback membership updates on gossip messages
    - Infection-style dissemination of state changes

Membership states: ALIVE → SUSPECT → DEAD → LEFT

Protocol:
    1. Each node periodically picks a random peer and sends PING
    2. If no ACK within timeout → pick k random peers, send PING_REQ(target)
    3. If still no ACK → mark target as SUSPECT
    4. After suspicion_timeout → mark as DEAD
    5. Membership changes piggyback on all PING/ACK messages
"""

from __future__ import annotations

import hashlib
import random
import struct
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class NodeState(IntEnum):
    """Node lifecycle states in the gossip protocol."""
    ALIVE = 0
    SUSPECT = 1
    DEAD = 2
    LEFT = 3


class GossipMessageType(IntEnum):
    """Wire message types for gossip protocol."""
    PING = 1
    ACK = 2
    PING_REQ = 3       # Indirect probe request
    PING_REQ_ACK = 4   # Response to indirect probe
    JOIN = 10
    LEAVE = 11
    STATE_SYNC = 20     # Full state synchronization


@dataclass(slots=True)
class MemberEntry:
    """Membership entry for a single node."""
    node_id: str
    host: str
    port: int
    state: NodeState = NodeState.ALIVE
    incarnation: int = 0       # Monotonic counter to refute suspicion
    last_heartbeat: float = 0.0
    suspect_since: float = 0.0
    metadata: dict[str, Any] = field(default_factory=dict)  # Roles, shard assignments, etc.

    @property
    def address(self) -> str:
        return f"{self.host}:{self.port}"

    @property
    def is_alive(self) -> bool:
        return self.state == NodeState.ALIVE

    @property
    def is_reachable(self) -> bool:
        return self.state in (NodeState.ALIVE, NodeState.SUSPECT)

    def to_dict(self) -> dict[str, Any]:
        return {
            "node_id": self.node_id,
            "host": self.host,
            "port": self.port,
            "state": self.state.name,
            "incarnation": self.incarnation,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> MemberEntry:
        return cls(
            node_id=d["node_id"],
            host=d["host"],
            port=d["port"],
            state=NodeState[d["state"]],
            incarnation=d.get("incarnation", 0),
            metadata=d.get("metadata", {}),
        )


@dataclass(slots=True)
class GossipMessage:
    """Wire message for gossip protocol."""
    msg_type: GossipMessageType
    sender_id: str
    target_id: str = ""
    payload: dict[str, Any] = field(default_factory=dict)
    membership_updates: list[dict[str, Any]] = field(default_factory=list)
    sequence: int = 0

    def serialize(self) -> bytes:
        """Serialize to compact binary format."""
        try:
            import orjson
            return orjson.dumps({
                "t": int(self.msg_type),
                "s": self.sender_id,
                "d": self.target_id,
                "p": self.payload,
                "m": self.membership_updates,
                "q": self.sequence,
            })
        except ImportError:
            import json
            return json.dumps({
                "t": int(self.msg_type),
                "s": self.sender_id,
                "d": self.target_id,
                "p": self.payload,
                "m": self.membership_updates,
                "q": self.sequence,
            }).encode()

    @classmethod
    def deserialize(cls, data: bytes) -> GossipMessage:
        try:
            import orjson
            d = orjson.loads(data)
        except ImportError:
            import json
            d = json.loads(data)
        return cls(
            msg_type=GossipMessageType(d["t"]),
            sender_id=d["s"],
            target_id=d.get("d", ""),
            payload=d.get("p", {}),
            membership_updates=d.get("m", []),
            sequence=d.get("q", 0),
        )


# Type for transport callback: (target_address, message_bytes) → response_bytes | None
TransportSend = Callable[[str, bytes], bytes | None]


@dataclass
class GossipConfig:
    """Configuration for gossip protocol."""
    ping_interval_s: float = 1.0        # How often to ping a random peer
    ping_timeout_s: float = 0.5         # Timeout for direct ping
    indirect_probes: int = 3            # Number of indirect probes on ping failure
    suspicion_timeout_s: float = 5.0    # Duration before SUSPECT → DEAD
    dead_timeout_s: float = 30.0        # Duration before DEAD entry is removed
    sync_interval_s: float = 10.0       # Full state sync interval
    max_piggyback: int = 6              # Max membership updates per message


class GossipProtocol:
    """SWIM-based gossip protocol for cluster membership.

    Usage:
        gossip = GossipProtocol("node-1", "10.0.0.1", 9000, config, transport_fn)
        gossip.join(["10.0.0.2:9000", "10.0.0.3:9000"])
        gossip.start()
        ...
        gossip.stop()
    """

    def __init__(
        self,
        node_id: str,
        host: str,
        port: int,
        config: GossipConfig | None = None,
        transport: TransportSend | None = None,
    ) -> None:
        self._config = config or GossipConfig()
        self._self_id = node_id
        self._self_entry = MemberEntry(
            node_id=node_id, host=host, port=port,
            state=NodeState.ALIVE, incarnation=1,
            last_heartbeat=time.time(),
        )
        self._members: dict[str, MemberEntry] = {node_id: self._self_entry}
        self._lock = threading.RLock()
        self._transport = transport
        self._sequence = 0
        self._running = False
        self._thread: threading.Thread | None = None
        self._sync_thread: threading.Thread | None = None

        # Callbacks for membership changes
        self._on_join: list[Callable[[MemberEntry], None]] = []
        self._on_leave: list[Callable[[MemberEntry], None]] = []
        self._on_suspect: list[Callable[[MemberEntry], None]] = []
        self._on_alive: list[Callable[[MemberEntry], None]] = []

        # Pending membership updates to piggyback
        self._update_queue: list[dict[str, Any]] = []

    # ── Public API ──────────────────────────────────────────────────

    def start(self) -> None:
        """Start the gossip protocol background threads."""
        self._running = True
        self._thread = threading.Thread(target=self._gossip_loop, daemon=True, name="gossip-ping")
        self._thread.start()
        self._sync_thread = threading.Thread(target=self._sync_loop, daemon=True, name="gossip-sync")
        self._sync_thread.start()

    def stop(self) -> None:
        """Stop gossip and broadcast LEAVE."""
        self._running = False
        # Broadcast leave
        self._self_entry.state = NodeState.LEFT
        self._broadcast_update(self._self_entry)
        if self._thread:
            self._thread.join(timeout=2.0)
        if self._sync_thread:
            self._sync_thread.join(timeout=2.0)

    def join(self, seed_addresses: list[str]) -> int:
        """Join cluster by contacting seed nodes. Returns number of members discovered."""
        join_msg = GossipMessage(
            msg_type=GossipMessageType.JOIN,
            sender_id=self._self_id,
            payload=self._self_entry.to_dict(),
            membership_updates=self._get_piggyback_updates(),
        )
        discovered = 0
        for addr in seed_addresses:
            resp_bytes = self._send(addr, join_msg.serialize())
            if resp_bytes:
                try:
                    resp = GossipMessage.deserialize(resp_bytes)
                    self._process_membership_updates(resp.membership_updates)
                    discovered += len(resp.membership_updates)
                except Exception:
                    pass
        return discovered

    def leave(self) -> None:
        """Gracefully leave the cluster."""
        self.stop()

    @property
    def node_id(self) -> str:
        return self._self_id

    @property
    def members(self) -> dict[str, MemberEntry]:
        with self._lock:
            return dict(self._members)

    @property
    def alive_members(self) -> list[MemberEntry]:
        """Get all alive/reachable members."""
        with self._lock:
            return [m for m in self._members.values() if m.is_reachable]

    @property
    def alive_count(self) -> int:
        return len(self.alive_members)

    def get_member(self, node_id: str) -> MemberEntry | None:
        return self._members.get(node_id)

    def set_metadata(self, key: str, value: Any) -> None:
        """Set metadata on this node (propagated via gossip)."""
        with self._lock:
            self._self_entry.metadata[key] = value
            self._self_entry.incarnation += 1
            self._broadcast_update(self._self_entry)

    def on_join(self, callback: Callable[[MemberEntry], None]) -> None:
        self._on_join.append(callback)

    def on_leave(self, callback: Callable[[MemberEntry], None]) -> None:
        self._on_leave.append(callback)

    def on_suspect(self, callback: Callable[[MemberEntry], None]) -> None:
        self._on_suspect.append(callback)

    def on_alive(self, callback: Callable[[MemberEntry], None]) -> None:
        self._on_alive.append(callback)

    # ── Message handling ────────────────────────────────────────────

    def handle_message(self, data: bytes) -> bytes | None:
        """Handle an incoming gossip message. Returns response bytes or None."""
        try:
            msg = GossipMessage.deserialize(data)
        except Exception:
            return None

        # Process piggybacked membership updates
        if msg.membership_updates:
            self._process_membership_updates(msg.membership_updates)

        if msg.msg_type == GossipMessageType.PING:
            return self._handle_ping(msg)
        elif msg.msg_type == GossipMessageType.PING_REQ:
            return self._handle_ping_req(msg)
        elif msg.msg_type == GossipMessageType.JOIN:
            return self._handle_join(msg)
        elif msg.msg_type == GossipMessageType.STATE_SYNC:
            return self._handle_state_sync(msg)
        return None

    def _handle_ping(self, msg: GossipMessage) -> bytes:
        """Respond to PING with ACK + piggyback updates."""
        ack = GossipMessage(
            msg_type=GossipMessageType.ACK,
            sender_id=self._self_id,
            target_id=msg.sender_id,
            membership_updates=self._get_piggyback_updates(),
            sequence=msg.sequence,
        )
        # Update sender's heartbeat
        sender = self._members.get(msg.sender_id)
        if sender:
            sender.last_heartbeat = time.time()
            if sender.state == NodeState.SUSPECT:
                sender.state = NodeState.ALIVE
                sender.suspect_since = 0.0
                self._fire_callbacks(self._on_alive, sender)
        return ack.serialize()

    def _handle_ping_req(self, msg: GossipMessage) -> bytes | None:
        """Handle indirect probe: ping target on behalf of requester."""
        target_id = msg.target_id
        target = self._members.get(target_id)
        if not target:
            return None

        # Send direct ping to target
        ping = GossipMessage(
            msg_type=GossipMessageType.PING,
            sender_id=self._self_id,
            target_id=target_id,
            sequence=self._next_seq(),
        )
        resp = self._send(target.address, ping.serialize())
        if resp:
            # Forward the ACK back
            fwd = GossipMessage(
                msg_type=GossipMessageType.PING_REQ_ACK,
                sender_id=self._self_id,
                target_id=msg.sender_id,
                payload={"target": target_id, "alive": True},
                membership_updates=self._get_piggyback_updates(),
            )
            return fwd.serialize()
        else:
            fwd = GossipMessage(
                msg_type=GossipMessageType.PING_REQ_ACK,
                sender_id=self._self_id,
                target_id=msg.sender_id,
                payload={"target": target_id, "alive": False},
            )
            return fwd.serialize()

    def _handle_join(self, msg: GossipMessage) -> bytes:
        """Handle JOIN request from a new node."""
        entry_data = msg.payload
        entry = MemberEntry.from_dict(entry_data)
        entry.last_heartbeat = time.time()

        with self._lock:
            existing = self._members.get(entry.node_id)
            if not existing or entry.incarnation >= existing.incarnation:
                self._members[entry.node_id] = entry
                self._broadcast_update(entry)
                if not existing:
                    self._fire_callbacks(self._on_join, entry)

        # Respond with full member list
        all_members = [m.to_dict() for m in self._members.values()]
        resp = GossipMessage(
            msg_type=GossipMessageType.STATE_SYNC,
            sender_id=self._self_id,
            target_id=msg.sender_id,
            membership_updates=all_members,
        )
        return resp.serialize()

    def _handle_state_sync(self, msg: GossipMessage) -> bytes:
        """Handle full state synchronization."""
        self._process_membership_updates(msg.membership_updates)
        all_members = [m.to_dict() for m in self._members.values()]
        resp = GossipMessage(
            msg_type=GossipMessageType.STATE_SYNC,
            sender_id=self._self_id,
            membership_updates=all_members,
        )
        return resp.serialize()

    # ── Background loops ────────────────────────────────────────────

    def _gossip_loop(self) -> None:
        """Main gossip loop: periodically ping a random peer."""
        while self._running:
            try:
                self._do_ping_round()
                self._check_suspects()
                self._prune_dead()
            except Exception:
                pass
            time.sleep(self._config.ping_interval_s)

    def _sync_loop(self) -> None:
        """Periodically do full state sync with a random peer."""
        while self._running:
            time.sleep(self._config.sync_interval_s)
            try:
                self._do_state_sync()
            except Exception:
                pass

    def _do_ping_round(self) -> None:
        """Pick a random alive peer and ping it."""
        with self._lock:
            candidates = [
                m for m in self._members.values()
                if m.node_id != self._self_id and m.is_reachable
            ]
        if not candidates:
            return

        target = random.choice(candidates)
        ping = GossipMessage(
            msg_type=GossipMessageType.PING,
            sender_id=self._self_id,
            target_id=target.node_id,
            membership_updates=self._get_piggyback_updates(),
            sequence=self._next_seq(),
        )

        resp_bytes = self._send(target.address, ping.serialize())
        if resp_bytes:
            try:
                resp = GossipMessage.deserialize(resp_bytes)
                self._process_membership_updates(resp.membership_updates)
                target.last_heartbeat = time.time()
                if target.state == NodeState.SUSPECT:
                    target.state = NodeState.ALIVE
                    target.suspect_since = 0.0
                    self._fire_callbacks(self._on_alive, target)
            except Exception:
                pass
        else:
            # Direct ping failed → indirect probing
            self._indirect_probe(target)

    def _indirect_probe(self, target: MemberEntry) -> None:
        """Send PING_REQ through k random peers to reach suspect node."""
        with self._lock:
            probers = [
                m for m in self._members.values()
                if m.node_id != self._self_id
                and m.node_id != target.node_id
                and m.is_reachable
            ]

        k = min(self._config.indirect_probes, len(probers))
        if k == 0:
            self._mark_suspect(target)
            return

        selected = random.sample(probers, k)
        any_ack = False

        for prober in selected:
            req = GossipMessage(
                msg_type=GossipMessageType.PING_REQ,
                sender_id=self._self_id,
                target_id=target.node_id,
                sequence=self._next_seq(),
            )
            resp_bytes = self._send(prober.address, req.serialize())
            if resp_bytes:
                try:
                    resp = GossipMessage.deserialize(resp_bytes)
                    if resp.payload.get("alive"):
                        any_ack = True
                        target.last_heartbeat = time.time()
                        break
                except Exception:
                    pass

        if not any_ack:
            self._mark_suspect(target)

    def _check_suspects(self) -> None:
        """Check if any SUSPECT nodes should be declared DEAD."""
        now = time.time()
        with self._lock:
            for m in self._members.values():
                if (
                    m.state == NodeState.SUSPECT
                    and m.suspect_since > 0
                    and (now - m.suspect_since) > self._config.suspicion_timeout_s
                ):
                    m.state = NodeState.DEAD
                    self._broadcast_update(m)
                    self._fire_callbacks(self._on_leave, m)

    def _prune_dead(self) -> None:
        """Remove DEAD entries after dead_timeout_s."""
        now = time.time()
        with self._lock:
            to_remove = [
                nid for nid, m in self._members.items()
                if m.state in (NodeState.DEAD, NodeState.LEFT)
                and m.last_heartbeat > 0
                and (now - m.last_heartbeat) > self._config.dead_timeout_s
                and nid != self._self_id
            ]
            for nid in to_remove:
                del self._members[nid]

    def _do_state_sync(self) -> None:
        """Full state sync with a random peer."""
        with self._lock:
            peers = [
                m for m in self._members.values()
                if m.node_id != self._self_id and m.is_reachable
            ]
        if not peers:
            return

        target = random.choice(peers)
        all_members = [m.to_dict() for m in self._members.values()]
        msg = GossipMessage(
            msg_type=GossipMessageType.STATE_SYNC,
            sender_id=self._self_id,
            target_id=target.node_id,
            membership_updates=all_members,
        )
        resp_bytes = self._send(target.address, msg.serialize())
        if resp_bytes:
            try:
                resp = GossipMessage.deserialize(resp_bytes)
                self._process_membership_updates(resp.membership_updates)
            except Exception:
                pass

    # ── Internal helpers ────────────────────────────────────────────

    def _mark_suspect(self, member: MemberEntry) -> None:
        """Mark a member as suspect."""
        if member.state == NodeState.ALIVE:
            member.state = NodeState.SUSPECT
            member.suspect_since = time.time()
            self._broadcast_update(member)
            self._fire_callbacks(self._on_suspect, member)

    def _process_membership_updates(self, updates: list[dict[str, Any]]) -> None:
        """Merge incoming membership updates into local state."""
        with self._lock:
            for upd in updates:
                entry = MemberEntry.from_dict(upd)
                existing = self._members.get(entry.node_id)

                if entry.node_id == self._self_id:
                    # Someone says we're suspect/dead → refute with higher incarnation
                    if entry.state in (NodeState.SUSPECT, NodeState.DEAD):
                        self._self_entry.incarnation = max(
                            self._self_entry.incarnation, entry.incarnation
                        ) + 1
                        self._self_entry.state = NodeState.ALIVE
                        self._broadcast_update(self._self_entry)
                    continue

                if not existing:
                    # New node
                    entry.last_heartbeat = time.time()
                    self._members[entry.node_id] = entry
                    if entry.state == NodeState.ALIVE:
                        self._fire_callbacks(self._on_join, entry)
                    continue

                # Update existing based on incarnation + state rules
                if entry.incarnation > existing.incarnation:
                    # Higher incarnation always wins
                    old_state = existing.state
                    existing.incarnation = entry.incarnation
                    existing.state = entry.state
                    existing.host = entry.host
                    existing.port = entry.port
                    existing.metadata.update(entry.metadata)
                    existing.last_heartbeat = time.time()
                    if old_state != entry.state:
                        if entry.state == NodeState.ALIVE:
                            existing.suspect_since = 0.0
                            self._fire_callbacks(self._on_alive, existing)
                        elif entry.state == NodeState.SUSPECT:
                            existing.suspect_since = time.time()
                            self._fire_callbacks(self._on_suspect, existing)
                        elif entry.state in (NodeState.DEAD, NodeState.LEFT):
                            self._fire_callbacks(self._on_leave, existing)
                elif entry.incarnation == existing.incarnation:
                    # Same incarnation → state ordering: ALIVE < SUSPECT < DEAD < LEFT
                    if entry.state > existing.state:
                        old_state = existing.state
                        existing.state = entry.state
                        if entry.state == NodeState.SUSPECT:
                            existing.suspect_since = time.time()
                            self._fire_callbacks(self._on_suspect, existing)
                        elif entry.state in (NodeState.DEAD, NodeState.LEFT):
                            self._fire_callbacks(self._on_leave, existing)

    def _broadcast_update(self, member: MemberEntry) -> None:
        """Queue a membership update for piggyback dissemination."""
        with self._lock:
            self._update_queue.append(member.to_dict())
            # Keep queue bounded
            if len(self._update_queue) > 100:
                self._update_queue = self._update_queue[-50:]

    def _get_piggyback_updates(self) -> list[dict[str, Any]]:
        """Get and drain recent membership updates for piggybacking."""
        with self._lock:
            updates = self._update_queue[:self._config.max_piggyback]
            if len(self._update_queue) > self._config.max_piggyback:
                self._update_queue = self._update_queue[self._config.max_piggyback:]
            else:
                self._update_queue = []
            # Always include self state
            updates.append(self._self_entry.to_dict())
            return updates

    def _send(self, address: str, data: bytes) -> bytes | None:
        """Send data to address using the transport callback."""
        if self._transport:
            try:
                return self._transport(address, data)
            except Exception:
                return None
        return None

    def _next_seq(self) -> int:
        self._sequence += 1
        return self._sequence

    @staticmethod
    def _fire_callbacks(callbacks: list[Callable], entry: MemberEntry) -> None:
        for cb in callbacks:
            try:
                cb(entry)
            except Exception:
                pass

    def summary(self) -> dict[str, Any]:
        """Return a summary of cluster membership."""
        with self._lock:
            states: dict[str, int] = {}
            for m in self._members.values():
                state_name = m.state.name
                states[state_name] = states.get(state_name, 0) + 1
            return {
                "self_id": self._self_id,
                "total_members": len(self._members),
                "states": states,
                "members": [m.to_dict() for m in self._members.values()],
            }
