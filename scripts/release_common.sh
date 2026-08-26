#!/usr/bin/env bash
# Shared release helpers — source from build/publish scripts (do not execute directly).
# Usage:
#   source "$(dirname "$0")/../scripts/release_common.sh"   # from qm_engine/scripts/
#   source "$(dirname "$0")/release_common.sh"              # from scripts/
set -euo pipefail

release_root() {
  if [[ -n "${QM_ROOT:-}" ]]; then
    echo "$QM_ROOT"
    return 0
  fi
  local here="${BASH_SOURCE[1]:-${BASH_SOURCE[0]}}"
  local dir
  dir="$(cd "$(dirname "$here")" && pwd)"
  if [[ -f "$dir/qm_engine/Cargo.toml" ]]; then
    echo "$dir"
  elif [[ -f "$dir/../qm_engine/Cargo.toml" ]]; then
    cd "$dir/.." && pwd
  else
    echo "release_common: cannot locate repo root from $here" >&2
    return 1
  fi
}

QM_ROOT="$(release_root)"
QM_ENGINE="$QM_ROOT/qm_engine"
QM_OUT="${QM_RELEASE_OUT:-$QM_ROOT/build/release}"

# Canonical standalone binary artifact names.
QM_CANONICAL_ARTIFACTS=(
  qm-macos-arm64
  qm-macos-x86_64
  qm-linux-x86_64
  qm-linux-aarch64
  qm-windows-x86_64.exe
  qm-windows-aarch64.exe
)

QM_VERSION_REGEX='^qm[[:space:]]+([0-9]+\.[0-9]+\.[0-9]+)$'

release_read_cargo_version() {
  grep '^version' "$QM_ENGINE/Cargo.toml" | head -1 | sed 's/.*"\(.*\)".*/\1/'
}

release_verify_manifest_versions() {
  local want="$1"
  local cargo
  cargo="$(python3 -c "import tomllib; print(tomllib.load(open('$QM_ENGINE/Cargo.toml','rb'))['package']['version'])")"
  if [[ "$cargo" != "$want" ]]; then
    echo "ERROR: version mismatch (want $want): cargo=$cargo" >&2
    return 1
  fi
}

release_sync_versions() {
  local ver="$1"
  python3 - "$ver" "$QM_ROOT" <<'PY'
import pathlib, re, sys, tomllib

ver, root = sys.argv[1], pathlib.Path(sys.argv[2])
cargo = root / "qm_engine/Cargo.toml"
text = cargo.read_text()
text = re.sub(r'^version = "[0-9]+\.[0-9]+\.[0-9]+"', f'version = "{ver}"', text, count=1, flags=re.M)
cargo.write_text(text)
print(f"synced Cargo.toml to {ver}")
PY
}

release_bump_patch() {
  local ver major minor patch
  ver="$(release_read_cargo_version)"
  IFS='.' read -r major minor patch <<<"$ver"
  patch=$((patch + 1))
  ver="${major}.${minor}.${patch}"
  release_sync_versions "$ver" >&2
  echo "$ver"
}

release_prepare_version() {
  local bump="${QM_RELEASE_BUMP:-1}"
  for arg in "${RELEASE_ARGS[@]:-}"; do
    [[ "$arg" == "--no-bump" ]] && bump=0
    [[ "$arg" == "--bump" ]] && bump=1
  done
  local ver
  if [[ "$bump" -eq 1 ]]; then
    ver="$(release_bump_patch)"
    echo "=== bumped release version -> $ver ==="
  else
    ver="$(release_read_cargo_version)"
    release_verify_manifest_versions "$ver"
    echo "=== release version (no bump): $ver ==="
  fi
  RELEASE_VERSION="$ver"
  export RELEASE_VERSION
}

release_validate_artifact_name() {
  local name="$1"
  case "$name" in
    qm-macos-arm64|qm-macos-x86_64|qm-linux-x86_64|qm-linux-aarch64|qm-windows-x86_64.exe|qm-windows-aarch64.exe)
      return 0
      ;;
    qm-linux-arm64|qm-linux-x64|qm-linux-*\ *)
      echo "ERROR: wrong binary name '$name' — use qm-linux-aarch64 or qm-linux-x86_64" >&2
      return 1
      ;;
    *)
      echo "ERROR: unknown artifact name '$name' (not in canonical list)" >&2
      return 1
      ;;
  esac
}

release_versioned_name() {
  local canonical="$1"
  local ver="$2"
  if [[ "$canonical" == *.exe ]]; then
    echo "${canonical%.exe}-${ver}.exe"
  else
    echo "${canonical}-${ver}"
  fi
}

release_install_binary() {
  local src="$1"
  local canonical="$2"
  local ver="${3:-$RELEASE_VERSION}"
  release_validate_artifact_name "$canonical"
  mkdir -p "$QM_OUT"
  cp "$src" "$QM_OUT/$canonical"
  chmod +x "$QM_OUT/$canonical" 2>/dev/null || true
  local versioned
  versioned="$(release_versioned_name "$canonical" "$ver")"
  cp "$src" "$QM_OUT/$versioned"
  chmod +x "$QM_OUT/$versioned" 2>/dev/null || true
  echo "  → $QM_OUT/$canonical (+ $versioned)"
}

release_verify_binary() {
  local path="$1"
  local want="${2:-$RELEASE_VERSION}"
  [[ -f "$path" ]] || { echo "ERROR: missing binary $path" >&2; return 1; }
  if [[ ! -x "$path" ]]; then
    if command -v file >/dev/null 2>&1 && file "$path" | grep -qE 'executable|ELF|Mach-O|PE32'; then
      echo "  skip exec verify (not runnable on this host): $(basename "$path")"
      return 0
    fi
    echo "ERROR: $(basename "$path") is not executable" >&2
    return 1
  fi
  local out got
  out="$("$path" --version 2>/dev/null || true)"
  if [[ ! "$out" =~ $QM_VERSION_REGEX ]]; then
    if command -v file >/dev/null 2>&1; then
      local kind host
      kind="$(file -b "$path" 2>/dev/null || true)"
      host="$(uname -m 2>/dev/null || true)"
      case "$kind" in
        *ELF*)
          if [[ "$(uname -s 2>/dev/null || true)" != "Linux" ]]; then
            echo "  skip exec verify (ELF on $(uname -s)): $(basename "$path")"
            return 0
          fi
          ;;
        *x86-64*|*x86_64*)
          if [[ "$host" != "x86_64" && "$host" != "amd64" ]]; then
            echo "  skip exec verify (cross x86_64 on $host): $(basename "$path")"
            return 0
          fi
          ;;
        *ARM\ aarch64*|*aarch64*|*ARM64*)
          if [[ "$host" != "aarch64" && "$host" != "arm64" ]]; then
            echo "  skip exec verify (cross aarch64 on $host): $(basename "$path")"
            return 0
          fi
          ;;
        *PE32+*)
          echo "  skip exec verify (windows PE on $(uname -s)): $(basename "$path")"
          return 0
          ;;
      esac
    fi
    echo "ERROR: $(basename "$path") --version invalid: '$out' (expected 'qm $want')" >&2
    return 1
  fi
  got="${BASH_REMATCH[1]}"
  if [[ "$got" != "$want" ]]; then
    echo "ERROR: $(basename "$path") reports $got, want $want" >&2
    return 1
  fi
  echo "  verified $(basename "$path"): qm $got"
}

release_stamp_and_manifest() {
  local ver="${1:-$RELEASE_VERSION}"
  mkdir -p "$QM_OUT"
  echo "$ver" > "$QM_OUT/.version"
  python3 - "$QM_OUT" "$ver" <<'PY'
import hashlib, json, sys, time
from pathlib import Path

out = Path(sys.argv[1])
ver = sys.argv[2]
canonical = {
    "qm-macos-arm64", "qm-macos-x86_64",
    "qm-linux-x86_64", "qm-linux-aarch64",
    "qm-windows-x86_64.exe", "qm-windows-aarch64.exe",
}
artifacts = []
for path in sorted(out.iterdir()):
    if not path.is_file() or path.name in (".version", "manifest.json"):
        continue
    base = path.name
    if base not in canonical and not (base.endswith(f"-{ver}") or base.endswith(f"-{ver}.exe")):
        continue
    artifacts.append({
        "name": base,
        "version": ver,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "bytes": path.stat().st_size,
    })
manifest = {
    "version": ver,
    "built_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    "artifacts": artifacts,
}
(out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(f"manifest: {out / 'manifest.json'} ({len(artifacts)} artifacts)")
PY
  echo "stamped $QM_OUT/.version = $ver"
}

release_verify_all_binaries() {
  local ver="${1:-$RELEASE_VERSION}"
  local stamp="$QM_OUT/.version"
  if [[ ! -f "$stamp" ]]; then
    echo "ERROR: missing $stamp — run a release build first" >&2
    return 1
  fi
  local got
  got="$(tr -d '[:space:]' < "$stamp")"
  if [[ "$got" != "$ver" ]]; then
    echo "ERROR: $stamp=$got but expected $ver" >&2
    return 1
  fi
  local any=0
  for name in "${QM_CANONICAL_ARTIFACTS[@]}"; do
    local path="$QM_OUT/$name"
    [[ -f "$path" ]] || continue
    release_verify_binary "$path" "$ver" || return 1
    any=1
  done
  if [[ "$any" -eq 0 ]]; then
    echo "WARN: no canonical binaries present under $QM_OUT" >&2
  fi
  if [[ -f "$QM_OUT/manifest.json" ]]; then
    echo "  manifest OK"
  fi
}

release_clean_stale_names() {
  rm -f "$QM_OUT/qm-linux-arm64" "$QM_OUT/qm-linux-x64" "$QM_OUT"/qm-linux-*\ 2>/dev/null || true
}

# Remove all release artifacts so a new build cannot mix with stale binaries.
release_wipe_output_dir() {
  mkdir -p "$QM_OUT"
  find "$QM_OUT" -maxdepth 1 -type f \( \
    -name 'qm-*' -o -name '.version' -o -name 'manifest.json' \
  \) -delete 2>/dev/null || true
  release_clean_stale_names
  echo "wiped $QM_OUT (qm-* / .version / manifest.json)"
}

# Drop version-suffixed copies and wrong names; keep only canonical + stamp for $ver.
release_prune_output_dir() {
  local ver="${1:-$RELEASE_VERSION}"
  local name path base
  for path in "$QM_OUT"/qm-*; do
    [[ -f "$path" ]] || continue
    base="$(basename "$path")"
    case "$base" in
      qm-macos-arm64|qm-macos-x86_64|qm-linux-x86_64|qm-linux-aarch64|qm-windows-x86_64.exe|qm-windows-aarch64.exe)
        continue
        ;;
      qm-linux-arm64|qm-linux-x64)
        rm -f "$path"
        ;;
      *)
        if [[ "$base" == *"-${ver}" || "$base" == *"-${ver}.exe" ]]; then
          continue
        fi
        rm -f "$path"
        ;;
    esac
  done
  release_clean_stale_names
}
