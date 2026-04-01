# CLAUDE.md

## What is MemFS?

A virtual faceted memory filesystem for AI coding agents. Memories are tagged documents stored in a Turso (SQLite-compatible) database, navigable as a directory tree where paths are facet:value filter pairs.

```
/memories/people/sister/dates/2025-03   ← filters: people:sister AND dates:2025-03
/memories/dates/2025-03/people/sister   ← same result set (order doesn't matter)
```

## Build & Test

```bash
cargo build --release       # release binary → target/release/memfs
cargo test                  # 32 unit tests
bash tests/test_integration.sh  # 31 integration tests (requires release build)
make integration            # builds + runs integration tests
```

## Architecture

```
src/
├── main.rs       # CLI entry: clap subcommands, env config, dispatch
├── path.rs       # Path parsing, resolution, filter extraction (26 unit tests)
├── state.rs      # Virtual CWD state file (~/.memfs_cwd) read/write
├── db.rs         # Turso connection (local-only default) + schema migrations
├── queries.rs    # All SQL queries — two-step ID resolution pattern
├── engine.rs     # Core orchestration: ties path + state + queries
├── format.rs     # Output formatting (ls columns, cat tags, grep lines)
```

**Design principle:** `main.rs` is thin (parse flags, call engine, print formatted output). All business logic lives in `engine.rs` which composes `path`, `state`, and `queries`.

## Key Invariant

**Navigation invariant:** Path segment order doesn't matter. `/memories/A/1/B/2` and `/memories/B/2/A/1` always produce the same memories. Paths are unordered sets of facet:value filters. `ParsedPath::equivalent()` enforces this via HashSet comparison.

## Path Parsing Rules

Segments after mount point consumed in pairs: `facet/value`.
- Even segment count = complete filter pairs (value level) → ls shows remaining facets + matching memories
- Odd trailing segment = facet category level → ls shows values for that facet
- Zero segments = root → ls shows all facet categories

## Database: Turso

Uses `turso` crate v0.4 (`tursodatabase/turso`). **NOT** libsql (unmaintained). Local-only by default via `Builder::new_local()`. Cloud sync is a TODO.

### Schema

- `memories` (id, filename, content, created_at, updated_at)
- `tags` (id, memory_id, facet, value) — `memory_id=0` rows are placeholders for pre-created values from `mkdir -p`
- `facets` (name TEXT PRIMARY KEY)

### Query Pattern

Turso doesn't support `INTERSECT` in subqueries. All filtered queries use a **two-step approach**:

1. `get_matching_memory_ids()` runs `GROUP BY memory_id HAVING COUNT(DISTINCT facet || ':' || value) = N` to get IDs
2. Use those IDs in a simple `WHERE id IN (id1, id2, ...)` clause

Never use subqueries with compound SELECT in Turso — they fail at runtime.

## Config (env vars)

| Variable | Default | Description |
|----------|---------|-------------|
| `MEMFS_MOUNT` | `/memories` | Virtual mount point |
| `MEMFS_DB` | `~/.memfs.db` | Turso database file |
| `MEMFS_STATE` | `~/.memfs_cwd` | Virtual CWD state file |

## Commands

`memfs {cd, ls, pwd, cat, mkdir, rm, mv, cp, write, append, grep, find}`

- `write`/`append` accept content as arg or via stdin
- `mkdir -p` creates facet categories AND ensures values exist (placeholder tags)
- `mv` retags a memory (changes facet:value), `cp` adds an additional tag
- `rm` deletes a memory, `rm -r` untags all memories from a facet:value

## Pending: FUSE Mount

Plan is in `FUSE_PLAN.md`. Mount MemFS as a real FUSE filesystem so `ls /memories` works natively without the `memfs` binary prefix. Uses `fuser` crate + macFUSE (macOS) / libfuse (Linux). Not yet implemented.
