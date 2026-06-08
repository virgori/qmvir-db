#!/usr/bin/env bash
set -euo pipefail

SNAPSHOT="${1:-.}"
cd "$SNAPSHOT"

cargo check --manifest-path qm_engine/Cargo.toml --no-default-features
cargo check --manifest-path qm_engine/Cargo.toml
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_recovery
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features --test release_crash_kill_recovery
python3 -m pytest -q
bash scripts/check_no_space_number_duplicates.sh
python3 scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default --output docs/native_sql_benchmark_last.json

rm -rf qm_engine/target .pytest_cache
find . -type d -name __pycache__ -prune -exec rm -rf {} +
find . -type f \( -name '*.pyc' -o -name '*.pyo' \) -delete

python3 scripts/check_local_release_snapshot.py .
