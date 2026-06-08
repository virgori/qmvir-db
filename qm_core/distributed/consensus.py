"""QM Distributed — Raft-inspired Consensus for Leader Election and Log Replication.

Simplified Raft protocol providing:
    - Leader election with term-based voting
    - Log replication for metadata changes
    - Heartbeat-based leader lease
    - Automatic leader failover

States: FOLLOWER → CANDIDATE → LEADER

This is used for:
    - Electing the cluster coordinator
    - Replicating shard assignment table
    - Coordinating DDL operations (CREATE/DROP TABLE)
    - Coordinating shard rebalancing decisions

NOT used for: data replication (that uses WAL streaming instead).
"""

from __future__ import annotations

import random
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class RaftState(IntEnum):
    """Raft node states."""
    FOLLOWER = 0
    CANDIDATE = 1
    LEADER = 2


class LogEntryType(IntEnum):
    """Types of Raft log entries."""
    NOOP = 0                # Leader no-op for term commit
    SHARD_ASSIGN = 1        # Shard assignment change
    TABLE_CREATE = 2        # DDL: Create table
    TABLE_DROP = 3          # DDL: Drop table
    CONFIG_CHANGE = 4       # Cluster configuration change
    MEMBERSHIP_CHANGE = 5   # Node join/leave


@dataclass(slots=True)
class LogEntry:
    """A single Raft log entry."""
    term: int
    index: int
    entry_type: LogEntryType
    data: dict[str, Any] = field(default_factory=dict)
    timestamp: float = 0.0

    def to_dict(self) -> dict[str, Any]:
        return {
            "term": self.term,
            "index": self.index,
            "type": int(self.entry_type),
            "data": self.data,
            "ts": self.timestamp,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> LogEntry:
        return cls(
            term=d["term"],
            index=d["index"],
            entry_type=LogEntryType(d["type"]),
            data=d.get("data", {}),
            timestamp=d.get("ts", 0.0),
        )


@dataclass(slots=True)
class VoteRequest:
    """RequestVote RPC."""
    term: int
    candidate_id: str
    last_log_index: int
    last_log_term: int


@dataclass(slots=True)
class VoteResponse:
    """Response to RequestVote."""
    term: int
    vote_granted: bool
    voter_id: str


@dataclass(slots=True)
class AppendEntriesRequest:
    """AppendEntries RPC (also used for heartbeats)."""
    term: int
    leader_id: str
    prev_log_index: int
    prev_log_term: int
    entries: list[LogEntry]
    leader_commit: int


@dataclass(slots=True)
class AppendEntriesResponse:
    """Response to AppendEntries."""
    term: int
    success: bool
    match_index: int
    responder_id: str


class RaftTransport:
    """Abstract transport layer for Raft RPCs."""

    def request_vote(self, target: str, req: VoteRequest) -> VoteResponse | None:
        raise NotImplementedError

    def append_entries(self, target: str, req: AppendEntriesRequest) -> AppendEntriesResponse | None:
        raise NotImplementedError


class InMemoryRaftTransport(RaftTransport):
    """In-memory transport for testing. Routes to registered node handlers."""

    def __init__(self) -> None:
        self._nodes: dict[str, RaftNode] = {}

    def register(self, node: RaftNode) -> None:
        self._nodes[node.node_id] = node

    def request_vote(self, target: str, req: VoteRequest) -> VoteResponse | None:
        node = self._nodes.get(target)
        if node:
            return node.handle_vote_request(req)
        return None

    def append_entries(self, target: str, req: AppendEntriesRequest) -> AppendEntriesResponse | None:
        node = self._nodes.get(target)
        if node:
            return node.handle_append_entries(req)
        return None


@dataclass
class RaftConfig:
    """Raft protocol configuration."""
    election_timeout_min_ms: int = 150   # Min election timeout
    election_timeout_max_ms: int = 300   # Max election timeout
    heartbeat_interval_ms: int = 50      # Leader heartbeat interval
    max_entries_per_append: int = 100    # Max entries per AppendEntries RPC


class RaftNode:
    """A single Raft consensus node.

    Usage:
        transport = InMemoryRaftTransport()
        node = RaftNode("node-1", ["node-2", "node-3"], transport, config)
        transport.register(node)
        node.start()
        ...
        node.stop()
    """

    def __init__(
        self,
        node_id: str,
        peers: list[str],
        transport: RaftTransport,
        config: RaftConfig | None = None,
    ) -> None:
        self._node_id = node_id
        self._peers = list(peers)
        self._transport = transport
        self._config = config or RaftConfig()

        # Persistent state (would be persisted to disk in production)
        self._current_term = 0
        self._voted_for: str | None = None
        self._log: list[LogEntry] = []  # 1-indexed, but stored 0-indexed

        # Volatile state
        self._state = RaftState.FOLLOWER
        self._commit_index = 0
        self._last_applied = 0
        self._leader_id: str | None = None

        # Leader-only volatile state
        self._next_index: dict[str, int] = {}   # peer → next log index to send
        self._match_index: dict[str, int] = {}   # peer → highest replicated index

        # Timing
        self._last_heartbeat = time.time()
        self._election_timeout = self._random_election_timeout()

        # Threading
        self._lock = threading.RLock()
        self._running = False
        self._thread: threading.Thread | None = None

        # Callbacks for committed entries
        self._apply_callbacks: list[Callable[[LogEntry], None]] = []

    @property
    def node_id(self) -> str:
        return self._node_id

    @property
    def state(self) -> RaftState:
        return self._state

    @property
    def current_term(self) -> int:
        return self._current_term

    @property
    def leader_id(self) -> str | None:
        return self._leader_id

    @property
    def is_leader(self) -> bool:
        return self._state == RaftState.LEADER

    @property
    def commit_index(self) -> int:
        return self._commit_index

    @property
    def log_length(self) -> int:
        return len(self._log)

    def on_apply(self, callback: Callable[[LogEntry], None]) -> None:
        """Register callback for when a log entry is committed and applied."""
        self._apply_callbacks.append(callback)

    # ── Lifecycle ───────────────────────────────────────────────────

    def start(self) -> None:
        """Start the Raft node."""
        self._running = True
        self._thread = threading.Thread(target=self._main_loop, daemon=True, name=f"raft-{self._node_id}")
        self._thread.start()

    def stop(self) -> None:
        """Stop the Raft node."""
        self._running = False
        if self._thread:
            self._thread.join(timeout=2.0)

    # ── Client API ──────────────────────────────────────────────────

    def propose(self, entry_type: LogEntryType, data: dict[str, Any]) -> LogEntry | None:
        """Propose a new log entry. Only succeeds on leader.

        Returns the LogEntry if accepted (not yet committed), None if not leader.
        """
        with self._lock:
            if self._state != RaftState.LEADER:
                return None

            entry = LogEntry(
                term=self._current_term,
                index=len(self._log) + 1,
                entry_type=entry_type,
                data=data,
                timestamp=time.time(),
            )
            self._log.append(entry)
            # Update self match
            self._match_index[self._node_id] = entry.index
            return entry

    def wait_committed(self, index: int, timeout_s: float = 5.0) -> bool:
        """Wait until the given log index is committed."""
        deadline = time.time() + timeout_s
        while time.time() < deadline:
            if self._commit_index >= index:
                return True
            time.sleep(0.01)
        return False

    def get_log_entry(self, index: int) -> LogEntry | None:
        """Get a log entry by index (1-based)."""
        if 1 <= index <= len(self._log):
            return self._log[index - 1]
        return None

    def get_committed_entries(self, since_index: int = 0) -> list[LogEntry]:
        """Get all committed entries since a given index."""
        with self._lock:
            entries = []
            for i in range(since_index, self._commit_index):
                if i < len(self._log):
                    entries.append(self._log[i])
            return entries

    # ── RPC handlers ────────────────────────────────────────────────

    def handle_vote_request(self, req: VoteRequest) -> VoteResponse:
        """Handle an incoming RequestVote RPC."""
        with self._lock:
            # Rule 1: If term < currentTerm → reject
            if req.term < self._current_term:
                return VoteResponse(
                    term=self._current_term, vote_granted=False,
                    voter_id=self._node_id,
                )

            # Step down if higher term
            if req.term > self._current_term:
                self._become_follower(req.term)

            # Rule 2: Vote if we haven't voted or already voted for this candidate
            can_vote = (
                self._voted_for is None or self._voted_for == req.candidate_id
            )

            # Rule 3: Candidate's log must be at least as up-to-date
            last_term = self._log[-1].term if self._log else 0
            last_index = len(self._log)
            log_ok = (
                req.last_log_term > last_term
                or (req.last_log_term == last_term and req.last_log_index >= last_index)
            )

            if can_vote and log_ok:
                self._voted_for = req.candidate_id
                self._last_heartbeat = time.time()  # Reset election timer
                return VoteResponse(
                    term=self._current_term, vote_granted=True,
                    voter_id=self._node_id,
                )

            return VoteResponse(
                term=self._current_term, vote_granted=False,
                voter_id=self._node_id,
            )

    def handle_append_entries(self, req: AppendEntriesRequest) -> AppendEntriesResponse:
        """Handle an incoming AppendEntries RPC (heartbeat or log replication)."""
        with self._lock:
            # Rule 1: Reject if term < currentTerm
            if req.term < self._current_term:
                return AppendEntriesResponse(
                    term=self._current_term, success=False,
                    match_index=0, responder_id=self._node_id,
                )

            # Recognize leader, step down if needed
            if req.term >= self._current_term:
                self._become_follower(req.term)
            self._leader_id = req.leader_id
            self._last_heartbeat = time.time()

            # Rule 2: Check prev_log consistency
            if req.prev_log_index > 0:
                if req.prev_log_index > len(self._log):
                    return AppendEntriesResponse(
                        term=self._current_term, success=False,
                        match_index=len(self._log), responder_id=self._node_id,
                    )
                prev_entry = self._log[req.prev_log_index - 1]
                if prev_entry.term != req.prev_log_term:
                    # Conflict: delete from here onwards
                    self._log = self._log[:req.prev_log_index - 1]
                    return AppendEntriesResponse(
                        term=self._current_term, success=False,
                        match_index=len(self._log), responder_id=self._node_id,
                    )

            # Append new entries
            for entry in req.entries:
                idx = entry.index - 1  # Convert to 0-based
                if idx < len(self._log):
                    if self._log[idx].term != entry.term:
                        self._log = self._log[:idx]
                        self._log.append(entry)
                else:
                    self._log.append(entry)

            # Update commit index
            if req.leader_commit > self._commit_index:
                self._commit_index = min(req.leader_commit, len(self._log))
                self._apply_committed()

            return AppendEntriesResponse(
                term=self._current_term, success=True,
                match_index=len(self._log), responder_id=self._node_id,
            )

    # ── Main loop ───────────────────────────────────────────────────

    def _main_loop(self) -> None:
        """Main Raft event loop."""
        while self._running:
            with self._lock:
                state = self._state

            if state == RaftState.FOLLOWER:
                self._follower_tick()
            elif state == RaftState.CANDIDATE:
                self._candidate_tick()
            elif state == RaftState.LEADER:
                self._leader_tick()

            time.sleep(0.01)  # 10ms tick

    def _follower_tick(self) -> None:
        """Follower: check if election timeout expired."""
        elapsed = (time.time() - self._last_heartbeat) * 1000
        if elapsed > self._election_timeout:
            self._start_election()

    def _candidate_tick(self) -> None:
        """Candidate: run election (called once per election)."""
        # Election already started in _start_election.
        # Check timeout for re-election.
        elapsed = (time.time() - self._last_heartbeat) * 1000
        if elapsed > self._election_timeout:
            self._start_election()

    def _leader_tick(self) -> None:
        """Leader: send heartbeats and replicate log entries."""
        self._send_heartbeats()
        self._advance_commit_index()
        self._apply_committed()
        time.sleep(self._config.heartbeat_interval_ms / 1000.0)

    # ── Election ────────────────────────────────────────────────────

    def _start_election(self) -> None:
        """Begin a new election."""
        with self._lock:
            self._current_term += 1
            self._state = RaftState.CANDIDATE
            self._voted_for = self._node_id
            self._last_heartbeat = time.time()
            self._election_timeout = self._random_election_timeout()

            term = self._current_term
            last_index = len(self._log)
            last_term = self._log[-1].term if self._log else 0

        votes = 1  # Self vote
        total = len(self._peers) + 1
        majority = total // 2 + 1

        # Single node cluster: self-vote is sufficient
        if votes >= majority:
            self._become_leader()
            return

        for peer in self._peers:
            req = VoteRequest(
                term=term,
                candidate_id=self._node_id,
                last_log_index=last_index,
                last_log_term=last_term,
            )
            resp = self._transport.request_vote(peer, req)
            if resp:
                with self._lock:
                    if resp.term > self._current_term:
                        self._become_follower(resp.term)
                        return
                if resp.vote_granted:
                    votes += 1
                    if votes >= majority:
                        self._become_leader()
                        return

    def _become_follower(self, term: int) -> None:
        """Step down to follower state."""
        self._state = RaftState.FOLLOWER
        self._current_term = term
        self._voted_for = None
        self._leader_id = None
        self._last_heartbeat = time.time()  # Reset election timer

    def _become_leader(self) -> None:
        """Transition to leader state."""
        with self._lock:
            self._state = RaftState.LEADER
            self._leader_id = self._node_id
            # Initialize next_index and match_index
            next_idx = len(self._log) + 1
            for peer in self._peers:
                self._next_index[peer] = next_idx
                self._match_index[peer] = 0
            self._match_index[self._node_id] = len(self._log)

        # Append a NOOP entry to commit entries from previous terms
        self.propose(LogEntryType.NOOP, {"leader": self._node_id})

    # ── Log replication ─────────────────────────────────────────────

    def _send_heartbeats(self) -> None:
        """Send AppendEntries (heartbeats + log entries) to all peers."""
        for peer in self._peers:
            self._replicate_to(peer)

    def _replicate_to(self, peer: str) -> None:
        """Send AppendEntries to a specific peer."""
        with self._lock:
            next_idx = self._next_index.get(peer, 1)
            prev_idx = next_idx - 1
            prev_term = self._log[prev_idx - 1].term if prev_idx > 0 and prev_idx <= len(self._log) else 0

            # Entries to send
            entries = self._log[next_idx - 1:next_idx - 1 + self._config.max_entries_per_append]

            req = AppendEntriesRequest(
                term=self._current_term,
                leader_id=self._node_id,
                prev_log_index=prev_idx,
                prev_log_term=prev_term,
                entries=entries,
                leader_commit=self._commit_index,
            )

        resp = self._transport.append_entries(peer, req)
        if resp:
            with self._lock:
                if resp.term > self._current_term:
                    self._become_follower(resp.term)
                    return
                if resp.success:
                    self._match_index[peer] = resp.match_index
                    self._next_index[peer] = resp.match_index + 1
                else:
                    # Decrement next_index and retry
                    self._next_index[peer] = max(1, self._next_index.get(peer, 1) - 1)

    def _advance_commit_index(self) -> None:
        """Advance commit_index based on majority replication."""
        with self._lock:
            if self._state != RaftState.LEADER:
                return

            total = len(self._peers) + 1
            majority = total // 2 + 1

            for n in range(self._commit_index + 1, len(self._log) + 1):
                if self._log[n - 1].term != self._current_term:
                    continue  # Only commit entries from current term
                replicated = sum(
                    1 for p in list(self._match_index.values())
                    if p >= n
                )
                if replicated >= majority:
                    self._commit_index = n

    def _apply_committed(self) -> None:
        """Apply committed but unapplied entries."""
        while self._last_applied < self._commit_index:
            self._last_applied += 1
            if self._last_applied <= len(self._log):
                entry = self._log[self._last_applied - 1]
                for cb in self._apply_callbacks:
                    try:
                        cb(entry)
                    except Exception:
                        pass

    # ── Helpers ─────────────────────────────────────────────────────

    def _random_election_timeout(self) -> float:
        return random.randint(
            self._config.election_timeout_min_ms,
            self._config.election_timeout_max_ms,
        )

    def status(self) -> dict[str, Any]:
        """Return node status."""
        with self._lock:
            return {
                "node_id": self._node_id,
                "state": self._state.name,
                "term": self._current_term,
                "leader_id": self._leader_id,
                "log_length": len(self._log),
                "commit_index": self._commit_index,
                "last_applied": self._last_applied,
                "voted_for": self._voted_for,
                "peers": self._peers,
            }
