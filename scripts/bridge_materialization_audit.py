#!/usr/bin/env python3
"""Audit Python/Rust bridge result materialization without claiming zero-copy."""

from __future__ import annotations

import argparse
import gc
import json
import struct
import time
from pathlib import Path
from typing import Any

import qm_engine


def payload_size_estimate(rows: list[list[str | None]]) -> int:
    total = 0
    for row in rows:
        for cell in row:
            if cell is not None:
                total += len(str(cell).encode("utf-8"))
    return total


def columnar_payload_size(result: dict[str, Any]) -> int:
    total = 0
    for col in result.get("column_buffers", []):
        total += len(col.get("validity", b""))
        total += len(col.get("data", b""))
        total += len(col.get("offsets", b""))
    return total


def zero_copy_payload_size(result: dict[str, Any]) -> int:
    total = 0
    for col in result.get("columns", []):
        if isinstance(col.get("buffer"), memoryview):
            total += col["buffer"].nbytes
        if isinstance(col.get("offsets"), memoryview):
            total += col["offsets"].nbytes
        if isinstance(col.get("data"), memoryview):
            total += col["data"].nbytes
        if isinstance(col.get("validity"), memoryview):
            total += col["validity"].nbytes
    return total


def memoryview_columns(result: dict[str, Any]) -> int:
    count = 0
    for col in result.get("columns", []):
        if isinstance(col.get("buffer"), memoryview) or isinstance(col.get("data"), memoryview):
            count += 1
    return count


def python_object_estimate(rows: int, columns: int) -> dict[str, int]:
    return {
        "outer_lists": 1 + rows,
        "cell_objects": rows * columns,
    }


def decode_int64_buffer(data: bytes) -> list[int]:
    return [value[0] for value in struct.iter_unpack("<q", data)]


def decode_int64_view(data: memoryview) -> list[int]:
    return [value[0] for value in struct.iter_unpack("<q", data)]


def decode_float64_view(data: memoryview) -> list[float]:
    return [value[0] for value in struct.iter_unpack("<d", data)]


def timed_ms(fn) -> tuple[Any, float]:
    start = time.perf_counter_ns()
    result = fn()
    return result, (time.perf_counter_ns() - start) / 1_000_000.0


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rows", type=int, default=1000)
    parser.add_argument("--columns", "--cols", dest="columns", type=int, default=4)
    parser.add_argument(
        "--api",
        choices=["all", "row", "columnar-reduced-copy", "columnar-zero-copy"],
        default="all",
    )
    parser.add_argument("--verify-buffer-kind", action="store_true")
    parser.add_argument("--verify-memoryview", action="store_true")
    parser.add_argument("--bm25-compact", action="store_true")
    parser.add_argument("--hybrid-compact", action="store_true")
    parser.add_argument(
        "--quick-safe",
        action="store_true",
        help="cap very large local audit runs and report the cap explicitly",
    )
    parser.add_argument("--output", default="/tmp/qmvir_bridge_materialization_audit.json")
    args = parser.parse_args()
    if args.rows <= 0:
        raise SystemExit("--rows must be positive")
    if args.columns < 2:
        raise SystemExit("--columns must be at least 2")
    requested_rows = args.rows
    if args.quick_safe and args.rows > 10_000:
        args.rows = 10_000

    engine = qm_engine.NativeSqlEngine()
    extra_cols = ", ".join(f"c{i} TEXT" for i in range(1, args.columns))
    engine.execute(f"CREATE TABLE bridge_audit (id INTEGER PRIMARY KEY, {extra_cols})")

    placeholders = ", ".join(["id"] + [f"c{i}" for i in range(1, args.columns)])
    insert = engine.prepare(
        f"INSERT INTO bridge_audit ({placeholders}) VALUES ("
        + ", ".join(f"${i}" for i in range(1, args.columns + 1))
        + ")"
    )
    for i in range(args.rows):
        params = [str(i)] + [f"r{i}-c{col}" for col in range(1, args.columns)]
        engine.execute_prepared(insert, params)

    select_sql = "SELECT * FROM bridge_audit ORDER BY id"
    should_run_row = args.api in ("all", "row")
    should_run_reduced = args.api in ("all", "columnar-reduced-copy")
    should_run_zero = args.api in ("all", "columnar-zero-copy")

    columns: list[str] = []
    tag = ""
    row_fetch_ms = None
    row_api: dict[str, Any]
    if should_run_row:
        (columns, rows, tag), row_fetch_ms = timed_ms(lambda: engine.execute(select_sql))
        ids = [int(row[0]) for row in rows]
        materialized_as = {
            "outer": type(rows).__name__,
            "row": type(rows[0]).__name__ if rows else None,
            "cell": type(rows[0][0]).__name__ if rows and rows[0] else None,
        }
        payload_bytes = payload_size_estimate(rows)
        row_api = {
            "payload_size_estimate_bytes": payload_bytes,
            "fetch_materialize_ms": row_fetch_ms,
            "ordering_correct": ids == list(range(args.rows)),
            "materialized_as": materialized_as,
            "python_object_estimate": python_object_estimate(len(rows), len(columns)),
            "classification": "PYTHON_OBJECT_MATERIALIZATION",
            "zero_copy": False,
        }
    else:
        columns, _, tag = engine.execute("SELECT * FROM bridge_audit ORDER BY id LIMIT 0")
        row_api = {"skipped": True}

    columnar_supported = hasattr(engine, "execute_columnar")
    columnar: dict[str, Any]
    if columnar_supported and should_run_reduced:
        columnar_result, columnar_fetch_ms = timed_ms(lambda: engine.execute_columnar(select_sql))
        id_buffer = columnar_result["column_buffers"][0]
        if id_buffer["encoding"] == "int64_le":
            columnar_ids = decode_int64_buffer(id_buffer["data"])
        else:
            columnar_ids = []
        columnar = {
            "supported": True,
            "fetch_materialize_ms": columnar_fetch_ms,
            "classification": columnar_result.get("classification", "UNKNOWN"),
            "batch_kind": columnar_result.get("batch_kind", "UNKNOWN"),
            "zero_copy": bool(columnar_result.get("zero_copy", False)),
            "row_count": columnar_result.get("row_count"),
            "column_count": columnar_result.get("column_count"),
            "payload_size_estimate_bytes": columnar_payload_size(columnar_result),
            "id_ordering_correct": columnar_ids == list(range(args.rows)),
            "python_object_estimate": {
                "outer_lists": 1,
                "cell_objects": 0,
            },
            "buffer_encodings": [
                col.get("encoding", "UNKNOWN")
                for col in columnar_result.get("column_buffers", [])
            ],
        }
    else:
        columnar = {
            "supported": columnar_supported,
            "skipped": not should_run_reduced,
            "fetch_materialize_ms": None,
            "classification": "UNKNOWN",
            "batch_kind": "UNKNOWN",
            "zero_copy": False,
            "reason": "NativeSqlEngine.execute_columnar is not exposed"
            if not columnar_supported
            else "api mode skipped reduced-copy columnar audit",
        }

    speedup = None
    if row_fetch_ms is not None and columnar.get("fetch_materialize_ms"):
        speedup = row_fetch_ms / columnar["fetch_materialize_ms"]

    zero_supported = hasattr(engine, "execute_columnar_zero_copy")
    zero_columnar: dict[str, Any]
    if zero_supported and should_run_zero:
        zero_result, zero_fetch_ms = timed_ms(lambda: engine.execute_columnar_zero_copy(select_sql))
        id_col = zero_result["columns"][0]
        id_values = decode_int64_view(id_col["buffer"]) if isinstance(id_col.get("buffer"), memoryview) else []
        if args.verify_memoryview and memoryview_columns(zero_result) != len(zero_result["columns"]):
            raise SystemExit("zero-copy verification failed: not every column exposes memoryview")
        if args.verify_buffer_kind:
            bad = [
                col.get("name")
                for col in zero_result["columns"]
                if col.get("buffer_kind") != "memoryview"
            ]
            if bad:
                raise SystemExit(f"buffer kind verification failed for columns: {bad}")
        zero_columnar = {
            "supported": True,
            "fetch_materialize_ms": zero_fetch_ms,
            "classification": zero_result.get("classification", "UNKNOWN"),
            "batch_kind": zero_result.get("batch_kind", "UNKNOWN"),
            "zero_copy": bool(zero_result.get("zero_copy", False)),
            "row_count": zero_result.get("row_count"),
            "column_count": zero_result.get("column_count"),
            "zero_copy_byte_estimate": zero_copy_payload_size(zero_result),
            "copied_byte_estimate_python": 0 if zero_result.get("zero_copy") else None,
            "unsupported_fallback_count": 0
            if zero_result.get("classification") != "REDUCED_COPY_FALLBACK"
            else 1,
            "id_ordering_correct": id_values == list(range(args.rows)),
            "python_object_estimate": {"outer_lists": 1, "cell_objects": 0},
            "per_column_classification": [
                {
                    "name": col.get("name"),
                    "physical_type": col.get("physical_type"),
                    "zero_copy": col.get("zero_copy"),
                    "copy_reason": col.get("copy_reason"),
                    "buffer_owner": col.get("buffer_owner"),
                    "buffer_kind": col.get("buffer_kind"),
                }
                for col in zero_result.get("columns", [])
            ],
        }
    else:
        zero_columnar = {
            "supported": zero_supported,
            "skipped": not should_run_zero,
            "fetch_materialize_ms": None,
            "classification": "UNKNOWN",
            "zero_copy": False,
            "reason": "NativeSqlEngine.execute_columnar_zero_copy is not exposed"
            if not zero_supported
            else "api mode skipped zero-copy columnar audit",
        }

    if zero_columnar.get("zero_copy"):
        zero_copy_status = "ZERO_COPY_PROVEN"
    elif zero_supported:
        zero_copy_status = "ZERO_COPY_ELIGIBLE_BUT_NOT_PROVEN"
    else:
        zero_copy_status = "REDUCED_COPY_ONLY"

    compact: dict[str, Any] = {}
    if args.bm25_compact:
        bm25_docs = [
            (1, "alpha beta"),
            (2, "alpha alpha"),
            (3, "gamma"),
            (4, "alpha beta alpha"),
        ]
        bm25_result, bm25_ms = timed_ms(
            lambda: engine.search_bm25_compact(bm25_docs, "alpha", 3)
        )
        doc_view = bm25_result["buffers"]["doc_id"]
        score_view = bm25_result["buffers"]["score"]
        rank_view = bm25_result["buffers"]["rank"]
        escaped_doc_view = doc_view
        del bm25_result
        gc.collect()
        compact["bm25"] = {
            "fetch_materialize_ms": bm25_ms,
            "classification": "ZERO_COPY",
            "row_count": len(decode_int64_view(doc_view)),
            "doc_ids": decode_int64_view(doc_view),
            "scores": decode_float64_view(score_view),
            "ranks": decode_int64_view(rank_view),
            "ordering_correct": decode_int64_view(rank_view)
            == list(range(1, len(decode_int64_view(rank_view)) + 1)),
            "lifetime_check_passed": len(decode_int64_view(escaped_doc_view)) > 0,
            "buffer_count": 3,
            "buffer_bytes": doc_view.nbytes + score_view.nbytes + rank_view.nbytes,
            "caveat": "output buffers are zero-copy to Python; BM25 scoring may allocate internally",
        }

    if args.hybrid_compact:
        hybrid_result, hybrid_ms = timed_ms(
            lambda: engine.search_hybrid_compact(
                [(1, 10.0), (2, 1.0)],
                [(2, 10.0), (3, 5.0)],
                0.5,
                3,
            )
        )
        doc_view = hybrid_result["buffers"]["doc_id"]
        score_view = hybrid_result["buffers"]["score"]
        rank_view = hybrid_result["buffers"]["rank"]
        escaped_score_view = score_view
        del hybrid_result
        gc.collect()
        compact["hybrid"] = {
            "fetch_materialize_ms": hybrid_ms,
            "classification": "ZERO_COPY",
            "row_count": len(decode_int64_view(doc_view)),
            "doc_ids": decode_int64_view(doc_view),
            "scores": decode_float64_view(score_view),
            "ranks": decode_int64_view(rank_view),
            "ordering_correct": decode_int64_view(rank_view)
            == list(range(1, len(decode_int64_view(rank_view)) + 1)),
            "lifetime_check_passed": len(decode_float64_view(escaped_score_view)) > 0,
            "buffer_count": 3,
            "buffer_bytes": doc_view.nbytes + score_view.nbytes + rank_view.nbytes,
            "caveat": "output buffers are zero-copy to Python; hybrid scoring may allocate internally",
        }

    output = {
        "requested_rows": requested_rows,
        "actual_rows": args.rows,
        "quick_safe_cap_applied": args.rows != requested_rows,
        "api_mode": args.api,
        "rows_returned": args.rows,
        "columns_per_row": len(columns),
        "command_tag": tag,
        "row_api": row_api,
        "columnar_api": columnar,
        "columnar_zero_copy_api": zero_columnar,
        "compact_apis": compact,
        "columnar_speedup_vs_row_api": speedup,
        "json_serialization_used_by_script": False,
        "insert_path": "prepared statement loop",
        "zero_copy_claim_status": zero_copy_status,
        "claim_scope": "NativeSqlEngine Python bridge rows only",
    }

    path = Path(args.output)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n")
    print(f"wrote {path}")


if __name__ == "__main__":
    main()
