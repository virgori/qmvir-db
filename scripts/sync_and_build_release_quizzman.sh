#!/usr/bin/env bash
# Sync repo to quizzman, install build deps, compile Linux binaries, pull back.
set -euo pipefail

unset CARGO_TARGET_DIR 2>/dev/null || true

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
QM_ROOT="$ROOT"
SSH_HOST="${SSH_HOST:-quizzman}"
REMOTE_DIR="${REMOTE_DIR:-/root/qm-linux-bench}"
BUILD_MAC=0
BUMP_ARGS=()

for arg in "$@"; do
  case "$arg" in
    --mac-too) BUILD_MAC=1 ;;
    --bump) BUMP_ARGS+=(--bump) ;;
    --no-bump) BUMP_ARGS+=(--no-bump) ;;
    -h|--help)
      echo "Usage: $0 [--mac-too] [--bump|--no-bump]"
      echo "  --bump      bump patch version before remote build (default if neither flag)"
      echo "  --no-bump   keep current Cargo.toml version"
      exit 0
      ;;
  esac
done

if [[ " ${BUMP_ARGS[*]:-} " != *" --no-bump "* && " ${BUMP_ARGS[*]:-} " != *" --bump "* ]]; then
  BUMP_ARGS=(--bump)
fi

# shellcheck source=release_common.sh
source "$ROOT/scripts/release_common.sh"

echo "=== [1/4] rsync source -> ${SSH_HOST}:${REMOTE_DIR} ==="
rsync -az --delete \
  --exclude '.git' \
  --exclude 'target' \
  --exclude 'node_modules' \
  --exclude '_py_legacy' \
  --exclude 'build/release' \
  --exclude '.env' \
  --exclude 'dist' \
  "$ROOT/" "${SSH_HOST}:${REMOTE_DIR}/"

echo "=== [2/4] setup build env on ${SSH_HOST} ==="
ssh -o BatchMode=yes "${SSH_HOST}" "bash ${REMOTE_DIR}/scripts/setup_quizzman_build_env.sh"

echo "=== [3/4] release build on ${SSH_HOST} (linux + windows) ==="
ssh -o BatchMode=yes "${SSH_HOST}" \
  "bash ${REMOTE_DIR}/qm_engine/scripts/build_release_quizzman.sh ${BUMP_ARGS[*]:-}"

echo "=== [4/5] sync bumped manifests + binaries <- ${SSH_HOST} ==="
mkdir -p "$ROOT/build/release"
# Remote --bump updates manifests only on quizzman; pull them back before publish.
rsync -avz \
  "${SSH_HOST}:${REMOTE_DIR}/pyproject.toml" \
  "${SSH_HOST}:${REMOTE_DIR}/qm_app.py" \
  "$ROOT/"
rsync -avz \
  "${SSH_HOST}:${REMOTE_DIR}/npm/package.json" \
  "$ROOT/npm/"
rsync -avz \
  "${SSH_HOST}:${REMOTE_DIR}/qm_engine/Cargo.toml" \
  "$ROOT/qm_engine/"
# Fresh artifact dir — never merge with stale local qm-* from an older release.
rm -rf "$ROOT/build/release"
mkdir -p "$ROOT/build/release"
rsync -avz \
  "${SSH_HOST}:${REMOTE_DIR}/build/release/" \
  "$ROOT/build/release/"

if [[ "$BUILD_MAC" -eq 1 ]]; then
  echo ""
  echo "=== [5/5] macOS only (local; Linux/Windows already built on quizzman) ==="
  cd "$ROOT/qm_engine"
  bash scripts/build_release.sh --no-bump
fi

RELEASE_VERSION="$(tr -d '[:space:]' < "$ROOT/build/release/.version" 2>/dev/null || release_read_cargo_version)"
echo ""
echo "=== Version verify (v$RELEASE_VERSION) ==="
for f in "$ROOT/build/release"/qm-macos-*; do
  [[ -f "$f" ]] || continue
  release_verify_binary "$f" "$RELEASE_VERSION" || true
done
for f in "$ROOT/build/release"/qm-linux-*; do
  [[ -f "$f" ]] || continue
  base="$(basename "$f")"
  [[ "$base" =~ -[0-9]+\.[0-9]+\.[0-9]+$ ]] && continue
  ver="$(ssh -o BatchMode=yes "${SSH_HOST}" "${REMOTE_DIR}/build/release/${base} --version" 2>/dev/null || true)"
  echo "  $base: $ver"
done

echo ""
echo "Done. Binaries in $ROOT/build/release/ (manifest + .version synced)"
