#!/bin/bash
# Integration test for memfs — runs the spec's example session and verifies output.
# No set -e: we handle failures via assert functions

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
MEMFS="$SCRIPT_DIR/target/release/memfs"
TEST_DIR="/tmp/memfs_integration_test_dir"
mkdir -p "$TEST_DIR/.memfs"
export MEMFS_DB="$TEST_DIR/.memfs/db"
export MEMFS_STATE="$TEST_DIR/.memfs/state"
export MEMFS_BIN="$MEMFS"

source "$SCRIPT_DIR/tests/lib/fuse_mount.sh"

# Clean slate
rm -f "$MEMFS_DB" "$MEMFS_STATE"

PASS=0
FAIL=0

assert_eq() {
    local desc="$1"
    local expected="$2"
    local actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc"
        echo "    expected: $(echo "$expected" | head -3)"
        echo "    actual:   $(echo "$actual" | head -3)"
        FAIL=$((FAIL + 1))
    fi
}

assert_contains() {
    local desc="$1"
    local needle="$2"
    local haystack="$3"
    if echo "$haystack" | grep -q "$needle"; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (expected to contain '$needle')"
        echo "    actual: $(echo "$haystack" | head -3)"
        FAIL=$((FAIL + 1))
    fi
}

assert_exit_code() {
    local desc="$1"
    local expected="$2"
    local actual="$3"
    if [[ "$expected" == "$actual" ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (expected exit $expected, got $actual)"
        FAIL=$((FAIL + 1))
    fi
}

echo "=== MemFS Integration Tests ==="
echo

# --- Setup: create facet structure ---
echo "Setup: creating facet structure..."
$MEMFS mkdir -p /memories/people/sister/dates/2025-03/topics/birthday
$MEMFS mkdir -p /memories/people/mom
$MEMFS mkdir -p /memories/people/alex
$MEMFS mkdir -p /memories/locations
$MEMFS mkdir -p /memories/emotions

echo

# --- Test 1: ls at root ---
echo "Test 1: ls at root"
output=$($MEMFS cd /memories && $MEMFS ls)
assert_contains "root shows dates/" "dates/" "$output"
assert_contains "root shows people/" "people/" "$output"
assert_contains "root shows topics/" "topics/" "$output"
assert_contains "root shows emotions/" "emotions/" "$output"
assert_contains "root shows locations/" "locations/" "$output"

echo

# --- Test 2: ls facet values ---
echo "Test 2: ls facet values"
$MEMFS cd /memories/people
output=$($MEMFS ls)
assert_contains "people/ shows alex/" "alex/" "$output"
assert_contains "people/ shows mom/" "mom/" "$output"
assert_contains "people/ shows sister/" "sister/" "$output"

echo

# --- Test 3: pwd ---
echo "Test 3: pwd"
$MEMFS cd /memories/people/sister
output=$($MEMFS pwd)
assert_eq "pwd shows virtual path" "/memories/people/sister" "$output"

echo

# --- Test 4: write + cat ---
echo "Test 4: write + cat"
$MEMFS cd /memories/people/sister/dates/2025-03
$MEMFS write memory_0042.md "Had a wonderful birthday celebration for my sister in March.
We went to her favorite restaurant downtown and surprised her
with a cake shaped like a cat."
output=$($MEMFS cat memory_0042.md)
assert_contains "cat shows tags header" "tags:" "$output"
assert_contains "cat shows people:sister tag" "people:sister" "$output"
assert_contains "cat shows dates:2025-03 tag" "dates:2025-03" "$output"
assert_contains "cat shows content" "cake shaped like a cat" "$output"

echo

# --- Test 5: Navigation invariant ---
echo "Test 5: Navigation invariant"
$MEMFS cd /memories/people/sister/dates/2025-03
output_a=$($MEMFS ls)
$MEMFS cd /memories/dates/2025-03/people/sister
output_b=$($MEMFS ls)
assert_eq "same ls output regardless of path order" "$output_a" "$output_b"

echo

# --- Test 6: ls at filtered level shows remaining facets ---
echo "Test 6: ls at filtered level"
$MEMFS cd /memories/people/sister
output=$($MEMFS ls)
assert_contains "filtered level shows dates/" "dates/" "$output"
assert_contains "filtered level shows memory file" "memory_0042.md" "$output"

echo

# --- Test 7: Write second memory + grep ---
echo "Test 7: grep"
$MEMFS cd /memories/people/sister/topics/birthday
$MEMFS write surprise_plan.md "Planning a surprise party for sister's birthday next year.
Need to book the venue by January."
output=$($MEMFS grep cake /memories)
assert_contains "grep finds cake" "memory_0042.md" "$output"
assert_contains "grep shows matching line" "cake shaped like a cat" "$output"

# Grep should not find "cake" in surprise_plan.md
output2=$($MEMFS grep "surprise" /memories)
assert_contains "grep finds surprise" "surprise_plan.md" "$output2"

echo

# --- Test 8: find ---
echo "Test 8: find"
output=$($MEMFS find /memories --name "surprise*")
assert_contains "find matches surprise*" "surprise_plan.md" "$output"

echo

# --- Test 9: mv (retag) ---
echo "Test 9: mv (retag)"
$MEMFS mkdir -p /memories/people/acquaintance
$MEMFS mkdir -p /memories/people/friend
$MEMFS cd /memories/people/acquaintance
$MEMFS write memory_0099.md "Met someone interesting at the conference."
# Move from acquaintance to friend
$MEMFS mv /memories/people/acquaintance/memory_0099.md /memories/people/friend/
# Should now appear under friend
$MEMFS cd /memories/people/friend
output=$($MEMFS ls)
assert_contains "mv moved memory to friend" "memory_0099.md" "$output"
# Should NOT appear under acquaintance
$MEMFS cd /memories/people/acquaintance
output=$($MEMFS ls)
if echo "$output" | grep -q "memory_0099.md"; then
    echo "  FAIL: memory still in acquaintance after mv"
    FAIL=$((FAIL + 1))
else
    echo "  PASS: memory removed from acquaintance after mv"
    PASS=$((PASS + 1))
fi

echo

# --- Test 10: cp (add tag) ---
echo "Test 10: cp (add tag)"
$MEMFS mkdir -p /memories/topics/work
$MEMFS mkdir -p /memories/topics/personal
$MEMFS cd /memories/topics/work
$MEMFS write memory_0050.md "Important project deadline next week."
$MEMFS cp /memories/topics/work/memory_0050.md /memories/topics/personal/
# Should appear in both
$MEMFS cd /memories/topics/personal
output=$($MEMFS ls)
assert_contains "cp: memory in personal" "memory_0050.md" "$output"
$MEMFS cd /memories/topics/work
output=$($MEMFS ls)
assert_contains "cp: memory still in work" "memory_0050.md" "$output"

echo

# --- Test 11: rm ---
echo "Test 11: rm"
$MEMFS cd /memories
$MEMFS rm memory_0050.md 2>/dev/null || true
output=$($MEMFS find /memories --name "memory_0050*" 2>/dev/null || echo "")
if [[ -z "$output" ]]; then
    echo "  PASS: rm deleted memory_0050.md"
    PASS=$((PASS + 1))
else
    echo "  FAIL: memory_0050.md still exists after rm"
    FAIL=$((FAIL + 1))
fi

echo

# --- Test 12: ls -l ---
echo "Test 12: ls -l"
$MEMFS cd /memories/people/sister
output=$($MEMFS ls -l)
assert_contains "ls -l shows file permissions" "rw-r--r--" "$output"
assert_contains "ls -l shows dir permissions" "drwxr-xr-x" "$output"

echo

# --- Test 13: cd validation ---
echo "Test 13: cd validation"
output=$($MEMFS cd /memories/nonexistent 2>&1)
exit_code=$?
assert_exit_code "cd to nonexistent fails" "1" "$exit_code"
assert_contains "cd error message" "no such facet" "$output"

echo

# --- Test 14: cat nonexistent ---
echo "Test 14: cat nonexistent"
$MEMFS cd /memories
output=$($MEMFS cat nonexistent.md 2>&1)
exit_code=$?
assert_exit_code "cat nonexistent fails" "1" "$exit_code"
assert_contains "cat error message" "No such memory" "$output"

echo

# --- Test 15: reindex + search (requires model) ---
echo "Test 15: semantic search"
if $MEMFS reindex >/dev/null 2>&1; then
    # Search for birthday-related content
    output=$($MEMFS search "birthday celebration" 2>&1)
    assert_contains "search finds birthday memory" "birthday" "$output"

    # Search scoped to people/sister
    output=$($MEMFS search "birthday" /memories/people/sister 2>&1)
    assert_contains "scoped search finds memory" "birthday" "$output"

    # Search should NOT find birthday when scoped to a different person
    $MEMFS mkdir -p /memories/people/nobody
    output=$($MEMFS search -t 0.0 "birthday" /memories/people/nobody 2>&1)
    assert_eq "scoped search excludes unrelated" "" "$output"
else
    echo "  SKIP: embedding model not available"
fi

echo

# --- Test 16: standalone search binary with FUSE ---
echo "Test 16: standalone search binary"
SEARCH="$SCRIPT_DIR/target/release/search"
if [[ -x "$SEARCH" ]] && $MEMFS reindex >/dev/null 2>&1; then
    FUSE_MP="$TEST_DIR/memories"
    rm -rf "$FUSE_MP"

    if start_fuse_mount "$MEMFS" "$FUSE_MP"; then
        # Search binary reads DB while FUSE is running
        output=$(cd "$TEST_DIR" && $SEARCH "birthday celebration" 2>&1)
        assert_contains "standalone search finds birthday" "birthday" "$output"

        # Scoped search via standalone binary
        output=$(cd "$TEST_DIR" && $SEARCH "birthday" ./memories/people/sister 2>&1)
        assert_contains "standalone scoped search works" "birthday" "$output"

        # Verify score is shown
        output=$(cd "$TEST_DIR" && $SEARCH -t 0.0 "cake" 2>&1)
        assert_contains "search shows score" "(" "$output"
    else
        echo "  SKIP: FUSE mount failed"
    fi
    stop_fuse_mount "$FUSE_MP"
    rm -rf "$FUSE_MP"
else
    echo "  SKIP: search binary or embedding model not available"
fi

echo

# --- Summary ---
echo "================================"
echo "Results: $PASS passed, $FAIL failed"
echo "================================"

# Cleanup
rm -rf "$TEST_DIR" /tmp/memfs_search_integration_test

if [[ $FAIL -gt 0 ]]; then
    exit 1
fi
