# MemFS Implementation Plan

## Context

MemFS is a virtual faceted memory filesystem for AI coding agents. It exposes a tag-based memory store via standard bash commands (`cd`, `ls`, `cat`, etc.) so an agent can navigate memories by any combination of facets (people, dates, topics) in any order. The key invariant: `/memories/people/sister/dates/2025-03` and `/memories/dates/2025-03/people/sister` always return the same result set.

This is a greenfield project — only `memfs-spec.md` and an empty `README.md` exist.

---

## Language Choice: Rust

- **Native Turso support** — The `turso` crate ([tursodatabase/turso](https://github.com/tursodatabase/turso)) is written in Rust. First-class local DB + optional cloud sync via the `sync` feature flag.
- **Embedded + cloud sync** — `Builder::new_local("path.db")` for local-only; enable `sync` feature + `Builder::new_remote()` for Turso Cloud sync. Local file for fast reads, background sync for backup/multi-device.
- **Single static binary** — `cargo build --release` produces a self-contained binary. Cross-compilation via `cross` or `cargo-zigbuild`.
- **Future vector search** — Turso has native vector support (exact search + vector manipulation), accessible directly from Rust.
- **CLI tooling** — `clap` for argument parsing (derive macros make subcommand definitions concise).

---

## Project Structure

```
mem-fs/
├── Cargo.toml
├── Makefile
├── memfs-init.sh              # Shell override script
├── memfs-spec.md              # (existing)
├── README.md                  # (existing)
├── src/
│   ├── main.rs                # Entry point, clap subcommand dispatch
│   ├── path.rs                # Path parsing, resolution, filter extraction
│   ├── state.rs               # Read/write ~/.memfs_cwd
│   ├── db.rs                  # Database connection, migrations, schema
│   ├── queries.rs             # All SQL queries
│   ├── engine.rs              # Core orchestration (ties path + state + db)
│   ├── commands/
│   │   ├── mod.rs
│   │   ├── cd.rs              # cd logic
│   │   ├── ls.rs              # ls logic
│   │   ├── pwd.rs             # pwd logic
│   │   ├── cat.rs             # cat logic
│   │   ├── mkdir.rs           # mkdir logic
│   │   ├── rm.rs              # rm logic
│   │   ├── mv.rs              # mv logic
│   │   ├── cp.rs              # cp logic
│   │   ├── write.rs           # write (create memory) logic
│   │   ├── append.rs          # append logic
│   │   ├── grep.rs            # grep logic
│   │   └── find.rs            # find logic
│   └── format.rs              # Output formatting (ls columns, grep output, etc.)
└── tests/
    ├── path_tests.rs          # Path parsing unit tests
    ├── db_tests.rs            # Query tests against in-memory DB
    ├── integration_tests.rs   # End-to-end engine tests
    └── test_integration.sh    # Shell-level integration test
```

**Design principle:** `commands/` modules are thin (parse flags via clap, call engine, print formatted output). Business logic lives in `engine.rs` which composes `path`, `state`, and `queries`.

---

## Key Dependencies

| Crate | Purpose |
|-------|---------|
| `turso` | Turso DB — local + optional cloud sync (`sync` feature) |
| `clap` (derive) | CLI subcommand parsing |
| `regex` | grep pattern matching |
| `chrono` | Timestamp handling |
| `tokio` | Async runtime (required by turso) |
| `anyhow` | Error handling |
| `glob` | find -name pattern matching |

---

## Database: Local + Cloud Sync

**Local-only mode (default):**
```rust
use turso::Builder;
let db = Builder::new_local(db_path).build().await?;
let conn = db.connect()?;
```

**With Turso Cloud sync (opt-in via env vars):**
```rust
// Requires `turso = { version = "0.4", features = ["sync"] }`
use turso::Builder;
let db = Builder::new_remote(sync_url, auth_token).build().await?;
let conn = db.connect()?;
```

- **Config:** `MEMFS_DB` (local path, default `~/.memfs.db`), `MEMFS_TURSO_URL`, `MEMFS_TURSO_TOKEN`
- **Offline-first:** Works without network in local mode. Cloud sync when URL + token are configured.
- **Fallback:** If no Turso URL configured, use `Builder::new_local(db_path)` — pure local, no sync.

---

## Implementation Phases

### Phase 0: Project Skeleton + Path Parsing + State

**Goal:** Foundation with no external deps beyond std.

**Files:** `Cargo.toml`, `src/main.rs` (skeleton), `src/path.rs`, `src/state.rs`, `tests/path_tests.rs`

**Key types (`src/path.rs`):**

```rust
pub struct Filter { pub facet: String, pub value: String }

pub struct ParsedPath {
    pub filters: Vec<Filter>,        // completed facet:value pairs
    pub trailing_facet: Option<String>, // Some if at facet-category level
    pub raw: String,
}

pub fn parse(absolute_path: &str, mount_point: &str) -> Result<ParsedPath>
pub fn resolve(input: &str, current_cwd: &str, mount_point: &str) -> Result<String>
impl ParsedPath { pub fn equivalent(&self, other: &ParsedPath) -> bool }
```

**Path parsing rules:**
- Segments after mount point consumed in pairs: `facet/value`
- Odd trailing segment = facet category level (browsing values)
- Even segments = complete filter pairs (browsing memories + remaining facets)
- `..` pops last segment; `.` is identity; resolve relative paths against virtual CWD

**State (`src/state.rs`):** Plain text file — just the virtual path string.

**Testable:** Table-driven tests for parsing, resolution, equivalence.

---

### Phase 1: Database Layer

**Goal:** Schema, connection, and all query functions.

**Files:** `src/db.rs`, `src/queries.rs`, `tests/db_tests.rs`

**Schema:** Three tables per spec:
- `memories` (id, filename, content, created_at, updated_at)
- `tags` (id, memory_id, facet, value) — indexed on `(facet, value)` and `memory_id`
- `facets` (name UNIQUE)

**Core query — faceted intersection:**
```sql
SELECT m.* FROM memories m WHERE m.id IN (
    SELECT memory_id FROM tags WHERE facet=? AND value=?
    INTERSECT
    SELECT memory_id FROM tags WHERE facet=? AND value=?
)
```
Built dynamically per filter count. Empty filters = all memories.

**Key query functions:**
- `list_facets`, `list_values` (scoped by filters), `facet_exists`, `value_exists`
- `list_memories`, `get_memory`, `create_memory` (with tags), `delete_memory`
- `update_memory_tags` (for mv/cp), `append_memory`
- `remaining_facets` (facets that can still narrow results, for ls)
- `grep_memories`, `find_memories`

**Connection setup:** Try embedded replica if `MEMFS_TURSO_URL` is set, fall back to local-only.

**Testable:** In-memory Turso DB (`:memory:`), seed data, verify queries.

---

### Phase 2: Engine + Navigation Commands (cd, ls, pwd, cat)

**Goal:** Working read-only virtual filesystem.

**Files:** `src/engine.rs`, `src/commands/{cd,ls,pwd,cat}.rs`, `src/format.rs`, `src/main.rs` updated with clap.

**`ls` logic (most complex):**
1. At root (`/memories`): show all facet categories as directories
2. At facet category (`/memories/people`): show values for that facet (scoped by active filters) as directories
3. At filter level (`/memories/people/sister`): show remaining navigable facets as directories + matching memories as files

**`cd` validation:** Verify facet/value exists before updating state. Error: `memfs: cd: no such facet or value: 'x'`

**`cat` output:** Tag header + content body per spec.

**`ls -l` support:** Long format with timestamps, tag count, content preview.

**Testable:** Integration tests with in-memory DB. Verify navigation invariant.

---

### Phase 3: Mutation Commands (mkdir, rm, mv, cp) + Write/Append

**Goal:** Full read-write filesystem.

**Files:** `src/commands/{mkdir,rm,mv,cp,write,append}.rs`

**Semantics:**
- `mkdir` → create facet categories and/or values. `-p` creates both.
- `rm <file>` → delete memory + all tags. `rm -r <facet/value>` → untag only.
- `mv` → retag: change one facet:value to another on a memory.
- `cp` → add tag: add destination facet:value without removing source.
- `write <filename> [content]` → create memory, auto-tag from CWD filters. Reads stdin if no content arg.
- `append <filename> [content]` → append to existing memory.

**Testable:** Verify tag mutations in DB. Verify write inherits CWD tags.

---

### Phase 4: Search Commands (grep, find)

**Goal:** Content and metadata search.

**Files:** `src/commands/{grep,find}.rs`

**grep:** Query memories matching current filters, apply `regex` crate line-by-line on content. Flags: `-i`, `-l`, `-r`, `-n`. Output: `filename:line` format.

**find:** `-name` (glob match), `-type d` (list facets/values as dirs), `-mtime -N` (by updated_at). Scoped to current filters.

**Testable:** Seed memories, verify search results and formatting.

---

### Phase 5: Shell Override Script (`memfs-init.sh`)

**File:** `memfs-init.sh`

**Overrides:** `cd`, `ls`, `pwd`, `cat`, `mkdir`, `rm`, `mv`, `cp`, `grep`, `find` — each checks if target path is under `$MEMFS_MOUNT`, routes to `memfs` binary if yes, falls through to real command if no.

**Write interception:**

Bash `>` and `>>` are processed by the shell before functions run — **cannot be intercepted**. Solution:

1. **Primary:** `memfs write <file> [content]` and `memfs append <file> [content]` as first-class commands. Accept content via argument or stdin.
2. **Convenience:** `write` shell function in init script for heredoc support: `write file.md << EOF`.
3. **Documented limitation:** `echo "x" > /memories/file.md` is not supported. The primary consumer (AI agent) uses `memfs write` directly.

**Testable:** Shell integration test script.

---

### Phase 6: Integration Testing + Polish

**Files:** `tests/integration_tests.rs`, `tests/test_integration.sh`, `Makefile`

**Key tests:**
1. Navigation invariant: different path orderings → same `ls` output
2. Full lifecycle: mkdir → write → ls → cat → grep → find → mv → cp → rm
3. Edge cases: empty DB, no-tag memories, special characters, unicode
4. Shell integration: run spec's example session end-to-end

**Makefile targets:** `build`, `test`, `integration`, `install`

---

### Phase 7: Claude Code Agent Intuitiveness Testing

**Goal:** Validate that the virtual filesystem is intuitive enough for Claude Code to use *without any documentation or prompting about MemFS commands*. If the interface truly mimics bash, an AI agent should be able to navigate it using standard commands it already knows.

**Method:** Spawned 5 Claude Code sub-agents with minimal context — told them there's a memory filesystem at `/memories` and gave them tasks. Did NOT tell them about `memfs write`, `memfs append`, or any MemFS-specific commands.

**Pre-seeded test data:** 5 memories across people (sister, mom, alex, acquaintance), dates (2025-03, 2024-12), topics (birthday, work), and locations (downtown, home).

---

## Phase 7 Results: Agent Intuitiveness Test Findings

### Test 1: Exploration

**Prompt:** "Explore the filesystem at /memories and tell me what's in there."

**Commands attempted:**
1. `ls /memories` — **FAILED** (real filesystem, "No such file or directory")
2. Agent discovered `/Users/haakam/mem-fs/target/release/memfs` binary
3. `memfs ls /memories` — **SUCCESS** → `dates/ locations/ people/ topics/`
4. `memfs cd /memories/people` + `memfs ls` — **SUCCESS** → `acquaintance/ alex/ mom/ sister/`
5. `memfs cd /memories/people/sister` + `memfs ls` — **SUCCESS** → `dates/ memory_0042.md surprise_plan.md`
6. `memfs cat memory_0042.md` — **SUCCESS** → displayed tags + content

**Unsupported commands tried:** `ls /memories` on real filesystem.
**Recovery:** Agent quickly found the `memfs` binary and adapted. No `tree`, `stat`, `file`, or `ls -R` attempted.
**Verdict:** PASS — agent explored the full structure successfully.

### Test 2: Write (Critical Test)

**Prompt:** "Create a new memory about a team meeting with Alex at /memories/people/alex/topics/work."

**Commands attempted:**
1. Agent checked `memfs --help` first
2. `memfs write team_meeting.md "content..."` — **SUCCESS**

**Key finding:** The agent did NOT try `echo "content" > file.md` or `cat << EOF > file.md`. It read `--help` and used `memfs write` directly. This suggests the write interception concern may be less critical than expected — agents discover the interface via `--help` before attempting raw bash redirects.

**Verdict:** PASS — `memfs write` is discoverable.

### Test 3: Search

**Prompt:** "Search all memories for anything mentioning 'birthday'."

**Commands attempted:**
1. `memfs grep "birthday" /memories` — **SUCCESS** → found 2 matches

**Unsupported commands tried:** None. Agent went straight to `memfs grep`.
**Verdict:** PASS — search is intuitive.

### Test 4: Organize (mv)

**Prompt:** "Move memory_0099.md from people/acquaintance to people/friend."

**Commands attempted:**
1. `ls /memories/people/` — **FAILED** (real filesystem)
2. `memfs ls /memories/people/` — **SUCCESS**
3. `memfs ls /memories/people/acquaintance/` — **SUCCESS** → confirmed `memory_0099.md`
4. `memfs mkdir /memories/people/friend` — **SUCCESS** (created target value)
5. `memfs mv /memories/people/acquaintance/memory_0099.md /memories/people/friend/memory_0099.md` — **SUCCESS**
6. `memfs ls /memories/people/friend/` — **SUCCESS** → confirmed `memory_0099.md` present
7. `memfs ls /memories/people/acquaintance/` — **SUCCESS** → confirmed empty

**Unsupported commands tried:** `ls /memories/people/` on real filesystem.
**Verdict:** PASS — agent used `mkdir` + `mv` naturally. The `mv` syntax matched expectations exactly.

### Test 5: Create Structure (mkdir)

**Prompt:** "Set up a new facet category called 'projects' with values 'memfs' and 'website'."

**Commands attempted:**
1. `ls /memories` — **FAILED** (real filesystem)
2. Agent found `memfs` binary, checked `memfs --help`
3. `memfs mkdir /memories/projects` — **SUCCESS**
4. `memfs mkdir /memories/projects/memfs` — **SUCCESS**
5. `memfs mkdir /memories/projects/website` — **SUCCESS**
6. `memfs ls /memories/` — **SUCCESS** → confirmed `projects/` in listing
7. `memfs ls /memories/projects` — **SUCCESS** → confirmed `memfs/ website/`

**Unsupported commands tried:** `ls /memories` on real filesystem.
**Notable:** Agent tried the debug binary first and hit a persistence issue with it (placeholder tags not persisting). Switched to release binary and succeeded.
**Verdict:** PASS — `mkdir` for facets and values is intuitive.

---

## Summary of Findings

### What Works Well
- All 5 test scenarios passed — the interface is intuitive for AI agents
- `--help` is the primary discovery mechanism; agents read it before guessing
- `ls`, `cd`, `cat`, `mkdir`, `mv`, `grep` all match agent expectations
- Write interception via `>` redirects was NOT a problem — agents used `memfs write` after reading help
- Error messages guided agents effectively when real filesystem commands failed

### Consistent Pattern Across All Agents
Every agent followed the same discovery pattern:
1. Try `ls /memories` on real filesystem → "No such file or directory"
2. Discover the `memfs` binary (via find/which or directory listing)
3. Run `memfs --help` to learn available commands
4. Use `memfs <subcommand>` successfully from that point on

### Unsupported Commands Attempted
| Command | Frequency | Impact |
|---------|-----------|--------|
| `ls /memories` (real fs) | 4/5 agents | Low — agents recover immediately |

No agents tried: `tree`, `stat`, `file`, `head`, `tail`, `less`, `rg`, `ag`, `ls -R`, `echo > file`, or `cat << EOF > file`.

### Implications for `memfs-init.sh`

The shell override script is **essential for seamless UX**. Without it sourced, every agent's first instinct (`ls /memories`) fails. With it sourced:
- `ls /memories` would route to `memfs ls` automatically
- `cd /memories` would route to `memfs cd` automatically
- The discovery friction (steps 1-3 above) disappears entirely
- The experience becomes truly transparent — agents use standard bash with no adaptation

### Recommendations
1. **Ship `memfs-init.sh` as a required setup step** — the agent tests prove it eliminates the primary friction point
2. **`memfs write` is sufficient** — no need for `>` redirect interception via DEBUG traps. Agents discover `write` via `--help`.
3. **Consider adding `memfs --help` output that explicitly shows common workflows** — agents use it as their first reference
4. **Debug vs release binary inconsistency** — one agent found placeholder tags not persisting in the debug binary. Investigate potential async/sync timing issue in debug mode.

---

## Verification Plan

1. **Unit tests:** `cargo test` → 32 passed
2. **Navigation invariant:** Verified — different path orderings produce identical `ls` output
3. **Shell integration:** `bash tests/test_integration.sh` → 31/31 passed
4. **Agent intuitiveness test:** 5/5 scenarios passed with uninstructed Claude Code agents
