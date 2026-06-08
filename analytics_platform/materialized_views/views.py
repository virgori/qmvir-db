"""QM Analytics Platform — Materialized Views.

Pre-computed aggregation results for dashboards:
  - Top items
  - Counts / sums / averages
  - Rankings
  - Cohort tables
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from typing import Any, Callable


@dataclass
class MaterializedView:
    """A materialized (pre-computed) view."""

    name: str
    source_dataset: str
    query: dict[str, Any]  # The aggregation query definition
    result: list[dict[str, Any]] = field(default_factory=list)
    last_refreshed: float = 0.0
    refresh_interval_s: float = 60.0
    is_stale: bool = True

    @property
    def needs_refresh(self) -> bool:
        if self.is_stale:
            return True
        return (time.time() - self.last_refreshed) > self.refresh_interval_s


class MaterializedViewManager:
    """Manages materialized views with periodic refresh."""

    def __init__(self) -> None:
        self._views: dict[str, MaterializedView] = {}
        self._refresh_fn: Callable[[dict[str, Any]], list[dict[str, Any]]] | None = None

    def set_refresh_function(
        self, fn: Callable[[dict[str, Any]], list[dict[str, Any]]]
    ) -> None:
        """Set the function used to compute view results."""
        self._refresh_fn = fn

    def create_view(
        self,
        name: str,
        source_dataset: str,
        query: dict[str, Any],
        refresh_interval_s: float = 60.0,
    ) -> MaterializedView:
        view = MaterializedView(
            name=name,
            source_dataset=source_dataset,
            query=query,
            refresh_interval_s=refresh_interval_s,
        )
        self._views[name] = view
        return view

    def get(self, name: str) -> list[dict[str, Any]] | None:
        """Get materialized view result. Auto-refresh if stale."""
        view = self._views.get(name)
        if not view:
            return None

        if view.needs_refresh and self._refresh_fn:
            view.result = self._refresh_fn(view.query)
            view.last_refreshed = time.time()
            view.is_stale = False

        return view.result

    def invalidate(self, name: str) -> None:
        """Mark a view as stale for next read."""
        view = self._views.get(name)
        if view:
            view.is_stale = True

    def invalidate_by_dataset(self, dataset: str) -> int:
        """Invalidate all views derived from a dataset."""
        count = 0
        for view in self._views.values():
            if view.source_dataset == dataset:
                view.is_stale = True
                count += 1
        return count

    def list_views(self) -> list[str]:
        return list(self._views.keys())
