#!/bin/bash
# MemFS uninstall — unmounts, removes binaries, and cleans up config.
# Does NOT delete your memories database unless --purge is passed.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Haakam21/mem-fs/main/uninstall.sh | bash
#   curl -fsSL ... | bash -s --purge    # also delete database and models

set -euo pipefail

PURGE=false
for arg in "$@"; do
    [[ "$arg" == "--purge" ]] && PURGE=true
done

INSTALL_BASE="${1:-$(pwd)}"
MOUNT_PATH="$INSTALL_BASE/memories"
OS="$(uname -s)"

# --- Unmount ---

if mount | grep -q "$MOUNT_PATH"; then
    echo "Unmounting $MOUNT_PATH..."
    if [[ "$OS" == "Darwin" ]]; then
        umount "$MOUNT_PATH" 2>/dev/null || true
    else
        fusermount -u "$MOUNT_PATH" 2>/dev/null || true
    fi
    sleep 1
fi
rmdir "$MOUNT_PATH" 2>/dev/null || true

# --- Remove binaries ---

rm -f "$HOME/.memfs/memfs"
rmdir "$HOME/.memfs" 2>/dev/null || true
rm -f "$HOME/.local/bin/search"
echo "Removed binaries"

# --- Remove config ---

rm -f "$INSTALL_BASE/.claude/settings.json"
rmdir "$INSTALL_BASE/.claude" 2>/dev/null || true
echo "Removed config"

# --- Purge data (optional) ---

if $PURGE; then
    rm -f "$INSTALL_BASE/.memfs.db" "$INSTALL_BASE/.memfs.db-wal" "$INSTALL_BASE/.memfs.db-shm"
    rm -rf "$HOME/.memfs/models"
    echo "Purged database and models"
else
    echo "Database preserved at $INSTALL_BASE/.memfs.db (use --purge to delete)"
fi

echo "MemFS uninstalled."
