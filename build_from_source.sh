#!/bin/bash
# Build QMvir from source (requires access to private repo)
# Usage: ./build_from_source.sh [version]

set -e

VERSION="${1:-main}"
REPO_URL="https://github.com/virgori/qmvir-db"
BUILD_DIR=$(mktemp -d)

echo "Building QMvir from source..."
echo "Version: $VERSION"
echo "Build dir: $BUILD_DIR"

# Check dependencies
command -v git >/dev/null 2>&1 || { echo "git required"; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 required"; exit 1; }
command -v cargo >/dev/null 2>&1 || { echo "rust/cargo required: https://rustup.rs/"; exit 1; }

# Clone source
cd "$BUILD_DIR"
git clone --depth 1 --branch "$VERSION" "$REPO_URL" source 2>/dev/null || {
    echo "ERROR: Cannot access private repo $REPO_URL"
    echo "You need:"
    echo "  1. SSH key configured, OR"
    echo "  2. Personal Access Token with repo access"
    echo ""
    echo "Try: git clone https://<TOKEN>@github.com/virgori/qmvir-db"
    exit 1
}

cd source

# Run build
if [ -f "scripts/build_release.py" ]; then
    python3 scripts/build_release.py --install --verify
elif [ -f "build.sh" ]; then
    ./build.sh
else
    echo "No build script found"
    exit 1
fi

echo ""
echo "✓ Build complete!"
echo "Wheels in: $BUILD_DIR/source/dist/"
