"""QM Phase 11 — Trigger System.

Provides:
    - BEFORE/AFTER triggers on INSERT, UPDATE, DELETE
    - Row-level and statement-level triggers
    - Trigger catalog for registration and management
    - TriggerExecutor for firing triggers around DML operations
"""

from qm_core.triggers.trigger import (
    Trigger,
    TriggerEvent,
    TriggerTiming,
    TriggerLevel,
    TriggerCatalog,
    TriggerExecutor,
    TriggerContext,
)

__all__ = [
    "Trigger",
    "TriggerEvent",
    "TriggerTiming",
    "TriggerLevel",
    "TriggerCatalog",
    "TriggerExecutor",
    "TriggerContext",
]
