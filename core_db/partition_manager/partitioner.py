"""QM Core DB — Partition Manager.

Supports:
  - Range partitioning (by time, by ID range)
  - Hash partitioning (by tenant_id, namespace)
  - List partitioning (by category, status)
  - Automatic partition creation and pruning
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class PartitionType(Enum):
    RANGE = "range"
    HASH = "hash"
    LIST = "list"


@dataclass
class Partition:
    """A single partition of a table."""

    partition_id: str
    table_name: str
    partition_type: PartitionType
    partition_key: str
    bounds: dict[str, Any] = field(default_factory=dict)
    row_count: int = 0
    size_bytes: int = 0
    is_active: bool = True

    @property
    def is_empty(self) -> bool:
        return self.row_count == 0


@dataclass
class PartitionRule:
    """Rule for automatic partition management."""

    table_name: str
    partition_type: PartitionType
    partition_key: str
    # For range: interval (e.g., "1 month", "1 day")
    range_interval: str | None = None
    # For hash: number of buckets
    hash_buckets: int | None = None
    # For list: explicit values
    list_values: list[Any] | None = None
    # Auto-create new partitions ahead of time
    auto_create_ahead: int = 2
    # Auto-drop old partitions after N periods
    retention_periods: int | None = None


class PartitionManager:
    """Manages table partitioning strategies."""

    def __init__(self) -> None:
        self._rules: dict[str, PartitionRule] = {}
        self._partitions: dict[str, list[Partition]] = {}

    def add_rule(self, rule: PartitionRule) -> None:
        """Register a partition rule for a table."""
        self._rules[rule.table_name] = rule
        if rule.table_name not in self._partitions:
            self._partitions[rule.table_name] = []

    def create_partition(
        self,
        table_name: str,
        partition_id: str,
        bounds: dict[str, Any],
    ) -> Partition:
        """Create a new partition."""
        rule = self._rules.get(table_name)
        if not rule:
            raise ValueError(f"No partition rule for table '{table_name}'")

        partition = Partition(
            partition_id=partition_id,
            table_name=table_name,
            partition_type=rule.partition_type,
            partition_key=rule.partition_key,
            bounds=bounds,
        )

        self._partitions.setdefault(table_name, []).append(partition)
        return partition

    def route(self, table_name: str, key_value: Any) -> Partition | None:
        """Route a row to the correct partition based on key value."""
        rule = self._rules.get(table_name)
        if not rule:
            return None

        partitions = self._partitions.get(table_name, [])

        if rule.partition_type == PartitionType.HASH:
            if rule.hash_buckets:
                bucket = hash(key_value) % rule.hash_buckets
                for p in partitions:
                    if p.bounds.get("bucket") == bucket:
                        return p

        elif rule.partition_type == PartitionType.RANGE:
            for p in partitions:
                low = p.bounds.get("low")
                high = p.bounds.get("high")
                if low is not None and high is not None:
                    if low <= key_value < high:
                        return p

        elif rule.partition_type == PartitionType.LIST:
            for p in partitions:
                if key_value in p.bounds.get("values", []):
                    return p

        return None

    def get_partitions(self, table_name: str) -> list[Partition]:
        return self._partitions.get(table_name, [])

    def prune_old(self, table_name: str) -> list[str]:
        """Prune oldest partitions based on retention policy."""
        rule = self._rules.get(table_name)
        if not rule or not rule.retention_periods:
            return []

        partitions = self._partitions.get(table_name, [])
        if len(partitions) <= rule.retention_periods:
            return []

        pruned: list[str] = []
        excess = len(partitions) - rule.retention_periods
        for p in partitions[:excess]:
            p.is_active = False
            pruned.append(p.partition_id)

        self._partitions[table_name] = [
            p for p in partitions if p.is_active
        ]
        return pruned
