"""QM Hub Engine — Architecture-compliant engine facade.

``QMHubEngine`` is a drop-in replacement for ``QMEngine`` that routes
ALL data operations through the Hub → Ring → Satellite IPC path.

Control Plane (this process):
    - SQL parsing, AST building, query planning
    - LSN sequencing, Merkle root auditing
    - Hub dispatch/collect

Data Plane (satellite processes):
    - Row storage, index maintenance (GeneralSatellite)
    - Vector HNSW search, DiskANN, XOR-Delta compression (VectorSatellite)
    - PL/QM procedure execution (ProcedureSatellite)

Usage:
    engine = QMHubEngine(data_dir="/tmp/qmdata")
    engine.create_table("users", {"name": "text", "age": "int"})
    engine.insert("users", {"name": "Alice", "age": 30})
    results = engine.execute_sql("SELECT * FROM users WHERE age > 25")
"""

from __future__ import annotations

import hashlib
import json
import os
import struct
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import msgpack

try:
    import qm_engine as _qm_engine
except Exception:
    _qm_engine = None

from qm_core.hub.dispatcher import HubDispatcher, DispatcherConfig
from qm_core.hub.hub import Hub
from qm_core.hub.lsn_sequencer import LSNStamp
from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType

from qm_core.satellite.base import SatelliteConfig
from qm_core.satellite.general_satellite import GeneralSatellite
from qm_core.satellite.vector_satellite import VectorSatellite, _pack_vector, _pack_search
from qm_core.satellite.procedure_satellite import ProcedureSatellite

# Execution kernel (control plane — parsing + planning only)
from qm_core.execution.parser import QueryParser, QueryAST
from qm_core.execution.planner import QueryPlanner, CostModel
from qm_core.execution.pipeline import (
    PipelineContext, ScoredCandidate, RRFFusionStage,
)
from qm_core.execution.sql_parser import (
    SQLParser, SelectStmt, InsertStmt, UpdateStmt, DeleteStmt, CreateTableStmt,
    ColumnRef, Literal, BinaryOp, UnaryOp, FunctionCall, StarExpr, AliasedExpr,
    InList, BetweenExpr, LikeExpr, IsNullExpr, WindowExpr, SQLSyntaxError,
)
from qm_core.execution.join import (
    Operator, ScanOperator, FilterOperator, ProjectOperator, LimitOperator,
    HashJoin, MergeJoin, NestedLoopJoin, SortOperator, HashAggregateOperator,
    JoinType, Row,
)
from qm_core.execution.window import (
    WindowOperator, WindowSpec, CTEOperator, DistinctOperator,
)
from qm_core.execution.vectorized import ColumnBatch, VecHashAggregate

# Statistics / optimizer (control plane)
from qm_core.statistics.cost_model import CostModelV2, TableCatalog, TableMeta, IndexMeta
from qm_core.statistics.sketches import ColumnSketch
from qm_core.index.stats import StatsCollector
from qm_core.optimizer.adaptive import AdaptiveExecutor, PlanHistory, RuleOptimizer
from qm_core.learned.assistants import (
    LearnedSelectivity, LearnedCachePolicy, LearnedFusionWeights,
    QueryIntentClassifier,
)


@dataclass
class _HubTableMeta:
    """Hub-side metadata for a table (no data, no indexes)."""
    schema: dict[str, str]
    primary_key: str = "_id"
    vector_dim: int | None = None
    row_count: int = 0
    index_names: list[str] | None = None


class QMHubEngine:
    """Hub-Satellite compliant QM engine.

    All data operations route through Hub IPC. The engine process
    only holds metadata (schemas, stats, plans) — never raw data.
    """

    VERSION = "2.0.0-hub"

    def __init__(self, data_dir: str | None = None, wal_enabled: bool = True) -> None:
        self._data_dir = Path(data_dir) if data_dir else Path(tempfile.mkdtemp(prefix="qmhub_"))
        self._data_dir.mkdir(parents=True, exist_ok=True)

        # ── Control Plane setup ─────────────────────────────────────
        ring_dir = str(self._data_dir / "rings")
        wal_path = str(self._data_dir / "hub_wal.bin") if wal_enabled else None

        self._dispatcher = HubDispatcher(DispatcherConfig(
            ring_dir=ring_dir,
            wal_path=wal_path,
        ))

        # ── In-process satellites (thread-based for single-node) ────
        # Each satellite has its own dedicated ring buffer
        sat_dir = str(self._data_dir / "satellites")
        self._gen_sat = GeneralSatellite(
            SatelliteConfig(satellite_id="gen-0", data_dir=sat_dir),
            self._dispatcher.ring_gen,
        )
        self._vec_sat = VectorSatellite(
            SatelliteConfig(satellite_id="vec-0", data_dir=sat_dir),
            self._dispatcher.ring_vec,
            dim=128,
        )
        self._proc_sat = ProcedureSatellite(
            SatelliteConfig(satellite_id="plqm-0", data_dir=sat_dir),
            self._dispatcher.ring_proc,
        )

        # Start satellite poll loops
        self._gen_sat.start()
        self._vec_sat.start()
        self._proc_sat.start()

        # ── Hub-side metadata (NO data, NO indexes) ────────────────
        self._tables: dict[str, _HubTableMeta] = {}

        # Execution (control plane only)
        self._parser = QueryParser()
        self._stats_collector = StatsCollector()
        self._cost_model = CostModel()
        self._planner = QueryPlanner(stats=self._stats_collector, cost_model=self._cost_model)

        # Optimizer
        self._plan_history = PlanHistory()
        self._rule_optimizer = RuleOptimizer()
        self._adaptive = AdaptiveExecutor(history=self._plan_history)

        # Learned
        self._learned_selectivity = LearnedSelectivity()
        self._learned_cache = LearnedCachePolicy()
        self._learned_fusion = LearnedFusionWeights()
        self._intent_classifier = QueryIntentClassifier()

        # Cost model V2
        self._catalog = TableCatalog()
        self._cost_v2 = CostModelV2(catalog=self._catalog)

    def close(self) -> None:
        """Shut down satellites and dispatcher."""
        self._gen_sat.stop()
        self._vec_sat.stop()
        self._proc_sat.stop()
        self._dispatcher.close()

    # ── DDL ─────────────────────────────────────────────────────────

    def create_table(self, name: str, schema: dict[str, str],
                     primary_key: str = "_id", vector_dim: int | None = None) -> None:
        if name in self._tables:
            raise ValueError(f"Table '{name}' already exists")

        # Hub-side metadata only
        self._tables[name] = _HubTableMeta(
            schema=schema, primary_key=primary_key, vector_dim=vector_dim,
            index_names=[],
        )
        self._catalog.register(name, TableMeta())

        # Dispatch DDL to satellite via Hub IPC
        result = self._dispatcher.create_table(name, schema)
        if not result.success:
            raise RuntimeError(f"Satellite DDL failed: {result.data}")

    def drop_table(self, name: str) -> bool:
        if name not in self._tables:
            return False
        del self._tables[name]
        self._dispatcher.drop_table(name)
        return True

    def create_index(self, table: str, index_name: str, columns: list[str],
                     index_type: str = "btree") -> None:
        meta = self._tables.get(table)
        if not meta:
            raise ValueError(f"Table '{table}' does not exist")

        # Dispatch index creation to satellite
        payload = msgpack.packb({
            "table": table,
            "action": "create_index",
            "index_name": index_name,
            "columns": columns,
            "index_type": index_type,
        })
        result = self._dispatcher.hub.dispatch_sync(
            CommandType.DDL, table, payload,
            timeout_ms=5000,
        )
        if meta.index_names is not None:
            meta.index_names.append(index_name)

        # Update catalog
        cat = self._catalog.get(table) or TableMeta()
        cat.indexes[index_name] = IndexMeta(columns=columns)
        self._catalog.register(table, cat)

    # ── DML — All via Hub IPC ───────────────────────────────────────

    def insert(self, table: str, doc: dict[str, Any]) -> int:
        meta = self._tables.get(table)
        if not meta:
            raise ValueError(f"Table '{table}' does not exist")

        result = self._dispatcher.insert(table, doc)
        if not result.success:
            raise RuntimeError(f"Insert failed: {result.data}")

        # Decode result to get row_id
        try:
            resp = msgpack.unpackb(result.data, raw=False)
            row_id = resp.get("row_id", 0)
        except Exception:
            row_id = 0

        meta.row_count += 1

        # Update catalog
        cat = self._catalog.get(table) or TableMeta()
        cat.row_count = meta.row_count
        cat.page_count = max(1, meta.row_count // 80)
        self._catalog.register(table, cat)

        return row_id

    def insert_batch(self, table: str, docs: list[dict[str, Any]]) -> list[int]:
        """Batch insert for high throughput.
        
        Uses optimized batch IPC command instead of individual inserts.
        Significantly faster for bulk data loading.
        """
        meta = self._tables.get(table)
        if not meta:
            raise ValueError(f"Table '{table}' does not exist")
        
        if not docs:
            return []
        
        # Use batch insert command
        result = self._dispatcher.insert_batch(table, docs)
        if not result.success:
            raise RuntimeError(f"Batch insert failed: {result.data}")
        
        try:
            resp = msgpack.unpackb(result.data, raw=False)
            row_ids = resp.get("row_ids", [])
        except Exception:
            row_ids = []
        
        # Update metadata
        meta.row_count += len(docs)
        cat = self._catalog.get(table) or TableMeta()
        cat.row_count = meta.row_count
        cat.page_count = max(1, meta.row_count // 80)
        self._catalog.register(table, cat)
        
        return row_ids

    def update(self, table: str, doc_id: int, updates: dict[str, Any]) -> bool:
        meta = self._tables.get(table)
        if not meta:
            raise ValueError(f"Table '{table}' does not exist")

        result = self._dispatcher.update(table, doc_id, updates)
        return result.success

    def delete(self, table: str, doc_id: int) -> bool:
        meta = self._tables.get(table)
        if not meta:
            raise ValueError(f"Table '{table}' does not exist")

        result = self._dispatcher.delete(table, doc_id)
        if result.success:
            meta.row_count = max(0, meta.row_count - 1)
        return result.success

    # ── Query — via Hub IPC ─────────────────────────────────────────

    def find(self, table: str, predicates: list[dict[str, Any]] | None = None,
             columns: list[str] | None = None, order_by: list[dict] | None = None,
             limit: int = 0, offset: int = 0) -> list[dict[str, Any]]:
        meta = self._tables.get(table)
        if not meta:
            raise ValueError(f"Table '{table}' does not exist")

        result = self._dispatcher.query(table, predicates)
        if not result.success:
            return []

        try:
            resp = msgpack.unpackb(result.data, raw=False)
            rows = resp.get("rows", [])
        except Exception:
            return []

        # Sort (control plane logic — no data computation)
        if order_by:
            for spec in reversed(order_by):
                col = spec.get("column", spec.get("field", ""))
                asc = spec.get("ascending", True)
                rows.sort(key=lambda r: r.get(col, 0), reverse=not asc)

        if offset:
            rows = rows[offset:]
        if limit:
            rows = rows[:limit]
        if columns:
            rows = [{c: r.get(c) for c in columns if c in r} for r in rows]

        return rows

    def search(self, table: str, query: str, top_k: int = 10,
               fields: list[str] | None = None) -> list[dict[str, Any]]:
        # Dispatch query to satellite for full-text search
        payload = msgpack.packb({
            "table": table,
            "predicates": [],
            "search_query": query,
            "top_k": top_k,
        })
        result = self._dispatcher.hub.dispatch_sync(
            CommandType.QUERY, table, payload,
            timeout_ms=5000,
        )
        if not result.success:
            return []
        try:
            resp = msgpack.unpackb(result.data, raw=False)
            return resp.get("rows", [])
        except Exception:
            return []

    def vector_search(self, table: str, vector: list[float], top_k: int = 10,
                      metric: str = "cosine") -> list[dict[str, Any]]:
        import numpy as np
        query_arr = np.array(vector, dtype=np.float32)
        result = self._dispatcher.vector_search(
            table, query_arr.tobytes(), len(vector), top_k,
        )
        if not result.success:
            return []

        # Unpack results: count(4) + [id(8) + dist(4)] * count
        data = result.data
        if len(data) < 4:
            return []
        (count,) = struct.unpack("<I", data[:4])
        results = []
        off = 4
        for _ in range(count):
            vid, dist = struct.unpack("<qf", data[off:off + 12])
            off += 12
            results.append({"_id": vid, "_distance": dist, "_score": 1.0 - dist})
        return results

    def hybrid_search(self, table: str, query: str, vector: list[float],
                      top_k: int = 10, alpha: float | None = None) -> list[dict[str, Any]]:
        intent = self._intent_classifier.classify(query, has_vector=True)
        if alpha is None:
            alpha = self._learned_fusion.get_alpha(intent)

        lex_results = self.search(table, query, top_k=top_k * 2)
        vec_results = self.vector_search(table, vector, top_k=top_k * 2)

        fusion = RRFFusionStage(k=60)
        lex_scored = [ScoredCandidate(doc_id=d.get("_id", 0), score=d.get("_score", 0)) for d in lex_results]
        vec_scored = [ScoredCandidate(doc_id=d.get("_id", 0), score=d.get("_score", 0)) for d in vec_results]
        ctx = PipelineContext(query=query, top_k=top_k)
        fused = fusion.execute_multi([lex_scored, vec_scored], ctx)

        output = []
        for c in fused:
            output.append({"_id": c.doc_id, "_score": c.score})
        return output

    def aggregate(self, table: str, group_by: list[str] | None = None,
                  aggregates: list[tuple[str, str, str]] | None = None,
                  predicates: list[dict[str, Any]] | None = None,
                  order_by: list[dict] | None = None,
                  limit: int = 0) -> list[dict[str, Any]]:
        # Get rows from satellite
        rows = self.find(table, predicates=predicates)
        if not rows:
            return []

        batch = ColumnBatch.from_rows(rows)

        if group_by and aggregates:
            agg_engine = VecHashAggregate(group_by, aggregates)
            result_batch = agg_engine.execute(batch)
            results = result_batch.to_rows()
        elif aggregates:
            result: dict[str, Any] = {}
            for func, col, alias in aggregates:
                if func == "count":
                    result[alias] = len(rows)
                elif func == "sum":
                    result[alias] = sum(r.get(col, 0) for r in rows if isinstance(r.get(col), (int, float)))
                elif func == "avg":
                    vals = [r.get(col, 0) for r in rows if isinstance(r.get(col), (int, float))]
                    result[alias] = sum(vals) / len(vals) if vals else 0
                elif func == "min":
                    vals = [r.get(col) for r in rows if r.get(col) is not None]
                    result[alias] = min(vals) if vals else None
                elif func == "max":
                    vals = [r.get(col) for r in rows if r.get(col) is not None]
                    result[alias] = max(vals) if vals else None
            results = [result]
        else:
            results = rows

        if order_by:
            for spec in reversed(order_by):
                col = spec.get("column", spec.get("field", ""))
                asc = spec.get("ascending", True)
                results.sort(key=lambda r: r.get(col, 0), reverse=not asc)
        if limit:
            results = results[:limit]
        return results

    # ── SQL Execution (control plane — parsing + plan only) ─────────

    def execute_sql(self, sql: str) -> list[dict[str, Any]]:
        # Fast-path: delegate to Rust HubEngine when available
        if _qm_engine is not None and os.environ.get("QM_RUST_SQL", "1") == "1":
            try:
                rust_engine = _qm_engine.HubEngine(str(self._data_dir))
                result_json = rust_engine.execute_sql(sql)
                result = json.loads(result_json)
                cols = result.get("columns", [])
                rows = result.get("rows", [])
                return [{cols[i]: r[i] for i in range(min(len(cols), len(r)))} for r in rows]
            except Exception:
                pass  # Fall through to Python path

        try:
            stmt = SQLParser(sql).parse()
        except SQLSyntaxError as e:
            raise ValueError(f"SQL syntax error: {e}") from e

        if isinstance(stmt, SelectStmt):
            return self._exec_select(stmt)
        elif isinstance(stmt, InsertStmt):
            return self._exec_insert_sql(stmt)
        elif isinstance(stmt, UpdateStmt):
            return self._exec_update_sql(stmt)
        elif isinstance(stmt, DeleteStmt):
            return self._exec_delete_sql(stmt)
        elif isinstance(stmt, CreateTableStmt):
            return self._exec_create_table_sql(stmt)
        else:
            raise ValueError(f"Unsupported SQL statement type: {type(stmt).__name__}")

    def _exec_select(self, stmt: SelectStmt) -> list[dict[str, Any]]:
        point_hit = self._try_point_select_fastpath(stmt)
        if point_hit is not None:
            return point_hit

        cte_data: dict[str, list[Row]] = {}
        for cte in stmt.ctes:
            cte_rows = self._exec_select(cte.query)
            cte_data[cte.name] = cte_rows

        if stmt.from_table is None:
            return [self._eval_row_exprs(stmt.columns, {})]

        table_name = stmt.from_table
        alias = stmt.from_alias or table_name

        # Get rows via IPC
        if table_name in cte_data:
            rows = cte_data[table_name]
        else:
            rows = self.find(table_name)
        left_rows_est = max(1, len(rows))

        op: Operator = ScanOperator(rows, alias="")
        available_tables: set[str] = {alias, table_name}
        pending_filters = self._split_conjunctive_predicates(stmt.where) if stmt.where else []
        op, pending_filters = self._apply_pushdown_filters(op, pending_filters, available_tables)

        for jc in stmt.joins:
            join_table = jc.table
            j_alias = jc.alias or join_table
            if join_table in cte_data:
                right_rows = cte_data[join_table]
                right_rows_est = max(1, len(right_rows))
            else:
                right_rows = self.find(join_table)
                meta = self._tables.get(join_table)
                right_rows_est = max(1, meta.row_count if meta else len(right_rows))

            right_only_filters, pending_filters = self._pop_table_local_filters(
                pending_filters,
                {j_alias, join_table},
            )
            if right_only_filters:
                right_pred = self._compile_expr(self._merge_with_and(right_only_filters))
                right_rows = [r for r in right_rows if right_pred(r)]
                right_rows_est = max(1, min(right_rows_est, len(right_rows)))
            right_op = ScanOperator(right_rows, alias="")

            jtype_map = {
                "INNER": JoinType.INNER, "LEFT": JoinType.LEFT,
                "RIGHT": JoinType.RIGHT, "FULL": JoinType.FULL,
                "CROSS": JoinType.CROSS,
            }
            jtype = jtype_map.get(jc.join_type, JoinType.INNER)

            if jc.condition and jtype != JoinType.CROSS:
                left_key, right_key = self._extract_join_keys(jc.condition)
                if left_key and right_key:
                    strategy = self._choose_join_strategy(left_rows_est, right_rows, right_key, jtype)
                    if strategy == "merge":
                        op = MergeJoin(op, right_op, left_key, right_key, jtype)
                    elif strategy == "nested":
                        pred_fn = self._compile_expr(jc.condition)
                        op = NestedLoopJoin(op, right_op, predicate=pred_fn, join_type=jtype)
                    else:
                        build_side = self._choose_hash_build_side(left_rows_est, right_rows_est, jtype)
                        if build_side == "left":
                            op = HashJoin(right_op, op, right_key, left_key, jtype)
                        else:
                            op = HashJoin(op, right_op, left_key, right_key, jtype)
                    left_rows_est = self._estimate_join_rows(
                        left_rows_est,
                        right_rows_est,
                        right_rows,
                        right_key,
                        jtype,
                    )
                else:
                    pred_fn = self._compile_expr(jc.condition)
                    op = NestedLoopJoin(op, right_op, predicate=pred_fn, join_type=jtype)
                    left_rows_est = max(1, min(left_rows_est * max(1, len(right_rows)), 2_000_000))
            else:
                op = NestedLoopJoin(op, right_op, join_type=jtype)
                left_rows_est = max(1, min(left_rows_est * max(1, len(right_rows)), 2_000_000))

            available_tables.update({j_alias, join_table})
            op, pending_filters = self._apply_pushdown_filters(op, pending_filters, available_tables)

        if pending_filters:
            pred_fn = self._compile_expr(self._merge_with_and(pending_filters))
            op = FilterOperator(op, pred_fn)

        has_aggs = any(self._has_aggregate(ae.expr) for ae in stmt.columns)
        if stmt.group_by or has_aggs:
            group_keys = [self._col_name(g) for g in stmt.group_by]
            agg_funcs = self._extract_aggregates(stmt.columns)
            if agg_funcs:
                op = HashAggregateOperator(op, group_keys, agg_funcs)
            if stmt.having:
                pred_fn = self._compile_expr(stmt.having)
                op = FilterOperator(op, pred_fn)

        window_funcs = self._extract_windows(stmt.columns)
        if window_funcs:
            op = WindowOperator(op, window_funcs)

        if stmt.distinct:
            op = DistinctOperator(op)

        if stmt.order_by:
            sort_keys = [(self._col_name(expr), asc) for expr, asc in stmt.order_by]
            op = SortOperator(op, sort_keys)

        if stmt.limit is not None:
            op = LimitOperator(op, stmt.limit, stmt.offset or 0)

        proj_cols = self._resolve_select_columns(stmt.columns, rows)
        if proj_cols and not any(isinstance(ae.expr, StarExpr) for ae in stmt.columns):
            op = ProjectOperator(op, proj_cols)

        return list(op)

    def _exec_insert_sql(self, stmt: InsertStmt) -> list[dict[str, Any]]:
        # Batch optimization: collect all rows first  
        docs = []
        for value_row in stmt.values:
            doc: dict[str, Any] = {}
            for i, col in enumerate(stmt.columns):
                if i < len(value_row):
                    doc[col] = self._eval_literal(value_row[i])
            docs.append(doc)
        
        # Use batch insert for multiple rows
        if len(docs) > 1:
            ids = self.insert_batch(stmt.table, docs)
        else:
            ids = [self.insert(stmt.table, docs[0])] if docs else []
        
        return [{"inserted": len(ids), "ids": ids}]

    def _exec_update_sql(self, stmt: UpdateStmt) -> list[dict[str, Any]]:
        updates: dict[str, Any] = {}
        for col, expr in stmt.assignments:
            updates[col] = self._eval_literal(expr)
        rows = self.find(stmt.table)
        count = 0
        if stmt.where:
            pred_fn = self._compile_expr(stmt.where)
            for row in rows:
                if pred_fn(row):
                    self.update(stmt.table, row.get("_id", 0), updates)
                    count += 1
        else:
            for row in rows:
                self.update(stmt.table, row.get("_id", 0), updates)
                count += 1
        return [{"updated": count}]

    def _exec_delete_sql(self, stmt: DeleteStmt) -> list[dict[str, Any]]:
        rows = self.find(stmt.table)
        count = 0
        if stmt.where:
            pred_fn = self._compile_expr(stmt.where)
            for row in rows:
                if pred_fn(row):
                    self.delete(stmt.table, row.get("_id", 0))
                    count += 1
        else:
            for row in rows:
                self.delete(stmt.table, row.get("_id", 0))
                count += 1
        return [{"deleted": count}]

    def _exec_create_table_sql(self, stmt: CreateTableStmt) -> list[dict[str, Any]]:
        schema: dict[str, str] = {}
        pk = "_id"
        for col_def in stmt.columns:
            schema[col_def.name] = col_def.data_type
            if col_def.primary_key:
                pk = col_def.name
        self.create_table(stmt.table, schema=schema, primary_key=pk)
        return [{"created": stmt.table}]

    # ── Procedure execution (via Hub IPC) ───────────────────────────

    def call_procedure(self, name: str, args: dict[str, Any] | None = None) -> Any:
        result = self._dispatcher.call_procedure(name, args or {})
        if not result.success:
            return None
        try:
            resp = msgpack.unpackb(result.data, raw=False)
            return resp.get("result")
        except Exception:
            return None

    # ── Diagnostics ─────────────────────────────────────────────────

    def execute(self, request: dict[str, Any]) -> list[dict[str, Any]]:
        ast = self._parser.parse(request)
        t0 = time.perf_counter()
        if ast.action == "search":
            results = self.search(ast.table, ast.query_text, top_k=ast.top_k or 10)
        elif ast.action == "vector_search":
            if self._allow_vector_layer(request):
                results = self.vector_search(ast.table, ast.query_vector, top_k=ast.top_k or 10)
            else:
                results = []
        elif ast.action == "hybrid_search":
            if self._allow_vector_layer(request):
                results = self.hybrid_search(ast.table, ast.query_text, ast.query_vector, top_k=ast.top_k or 10)
            else:
                results = self.search(ast.table, ast.query_text, top_k=ast.top_k or 10)
        elif ast.action == "aggregate":
            agg_specs = [(a.func, a.column, a.alias or f"{a.func}_{a.column}") for a in ast.aggregates]
            results = self.aggregate(ast.table, group_by=ast.group_by, aggregates=agg_specs,
                                     predicates=[self._pred_to_dict(p) for p in ast.predicates])
        elif ast.action in ("find", "get"):
            preds = [self._pred_to_dict(p) for p in ast.predicates] if ast.predicates else None
            results = self.find(ast.table, predicates=preds, columns=ast.columns,
                                limit=ast.limit, offset=ast.offset)
        elif ast.action == "insert":
            doc_id = self.insert(ast.table, ast.data or {})
            results = [{"_id": doc_id, "status": "inserted"}]
        elif ast.action == "update":
            preds = [self._pred_to_dict(p) for p in ast.predicates]
            matched = self.find(ast.table, predicates=preds)
            count = 0
            for row in matched:
                if self.update(ast.table, row.get("_id", 0), ast.data or {}):
                    count += 1
            results = [{"matched": len(matched), "modified": count}]
        elif ast.action == "delete":
            preds = [self._pred_to_dict(p) for p in ast.predicates]
            matched = self.find(ast.table, predicates=preds)
            count = 0
            for row in matched:
                if self.delete(ast.table, row.get("_id", 0)):
                    count += 1
            results = [{"deleted": count}]
        else:
            results = []

        elapsed = (time.perf_counter() - t0) * 1000
        query_hash = hashlib.md5(str(request).encode()).hexdigest()[:12]
        plan = self._planner.plan(ast)
        self._adaptive.record_execution(query_hash, plan, elapsed, len(results))
        return results

    def explain(self, request: dict[str, Any]) -> str:
        ast = self._parser.parse(request)
        plan = self._planner.plan(ast)
        optimized = self._adaptive.optimize_plan(plan)
        return optimized.explain()

    def analyze(self, table: str) -> dict[str, Any]:
        meta = self._tables.get(table)
        if not meta:
            raise ValueError(f"Table '{table}' does not exist")
        return {"table": table, "row_count": meta.row_count, "columns": {}}

    def checkpoint(self) -> None:
        pass  # Hub WAL handled by dispatcher

    def stats(self) -> dict[str, Any]:
        return {
            "version": self.VERSION,
            "tables": {name: {"rows": m.row_count, "indexes": m.index_names or []}
                       for name, m in self._tables.items()},
            "hub": self._dispatcher.stats(),
            "plan_accuracy": self._adaptive.estimation_accuracy,
        }

    # ── Expression compiler (shared with QMEngine) ──────────────────

    def _compile_expr(self, node: Any) -> Any:
        if isinstance(node, Literal):
            val = node.value
            return lambda row, v=val: v
        if isinstance(node, ColumnRef):
            col = node.column
            tbl = node.table
            if tbl:
                qualified = f"{tbl}.{col}"
                return lambda row, q=qualified, c=col: row.get(q, row.get(c))
            return lambda row, c=col: row.get(c)
        if isinstance(node, BinaryOp):
            left_fn = self._compile_expr(node.left)
            right_fn = self._compile_expr(node.right)
            op = node.op
            if op == "AND":
                return lambda row, l=left_fn, r=right_fn: bool(l(row)) and bool(r(row))
            if op == "OR":
                return lambda row, l=left_fn, r=right_fn: bool(l(row)) or bool(r(row))
            if op == "=":
                return lambda row, l=left_fn, r=right_fn: l(row) == r(row)
            if op == "!=":
                return lambda row, l=left_fn, r=right_fn: l(row) != r(row)
            if op == "<":
                return lambda row, l=left_fn, r=right_fn: (l(row) is not None and r(row) is not None and l(row) < r(row))
            if op == ">":
                return lambda row, l=left_fn, r=right_fn: (l(row) is not None and r(row) is not None and l(row) > r(row))
            if op == "<=":
                return lambda row, l=left_fn, r=right_fn: (l(row) is not None and r(row) is not None and l(row) <= r(row))
            if op == ">=":
                return lambda row, l=left_fn, r=right_fn: (l(row) is not None and r(row) is not None and l(row) >= r(row))
            if op == "+":
                return lambda row, l=left_fn, r=right_fn: (l(row) or 0) + (r(row) or 0)
            if op == "-":
                return lambda row, l=left_fn, r=right_fn: (l(row) or 0) - (r(row) or 0)
            if op == "*":
                return lambda row, l=left_fn, r=right_fn: (l(row) or 0) * (r(row) or 0)
            if op == "/":
                return lambda row, l=left_fn, r=right_fn: (l(row) or 0) / (r(row) or 1)
        if isinstance(node, UnaryOp):
            inner = self._compile_expr(node.operand)
            if node.op == "NOT":
                return lambda row, f=inner: not bool(f(row))
            if node.op == "-":
                return lambda row, f=inner: -(f(row) or 0)
        if isinstance(node, InList):
            expr_fn = self._compile_expr(node.expr)
            val_fns = [self._compile_expr(v) for v in node.values]
            neg = node.negate
            return lambda row, e=expr_fn, vs=val_fns, n=neg: (e(row) not in [v(row) for v in vs]) if n else (e(row) in [v(row) for v in vs])
        if isinstance(node, BetweenExpr):
            expr_fn = self._compile_expr(node.expr)
            lo_fn = self._compile_expr(node.low)
            hi_fn = self._compile_expr(node.high)
            neg = node.negate
            return lambda row, e=expr_fn, lo=lo_fn, hi=hi_fn, n=neg: not (lo(row) <= e(row) <= hi(row)) if n else (lo(row) <= e(row) <= hi(row))
        if isinstance(node, LikeExpr):
            expr_fn = self._compile_expr(node.expr)
            pat_fn = self._compile_expr(node.pattern)
            neg = node.negate
            def _like(row, e=expr_fn, p=pat_fn, n=neg):
                import re as _re
                v = e(row)
                pat = p(row)
                if v is None or pat is None:
                    return False
                regex = pat.replace("%", ".*").replace("_", ".")
                matched = bool(_re.match(f"^{regex}$", str(v), _re.IGNORECASE))
                return not matched if n else matched
            return _like
        if isinstance(node, IsNullExpr):
            expr_fn = self._compile_expr(node.expr)
            neg = node.negate
            return lambda row, e=expr_fn, n=neg: (e(row) is not None) if n else (e(row) is None)
        if isinstance(node, FunctionCall):
            return self._compile_function(node)
        return lambda row: None

    def _compile_function(self, node: FunctionCall) -> Any:
        name = node.name.upper()
        if name in ("COUNT", "SUM", "AVG", "MIN", "MAX"):
            alias = f"{name.lower()}_{self._col_name(node.args[0]) if node.args else '*'}"
            return lambda row, a=alias: row.get(a)
        if name == "COALESCE":
            fns = [self._compile_expr(a) for a in node.args]
            return lambda row, fs=fns: next((f(row) for f in fs if f(row) is not None), None)
        if name == "ABS":
            inner = self._compile_expr(node.args[0]) if node.args else lambda row: None
            return lambda row, f=inner: abs(f(row)) if f(row) is not None else None
        if name == "UPPER":
            inner = self._compile_expr(node.args[0]) if node.args else lambda row: None
            return lambda row, f=inner: str(f(row)).upper() if f(row) is not None else None
        if name == "LOWER":
            inner = self._compile_expr(node.args[0]) if node.args else lambda row: None
            return lambda row, f=inner: str(f(row)).lower() if f(row) is not None else None
        if name == "LENGTH":
            inner = self._compile_expr(node.args[0]) if node.args else lambda row: None
            return lambda row, f=inner: len(str(f(row))) if f(row) is not None else None
        return lambda row: None

    def _extract_join_keys(self, condition: Any) -> tuple[str | None, str | None]:
        if isinstance(condition, BinaryOp) and condition.op == "=":
            left_col = self._col_name(condition.left)
            right_col = self._col_name(condition.right)
            if left_col and right_col:
                return left_col, right_col
        return None, None

    @staticmethod
    def _allow_vector_layer(request: dict[str, Any]) -> bool:
        if request.get("allow_vector_join") is True:
            return True
        purpose = str(request.get("purpose", "")).lower()
        layer = str(request.get("layer", "")).lower()
        allowed = {
            "suggestion",
            "suggest",
            "question-suggestion",
            "recommendation",
            "recommend",
            "autocomplete",
        }
        return purpose in allowed or layer in allowed

    def _extract_point_predicate_expr(
        self,
        expr: Any,
        table: str,
        alias: str,
    ) -> tuple[str, Any] | None:
        if not isinstance(expr, BinaryOp) or expr.op != "=":
            return None

        left, right = expr.left, expr.right
        if isinstance(left, Literal) and isinstance(right, ColumnRef):
            left, right = right, left
        if not isinstance(left, ColumnRef) or not isinstance(right, Literal):
            return None
        if left.table and left.table not in (table, alias):
            return None
        return left.column, right.value

    def _is_simple_point_projection(self, stmt: SelectStmt) -> bool:
        for ae in stmt.columns:
            if isinstance(ae.expr, StarExpr):
                continue
            if not isinstance(ae.expr, ColumnRef):
                return False
        return True

    def _try_point_select_fastpath(self, stmt: SelectStmt) -> list[dict[str, Any]] | None:
        if stmt.from_table is None:
            return None
        if stmt.ctes or stmt.joins or stmt.group_by or stmt.having or stmt.order_by or stmt.distinct:
            return None
        if stmt.where is None:
            return None
        if not self._is_simple_point_projection(stmt):
            return None
        if stmt.offset not in (None, 0):
            return []

        table = stmt.from_table
        meta = self._tables.get(table)
        if meta is None:
            return None
        alias = stmt.from_alias or table
        point = self._extract_point_predicate_expr(stmt.where, table, alias)
        if point is None:
            return None

        col, val = point
        rows = self.find(table, predicates=[{"column": col, "op": "eq", "value": val}], limit=1)
        if not rows:
            return []

        row = rows[0]
        if any(isinstance(ae.expr, StarExpr) for ae in stmt.columns):
            out = [row]
        else:
            out_row: Row = {}
            for ae in stmt.columns:
                if isinstance(ae.expr, ColumnRef):
                    out_row[ae.alias or ae.expr.column] = row.get(ae.expr.column)
            out = [out_row]

        if stmt.limit is not None:
            out = out[:stmt.limit]
        return out

    def _choose_join_strategy(
        self,
        left_rows_est: int,
        right_rows: list[Row],
        right_key: str,
        join_type: JoinType,
    ) -> str:
        left_n = max(1, left_rows_est)
        right_n = max(1, len(right_rows))
        work = left_n * right_n

        if join_type in (JoinType.LEFT, JoinType.RIGHT, JoinType.FULL):
            return "hash"
        if work <= 12_000:
            return "nested"
        if left_n >= 15_000 and right_n >= 15_000 and self._is_sorted_on(right_rows, right_key):
            return "merge"

        right_sel = self._key_selectivity(right_rows, right_key)
        if right_sel >= 0.98 and work <= 350_000:
            return "nested"
        return "hash"

    @staticmethod
    def _choose_hash_build_side(left_rows_est: int, right_rows_est: int, join_type: JoinType) -> str:
        if join_type != JoinType.INNER:
            return "right"
        return "left" if left_rows_est <= right_rows_est else "right"

    def _estimate_join_rows(
        self,
        left_rows_est: int,
        right_rows_n: int,
        right_rows: list[Row],
        right_key: str,
        join_type: JoinType,
    ) -> int:
        if join_type in (JoinType.LEFT, JoinType.RIGHT, JoinType.FULL):
            return max(left_rows_est, right_rows_n)
        if join_type == JoinType.CROSS:
            return max(1, min(left_rows_est * max(1, right_rows_n), 2_000_000))

        right_sel = self._key_selectivity(right_rows, right_key)
        est = int(left_rows_est * max(0.25, min(1.5, right_sel * 2.0)))
        return max(1, min(est, 2_000_000))

    @staticmethod
    def _key_selectivity(rows: list[Row], key: str) -> float:
        if not rows:
            return 1.0
        return len({r.get(key) for r in rows}) / max(1, len(rows))

    @staticmethod
    def _is_sorted_on(rows: list[Row], key: str, max_check: int = 2048) -> bool:
        if len(rows) < 2:
            return True
        n = min(len(rows), max_check)
        prev = rows[0].get(key)
        for i in range(1, n):
            cur = rows[i].get(key)
            if prev is not None and cur is not None and prev > cur:
                return False
            prev = cur
        return True

    def _split_conjunctive_predicates(self, expr: Any) -> list[Any]:
        if expr is None:
            return []
        if isinstance(expr, BinaryOp) and expr.op == "AND":
            return self._split_conjunctive_predicates(expr.left) + self._split_conjunctive_predicates(expr.right)
        return [expr]

    def _merge_with_and(self, predicates: list[Any]) -> Any:
        out = predicates[0]
        for pred in predicates[1:]:
            out = BinaryOp("AND", out, pred)
        return out

    def _pop_table_local_filters(self, pending: list[Any], tables: set[str]) -> tuple[list[Any], list[Any]]:
        apply_now: list[Any] = []
        remain: list[Any] = []
        for pred in pending:
            refs = self._expr_tables(pred)
            if refs and refs.issubset(tables):
                apply_now.append(pred)
            else:
                remain.append(pred)
        return apply_now, remain

    def _expr_tables(self, node: Any) -> set[str]:
        tables: set[str] = set()
        if isinstance(node, ColumnRef):
            if node.table:
                tables.add(node.table)
            return tables
        if isinstance(node, BinaryOp):
            return self._expr_tables(node.left) | self._expr_tables(node.right)
        if isinstance(node, UnaryOp):
            return self._expr_tables(node.operand)
        if isinstance(node, FunctionCall):
            out: set[str] = set()
            for arg in node.args:
                out |= self._expr_tables(arg)
            return out
        if isinstance(node, InList):
            out = self._expr_tables(node.expr)
            for v in node.values:
                out |= self._expr_tables(v)
            return out
        if isinstance(node, BetweenExpr):
            return self._expr_tables(node.expr) | self._expr_tables(node.low) | self._expr_tables(node.high)
        if isinstance(node, LikeExpr):
            return self._expr_tables(node.expr) | self._expr_tables(node.pattern)
        if isinstance(node, IsNullExpr):
            return self._expr_tables(node.expr)
        return tables

    def _apply_pushdown_filters(
        self,
        op: Operator,
        pending: list[Any],
        available_tables: set[str],
    ) -> tuple[Operator, list[Any]]:
        if not pending:
            return op, pending
        apply_now: list[Any] = []
        remain: list[Any] = []
        for pred in pending:
            refs = self._expr_tables(pred)
            if not refs or refs.issubset(available_tables):
                apply_now.append(pred)
            else:
                remain.append(pred)
        if apply_now:
            op = FilterOperator(op, self._compile_expr(self._merge_with_and(apply_now)))
        return op, remain

    @staticmethod
    def _col_name(node: Any) -> str:
        if isinstance(node, ColumnRef):
            return node.column
        if isinstance(node, str):
            return node
        return ""

    @staticmethod
    def _has_aggregate(node: Any) -> bool:
        if isinstance(node, FunctionCall):
            return node.name.upper() in ("COUNT", "SUM", "AVG", "MIN", "MAX")
        if isinstance(node, BinaryOp):
            return QMHubEngine._has_aggregate(node.left) or QMHubEngine._has_aggregate(node.right)
        return False

    def _extract_aggregates(self, columns: list[AliasedExpr]) -> list[tuple[str, str, str]]:
        aggs: list[tuple[str, str, str]] = []
        for ae in columns:
            if isinstance(ae.expr, FunctionCall) and ae.expr.name.upper() in ("COUNT", "SUM", "AVG", "MIN", "MAX"):
                func = ae.expr.name.upper()
                col = self._col_name(ae.expr.args[0]) if ae.expr.args and not isinstance(ae.expr.args[0], StarExpr) else "*"
                alias = ae.alias or f"{func.lower()}_{col}"
                aggs.append((func, col, alias))
        return aggs

    def _extract_windows(self, columns: list[AliasedExpr]) -> list[tuple[str, str | None, str, WindowSpec]]:
        windows: list[tuple[str, str | None, str, WindowSpec]] = []
        for ae in columns:
            if isinstance(ae.expr, WindowExpr):
                we = ae.expr
                func_name = we.func.name
                input_col = self._col_name(we.func.args[0]) if we.func.args and not isinstance(we.func.args[0], StarExpr) else None
                alias = ae.alias or f"{func_name.lower()}_win"
                spec = WindowSpec(
                    partition_by=[self._col_name(p) for p in we.partition_by],
                    order_by=[(self._col_name(e), asc) for e, asc in we.order_by],
                )
                windows.append((func_name, input_col, alias, spec))
        return windows

    def _resolve_select_columns(self, columns: list[AliasedExpr], sample_rows: list[Row]) -> list[str]:
        cols: list[str] = []
        for ae in columns:
            if isinstance(ae.expr, StarExpr):
                return []
            if ae.alias:
                cols.append(ae.alias)
            elif isinstance(ae.expr, ColumnRef):
                cols.append(ae.expr.column)
            elif isinstance(ae.expr, FunctionCall):
                func = ae.expr.name.upper()
                arg = self._col_name(ae.expr.args[0]) if ae.expr.args and not isinstance(ae.expr.args[0], StarExpr) else "*"
                cols.append(f"{func.lower()}_{arg}")
            elif isinstance(ae.expr, WindowExpr):
                cols.append(ae.alias or f"{ae.expr.func.name.lower()}_win")
        return cols

    def _eval_row_exprs(self, columns: list[AliasedExpr], row: Row) -> Row:
        result: Row = {}
        for ae in columns:
            fn = self._compile_expr(ae.expr)
            name = ae.alias or "?column?"
            result[name] = fn(row)
        return result

    @staticmethod
    def _eval_literal(node: Any) -> Any:
        if isinstance(node, Literal):
            return node.value
        return None

    @staticmethod
    def _pred_to_dict(pred: Any) -> dict[str, Any]:
        if isinstance(pred, dict):
            return pred
        return {
            "column": getattr(pred, "column", ""),
            "op": getattr(pred, "op", "eq") if isinstance(getattr(pred, "op", None), str) else getattr(pred, "op", None).value if hasattr(getattr(pred, "op", None), "value") else "eq",
            "value": getattr(pred, "value", None),
        }

    # ── Hub properties ──────────────────────────────────────────────

    @property
    def dispatcher(self) -> HubDispatcher:
        return self._dispatcher

    @property
    def current_lsn(self) -> int:
        return self._dispatcher.current_lsn

    @property
    def merkle_root(self) -> bytes:
        return self._dispatcher.merkle_root
