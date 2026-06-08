"""QM Trigger System — BEFORE/AFTER triggers with row/statement granularity.

Supports:
    - BEFORE INSERT/UPDATE/DELETE — can modify NEW row or cancel operation
    - AFTER INSERT/UPDATE/DELETE — for side effects, CDC, audit logging
    - Row-level (FOR EACH ROW) and statement-level (FOR EACH STATEMENT)
    - Priority ordering (lower = runs first)
    - Conditional triggers (WHEN clause as callable predicate)
    - Trigger enable/disable
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum, auto
from typing import Any, Callable


class TriggerTiming(Enum):
    """When the trigger fires relative to the operation."""
    BEFORE = "before"
    AFTER = "after"
    INSTEAD_OF = "instead_of"


class TriggerEvent(Enum):
    """Which DML event triggers this trigger."""
    INSERT = "insert"
    UPDATE = "update"
    DELETE = "delete"


class TriggerLevel(Enum):
    """Granularity of the trigger."""
    ROW = "row"          # Fires once per affected row
    STATEMENT = "statement"  # Fires once per statement


@dataclass
class TriggerContext:
    """Context passed to trigger functions.

    Attributes:
        table: Target table name
        event: The DML event type
        timing: BEFORE or AFTER
        old_row: The row before modification (UPDATE/DELETE only)
        new_row: The row after modification (INSERT/UPDATE only)
        cancelled: Set to True in BEFORE triggers to cancel the operation
        extra: Arbitrary context data
    """
    table: str
    event: TriggerEvent
    timing: TriggerTiming
    old_row: dict[str, Any] | None = None
    new_row: dict[str, Any] | None = None
    cancelled: bool = False
    extra: dict[str, Any] = field(default_factory=dict)

    def cancel(self) -> None:
        """Cancel the operation (only effective in BEFORE triggers)."""
        self.cancelled = True


@dataclass
class Trigger:
    """A registered database trigger.

    The `action` callable receives a TriggerContext and may:
        - Modify ctx.new_row (BEFORE INSERT/UPDATE) to change the data
        - Call ctx.cancel() (BEFORE) to prevent the operation
        - Perform side effects (AFTER) like logging or notifications
    """
    name: str
    table: str
    event: TriggerEvent
    timing: TriggerTiming
    action: Callable[[TriggerContext], None]
    level: TriggerLevel = TriggerLevel.ROW
    priority: int = 100  # Lower = fires first
    enabled: bool = True
    when: Callable[[TriggerContext], bool] | None = None  # Conditional predicate
    created_at: float = field(default_factory=time.time)
    description: str = ""


class TriggerCatalog:
    """Registry for database triggers."""

    def __init__(self) -> None:
        # (table, event, timing) → list of triggers (sorted by priority)
        self._triggers: dict[tuple[str, TriggerEvent, TriggerTiming], list[Trigger]] = {}
        self._all_triggers: dict[str, Trigger] = {}

    def register(self, trigger: Trigger) -> None:
        """Register a trigger."""
        key = (trigger.table.lower(), trigger.event, trigger.timing)
        if key not in self._triggers:
            self._triggers[key] = []
        self._triggers[key].append(trigger)
        self._triggers[key].sort(key=lambda t: t.priority)
        self._all_triggers[trigger.name.lower()] = trigger

    def drop(self, name: str) -> bool:
        """Drop a trigger by name."""
        trigger = self._all_triggers.pop(name.lower(), None)
        if trigger is None:
            return False
        key = (trigger.table.lower(), trigger.event, trigger.timing)
        if key in self._triggers:
            self._triggers[key] = [t for t in self._triggers[key] if t.name.lower() != name.lower()]
        return True

    def get(self, name: str) -> Trigger | None:
        """Look up a trigger by name."""
        return self._all_triggers.get(name.lower())

    def get_triggers(
        self, table: str, event: TriggerEvent, timing: TriggerTiming
    ) -> list[Trigger]:
        """Get all matching triggers for a table/event/timing combination."""
        key = (table.lower(), event, timing)
        return [t for t in self._triggers.get(key, []) if t.enabled]

    def list_triggers(self, table: str | None = None) -> list[Trigger]:
        """List all triggers, optionally filtered by table."""
        if table is None:
            return list(self._all_triggers.values())
        return [t for t in self._all_triggers.values() if t.table.lower() == table.lower()]

    def enable(self, name: str) -> bool:
        trigger = self._all_triggers.get(name.lower())
        if trigger:
            trigger.enabled = True
            return True
        return False

    def disable(self, name: str) -> bool:
        trigger = self._all_triggers.get(name.lower())
        if trigger:
            trigger.enabled = False
            return True
        return False


class TriggerExecutor:
    """Fires triggers around DML operations.

    Usage:
        executor = TriggerExecutor(catalog)

        # Before insert
        ctx = TriggerContext(table="articles", event=TriggerEvent.INSERT,
                            timing=TriggerTiming.BEFORE, new_row=row_data)
        executor.fire_before(ctx)
        if ctx.cancelled:
            return  # Operation cancelled by trigger

        # ... perform the actual insert ...

        # After insert
        ctx.timing = TriggerTiming.AFTER
        executor.fire_after(ctx)
    """

    def __init__(self, catalog: TriggerCatalog) -> None:
        self._catalog = catalog
        self._fire_count = 0
        self._fire_log: list[dict[str, Any]] = []

    @property
    def fire_count(self) -> int:
        return self._fire_count

    @property
    def fire_log(self) -> list[dict[str, Any]]:
        return list(self._fire_log)

    def fire_before(self, ctx: TriggerContext) -> TriggerContext:
        """Fire all matching BEFORE triggers. Modifies ctx in place."""
        ctx.timing = TriggerTiming.BEFORE
        triggers = self._catalog.get_triggers(ctx.table, ctx.event, TriggerTiming.BEFORE)
        for trigger in triggers:
            if ctx.cancelled:
                break
            if trigger.when and not trigger.when(ctx):
                continue
            trigger.action(ctx)
            self._fire_count += 1
            self._fire_log.append({
                "trigger": trigger.name,
                "table": ctx.table,
                "event": ctx.event.value,
                "timing": "before",
                "ts": time.time(),
            })
        return ctx

    def fire_after(self, ctx: TriggerContext) -> None:
        """Fire all matching AFTER triggers."""
        ctx.timing = TriggerTiming.AFTER
        triggers = self._catalog.get_triggers(ctx.table, ctx.event, TriggerTiming.AFTER)
        for trigger in triggers:
            if trigger.when and not trigger.when(ctx):
                continue
            trigger.action(ctx)
            self._fire_count += 1
            self._fire_log.append({
                "trigger": trigger.name,
                "table": ctx.table,
                "event": ctx.event.value,
                "timing": "after",
                "ts": time.time(),
            })
