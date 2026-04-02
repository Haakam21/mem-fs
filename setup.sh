#!/bin/bash
# MemFS setup — downloads (or builds), installs, mounts, and configures Claude Code.
#
# Usage:
#   bash setup.sh                    # defaults: mount at ./memories
#   bash setup.sh /path/to/memories  # custom mount point
#
# What it does:
#   1. Downloads a prebuilt binary (or builds from source if unavailable)
#   2. Installs the binary to ~/.local/bin/memfs
#   3. Mounts the FUSE filesystem at the specified path
#   4. Creates a CLAUDE.md in the parent directory so Claude Code knows about it
#
# Prerequisites:
#   - macFUSE (macOS): https://macfuse.io
#   - libfuse (Linux): apt install libfuse3-dev

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MOUNT_PATH="${1:-$SCRIPT_DIR/memories}"
MOUNT_PARENT="$(dirname "$MOUNT_PATH")"
MOUNT_NAME="$(basename "$MOUNT_PATH")"
INSTALL_DIR="$HOME/.local/bin"
REPO="Haakam21/mem-fs"

# --- Detect platform ---

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
    Darwin-arm64)  ARTIFACT="memfs-darwin-arm64" ;;
    Darwin-x86_64) ARTIFACT="memfs-darwin-x86_64" ;;
    Linux-x86_64)  ARTIFACT="memfs-linux-x86_64" ;;
    *) ARTIFACT="" ;;
esac

# --- Install binary ---

mkdir -p "$INSTALL_DIR"

if [[ -n "$ARTIFACT" ]] && command -v gh &>/dev/null; then
    # Try downloading prebuilt binary from latest release
    echo "Downloading prebuilt binary ($ARTIFACT)..."
    if gh release download --repo "$REPO" --pattern "$ARTIFACT" --dir "$INSTALL_DIR" --clobber 2>/dev/null; then
        mv "$INSTALL_DIR/$ARTIFACT" "$INSTALL_DIR/memfs"
        chmod +x "$INSTALL_DIR/memfs"
        echo "Installed prebuilt memfs to $INSTALL_DIR/memfs"
    else
        echo "No prebuilt binary available, building from source..."
        ARTIFACT=""
    fi
fi

if [[ -z "$ARTIFACT" ]] || [[ ! -x "$INSTALL_DIR/memfs" ]]; then
    # Fall back to building from source
    if ! command -v cargo &>/dev/null; then
        echo "Error: No prebuilt binary and Rust toolchain not found."
        echo "  Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        exit 1
    fi
    echo "Building memfs from source..."
    cd "$SCRIPT_DIR"
    PKG_CONFIG_PATH="/usr/local/lib/pkgconfig:${PKG_CONFIG_PATH:-}" cargo build --release --quiet
    cp target/release/memfs "$INSTALL_DIR/memfs"
    echo "Installed memfs to $INSTALL_DIR/memfs"
fi

# Add to PATH if not already there
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo "Note: Add $INSTALL_DIR to your PATH:"
    echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.zshrc"
fi

# --- Check FUSE runtime ---

if [[ "$OS" == "Darwin" ]]; then
    if [[ ! -d /Library/Frameworks/macFUSE.framework ]]; then
        echo "Error: macFUSE not installed. Download from https://macfuse.io"
        echo "  After install, approve the kernel extension in System Settings > Privacy & Security"
        exit 1
    fi
else
    if ! command -v fusermount &>/dev/null && ! command -v fusermount3 &>/dev/null; then
        echo "Error: FUSE not installed."
        echo "  Install with: apt install fuse3 (Debian/Ubuntu)"
        exit 1
    fi
fi

# --- Mount ---

# Unmount if already mounted
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
export MEMFS_DB="$MOUNT_PARENT/.memfs.db"
echo "Mounting memfs at $MOUNT_PATH..."
"$INSTALL_DIR/memfs" mount -f "$MOUNT_PATH" &
MOUNT_PID=$!
sleep 2

# Verify mount
if ! ls "$MOUNT_PATH" &>/dev/null; then
    echo "Error: Mount failed"
    kill $MOUNT_PID 2>/dev/null || true
    exit 1
fi

echo "Mounted successfully (PID $MOUNT_PID)"

# --- Create CLAUDE.md indicator ---

CLAUDE_MD="$MOUNT_PARENT/CLAUDE.md"
MEMORIES_LINE="Your memories are in the ./$MOUNT_NAME directory."

if [[ ! -f "$CLAUDE_MD" ]]; then
    echo "$MEMORIES_LINE" > "$CLAUDE_MD"
    echo "Created $CLAUDE_MD"
elif ! grep -qF "$MEMORIES_LINE" "$CLAUDE_MD" 2>/dev/null; then
    echo "" >> "$CLAUDE_MD"
    echo "$MEMORIES_LINE" >> "$CLAUDE_MD"
    echo "Appended to $CLAUDE_MD"
else
    echo "$CLAUDE_MD already configured"
fi

echo ""
echo "=== MemFS is ready ==="
echo "  Mount point:  $MOUNT_PATH"
echo "  Database:     $MEMFS_DB"
echo "  Claude hint:  $CLAUDE_MD"
echo ""
echo "To unmount:     memfs unmount $MOUNT_PATH"
echo "To remount:     MEMFS_DB=$MEMFS_DB memfs mount -f $MOUNT_PATH &"
