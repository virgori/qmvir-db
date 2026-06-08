"""QM Hub — Merkle Audit Tree.

Verifies data integrity across satellites without transferring raw data.
The Hub maintains a Merkle tree where:
 - Leaves  = hash(satellite_data_page)  (reported by satellites)
 - Interior = hash(left_child || right_child)
 - Root   = single 32-byte digest summarising ALL data

Audit protocol:
 1. Satellite writes data → computes page hash → reports to Hub
 2. Hub inserts leaf into Merkle tree → recomputes root
 3. Periodic audit: Hub asks satellite for specific page hashes
    and verifies against its stored tree
 4. Mismatch → flag corruption, trigger re-sync from replica

Uses SHA-256 for collision resistance (banking-grade integrity).
"""

from __future__ import annotations

import hashlib
import struct
from dataclasses import dataclass, field
from typing import Optional


@dataclass(slots=True)
class MerkleNode:
    """A node in the Merkle audit tree."""
    hash: bytes              # 32-byte SHA-256 digest
    left: Optional["MerkleNode"] = None
    right: Optional["MerkleNode"] = None
    leaf_key: Optional[str] = None   # only for leaf nodes
    dirty: bool = False              # needs recomputation

    @property
    def is_leaf(self) -> bool:
        return self.left is None and self.right is None


def _hash_pair(left: bytes, right: bytes) -> bytes:
    """Hash two 32-byte digests together."""
    return hashlib.sha256(left + right).digest()


def _hash_leaf(data: bytes) -> bytes:
    """Hash leaf data with a domain separator to prevent second-preimage."""
    return hashlib.sha256(b"\x00" + data).digest()


def _hash_internal(left: bytes, right: bytes) -> bytes:
    """Hash internal node with domain separator."""
    return hashlib.sha256(b"\x01" + left + right).digest()


class MerkleAuditor:
    """Incremental Merkle tree for data integrity auditing.

    Supports:
     - Incremental leaf updates (O(log N) recomputation)
     - Proof generation (for verifying a single leaf)
     - Root comparison across satellites (detect divergence)
     - Serialization for WAL checkpoints

    Parameters
    ----------
    expected_capacity : int
        Hint for initial tree size (rounded up to power of 2).
    """

    def __init__(self, expected_capacity: int = 1024):
        # Map leaf_key → (index, hash)
        self._leaves: dict[str, tuple[int, bytes]] = {}
        self._leaf_order: list[str] = []   # insertion order
        self._root_hash: bytes = b"\x00" * 32
        self._dirty = True

    # ── Leaf operations ─────────────────────────────────────────────

    def update_leaf(self, key: str, data_hash: bytes) -> None:
        """Insert or update a leaf node.

        Parameters
        ----------
        key : str
            Unique identifier (e.g. "satellite_id:page_id").
        data_hash : bytes
            32-byte SHA-256 hash of the data page (computed by satellite).
        """
        if len(data_hash) != 32:
            raise ValueError(f"Expected 32-byte hash, got {len(data_hash)}")

        if key in self._leaves:
            idx, _ = self._leaves[key]
            self._leaves[key] = (idx, data_hash)
        else:
            idx = len(self._leaf_order)
            self._leaf_order.append(key)
            self._leaves[key] = (idx, data_hash)

        self._dirty = True

    def remove_leaf(self, key: str) -> None:
        """Remove a leaf (marks as empty hash)."""
        if key in self._leaves:
            idx, _ = self._leaves[key]
            self._leaves[key] = (idx, b"\x00" * 32)
            self._dirty = True

    # ── Root computation ────────────────────────────────────────────

    def root(self) -> bytes:
        """Return the current Merkle root (recomputes if dirty)."""
        if self._dirty:
            self._recompute()
        return self._root_hash

    def _recompute(self) -> None:
        """Rebuild the Merkle root from all leaves."""
        n = len(self._leaf_order)
        if n == 0:
            self._root_hash = b"\x00" * 32
            self._dirty = False
            return

        # Collect leaf hashes in order
        hashes = [self._leaves[k][1] for k in self._leaf_order]

        # Pad to power of 2
        cap = 1
        while cap < n:
            cap <<= 1
        while len(hashes) < cap:
            hashes.append(b"\x00" * 32)

        # Build tree bottom-up
        level = [_hash_leaf(h) for h in hashes]
        while len(level) > 1:
            next_level: list[bytes] = []
            for i in range(0, len(level), 2):
                left = level[i]
                right = level[i + 1] if i + 1 < len(level) else b"\x00" * 32
                next_level.append(_hash_internal(left, right))
            level = next_level

        self._root_hash = level[0]
        self._dirty = False

    # ── Merkle proof ────────────────────────────────────────────────

    def proof(self, key: str) -> list[tuple[str, bytes]]:
        """Generate a Merkle proof for a specific leaf.

        Returns a list of (side, sibling_hash) pairs from leaf → root.
        ``side`` is 'L' or 'R' indicating which side the sibling is on.
        """
        if key not in self._leaves:
            raise KeyError(f"Leaf {key!r} not in tree")

        n = len(self._leaf_order)
        cap = 1
        while cap < n:
            cap <<= 1

        hashes = [self._leaves[k][1] for k in self._leaf_order]
        while len(hashes) < cap:
            hashes.append(b"\x00" * 32)

        idx, _ = self._leaves[key]
        level = [_hash_leaf(h) for h in hashes]
        proof_path: list[tuple[str, bytes]] = []

        pos = idx
        while len(level) > 1:
            # Sibling
            if pos % 2 == 0:
                sibling = level[pos + 1] if pos + 1 < len(level) else b"\x00" * 32
                proof_path.append(("R", sibling))
            else:
                sibling = level[pos - 1]
                proof_path.append(("L", sibling))

            # Next level
            next_level: list[bytes] = []
            for i in range(0, len(level), 2):
                left = level[i]
                right = level[i + 1] if i + 1 < len(level) else b"\x00" * 32
                next_level.append(_hash_internal(left, right))
            level = next_level
            pos //= 2

        return proof_path

    @staticmethod
    def verify_proof(
        leaf_data_hash: bytes,
        proof_path: list[tuple[str, bytes]],
        expected_root: bytes,
    ) -> bool:
        """Verify a Merkle proof against an expected root."""
        current = _hash_leaf(leaf_data_hash)
        for side, sibling in proof_path:
            if side == "R":
                current = _hash_internal(current, sibling)
            else:
                current = _hash_internal(sibling, current)
        return current == expected_root

    # ── Audit ───────────────────────────────────────────────────────

    def audit_leaf(self, key: str, reported_hash: bytes) -> bool:
        """Check if a satellite-reported hash matches what the Hub expects."""
        if key not in self._leaves:
            return False
        _, stored = self._leaves[key]
        return stored == reported_hash

    def divergent_leaves(self, other_leaves: dict[str, bytes]) -> list[str]:
        """Find leaves that differ between Hub expectation and satellite report."""
        diverged: list[str] = []
        for key, reported in other_leaves.items():
            if key in self._leaves:
                _, stored = self._leaves[key]
                if stored != reported:
                    diverged.append(key)
            else:
                diverged.append(key)
        return diverged

    # ── Serialization ───────────────────────────────────────────────

    def checkpoint(self) -> bytes:
        """Serialize the leaf set for WAL checkpoint."""
        parts: list[bytes] = []
        parts.append(struct.pack("<I", len(self._leaf_order)))
        for key in self._leaf_order:
            kb = key.encode("utf-8")
            _, h = self._leaves[key]
            parts.append(struct.pack("<H", len(kb)))
            parts.append(kb)
            parts.append(h)  # 32 bytes
        return b"".join(parts)

    @classmethod
    def from_checkpoint(cls, data: bytes) -> "MerkleAuditor":
        """Restore from checkpoint bytes."""
        auditor = cls()
        offset = 0
        (count,) = struct.unpack("<I", data[offset : offset + 4])
        offset += 4
        for _ in range(count):
            (klen,) = struct.unpack("<H", data[offset : offset + 2])
            offset += 2
            key = data[offset : offset + klen].decode("utf-8")
            offset += klen
            h = data[offset : offset + 32]
            offset += 32
            auditor.update_leaf(key, h)
        return auditor

    # ── Diagnostics ─────────────────────────────────────────────────

    @property
    def leaf_count(self) -> int:
        return len(self._leaf_order)

    def stats(self) -> dict:
        return {
            "leaf_count": len(self._leaf_order),
            "root_hash": self.root().hex()[:16] + "...",
            "dirty": self._dirty,
        }
