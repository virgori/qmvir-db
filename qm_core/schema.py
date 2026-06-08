"""QM Schema — Table Constraints, Data Types, and Catalog.

Provides:
    - Column types with validation
    - Constraints: PRIMARY KEY, UNIQUE, NOT NULL, CHECK, FOREIGN KEY, DEFAULT
    - Table catalog (system-level schema registry)
    - Constraint enforcement on insert/update/delete
"""

from __future__ import annotations

import time
from dataclasses import dataclass, field
from enum import IntEnum, auto
from typing import Any, Callable


# ── Data Types ──────────────────────────────────────────────────────

class DataType(IntEnum):
    INTEGER = auto()
    BIGINT = auto()
    FLOAT = auto()
    DOUBLE = auto()
    TEXT = auto()
    VARCHAR = auto()
    BOOLEAN = auto()
    BLOB = auto()
    TIMESTAMP = auto()
    JSON = auto()
    VECTOR = auto()  # Fixed-dim float array

    @staticmethod
    def from_str(s: str) -> DataType:
        mapping = {
            "INT": DataType.INTEGER, "INTEGER": DataType.INTEGER,
            "BIGINT": DataType.BIGINT,
            "FLOAT": DataType.FLOAT, "REAL": DataType.FLOAT,
            "DOUBLE": DataType.DOUBLE,
            "TEXT": DataType.TEXT, "STRING": DataType.TEXT,
            "VARCHAR": DataType.VARCHAR,
            "BOOL": DataType.BOOLEAN, "BOOLEAN": DataType.BOOLEAN,
            "BLOB": DataType.BLOB, "BYTES": DataType.BLOB,
            "TIMESTAMP": DataType.TIMESTAMP, "DATETIME": DataType.TIMESTAMP,
            "JSON": DataType.JSON, "JSONB": DataType.JSON,
            "VECTOR": DataType.VECTOR,
        }
        return mapping.get(s.upper(), DataType.TEXT)


def validate_type(value: Any, dtype: DataType) -> bool:
    """Check if a value matches the expected data type."""
    if value is None:
        return True  # NULL is always type-valid; nullability checked separately
    if dtype in (DataType.INTEGER, DataType.BIGINT):
        return isinstance(value, int)
    if dtype in (DataType.FLOAT, DataType.DOUBLE):
        return isinstance(value, (int, float))
    if dtype in (DataType.TEXT, DataType.VARCHAR):
        return isinstance(value, str)
    if dtype == DataType.BOOLEAN:
        return isinstance(value, bool)
    if dtype == DataType.BLOB:
        return isinstance(value, (bytes, bytearray))
    if dtype == DataType.TIMESTAMP:
        return isinstance(value, (int, float))
    return True  # JSON, VECTOR — accept anything


# ── Constraints ─────────────────────────────────────────────────────

class ConstraintType(IntEnum):
    PRIMARY_KEY = auto()
    UNIQUE = auto()
    NOT_NULL = auto()
    CHECK = auto()
    FOREIGN_KEY = auto()
    DEFAULT = auto()


@dataclass
class ColumnSchema:
    """Schema definition for a single column."""
    name: str
    dtype: DataType
    nullable: bool = True
    primary_key: bool = False
    unique: bool = False
    default: Any = None
    check_expr: Callable[[Any], bool] | None = None
    references: tuple[str, str] | None = None  # (table, column)


@dataclass
class TableConstraint:
    """Table-level constraint (e.g., composite PK, composite UNIQUE)."""
    ctype: ConstraintType
    columns: list[str]
    name: str | None = None
    check_fn: Callable[[dict[str, Any]], bool] | None = None
    ref_table: str | None = None
    ref_columns: list[str] | None = None


@dataclass
class TableDef:
    """Full table schema definition."""
    name: str
    columns: list[ColumnSchema]
    constraints: list[TableConstraint] = field(default_factory=list)
    created_ts: float = field(default_factory=time.time)

    @property
    def column_names(self) -> list[str]:
        return [c.name for c in self.columns]

    @property
    def primary_key_columns(self) -> list[str]:
        # Column-level PK
        pk_cols = [c.name for c in self.columns if c.primary_key]
        # Table-level PK
        for tc in self.constraints:
            if tc.ctype == ConstraintType.PRIMARY_KEY:
                pk_cols.extend(tc.columns)
        return list(dict.fromkeys(pk_cols))  # Deduplicate, preserve order

    def get_column(self, name: str) -> ColumnSchema | None:
        for c in self.columns:
            if c.name == name:
                return c
        return None


class ConstraintViolation(Exception):
    """Raised when an insert/update violates a constraint."""
    pass


class ConstraintChecker:
    """Validates rows against table schema constraints.

    Checks:
        1. NOT NULL — column must not be None
        2. Type validation — value matches declared type
        3. PRIMARY KEY — not null + unique
        4. UNIQUE — no duplicate values in column(s)
        5. CHECK — arbitrary expression must return True
        6. FOREIGN KEY — referenced row must exist
        7. DEFAULT — fill in missing columns
    """

    def __init__(self, table_def: TableDef) -> None:
        self._table = table_def
        # Track unique indexes: column_name → set of existing values
        self._unique_index: dict[str, set[Any]] = {}
        # Composite unique indexes: tuple of column names → set of value tuples
        self._composite_unique: dict[tuple[str, ...], set[tuple]] = {}

        # Initialize unique tracking
        for col in table_def.columns:
            if col.primary_key or col.unique:
                self._unique_index[col.name] = set()
        for tc in table_def.constraints:
            if tc.ctype in (ConstraintType.UNIQUE, ConstraintType.PRIMARY_KEY):
                key = tuple(tc.columns)
                self._composite_unique[key] = set()

    def apply_defaults(self, row: dict[str, Any]) -> dict[str, Any]:
        """Fill in default values for missing columns."""
        result = dict(row)
        for col in self._table.columns:
            if col.name not in result or result[col.name] is None:
                if col.default is not None:
                    result[col.name] = col.default() if callable(col.default) else col.default
        return result

    def validate_insert(self, row: dict[str, Any]) -> None:
        """Validate a row before insertion. Raises ConstraintViolation."""
        row = self.apply_defaults(row)
        self._check_types(row)
        self._check_not_null(row)
        self._check_unique(row)
        self._check_column_checks(row)
        self._check_table_checks(row)

    def register_row(self, row: dict[str, Any]) -> None:
        """Register a successfully inserted row in unique indexes."""
        for col_name, index in self._unique_index.items():
            val = row.get(col_name)
            if val is not None:
                index.add(val)
        for cols, index in self._composite_unique.items():
            vals = tuple(row.get(c) for c in cols)
            index.add(vals)

    def unregister_row(self, row: dict[str, Any]) -> None:
        """Remove a row from unique indexes (for delete/update)."""
        for col_name, index in self._unique_index.items():
            val = row.get(col_name)
            index.discard(val)
        for cols, index in self._composite_unique.items():
            vals = tuple(row.get(c) for c in cols)
            index.discard(vals)

    def validate_update(self, old_row: dict[str, Any], new_row: dict[str, Any]) -> None:
        """Validate an update. Temporarily unregister old, check new, re-register."""
        self.unregister_row(old_row)
        try:
            new_full = self.apply_defaults(new_row)
            self._check_types(new_full)
            self._check_not_null(new_full)
            self._check_unique(new_full)
            self._check_column_checks(new_full)
            self._check_table_checks(new_full)
        except ConstraintViolation:
            self.register_row(old_row)
            raise
        self.register_row(new_full)

    def _check_types(self, row: dict[str, Any]) -> None:
        for col in self._table.columns:
            val = row.get(col.name)
            if val is not None and not validate_type(val, col.dtype):
                raise ConstraintViolation(
                    f"Column '{col.name}': expected {col.dtype.name}, "
                    f"got {type(val).__name__}"
                )

    def _check_not_null(self, row: dict[str, Any]) -> None:
        for col in self._table.columns:
            if not col.nullable and row.get(col.name) is None:
                raise ConstraintViolation(
                    f"Column '{col.name}' cannot be NULL"
                )

    def _check_unique(self, row: dict[str, Any]) -> None:
        for col_name, index in self._unique_index.items():
            val = row.get(col_name)
            if val is not None and val in index:
                raise ConstraintViolation(
                    f"Duplicate value '{val}' for unique column '{col_name}'"
                )
        for cols, index in self._composite_unique.items():
            vals = tuple(row.get(c) for c in cols)
            if all(v is not None for v in vals) and vals in index:
                raise ConstraintViolation(
                    f"Duplicate composite key {cols}={vals}"
                )

    def _check_column_checks(self, row: dict[str, Any]) -> None:
        for col in self._table.columns:
            if col.check_expr is not None:
                val = row.get(col.name)
                if val is not None and not col.check_expr(val):
                    raise ConstraintViolation(
                        f"CHECK constraint failed for column '{col.name}'"
                    )

    def _check_table_checks(self, row: dict[str, Any]) -> None:
        for tc in self._table.constraints:
            if tc.ctype == ConstraintType.CHECK and tc.check_fn is not None:
                if not tc.check_fn(row):
                    raise ConstraintViolation(
                        f"Table CHECK constraint '{tc.name or 'unnamed'}' failed"
                    )


# ── Catalog ─────────────────────────────────────────────────────────

class Catalog:
    """System catalog — registry of all table schemas and their constraints."""

    def __init__(self) -> None:
        self._tables: dict[str, TableDef] = {}
        self._checkers: dict[str, ConstraintChecker] = {}

    def create_table(self, table_def: TableDef) -> None:
        """Register a new table schema."""
        if table_def.name in self._tables:
            raise ValueError(f"Table '{table_def.name}' already exists")
        self._tables[table_def.name] = table_def
        self._checkers[table_def.name] = ConstraintChecker(table_def)

    def drop_table(self, name: str) -> None:
        self._tables.pop(name, None)
        self._checkers.pop(name, None)

    def get_table(self, name: str) -> TableDef | None:
        return self._tables.get(name)

    def get_checker(self, name: str) -> ConstraintChecker | None:
        return self._checkers.get(name)

    def table_exists(self, name: str) -> bool:
        return name in self._tables

    @property
    def table_names(self) -> list[str]:
        return list(self._tables.keys())

    def get_foreign_key_targets(self, table: str) -> list[tuple[str, str, str, str]]:
        """Return FK relationships: [(from_col, ref_table, ref_col, constraint_name)]"""
        td = self._tables.get(table)
        if not td:
            return []
        fks: list[tuple[str, str, str, str]] = []
        for col in td.columns:
            if col.references:
                ref_table, ref_col = col.references
                fks.append((col.name, ref_table, ref_col, f"fk_{col.name}"))
        for tc in td.constraints:
            if tc.ctype == ConstraintType.FOREIGN_KEY and tc.ref_table and tc.ref_columns:
                for i, col in enumerate(tc.columns):
                    ref_col = tc.ref_columns[i] if i < len(tc.ref_columns) else tc.ref_columns[0]
                    fks.append((col, tc.ref_table, ref_col, tc.name or f"fk_{col}"))
        return fks
