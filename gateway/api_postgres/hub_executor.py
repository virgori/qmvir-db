"""Adapter from hub engine result objects to PostgreSQL wire rows."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any


def make_hub_executor(engine: Any) -> Callable[[str], tuple[list[str], list[list[Any]]]]:
    """Return a simple query executor backed by ``engine.execute_sql``."""

    def execute(sql: str) -> tuple[list[str], list[list[Any]]]:
        result = engine.execute_sql(sql)
        if not result:
            return [], []
        if isinstance(result, tuple) and len(result) >= 2:
            return result[0], result[1]
        if isinstance(result, list) and isinstance(result[0], dict):
            cols = list(result[0].keys())
            rows = [[row.get(col) for col in cols] for row in result]
            return cols, rows
        return ["result"], [[item] for item in result]

    return execute
