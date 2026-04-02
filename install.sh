#!/bin/bash
# MemFS install — downloads the binary, mounts, and configures Claude Code.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Haakam21/mem-fs/main/install.sh | bash
#
# Installs binary to ~/.local/bin, mounts ./memories/, creates ./CLAUDE.md
#
# Prerequisites:
#   - macFUSE (macOS): https://macfuse.io
#   - libfuse (Linux): apt install fuse3
#   - gh CLI: https://cli.github.com (for downloading release binary)

set -euo pipefail

INSTALL_BASE="${1:-$(pwd)}"
MOUNT_PATH="$INSTALL_BASE/memories"
BIN_DIR="$HOME/.local/bin"
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

# --- Download binary ---

mkdir -p "$BIN_DIR"
echo "Downloading memfs ($ARTIFACT)..."
gh release download --repo "$REPO" --pattern "$ARTIFACT" --dir "$BIN_DIR" --clobber
mv "$BIN_DIR/$ARTIFACT" "$BIN_DIR/memfs"
chmod +x "$BIN_DIR/memfs"
echo "Installed to $BIN_DIR/memfs"

# --- Mount ---

if mount | grep -q "$MOUNT_PATH"; then
    echo "Unmounting existing mount at $MOUNT_PATH..."
    if [[ "$OS" == "Darwin" ]]; then
        umount "$MOUNT_PATH" 2>/dev/null || true
    else
        fusermount -u "$MOUNT_PATH" 2>/dev/null || true
    fi
    sleep 1
fi

mkdir -p "$MOUNT_PATH"
export MEMFS_DB="$INSTALL_BASE/.memfs.db"
echo "Mounting memfs at $MOUNT_PATH..."
"$BIN_DIR/memfs" mount -f "$MOUNT_PATH" &
MOUNT_PID=$!
sleep 2

if ! ls "$MOUNT_PATH" &>/dev/null; then
    echo "Error: Mount failed"
    kill $MOUNT_PID 2>/dev/null || true
    exit 1
fi

echo "Mounted (PID $MOUNT_PID)"

# --- Seed starter facets so agents see the pattern ---

if [ -z "$(ls "$MOUNT_PATH" 2>/dev/null)" ]; then
    mkdir -p "$MOUNT_PATH/people" "$MOUNT_PATH/topics" "$MOUNT_PATH/dates"
    echo "Seeded starter facets: people/, topics/, dates/"
fi

# --- Create CLAUDE.md ---

CLAUDE_MD="$INSTALL_BASE/CLAUDE.md"
MEMORIES_LINE="Your memories are in the ./memories directory."

if [[ ! -f "$CLAUDE_MD" ]]; then
    echo "$MEMORIES_LINE" > "$CLAUDE_MD"
elif ! grep -qF "$MEMORIES_LINE" "$CLAUDE_MD" 2>/dev/null; then
    echo "" >> "$CLAUDE_MD"
    echo "$MEMORIES_LINE" >> "$CLAUDE_MD"
fi

echo ""
echo "=== MemFS is ready ==="
echo "  Install dir:  $INSTALL_BASE"
echo "  Mount point:  $MOUNT_PATH"
echo "  Database:     $MEMFS_DB"
echo "  Claude hint:  $CLAUDE_MD"
echo ""
echo "To unmount:     $BIN_DIR/memfs unmount $MOUNT_PATH"
echo "To remount:     MEMFS_DB=$MEMFS_DB $BIN_DIR/memfs mount -f $MOUNT_PATH &"
