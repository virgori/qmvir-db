#!/usr/bin/env bash
# Publish qmvir 6.x: GitHub release binaries, npm, and PyPI.
# Tokens from repo-root .env:
#   github_token=...  (ghp_... — repo scope on virgori/qmvir-releases)
#   npmjs_token=...   (npm automation token)
#   pypi_token=...    (PyPI API token, pypi-AgE...)
#
# Usage:
#   cp .env.example .env   # fill tokens
#   bash scripts/publish_packages.sh
#   bash scripts/publish_packages.sh --github-only
#   bash scripts/publish_packages.sh --npm-only
#   bash scripts/publish_packages.sh --pypi-only
#   bash scripts/publish_packages.sh --skip-github
#   bash scripts/publish_packages.sh --dry-run
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

GITHUB_ONLY=0
NPM_ONLY=0
PYPI_ONLY=0
SKIP_GITHUB=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --github-only) GITHUB_ONLY=1 ;;
    --npm-only) NPM_ONLY=1 ;;
    --pypi-only) PYPI_ONLY=1 ;;
    --skip-github) SKIP_GITHUB=1 ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help)
      sed -n '2,16p' "$0"
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

# shellcheck disable=SC1091
set -a
source "$ROOT/.env"
set +a

GITHUB_TOKEN_RESOLVED="${github_token:-${GITHUB_TOKEN:-${GH_TOKEN:-}}}"
NPM_TOKEN_RESOLVED="${npmjs_token:-${NPM_TOKEN:-}}"
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
if [[ "$DO_NPM" -eq 1 && -z "$NPM_TOKEN_RESOLVED" ]]; then
  if ! npm whoami --registry https://registry.npmjs.org/ >/dev/null 2>&1; then
    echo "npm: run 'npm login' first, or set npmjs_token in .env" >&2
    exit 1
  fi
fi
if [[ "$DO_PYPI" -eq 1 && -z "$PYPI_TOKEN_RESOLVED" ]]; then
  echo "pypi_token (or PYPI_TOKEN) is empty in .env" >&2
  exit 1
fi

VERSION="$(python3 -c "import tomllib; print(tomllib.load(open('pyproject.toml','rb'))['project']['version'])")"
NPM_VERSION="$(python3 -c "import json; print(json.load(open('npm/package.json'))['version'])")"
if [[ "$VERSION" != "$NPM_VERSION" ]]; then
  echo "version mismatch: pyproject=$VERSION npm/package.json=$NPM_VERSION" >&2
  exit 1
fi
TAG="v$VERSION"
GITHUB_REPO="virgori/qmvir-releases"
BIN_DIR="$ROOT/build/release"
echo "publish target version: $VERSION"

publish_github_release() {
  if ! command -v gh >/dev/null 2>&1; then
    echo "gh CLI not found — install: brew install gh" >&2
    exit 1
  fi

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
    echo "build first: bash qm_engine/scripts/build_release.sh" >&2
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
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "dry-run: would npm publish from npm/"
  else
    (
      cd "$ROOT/npm"
      if npm whoami --registry https://registry.npmjs.org/ >/dev/null 2>&1; then
        echo "npm auth: using logged-in session ($(npm whoami --registry https://registry.npmjs.org/))"
        unset NODE_AUTH_TOKEN
      elif [[ -n "$NPM_TOKEN_RESOLVED" ]]; then
        echo "npm auth: using npmjs_token from .env (run 'npm login' once if publish returns 404)"
        export NODE_AUTH_TOKEN="$NPM_TOKEN_RESOLVED"
      else
        echo "error: npm not authenticated — run: npm login" >&2
        exit 1
      fi
      if [[ -f sdk/client.ts ]]; then
        npx -p typescript tsc --declaration --module nodenext --target es2020 \
          --moduleResolution nodenext --esModuleInterop --outDir sdk sdk/client.ts 2>/dev/null || true
      fi
      npm publish --access public
    )
    echo "npm publish done"
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
