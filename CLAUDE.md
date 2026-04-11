# CLAUDE.md

## What is MemFS?

A virtual faceted memory filesystem for AI coding agents. Memories are tagged documents stored in a Turso (SQLite-compatible) database, navigable as a directory tree where paths are facet:value filter pairs.

```
/memories/people/sister/dates/2025-03   ← filters: people:sister AND dates:2025-03
/memories/dates/2025-03/people/sister   ← same result set (order doesn't matter)
```

## Build & Test

```bash
cargo build --release                          # release binary → target/release/memfs + search
cargo test --no-default-features --bin memfs    # fast: 30 unit tests, no ONNX deps (~5s)
cargo test                                     # full: includes embedding tests (~30s, needs model)
make integration                               # 37 integration tests (requires release build)
```

**Feature flags:** The `search` feature (default: on) pulls in `ort`, `tokenizers`, `ndarray`, `rusqlite`. Disable with `--no-default-features` for fast iteration on non-search code.

## Architecture

```
src/
├── main.rs         # CLI entry: clap subcommands, dispatch. Mount/Unmount handled
│                   #   BEFORE tokio runtime to avoid nested-runtime panic.
├── engine.rs       # Core orchestration: ties path + state + queries + embeddings
├── queries.rs      # All SQL queries — two-step ID resolution pattern
├── path.rs         # Path parsing, resolution, filter extraction
├── state.rs        # Virtual CWD state file read/write
├── db.rs           # Turso connection (local or sync), schema migrations, Db enum
├── settings.rs     # Reads ~/.memfs/settings.json (turso creds, search config)
├── embeddings.rs   # ONNX model loading, tokenization, embedding, cosine similarity
├── format.rs       # Output formatting (ls columns, cat tags, grep lines, search results)
├── fuse.rs         # FUSE filesystem: implements fuser::Filesystem trait
├── util.rs         # Shared utilities (expand_tilde)
├── lib.rs          # Library crate exposing modules for the search binary
└── bin/
    └── search.rs   # Standalone semantic search binary (installed to PATH)
```

**Design principles:**
- `main.rs` is thin — parse flags, call engine, print formatted output
- All business logic lives in `engine.rs` which composes `path`, `state`, and `queries`
- The FUSE layer (`fuse.rs`) calls `queries` directly (not through Engine) to avoid CWD state file dependency
- The `search` binary reads the DB via `rusqlite` (read-only copy) to bypass the FUSE daemon's file lock

## Key Invariant

**Navigation invariant:** Path segment order doesn't matter. `/memories/A/1/B/2` and `/memories/B/2/A/1` always produce the same memories. Paths are unordered sets of facet:value filters.

## Path Parsing Rules

Segments after mount point consumed in pairs: `facet/value`.
- Even segment count = complete filter pairs (value level) → ls shows remaining facets + matching memories
- Odd trailing segment = facet category level → ls shows values for that facet + memory files tagged under that facet
- Zero segments = root → ls shows all facet categories

## Database

Uses `turso` crate v0.4 with `sync` feature. Local-only by default (`Builder::new_local`). Cloud sync enabled via `~/.memfs/settings.json` with `turso_url` and `turso_token` — sync is a separate explicit step via `memfs sync`.

### Schema

- `memories` (id, filename, content, created_at, updated_at)
- `tags` (id, memory_id, facet, value) — `memory_id=NULL` rows are placeholders for pre-created values from `mkdir -p`
- `facets` (name TEXT PRIMARY KEY)
- `embeddings` (memory_id PK, embedding BLOB, model_version TEXT)

### Query Pattern

Turso doesn't support `INTERSECT` in subqueries. All filtered queries use a **two-step approach**:

1. `get_matching_memory_ids()` runs `GROUP BY memory_id HAVING COUNT(DISTINCT facet || ':' || value) = N` to get IDs
2. Use those IDs in a simple `WHERE id IN (id1, id2, ...)` clause

Never use subqueries with compound SELECT in Turso — they fail at runtime.

## Config

All data lives in `~/.memfs/` (outside the project directory, hidden from agents):

```
~/.memfs/
├── db              # SQLite database
├── state           # Virtual CWD for CLI
├── settings.json   # Optional: cloud sync + search config
├── memfs           # Binary
├── mount/          # Global FUSE mount point (single daemon)
└── models/         # ONNX embedding model
```

### settings.json

```json
{
  "turso_url": "libsql://your-db.turso.io",
  "turso_token": "your-token",
  "search_threshold": 0.3,
  "search_limit": 10
}
```

All fields optional. Defaults: local-only DB, threshold 0.3, limit 10.

### Environment variables (override defaults)

| Variable | Default | Description |
|----------|---------|-------------|
| `MEMFS_MOUNT` | `/memories` | Virtual mount point |
| `MEMFS_DB` | `~/.memfs/db` | Database path |
| `MEMFS_STATE` | `~/.memfs/state` | CWD state path |
| `MEMFS_MODEL_PATH` | `~/.memfs/models` | ONNX model directory |

## Commands

`memfs {cd, ls, pwd, cat, mkdir, rm, mv, cp, write, append, grep, find, search, reindex, sync, mount, unmount, init, update, uninstall}`

- `write`/`append` accept content as arg or via stdin
- `mkdir -p` creates facet categories AND ensures values exist (placeholder tags)
- `mv` retags a memory (changes facet:value), `cp` adds an additional tag
- `rm` deletes a memory, `rm -r` untags all memories from a facet:value
- `mount <path>` mounts as FUSE filesystem, `unmount <path>` unmounts
- `search "query"` semantic search with embeddings
- `reindex` generates embeddings for all memories
- `sync` pulls from cloud then pushes local changes
- `init` interactive setup (cloud credentials, mount, Claude Code config)
- `update` self-update to latest GitHub release
- `uninstall [--purge]` remove binaries and config

## Semantic Search

Uses `all-MiniLM-L6-v2` (384-dim ONNX model, ~80MB). Model downloaded to `~/.memfs/models/` on first use of `search` or `reindex`.

- Embeddings generated automatically on write (CLI and FUSE)
- Stored in `embeddings` table as BLOB (1536 bytes per memory)
- Standalone `search` binary installed to `~/.local/bin/search` (on PATH, discoverable by agents)
- Search binary reads DB via rusqlite with read-only copy to bypass FUSE daemon's file lock

## FUSE Mount

### Prerequisites

- **macOS:** macFUSE (`brew install macfuse` or macfuse.io) + kernel extension approval in System Settings > Privacy & Security
- **Linux:** `apt install libfuse-dev` or `dnf install fuse-devel`
- **Build:** `PKG_CONFIG_PATH="/usr/local/lib/pkgconfig" cargo build --release`

### FUSE Architecture (`src/fuse.rs`)

**Inode strategy:**
- Root = inode 1. Directories dynamically allocated from 2 upward.
- Files: `inode = memory_id + 1_000_000` (deterministic).
- Bidirectional `HashMap<u64, String>` for directory inode ↔ path mapping.

**Async bridge:** Stores a `tokio::runtime::Runtime`. FUSE trait methods are sync; each calls `rt.block_on(async { ... })`.

**Key design decisions:**
- Directories always win over files in `lookup()` (facet name shadows memory filename)
- `read()` returns raw content only (no tags header)
- `main.rs` handles `Mount`/`Unmount` before creating tokio runtime (avoids nested-runtime panic)
- Write buffering: `create()` inserts empty memory immediately (stable inode), `write()` buffers, `release()` flushes
- Truncation via `setattr(size=0)` — macFUSE strips O_TRUNC from open flags
- Attribute TTL = 0 (no kernel caching) to prevent stale reads after writes
- Temp/editor files (`.tmp.*`, `._*`, `.#*`, `*~`) skipped during auto-tagging; `._*` and `.#*` also hidden from lookup/readdir
- At facet-level, writing `people/haakam.md` auto-tags with `people:haakam` (file_stem)
- Untagged files allowed at root level — root is the "inbox"
- `rename()` deletes existing target (Unix semantics) to prevent duplicate memories from atomic writes
- Single daemon at `~/.memfs/mount`, project directories symlink `./memories → ~/.memfs/mount`

## Cloud Sync

FUSE always uses local-only DB (`Builder::new_local`). Cloud sync is a separate operation via `memfs sync` which:
1. Unloads launchd/systemd service (prevents auto-restart), then stops daemon and unmounts
2. Reads all local data into memory via `new_local` connection
3. Removes sync metadata files, opens `new_remote` sync connection, pulls remote changes
4. Re-inserts all local data through the sync connection (in a transaction) so the CDC engine tracks it, then pushes
5. Reloads launchd/systemd service

The re-insert step is necessary because FUSE writes go through `new_local` which doesn't create CDC (Change Data Capture) entries. The sync builder also overwrites the local DB when setting up its embedded replica, so data must be read before the sync connection opens.

Without credentials in `~/.memfs/settings.json`, everything works local-only.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Haakam21/mem-fs/main/install.sh | bash
~/.memfs/memfs init
```

Install downloads binaries (`memfs` to `~/.memfs/`, `search` to `~/.local/bin/`). `init` interactively sets up cloud sync credentials, starts a single FUSE daemon at `~/.memfs/mount`, seeds starter facets, creates a `./memories` symlink + `CLAUDE.md` + `.claude/settings.json`. Running `init` in additional directories creates symlinks to the same shared mount. `memfs update` self-updates to the latest release. FUSE daemon managed by launchd (macOS) or systemd (Linux) — auto-restarts on crash, starts on login.

## Testing

```bash
cargo test --no-default-features --bin memfs   # fast unit tests (no ONNX)
cargo test                                      # full tests including embeddings
make integration                                # 37 bash integration tests
bash tests/test_fuse_agent.sh                   # spawn uninstructed agent against FUSE mount
```

## Development Workflow

- **Always run `cargo test` after making changes** — before committing, verify all tests pass.
- **Update README.md and CLAUDE.md when behavior changes** — if you change how a feature works (sync, FUSE, install, etc.), update the corresponding docs in the same commit.
