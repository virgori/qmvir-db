"""QM Distributed — WAL-based Replication Manager.

Provides:
    - Primary → Replica asynchronous log shipping
    - Synchronous replication option (quorum writes)
    - Per-shard replication streams
    - LSN tracking and lag monitoring
    - Automatic catch-up for lagging replicas
    - Failover: promote replica to primary

Architecture:
    Primary Node                    Replica Node
    ┌─────────┐                    ┌─────────────┐
    │ QMEngine│──WAL──→ ReplicationSender ──→ ReplicationReceiver ──→ QMEngine
    └─────────┘    │                    │   └─────────────┘
                   │                    │
                   └── LSN tracking ────┘

Replication modes:
    - ASYNC: Fire-and-forget, lowest latency, possible data loss
    - SYNC_ONE: Wait for at least one replica ACK before commit
    - SYNC_QUORUM: Wait for majority of replicas (strongest guarantee)
"""

from __future__ import annotations

import struct
import threading
import time
from collections import deque
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class ReplicationMode(IntEnum):
    """Replication mode for a shard."""
    ASYNC = 0         # Fire-and-forget (fastest, weakest guarantee)
    SYNC_ONE = 1      # Wait for 1 replica ACK
    SYNC_QUORUM = 2   # Wait for majority ACK (strongest)


class ReplicaState(IntEnum):
    """State of a replica stream."""
    INACTIVE = 0
    STREAMING = 1      # Normal log shipping
    CATCHING_UP = 2    # Behind, bulk catching up
    LAGGING = 3        # Behind but still receiving
    DISCONNECTED = 4   # No heartbeat
    PROMOTING = 5      # Being promoted to primary


@dataclass(slots=True)
class WALEntry:
    """A WAL entry for replication (simplified from storage.wal.WALRecord)."""
    lsn: int
    txn_id: int
    op: int          # WALOp integer value
    table: str
    key: str
    data: dict[str, Any] | None
    old_data: dict[str, Any] | None
    timestamp: float

    def serialize(self) -> bytes:
        """Compact binary serialization for network transfer."""
        try:
            import orjson
            payload = orjson.dumps({
                "l": self.lsn, "t": self.txn_id, "o": self.op,
                "tb": self.table, "k": self.key,
                "d": self.data, "od": self.old_data,
                "ts": self.timestamp,
            })
        except ImportError:
            import json
            payload = json.dumps({
                "l": self.lsn, "t": self.txn_id, "o": self.op,
                "tb": self.table, "k": self.key,
                "d": self.data, "od": self.old_data,
                "ts": self.timestamp,
            }).encode()
        return struct.pack("<I", len(payload)) + payload

    @classmethod
    def deserialize(cls, data: bytes) -> WALEntry:
        try:
            import orjson
            d = orjson.loads(data)
        except ImportError:
            import json
            d = json.loads(data)
        return cls(
            lsn=d["l"], txn_id=d["t"], op=d["o"],
            table=d["tb"], key=d["k"],
            data=d.get("d"), old_data=d.get("od"),
            timestamp=d.get("ts", 0.0),
        )


@dataclass
class ReplicaInfo:
    """Information about a replica for a given shard."""
    replica_id: str          # Node ID of the replica
    shard_id: int
    table: str
    state: ReplicaState = ReplicaState.INACTIVE
    applied_lsn: int = 0    # Last LSN applied on replica
    sent_lsn: int = 0       # Last LSN sent to replica
    flush_lsn: int = 0      # Last LSN flushed to disk on replica
    lag_bytes: int = 0
    lag_ms: float = 0.0
    last_heartbeat: float = 0.0
    last_error: str | None = None
    connected_at: float = 0.0
    bytes_sent: int = 0

    @property
    def is_healthy(self) -> bool:
        return (
            self.state in (ReplicaState.STREAMING, ReplicaState.CATCHING_UP)
            and (time.time() - self.last_heartbeat) < 30.0
        )

    @property
    def is_caught_up(self) -> bool:
        return self.applied_lsn >= self.sent_lsn - 1

    def to_dict(self) -> dict[str, Any]:
        return {
            "replica_id": self.replica_id,
            "shard_id": self.shard_id,
            "table": self.table,
            "state": self.state.name,
            "applied_lsn": self.applied_lsn,
            "sent_lsn": self.sent_lsn,
            "lag_bytes": self.lag_bytes,
            "lag_ms": self.lag_ms,
            "is_healthy": self.is_healthy,
        }


@dataclass
class ReplicationConfig:
    """Configuration for replication."""
    mode: ReplicationMode = ReplicationMode.ASYNC
    max_batch_size: int = 1000           # Max entries per batch
    max_batch_bytes: int = 4 * 1024 * 1024  # 4MB max batch
    heartbeat_interval_s: float = 1.0
    sync_timeout_s: float = 5.0          # Timeout for sync replication ACK
    catchup_batch_size: int = 10000      # Entries per catch-up batch
    max_lag_before_disconnect: int = 100000  # Max LSN lag before disconnecting
    wal_retention_min: int = 1440        # WAL retention in minutes (24h)


# Transport callback: (replica_address, data) → response_bytes | None
ReplicationTransport = Callable[[str, bytes], bytes | None]


@dataclass
class ReplicationAck:
    """Acknowledgment from a replica."""
    replica_id: str
    applied_lsn: int
    flush_lsn: int
    timestamp: float = 0.0

    def serialize(self) -> bytes:
        return struct.pack("<32sQQd",
            self.replica_id.encode().ljust(32, b'\0'),
            self.applied_lsn,
            self.flush_lsn,
            self.timestamp or time.time(),
        )

    @classmethod
    def deserialize(cls, data: bytes) -> ReplicationAck:
        rid_bytes, applied, flush, ts = struct.unpack("<32sQQd", data[:56])
        return cls(
            replica_id=rid_bytes.rstrip(b'\0').decode(),
            applied_lsn=applied,
            flush_lsn=flush,
            timestamp=ts,
        )


class ReplicationSender:
    """Sends WAL entries to replicas.

    Runs on the PRIMARY node. Manages replication streams for all replicas
    of a given shard.

    Usage:
        sender = ReplicationSender("shard-0", config, transport)
        sender.add_replica("node-2", "10.0.0.2:9100")
        sender.on_wal_entry(entry)  # Called for each new WAL entry
        sender.start()
    """

    def __init__(
        self,
        shard_id: int,
        table: str,
        config: ReplicationConfig | None = None,
        transport: ReplicationTransport | None = None,
    ) -> None:
        self._shard_id = shard_id
        self._table = table
        self._config = config or ReplicationConfig()
        self._transport = transport

        self._replicas: dict[str, ReplicaInfo] = {}
        self._replica_addresses: dict[str, str] = {}  # replica_id → address

        # WAL buffer for pending entries
        self._wal_buffer: deque[WALEntry] = deque(maxlen=100000)
        self._current_lsn = 0

        # Sync replication waiters
        self._sync_waiters: dict[int, threading.Event] = {}
        self._sync_ack_count: dict[int, int] = {}

        self._lock = threading.Lock()
        self._running = False
        self._thread: threading.Thread | None = None

    def add_replica(self, replica_id: str, address: str) -> ReplicaInfo:
        """Add a replica to the replication stream."""
        info = ReplicaInfo(
            replica_id=replica_id,
            shard_id=self._shard_id,
            table=self._table,
            state=ReplicaState.STREAMING,
            last_heartbeat=time.time(),
            connected_at=time.time(),
        )
        with self._lock:
            self._replicas[replica_id] = info
            self._replica_addresses[replica_id] = address
        return info

    def remove_replica(self, replica_id: str) -> None:
        with self._lock:
            self._replicas.pop(replica_id, None)
            self._replica_addresses.pop(replica_id, None)

    def on_wal_entry(self, entry: WALEntry) -> bool:
        """Called when a new WAL entry is written on the primary.

        For ASYNC: returns True immediately.
        For SYNC_ONE/QUORUM: blocks until enough ACKs received.
        """
        with self._lock:
            self._wal_buffer.append(entry)
            self._current_lsn = entry.lsn

        if self._config.mode == ReplicationMode.ASYNC:
            return True

        # Synchronous replication: wait for ACKs
        event = threading.Event()
        self._sync_waiters[entry.lsn] = event
        self._sync_ack_count[entry.lsn] = 0

        # Trigger immediate send
        self._send_to_all()

        # Wait for required acks
        required = self._required_acks()
        success = event.wait(timeout=self._config.sync_timeout_s)

        # Cleanup
        self._sync_waiters.pop(entry.lsn, None)
        self._sync_ack_count.pop(entry.lsn, None)

        return success and self._sync_ack_count.get(entry.lsn, 0) >= required

    def handle_ack(self, ack: ReplicationAck) -> None:
        """Process an ACK from a replica."""
        with self._lock:
            info = self._replicas.get(ack.replica_id)
            if info:
                info.applied_lsn = ack.applied_lsn
                info.flush_lsn = ack.flush_lsn
                info.last_heartbeat = time.time()
                info.lag_bytes = max(0, self._current_lsn - ack.applied_lsn)
                info.lag_ms = (time.time() - ack.timestamp) * 1000 if ack.timestamp else 0

                if info.state == ReplicaState.CATCHING_UP and info.is_caught_up:
                    info.state = ReplicaState.STREAMING

            # Wake sync waiters
            for lsn, event in list(self._sync_waiters.items()):
                if ack.applied_lsn >= lsn:
                    self._sync_ack_count[lsn] = self._sync_ack_count.get(lsn, 0) + 1
                    if self._sync_ack_count[lsn] >= self._required_acks():
                        event.set()

    def start(self) -> None:
        """Start the replication sender thread."""
        self._running = True
        self._thread = threading.Thread(
            target=self._sender_loop, daemon=True,
            name=f"repl-sender-{self._shard_id}",
        )
        self._thread.start()

    def stop(self) -> None:
        self._running = False
        if self._thread:
            self._thread.join(timeout=2.0)

    def get_replicas(self) -> list[ReplicaInfo]:
        return list(self._replicas.values())

    def get_replica(self, replica_id: str) -> ReplicaInfo | None:
        return self._replicas.get(replica_id)

    @property
    def current_lsn(self) -> int:
        return self._current_lsn

    def _sender_loop(self) -> None:
        """Background loop to send batches to replicas."""
        while self._running:
            try:
                self._send_to_all()
                self._check_replica_health()
            except Exception:
                pass
            time.sleep(self._config.heartbeat_interval_s)

    def _send_to_all(self) -> None:
        """Send pending entries to all replicas."""
        for replica_id, info in list(self._replicas.items()):
            if info.state in (ReplicaState.DISCONNECTED, ReplicaState.INACTIVE):
                continue
            try:
                self._send_to_replica(replica_id, info)
            except Exception as e:
                info.last_error = str(e)

    def _send_to_replica(self, replica_id: str, info: ReplicaInfo) -> None:
        """Send pending entries to a specific replica."""
        # Collect entries since replica's last applied LSN
        entries_to_send: list[WALEntry] = []
        batch_bytes = 0

        for entry in self._wal_buffer:
            if entry.lsn > info.sent_lsn:
                entries_to_send.append(entry)
                batch_bytes += 100  # Approximate entry size
                if len(entries_to_send) >= self._config.max_batch_size:
                    break
                if batch_bytes >= self._config.max_batch_bytes:
                    break

        if not entries_to_send:
            return

        # Serialize batch
        try:
            import orjson
            batch_data = orjson.dumps({
                "shard_id": self._shard_id,
                "table": self._table,
                "entries": [
                    {"l": e.lsn, "t": e.txn_id, "o": e.op,
                     "tb": e.table, "k": e.key,
                     "d": e.data, "od": e.old_data, "ts": e.timestamp}
                    for e in entries_to_send
                ],
            })
        except ImportError:
            import json
            batch_data = json.dumps({
                "shard_id": self._shard_id,
                "table": self._table,
                "entries": [
                    {"l": e.lsn, "t": e.txn_id, "o": e.op,
                     "tb": e.table, "k": e.key,
                     "d": e.data, "od": e.old_data, "ts": e.timestamp}
                    for e in entries_to_send
                ],
            }).encode()

        # Send
        address = self._replica_addresses.get(replica_id, "")
        if self._transport and address:
            resp = self._transport(address, batch_data)
            if resp:
                try:
                    ack = ReplicationAck.deserialize(resp)
                    self.handle_ack(ack)
                except Exception:
                    pass

        # Update sent_lsn
        info.sent_lsn = entries_to_send[-1].lsn
        info.bytes_sent += len(batch_data)

    def _check_replica_health(self) -> None:
        """Check replica health and mark disconnected ones."""
        now = time.time()
        for info in self._replicas.values():
            if info.state == ReplicaState.INACTIVE:
                continue
            if (now - info.last_heartbeat) > 30.0:
                info.state = ReplicaState.DISCONNECTED
            elif info.lag_bytes > self._config.max_lag_before_disconnect:
                info.state = ReplicaState.LAGGING

    def _required_acks(self) -> int:
        """How many ACKs needed based on replication mode."""
        total = len(self._replicas)
        if self._config.mode == ReplicationMode.SYNC_ONE:
            return 1
        elif self._config.mode == ReplicationMode.SYNC_QUORUM:
            return (total // 2) + 1
        return 0


class ReplicationReceiver:
    """Receives and applies WAL entries from the primary.

    Runs on REPLICA nodes. Applies entries to the local engine.

    Usage:
        receiver = ReplicationReceiver("shard-0", apply_fn, config)
        receiver.handle_batch(batch_data)  # Called by network layer
    """

    def __init__(
        self,
        shard_id: int,
        table: str,
        apply_fn: Callable[[WALEntry], bool] | None = None,
        config: ReplicationConfig | None = None,
    ) -> None:
        self._shard_id = shard_id
        self._table = table
        self._apply_fn = apply_fn
        self._config = config or ReplicationConfig()

        self._applied_lsn = 0
        self._flush_lsn = 0
        self._entries_applied = 0
        self._last_receive_time = 0.0
        self._lock = threading.Lock()

    @property
    def applied_lsn(self) -> int:
        return self._applied_lsn

    @property
    def flush_lsn(self) -> int:
        return self._flush_lsn

    def handle_batch(self, batch_data: bytes) -> ReplicationAck:
        """Handle an incoming batch of WAL entries from the primary."""
        try:
            import orjson
            batch = orjson.loads(batch_data)
        except ImportError:
            import json
            batch = json.loads(batch_data)

        entries_raw = batch.get("entries", [])
        applied = 0

        for e_raw in entries_raw:
            entry = WALEntry(
                lsn=e_raw["l"], txn_id=e_raw["t"], op=e_raw["o"],
                table=e_raw["tb"], key=e_raw["k"],
                data=e_raw.get("d"), old_data=e_raw.get("od"),
                timestamp=e_raw.get("ts", 0.0),
            )

            # Skip already-applied entries
            if entry.lsn <= self._applied_lsn:
                continue

            # Apply entry
            success = True
            if self._apply_fn:
                try:
                    success = self._apply_fn(entry)
                except Exception:
                    success = False

            if success:
                with self._lock:
                    self._applied_lsn = entry.lsn
                    self._entries_applied += 1
                    applied += 1

        self._last_receive_time = time.time()
        self._flush_lsn = self._applied_lsn  # Simplified: flush = applied

        return ReplicationAck(
            replica_id="",  # Caller should set this
            applied_lsn=self._applied_lsn,
            flush_lsn=self._flush_lsn,
            timestamp=time.time(),
        )

    def stats(self) -> dict[str, Any]:
        return {
            "shard_id": self._shard_id,
            "table": self._table,
            "applied_lsn": self._applied_lsn,
            "flush_lsn": self._flush_lsn,
            "entries_applied": self._entries_applied,
            "last_receive": self._last_receive_time,
        }


class ReplicationManager:
    """High-level replication manager coordinating senders and receivers.

    Manages all replication streams for a node (both as primary and replica).

    Usage:
        mgr = ReplicationManager(node_id, config)
        mgr.setup_primary("table", shard_id=0, replicas=["node-2", "node-3"])
        mgr.on_wal_write("table", 0, wal_entry)  # When primary writes
    """

    def __init__(
        self,
        node_id: str,
        config: ReplicationConfig | None = None,
        transport: ReplicationTransport | None = None,
    ) -> None:
        self._node_id = node_id
        self._config = config or ReplicationConfig()
        self._transport = transport

        # Senders (when we are primary)
        # Key: (table, shard_id)
        self._senders: dict[tuple[str, int], ReplicationSender] = {}

        # Receivers (when we are replica)
        # Key: (table, shard_id)
        self._receivers: dict[tuple[str, int], ReplicationReceiver] = {}

        self._lock = threading.Lock()

    def setup_primary(
        self,
        table: str,
        shard_id: int,
        replica_addresses: dict[str, str],  # replica_id → address
    ) -> ReplicationSender:
        """Setup this node as primary for a shard."""
        sender = ReplicationSender(
            shard_id=shard_id, table=table,
            config=self._config, transport=self._transport,
        )
        for rid, addr in replica_addresses.items():
            sender.add_replica(rid, addr)

        with self._lock:
            self._senders[(table, shard_id)] = sender
        sender.start()
        return sender

    def setup_replica(
        self,
        table: str,
        shard_id: int,
        apply_fn: Callable[[WALEntry], bool] | None = None,
    ) -> ReplicationReceiver:
        """Setup this node as replica for a shard."""
        receiver = ReplicationReceiver(
            shard_id=shard_id, table=table,
            apply_fn=apply_fn, config=self._config,
        )
        with self._lock:
            self._receivers[(table, shard_id)] = receiver
        return receiver

    def on_wal_write(self, table: str, shard_id: int, entry: WALEntry) -> bool:
        """Called when a WAL entry is written on a primary shard.

        Forwards to the appropriate replication sender.
        Returns True if replication requirements met.
        """
        sender = self._senders.get((table, shard_id))
        if sender:
            return sender.on_wal_entry(entry)
        return True  # No replication configured

    def handle_replication_batch(
        self, table: str, shard_id: int, batch_data: bytes,
    ) -> bytes:
        """Handle incoming replication batch on a replica.

        Returns serialized ACK.
        """
        receiver = self._receivers.get((table, shard_id))
        if receiver:
            ack = receiver.handle_batch(batch_data)
            ack.replica_id = self._node_id
            return ack.serialize()
        return b""

    def promote_to_primary(self, table: str, shard_id: int) -> bool:
        """Promote a replica to primary.

        1. Stop receiving replication
        2. Setup sender for remaining replicas
        """
        with self._lock:
            receiver = self._receivers.pop((table, shard_id), None)
            if not receiver:
                return False
            # Create a sender (replicas will be added by coordinator)
            sender = ReplicationSender(
                shard_id=shard_id, table=table,
                config=self._config, transport=self._transport,
            )
            self._senders[(table, shard_id)] = sender
            sender.start()
        return True

    def demote_to_replica(
        self,
        table: str,
        shard_id: int,
        apply_fn: Callable[[WALEntry], bool] | None = None,
    ) -> bool:
        """Demote a primary to replica."""
        with self._lock:
            sender = self._senders.pop((table, shard_id), None)
            if sender:
                sender.stop()
            receiver = ReplicationReceiver(
                shard_id=shard_id, table=table,
                apply_fn=apply_fn, config=self._config,
            )
            self._receivers[(table, shard_id)] = receiver
        return True

    def get_sender(self, table: str, shard_id: int) -> ReplicationSender | None:
        return self._senders.get((table, shard_id))

    def get_receiver(self, table: str, shard_id: int) -> ReplicationReceiver | None:
        return self._receivers.get((table, shard_id))

    def stop_all(self) -> None:
        """Stop all replication streams."""
        for sender in self._senders.values():
            sender.stop()
        self._senders.clear()
        self._receivers.clear()

    def stats(self) -> dict[str, Any]:
        """Replication statistics."""
        sender_stats = {}
        for (table, sid), sender in self._senders.items():
            sender_stats[f"{table}:{sid}"] = {
                "current_lsn": sender.current_lsn,
                "replicas": [r.to_dict() for r in sender.get_replicas()],
            }

        receiver_stats = {}
        for (table, sid), receiver in self._receivers.items():
            receiver_stats[f"{table}:{sid}"] = receiver.stats()

        return {
            "node_id": self._node_id,
            "mode": self._config.mode.name,
            "as_primary": sender_stats,
            "as_replica": receiver_stats,
        }
