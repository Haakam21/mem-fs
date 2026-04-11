#!/bin/bash
# MemFS uninstall — stops service, unmounts, removes binaries and config.
# Does NOT delete your memories database unless --purge is passed.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Haakam21/mem-fs/main/uninstall.sh | bash
#   curl -fsSL ... | bash -s -- --purge    # also delete database and models

set -euo pipefail

PURGE=false
for arg in "$@"; do
    if [[ "$arg" == "--purge" ]]; then
        PURGE=true
    fi
done

OS="$(uname -s)"
DATA_DIR="$HOME/.memfs"
GLOBAL_MOUNT="$DATA_DIR/mount"

# --- Stop service (prevents auto-restart) ---

if [[ "$OS" == "Darwin" ]]; then
    PLIST="$HOME/Library/LaunchAgents/com.memfs.mount.plist"
    if [[ -f "$PLIST" ]]; then
        launchctl unload -w "$PLIST" 2>/dev/null || true
        rm -f "$PLIST"
        echo "Stopped and removed launchd service"
    fi
else
    systemctl --user disable --now memfs 2>/dev/null || true
    rm -f "$HOME/.config/systemd/user/memfs.service"
    echo "Stopped and removed systemd service"
fi

# --- Stop daemon and unmount ---

pkill -f "memfs mount" 2>/dev/null || true
if [[ "$OS" == "Darwin" ]]; then
    umount "$GLOBAL_MOUNT" 2>/dev/null || true
else
    fusermount -u "$GLOBAL_MOUNT" 2>/dev/null || true
fi
sleep 1
rmdir "$GLOBAL_MOUNT" 2>/dev/null || true

# --- Remove local symlink (cwd) ---

if [[ -L "$(pwd)/memories" ]]; then
    rm -f "$(pwd)/memories"
fi

# --- Remove binaries ---

rm -f "$HOME/.local/bin/search"
echo "Removed binaries"

# --- Remove Claude Code config (cwd) ---

rm -f "$(pwd)/.claude/settings.json" 2>/dev/null || true
rmdir "$(pwd)/.claude" 2>/dev/null || true

# --- Purge data (optional) ---

if $PURGE; then
    rm -rf "$DATA_DIR"
    echo "Purged ~/.memfs (database, models, config, binary)"
else
    echo "Data preserved at $DATA_DIR (use --purge to delete)"
fi

echo "MemFS uninstalled."
