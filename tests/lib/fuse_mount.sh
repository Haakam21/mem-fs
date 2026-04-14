#!/bin/bash
# Shared FUSE mount helpers for the memfs integration test suite.
#
# Source from each test script after setting MEMFS to the release binary
# path. The helpers own the MEMFS_MOUNT_PID global so callers don't have
# to track the background daemon PID themselves.
#
# Used by tests/test_fuse_write.sh, tests/test_fuse_agent.sh, and
# tests/test_integration.sh (the FUSE portion of Test 16).

# Poll the kernel mount table until `$1` is a live FUSE mount or 5 s
# elapses. Mirrors the Rust `is_fuse_mounted` helper in src/main.rs so
# both sides agree on what counts as "mounted" — `ls $target` returning
# a directory listing is NOT sufficient, because it succeeds against a
# plain backing directory with no FUSE overlay.
#
# Usage: wait_for_fuse_mount <mount-point>
# Exits 0 on success, 1 on timeout.
wait_for_fuse_mount() {
    local target="$1"
    for _ in $(seq 1 50); do
        if grep -qs "$target.*fuse" /proc/self/mountinfo 2>/dev/null \
           || mount 2>/dev/null | grep -F " on $target " | grep -q "fuse"; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

# Start a memfs FUSE daemon in the background and wait for the mount to
# come up. On failure, dump the mount log (if provided) so the caller
# can see why the daemon crashed.
#
# Usage: start_fuse_mount <memfs-binary> <mount-point> [log-file]
# Side effect: sets MEMFS_MOUNT_PID to the background process PID.
# Exits 0 on success, 1 if the mount never appears.
start_fuse_mount() {
    local memfs="$1"
    local mp="$2"
    local log="${3:-/dev/null}"
    mkdir -p "$mp"
    "$memfs" mount -f "$mp" >"$log" 2>&1 &
    MEMFS_MOUNT_PID=$!
    if ! wait_for_fuse_mount "$mp"; then
        echo "ERROR: FUSE mount at $mp did not come up within 5s"
        if [ "$log" != "/dev/null" ] && [ -s "$log" ]; then
            echo "--- mount log ---"
            cat "$log"
        fi
        return 1
    fi
    return 0
}

# Kill the memfs FUSE daemon (if MEMFS_MOUNT_PID is set) and lazy-unmount
# the path. Tries the libfuse3, libfuse2, and plain umount tools in turn
# — safe to call when nothing is mounted, and safe to call multiple times
# from an EXIT trap.
#
# Usage: stop_fuse_mount <mount-point>
stop_fuse_mount() {
    local mp="$1"
    if [ -n "${MEMFS_MOUNT_PID:-}" ]; then
        kill "$MEMFS_MOUNT_PID" 2>/dev/null || true
        wait "$MEMFS_MOUNT_PID" 2>/dev/null || true
        MEMFS_MOUNT_PID=""
    fi
    fusermount3 -u -z "$mp" 2>/dev/null \
        || fusermount -u -z "$mp" 2>/dev/null \
        || umount "$mp" 2>/dev/null \
        || true
}
