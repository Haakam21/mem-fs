#!/bin/bash
# End-to-end test for merge-based sync with two agents.
# Requires MEMFS_TURSO_URL and MEMFS_TURSO_TOKEN env vars (or .env file).

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
MEMFS="$SCRIPT_DIR/target/release/memfs"

# Load .env if present
if [[ -f "$SCRIPT_DIR/.env" ]]; then
    export $(grep -v '^#' "$SCRIPT_DIR/.env" | xargs)
fi

TURSO_URL="${MEMFS_TURSO_URL:-}"
TURSO_TOKEN="${MEMFS_TURSO_TOKEN:-}"

if [[ -z "$TURSO_URL" || -z "$TURSO_TOKEN" ]]; then
    echo "SKIP: MEMFS_TURSO_URL and MEMFS_TURSO_TOKEN required"
    exit 0
fi

# Unique prefix per run to avoid collisions with leftover cloud data
RUN_ID="test_$(date +%s)"

AGENT_A_DIR="/tmp/memfs_sync_test_a_$$"
AGENT_B_DIR="/tmp/memfs_sync_test_b_$$"
HOME_CONFLICTS="$HOME/.memfs/conflicts"

rm -rf "$AGENT_A_DIR" "$AGENT_B_DIR"
mkdir -p "$AGENT_A_DIR/.memfs" "$AGENT_B_DIR/.memfs"

PASS=0
FAIL=0

assert_contains() {
    local desc="$1" needle="$2" haystack="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (expected to contain '$needle')"
        echo "    actual: $(echo "$haystack" | head -5)"
        FAIL=$((FAIL + 1))
    fi
}

assert_not_contains() {
    local desc="$1" needle="$2" haystack="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        echo "  FAIL: $desc (should NOT contain '$needle')"
        FAIL=$((FAIL + 1))
    else
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    fi
}

assert_file_exists() {
    local desc="$1" path="$2"
    if [[ -f "$path" ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (file not found: $path)"
        FAIL=$((FAIL + 1))
    fi
}

assert_file_not_exists() {
    local desc="$1" path="$2"
    if [[ ! -f "$path" ]]; then
        echo "  PASS: $desc"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $desc (file should not exist: $path)"
        FAIL=$((FAIL + 1))
    fi
}

# Write settings.json for each agent
for dir in "$AGENT_A_DIR" "$AGENT_B_DIR"; do
    cat > "$dir/.memfs/settings.json" <<SETTINGS
{
  "turso_url": "$TURSO_URL",
  "turso_token": "$TURSO_TOKEN"
}
SETTINGS
done

# Helpers
run_a() { MEMFS_DB="$AGENT_A_DIR/.memfs/db" MEMFS_STATE="$AGENT_A_DIR/.memfs/state" "$MEMFS" "$@"; }
run_b() { MEMFS_DB="$AGENT_B_DIR/.memfs/db" MEMFS_STATE="$AGENT_B_DIR/.memfs/state" "$MEMFS" "$@"; }
sync_a() { MEMFS_DB="$AGENT_A_DIR/.memfs/db" MEMFS_STATE="$AGENT_A_DIR/.memfs/state" "$MEMFS" sync 2>&1; }
sync_b() { MEMFS_DB="$AGENT_B_DIR/.memfs/db" MEMFS_STATE="$AGENT_B_DIR/.memfs/state" "$MEMFS" sync 2>&1; }

echo "=== Sync Merge End-to-End Test (run: $RUN_ID) ==="
echo

# --- Step 1: Agent A writes memories ---
echo "Step 1: Agent A writes memories..."
run_a write "/memories/${RUN_ID}_only-a.md" "unique to agent A"
run_a write "/memories/${RUN_ID}_shared.md" "Agent A's version of shared note"
run_a write "/memories/${RUN_ID}_identical.md" "same content on both sides"
echo

# --- Step 2: Agent B writes memories ---
echo "Step 2: Agent B writes memories..."
run_b write "/memories/${RUN_ID}_only-b.md" "unique to agent B"
run_b write "/memories/${RUN_ID}_shared.md" "Agent B's version of shared note"
run_b write "/memories/${RUN_ID}_identical.md" "same content on both sides"
echo

# --- Step 3: Agent A syncs ---
echo "Step 3: Agent A syncs to cloud..."
sync_a_output=$(sync_a)
echo "$sync_a_output"
assert_contains "Agent A sync succeeds" "Synced" "$sync_a_output"
echo

# --- Step 4: Agent B syncs (merge + conflict) ---
echo "Step 4: Agent B syncs (merge with cloud)..."
sync_b_output=$(sync_b)
echo "$sync_b_output"
assert_contains "Agent B sync succeeds" "Synced" "$sync_b_output"
assert_contains "Agent B sync reports merged from remote" "merged from remote" "$sync_b_output"
assert_contains "Agent B sync detects conflict" "conflict" "$sync_b_output"
assert_contains "Agent B sync names conflicting file" "${RUN_ID}_shared.md" "$sync_b_output"
echo

# --- Step 5: Verify conflict file ---
echo "Step 5: Verify conflict files..."
assert_file_exists "Conflict file for shared" "$HOME_CONFLICTS/${RUN_ID}_shared.md"

if [[ -f "$HOME_CONFLICTS/${RUN_ID}_shared.md" ]]; then
    conflict_content=$(cat "$HOME_CONFLICTS/${RUN_ID}_shared.md")
    assert_contains "Conflict file has Agent A's content" "Agent A's version" "$conflict_content"
    assert_not_contains "Conflict file does NOT have Agent B's content" "Agent B's version" "$conflict_content"
fi
assert_file_not_exists "No conflict for identical" "$HOME_CONFLICTS/${RUN_ID}_identical.md"
echo

# --- Step 6: Verify Agent B's DB has merged data ---
echo "Step 6: Verify Agent B's merged state..."
# Agent B should have: only-b, shared (B's version), identical, AND only-a (merged from A)
b_cat_only_a=$(run_b cat "/memories/${RUN_ID}_only-a.md")
assert_contains "Agent B has Agent A's unique memory" "unique to agent A" "$b_cat_only_a"

b_cat_shared=$(run_b cat "/memories/${RUN_ID}_shared.md")
assert_contains "Agent B's shared note has B's content" "Agent B's version" "$b_cat_shared"

b_cat_identical=$(run_b cat "/memories/${RUN_ID}_identical.md")
assert_contains "Identical memory preserved" "same content on both sides" "$b_cat_identical"
echo

# --- Step 7: Agent B reconciles and syncs again ---
echo "Step 7: Agent B reconciles conflict and syncs again..."
run_b write "/memories/${RUN_ID}_shared.md" "Reconciled: merged both agents' notes"
sync_b2_output=$(sync_b)
echo "$sync_b2_output"
assert_contains "Second sync succeeds" "Synced" "$sync_b2_output"
# "Synced." (no conflicts) vs "Synced with N conflict(s)"
assert_not_contains "No conflicts on second sync" "conflict" "$sync_b2_output"
echo

# --- Step 8: Agent A syncs — gets B's unique memory, sees conflict on shared ---
echo "Step 8: Agent A syncs to get merged state..."
sync_a2_output=$(sync_a)
echo "$sync_a2_output"
assert_contains "Agent A second sync succeeds" "Synced" "$sync_a2_output"

# Agent A should see B's unique memory (auto-merged)
a_cat_only_b=$(run_a cat "/memories/${RUN_ID}_only-b.md")
assert_contains "Agent A has Agent B's unique memory" "unique to agent B" "$a_cat_only_b"

# Agent A sees a conflict: B pushed reconciled content, A still has original
assert_contains "Agent A sees conflict on shared" "conflict" "$sync_a2_output"
# The conflict file should have B's reconciled version
if [[ -f "$HOME_CONFLICTS/${RUN_ID}_shared.md" ]]; then
    a_conflict=$(cat "$HOME_CONFLICTS/${RUN_ID}_shared.md")
    assert_contains "Conflict file has reconciled content" "Reconciled: merged both" "$a_conflict"
fi
echo

# --- Summary ---
echo "========================"
echo "  $PASS passed, $FAIL failed"
echo "========================"

# Cleanup
rm -rf "$AGENT_A_DIR" "$AGENT_B_DIR"

exit $FAIL
