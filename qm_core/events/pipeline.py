"""QM Event Pipeline — Composable filter → transform → sink chains.

Build CDC (Change Data Capture) or ETL-like pipelines from database events.

Usage:
    pipeline = EventPipeline("cdc_articles")
    pipeline.filter(lambda e: e.table == "articles")
    pipeline.transform(lambda e: {"id": e.key, "action": e.event_type.value})
    pipeline.sink(lambda record: webhook_post(record))
    pipeline.attach(bus)   # starts consuming events
    pipeline.detach()      # stops
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable

from .bus import Event, EventBus, EventType


class StageKind(Enum):
    FILTER = "filter"
    TRANSFORM = "transform"
    SINK = "sink"


@dataclass
class PipelineStage:
    """A single processing stage in an event pipeline."""
    kind: StageKind
    fn: Callable
    name: str = ""

    def __repr__(self) -> str:
        return f"PipelineStage({self.kind.value}, {self.name or '?'})"


class EventPipeline:
    """Composable event processing pipeline.

    Events flow: source → filter* → transform* → sink*
    Filters drop events that return False.
    Transforms map an Event to an arbitrary dict/record.
    Sinks consume the final record.
    """

    def __init__(self, name: str = "pipeline") -> None:
        self.name = name
        self._stages: list[PipelineStage] = []
        self._bus: EventBus | None = None
        self._sub_id: int | None = None
        self._processed = 0
        self._dropped = 0
        self._errors: list[tuple[str, Exception]] = []

    # ── builder API ───────────────────────────────────

    def filter(self, fn: Callable[[Event], bool], *, name: str = "") -> "EventPipeline":
        """Add a filter stage. Events where fn returns False are dropped."""
        self._stages.append(PipelineStage(StageKind.FILTER, fn, name=name or f"filter_{len(self._stages)}"))
        return self

    def transform(self, fn: Callable[[Event | dict], Any], *, name: str = "") -> "EventPipeline":
        """Add a transform stage. Maps the current record to a new form."""
        self._stages.append(PipelineStage(StageKind.TRANSFORM, fn, name=name or f"transform_{len(self._stages)}"))
        return self

    def sink(self, fn: Callable[[Any], None], *, name: str = "") -> "EventPipeline":
        """Add a sink stage. Consumes the final record."""
        self._stages.append(PipelineStage(StageKind.SINK, fn, name=name or f"sink_{len(self._stages)}"))
        return self

    # ── lifecycle ─────────────────────────────────────

    def attach(self, bus: EventBus, **subscribe_kwargs: Any) -> int:
        """Attach this pipeline to an EventBus. Returns the subscription ID."""
        self._bus = bus
        self._sub_id = bus.subscribe(callback=self._on_event, **subscribe_kwargs)
        return self._sub_id

    def detach(self) -> None:
        """Detach from the event bus."""
        if self._bus and self._sub_id is not None:
            self._bus.unsubscribe(self._sub_id)
            self._sub_id = None
            self._bus = None

    # ── processing ────────────────────────────────────

    def process(self, event: Event) -> Any | None:
        """Manually push an event through the pipeline. Returns final record or None if filtered."""
        record: Any = event
        for stage in self._stages:
            if stage.kind == StageKind.FILTER:
                if not stage.fn(record):
                    self._dropped += 1
                    return None
            elif stage.kind == StageKind.TRANSFORM:
                record = stage.fn(record)
            elif stage.kind == StageKind.SINK:
                stage.fn(record)

        self._processed += 1
        return record

    def _on_event(self, event: Event) -> None:
        """Internal callback invoked by the EventBus."""
        try:
            self.process(event)
        except Exception as exc:
            self._errors.append((self.name, exc))

    # ── introspection ─────────────────────────────────

    @property
    def stages(self) -> list[PipelineStage]:
        return list(self._stages)

    @property
    def processed(self) -> int:
        return self._processed

    @property
    def dropped(self) -> int:
        return self._dropped

    @property
    def errors(self) -> list[tuple[str, Exception]]:
        return list(self._errors)

    @property
    def is_attached(self) -> bool:
        return self._bus is not None and self._sub_id is not None
