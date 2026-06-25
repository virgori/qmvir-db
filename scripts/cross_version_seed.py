#!/usr/bin/env python3
"""Build a persistent data_dir artifact for cross-version upgrade tests."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("data_dir", type=Path)
    args = parser.parse_args()

    try:
        import qm_engine  # type: ignore
    except ImportError as exc:
        print(f"qm_engine not installed: {exc}", file=sys.stderr)
        return 2

    data_dir = args.data_dir
    data_dir.mkdir(parents=True, exist_ok=True)
    engine = qm_engine.NativeSqlEngine(str(data_dir))
    if hasattr(engine, "set_wal_sync_policy"):
        engine.set_wal_sync_policy("per_commit_sync")

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
    print(str(data_dir))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
