"""QM Engine — Top-Level Database Core.

Ties together all kernels into a unified API:

    storage kernel → index kernel → execution kernel →
    statistics + cost model → adaptive optimizer → learned assistants

Usage:
    engine = QMEngine("/path/to/data")
    engine.create_table("docs", schema={"title": "text", "body": "text", "views": "int"})
    engine.insert("docs", {"title": "Hello", "body": "World", "views": 42})
    engine.create_index("docs", "views_idx", columns=["views"], index_type="btree")

    # Find
    results = engine.find("docs", predicates=[{"column": "views", "op": "gt", "value": 10}])

    # SQL
    results = engine.execute_sql("SELECT title, views FROM docs WHERE views > 10 ORDER BY views DESC")

    # JOIN
    results = engine.execute_sql("SELECT o.id, u.name FROM orders o JOIN users u ON o.user_id = u.id")

    # Full-text search
    results = engine.search("docs", query="database systems", top_k=10)

    # Vector search
    results = engine.vector_search("docs", vector=[0.1, 0.2, ...], top_k=5)

    # Hybrid
    results = engine.hybrid_search("docs", query="modern database", vector=[...], top_k=10)

    # Aggregate
    results = engine.aggregate("docs", group_by=["category"], aggregates=[("count", "*", "cnt")])
"""

from __future__ import annotations

import hashlib
import os
import time
from pathlib import Path
from dataclasses import dataclass, field
from typing import Any

# Storage kernel
from qm_core.storage.wal import WriteAheadLog, WALOp
from qm_core.storage.buffer_pool import BufferPool
from qm_core.storage.mvcc import MVCCEngine
from qm_core.storage.segments import SegmentManager
from qm_core.storage.compaction import CompactionEngine, CompactionStrategy, CompactionPolicy

# Concurrency
from qm_core.concurrency import RWLock, WriteConflictError, BackgroundWorker

# Schema & Constraints
from qm_core.schema import (
    Catalog, TableDef, ColumnSchema, DataType, ConstraintChecker, ConstraintViolation,
)

# Index kernel
from qm_core.index.btree import BPlusTree
from qm_core.index.roaring import RoaringBitmap
from qm_core.index.inverted import InvertedIndex
from qm_core.index.hnsw import HNSWIndex, ProductQuantizer, TwoStageANN
from qm_core.index.stats import StatsCollector, TableStats, ColumnStats

# Execution kernel
from qm_core.execution.parser import QueryParser, QueryAST
from qm_core.execution.planner import QueryPlanner, CostModel
from qm_core.execution.vectorized import ColumnBatch, VecFilter, VecSort, VecHashAggregate, VecScorer
from qm_core.execution.pipeline import (
    RetrievalPipeline, PipelineContext, ScoredCandidate,
    CandidateGenStage, LightweightScoringStage, LateMaterializeStage,
    RRFFusionStage,
)

# SQL parser + JOIN operators + Window functions
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

# Statistics
from qm_core.statistics.cost_model import CostModelV2, TableCatalog, TableMeta, IndexMeta, CostParams
from qm_core.statistics.sketches import ColumnSketch, HyperLogLog, TDigest

# Optimizer
from qm_core.optimizer.adaptive import AdaptiveExecutor, PlanHistory, RuleOptimizer

# Learned
from qm_core.learned.assistants import (
    LearnedSelectivity, LearnedCachePolicy, LearnedFusionWeights,
    QueryIntentClassifier,
)


@dataclass
class TableSchema:
    """Schema definition for a table."""
    columns: dict[str, str]  # name → type (text, int, float, vector, json)
    primary_key: str = "_id"
    vector_dim: int | None = None  # If table has vector columns


@dataclass
class TableState:
    """Runtime state for a table."""
    schema: TableSchema
    rows: dict[int, dict[str, Any]] = field(default_factory=dict)
    next_id: int = 1
    indexes: dict[str, Any] = field(default_factory=dict)  # name → index object
    inverted: InvertedIndex | None = None
    hnsw: HNSWIndex | None = None
    pq: ProductQuantizer | None = None
    sketches: dict[str, ColumnSketch] = field(default_factory=dict)  # col → sketch
    row_count: int = 0


class QMEngine:
    """Top-level QM database engine.

    Integrates all kernel layers into a clean, unified API.
    """

    VERSION = "1.0.0"

    def __init__(self, data_dir: str | None = None, wal_enabled: bool = True) -> None:
        self._data_dir = Path(data_dir) if data_dir else None
        if self._data_dir:
            self._data_dir.mkdir(parents=True, exist_ok=True)

        # Storage
        self._wal: WriteAheadLog | None = None
        if wal_enabled and self._data_dir:
            wal_dir = self._data_dir / "wal"
            wal_dir.mkdir(exist_ok=True)
            self._wal = WriteAheadLog(str(wal_dir))
        self._buffer_pool = BufferPool(capacity=4096)
        self._mvcc = MVCCEngine()
        self._compaction: CompactionEngine | None = None
        if self._data_dir:
            seg_dir = self._data_dir / "segments"
            seg_dir.mkdir(exist_ok=True)
            seg_mgr = SegmentManager(str(seg_dir))
            self._compaction = CompactionEngine(
                seg_mgr, CompactionPolicy(strategy=CompactionStrategy.LEVELED)
            )

        # Tables
        self._tables: dict[str, TableState] = {}

        # Execution
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

        # JIT-ish caches for hot logical query paths.
        self._point_query_cache: dict[tuple[str, str], tuple[str, Any]] = {}

    # ── DDL ─────────────────────────────────────────────────────────

    def create_table(self, name: str, schema: dict[str, str],
                     primary_key: str = "_id", vector_dim: int | None = None) -> None:
        """Create a new table."""
        if name in self._tables:
            raise ValueError(f"Table '{name}' already exists")
        ts = TableSchema(columns=schema, primary_key=primary_key, vector_dim=vector_dim)
        self._tables[name] = TableState(schema=ts)
        # Register in catalog
        self._catalog.register(name, TableMeta())
        # Initialize sketches for each column
        for col in schema:
            self._tables[name].sketches[col] = ColumnSketch()

    def drop_table(self, name: str) -> bool:
        if name in self._tables:
            del self._tables[name]
            return True
        return False

    def create_index(self, table: str, index_name: str, columns: list[str],
                     index_type: str = "btree") -> None:
        """Create an index on a table.

        index_type: btree, inverted, hnsw, bitmap
        """
        state = self._get_table(table)
        if index_type == "btree":
            tree = BPlusTree(order=128)
            # Backfill existing data
            col = columns[0]
            for row_id, row in state.rows.items():
                key = row.get(col)
                if key is not None:
                    tree.insert(key, row_id)
            state.indexes[index_name] = tree

        elif index_type == "inverted":
            inv = InvertedIndex()
            # Backfill
            for row_id, row in state.rows.items():
                fields_map: dict[str, list[str]] = {}
                for col in columns:
                    text = row.get(col, "")
                    if isinstance(text, str):
                        fields_map[col] = text.lower().split()
                if fields_map:
                    inv.add_document(row_id, fields_map)
            inv.finalize()
            state.inverted = inv
            state.indexes[index_name] = inv

        elif index_type == "hnsw":
            import numpy as np
            dim = state.schema.vector_dim or 128
            hnsw = HNSWIndex(dim=dim)
            # Backfill
            vec_col = columns[0] if columns else "vector"
            for row_id, row in state.rows.items():
                vec = row.get(vec_col)
                if vec is not None:
                    hnsw.add(row_id, np.array(vec, dtype=np.float32))
            state.hnsw = hnsw
            state.indexes[index_name] = hnsw

        # Update catalog
        meta = self._catalog.get(table) or TableMeta()
        meta.indexes[index_name] = IndexMeta(columns=columns)
        self._catalog.register(table, meta)

    # ── DML ─────────────────────────────────────────────────────────

    def insert(self, table: str, doc: dict[str, Any]) -> int:
        """Insert a document, returns document ID."""
        state = self._get_table(table)
        doc_id = state.next_id
        state.next_id += 1

        # WAL
        if self._wal:
            self._wal.append(WALOp.INSERT, txn_id=0, table=table,
                             key=str(doc_id), data=doc)

        # MVCC write
        txn = self._mvcc.begin()
        self._mvcc.insert(txn, table, str(doc_id), doc)
        self._mvcc.commit(txn)

        # Store
        doc["_id"] = doc_id
        state.rows[doc_id] = doc
        state.row_count += 1

        # Update indexes
        for idx_name, idx in state.indexes.items():
            if isinstance(idx, BPlusTree):
                # Find column for this index
                idx_meta = (self._catalog.get(table) or TableMeta()).indexes.get(idx_name)
                if idx_meta and idx_meta.columns:
                    key = doc.get(idx_meta.columns[0])
                    if key is not None:
                        idx.insert(key, doc_id)

        # Update inverted index (add but don't finalize for perf)
        if state.inverted:
            fields: dict[str, list[str]] = {}
            for col, val in doc.items():
                if isinstance(val, str):
                    fields[col] = val.lower().split()
            if fields:
                state.inverted.add_document(doc_id, fields)

        # Update HNSW
        if state.hnsw:
            vec_col = "vector"
            vec = doc.get(vec_col)
            if vec is not None:
                import numpy as np
                state.hnsw.add(doc_id, np.array(vec, dtype=np.float32))

        # Update sketches
        for col, val in doc.items():
            sketch = state.sketches.get(col)
            if sketch:
                sketch.add(val)

        # Update catalog
        meta = self._catalog.get(table) or TableMeta()
        meta.row_count = state.row_count
        meta.page_count = max(1, state.row_count // 80)
        self._catalog.register(table, meta)

        return doc_id

    def insert_batch(self, table: str, docs: list[dict[str, Any]]) -> list[int]:
        """Batch insert."""
        return [self.insert(table, doc) for doc in docs]

    def update(self, table: str, doc_id: int, updates: dict[str, Any]) -> bool:
        """Update a document."""
        state = self._get_table(table)
        if doc_id not in state.rows:
            return False

        if self._wal:
            self._wal.append(WALOp.UPDATE, txn_id=0, table=table,
                             key=str(doc_id), data=updates)

        state.rows[doc_id].update(updates)
        return True

    def delete(self, table: str, doc_id: int) -> bool:
        """Delete a document."""
        state = self._get_table(table)
        if doc_id not in state.rows:
            return False

        if self._wal:
            self._wal.append(WALOp.DELETE, txn_id=0, table=table,
                             key=str(doc_id))

        del state.rows[doc_id]
        state.row_count -= 1
        return True

    # ── Query ───────────────────────────────────────────────────────

    def find(self, table: str, predicates: list[dict[str, Any]] | None = None,
             columns: list[str] | None = None, order_by: list[dict] | None = None,
             limit: int = 0, offset: int = 0) -> list[dict[str, Any]]:
        """Find documents matching predicates."""
        state = self._get_table(table)
        t0 = time.perf_counter()

        # Point-query fast path for ID lookups.
        if predicates and len(predicates) == 1 and not order_by and not offset:
            point = self._point_predicate_from_dict(predicates[0])
            if point is not None:
                col, val = point
                row = self._lookup_point_row(state, col, val)
                if row is None:
                    return []
                out = [row]
                if columns:
                    out = [{c: row.get(c) for c in columns if c in row}]
                if limit:
                    out = out[:limit]
                return out

        # Fast path: no predicates → full scan
        if not predicates:
            rows = list(state.rows.values())
        else:
            rows = self._filter_rows(state, predicates)

        # Sort
        if order_by:
            for spec in reversed(order_by):
                col = spec.get("column", spec.get("field", ""))
                asc = spec.get("ascending", True)
                rows.sort(key=lambda r: r.get(col, 0), reverse=not asc)

        # Offset + Limit
        if offset:
            rows = rows[offset:]
        if limit:
            rows = rows[:limit]

        # Project
        if columns:
            rows = [{c: r.get(c) for c in columns if c in r} for r in rows]

        elapsed = (time.perf_counter() - t0) * 1000
        return rows

    def search(self, table: str, query: str, top_k: int = 10,
               fields: list[str] | None = None) -> list[dict[str, Any]]:
        """Full-text search using Block-Max WAND."""
        state = self._get_table(table)
        if not state.inverted:
            # Fallback: naive text match
            return self._naive_text_search(state, query, top_k)

        # Ensure index is finalized
        state.inverted.finalize()

        # Use BMW search
        terms = query.lower().split()
        results = state.inverted.search_bmw(terms, top_k=top_k)

        # Late materialization: fetch full docs for top results
        output = []
        for doc_id, score in results:
            doc = state.rows.get(doc_id, {}).copy()
            doc["_score"] = score
            output.append(doc)
        return output

    def vector_search(self, table: str, vector: list[float], top_k: int = 10,
                      metric: str = "cosine") -> list[dict[str, Any]]:
        """Vector similarity search using HNSW."""
        state = self._get_table(table)
        if not state.hnsw:
            return self._naive_vector_search(state, vector, top_k)

        import numpy as np
        results = state.hnsw.search(np.array(vector, dtype=np.float32), top_k=top_k)

        output = []
        for vr in results:
            doc = state.rows.get(vr.id, {}).copy()
            doc["_distance"] = vr.distance
            doc["_score"] = vr.score
            output.append(doc)
        return output

    def hybrid_search(self, table: str, query: str, vector: list[float],
                      top_k: int = 10, alpha: float | None = None) -> list[dict[str, Any]]:
        """Hybrid lexical + vector search with RRF fusion."""
        # Determine alpha from learned weights
        intent = self._intent_classifier.classify(query, has_vector=True)
        if alpha is None:
            alpha = self._learned_fusion.get_alpha(intent)

        # Lexical arm
        lex_results = self.search(table, query, top_k=top_k * 2)

        # Vector arm
        vec_results = self.vector_search(table, vector, top_k=top_k * 2)

        # RRF fusion
        fusion = RRFFusionStage(k=60)
        lex_scored = [ScoredCandidate(doc_id=d.get("_id", 0), score=d.get("_score", 0)) for d in lex_results]
        vec_scored = [ScoredCandidate(doc_id=d.get("_id", 0), score=d.get("_score", 0)) for d in vec_results]

        ctx = PipelineContext(query=query, top_k=top_k)
        fused = fusion.execute_multi([lex_scored, vec_scored], ctx)

        # Late materialization
        state = self._get_table(table)
        output = []
        for c in fused:
            doc = state.rows.get(c.doc_id, {}).copy()
            doc["_score"] = c.score
            output.append(doc)
        return output

    def aggregate(self, table: str, group_by: list[str] | None = None,
                  aggregates: list[tuple[str, str, str]] | None = None,
                  predicates: list[dict[str, Any]] | None = None,
                  order_by: list[dict] | None = None,
                  limit: int = 0) -> list[dict[str, Any]]:
        """Run an aggregation query."""
        state = self._get_table(table)

        # Filter
        if predicates:
            rows = self._filter_rows(state, predicates)
        else:
            rows = list(state.rows.values())

        if not rows:
            return []

        # Convert to ColumnBatch for vectorized aggregation
        batch = ColumnBatch.from_rows(rows)

        if group_by and aggregates:
            agg_engine = VecHashAggregate(group_by, aggregates)
            result_batch = agg_engine.execute(batch)
            results = result_batch.to_rows()
        elif aggregates:
            # Global aggregation (no group by)
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

        # Sort
        if order_by:
            for spec in reversed(order_by):
                col = spec.get("column", spec.get("field", ""))
                asc = spec.get("ascending", True)
                results.sort(key=lambda r: r.get(col, 0), reverse=not asc)

        if limit:
            results = results[:limit]

        return results

    # ── Query DSL ───────────────────────────────────────────────────

    def execute(self, request: dict[str, Any]) -> list[dict[str, Any]]:
        """Execute a query DSL request.

        This is the full pipeline: parse → plan → optimize → execute.
        """
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
            state = self._get_table(ast.table)
            matched = self._filter_rows(state, preds)
            count = 0
            for row in matched:
                if self.update(ast.table, row.get("_id", 0), ast.data or {}):
                    count += 1
            results = [{"matched": len(matched), "modified": count}]
        elif ast.action == "delete":
            preds = [self._pred_to_dict(p) for p in ast.predicates]
            state = self._get_table(ast.table)
            matched = self._filter_rows(state, preds)
            count = 0
            for row in matched:
                if self.delete(ast.table, row.get("_id", 0)):
                    count += 1
            results = [{"deleted": count}]
        else:
            results = []

        elapsed = (time.perf_counter() - t0) * 1000

        # Record for adaptive optimizer
        query_hash = hashlib.md5(str(request).encode()).hexdigest()[:12]
        plan = self._planner.plan(ast)
        self._adaptive.record_execution(query_hash, plan, elapsed, len(results))

        return results

    def explain(self, request: dict[str, Any]) -> str:
        """EXPLAIN a query — show the physical plan."""
        ast = self._parser.parse(request)
        plan = self._planner.plan(ast)
        optimized = self._adaptive.optimize_plan(plan)
        return optimized.explain()

    # ── SQL Execution ───────────────────────────────────────────────

    def execute_sql(self, sql: str) -> list[dict[str, Any]]:
        """Execute a SQL statement and return results.

        Supports:
            SELECT [DISTINCT] ... FROM table
                [JOIN table ON ...]
                [WHERE ...]
                [GROUP BY ... [HAVING ...]]
                [ORDER BY ...]
                [LIMIT n [OFFSET m]]
            INSERT INTO table (...) VALUES (...)
            UPDATE table SET ... [WHERE ...]
            DELETE FROM table [WHERE ...]
            CREATE TABLE table (...)
            WITH cte AS (...) SELECT ...
        """
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
        """Execute a SELECT statement using volcano operators."""
        point_hit = self._try_point_select_fastpath(stmt)
        if point_hit is not None:
            return point_hit

        # Handle CTEs: materialize each CTE first
        cte_data: dict[str, list[Row]] = {}
        for cte in stmt.ctes:
            cte_rows = self._exec_select(cte.query)
            cte_data[cte.name] = cte_rows

        # Resolve source table
        if stmt.from_table is None:
            # SELECT without FROM (e.g., SELECT 1+1)
            return [self._eval_row_exprs(stmt.columns, {})]

        table_name = stmt.from_table
        alias = stmt.from_alias or table_name

        # Get rows from CTE or real table
        if table_name in cte_data:
            rows = cte_data[table_name]
        else:
            state = self._get_table(table_name)
            rows = list(state.rows.values())
        left_rows_est = max(1, len(rows))

        # Build pipeline starting from scan
        op: Operator = ScanOperator(rows, alias="")
        available_tables: set[str] = {alias, table_name}
        pending_filters = self._split_conjunctive_predicates(stmt.where) if stmt.where else []
        op, pending_filters = self._apply_pushdown_filters(op, pending_filters, available_tables)

        # Process JOINs
        for jc in stmt.joins:
            join_table = jc.table
            j_alias = jc.alias or join_table
            if join_table in cte_data:
                right_rows = cte_data[join_table]
                right_rows_est = max(1, len(right_rows))
            else:
                right_state = self._get_table(join_table)
                right_rows = list(right_state.rows.values())
                right_rows_est = max(1, right_state.row_count)

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
                # Try to extract equality join keys
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

        # WHERE
        if pending_filters:
            pred_fn = self._compile_expr(self._merge_with_and(pending_filters))
            op = FilterOperator(op, pred_fn)

        # GROUP BY + aggregates
        has_aggs = any(self._has_aggregate(ae.expr) for ae in stmt.columns)
        if stmt.group_by or has_aggs:
            group_keys = [self._col_name(g) for g in stmt.group_by]
            agg_funcs = self._extract_aggregates(stmt.columns)
            if agg_funcs:
                op = HashAggregateOperator(op, group_keys, agg_funcs)

            # HAVING
            if stmt.having:
                pred_fn = self._compile_expr(stmt.having)
                op = FilterOperator(op, pred_fn)

        # Window functions
        window_funcs = self._extract_windows(stmt.columns)
        if window_funcs:
            op = WindowOperator(op, window_funcs)

        # DISTINCT
        if stmt.distinct:
            op = DistinctOperator(op)

        # ORDER BY
        if stmt.order_by:
            sort_keys = [(self._col_name(expr), asc) for expr, asc in stmt.order_by]
            op = SortOperator(op, sort_keys)

        # LIMIT / OFFSET
        if stmt.limit is not None:
            op = LimitOperator(op, stmt.limit, stmt.offset or 0)

        # PROJECT
        proj_cols = self._resolve_select_columns(stmt.columns, rows)
        if proj_cols and not any(isinstance(ae.expr, StarExpr) for ae in stmt.columns):
            op = ProjectOperator(op, proj_cols)

        # Execute pipeline
        results: list[dict[str, Any]] = list(op)
        return results

    def _exec_insert_sql(self, stmt: InsertStmt) -> list[dict[str, Any]]:
        ids: list[int] = []
        for value_row in stmt.values:
            doc: dict[str, Any] = {}
            for i, col in enumerate(stmt.columns):
                if i < len(value_row):
                    doc[col] = self._eval_literal(value_row[i])
            ids.append(self.insert(stmt.table, doc))
        return [{"inserted": len(ids), "ids": ids}]

    def _exec_update_sql(self, stmt: UpdateStmt) -> list[dict[str, Any]]:
        state = self._get_table(stmt.table)
        updates: dict[str, Any] = {}
        for col, expr in stmt.assignments:
            updates[col] = self._eval_literal(expr)

        count = 0
        if stmt.where:
            pred_fn = self._compile_expr(stmt.where)
            for row_id, row in list(state.rows.items()):
                if pred_fn(row):
                    self.update(stmt.table, row_id, updates)
                    count += 1
        else:
            for row_id in list(state.rows.keys()):
                self.update(stmt.table, row_id, updates)
                count += 1
        return [{"updated": count}]

    def _exec_delete_sql(self, stmt: DeleteStmt) -> list[dict[str, Any]]:
        state = self._get_table(stmt.table)
        count = 0
        if stmt.where:
            pred_fn = self._compile_expr(stmt.where)
            for row_id, row in list(state.rows.items()):
                if pred_fn(row):
                    self.delete(stmt.table, row_id)
                    count += 1
        else:
            for row_id in list(state.rows.keys()):
                self.delete(stmt.table, row_id)
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

    # ── SQL Expression Compiler ─────────────────────────────────────

    def _compile_expr(self, node: Any) -> Any:
        """Compile an AST expression node to a Python callable (Row -> value)."""
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
        # Fallback: constant None
        return lambda row: None

    def _compile_function(self, node: FunctionCall) -> Any:
        """Compile aggregate/scalar function calls."""
        name = node.name.upper()
        if name in ("COUNT", "SUM", "AVG", "MIN", "MAX"):
            # For aggregate inside HAVING: just return column value
            # (the actual aggregation is done by HashAggregateOperator)
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
        """Try to extract left_key, right_key from a simple a.col = b.col condition."""
        if isinstance(condition, BinaryOp) and condition.op == "=":
            left_col = self._col_name(condition.left)
            right_col = self._col_name(condition.right)
            if left_col and right_col:
                return left_col, right_col
        return None, None

    def _choose_join_strategy(
        self,
        left_rows_est: int,
        right_rows: list[Row],
        right_key: str,
        join_type: JoinType,
    ) -> str:
        """Select join strategy based on size and key selectivity."""
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
        """Extract column name from an AST node."""
        if isinstance(node, ColumnRef):
            return node.column
        if isinstance(node, str):
            return node
        return ""

    @staticmethod
    def _has_aggregate(node: Any) -> bool:
        """Check if an expression contains aggregate functions."""
        if isinstance(node, FunctionCall):
            return node.name.upper() in ("COUNT", "SUM", "AVG", "MIN", "MAX")
        if isinstance(node, BinaryOp):
            return QMEngine._has_aggregate(node.left) or QMEngine._has_aggregate(node.right)
        return False

    def _extract_aggregates(self, columns: list[AliasedExpr]) -> list[tuple[str, str, str]]:
        """Extract (func, col, alias) from SELECT columns."""
        aggs: list[tuple[str, str, str]] = []
        for ae in columns:
            if isinstance(ae.expr, FunctionCall) and ae.expr.name.upper() in ("COUNT", "SUM", "AVG", "MIN", "MAX"):
                func = ae.expr.name.upper()
                col = self._col_name(ae.expr.args[0]) if ae.expr.args and not isinstance(ae.expr.args[0], StarExpr) else "*"
                alias = ae.alias or f"{func.lower()}_{col}"
                aggs.append((func, col, alias))
        return aggs

    def _extract_windows(self, columns: list[AliasedExpr]) -> list[tuple[str, str | None, str, WindowSpec]]:
        """Extract window function specs from SELECT columns."""
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
        """Resolve SELECT column list to actual column names."""
        cols: list[str] = []
        for ae in columns:
            if isinstance(ae.expr, StarExpr):
                return []  # SELECT * — don't project
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
        """Evaluate expressions for a single row (SELECT without FROM)."""
        result: Row = {}
        for ae in columns:
            fn = self._compile_expr(ae.expr)
            name = ae.alias or "?column?"
            result[name] = fn(row)
        return result

    @staticmethod
    def _eval_literal(node: Any) -> Any:
        """Extract literal value from AST node."""
        if isinstance(node, Literal):
            return node.value
        return None

    # ── Maintenance ─────────────────────────────────────────────────

    def analyze(self, table: str) -> dict[str, Any]:
        """Collect statistics for a table (like PostgreSQL ANALYZE)."""
        state = self._get_table(table)
        result = {"table": table, "row_count": state.row_count, "columns": {}}

        for col in state.schema.columns:
            values = [row.get(col) for row in state.rows.values() if col in row]
            if values:
                self._stats_collector.analyze_column(table, col, values)
            result["columns"][col] = {
                "distinct": state.sketches.get(col, ColumnSketch()).estimated_distinct(),
                "null_count": sum(1 for v in values if v is None),
                "sample_size": len(values),
            }

        return result

    def checkpoint(self) -> None:
        """Force a WAL checkpoint."""
        if self._wal:
            self._wal.checkpoint()

    def stats(self) -> dict[str, Any]:
        """Engine-wide statistics."""
        return {
            "version": self.VERSION,
            "tables": {name: {"rows": s.row_count, "indexes": list(s.indexes.keys())}
                       for name, s in self._tables.items()},
            "buffer_pool": self._buffer_pool.stats,
            "plan_accuracy": self._adaptive.estimation_accuracy,
            "learned_selectivity": self._learned_selectivity.stats(),
        }

    # ── Internal helpers ────────────────────────────────────────────

    def _get_table(self, name: str) -> TableState:
        state = self._tables.get(name)
        if not state:
            raise ValueError(f"Table '{name}' does not exist")
        return state

    def _filter_rows(self, state: TableState, predicates: list[dict[str, Any]]) -> list[dict[str, Any]]:
        """Filter rows using predicates."""
        rows = list(state.rows.values())
        for pred in predicates:
            col = pred.get("column", pred.get("field", ""))
            op = pred.get("op", "eq")
            val = pred.get("value")
            rows = [r for r in rows if self._eval_pred(r.get(col), op, val)]
        return rows

    @staticmethod
    def _eval_pred(actual: Any, op: str, value: Any) -> bool:
        if actual is None:
            return op == "is_null"
        try:
            if op == "eq":
                return actual == value
            elif op == "neq":
                return actual != value
            elif op == "gt":
                return actual > value
            elif op == "gte":
                return actual >= value
            elif op == "lt":
                return actual < value
            elif op == "lte":
                return actual <= value
            elif op == "in":
                return actual in value
            elif op == "not_in":
                return actual not in value
            elif op == "between":
                return value[0] <= actual <= value[1]
            elif op == "like":
                import re
                pattern = value.replace("%", ".*").replace("_", ".")
                return bool(re.match(pattern, str(actual), re.IGNORECASE))
            elif op == "is_null":
                return actual is None
        except (TypeError, ValueError):
            return False
        return False

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

    @staticmethod
    def _point_predicate_from_dict(pred: dict[str, Any]) -> tuple[str, Any] | None:
        col = pred.get("column", pred.get("field", ""))
        op = str(pred.get("op", "eq")).lower()
        if op in ("eq", "=") and col:
            return str(col), pred.get("value")
        return None

    def _lookup_point_row(self, state: TableState, column: str, value: Any) -> Row | None:
        pk = state.schema.primary_key

        if column == "_id" and isinstance(value, int):
            row = state.rows.get(value)
            return row.copy() if row else None

        if column == pk:
            # Fallback to PK scan for correctness even when no explicit PK index exists.
            for row in state.rows.values():
                if row.get(pk) == value:
                    return row.copy()
            return None

        # General column scan for non-PK columns.
        for row in state.rows.values():
            if row.get(column) == value:
                return row.copy()
        return None

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
        state = self._tables.get(table)
        if state is None:
            return None
        alias = stmt.from_alias or table

        point = self._extract_point_predicate_expr(stmt.where, table, alias)
        if point is None:
            return None

        row = self._lookup_point_row(state, point[0], point[1])
        if row is None:
            return []

        if any(isinstance(ae.expr, StarExpr) for ae in stmt.columns):
            out_row = row
        else:
            cols = [ae.alias or ae.expr.column for ae in stmt.columns if isinstance(ae.expr, ColumnRef)]
            source_cols = [ae.expr.column for ae in stmt.columns if isinstance(ae.expr, ColumnRef)]
            out_row = {cols[i]: row.get(source_cols[i]) for i in range(len(cols))}

        out = [out_row]
        if stmt.limit is not None:
            out = out[:stmt.limit]
        return out

    @staticmethod
    def _pred_to_dict(pred: Any) -> dict[str, Any]:
        """Convert a Predicate object to a dict."""
        if isinstance(pred, dict):
            return pred
        return {
            "column": getattr(pred, "column", ""),
            "op": getattr(pred, "op", "eq") if isinstance(getattr(pred, "op", None), str) else getattr(pred, "op", None).value if hasattr(getattr(pred, "op", None), "value") else "eq",
            "value": getattr(pred, "value", None),
        }

    def _naive_text_search(self, state: TableState, query: str, top_k: int) -> list[dict[str, Any]]:
        """Simple text search fallback when no inverted index exists."""
        terms = query.lower().split()
        scored = []
        for doc_id, row in state.rows.items():
            text = " ".join(str(v) for v in row.values() if isinstance(v, str)).lower()
            score = sum(text.count(t) for t in terms)
            if score > 0:
                scored.append((doc_id, score))
        scored.sort(key=lambda x: x[1], reverse=True)
        results = []
        for doc_id, score in scored[:top_k]:
            doc = state.rows[doc_id].copy()
            doc["_score"] = score
            results.append(doc)
        return results

    def _naive_vector_search(self, state: TableState, vector: list[float], top_k: int) -> list[dict[str, Any]]:
        """Brute-force vector search fallback."""
        import math
        scored = []
        for doc_id, row in state.rows.items():
            vec = row.get("vector")
            if vec and len(vec) == len(vector):
                dot = sum(a * b for a, b in zip(vector, vec))
                norm_q = math.sqrt(sum(x * x for x in vector))
                norm_v = math.sqrt(sum(x * x for x in vec))
                sim = dot / (norm_q * norm_v + 1e-10)
                scored.append((doc_id, sim))
        scored.sort(key=lambda x: x[1], reverse=True)
        results = []
        for doc_id, sim in scored[:top_k]:
            doc = state.rows[doc_id].copy()
            doc["_score"] = sim
            doc["_distance"] = 1.0 - sim
            results.append(doc)
        return results
