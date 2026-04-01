# MemFS — Virtual Faceted Memory Filesystem

MemFS is a virtual filesystem that exposes a faceted, multi-dimensional memory store via standard bash commands. Memories are tagged documents that can be navigated from multiple angles — by person, date, topic, or any user-defined facet — in any order, always converging on the same result set.

Designed as the memory layer for a personal AI agent.

## How It Works

Memories live in a [Turso](https://github.com/tursodatabase/turso) database (SQLite-compatible). MemFS presents them as a navigable directory tree where paths are facet filters:

```
/memories/people/sister/dates/2025-03/
```

This path filters to memories tagged `people:sister` AND `dates:2025-03`. The same memories are reachable via any ordering:

```
/memories/dates/2025-03/people/sister/    # same result set
```

At each level, `ls` shows:
- **Directories** — remaining facets and values that can further narrow results
- **Files** — memories matching all current filters

## Quick Start

### Prerequisites

- Rust toolchain (`rustup`)
- macFUSE (macOS): `brew install macfuse`
- libfuse (Linux): `apt install libfuse-dev`

### Build

```bash
make build
# or
cargo build --release
```

### Usage

**Mount as a real filesystem (recommended):**

```bash
memfs mount /memories
```

Then use standard commands:

```bash
ls /memories
# dates/  people/  topics/

cd /memories/people/sister
ls
# dates/  topics/  memory_0042.md  surprise_plan.md

cat memory_0042.md
# --- tags: people:sister, dates:2025-03, topics:birthday ---
# Had a wonderful birthday celebration for my sister in March.

echo "New memory content" > /memories/people/alex/topics/work/meeting_notes.md

grep -r "birthday" /memories
# memory_0042.md:Had a wonderful birthday celebration for my sister in March.
```

**Direct CLI usage (no mount required):**

```bash
memfs mkdir -p /memories/people/sister/dates/2025-03
memfs cd /memories/people/sister/dates/2025-03
memfs write memory_0042.md "Birthday celebration for my sister."
memfs ls
memfs cat memory_0042.md
memfs grep "birthday" /memories
```

## Commands

| Command | Description |
|---------|-------------|
| `cd <path>` | Navigate the faceted tree |
| `ls [path]` | List facets, values, and memories at current scope |
| `pwd` | Print current virtual working directory |
| `cat <file>` | Display memory content with tags |
| `mkdir [-p] <path>` | Create facet categories or values |
| `rm [-r] <target>` | Delete a memory or untag a facet value |
| `mv <src> <dst>` | Retag a memory (move between facet values) |
| `cp <src> <dst>` | Add an additional tag to a memory |
| `write <file> [content]` | Create a new memory (reads stdin if no content) |
| `append <file> [content]` | Append to an existing memory |
| `grep <pattern> [path]` | Search memory content with regex |
| `find [path] --name <pattern>` | Search by filename or metadata |
| `mount <path>` | Mount as FUSE filesystem |

## Core Concepts

### Facets

Facets are dimensions for organizing memories: `people`, `dates`, `topics`, `locations`, or any custom category. Each facet has values (e.g., `people/sister`, `dates/2025-03`). Facets are fully dynamic — create new ones with `mkdir`.

### Tagging

When a memory is created inside a faceted path, it inherits those facet:value pairs as tags. Navigate to `/memories/people/sister/topics/birthday` and create a file — it's automatically tagged `people:sister, topics:birthday`.

### Navigation Invariant

The path `/memories/A/1/B/2` and `/memories/B/2/A/1` always produce the same memories. Paths are unordered sets of facet:value filters.

## Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `MEMFS_MOUNT` | `/memories` | Virtual filesystem mount point |
| `MEMFS_DB` | `~/.memfs.db` | Path to the Turso database file |
| `MEMFS_STATE` | `~/.memfs_cwd` | Path to the virtual CWD state file |
| `MEMFS_TURSO_URL` | — | Turso Cloud sync URL (optional) |
| `MEMFS_TURSO_TOKEN` | — | Turso Cloud auth token (optional) |

## Database

MemFS uses [Turso](https://github.com/tursodatabase/turso) (SQLite-compatible, written in Rust) in embedded mode. Local-only by default. Set `MEMFS_TURSO_URL` and `MEMFS_TURSO_TOKEN` to enable cloud sync for backup and multi-device access.

## Development

```bash
cargo test          # 32 unit tests
make integration    # 31 integration tests
cargo build         # debug build
make build          # release build
```

## License

MIT
