# MemFS — Virtual Faceted Memory Filesystem

A memory layer for AI agents. Memories are tagged documents stored in a SQLite database, mounted as a real FUSE filesystem. Agents navigate, read, write, and search memories using standard Unix tools — no special commands needed.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/Haakam21/mem-fs/main/install.sh | bash
~/.memfs/memfs init
```

The first command downloads the binaries. The second sets up everything interactively — prompts for optional Turso Cloud credentials, mounts the filesystem, and configures Claude Code.

Requires macFUSE ([macfuse.io](https://macfuse.io)) on macOS, or `apt install fuse3` plus `user_allow_other` in `/etc/fuse.conf` on Linux (memfs uses `AutoUnmount`, which libfuse3 gates behind `user_allow_other`). `memfs init` checks this upfront and prints the exact `echo ... | sudo tee` fix if it's missing — no silent mount failures.

## How It Works

Memories are organized by facets — dimensions like `people`, `dates`, `topics`. Paths are facet:value filter pairs:

```
/memories/people/sister/dates/2025-03/
```

This filters to memories tagged `people:sister` AND `dates:2025-03`. Path order doesn't matter:

```
/memories/dates/2025-03/people/sister/    # same result set
```

At each level, `ls` shows directories (remaining facets/values) and files (matching memories).

## Usage

After install, use standard tools on the `memories/` directory:

```bash
ls memories/                              # list facet categories
ls memories/people/sister/                # memories about sister
cat memories/people/sister/birthday.md    # read a memory
echo "content" > memories/people/sister/new.md  # create a memory
mkdir memories/projects                   # create a new facet
grep -r "birthday" memories/              # keyword search
search "birthday celebration"             # semantic search (by meaning)
```

## Semantic Search

A standalone `search` command finds memories by meaning, not just keywords:

```bash
search "birthday celebration"                    # all memories
search "birthday celebration" ./memories/people  # scoped to a facet
search -k 5 -t 0.4 "cooking recipes"            # top 5, threshold 0.4
```

Uses a local embedding model (all-MiniLM-L6-v2, downloaded on first use). Embeddings generated automatically on write.

## Auto-Tagging

Memories are automatically tagged based on semantic similarity. When you write a memory, its embedding is compared against the centroid of each existing facet:value. If similarity exceeds a threshold, the tag is applied automatically.

For example, if you have 3+ memories tagged `topics:cooking`, writing a new memory about a recipe anywhere in the filesystem will auto-tag it with `topics:cooking` — no manual tagging needed.

Auto-tagging activates once a facet:value has at least 3 tagged memories (configurable via `autotag_min_memories`). The similarity threshold defaults to 0.5 (configurable via `autotag_threshold`).

## Cloud Sync

Sync memories across machines via [Turso Cloud](https://turso.tech). Run `memfs init` and enter your credentials, or create `~/.memfs/settings.json`:

```json
{
  "turso_url": "libsql://your-db.turso.io",
  "turso_token": "your-token"
}
```

`memfs init` is re-runnable — it prompts for URL and token every time, and blank input keeps the existing value. To rotate credentials without editing `settings.json` by hand, re-run `memfs init` (or pipe new values in) and only the fields you supply are overwritten.

Run `~/.memfs/memfs sync` to pull remote changes, merge, and push. FUSE runs local-only for reliability; sync is a separate step.

Sync uses **merge semantics**: memories with unique filenames are auto-merged from both sides. If two agents wrote different content under the same filename, the local version is kept and the remote version is saved to `~/.memfs/conflicts/` for manual reconciliation. After resolving conflicts, run `memfs sync` again.

## Configuration

All data lives in `~/.memfs/` (outside the project directory):

| File | Description |
|------|-------------|
| `~/.memfs/db` | SQLite database |
| `~/.memfs/settings.json` | Cloud sync credentials + search config |
| `~/.memfs/state` | Virtual CWD for CLI |

Optional settings.json fields:

```json
{
  "search_threshold": 0.3,
  "search_limit": 10,
  "autotag_threshold": 0.5,
  "autotag_min_memories": 3
}
```

## Update

```bash
~/.memfs/memfs update
```

Checks for new releases on GitHub and updates both binaries in place.

## Uninstall

```bash
~/.memfs/memfs uninstall          # keeps database
~/.memfs/memfs uninstall --purge  # deletes everything
```

## Development

```bash
cargo test --no-default-features --bin memfs   # fast unit tests (no ONNX, ~5s)
cargo test                                      # full tests including embeddings
make integration                                # 37 integration tests
PKG_CONFIG_PATH="/usr/local/lib/pkgconfig" cargo build --release  # release build
```

## License

MIT
