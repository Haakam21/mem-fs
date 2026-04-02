#!/bin/bash
# MemFS setup — builds, installs, mounts, and configures Claude Code integration.
#
# Usage:
#   bash setup.sh                    # defaults: mount at ~/memories
#   bash setup.sh /path/to/memories  # custom mount point
#
# What it does:
#   1. Builds memfs (requires Rust toolchain + macFUSE/libfuse)
#   2. Installs the binary to ~/.local/bin/memfs
#   3. Mounts the FUSE filesystem at the specified path
#   4. Creates a CLAUDE.md in the parent directory so Claude Code knows about it

set -euo pipefail

MOUNT_PATH="${1:-$SCRIPT_DIR/memories}"
MOUNT_PARENT="$(dirname "$MOUNT_PATH")"
MOUNT_NAME="$(basename "$MOUNT_PATH")"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
INSTALL_DIR="$HOME/.local/bin"

# --- Check prerequisites ---

if ! command -v cargo &>/dev/null; then
    echo "Error: Rust toolchain not found. Install from https://rustup.rs"
    exit 1
fi

if ! command -v pkg-config &>/dev/null; then
    echo "Error: pkg-config not found."
    if [[ "$(uname)" == "Darwin" ]]; then
        echo "  Install with: brew install pkgconf"
    else
        echo "  Install with: apt install pkg-config"
    fi
    exit 1
fi

# Check for FUSE library
if [[ "$(uname)" == "Darwin" ]]; then
    if ! pkg-config --exists fuse 2>/dev/null && ! pkg-config --exists osxfuse 2>/dev/null; then
        if [[ -f /usr/local/lib/pkgconfig/fuse.pc ]]; then
            export PKG_CONFIG_PATH="/usr/local/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
        else
            echo "Error: macFUSE not found. Install from https://macfuse.io"
            echo "  After install, approve the kernel extension in System Settings > Privacy & Security"
            exit 1
        fi
    fi
else
    if ! pkg-config --exists fuse 2>/dev/null; then
        echo "Error: libfuse not found."
        echo "  Install with: apt install libfuse-dev (Debian/Ubuntu)"
        echo "  Install with: dnf install fuse-devel (Fedora)"
        exit 1
    fi
fi

# --- Build ---

echo "Building memfs..."
cd "$SCRIPT_DIR"
cargo build --release --quiet

# --- Install binary ---

mkdir -p "$INSTALL_DIR"
cp target/release/memfs "$INSTALL_DIR/memfs"
echo "Installed memfs to $INSTALL_DIR/memfs"

# Add to PATH if not already there
if [[ ":$PATH:" != *":$INSTALL_DIR:"* ]]; then
    echo "Note: Add $INSTALL_DIR to your PATH:"
    echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.zshrc"
fi

# --- Mount ---

# Unmount if already mounted
if mount | grep -q "$MOUNT_PATH"; then
    echo "Unmounting existing mount at $MOUNT_PATH..."
    if [[ "$(uname)" == "Darwin" ]]; then
        umount "$MOUNT_PATH" 2>/dev/null || true
    else
        fusermount -u "$MOUNT_PATH" 2>/dev/null || true
    fi
    sleep 1
fi

mkdir -p "$MOUNT_PATH"
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

# Only create if it doesn't exist or doesn't mention memories
MEMORIES_LINE="Your memories are in the ./$MOUNT_NAME directory."

if [[ ! -f "$CLAUDE_MD" ]]; then
    echo "$MEMORIES_LINE" > "$CLAUDE_MD"
    echo "Created $CLAUDE_MD"
elif ! grep -qF "$MOUNT_NAME" "$CLAUDE_MD" 2>/dev/null; then
    echo "" >> "$CLAUDE_MD"
    echo "$MEMORIES_LINE" >> "$CLAUDE_MD"
    echo "Appended to $CLAUDE_MD"
else
    echo "$CLAUDE_MD already mentions $MOUNT_NAME"
fi

echo ""
echo "=== MemFS is ready ==="
echo "  Mount point:  $MOUNT_PATH"
echo "  Database:     ~/.memfs.db"
echo "  Claude hint:  $CLAUDE_MD"
echo ""
echo "To unmount:     memfs unmount $MOUNT_PATH"
echo "To remount:     memfs mount -f $MOUNT_PATH &"
