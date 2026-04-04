#!/bin/bash
# MemFS install — downloads binaries, then runs `memfs init` for setup.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Haakam21/mem-fs/main/install.sh | bash
#
# Prerequisites:
#   - macFUSE (macOS): https://macfuse.io
#   - libfuse (Linux): apt install fuse3
#   - gh CLI: https://cli.github.com

set -euo pipefail

BIN_DIR="$HOME/.memfs"
REPO="Haakam21/mem-fs"

# --- Detect platform ---

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
    Darwin-arm64)  ARTIFACT="memfs-darwin-arm64" ;;
    Darwin-x86_64) ARTIFACT="memfs-darwin-x86_64" ;;
    Linux-x86_64)  ARTIFACT="memfs-linux-x86_64" ;;
    *) echo "Error: unsupported platform $OS-$ARCH"; exit 1 ;;
esac

# --- Check prerequisites ---

if [[ "$OS" == "Darwin" ]]; then
    if [[ ! -d /Library/Frameworks/macFUSE.framework ]]; then
        echo "Error: macFUSE not installed. Download from https://macfuse.io"
        echo "  After install, approve the kernel extension in System Settings > Privacy & Security"
        exit 1
    fi
else
    if ! command -v fusermount &>/dev/null && ! command -v fusermount3 &>/dev/null; then
        echo "Error: FUSE not installed. Install with: apt install fuse3"
        exit 1
    fi
fi

if ! command -v gh &>/dev/null; then
    echo "Error: gh CLI not found. Install from https://cli.github.com"
    exit 1
fi

# --- Download binaries ---

mkdir -p "$BIN_DIR"
echo "Downloading memfs ($ARTIFACT)..."
gh release download --repo "$REPO" --pattern "$ARTIFACT" --dir "$BIN_DIR" --clobber
mv "$BIN_DIR/$ARTIFACT" "$BIN_DIR/memfs"
chmod +x "$BIN_DIR/memfs"

# Install search binary to PATH
mkdir -p "$HOME/.local/bin"
SEARCH_ARTIFACT="search-${ARTIFACT#memfs-}"
echo "Downloading search ($SEARCH_ARTIFACT)..."
gh release download --repo "$REPO" --pattern "$SEARCH_ARTIFACT" --dir "$HOME/.local/bin" --clobber 2>/dev/null && \
    mv "$HOME/.local/bin/$SEARCH_ARTIFACT" "$HOME/.local/bin/search" && \
    chmod +x "$HOME/.local/bin/search" || true

echo ""
echo "Binaries installed. Now run:"
echo ""
echo "  ~/.memfs/memfs init"
echo ""
