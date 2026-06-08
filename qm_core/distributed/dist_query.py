"""QM Distributed — Distributed Query Engine.

Provides:
    - Cross-shard scatter-gather query execution
    - Partial aggregation on shards + merge on coordinator
    - Distributed sort with merge-sort across shards
    - Query routing based on shard key (single-shard optimization)
    - Parallel shard execution with timeout
    - Result streaming for large result sets

Architecture:
    Client → Coordinator → ScatterGather → [Shard1, Shard2, ...Shard N] → Merge → Client

    ScatterGatherPlan:
        1. Analyze query to determine affected shards
        2. Route to affected shards (or all shards for full-scan)
        3. Execute on each shard in parallel
        4. Merge results (sort, aggregate, limit)
"""

from __future__ import annotations

import heapq
import threading
import time
from collections import defaultdict
from concurrent.futures import Future, ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any, Callable


class QueryType(IntEnum):
    """Type of distributed query."""
    FIND = 0          # Find documents matching filter
    AGGREGATE = 1     # Aggregation pipeline
    COUNT = 2         # Count documents
    UPDATE = 3        # Update documents
    DELETE = 4        # Delete documents
    SEARCH = 5        # Full-text search
    VECTOR = 6        # Vector similarity search
    HYBRID = 7        # Hybrid search


class MergeStrategy(IntEnum):
    """How to merge results from multiple shards."""
    CONCAT = 0        # Simple concatenation
    SORT_MERGE = 1    # Merge sorted results (preserving order)
    AGG_MERGE = 2     # Merge aggregation results
    UNION = 3         # Union (deduplicate)
    FIRST = 4         # Take first shard's result only
    SUM = 5           # Sum counts


@dataclass
class ShardQuery:
    """A query targeted at a specific shard."""
    shard_id: int
    node_id: str
    table: str
    query_type: QueryType
    filter: dict[str, Any] | None = None
    projection: list[str] | None = None
    sort: list[tuple[str, int]] | None = None  # [(field, 1/-1)]
    limit: int | None = None
    offset: int | None = None
    pipeline: list[dict[str, Any]] | None = None  # For aggregations
    search_query: str | None = None
    vector: list[float] | None = None
    vector_field: str | None = None
    k: int | None = None


@dataclass
class ShardResult:
    """Result from a single shard execution."""
    shard_id: int
    node_id: str
    rows: list[dict[str, Any]] = field(default_factory=list)
    count: int = 0
    aggregation: dict[str, Any] | None = None
    error: str | None = None
    execution_time_ms: float = 0.0
    has_more: bool = False


@dataclass
class ScatterGatherPlan:
    """Execution plan for a distributed query."""
    query_id: str
    table: str
    query_type: QueryType
    shard_queries: list[ShardQuery] = field(default_factory=list)
    merge_strategy: MergeStrategy = MergeStrategy.CONCAT
    global_sort: list[tuple[str, int]] | None = None
    global_limit: int | None = None
    global_offset: int | None = None
    single_shard: bool = False        # Optimization: only one shard needed
    parallel: bool = True
    timeout_s: float = 30.0

    def to_dict(self) -> dict[str, Any]:
        return {
            "query_id": self.query_id,
            "table": self.table,
            "query_type": self.query_type.name,
            "num_shards": len(self.shard_queries),
            "merge_strategy": self.merge_strategy.name,
            "single_shard": self.single_shard,
            "timeout_s": self.timeout_s,
        }


@dataclass
class DistributedQueryResult:
    """Final merged result of a distributed query."""
    rows: list[dict[str, Any]] = field(default_factory=list)
    count: int = 0
    aggregation: dict[str, Any] | None = None
    shard_results: list[ShardResult] = field(default_factory=list)
    total_execution_ms: float = 0.0
    shards_contacted: int = 0
    shards_succeeded: int = 0
    shards_failed: int = 0
    plan: ScatterGatherPlan | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "count": self.count,
            "total_rows": len(self.rows),
            "total_execution_ms": round(self.total_execution_ms, 2),
            "shards_contacted": self.shards_contacted,
            "shards_succeeded": self.shards_succeeded,
            "shards_failed": self.shards_failed,
        }


# Shard execution function type
# (shard_id, node_id, query) → result
ShardExecutor = Callable[[ShardQuery], ShardResult]


class ResultMerger:
    """Merges results from multiple shards into a single result."""

    @staticmethod
    def merge(
        results: list[ShardResult],
        strategy: MergeStrategy,
        sort: list[tuple[str, int]] | None = None,
        limit: int | None = None,
        offset: int | None = None,
    ) -> DistributedQueryResult:
        """Merge shard results according to strategy."""
        merged = DistributedQueryResult(
            shard_results=results,
            shards_contacted=len(results),
            shards_succeeded=sum(1 for r in results if r.error is None),
            shards_failed=sum(1 for r in results if r.error is not None),
        )

        if strategy == MergeStrategy.CONCAT:
            ResultMerger._merge_concat(merged, results, sort, limit, offset)
        elif strategy == MergeStrategy.SORT_MERGE:
            ResultMerger._merge_sorted(merged, results, sort, limit, offset)
        elif strategy == MergeStrategy.AGG_MERGE:
            ResultMerger._merge_aggregation(merged, results)
        elif strategy == MergeStrategy.SUM:
            ResultMerger._merge_sum(merged, results)
        elif strategy == MergeStrategy.UNION:
            ResultMerger._merge_union(merged, results, sort, limit, offset)
        elif strategy == MergeStrategy.FIRST:
            ResultMerger._merge_first(merged, results)

        return merged

    @staticmethod
    def _merge_concat(
        merged: DistributedQueryResult,
        results: list[ShardResult],
        sort: list[tuple[str, int]] | None,
        limit: int | None,
        offset: int | None,
    ) -> None:
        """Concatenate all results, optionally sort and limit."""
        all_rows: list[dict[str, Any]] = []
        for r in results:
            if r.error is None:
                all_rows.extend(r.rows)

        if sort:
            all_rows = ResultMerger._sort_rows(all_rows, sort)

        if offset:
            all_rows = all_rows[offset:]
        if limit:
            all_rows = all_rows[:limit]

        merged.rows = all_rows
        merged.count = len(all_rows)

    @staticmethod
    def _merge_sorted(
        merged: DistributedQueryResult,
        results: list[ShardResult],
        sort: list[tuple[str, int]] | None,
        limit: int | None,
        offset: int | None,
    ) -> None:
        """Merge pre-sorted results using k-way merge."""
        if not sort:
            # Fallback to concat
            ResultMerger._merge_concat(merged, results, sort, limit, offset)
            return

        # K-way merge using heap
        sort_field = sort[0][0]
        ascending = sort[0][1] > 0

        # Build iterators
        iterators: list[tuple[int, int, dict]] = []  # (shard_idx, row_idx, row)
        for si, r in enumerate(results):
            if r.error is None and r.rows:
                for ri, row in enumerate(r.rows):
                    val = row.get(sort_field, "")
                    # Use (value, shard_idx, row_idx) for heap ordering
                    if ascending:
                        iterators.append((val, si, ri, row))  # type: ignore
                    else:
                        iterators.append((_negate(val), si, ri, row))  # type: ignore

        heapq.heapify(iterators)

        all_rows: list[dict[str, Any]] = []
        while iterators:
            _, _, _, row = heapq.heappop(iterators)
            all_rows.append(row)

        if offset:
            all_rows = all_rows[offset:]
        if limit:
            all_rows = all_rows[:limit]

        merged.rows = all_rows
        merged.count = len(all_rows)

    @staticmethod
    def _merge_aggregation(
        merged: DistributedQueryResult,
        results: list[ShardResult],
    ) -> None:
        """Merge aggregation results from multiple shards."""
        combined: dict[str, Any] = {}

        for r in results:
            if r.error is not None or r.aggregation is None:
                continue

            for key, value in r.aggregation.items():
                if key not in combined:
                    combined[key] = value
                elif isinstance(value, (int, float)):
                    existing = combined[key]
                    if isinstance(existing, (int, float)):
                        # For count: sum; for avg: would need special handling
                        combined[key] = existing + value
                elif isinstance(value, dict):
                    # Group-by aggregation: merge groups
                    if isinstance(combined[key], dict):
                        for gk, gv in value.items():
                            if gk in combined[key] and isinstance(gv, (int, float)):
                                combined[key][gk] = combined[key][gk] + gv
                            else:
                                combined[key][gk] = gv

        merged.aggregation = combined
        merged.count = sum(r.count for r in results if r.error is None)

    @staticmethod
    def _merge_sum(
        merged: DistributedQueryResult,
        results: list[ShardResult],
    ) -> None:
        """Sum counts from all shards."""
        merged.count = sum(r.count for r in results if r.error is None)

    @staticmethod
    def _merge_union(
        merged: DistributedQueryResult,
        results: list[ShardResult],
        sort: list[tuple[str, int]] | None,
        limit: int | None,
        offset: int | None,
    ) -> None:
        """Union with deduplication by _id."""
        seen: set[str] = set()
        unique_rows: list[dict[str, Any]] = []

        for r in results:
            if r.error is None:
                for row in r.rows:
                    row_id = str(row.get("_id", id(row)))
                    if row_id not in seen:
                        seen.add(row_id)
                        unique_rows.append(row)

        if sort:
            unique_rows = ResultMerger._sort_rows(unique_rows, sort)
        if offset:
            unique_rows = unique_rows[offset:]
        if limit:
            unique_rows = unique_rows[:limit]

        merged.rows = unique_rows
        merged.count = len(unique_rows)

    @staticmethod
    def _merge_first(
        merged: DistributedQueryResult,
        results: list[ShardResult],
    ) -> None:
        """Take the first successful shard's result."""
        for r in results:
            if r.error is None:
                merged.rows = r.rows
                merged.count = r.count
                merged.aggregation = r.aggregation
                break

    @staticmethod
    def _sort_rows(
        rows: list[dict[str, Any]],
        sort: list[tuple[str, int]],
    ) -> list[dict[str, Any]]:
        """Sort rows by multiple fields."""
        for sort_field, direction in reversed(sort):
            reverse = direction < 0
            rows.sort(key=lambda r: r.get(sort_field, ""), reverse=reverse)
        return rows


def _negate(val: Any) -> Any:
    """Negate a value for descending heap sort."""
    if isinstance(val, (int, float)):
        return -val
    if isinstance(val, str):
        # Invert string comparison using XOR with 0xFF (simple approach)
        return tuple(255 - ord(c) for c in val[:100])
    return val


class DistributedQueryEngine:
    """Distributed query execution engine.

    Coordinates scatter-gather query execution across shards.

    Usage:
        engine = DistributedQueryEngine(shard_executor, shard_router)
        result = engine.find("users", {"age": {"$gte": 18}}, limit=100)
        result = engine.aggregate("orders", [{"$group": {"_id": "$status", "count": {"$sum": 1}}}])
    """

    def __init__(
        self,
        shard_executor: ShardExecutor | None = None,
        shard_router: Callable[[str, dict | None], list[tuple[int, str]]] | None = None,
        max_workers: int = 16,
        default_timeout_s: float = 30.0,
    ) -> None:
        self._executor = shard_executor
        self._router = shard_router
        self._pool = ThreadPoolExecutor(max_workers=max_workers, thread_name_prefix="dq")
        self._timeout = default_timeout_s
        self._query_counter = 0
        self._lock = threading.Lock()

        # Stats
        self._total_queries = 0
        self._total_shard_calls = 0
        self._total_errors = 0

    def find(
        self,
        table: str,
        filter: dict[str, Any] | None = None,
        projection: list[str] | None = None,
        sort: list[tuple[str, int]] | None = None,
        limit: int | None = None,
        offset: int | None = None,
    ) -> DistributedQueryResult:
        """Execute a distributed find query."""
        plan = self._plan_find(table, filter, projection, sort, limit, offset)
        return self._execute_plan(plan)

    def aggregate(
        self,
        table: str,
        pipeline: list[dict[str, Any]],
    ) -> DistributedQueryResult:
        """Execute a distributed aggregation."""
        plan = self._plan_aggregate(table, pipeline)
        return self._execute_plan(plan)

    def count(
        self,
        table: str,
        filter: dict[str, Any] | None = None,
    ) -> int:
        """Count documents across all shards."""
        plan = self._plan_count(table, filter)
        result = self._execute_plan(plan)
        return result.count

    def search(
        self,
        table: str,
        query: str,
        limit: int | None = None,
    ) -> DistributedQueryResult:
        """Distributed full-text search."""
        shards = self._resolve_shards(table, None)
        plan = self._create_plan(table, QueryType.SEARCH, MergeStrategy.CONCAT)

        for shard_id, node_id in shards:
            plan.shard_queries.append(ShardQuery(
                shard_id=shard_id, node_id=node_id, table=table,
                query_type=QueryType.SEARCH,
                search_query=query,
                limit=limit,
            ))

        plan.global_limit = limit
        return self._execute_plan(plan)

    def vector_search(
        self,
        table: str,
        vector: list[float],
        vector_field: str = "embedding",
        k: int = 10,
    ) -> DistributedQueryResult:
        """Distributed vector similarity search.

        Each shard returns top-k, then we merge and re-rank globally.
        """
        shards = self._resolve_shards(table, None)
        plan = self._create_plan(table, QueryType.VECTOR, MergeStrategy.SORT_MERGE)
        plan.global_sort = [("_score", -1)]  # Descending by score
        plan.global_limit = k

        for shard_id, node_id in shards:
            plan.shard_queries.append(ShardQuery(
                shard_id=shard_id, node_id=node_id, table=table,
                query_type=QueryType.VECTOR,
                vector=vector,
                vector_field=vector_field,
                k=k,  # Each shard returns top-k
            ))

        return self._execute_plan(plan)

    def execute_write(
        self,
        table: str,
        shard_key_value: Any,
        query_type: QueryType,
        filter: dict[str, Any] | None = None,
    ) -> DistributedQueryResult:
        """Execute a distributed write (update/delete).

        If shard_key is provided, routes to single shard.
        Otherwise, scatters to all shards.
        """
        if shard_key_value is not None:
            shards = self._resolve_shards(table, {"_shard_key": shard_key_value})
        else:
            shards = self._resolve_shards(table, None)

        plan = self._create_plan(table, query_type, MergeStrategy.SUM)
        for shard_id, node_id in shards:
            plan.shard_queries.append(ShardQuery(
                shard_id=shard_id, node_id=node_id, table=table,
                query_type=query_type,
                filter=filter,
            ))

        return self._execute_plan(plan)

    # ── Planning ────────────────────────────────────

    def _plan_find(
        self,
        table: str,
        filter: dict[str, Any] | None,
        projection: list[str] | None,
        sort: list[tuple[str, int]] | None,
        limit: int | None,
        offset: int | None,
    ) -> ScatterGatherPlan:
        shards = self._resolve_shards(table, filter)

        if len(shards) == 1:
            plan = self._create_plan(table, QueryType.FIND, MergeStrategy.FIRST)
            plan.single_shard = True
        elif sort:
            plan = self._create_plan(table, QueryType.FIND, MergeStrategy.SORT_MERGE)
            plan.global_sort = sort
        else:
            plan = self._create_plan(table, QueryType.FIND, MergeStrategy.CONCAT)

        plan.global_limit = limit
        plan.global_offset = offset

        # Per-shard limit: request limit+offset from each shard for correctness
        per_shard_limit = None
        if limit is not None:
            per_shard_limit = limit + (offset or 0)

        for shard_id, node_id in shards:
            plan.shard_queries.append(ShardQuery(
                shard_id=shard_id, node_id=node_id, table=table,
                query_type=QueryType.FIND,
                filter=filter,
                projection=projection,
                sort=sort,
                limit=per_shard_limit,
            ))

        return plan

    def _plan_aggregate(
        self,
        table: str,
        pipeline: list[dict[str, Any]],
    ) -> ScatterGatherPlan:
        shards = self._resolve_shards(table, None)  # Aggregations always scatter
        plan = self._create_plan(table, QueryType.AGGREGATE, MergeStrategy.AGG_MERGE)

        for shard_id, node_id in shards:
            plan.shard_queries.append(ShardQuery(
                shard_id=shard_id, node_id=node_id, table=table,
                query_type=QueryType.AGGREGATE,
                pipeline=pipeline,
            ))

        return plan

    def _plan_count(
        self,
        table: str,
        filter: dict[str, Any] | None,
    ) -> ScatterGatherPlan:
        shards = self._resolve_shards(table, filter)
        plan = self._create_plan(table, QueryType.COUNT, MergeStrategy.SUM)

        for shard_id, node_id in shards:
            plan.shard_queries.append(ShardQuery(
                shard_id=shard_id, node_id=node_id, table=table,
                query_type=QueryType.COUNT,
                filter=filter,
            ))

        return plan

    def _create_plan(
        self,
        table: str,
        query_type: QueryType,
        merge_strategy: MergeStrategy,
    ) -> ScatterGatherPlan:
        with self._lock:
            self._query_counter += 1
            qid = f"dq-{self._query_counter:08d}"

        return ScatterGatherPlan(
            query_id=qid,
            table=table,
            query_type=query_type,
            merge_strategy=merge_strategy,
            timeout_s=self._timeout,
        )

    def _resolve_shards(
        self,
        table: str,
        filter: dict[str, Any] | None,
    ) -> list[tuple[int, str]]:
        """Resolve which shards to query.

        Returns list of (shard_id, node_id).
        """
        if self._router:
            return self._router(table, filter)
        # Default: single shard (non-distributed fallback)
        return [(0, "local")]

    # ── Execution ───────────────────────────────────

    def _execute_plan(self, plan: ScatterGatherPlan) -> DistributedQueryResult:
        """Execute a scatter-gather plan."""
        start = time.monotonic()

        with self._lock:
            self._total_queries += 1

        if not plan.shard_queries:
            return DistributedQueryResult(plan=plan)

        if plan.single_shard or len(plan.shard_queries) == 1:
            # Single shard optimization: no parallel overhead
            results = [self._execute_shard(plan.shard_queries[0])]
        elif plan.parallel:
            results = self._execute_parallel(plan.shard_queries, plan.timeout_s)
        else:
            results = [self._execute_shard(q) for q in plan.shard_queries]

        # Merge
        merged = ResultMerger.merge(
            results,
            strategy=plan.merge_strategy,
            sort=plan.global_sort,
            limit=plan.global_limit,
            offset=plan.global_offset,
        )
        merged.plan = plan
        merged.total_execution_ms = (time.monotonic() - start) * 1000

        with self._lock:
            self._total_shard_calls += len(plan.shard_queries)
            self._total_errors += merged.shards_failed

        return merged

    def _execute_parallel(
        self,
        queries: list[ShardQuery],
        timeout: float,
    ) -> list[ShardResult]:
        """Execute shard queries in parallel."""
        futures: dict[Future, ShardQuery] = {}
        for q in queries:
            fut = self._pool.submit(self._execute_shard, q)
            futures[fut] = q

        results: list[ShardResult] = []
        for fut in as_completed(futures, timeout=timeout):
            try:
                result = fut.result(timeout=1.0)
                results.append(result)
            except Exception as e:
                q = futures[fut]
                results.append(ShardResult(
                    shard_id=q.shard_id,
                    node_id=q.node_id,
                    error=str(e),
                ))

        return results

    def _execute_shard(self, query: ShardQuery) -> ShardResult:
        """Execute a query on a single shard."""
        start = time.monotonic()
        try:
            if self._executor:
                result = self._executor(query)
            else:
                result = ShardResult(
                    shard_id=query.shard_id,
                    node_id=query.node_id,
                    error="No shard executor configured",
                )
            result.execution_time_ms = (time.monotonic() - start) * 1000
            return result
        except Exception as e:
            return ShardResult(
                shard_id=query.shard_id,
                node_id=query.node_id,
                error=str(e),
                execution_time_ms=(time.monotonic() - start) * 1000,
            )

    # ── Stats ───────────────────────────────────────

    def stats(self) -> dict[str, Any]:
        return {
            "total_queries": self._total_queries,
            "total_shard_calls": self._total_shard_calls,
            "total_errors": self._total_errors,
        }

    def shutdown(self) -> None:
        """Shutdown the thread pool."""
        self._pool.shutdown(wait=False)
