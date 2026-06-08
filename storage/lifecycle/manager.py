"""QM Storage — Data Lifecycle Manager.

Manages hot → warm → cold data tier transitions:
  - Age-based policies
  - Access-frequency-based policies
  - Compression tier upgrades
  - Archive to cold storage
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from storage.compression.engine import CompressionTier


@dataclass
class LifecyclePolicy:
    """Policy for data tier transitions."""

    table: str
    hot_retention_s: float = 86400 * 7  # 7 days
    warm_retention_s: float = 86400 * 90  # 90 days
    cold_retention_s: float = 86400 * 365  # 1 year
    archive_after_s: float | None = None  # None = never archive
    delete_after_s: float | None = None  # None = never delete


@dataclass
class DataSegmentInfo:
    """Metadata about a data segment for lifecycle management."""

    segment_id: str
    table: str
    tier: CompressionTier = CompressionTier.HOT
    created_at: float = field(default_factory=time.time)
    last_accessed: float = field(default_factory=time.time)
    access_count: int = 0
    row_count: int = 0
    size_bytes: int = 0


class LifecycleManager:
    """Manages data lifecycle across hot/warm/cold tiers."""

    def __init__(self) -> None:
        self._policies: dict[str, LifecyclePolicy] = {}
        self._segments: dict[str, DataSegmentInfo] = {}

    def set_policy(self, policy: LifecyclePolicy) -> None:
        self._policies[policy.table] = policy

    def register_segment(self, segment: DataSegmentInfo) -> None:
        self._segments[segment.segment_id] = segment

    def evaluate(self) -> list[dict[str, Any]]:
        """Evaluate all segments and return tier transition actions."""
        actions: list[dict[str, Any]] = []
        now = time.time()

        for seg in self._segments.values():
            policy = self._policies.get(seg.table)
            if not policy:
                continue

            age = now - seg.created_at

            if seg.tier == CompressionTier.HOT and age > policy.hot_retention_s:
                actions.append({
                    "action": "tier_transition",
                    "segment_id": seg.segment_id,
                    "from_tier": "hot",
                    "to_tier": "warm",
                })

            elif seg.tier == CompressionTier.WARM and age > policy.warm_retention_s:
                actions.append({
                    "action": "tier_transition",
                    "segment_id": seg.segment_id,
                    "from_tier": "warm",
                    "to_tier": "cold",
                })

            elif (
                seg.tier == CompressionTier.COLD
                and policy.archive_after_s
                and age > policy.archive_after_s
            ):
                actions.append({
                    "action": "archive",
                    "segment_id": seg.segment_id,
                })

            if policy.delete_after_s and age > policy.delete_after_s:
                actions.append({
                    "action": "delete",
                    "segment_id": seg.segment_id,
                })

        return actions

    def apply_transition(self, segment_id: str, new_tier: CompressionTier) -> bool:
        """Apply a tier transition to a segment."""
        seg = self._segments.get(segment_id)
        if not seg:
            return False
        seg.tier = new_tier
        return True
