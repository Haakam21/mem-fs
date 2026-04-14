#!/bin/bash
# FUSE agent test — seeds a rich memory database, mounts via FUSE, then spawns
# a Claude Code sub-agent with zero context to verify it can navigate the
# faceted filesystem using only standard Unix tools.
#
# Prerequisites:
#   - macFUSE installed (macOS) or libfuse (Linux)
#   - PKG_CONFIG_PATH includes fuse .pc files (e.g. /usr/local/lib/pkgconfig)
#   - cargo build --release has been run
#   - claude (Claude Code CLI) is on PATH
#
# Usage:
#   bash tests/test_fuse_agent.sh
#
# What it does:
#   1. Seeds 14 memories across 5 facets (people, dates, topics, emotions, locations)
#      with cross-tags so the same memory appears under multiple filter paths
#   2. Mounts at /tmp/memfs_agent_test_mount via `memfs mount -f`
#   3. Spawns a Claude Code sub-agent with a hard query that requires deep
#      faceted navigation (3+ filter depth) to answer correctly
#   4. Prints the agent's response and unmounts
#
# The key test: can an uninstructed agent navigate paths like
#   /tmp/.../emotions/happy/people/sister/dates/2025-03/birthday_surprise.md
# using only ls, cat, find, grep — with no documentation?

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
MEMFS="$SCRIPT_DIR/target/release/memfs"
export MEMFS_DB="/tmp/memfs_fuse_agent_test.db"
export MEMFS_STATE="/tmp/memfs_fuse_agent_test_cwd"
FUSE_MP="/tmp/memfs_agent_test_mount"

source "$SCRIPT_DIR/tests/lib/fuse_mount.sh"

# --- Cleanup from previous runs ---
stop_fuse_mount "$FUSE_MP"
rm -f "$MEMFS_DB" "$MEMFS_STATE"
rm -rf "$FUSE_MP"

# --- Seed data ---
echo "=== Seeding 14 memories across 5 facets ==="

for fv in \
  "people/sister" "people/mom" "people/alex" "people/dad" "people/jamie" \
  "dates/2024-06" "dates/2024-09" "dates/2024-12" "dates/2025-01" "dates/2025-03" \
  "topics/cooking" "topics/travel" "topics/work" "topics/health" "topics/music" \
  "emotions/happy" "emotions/sad" "emotions/grateful" "emotions/anxious" \
  "locations/home" "locations/paris" "locations/tahoe" "locations/office" "locations/hospital"
do
  $MEMFS mkdir -p "/memories/$fv"
done

tag() {
  local file="$1"; shift
  for fv in "$@"; do
    $MEMFS cp "/memories/$CWD/$file" "/memories/$fv"
  done
}

CWD="people/sister"; $MEMFS cd /memories/$CWD
$MEMFS write birthday_surprise.md "Planned a surprise 30th birthday party for sister at the Italian place on Valencia St. Ordered the tiramisu cake she loves. She walked in and burst into happy tears. Mom flew in from Denver as the real surprise — sister hadn't seen her in 8 months."
tag birthday_surprise.md dates/2025-03 emotions/happy locations/home people/mom

$MEMFS write childhood_forts.md "Sister and I used to build elaborate blanket forts every Friday night. We had rules: no flashlights after midnight, snacks had to be pre-approved, and the dog was always allowed in. We kept this up until I was 14."
tag childhood_forts.md emotions/happy emotions/grateful

$MEMFS write sister_diagnosis.md "Sister called me crying. She was diagnosed with thyroid issues. Doctor says it's very treatable but she needs to take medication daily. Spent two hours on the phone reassuring her. Made her promise to send me her lab results."
tag sister_diagnosis.md dates/2024-09 emotions/anxious topics/health locations/hospital

$MEMFS write paris_trip_plan.md "Sister and I are planning a trip to Paris for her recovery celebration. Looking at flights in June. She wants to see the Musée d'Orsay and eat croissants for every meal. I'm researching hotels in Le Marais."
tag paris_trip_plan.md dates/2025-01 topics/travel locations/paris emotions/happy

CWD="people/mom"; $MEMFS cd /memories/$CWD
$MEMFS write chocolate_cake_recipe.md "Mom's famous chocolate cake recipe. She learned it from Grandma Rose.
- 2 cups flour, 1.5 cups sugar, 3/4 cup cocoa powder
- 2 eggs, 1 cup buttermilk, 1/2 cup vegetable oil
- Secret: pinch of espresso powder in the batter
Bake at 350F for 30 minutes. Frost with ganache.
This is what I made for sister's birthday party."
tag chocolate_cake_recipe.md topics/cooking locations/home

$MEMFS write mom_visit_march.md "Mom surprised everyone by flying in for sister's birthday. She stayed for a week. We cooked together every night — she taught me her pozole recipe. She looked healthy and happy. She said retirement suits her."
tag mom_visit_march.md dates/2025-03 emotions/happy emotions/grateful locations/home

$MEMFS write mom_health_scare.md "Got a call from dad that mom had a dizzy spell at the grocery store. Turned out to be low blood pressure from her new medication. Doctor adjusted the dose. Scared me though — called her every day for two weeks after."
tag mom_health_scare.md dates/2024-06 emotions/anxious topics/health people/dad

CWD="people/alex"; $MEMFS cd /memories/$CWD
$MEMFS write hiking_tamalpais.md "Hiked Mt. Tam with Alex on a perfect clear day. Could see all the way to the Farallon Islands. Saw a red-tailed hawk circling above Stinson. Alex twisted his ankle on the descent but powered through. Stopped at Pelican Inn for fish and chips after."
tag hiking_tamalpais.md dates/2025-01 topics/travel emotions/happy

$MEMFS write alex_promotion.md "Alex got promoted to Staff Engineer! Took him out for celebratory ramen at Mensho. He's been working toward this for 3 years. Really proud of him. He said he might transfer to the London office next year."
tag alex_promotion.md dates/2024-12 topics/work emotions/happy emotions/grateful

$MEMFS write alex_music_recs.md "Alex made me a playlist of Japanese city pop. Standouts: Tatsuro Yamashita 'Ride on Time', Mariya Takeuchi 'Plastic Love', Taeko Ohnuki 'Kusuri wo Takusan'. Been listening on repeat at the office."
tag alex_music_recs.md topics/music locations/office

CWD="people/jamie"; $MEMFS cd /memories/$CWD
$MEMFS write jamie_new_job.md "Jamie started their new job at the climate tech startup. They're nervous but excited. The team is small — only 12 people. They'll be leading the data pipeline work. Took them out for coffee to celebrate."
tag jamie_new_job.md dates/2024-09 topics/work emotions/happy

$MEMFS write jamie_tahoe_weekend.md "Weekend trip to Tahoe with Jamie. Stayed at a cabin near Emerald Bay. Went kayaking, saw a bald eagle. Jamie opened up about feeling burned out before the job switch. Good to see them thriving now."
tag jamie_tahoe_weekend.md dates/2024-12 topics/travel locations/tahoe emotions/grateful

CWD="people/dad"; $MEMFS cd /memories/$CWD
$MEMFS write dad_woodworking.md "Dad showed me the bookshelf he built in his workshop. Solid walnut, hand-cut dovetail joints. He's been working on it for 3 months. He wants to build one for each of us. He seemed really peaceful — retirement looks good on both of them."
tag dad_woodworking.md dates/2025-01 emotions/grateful locations/home

echo "=== Mounting FUSE at $FUSE_MP ==="
if ! start_fuse_mount "$MEMFS" "$FUSE_MP"; then
  echo "ERROR: FUSE mount failed"
  exit 1
fi

echo "=== Mount verified. Facets: $(ls "$FUSE_MP" | tr '\n' ' ') ==="
echo ""
echo "=== Spawning Claude Code sub-agent ==="
echo "=== The agent has NO documentation — only standard Unix tools ==="
echo ""

# --- Spawn the agent ---
# The query is designed to require deep faceted navigation:
#   Q1 forces: emotions/happy + people/sister + dates/2025-03 (3 filters deep)
#   Q2 forces: cross-referencing topics/health/people against topics/travel/people
#   Q3 forces: a write operation through FUSE
claude --print -p "The user stores personal memories at $FUSE_MP. They asked:

1. \"What happy memories do I have about my sister from March 2025?\"
2. \"Which of my travel memories involve someone who had a health scare?\"
3. \"Write me a new memory at $FUSE_MP/people/sister/ about making mom's chocolate cake together last weekend.\"

Use only standard Unix tools (ls, cat, find, grep, echo, etc). No documentation is available.

IMPORTANT: In your final response, show every command you ran and its output so we can trace your exact path through the filesystem. For question 1, try to narrow down using the directory structure rather than reading every file."

echo ""
echo "=== Agent finished ==="

# --- Verify the agent's write landed ---
echo ""
echo "=== Verifying agent write ==="
NEW_FILES=$(ls "$FUSE_MP/people/sister/" | grep -v birthday_surprise | grep -v childhood_forts | grep -v sister_diagnosis | grep -v paris_trip_plan || true)
if [ -n "$NEW_FILES" ]; then
  echo "New file(s) created by agent: $NEW_FILES"
  for f in $NEW_FILES; do
    echo "--- $f ---"
    cat "$FUSE_MP/people/sister/$f"
    echo ""
  done
else
  echo "WARNING: Agent did not create any new files"
fi

# --- Cleanup ---
echo "=== Unmounting ==="
stop_fuse_mount "$FUSE_MP"
echo "=== Done ==="
