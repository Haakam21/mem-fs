#!/bin/bash
# Regression test for the "intuitive FUSE write" path.
#
# Bug history (pre-v0.12.2): `echo foo > $MOUNT/topics/bar.md` could land in
# a backing directory instead of the FUSE-indexed db, with no error signal.
# This happened when the FUSE daemon couldn't mount (e.g. /etc/fuse.conf
# missing user_allow_other), because:
#   1. init's read_dir().is_ok() health check returned true for any readable
#      dir — even the plain backing directory with no FUSE overlay.
#   2. init pre-seeded facet categories as real subdirs on the backing fs,
#      so writes had a place to land.
#
# This test pins down the expected behavior post-fix:
#   * FUSE mount is live.
#   * `echo > $MOUNT/topics/foo.md` creates an indexed memory.
#   * The memory is auto-tagged `topics:foo` (stem of the filename).
#   * The memory is searchable via `memfs find`.
#   * The memory survives an unmount/remount cycle.
#
# Skips gracefully when FUSE or libfuse isn't available (CI without fuse).

set -u

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
MEMFS="$SCRIPT_DIR/target/release/memfs"
TEST_DIR="/tmp/memfs_fuse_write_test"
export MEMFS_DB="$TEST_DIR/db"
export MEMFS_STATE="$TEST_DIR/state"
FUSE_MP="$TEST_DIR/mount"

PASS=0
FAIL=0

pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); }

cleanup() {
    [ -n "${MOUNT_PID:-}" ] && kill "$MOUNT_PID" 2>/dev/null
    fusermount3 -u -z "$FUSE_MP" 2>/dev/null || fusermount -u -z "$FUSE_MP" 2>/dev/null || umount "$FUSE_MP" 2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "$TEST_DIR"
}
trap cleanup EXIT

# --- Precheck ---
if [ ! -x "$MEMFS" ]; then
    echo "SKIP: release binary missing at $MEMFS — run \`cargo build --release\` first"
    exit 0
fi
if ! command -v fusermount3 >/dev/null 2>&1 && ! command -v fusermount >/dev/null 2>&1; then
    echo "SKIP: fusermount not available"
    exit 0
fi

# --- Setup fresh state ---
rm -rf "$TEST_DIR"
mkdir -p "$TEST_DIR" "$FUSE_MP"

echo "=== MemFS FUSE write regression ==="
echo

# --- Mount via the daemon ---
"$MEMFS" mount -f "$FUSE_MP" >"$TEST_DIR/mount.log" 2>&1 &
MOUNT_PID=$!
sleep 2

# Verify it's actually a FUSE mount, not just a readable directory.
if ! grep -qs "$FUSE_MP.*fuse" /proc/self/mountinfo 2>/dev/null \
   && ! mount 2>/dev/null | grep -q "$FUSE_MP"; then
    echo "SKIP: FUSE mount did not come up"
    echo "--- mount log ---"
    cat "$TEST_DIR/mount.log"
    exit 0
fi
pass "FUSE mount is live"

# --- Test 1: write through FUSE to a facet dir ---
echo "Test 1: echo > \$MOUNT/topics/fuse_write_test.md"
# Create the topics dir via the mount. On a fresh db, the topics facet
# doesn't exist yet, so we explicitly mkdir it first to trigger facet
# creation. (This mirrors how `memfs init` seeds facet categories.)
mkdir -p "$FUSE_MP/topics"
echo "hello from fuse write" > "$FUSE_MP/topics/fuse_write_test.md"
write_exit=$?

if [ $write_exit -eq 0 ]; then
    pass "write exited 0"
else
    fail "write exited $write_exit"
fi

# --- Test 2: memory is indexed (visible via memfs find) ---
echo "Test 2: memory is in the index"
# `memfs find` and the daemon share the same db, so we need a read-only
# client. Stop the daemon briefly to release the lock.
kill "$MOUNT_PID" 2>/dev/null
wait "$MOUNT_PID" 2>/dev/null
MOUNT_PID=""
sleep 0.3

find_output=$("$MEMFS" find --name "fuse_write_test*" 2>&1)
if echo "$find_output" | grep -q "fuse_write_test.md"; then
    pass "memfs find shows the memory"
else
    fail "memfs find did not show the memory (got: $find_output)"
fi

# --- Test 3: auto-tag is correct ---
echo "Test 3: auto-tag topics:fuse_write_test"
cat_output=$("$MEMFS" cat fuse_write_test.md 2>&1)
if echo "$cat_output" | grep -q "topics:fuse_write_test"; then
    pass "tag header contains topics:fuse_write_test"
else
    fail "tag header missing topics:fuse_write_test (got: $cat_output)"
fi

# --- Test 4: content round-trip ---
echo "Test 4: content is correct"
if echo "$cat_output" | grep -q "hello from fuse write"; then
    pass "content preserved"
else
    fail "content missing from memory"
fi

# --- Test 5: survives remount ---
echo "Test 5: memory survives remount"
"$MEMFS" mount -f "$FUSE_MP" >"$TEST_DIR/mount2.log" 2>&1 &
MOUNT_PID=$!
sleep 2

if [ -f "$FUSE_MP/topics/fuse_write_test.md" ]; then
    pass "file visible after remount"
else
    fail "file not visible after remount"
fi

remount_content=$(cat "$FUSE_MP/topics/fuse_write_test.md" 2>/dev/null)
if [ "$remount_content" = "hello from fuse write" ]; then
    pass "content readable through FUSE after remount"
else
    fail "content mismatch after remount: got '$remount_content'"
fi

# --- Summary ---
echo
echo "================================"
echo "Results: $PASS passed, $FAIL failed"
echo "================================"

if [ "$FAIL" -gt 0 ]; then
    exit 1
fi
exit 0
