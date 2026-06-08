"""QM Core DB — Schema definitions, constraints, and collection registry.

Manages table/collection definitions with:
  - Column types and constraints (PK, FK, unique, check)
  - Indexes (B-tree, composite, partial, covering)
  - Partitioning rules
  - Audit fields (created_at, updated_at, version)
  - Multi-tenant isolation (tenant_id)
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class ColumnType(Enum):
    """Supported column data types."""

    INT32 = "int32"
    INT64 = "int64"
    FLOAT32 = "float32"
    FLOAT64 = "float64"
    TEXT = "text"
    VARCHAR = "varchar"
    BOOLEAN = "boolean"
    TIMESTAMP = "timestamp"
    UUID = "uuid"
    JSONB = "jsonb"
    BLOB = "blob"
    ENUM = "enum"
    ARRAY = "array"


class ConstraintType(Enum):
    """Column / table constraint types."""

    PRIMARY_KEY = "primary_key"
    FOREIGN_KEY = "foreign_key"
    UNIQUE = "unique"
    NOT_NULL = "not_null"
    CHECK = "check"
    DEFAULT = "default"


class IndexType(Enum):
    """Index types for the schema registry."""

    BTREE = "btree"
    HASH = "hash"
    BITMAP = "bitmap"
    INVERTED = "inverted"
    VECTOR = "vector"
    COMPOSITE = "composite"


class PartitionStrategy(Enum):
    """Partition strategies."""

    RANGE = "range"
    HASH = "hash"
    LIST = "list"


@dataclass
class ColumnDef:
    """Column definition."""

    name: str
    col_type: ColumnType
    nullable: bool = True
    default: Any = None
    constraints: list[ConstraintType] = field(default_factory=list)
    enum_values: list[str] | None = None
    max_length: int | None = None


@dataclass
class IndexDef:
    """Index definition."""

    name: str
    columns: list[str]
    index_type: IndexType = IndexType.BTREE
    unique: bool = False
    partial_filter: str | None = None  # e.g. "is_deleted = false"
    include_columns: list[str] | None = None  # covering index columns


@dataclass
class ForeignKeyDef:
    """Foreign key definition."""

    name: str
    columns: list[str]
    ref_table: str
    ref_columns: list[str]
    on_delete: str = "RESTRICT"  # CASCADE, SET NULL, RESTRICT
    on_update: str = "CASCADE"


@dataclass
class PartitionDef:
    """Partition definition."""

    strategy: PartitionStrategy
    partition_key: str
    partitions: list[dict[str, Any]] = field(default_factory=list)


@dataclass
class TableSchema:
    """Full table/collection schema definition."""

    name: str
    columns: list[ColumnDef]
    indexes: list[IndexDef] = field(default_factory=list)
    foreign_keys: list[ForeignKeyDef] = field(default_factory=list)
    partition: PartitionDef | None = None
    multi_tenant: bool = True  # auto-add tenant_id
    soft_delete: bool = True  # auto-add is_deleted + deleted_at
    audit_fields: bool = True  # auto-add created_at, updated_at, version

    def get_primary_key(self) -> list[str]:
        """Return primary key columns."""
        for idx in self.indexes:
            if idx.unique and idx.index_type == IndexType.BTREE:
                return idx.columns
        # Fallback: look at column constraints
        return [c.name for c in self.columns if ConstraintType.PRIMARY_KEY in c.constraints]


class SchemaRegistry:
    """Registry for all table schemas in the database."""

    def __init__(self) -> None:
        self._schemas: dict[str, TableSchema] = {}

    def register(self, schema: TableSchema) -> None:
        """Register a table schema."""
        # Auto-add tenant_id if multi-tenant
        if schema.multi_tenant:
            col_names = {c.name for c in schema.columns}
            if "tenant_id" not in col_names:
                schema.columns.insert(
                    0,
                    ColumnDef(
                        name="tenant_id",
                        col_type=ColumnType.UUID,
                        nullable=False,
                        constraints=[ConstraintType.NOT_NULL],
                    ),
                )

        # Auto-add audit fields
        if schema.audit_fields:
            col_names = {c.name for c in schema.columns}
            for audit_col, audit_type in [
                ("created_at", ColumnType.TIMESTAMP),
                ("updated_at", ColumnType.TIMESTAMP),
                ("version", ColumnType.INT64),
            ]:
                if audit_col not in col_names:
                    schema.columns.append(
                        ColumnDef(name=audit_col, col_type=audit_type, nullable=False)
                    )

        # Auto-add soft delete fields
        if schema.soft_delete:
            col_names = {c.name for c in schema.columns}
            if "is_deleted" not in col_names:
                schema.columns.append(
                    ColumnDef(
                        name="is_deleted",
                        col_type=ColumnType.BOOLEAN,
                        nullable=False,
                        default=False,
                    )
                )
            if "deleted_at" not in col_names:
                schema.columns.append(
                    ColumnDef(name="deleted_at", col_type=ColumnType.TIMESTAMP, nullable=True)
                )

        self._schemas[schema.name] = schema

    def get(self, name: str) -> TableSchema | None:
        return self._schemas.get(name)

    def list_tables(self) -> list[str]:
        return list(self._schemas.keys())

    def validate_data(self, table: str, data: dict[str, Any]) -> list[str]:
        """Validate data against schema constraints. Returns list of errors."""
        schema = self._schemas.get(table)
        if not schema:
            return [f"Table '{table}' not found"]

        errors: list[str] = []
        col_map = {c.name: c for c in schema.columns}

        for col in schema.columns:
            if ConstraintType.NOT_NULL in col.constraints and col.name not in data:
                if col.default is None:
                    errors.append(f"Column '{col.name}' is required")

        for key in data:
            if key not in col_map:
                errors.append(f"Unknown column '{key}'")

        return errors
