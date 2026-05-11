#!/bin/bash
# QMvir Quick Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/virgori/qmvir-db/main/install.sh | bash

set -e

VERSION="${1:-latest}"
REPO="virgori/qmvir-db"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Detect platform
detect_platform() {
    OS=$(uname -s | tr '[:upper:]' '[:lower:]')
    ARCH=$(uname -m)
    
    case "$OS" in
        darwin)
            case "$ARCH" in
                arm64)  echo "macos-arm64" ;;
                x86_64) echo "macos-x86_64" ;;
                *) echo "unsupported"; exit 1 ;;
            esac
            ;;
        linux)
            case "$ARCH" in
                x86_64) echo "linux-x86_64" ;;
                aarch64) echo "linux-arm64" ;;
                *) echo "unsupported"; exit 1 ;;
            esac
            ;;
        *)
            echo "unsupported"
            exit 1
            ;;
    esac
}

PLATFORM=$(detect_platform)
echo "Detected platform: $PLATFORM"

# Get latest release URL
if [ "$VERSION" = "latest" ]; then
    URL="https://github.com/$REPO/releases/latest/download/qmvir-${PLATFORM}.tar.gz"
else
    URL="https://github.com/$REPO/releases/download/$VERSION/qmvir-${PLATFORM}.tar.gz"
fi

echo "Downloading from: $URL"

# Download and extract
TMP_DIR=$(mktemp -d)
trap "rm -rf $TMP_DIR" EXIT

curl -fsSL "$URL" -o "$TMP_DIR/qmvir.tar.gz"
tar -xzf "$TMP_DIR/qmvir.tar.gz" -C "$TMP_DIR"

# Install
mkdir -p "$INSTALL_DIR"
if [ -f "$TMP_DIR/qmvir" ]; then
    cp "$TMP_DIR/qmvir" "$INSTALL_DIR/"
    chmod +x "$INSTALL_DIR/qmvir"
    echo "✓ Installed qmvir to $INSTALL_DIR/qmvir"
else
    # Python wheels install
    pip install "$TMP_DIR"/*.whl --force-reinstall
    echo "✓ Installed QMvir Python packages"
fi

echo ""
echo "Add to PATH if needed:"
echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
