#!/usr/bin/env python3
"""P0 release gate: bulk ingest, vector torture, index fidelity, cross-version reload.

Designed to catch real-world failures before npm/PyPI publish:
  - slow bulk import
  - vector dim / literal errors
  - search index wrong after checkpoint reload
  - data_dir incompatibility across reopen / prior wheel
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from publish_benchmark_lib import environment_info, percentile  # noqa: E402

# P0 release gate checklist — each item documents scope and PG comparability.
RELEASE_GATE_CHECKLIST: list[dict[str, Any]] = [
    {
        "id": "bulk_ingest",
        "title": "Bulk ingest profile",
        "description": "Batch INSERT + btree/GIN/trigram index build + checkpoint",
        "pg_comparable": True,
        "metrics": ["ingest_rows_per_sec", "index_build_elapsed_s"],
    },
    {
        "id": "vector_torture",
        "title": "Vector torture",
        "description": "Dim mismatch, NaN/inf/malformed reject, bulk load, exact KNN reload, HNSW",
        "pg_comparable": True,
        "metrics": ["dim_validation", "bulk_rows_per_sec", "knn_p50_ms"],
    },
    {
        "id": "search_index_fidelity",
        "title": "Search index fidelity after reload",
        "description": "Query results identical before/after checkpoint (exact indexes; HNSW functional only)",
        "pg_comparable": True,
        "metrics": ["fts", "json_path", "trigram", "equality", "vector_knn_p50_ms"],
    },
    {
        "id": "cross_version_upgrade",
        "title": "Cross-version upgrade",
        "description": "v1 search checkpoint migration + optional QM_PREV_WHEEL / --prev-data-dir",
        "pg_comparable": False,
        "metrics": ["row_count", "fts", "equality", "vector_knn"],
    },
    {
        "id": "npm_smoke",
        "title": "npm / PyPI smoke",
        "description": "Import qm_engine, basic SQL, version alignment with npm/package.json",
        "pg_comparable": False,
        "metrics": ["python_smoke_ok", "version_aligned"],
    },
]


def read_repo_version() -> str | None:
    pyproject = SCRIPT_DIR.parent / "pyproject.toml"
    if not pyproject.exists():
        return None
    for line in pyproject.read_text().splitlines():
        line = line.strip()
        if line.startswith("version") and "=" in line:
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    return None


def downgrade_search_checkpoint_to_v1(data_dir: Path) -> bool:
    path = data_dir / "native_sql.search_indexes"
    if not path.exists():
        return False
    data = json.loads(path.read_text())
    data["version"] = 1
    data.pop("hnsw", None)
    path.write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
    return True


def seed_cross_version_dir(qm_engine: Any, dest: Path) -> None:
    dest.mkdir(parents=True, exist_ok=True)
    engine = make_engine(qm_engine, dest)
    engine.execute(
        "CREATE TABLE cross_ver (id INTEGER PRIMARY KEY, data JSON, tags TEXT, body TEXT)"
    )
    engine.execute("CREATE TABLE cross_vec (id INTEGER PRIMARY KEY, embedding VECTOR(16))")
    for i in range(2000):
        engine.execute(
            f"INSERT INTO cross_ver (id, data, tags, body) VALUES "
            f"({i}, '{{\"name\":\"user{i}\"}}', 'tag_{i % 15}', 'text {i} needle')"
        )
        lit = "[" + ",".join(f"{((i + j) % 5) / 5.0:.3f}" for j in range(16)) + "]"
        engine.execute(f"INSERT INTO cross_vec (id, embedding) VALUES ({i}, '{lit}')")
    engine.execute("CREATE INDEX cross_ver_tags ON cross_ver (tags)")
    engine.execute("CREATE INDEX cross_body ON cross_ver (body) USING gin")
    engine.execute("CREATE INDEX cross_body_trgm ON cross_ver (body) USING gin_trgm")
    engine.execute("CREATE INDEX cross_json_name ON cross_ver (data) USING json_path('name')")
    engine.execute("CREATE INDEX cross_vec_hnsw ON cross_vec (embedding) USING hnsw")
    if hasattr(engine, "sync_wal"):
        engine.sync_wal()
    if hasattr(engine, "checkpoint"):
        engine.checkpoint()


def run_cross_version_checks(engine: Any) -> dict[str, Any]:
    vec_q = (
        "'[0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8,0.9,0.1,0.2,0.3,0.4,0.5,0.6,0.7]'"
    )
    return {
        "row_count": int(cell_text(engine.execute("SELECT COUNT(*) FROM cross_ver")) or "0") == 2000,
        "equality": len(query_ids(engine, "SELECT id FROM cross_ver WHERE tags = 'tag_7' ORDER BY id")) >= 1,
        "fts": len(query_ids(engine, "SELECT id FROM cross_ver WHERE body @@ 'needle' LIMIT 5")) == 5,
        "json_path": len(
            query_ids(
                engine,
                "SELECT id FROM cross_ver WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'user42'",
            )
        )
        == 1,
        "vector": len(
            query_ids(
                engine,
                f"SELECT id FROM cross_vec ORDER BY embedding <-> {vec_q} LIMIT 5",
            )
        )
        == 5,
    }


def cell_text(result: Any, row: int = 0, col: int = 0) -> str:
    if not isinstance(result, tuple) or len(result) < 2:
        return ""
    rows = result[1]
    if not rows or row >= len(rows):
        return ""
    cells = rows[row]
    if not cells or col >= len(cells):
        return ""
    raw = cells[col]
    if raw is None:
        return ""
    if isinstance(raw, bytes):
        return raw.decode("utf-8", errors="replace")
    return str(raw)


def query_ids(engine: Any, sql: str) -> list[str]:
    result = engine.execute(sql)
    if not isinstance(result, tuple) or len(result) < 2:
        return []
    out: list[str] = []
    for row in result[1]:
        if row and row[0] is not None:
            val = row[0]
            out.append(val.decode("utf-8") if isinstance(val, bytes) else str(val))
    return out


def expect_error(engine: Any, sql: str) -> tuple[bool, str]:
    try:
        engine.execute(sql)
        return False, "expected error but succeeded"
    except Exception as exc:
        return True, str(exc)


def make_engine(qm_engine: Any, data_dir: Path) -> Any:
    engine = qm_engine.NativeSqlEngine(str(data_dir))
    if hasattr(engine, "set_wal_sync_policy"):
        engine.set_wal_sync_policy("per_commit_sync")
    return engine


def bulk_ingest_profile(
    qm_engine: Any,
    *,
    row_targets: list[int],
    batch_size: int = 1000,
) -> dict[str, Any]:
    results: list[dict[str, Any]] = []
    for target in row_targets:
        with tempfile.TemporaryDirectory(prefix=f"qm-bulk-{target}-") as tmp:
            data_dir = Path(tmp)
            engine = make_engine(qm_engine, data_dir)
            engine.execute(
                "CREATE TABLE bulk_ingest ("
                "id INTEGER PRIMARY KEY, tags TEXT, body TEXT, score INTEGER)"
            )
            t0 = time.perf_counter()
            inserted = 0
            batch: list[str] = []
            for i in range(target):
                tag = f"tag_{i % 50}"
                body = f"chunk {i} alpha beta skewed text len={i % 17}"
                batch.append(f"({i}, '{tag}', '{body}', {i % 100})")
                if len(batch) >= batch_size:
                    engine.execute(
                        "INSERT INTO bulk_ingest (id, tags, body, score) VALUES "
                        + ",".join(batch)
                    )
                    inserted += len(batch)
                    batch.clear()
            if batch:
                engine.execute(
                    "INSERT INTO bulk_ingest (id, tags, body, score) VALUES " + ",".join(batch)
                )
                inserted += len(batch)
            ingest_elapsed = time.perf_counter() - t0

            idx_start = time.perf_counter()
            engine.execute("CREATE INDEX idx_bulk_tags ON bulk_ingest (tags)")
            engine.execute("CREATE INDEX idx_bulk_body ON bulk_ingest (body) USING gin")
            engine.execute("CREATE INDEX idx_bulk_body_trgm ON bulk_ingest (body) USING gin_trgm")
            index_elapsed = time.perf_counter() - idx_start

            ck_start = time.perf_counter()
            if hasattr(engine, "sync_wal"):
                engine.sync_wal()
            if hasattr(engine, "checkpoint"):
                engine.checkpoint()
            checkpoint_elapsed = time.perf_counter() - ck_start

            count = int(cell_text(engine.execute("SELECT COUNT(*) FROM bulk_ingest")) or "0")
            results.append(
                {
                    "rows_target": target,
                    "rows_inserted": inserted,
                    "row_count_ok": count == target,
                    "batch_size": batch_size,
                    "ingest_elapsed_s": ingest_elapsed,
                    "ingest_rows_per_sec": inserted / ingest_elapsed if ingest_elapsed > 0 else 0.0,
                    "index_build_elapsed_s": index_elapsed,
                    "checkpoint_elapsed_s": checkpoint_elapsed,
                }
            )
    ok = all(r["row_count_ok"] for r in results)
    return {"ok": ok, "profiles": results}


def vector_torture(qm_engine: Any, *, bulk_rows: int = 20_000, dim: int = 32) -> dict[str, Any]:
    cases: list[dict[str, Any]] = []

    def error_case(name: str, setup_sql: list[str], bad_sql: str) -> None:
        with tempfile.TemporaryDirectory(prefix=f"qm-vec-err-{name}-") as tmp:
            engine = make_engine(qm_engine, Path(tmp))
            for sql in setup_sql:
                engine.execute(sql)
            ok, detail = expect_error(engine, bad_sql)
            cases.append({"case": name, "expect_error": True, "ok": ok, "detail": detail})

    error_case(
        "insert_dim_plus_one",
        [f"CREATE TABLE vec_bad_dim (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))"],
        f"INSERT INTO vec_bad_dim (id, embedding) VALUES (1, '[{','.join(['0.1'] * (dim + 1))}]')",
    )
    error_case(
        "insert_dim_too_short",
        [f"CREATE TABLE vec_short (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))"],
        f"INSERT INTO vec_short (id, embedding) VALUES (2, '[0.1,0.2]')",
    )
    error_case(
        "knn_query_dim_mismatch",
        [
            f"CREATE TABLE vec_q (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))",
            f"INSERT INTO vec_q (id, embedding) VALUES (10, '[{','.join(['0.5'] * dim)}]')",
        ],
        "SELECT id FROM vec_q ORDER BY embedding <-> '[0.1,0.2,0.3]' LIMIT 1",
    )
    for name, bad_sql in [
        ("insert_empty_vector", f"INSERT INTO vec_lit (id, embedding) VALUES (1, '[]')"),
        ("insert_malformed_literal", f"INSERT INTO vec_lit (id, embedding) VALUES (1, '[1,bad,3]')"),
        ("insert_nan", f"INSERT INTO vec_lit (id, embedding) VALUES (1, '[NaN,0,0]')"),
        ("insert_inf", f"INSERT INTO vec_lit (id, embedding) VALUES (1, '[inf,0,0]')"),
    ]:
        error_case(
            name,
            [f"CREATE TABLE vec_lit (id INTEGER PRIMARY KEY, embedding VECTOR(3))"],
            bad_sql,
        )

    with tempfile.TemporaryDirectory(prefix="qm-vector-torture-") as tmp:
        data_dir = Path(tmp)
        exact_engine = make_engine(qm_engine, data_dir)
        exact_engine.execute(f"CREATE TABLE vec_exact (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
        for i in range(128):
            lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
            exact_engine.execute(f"INSERT INTO vec_exact (id, embedding) VALUES ({i}, '{lit}')")
        query = "[" + ",".join(f"{0.1 + j * 0.01:.4f}" for j in range(dim)) + "]"
        exact_before = query_ids(
            exact_engine, f"SELECT id FROM vec_exact ORDER BY embedding <-> '{query}' LIMIT 10"
        )
        if hasattr(exact_engine, "checkpoint"):
            exact_engine.checkpoint()
        exact_reopened = make_engine(qm_engine, data_dir)
        exact_after = query_ids(
            exact_reopened, f"SELECT id FROM vec_exact ORDER BY embedding <-> '{query}' LIMIT 10"
        )
        cases.append(
            {
                "case": "knn_exact_stable_after_reload",
                "expect_error": False,
                "ok": exact_before == exact_after and len(exact_before) == 10,
                "detail": f"before={exact_before[:3]} after={exact_after[:3]}",
            }
        )
        del exact_engine, exact_reopened

        engine = make_engine(qm_engine, data_dir)
        engine.execute(f"CREATE TABLE vec_torture (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")

        batch = 500
        t0 = time.perf_counter()
        for start in range(0, bulk_rows, batch):
            values = []
            for i in range(start, min(bulk_rows, start + batch)):
                row_lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
                values.append(f"({i}, '{row_lit}')")
            engine.execute(
                "INSERT INTO vec_torture (id, embedding) VALUES " + ",".join(values)
            )
        bulk_elapsed = time.perf_counter() - t0

        knn_ok = len(
            query_ids(engine, f"SELECT id FROM vec_torture ORDER BY embedding <-> '{query}' LIMIT 10")
        ) == 10

        if hasattr(engine, "checkpoint"):
            engine.checkpoint()
        reopened = make_engine(qm_engine, data_dir)
        reopened.execute("CREATE INDEX idx_vec_torture ON vec_torture (embedding) USING hnsw")
        knn_hnsw = query_ids(
            reopened, f"SELECT id FROM vec_torture ORDER BY embedding <-> '{query}' LIMIT 10"
        )
        hnsw_functional = len(knn_hnsw) == 10

        count = int(cell_text(reopened.execute("SELECT COUNT(*) FROM vec_torture")) or "0")
        cases.append(
            {
                "case": "bulk_vector_load_knn",
                "expect_error": False,
                "ok": knn_ok and count == bulk_rows,
                "detail": f"count={count}",
                "bulk_rows": bulk_rows,
                "bulk_elapsed_s": bulk_elapsed,
                "bulk_rows_per_sec": bulk_rows / bulk_elapsed if bulk_elapsed > 0 else 0.0,
            }
        )
        cases.append(
            {
                "case": "hnsw_knn_functional",
                "expect_error": False,
                "ok": hnsw_functional,
                "detail": f"knn={len(knn_hnsw)}",
            }
        )

    ok = all(c["ok"] for c in cases)
    dim_validation_ok = all(
        c["ok"]
        for c in cases
        if c["case"] in ("insert_dim_plus_one", "insert_dim_too_short")
    )
    literal_validation_ok = all(
        c["ok"]
        for c in cases
        if c["case"]
        in ("insert_empty_vector", "insert_malformed_literal", "insert_nan", "insert_inf")
    )
    return {
        "ok": ok,
        "dim_validation_ok": dim_validation_ok,
        "literal_validation_ok": literal_validation_ok,
        "dim": dim,
        "cases": cases,
    }


def search_index_fidelity_after_reload(qm_engine: Any, *, rows: int = 5000) -> dict[str, Any]:
    queries = {
        "fts": "SELECT id FROM fidelity WHERE body @@ 'needle alpha' LIMIT 10",
        "json_path": "SELECT id FROM fidelity WHERE JSON_EXTRACT_PATH_TEXT(data, 'name') = 'user42'",
        "trigram": "SELECT id FROM fidelity WHERE body LIKE '%needle%' ORDER BY id",
        "equality": "SELECT id FROM fidelity WHERE tags = 'tag_7' ORDER BY id",
        "vector": None,
    }
    dim = 32
    vec_query = "[" + ",".join(f"{0.1 + j * 0.01:.4f}" for j in range(dim)) + "]"
    queries["vector"] = f"SELECT id FROM fidelity_vec ORDER BY embedding <-> '{vec_query}' LIMIT 10"
    vector_rows = min(rows, 128)  # stay below adaptive HNSW threshold for exact KNN fidelity

    with tempfile.TemporaryDirectory(prefix="qm-fidelity-") as tmp:
        data_dir = Path(tmp)
        engine = make_engine(qm_engine, data_dir)
        engine.execute(
            "CREATE TABLE fidelity (id INTEGER PRIMARY KEY, data JSON, tags TEXT, body TEXT)"
        )
        engine.execute(f"CREATE TABLE fidelity_vec (id INTEGER PRIMARY KEY, embedding VECTOR({dim}))")
        engine.execute("CREATE INDEX fidelity_tags ON fidelity (tags)")

        batch = 500
        for start in range(0, rows, batch):
            for i in range(start, min(rows, start + batch)):
                engine.execute(
                    f"INSERT INTO fidelity (id, data, tags, body) VALUES "
                    f"({i}, '{{\"name\":\"user{i}\",\"score\":{i % 100}}}', 'tag_{i % 20}', "
                    f"'alpha beta gamma text chunk {i} needle')"
                )
        for i in range(vector_rows):
            lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
            engine.execute(f"INSERT INTO fidelity_vec (id, embedding) VALUES ({i}, '{lit}')")

        engine.execute("CREATE INDEX idx_fidelity_body ON fidelity (body) USING gin")
        engine.execute("CREATE INDEX idx_fidelity_body_trgm ON fidelity (body) USING gin_trgm")
        engine.execute("CREATE INDEX idx_fidelity_data_name ON fidelity (data) USING json_path('name')")

        before = {name: query_ids(engine, sql) for name, sql in queries.items()}
        if hasattr(engine, "sync_wal"):
            engine.sync_wal()
        if hasattr(engine, "checkpoint"):
            engine.checkpoint()
        del engine

        reopen_ms: list[float] = []
        after: dict[str, list[str]] = {}
        for _ in range(3):
            t0 = time.perf_counter()
            reopened = make_engine(qm_engine, data_dir)
            reopen_ms.append((time.perf_counter() - t0) * 1000.0)
            after = {name: query_ids(reopened, sql) for name, sql in queries.items()}
            del reopened

        # HNSW functional check on a larger slice (above adaptive threshold).
        hnsw_engine = make_engine(qm_engine, data_dir)
        for i in range(vector_rows, min(rows, 512)):
            lit = "[" + ",".join(f"{((i + j) % 17) / 17.0:.4f}" for j in range(dim)) + "]"
            hnsw_engine.execute(f"INSERT INTO fidelity_vec (id, embedding) VALUES ({i}, '{lit}')")
        hnsw_engine.execute("CREATE INDEX idx_fidelity_vec ON fidelity_vec (embedding) USING hnsw")
        hnsw_ids = query_ids(hnsw_engine, queries["vector"])
        hnsw_functional = len(hnsw_ids) == 10
        if hasattr(hnsw_engine, "checkpoint"):
            hnsw_engine.checkpoint()
        del hnsw_engine
        hnsw_reopened = make_engine(qm_engine, data_dir)
        hnsw_after = query_ids(hnsw_reopened, queries["vector"])
        hnsw_reload_ok = len(hnsw_after) == 10

        checks = []
        for name in queries:
            match = before[name] == after[name]
            checks.append(
                {
                    "query": name,
                    "ok": match,
                    "before_count": len(before[name]),
                    "after_count": len(after[name]),
                    "before_sample": before[name][:5],
                    "after_sample": after[name][:5],
                }
            )
        checks.append(
            {
                "query": "vector_hnsw_functional",
                "ok": hnsw_functional and hnsw_reload_ok,
                "before_count": len(hnsw_ids),
                "after_count": len(hnsw_after),
                "before_sample": hnsw_ids[:5],
                "after_sample": hnsw_after[:5],
            }
        )
        count = int(
            cell_text(make_engine(qm_engine, data_dir).execute("SELECT COUNT(*) FROM fidelity")) or "0"
        )
        exact_ok = all(c["ok"] for c in checks if c["query"] != "vector_hnsw_functional")
        ok = exact_ok and hnsw_functional and hnsw_reload_ok and count == rows
        return {
            "ok": ok,
            "rows": rows,
            "row_count_ok": count == rows,
            "reopen_p50_ms": percentile(reopen_ms, 50),
            "checks": checks,
        }


def cross_version_upgrade(
    qm_engine: Any,
    *,
    prev_data_dir: Path | None = None,
    prev_wheel: str | None = None,
) -> dict[str, Any]:
    """Reopen data_dir after v1 checkpoint migration and optional prior-wheel seed."""
    persist = Path(tempfile.mkdtemp(prefix="qm-crossver-persist-"))
    created_with: str
    v1_downgraded = False
    prev_wheel_used = False

    if prev_data_dir and prev_data_dir.exists():
        shutil.copytree(prev_data_dir, persist, dirs_exist_ok=True)
        created_with = "external_data_dir"
    elif prev_wheel and Path(prev_wheel).exists():
        build_dir = persist / "seed"
        build_dir.mkdir()
        subprocess.run(
            [sys.executable, "-m", "pip", "install", "-q", "--force-reinstall", prev_wheel],
            check=True,
        )
        subprocess.run(
            [sys.executable, str(SCRIPT_DIR / "cross_version_seed.py"), str(build_dir)],
            check=True,
        )
        shutil.copytree(build_dir, persist, dirs_exist_ok=True)
        created_with = f"wheel:{prev_wheel}"
        prev_wheel_used = True
    else:
        seed_cross_version_dir(qm_engine, persist)
        created_with = "current_seed_v2_checkpoint"
        v1_downgraded = downgrade_search_checkpoint_to_v1(persist)

    upgraded = make_engine(qm_engine, persist)
    checks = run_cross_version_checks(upgraded)
    ok = bool(
        checks["row_count"]
        and checks["fts"]
        and checks["json_path"]
        and checks["vector"]
    )
    return {
        "ok": ok,
        "created_with": created_with,
        "v1_checkpoint_downgraded": v1_downgraded,
        "prev_wheel_used": prev_wheel_used,
        "data_dir": str(persist),
        "checks": checks,
    }


def npm_smoke() -> dict[str, Any]:
    npm_dir = SCRIPT_DIR.parent / "npm"
    if not npm_dir.exists():
        return {"ok": False, "skipped": True, "reason": "npm/ directory missing"}
    try:
        import qm_engine  # type: ignore

        engine = qm_engine.NativeSqlEngine()
        engine.execute("CREATE TABLE npm_smoke (id INTEGER PRIMARY KEY, v INTEGER)")
        engine.execute("INSERT INTO npm_smoke (id, v) VALUES (1, 42)")
        val = cell_text(engine.execute("SELECT v FROM npm_smoke WHERE id = 1"))
        py_ok = val == "42"
    except Exception as exc:
        return {"ok": False, "python_import_ok": False, "error": str(exc)}

    pkg = json.loads((npm_dir / "package.json").read_text())
    repo_version = read_repo_version()
    py_version = None
    installed_versions: dict[str, str] = {}
    try:
        import importlib.metadata as md

        for dist in ("qmvir", "qm_engine"):
            try:
                installed_versions[dist] = md.version(dist)
            except Exception:
                pass
        py_version = installed_versions.get("qmvir") or installed_versions.get("qm_engine")
    except Exception:
        pass

    expected = pkg.get("version")
    version_aligned = expected == repo_version and (
        not installed_versions or expected in installed_versions.values()
    )
    return {
        "ok": py_ok and version_aligned,
        "python_smoke_ok": py_ok,
        "npm_package_version": expected,
        "repo_pyproject_version": repo_version,
        "installed_qmvir_version": py_version,
        "installed_distributions": installed_versions,
        "version_aligned": version_aligned,
    }


def run_all(
    qm_engine: Any,
    *,
    quick: bool = False,
    prev_data_dir: Path | None = None,
    prev_wheel: str | None = None,
) -> dict[str, Any]:
    row_targets = [50_000] if quick else [100_000, 500_000]
    bulk_batch = 2000 if quick else 1000
    vec_rows = 10_000 if quick else 50_000
    fidelity_rows = 2000 if quick else 5000

    sections = {
        "bulk_ingest": bulk_ingest_profile(qm_engine, row_targets=row_targets, batch_size=bulk_batch),
        "vector_torture": vector_torture(qm_engine, bulk_rows=vec_rows),
        "search_index_fidelity": search_index_fidelity_after_reload(qm_engine, rows=fidelity_rows),
        "cross_version_upgrade": cross_version_upgrade(
            qm_engine, prev_data_dir=prev_data_dir, prev_wheel=prev_wheel
        ),
        "npm_smoke": npm_smoke(),
    }
    passed = sum(1 for s in sections.values() if s.get("ok"))
    total = sum(1 for s in sections.values() if not s.get("skipped"))
    return {
        "ok": passed == total and total > 0,
        "passed": passed,
        "total": total,
        "sections": sections,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="P0 real-data release gate")
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--output", type=Path, default=Path("/tmp/qm_release_gate_realdata.json"))
    parser.add_argument("--prev-data-dir", type=Path, default=None)
    parser.add_argument("--prev-wheel", type=str, default=os.environ.get("QM_PREV_WHEEL"))
    args = parser.parse_args()

    try:
        import qm_engine  # type: ignore
    except Exception as exc:
        print(f"failed to import qm_engine: {exc}", file=sys.stderr)
        return 2

    payload = {
        "label": "RELEASE_GATE_REALDATA",
        "mode": "quick" if args.quick else "full",
        "checklist": RELEASE_GATE_CHECKLIST,
        "environment": environment_info(),
        **run_all(
            qm_engine,
            quick=args.quick,
            prev_data_dir=args.prev_data_dir,
            prev_wheel=args.prev_wheel,
        ),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    text = json.dumps(payload, indent=2, sort_keys=True)
    args.output.write_text(text + "\n")
    print(text)
    return 0 if payload.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
