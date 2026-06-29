#!/usr/bin/env bash
# Publish qmvir: GitHub release binaries, npm, and PyPI.
# Tokens from repo-root .env:
#   github_token=...  (ghp_... — repo scope on virgori/qmvir-releases)
#   npmjs_token=...   (npm automation token — tried first)
#   npm_bypass=...    (granular token with Bypass 2FA — tried before npmjs_token)
#   pypi_token=...    (PyPI API token)
#
# npm auth: token (.env) → Mac ~/.npmrc → browser (--npm-login / --npm-mac)
# EOTP: create Granular token with "Bypass 2FA" on npmjs.com, or npm login --auth-type=web on Mac
#
# Usage:
#   cp .env.example .env && fill tokens
#   bash scripts/publish_packages.sh
#   bash scripts/publish_packages.sh --npm-only
#   bash scripts/publish_packages.sh --npm-login   # force browser login
#   bash scripts/publish_packages.sh --github-only
#   bash scripts/publish_packages.sh --pypi-only
#   bash scripts/publish_packages.sh --dry-run
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

GITHUB_ONLY=0
NPM_ONLY=0
PYPI_ONLY=0
SKIP_GITHUB=0
DRY_RUN=0
NPM_LOGIN=0
NPM_SESSION=0
NPM_OTP=""
for arg in "$@"; do
  case "$arg" in
    --github-only) GITHUB_ONLY=1 ;;
    --npm-only) NPM_ONLY=1 ;;
    --pypi-only) PYPI_ONLY=1 ;;
    --skip-github) SKIP_GITHUB=1 ;;
    --dry-run) DRY_RUN=1 ;;
    --npm-login) NPM_LOGIN=1 ;;
    --npm-session|--npm-mac) NPM_SESSION=1 ;;
    --otp=*) NPM_OTP="${arg#--otp=}" ;;
    -h|--help)
      sed -n '2,18p' "$0"
      exit 0
      ;;
    *)
      echo "unknown arg: $arg (try --help)" >&2
      exit 2
      ;;
  esac
done

if [[ "$GITHUB_ONLY" -eq 1 && ( "$NPM_ONLY" -eq 1 || "$PYPI_ONLY" -eq 1 ) ]]; then
  echo "--github-only cannot combine with --npm-only / --pypi-only" >&2
  exit 2
fi

if [[ ! -f "$ROOT/.env" ]]; then
  echo "missing $ROOT/.env — copy .env.example and set tokens" >&2
  exit 1
fi

# Load .env — skip keys like qmvir-6.2= that are not valid bash identifiers
while IFS= read -r line || [[ -n "$line" ]]; do
  line="${line%%#*}"
  line="$(echo "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
  [[ -z "$line" ]] && continue
  if [[ "$line" =~ ^[A-Za-z_][A-Za-z0-9_]*= ]]; then
    export "$line"
  fi
done < "$ROOT/.env"

GITHUB_TOKEN_RESOLVED="${github_token:-${GITHUB_TOKEN:-${GH_TOKEN:-}}}"
NPM_JS_TOKEN="${npmjs_token:-${NPM_TOKEN:-}}"
NPM_BYPASS_TOKEN="${npm_bypass:-${NPM_BYPASS:-}}"
# npm publish token — prefer generic key; legacy qmvir-6.2= still supported
NPM_QMVIR_TOKEN="$(grep -E '^npm_publish_token=' "$ROOT/.env" 2>/dev/null | cut -d= -f2- | tr -d '\r\n' || true)"
if [[ -z "$NPM_QMVIR_TOKEN" ]]; then
  NPM_QMVIR_TOKEN="$(grep '^qmvir-6\.2=' "$ROOT/.env" 2>/dev/null | cut -d= -f2- | tr -d '\r\n' || true)"
fi
PYPI_TOKEN_RESOLVED="${pypi_token:-${PYPI_TOKEN:-}}"

DO_GITHUB=0
DO_NPM=0
DO_PYPI=0
if [[ "$GITHUB_ONLY" -eq 1 ]]; then
  DO_GITHUB=1
elif [[ "$NPM_ONLY" -eq 1 ]]; then
  DO_NPM=1
elif [[ "$PYPI_ONLY" -eq 1 ]]; then
  DO_PYPI=1
else
  [[ "$SKIP_GITHUB" -eq 0 ]] && DO_GITHUB=1
  DO_NPM=1
  DO_PYPI=1
fi

if [[ "$DO_GITHUB" -eq 1 && -z "$GITHUB_TOKEN_RESOLVED" ]]; then
  echo "github_token (or GITHUB_TOKEN / GH_TOKEN) is empty in .env" >&2
  exit 1
fi
if [[ "$DO_PYPI" -eq 1 && -z "$PYPI_TOKEN_RESOLVED" ]]; then
  echo "pypi_token (or PYPI_TOKEN) is empty in .env" >&2
  exit 1
fi

VERSION="$(python3 -c "import tomllib; print(tomllib.load(open('pyproject.toml','rb'))['project']['version'])")"
NPM_VERSION="$(python3 -c "import json; print(json.load(open('npm/package.json'))['version'])")"
CARGO_VERSION="$(python3 -c "import tomllib; print(tomllib.load(open('qm_engine/Cargo.toml','rb'))['package']['version'])")"
if [[ "$VERSION" != "$NPM_VERSION" || "$VERSION" != "$CARGO_VERSION" ]]; then
  echo "version mismatch: pyproject=$VERSION npm=$NPM_VERSION cargo=$CARGO_VERSION" >&2
  echo "sync: pyproject.toml, npm/package.json, qm_engine/Cargo.toml, qm_app.py" >&2
  exit 1
fi
TAG="v$VERSION"
GITHUB_REPO="virgori/qmvir-releases"
BIN_DIR="$ROOT/build/release"
echo "publish target version: $VERSION"

verify_release_binaries() {
  local want="$1"
  QM_ROOT="$ROOT"
  # shellcheck source=release_common.sh
  source "$ROOT/scripts/release_common.sh"
  RELEASE_VERSION="$want"
  release_verify_manifest_versions "$want"
  release_verify_all_binaries "$want"
}

# Sets NPM_AUTH_MODE: token | session | browser
NPM_AUTH_MODE=""
_try_npm_token() {
  local token=$1
  [[ -n "$token" ]] || return 1
  export NODE_AUTH_TOKEN="$token"
  npm whoami --registry https://registry.npmjs.org/ >/dev/null 2>&1
}

setup_npm_auth() {
  NPM_AUTH_MODE=""
  NPM_TOKEN_RESOLVED=""
  unset NODE_AUTH_TOKEN 2>/dev/null || true

  if [[ "$NPM_LOGIN" -eq 1 ]]; then
    echo "npm: browser login (--npm-login) — Mac Passkey / Touch ID OK..."
    npm login --auth-type=web --registry https://registry.npmjs.org/
    NPM_AUTH_MODE="browser"
    echo "npm auth: $(npm whoami --registry https://registry.npmjs.org/) (browser)"
    return 0
  fi

  if [[ "$NPM_SESSION" -eq 1 ]]; then
    unset NODE_AUTH_TOKEN 2>/dev/null || true
    if npm whoami --registry https://registry.npmjs.org/ >/dev/null 2>&1; then
      NPM_AUTH_MODE="session"
      echo "npm auth: Mac ~/.npmrc session ($(npm whoami --registry https://registry.npmjs.org/))"
      return 0
    fi
    echo "npm: no Mac session — opening browser login..."
    npm login --auth-type=web --registry https://registry.npmjs.org/
    NPM_AUTH_MODE="browser"
    echo "npm auth: $(npm whoami --registry https://registry.npmjs.org/) (browser)"
    return 0
  fi

  if [[ -n "$NPM_QMVIR_TOKEN" ]]; then
    _npm_use_bypass_npmrc
    NPM_AUTH_MODE="token"
    NPM_TOKEN_RESOLVED="$NPM_QMVIR_TOKEN"
    return 0
  fi

  if [[ -n "$NPM_BYPASS_TOKEN" ]]; then
    export NODE_AUTH_TOKEN="$NPM_BYPASS_TOKEN"
    NPM_AUTH_MODE="token"
    NPM_TOKEN_RESOLVED="$NPM_BYPASS_TOKEN"
    echo "npm auth: npm_bypass (bypass 2FA token)"
    return 0
  fi

  for label_token in \
    "npmjs_token:$NPM_JS_TOKEN"; do
    local label="${label_token%%:*}"
    local token="${label_token#*:}"
    if _try_npm_token "$token"; then
      NPM_AUTH_MODE="token"
      NPM_TOKEN_RESOLVED="$token"
      echo "npm auth: $label ($(npm whoami --registry https://registry.npmjs.org/))"
      return 0
    fi
  done
  unset NODE_AUTH_TOKEN 2>/dev/null || true

  if npm whoami --registry https://registry.npmjs.org/ >/dev/null 2>&1; then
    NPM_AUTH_MODE="session"
    echo "npm auth: existing session ($(npm whoami --registry https://registry.npmjs.org/))"
    return 0
  fi

  echo "npm: not authenticated — opening browser login..."
  npm login --auth-type=web --registry https://registry.npmjs.org/
  NPM_AUTH_MODE="browser"
  echo "npm auth: $(npm whoami --registry https://registry.npmjs.org/) (browser)"
}

_npm_pick_publish_token() {
  for t in "$NPM_QMVIR_TOKEN" "$NPM_BYPASS_TOKEN" "$NPM_JS_TOKEN"; do
    [[ -n "$t" ]] && { echo "$t"; return 0; }
  done
  return 1
}

_npm_use_bypass_npmrc() {
  local token
  token="$(_npm_pick_publish_token)" || return 1
  NPM_PUBLISH_NPMRC="$(mktemp)"
  printf '//registry.npmjs.org/:_authToken=%s\n' "$token" > "$NPM_PUBLISH_NPMRC"
  export NPM_CONFIG_USERCONFIG="$NPM_PUBLISH_NPMRC"
  unset NODE_AUTH_TOKEN 2>/dev/null || true
  if [[ "$token" == "$NPM_QMVIR_TOKEN" ]]; then
    echo "npm auth: npm_publish_token via .npmrc"
  else
    echo "npm auth: npm_bypass via .npmrc (no OTP)"
  fi
  return 0
}

_npm_cleanup_publish_npmrc() {
  [[ -n "${NPM_PUBLISH_NPMRC:-}" && -f "$NPM_PUBLISH_NPMRC" ]] && rm -f "$NPM_PUBLISH_NPMRC"
  unset NPM_PUBLISH_NPMRC NPM_CONFIG_USERCONFIG 2>/dev/null || true
}

publish_npm() {
  setup_npm_auth
  _npm_publish_once() {
    (
      cd "$ROOT/npm"
      if [[ "$NPM_AUTH_MODE" == "token" && -z "${NPM_CONFIG_USERCONFIG:-}" ]]; then
        export NODE_AUTH_TOKEN="$NPM_TOKEN_RESOLVED"
      else
        unset NODE_AUTH_TOKEN 2>/dev/null || true
      fi
      if [[ -f sdk/client.ts ]]; then
        npx -p typescript tsc --declaration --module nodenext --target es2020 \
          --moduleResolution nodenext --esModuleInterop --outDir sdk sdk/client.ts 2>/dev/null || true
      fi
      npm publish --access public ${NPM_OTP:+--otp="$NPM_OTP"}
    )
  }
  local log
  log="$(mktemp)"
  if _npm_publish_once 2> >(tee "$log" >&2); then
    rm -f "$log"
    _npm_cleanup_publish_npmrc
    return 0
  fi
  if grep -qE 'EOTP|one-time password' "$log"; then
    if _npm_use_bypass_npmrc; then
      echo "npm: browser/session hit EOTP — retry with npm_bypass .npmrc..." >&2
      NPM_AUTH_MODE="token"
      rm -f "$log"
      log="$(mktemp)"
      if _npm_publish_once 2> >(tee "$log" >&2); then
        rm -f "$log"
        _npm_cleanup_publish_npmrc
        return 0
      fi
    fi
    if [[ "$NPM_AUTH_MODE" == "token" ]] && [[ -n "$NPM_BYPASS_TOKEN" && "$NPM_TOKEN_RESOLVED" != "$NPM_BYPASS_TOKEN" ]]; then
      echo "npm: EOTP — retrying with npm_bypass token..." >&2
      NPM_AUTH_MODE="token"
      NPM_TOKEN_RESOLVED="$NPM_BYPASS_TOKEN"
      rm -f "$log"
      _npm_publish_once
      local pub_ec=$?
      _npm_cleanup_publish_npmrc
      return "$pub_ec"
    fi
    echo "npm: token requires OTP — retrying with Mac ~/.npmrc session (keychain)..." >&2
    unset NODE_AUTH_TOKEN 2>/dev/null || true
    if npm whoami --registry https://registry.npmjs.org/ >/dev/null 2>&1; then
      NPM_AUTH_MODE="session"
      echo "npm auth: Mac session ($(npm whoami --registry https://registry.npmjs.org/))"
      rm -f "$log"
      _npm_publish_once
      local pub_ec=$?
      _npm_cleanup_publish_npmrc
      return "$pub_ec"
    fi
    echo "npm: no Mac session — run: npm login --auth-type=web" >&2
  fi
  rm -f "$log"
  _npm_cleanup_publish_npmrc
  return 1
}

publish_github_release() {
  if ! command -v gh >/dev/null 2>&1; then
    echo "gh CLI not found — install: brew install gh" >&2
    exit 1
  fi

  verify_release_binaries "$VERSION"

  local expected=(
    "qm-macos-arm64"
    "qm-macos-x86_64"
    "qm-linux-x86_64"
    "qm-linux-aarch64"
    "qm-windows-x86_64.exe"
    "qm-windows-aarch64.exe"
  )
  local assets=()
  local name path
  for name in "${expected[@]}"; do
    path="$BIN_DIR/$name"
    if [[ -f "$path" ]]; then
      assets+=("$path")
    else
      echo "  skip missing: $name"
    fi
  done
  if [[ ${#assets[@]} -eq 0 ]]; then
    echo "no release binaries in $BIN_DIR" >&2
    echo "build first: bash scripts/sync_and_build_release_quizzman.sh [--mac-too]" >&2
    exit 1
  fi

  export GH_TOKEN="$GITHUB_TOKEN_RESOLVED"
  echo "=== GitHub ($GITHUB_REPO $TAG, ${#assets[@]} assets) ==="

  if [[ "$DRY_RUN" -eq 1 ]]; then
    printf 'dry-run: would upload to %s\n' "$GITHUB_REPO"
    printf '  %s\n' "${assets[@]}"
    return 0
  fi

  if gh release view "$TAG" --repo "$GITHUB_REPO" >/dev/null 2>&1; then
    gh release upload "$TAG" "${assets[@]}" --repo "$GITHUB_REPO" --clobber
  else
    gh release create "$TAG" "${assets[@]}" \
      --repo "$GITHUB_REPO" \
      --title "QMvir $TAG" \
      --notes "QMvir $TAG standalone binaries (npm postinstall downloads these)."
  fi
  echo "GitHub release done: https://github.com/$GITHUB_REPO/releases/tag/$TAG"
}

if [[ "$DO_GITHUB" -eq 1 ]]; then
  publish_github_release
fi

if [[ "$DO_NPM" -eq 1 ]]; then
  echo "=== npm (qmvir@$VERSION) ==="
  if [[ "$DRY_RUN" -eq 0 ]]; then
    verify_release_binaries "$VERSION"
  fi
  if [[ "$DRY_RUN" -eq 1 ]]; then
    if [[ -n "$NPM_BYPASS_TOKEN" || -n "$NPM_JS_TOKEN" ]]; then
      echo "dry-run: would npm publish using npm_bypass / npmjs_token from .env"
    else
      echo "dry-run: would npm login (if needed) && npm publish from npm/"
    fi
  else
    if ! publish_npm; then
      if [[ "$NPM_AUTH_MODE" == "token" ]]; then
        echo "npm publish failed with token — retry with browser login:" >&2
        echo "  bash scripts/publish_packages.sh --npm-only --npm-login" >&2
      fi
      exit 1
    fi
    echo "npm publish done: https://www.npmjs.com/package/qmvir/v/$VERSION"
  fi
fi

if [[ "$DO_PYPI" -eq 1 ]]; then
  echo "=== PyPI (qmvir==$VERSION) ==="
  python3 -m pip install -q -U maturin twine build 2>/dev/null || true
  rm -rf "$ROOT/dist"
  mkdir -p "$ROOT/dist"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "dry-run: would maturin build --release -o dist/ && twine upload dist/*"
  else
    python3 -m maturin build --release --out "$ROOT/dist"
    TWINE_USERNAME=__token__ TWINE_PASSWORD="$PYPI_TOKEN_RESOLVED" \
      python3 -m twine upload --non-interactive "$ROOT/dist"/*.whl
    echo "PyPI upload done"
  fi
fi

echo "All requested publishes finished for $VERSION"
