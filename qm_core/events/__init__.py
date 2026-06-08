"""QM Phase 11 — Event-Driven Pipelines.

Provides:
    - EventBus — publish/subscribe for database events
    - EventPipeline — composable filter → transform → sink chains
    - CDC integration via event subscriptions
"""

from qm_core.events.bus import EventBus, Event, EventType
from qm_core.events.pipeline import EventPipeline, PipelineStage

__all__ = [
    "EventBus",
    "Event",
    "EventType",
    "EventPipeline",
    "PipelineStage",
]
