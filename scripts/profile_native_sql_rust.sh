#!/usr/bin/env bash
set -euo pipefail

TARGET="${1:-native_sql_core_bench}"
ITERATIONS="${ITERATIONS:-100000}"
OUT_DIR="${OUT_DIR:-docs/profiles}"
mkdir -p "${OUT_DIR}"

JSON_OUT="${OUT_DIR}/${TARGET}_timing_summary.json"
TEXT_OUT="${OUT_DIR}/${TARGET}_profile_summary.txt"
FLAME_OUT="${OUT_DIR}/${TARGET}_flamegraph.svg"

{
  echo "target=${TARGET}"
  echo "iterations=${ITERATIONS}"
  echo "date=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "rust=$(rustc --version 2>/dev/null || true)"
  echo "os=$(uname -a)"
} > "${TEXT_OUT}"

if command -v cargo-flamegraph >/dev/null 2>&1; then
  cargo flamegraph \
    --manifest-path qm_engine/Cargo.toml \
    --release \
    --no-default-features \
    --bin native_sql_core_bench \
    --output "${FLAME_OUT}" \
    -- \
    --iterations "${ITERATIONS}" \
    --output "${JSON_OUT}" >> "${TEXT_OUT}" 2>&1
elif command -v samply >/dev/null 2>&1; then
  echo "samply is available; run manually for an interactive profile:" >> "${TEXT_OUT}"
  echo "samply record cargo run --manifest-path qm_engine/Cargo.toml --release --no-default-features --bin native_sql_core_bench -- --iterations ${ITERATIONS} --output ${JSON_OUT}" >> "${TEXT_OUT}"
  cargo run --quiet \
    --manifest-path qm_engine/Cargo.toml \
    --release \
    --no-default-features \
    --bin native_sql_core_bench \
    -- \
    --iterations "${ITERATIONS}" \
    --output "${JSON_OUT}" >> "${TEXT_OUT}" 2>&1
elif command -v instruments >/dev/null 2>&1; then
  echo "macOS Instruments is available; CLI template selection is environment-specific." >> "${TEXT_OUT}"
  cargo run --quiet \
    --manifest-path qm_engine/Cargo.toml \
    --release \
    --no-default-features \
    --bin native_sql_core_bench \
    -- \
    --iterations "${ITERATIONS}" \
    --output "${JSON_OUT}" >> "${TEXT_OUT}" 2>&1
else
  echo "No flamegraph/samply/instruments profiler found; writing timing summary only." >> "${TEXT_OUT}"
  cargo run --quiet \
    --manifest-path qm_engine/Cargo.toml \
    --release \
    --no-default-features \
    --bin native_sql_core_bench \
    -- \
    --iterations "${ITERATIONS}" \
    --output "${JSON_OUT}" >> "${TEXT_OUT}" 2>&1
fi

echo "summary=${TEXT_OUT}"
echo "timing_json=${JSON_OUT}"
if [ -f "${FLAME_OUT}" ]; then
  echo "flamegraph=${FLAME_OUT}"
fi
