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
├── settings.rs     # Reads .memfs/settings.json (turso creds, search config)
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

Uses `turso` crate v0.4 with `sync` feature. Local-only by default. Cloud sync enabled via `.memfs/settings.json` with `turso_url` and `turso_token` — uses embedded replica with fire-and-forget async push after writes and pull on mount.

### Schema

- `memories` (id, filename, content, created_at, updated_at)
- `tags` (id, memory_id, facet, value) — `memory_id=0` rows are placeholders for pre-created values from `mkdir -p`
- `facets` (name TEXT PRIMARY KEY)
- `embeddings` (memory_id PK, embedding BLOB, model_version TEXT)

### Query Pattern

Turso doesn't support `INTERSECT` in subqueries. All filtered queries use a **two-step approach**:

1. `get_matching_memory_ids()` runs `GROUP BY memory_id HAVING COUNT(DISTINCT facet || ':' || value) = N` to get IDs
2. Use those IDs in a simple `WHERE id IN (id1, id2, ...)` clause

Never use subqueries with compound SELECT in Turso — they fail at runtime.

## Config

All per-install data lives in `.memfs/`:

```
.memfs/
├── db              # SQLite database
├── state           # Virtual CWD for CLI
└── settings.json   # Optional: cloud sync + search config
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
| `MEMFS_DB` | `./.memfs/db` | Database path |
| `MEMFS_STATE` | `./.memfs/state` | CWD state path |
| `MEMFS_MODEL_PATH` | `~/.memfs/models` | ONNX model directory |

## Commands

`memfs {cd, ls, pwd, cat, mkdir, rm, mv, cp, write, append, grep, find, search, reindex, sync, mount, unmount}`

- `write`/`append` accept content as arg or via stdin
- `mkdir -p` creates facet categories AND ensures values exist (placeholder tags)
- `mv` retags a memory (changes facet:value), `cp` adds an additional tag
- `rm` deletes a memory, `rm -r` untags all memories from a facet:value
- `mount <path>` mounts as FUSE filesystem, `unmount <path>` unmounts
- `search "query"` semantic search with embeddings
- `reindex` generates embeddings for all memories
- `sync` pulls from cloud then pushes local changes

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
- Temp files (`.tmp.*`) skipped during auto-tagging — Claude Code's Write tool uses atomic writes
- At facet-level, writing `people/haakam.md` auto-tags with `people:haakam` (file_stem)
- Cloud sync: fire-and-forget `push()` spawned after every mutation

## Cloud Sync

When `.memfs/settings.json` has `turso_url` and `turso_token`:
- `db::open()` uses `turso::sync::Builder` with `bootstrap_if_empty(true)`
- On mount/open, pulls latest from cloud
- After every mutation (CLI and FUSE), spawns async `push()` (fire-and-forget)
- `memfs sync` does explicit pull + push
- Without credentials, everything works local-only (no change from default)

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Haakam21/mem-fs/main/install.sh | bash
```

Installs `memfs` to `~/.memfs/memfs` (hidden from agents), `search` to `~/.local/bin/search` (on PATH). Creates `./memories/` FUSE mount, `.memfs/` data dir, `CLAUDE.md`, and `.claude/settings.json`.

## Testing

```bash
cargo test --no-default-features --bin memfs   # fast unit tests (no ONNX)
cargo test                                      # full tests including embeddings
make integration                                # 37 bash integration tests
bash tests/test_fuse_agent.sh                   # spawn uninstructed agent against FUSE mount
```
