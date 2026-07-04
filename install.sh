#!/bin/bash
# QMvir Quick Installer — downloads bare binaries from public qmvir-releases.
# Usage: curl -fsSL https://raw.githubusercontent.com/virgori/qmvir-releases/main/install.sh | bash

set -e

VERSION="${1:-latest}"
REPO="virgori/qmvir-releases"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

detect_platform() {
    OS=$(uname -s | tr '[:upper:]' '[:lower:]')
    ARCH=$(uname -m)

    case "$OS" in
        darwin)
            case "$ARCH" in
                arm64)  echo "qm-macos-arm64" ;;
                x86_64) echo "qm-macos-x86_64" ;;
                *) echo "unsupported"; exit 1 ;;
            esac
            ;;
        linux)
            case "$ARCH" in
                x86_64) echo "qm-linux-x86_64" ;;
                aarch64) echo "qm-linux-aarch64" ;;
                *) echo "unsupported"; exit 1 ;;
            esac
            ;;
        mingw*|msys*|cygwin*)
            case "$ARCH" in
                x86_64) echo "qm-windows-x86_64.exe" ;;
                aarch64|arm64) echo "qm-windows-aarch64.exe" ;;
                *) echo "unsupported"; exit 1 ;;
            esac
            ;;
        *)
            echo "unsupported"
            exit 1
            ;;
    esac
}

ARTIFACT=$(detect_platform)
echo "Detected artifact: $ARTIFACT"

if [ "$VERSION" = "latest" ]; then
    URL="https://github.com/$REPO/releases/latest/download/$ARTIFACT"
else
    URL="https://github.com/$REPO/releases/download/$VERSION/$ARTIFACT"
fi

echo "Downloading: $URL"
mkdir -p "$INSTALL_DIR"
TMP=$(mktemp)
trap 'rm -f "$TMP"' EXIT
curl -fsSL "$URL" -o "$TMP"
chmod +x "$TMP"
install_name="qm"
[[ "$ARTIFACT" == *.exe ]] && install_name="qm.exe"
mv "$TMP" "$INSTALL_DIR/$install_name"
echo "✓ Installed $INSTALL_DIR/$install_name"
echo "  $("$INSTALL_DIR/$install_name" --version 2>/dev/null || true)"
echo ""
echo "Add to PATH if needed:"
echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
