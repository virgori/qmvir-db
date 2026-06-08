"""QM Distributed — Distributed Transactions.

Provides:
    - Two-Phase Commit (2PC) for cross-shard atomic transactions
    - Participant protocol for shard-level transaction management
    - Timeout-based recovery and rollback
    - Saga pattern for long-running distributed transactions
    - Transaction coordinator with persistent decision log

Architecture:
    2PC Protocol:
        Phase 1 (Prepare): Coordinator → all participants: "Can you commit?"
        Phase 2 (Commit/Abort): Coordinator → all participants: "Commit!" or "Abort!"

    Saga Pattern:
        Step 1 → Step 2 → Step 3  (forward execution)
             ↩ Compensate 1 ← Compensate 2 ← Compensate 3 (on failure)

Recovery Rules:
    - If coordinator crashes before decision: ABORT (timeout)
    - If coordinator crashes after commit decision: participants retry
    - If participant crashes after prepare: coordinator decides
    - Saga compensations are idempotent
"""

from __future__ import annotations

import threading
import time
import uuid
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class TxnPhase(IntEnum):
    """Phase of a 2PC transaction."""
    INIT = 0
    PREPARING = 1
    PREPARED = 2       # All participants voted YES
    COMMITTING = 3
    COMMITTED = 4
    ABORTING = 5
    ABORTED = 6
    UNKNOWN = 7        # Recovery needed


class ParticipantVote(IntEnum):
    """Participant's vote in prepare phase."""
    PENDING = 0
    YES = 1
    NO = 2
    TIMEOUT = 3


@dataclass
class TransactionParticipant:
    """A participant in a distributed transaction (one per shard)."""
    shard_id: int
    node_id: str
    vote: ParticipantVote = ParticipantVote.PENDING
    prepared_at: float = 0.0
    committed_at: float = 0.0
    error: str | None = None


@dataclass
class DistributedTransaction:
    """Represents a distributed transaction across multiple shards."""
    txn_id: str = ""
    coordinator_id: str = ""
    table: str = ""
    phase: TxnPhase = TxnPhase.INIT
    participants: list[TransactionParticipant] = field(default_factory=list)
    operations: list[dict[str, Any]] = field(default_factory=list)
    created_at: float = 0.0
    decided_at: float = 0.0
    completed_at: float = 0.0
    timeout_s: float = 30.0
    retries: int = 0

    def __post_init__(self) -> None:
        if not self.txn_id:
            self.txn_id = f"dtxn-{uuid.uuid4().hex[:16]}"
        if not self.created_at:
            self.created_at = time.time()

    @property
    def is_decided(self) -> bool:
        return self.phase in (TxnPhase.COMMITTED, TxnPhase.ABORTED)

    @property
    def all_prepared(self) -> bool:
        return all(p.vote == ParticipantVote.YES for p in self.participants)

    @property
    def any_rejected(self) -> bool:
        return any(p.vote == ParticipantVote.NO for p in self.participants)

    @property
    def is_expired(self) -> bool:
        return (time.time() - self.created_at) > self.timeout_s

    def to_dict(self) -> dict[str, Any]:
        return {
            "txn_id": self.txn_id,
            "table": self.table,
            "phase": self.phase.name,
            "participants": len(self.participants),
            "operations": len(self.operations),
            "created_at": self.created_at,
            "elapsed_s": round(time.time() - self.created_at, 2),
        }


# Callbacks for participant actions
PrepareCallback = Callable[[str, int, list[dict[str, Any]]], bool]  # (txn_id, shard_id, ops) → success
CommitCallback = Callable[[str, int], bool]    # (txn_id, shard_id) → success
AbortCallback = Callable[[str, int], bool]     # (txn_id, shard_id) → success

# Transport for coordinator → participant communication
TxnTransport = Callable[[str, str, dict[str, Any]], dict[str, Any] | None]
# (node_address, action, payload) → response


class TwoPhaseCommit:
    """Two-Phase Commit coordinator.

    Manages the 2PC protocol for cross-shard transactions.

    Usage:
        coordinator = TwoPhaseCommit(node_id="coord-1")
        txn = coordinator.begin("orders", shards=[(0, "node-1"), (1, "node-2")])
        txn.operations = [
            {"shard": 0, "op": "insert", "data": {...}},
            {"shard": 1, "op": "update", "filter": {...}, "data": {...}},
        ]
        result = coordinator.execute(txn)
    """

    def __init__(
        self,
        node_id: str,
        prepare_fn: PrepareCallback | None = None,
        commit_fn: CommitCallback | None = None,
        abort_fn: AbortCallback | None = None,
        transport: TxnTransport | None = None,
        default_timeout_s: float = 30.0,
    ) -> None:
        self._node_id = node_id
        self._prepare_fn = prepare_fn
        self._commit_fn = commit_fn
        self._abort_fn = abort_fn
        self._transport = transport
        self._default_timeout = default_timeout_s

        # Transaction log (would be persisted in production)
        self._transactions: dict[str, DistributedTransaction] = {}
        self._decision_log: list[tuple[str, TxnPhase, float]] = []

        self._lock = threading.Lock()

        # Stats
        self._total_txns = 0
        self._committed = 0
        self._aborted = 0
        self._timed_out = 0

    def begin(
        self,
        table: str,
        shards: list[tuple[int, str]],  # [(shard_id, node_id)]
        timeout_s: float | None = None,
    ) -> DistributedTransaction:
        """Begin a new distributed transaction."""
        txn = DistributedTransaction(
            coordinator_id=self._node_id,
            table=table,
            timeout_s=timeout_s or self._default_timeout,
        )
        for shard_id, node_id in shards:
            txn.participants.append(TransactionParticipant(
                shard_id=shard_id,
                node_id=node_id,
            ))

        with self._lock:
            self._transactions[txn.txn_id] = txn
            self._total_txns += 1

        return txn

    def execute(self, txn: DistributedTransaction) -> bool:
        """Execute the full 2PC protocol.

        Returns True if committed, False if aborted.
        """
        # Phase 1: Prepare
        prepared = self._phase_prepare(txn)

        if prepared and txn.all_prepared:
            # Decision: COMMIT
            self._log_decision(txn.txn_id, TxnPhase.COMMITTED)
            txn.phase = TxnPhase.COMMITTING
            txn.decided_at = time.time()

            # Phase 2: Commit
            self._phase_commit(txn)
            txn.phase = TxnPhase.COMMITTED
            txn.completed_at = time.time()

            with self._lock:
                self._committed += 1
            return True
        else:
            # Decision: ABORT
            self._log_decision(txn.txn_id, TxnPhase.ABORTED)
            txn.phase = TxnPhase.ABORTING
            txn.decided_at = time.time()

            # Phase 2: Abort
            self._phase_abort(txn)
            txn.phase = TxnPhase.ABORTED
            txn.completed_at = time.time()

            with self._lock:
                self._aborted += 1
            return False

    def _phase_prepare(self, txn: DistributedTransaction) -> bool:
        """Phase 1: Ask all participants to prepare."""
        txn.phase = TxnPhase.PREPARING

        for participant in txn.participants:
            if txn.is_expired:
                participant.vote = ParticipantVote.TIMEOUT
                with self._lock:
                    self._timed_out += 1
                return False

            # Get operations for this shard
            shard_ops = [
                op for op in txn.operations
                if op.get("shard") == participant.shard_id
            ]

            success = self._send_prepare(
                txn.txn_id, participant, shard_ops,
            )

            if success:
                participant.vote = ParticipantVote.YES
                participant.prepared_at = time.time()
            else:
                participant.vote = ParticipantVote.NO
                return False

        txn.phase = TxnPhase.PREPARED
        return True

    def _phase_commit(self, txn: DistributedTransaction) -> None:
        """Phase 2: Tell all participants to commit."""
        for participant in txn.participants:
            self._send_commit(txn.txn_id, participant)
            participant.committed_at = time.time()

    def _phase_abort(self, txn: DistributedTransaction) -> None:
        """Phase 2: Tell all participants to abort."""
        for participant in txn.participants:
            if participant.vote == ParticipantVote.YES:
                self._send_abort(txn.txn_id, participant)

    def _send_prepare(
        self,
        txn_id: str,
        participant: TransactionParticipant,
        operations: list[dict[str, Any]],
    ) -> bool:
        """Send prepare request to a participant."""
        if self._prepare_fn:
            try:
                return self._prepare_fn(txn_id, participant.shard_id, operations)
            except Exception as e:
                participant.error = str(e)
                return False

        if self._transport:
            try:
                resp = self._transport(participant.node_id, "prepare", {
                    "txn_id": txn_id,
                    "shard_id": participant.shard_id,
                    "operations": operations,
                })
                return resp is not None and resp.get("ok", False)
            except Exception as e:
                participant.error = str(e)
                return False

        return True  # No transport = local mode, always succeed

    def _send_commit(self, txn_id: str, participant: TransactionParticipant) -> bool:
        if self._commit_fn:
            try:
                return self._commit_fn(txn_id, participant.shard_id)
            except Exception as e:
                participant.error = str(e)
                return False

        if self._transport:
            try:
                resp = self._transport(participant.node_id, "commit", {
                    "txn_id": txn_id,
                    "shard_id": participant.shard_id,
                })
                return resp is not None and resp.get("ok", False)
            except Exception as e:
                participant.error = str(e)
                return False
        return True

    def _send_abort(self, txn_id: str, participant: TransactionParticipant) -> bool:
        if self._abort_fn:
            try:
                return self._abort_fn(txn_id, participant.shard_id)
            except Exception as e:
                participant.error = str(e)
                return False

        if self._transport:
            try:
                resp = self._transport(participant.node_id, "abort", {
                    "txn_id": txn_id,
                    "shard_id": participant.shard_id,
                })
                return resp is not None and resp.get("ok", False)
            except Exception as e:
                participant.error = str(e)
                return False
        return True

    def _log_decision(self, txn_id: str, decision: TxnPhase) -> None:
        """Log the coordinator's commit/abort decision (crash recovery)."""
        self._decision_log.append((txn_id, decision, time.time()))

    def recover(self) -> list[str]:
        """Recover in-doubt transactions after coordinator restart.

        Returns list of txn_ids that were recovered.
        """
        recovered: list[str] = []
        for txn_id, txn in self._transactions.items():
            if txn.phase in (TxnPhase.PREPARING, TxnPhase.PREPARED):
                # No decision logged → ABORT
                self._phase_abort(txn)
                txn.phase = TxnPhase.ABORTED
                recovered.append(txn_id)
            elif txn.phase == TxnPhase.COMMITTING:
                # Decision was COMMIT but not completed → retry commit
                self._phase_commit(txn)
                txn.phase = TxnPhase.COMMITTED
                recovered.append(txn_id)
        return recovered

    def get_transaction(self, txn_id: str) -> DistributedTransaction | None:
        return self._transactions.get(txn_id)

    def stats(self) -> dict[str, Any]:
        return {
            "total": self._total_txns,
            "committed": self._committed,
            "aborted": self._aborted,
            "timed_out": self._timed_out,
            "in_flight": sum(
                1 for t in self._transactions.values()
                if not t.is_decided
            ),
        }


# ── Saga Pattern ──────────────────────────────────

class SagaStepState(IntEnum):
    PENDING = 0
    EXECUTING = 1
    COMPLETED = 2
    COMPENSATING = 3
    COMPENSATED = 4
    FAILED = 5


@dataclass
class SagaStep:
    """A step in a saga."""
    name: str
    execute_fn: Callable[..., Any]
    compensate_fn: Callable[..., Any] | None = None
    args: tuple[Any, ...] = ()
    kwargs: dict[str, Any] = field(default_factory=dict)
    state: SagaStepState = SagaStepState.PENDING
    result: Any = None
    error: str | None = None
    executed_at: float = 0.0
    compensated_at: float = 0.0


@dataclass
class SagaState:
    """State of a saga execution."""
    saga_id: str = ""
    steps: list[SagaStep] = field(default_factory=list)
    current_step: int = 0
    completed: bool = False
    failed: bool = False
    compensated: bool = False
    created_at: float = 0.0
    completed_at: float = 0.0
    error: str | None = None

    def __post_init__(self) -> None:
        if not self.saga_id:
            self.saga_id = f"saga-{uuid.uuid4().hex[:12]}"
        if not self.created_at:
            self.created_at = time.time()

    def to_dict(self) -> dict[str, Any]:
        return {
            "saga_id": self.saga_id,
            "steps": len(self.steps),
            "current_step": self.current_step,
            "completed": self.completed,
            "failed": self.failed,
            "compensated": self.compensated,
            "step_states": [
                {"name": s.name, "state": s.state.name, "error": s.error}
                for s in self.steps
            ],
        }


class SagaOrchestrator:
    """Orchestrates saga-based distributed transactions.

    Use sagas for long-running operations where 2PC is impractical.
    Each step has an execute and compensate function.

    Usage:
        saga = SagaOrchestrator()
        saga.add_step("create_order", create_order_fn, cancel_order_fn, args=(order_data,))
        saga.add_step("reserve_inventory", reserve_fn, release_fn, args=(items,))
        saga.add_step("charge_payment", charge_fn, refund_fn, args=(payment,))

        result = saga.execute()
        if result.failed:
            print("Saga failed, compensations applied:", result.compensated)
    """

    def __init__(self) -> None:
        self._state: SagaState | None = None
        self._sagas: dict[str, SagaState] = {}

    def create_saga(self) -> SagaState:
        """Create a new saga."""
        state = SagaState()
        self._state = state
        self._sagas[state.saga_id] = state
        return state

    def add_step(
        self,
        name: str,
        execute_fn: Callable[..., Any],
        compensate_fn: Callable[..., Any] | None = None,
        args: tuple[Any, ...] = (),
        kwargs: dict[str, Any] | None = None,
    ) -> SagaOrchestrator:
        """Add a step to the current saga. Returns self for chaining."""
        if not self._state:
            self.create_saga()
        assert self._state is not None

        step = SagaStep(
            name=name,
            execute_fn=execute_fn,
            compensate_fn=compensate_fn,
            args=args,
            kwargs=kwargs or {},
        )
        self._state.steps.append(step)
        return self

    def execute(self, state: SagaState | None = None) -> SagaState:
        """Execute all saga steps.

        On failure, automatically runs compensations in reverse order.
        """
        saga = state or self._state
        if not saga:
            raise RuntimeError("No saga to execute")

        # Forward execution
        for i, step in enumerate(saga.steps):
            saga.current_step = i
            step.state = SagaStepState.EXECUTING

            try:
                step.result = step.execute_fn(*step.args, **step.kwargs)
                step.state = SagaStepState.COMPLETED
                step.executed_at = time.time()
            except Exception as e:
                step.state = SagaStepState.FAILED
                step.error = str(e)
                saga.error = f"Step '{step.name}' failed: {e}"
                saga.failed = True

                # Backward compensation
                self._compensate(saga, i - 1)
                saga.completed_at = time.time()
                return saga

        saga.completed = True
        saga.completed_at = time.time()
        return saga

    def _compensate(self, saga: SagaState, from_step: int) -> None:
        """Run compensations in reverse order from the given step."""
        for i in range(from_step, -1, -1):
            step = saga.steps[i]
            if step.state == SagaStepState.COMPLETED and step.compensate_fn:
                step.state = SagaStepState.COMPENSATING
                try:
                    step.compensate_fn(*step.args, **step.kwargs)
                    step.state = SagaStepState.COMPENSATED
                    step.compensated_at = time.time()
                except Exception as e:
                    step.error = f"Compensation failed: {e}"
                    # Log but continue compensating other steps

        saga.compensated = True

    def get_saga(self, saga_id: str) -> SagaState | None:
        return self._sagas.get(saga_id)
