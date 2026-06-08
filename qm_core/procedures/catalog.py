"""QM Stored Procedure Catalog — Registration, lookup, execution.

Provides:
    - StoredProcedure definitions with parameter metadata
    - ProcedureCatalog for registration and execution
    - Input validation and type coercion
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable

from qm_core.procedures.plqm import PLQMInterpreter, PLQMError


class ParamType(Enum):
    """Supported parameter types for stored procedures."""
    TEXT = "text"
    INT = "int"
    FLOAT = "float"
    BOOL = "bool"
    ANY = "any"


@dataclass
class ProcedureParam:
    """A single parameter of a stored procedure."""
    name: str
    type: ParamType = ParamType.ANY
    default: Any = None
    required: bool = True


@dataclass
class StoredProcedure:
    """Registered stored procedure."""
    name: str
    params: list[ProcedureParam]
    body: str  # PL/QM source code
    language: str = "plqm"
    owner: str = "system"
    created_at: float = field(default_factory=time.time)
    description: str = ""
    volatile: bool = True  # True = may modify data, False = read-only

    def validate_args(self, args: dict[str, Any]) -> dict[str, Any]:
        """Validate and coerce arguments against parameter definitions."""
        result: dict[str, Any] = {}
        for param in self.params:
            if param.name in args:
                result[param.name] = self._coerce(args[param.name], param.type)
            elif param.default is not None:
                result[param.name] = param.default
            elif param.required:
                raise PLQMError(f"Missing required parameter: {param.name}")
        return result

    @staticmethod
    def _coerce(value: Any, target: ParamType) -> Any:
        if target == ParamType.ANY:
            return value
        if target == ParamType.TEXT:
            return str(value)
        if target == ParamType.INT:
            return int(value)
        if target == ParamType.FLOAT:
            return float(value)
        if target == ParamType.BOOL:
            return bool(value)
        return value


class ProcedureCatalog:
    """Catalog for stored procedures — register, lookup, execute.

    Usage:
        catalog = ProcedureCatalog()
        catalog.register(StoredProcedure(
            name="inc_counter",
            params=[ProcedureParam("name", ParamType.TEXT)],
            body='DECLARE v INT = db_read("counters", name); ...',
        ))
        result = catalog.call("inc_counter", {"name": "visits"})
    """

    def __init__(self) -> None:
        self._procedures: dict[str, StoredProcedure] = {}
        self._interpreter = PLQMInterpreter()
        self._exec_count: dict[str, int] = {}
        self._exec_time_ms: dict[str, float] = {}

    @property
    def interpreter(self) -> PLQMInterpreter:
        return self._interpreter

    def register(self, proc: StoredProcedure) -> None:
        """Register a stored procedure."""
        self._procedures[proc.name.lower()] = proc
        self._exec_count[proc.name.lower()] = 0
        self._exec_time_ms[proc.name.lower()] = 0.0

    def drop(self, name: str) -> bool:
        """Drop a stored procedure. Returns True if it existed."""
        key = name.lower()
        if key in self._procedures:
            del self._procedures[key]
            return True
        return False

    def get(self, name: str) -> StoredProcedure | None:
        """Look up a stored procedure by name."""
        return self._procedures.get(name.lower())

    def list_procedures(self) -> list[StoredProcedure]:
        """List all registered procedures."""
        return list(self._procedures.values())

    def call(self, name: str, args: dict[str, Any] | None = None) -> Any:
        """Execute a stored procedure by name with arguments."""
        proc = self._procedures.get(name.lower())
        if proc is None:
            raise PLQMError(f"Procedure not found: {name}")

        validated = proc.validate_args(args or {})
        start = time.monotonic()

        result = self._interpreter.execute(proc.body, params=validated)

        elapsed = (time.monotonic() - start) * 1000
        key = name.lower()
        self._exec_count[key] = self._exec_count.get(key, 0) + 1
        self._exec_time_ms[key] = self._exec_time_ms.get(key, 0) + elapsed

        return result

    def stats(self) -> dict[str, dict[str, Any]]:
        """Get execution statistics for all procedures."""
        return {
            name: {
                "exec_count": self._exec_count.get(name, 0),
                "total_time_ms": round(self._exec_time_ms.get(name, 0), 3),
            }
            for name in self._procedures
        }
