# MemFS FUSE Mount Implementation Plan

## Context

MemFS has a working CLI binary (`memfs`) with all commands implemented (cd, ls, cat, mkdir, rm, mv, cp, write, append, grep, find) backed by a Turso database. Agent intuitiveness testing revealed a critical UX gap: every Claude Code agent's first instinct is `ls /memories` on the real filesystem, which fails because `/memories` doesn't exist on disk. Agents must discover the `memfs` binary and learn its interface via `--help`.

**The fix:** Mount MemFS as a real FUSE filesystem at `/memories`. This makes `ls /memories`, `cat /memories/people/sister/memory_0042.md`, and even `echo "content" > /memories/.../file.md` work transparently — no shell overrides, no binary discovery, no learning curve. Every standard tool (ls, cat, grep, find, tree, stat) works natively.

---

## Approach: Add `memfs mount` Subcommand

Add a new `memfs mount /memories` command that starts a FUSE daemon. The existing CLI commands remain for direct use. The FUSE layer wraps the existing `Engine` — no business logic changes needed.

**New dependency:** `fuser` crate (Rust FUSE implementation, supports macOS via macFUSE and Linux via libfuse).

---

## Architecture

```
memfs mount /memories     ← new: starts FUSE daemon
      │
      ▼
  src/fuse.rs             ← new: implements fuser::Filesystem trait
      │
      ▼
  src/engine.rs           ← existing: all business logic (unchanged)
      │
      ▼
  src/queries.rs          ← existing: all SQL queries (unchanged)
      │
      ▼
  Turso DB                ← existing: unchanged
```

The FUSE layer (`fuse.rs`) is a thin adapter that:
1. Translates FUSE inode operations → virtual paths
2. Calls existing `Engine` methods
3. Returns FUSE-formatted replies

---

## Key Design Decisions

### 1. Inode Mapping

FUSE identifies files/dirs by inode numbers, not paths. We need a stable mapping:

- **Inode 1** = root (`/memories`)
- **Dynamically allocated inodes** for facets, values, and memory files
- Store a bidirectional `HashMap<u64, String>` (inode ↔ virtual path) in the `MemfsFs` struct
- Inodes are allocated on `lookup()` and cached for the session
- Memory files also get a separate inode for each unique path they appear at

### 2. Async Bridge

The existing engine is async (Turso requires `tokio`), but `fuser::Filesystem` trait methods are synchronous. Bridge with:

```rust
fn blocking<F: Future<Output = T>, T>(future: F) -> T {
    self.runtime.block_on(future)
}
```

Store a `tokio::runtime::Runtime` in the FUSE struct and use `block_on()` to call async engine methods from sync FUSE callbacks.

### 3. Directory Structure Mapping

The virtual filesystem tree maps to FUSE as:

```
/memories/                          → inode 1 (root dir)
/memories/people/                   → inode for facet "people" (dir)
/memories/people/sister/            → inode for value "sister" under "people" (dir)
/memories/people/sister/dates/      → inode for facet "dates" in context of people:sister (dir)
/memories/people/sister/dates/2025-03/  → inode for value (dir)
/memories/people/sister/memory_0042.md  → inode for file (regular file)
```

At each directory level, `readdir()` calls `engine.ls()` to get the entries.

### 4. Write Support

FUSE provides native write interception — `echo "content" > /memories/.../file.md` triggers:
1. `create()` → allocate a new file, return file handle
2. `write()` → buffer content in memory
3. `flush()`/`release()` → commit buffered content to DB via `engine.write()`

For append (`>>`):
1. `open()` with append flag → return file handle
2. `write()` → buffer appended content
3. `release()` → commit via `engine.append()`

Store in-progress writes in a `HashMap<u64, WriteBuffer>` keyed by file handle.

### 5. File Attributes

`getattr()` must return realistic `FileAttr`. Map from existing data:

| Attribute | Directories (facets/values) | Files (memories) |
|-----------|---------------------------|------------------|
| kind | `FileType::Directory` | `FileType::RegularFile` |
| size | 0 | `content.len()` (+ tags header) |
| perm | 0o755 | 0o644 |
| nlink | 2 | 1 |
| uid/gid | current user | current user |
| atime/mtime | now | `updated_at` from DB |
| crtime | now | `created_at` from DB |

---

## New Files

### `src/fuse.rs` — FUSE filesystem implementation

```rust
pub struct MemfsFs {
    engine: Engine,
    runtime: tokio::runtime::Runtime,
    // Inode management
    next_inode: AtomicU64,
    inode_to_path: RwLock<HashMap<u64, String>>,
    path_to_inode: RwLock<HashMap<String, u64>>,
    // Write buffer for in-progress file writes
    next_fh: AtomicU64,
    write_buffers: RwLock<HashMap<u64, WriteBuffer>>,
}
```

**Methods to implement from `fuser::Filesystem`:**

| Method | Backed by | Notes |
|--------|-----------|-------|
| `init()` | — | Allocate root inode |
| `getattr(ino)` | `engine.ls()` or `engine.cat()` | Return `FileAttr` |
| `lookup(parent, name)` | path resolution + validation | Allocate inode, return `FileAttr` |
| `readdir(ino, offset)` | `engine.ls(path)` | Iterate entries with offset |
| `open(ino, flags)` | — | Return file handle |
| `read(ino, fh, offset, size)` | `engine.cat(filename)` | Return content slice |
| `create(parent, name, ...)` | `engine.write(name, "")` | Create empty, return fh |
| `write(ino, fh, offset, data)` | buffer in `write_buffers` | Buffer until release |
| `release(ino, fh, ...)` | `engine.write()` or `engine.append()` | Flush buffer to DB |
| `mkdir(parent, name, ...)` | `engine.mkdir(path)` | Create facet/value |
| `unlink(parent, name)` | `engine.rm(filename)` | Delete memory |
| `rmdir(parent, name)` | `engine.rm(path, recursive=true)` | Remove facet value |
| `rename(...)` | `engine.mv(src, dst)` | Retag memory |

### Modifications to `src/main.rs`

Add a `Mount` subcommand:

```rust
Commands::Mount {
    mountpoint: String,    // e.g., "/memories"
    #[arg(short, long)]
    foreground: bool,      // run in foreground (default: daemonize)
}
```

### Modifications to `Cargo.toml`

```toml
fuser = "0.15"
```

---

## Implementation Steps

### Step 1: Add `fuser` dependency and `Mount` subcommand to CLI

- Update `Cargo.toml` with `fuser`
- Add `Mount` variant to `Commands` enum in `main.rs`
- Stub out the mount handler

### Step 2: Core FUSE struct with inode management

- Create `src/fuse.rs` with `MemfsFs` struct
- Implement inode allocation and path ↔ inode mapping
- Implement `init()` and `getattr()` for root
- Implement `lookup()` — the core method that resolves names to inodes

### Step 3: Read-only operations

- Implement `readdir()` backed by `engine.ls()`
- Implement `open()` + `read()` backed by `engine.cat()`
- **Test:** `memfs mount /memories` then `ls /memories`, `cat /memories/.../file.md`

### Step 4: Write operations

- Implement `create()` + `write()` + `release()` with write buffering
- Implement `mkdir()` backed by `engine.mkdir()`
- **Test:** `echo "content" > /memories/.../file.md`, `mkdir /memories/projects`

### Step 5: Mutation operations

- Implement `unlink()` backed by `engine.rm()`
- Implement `rmdir()` backed by `engine.rm(recursive=true)`
- Implement `rename()` backed by `engine.mv()`
- **Test:** `rm /memories/.../file.md`, `mv /memories/.../file.md /memories/.../`

### Step 6: Polish and unmount

- Handle `SIGINT`/`SIGTERM` for clean unmount
- Add `memfs unmount /memories` command (calls `fusermount -u` or `umount`)
- Foreground vs background daemon mode
- Update integration tests

---

## Verification

1. **Mount and explore:** `memfs mount /tmp/test_memories` then `ls`, `cd`, `tree`
2. **Read:** `cat /tmp/test_memories/people/sister/memory_0042.md`
3. **Write:** `echo "test content" > /tmp/test_memories/people/sister/test.md`
4. **Append:** `echo "more" >> /tmp/test_memories/people/sister/test.md`
5. **Organize:** `mv`, `mkdir`, `rm` via real coreutils
6. **Agent re-test:** Spawn uninstructed Claude Code agent — it should succeed with standard commands, no `memfs` binary discovery needed
7. **Navigation invariant:** Both path orderings produce same `ls` output
8. **Unmount:** `memfs unmount /tmp/test_memories` or `Ctrl+C` in foreground mode

---

## Prerequisites

- **macOS:** Install macFUSE (`brew install macfuse` or from macfuse.io). Requires allowing the kernel extension in System Settings > Privacy & Security.
- **Linux:** Install `libfuse-dev` (`apt install libfuse-dev` or `dnf install fuse-devel`).
