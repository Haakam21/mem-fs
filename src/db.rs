use anyhow::Result;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use turso::Connection;

use crate::queries;
use crate::settings::Settings;
use crate::util;

/// Open (or create) a local Turso database. Always local-only — cloud sync
/// is handled separately by the `sync()` function to avoid runtime conflicts
/// with the FUSE event loop.
pub async fn open(db_path: &str) -> Result<turso::Database> {
    let path = util::expand_tilde(db_path);

    if let Some(parent) = Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let db = turso::Builder::new_local(&path).build().await?;
    Ok(db)
}

/// Sync local database with Turso Cloud using merge semantics. Memories with
/// unique filenames are auto-merged. Conflicts (same filename, different
/// content) are written to `~/.memfs/conflicts/` for the agent to reconcile.
/// The FUSE daemon must NOT be running (it holds the DB lock).
pub async fn sync(db_path: &str, settings: &Settings) -> Result<()> {
    let path = util::expand_tilde(db_path);

    let (url, token) = match (&settings.turso_url, &settings.turso_token) {
        (Some(url), Some(token)) => (url.clone(), token.clone()),
        _ => {
            eprintln!("No cloud credentials configured. Nothing to sync.");
            return Ok(());
        }
    };

    eprintln!("Syncing with {}...", url);

    // Read all local data into memory. The sync builder overwrites the DB
    // when setting up an embedded replica, and FUSE writes (via new_local)
    // don't create CDC entries, so we re-insert through the sync connection.
    eprintln!("  Reading local data...");
    let local_data = {
        let local_db = turso::Builder::new_local(&path).build().await?;
        let c: Connection = local_db.connect()?;
        read_all_data(&c).await?
    };

    // Remove local DB and sync metadata so new_remote starts from scratch.
    // This ensures the embedded replica after pull contains ONLY remote data,
    // not a merge of local + remote (which would defeat conflict detection).
    for suffix in &["", "-wal", "-shm", "-info", "-changes", "-wal-revert"] {
        let _ = std::fs::remove_file(format!("{}{}", path, suffix));
    }

    eprintln!("  Connecting...");
    let db = turso::sync::Builder::new_remote(&path)
        .with_remote_url(&url)
        .with_auth_token(&token)
        .bootstrap_if_empty(false)
        .build()
        .await?;

    eprintln!("  Pulling...");
    if let Err(e) = db.pull().await {
        eprintln!("  Pull failed ({}), pushing fresh data.", e);
    }

    let conn: Connection = db.connect().await?;
    create_tables(&conn).await?;
    let remote_data = read_all_data(&conn).await?;

    // Deduplicate by filename (keep most recently updated) and filter junk
    let local_memories = dedup_by_filename(
        local_data.memories.iter().filter(|m| !util::is_junk_file(&m.filename))
    );
    let remote_memories = dedup_by_filename(
        remote_data.memories.iter().filter(|m| !util::is_junk_file(&m.filename))
    );

    let mut local_by_filename: HashMap<&str, &MemoryRow> = HashMap::new();
    let mut local_ids: HashSet<i64> = HashSet::new();
    for m in &local_memories {
        local_by_filename.insert(m.filename.as_str(), m);
        local_ids.insert(m.id);
    }

    let mut local_tags_by_memory: HashMap<i64, Vec<&TagRow>> = HashMap::new();
    for t in &local_data.tags {
        if let Some(mid) = t.memory_id {
            local_tags_by_memory.entry(mid).or_default().push(t);
        }
    }

    let mut remote_tags_by_memory: HashMap<i64, Vec<&TagRow>> = HashMap::new();
    for t in &remote_data.tags {
        if let Some(mid) = t.memory_id {
            remote_tags_by_memory.entry(mid).or_default().push(t);
        }
    }

    let remote_embeddings_by_memory: HashMap<i64, &EmbeddingRow> = remote_data.embeddings.iter()
        .map(|e| (e.memory_id, e))
        .collect();

    // If we synced before, only flag conflicts for remote data modified after our
    // last push. This prevents our own push from echoing back as a conflict when
    // we update a memory locally and sync again.
    let last_push_file = format!("{}.last_push", path);
    let last_push = std::fs::read_to_string(&last_push_file).ok();

    let mut remote_only: Vec<&MemoryRow> = Vec::new();
    let mut conflicts: Vec<(&MemoryRow, &MemoryRow)> = Vec::new(); // (local, remote)

    for rm in &remote_memories {
        match local_by_filename.get(rm.filename.as_str()) {
            None => remote_only.push(rm),
            Some(lm) => {
                if lm.content != rm.content {
                    // Only flag as conflict if the remote was modified after our
                    // last push (by another agent). If it's unchanged since we
                    // pushed, our local version is an update.
                    let remote_changed = match &last_push {
                        Some(lp) => rm.updated_at.as_str() > lp.as_str(),
                        None => true, // first sync: all differences are conflicts
                    };
                    if remote_changed {
                        conflicts.push((lm, rm));
                    }
                }
            }
        }
    }
    let conflicts_dir = util::expand_tilde("~/.memfs/conflicts");
    if Path::new(&conflicts_dir).exists() {
        let _ = std::fs::remove_dir_all(&conflicts_dir);
    }
    if !conflicts.is_empty() {
        std::fs::create_dir_all(&conflicts_dir)?;
        for (_, remote_m) in &conflicts {
            // Use file_name() — DB stores absolute paths like /memories/note.md
            let leaf = Path::new(&remote_m.filename)
                .file_name()
                .unwrap_or(std::ffi::OsStr::new(&remote_m.filename));
            let conflict_path = Path::new(&conflicts_dir).join(leaf);
            let tags: Vec<String> = remote_tags_by_memory
                .get(&remote_m.id)
                .map(|tags| tags.iter().map(|t| format!("{}:{}", t.facet, t.value)).collect())
                .unwrap_or_default();
            let header = if tags.is_empty() {
                String::new()
            } else {
                format!("[Remote tags: {}]\n\n", tags.join(", "))
            };
            std::fs::write(&conflict_path, format!("{}{}", header, remote_m.content))?;
        }
    }

    let merged_count = local_memories.len() + remote_only.len();
    eprintln!("  Pushing {} memories ({} local, {} merged from remote)...",
        merged_count, local_memories.len(), remote_only.len());

    conn.execute("BEGIN", ()).await?;
    conn.execute("DELETE FROM embeddings", ()).await?;
    conn.execute("DELETE FROM tags", ()).await?;
    conn.execute("DELETE FROM memories", ()).await?;
    conn.execute("DELETE FROM facets", ()).await?;

    for m in &local_memories {
        conn.execute(
            "INSERT INTO memories (id, filename, content, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
            turso::params![m.id, m.filename.as_str(), m.content.as_str(), m.created_at.as_str(), m.updated_at.as_str()],
        ).await?;
    }
    for t in local_data.tags.iter().filter(|t| t.memory_id.map_or(false, |id| local_ids.contains(&id))) {
        conn.execute(
            "INSERT INTO tags (id, memory_id, facet, value) VALUES (?, ?, ?, ?)",
            turso::params![t.id, t.memory_id, t.facet.as_str(), t.value.as_str()],
        ).await?;
    }
    for e in local_data.embeddings.iter().filter(|e| local_ids.contains(&e.memory_id)) {
        conn.execute(
            "INSERT INTO embeddings (memory_id, embedding, model_version) VALUES (?, ?, ?)",
            turso::params![e.memory_id, e.embedding.as_slice(), e.model_version.as_str()],
        ).await?;
    }

    // Remote-only memories get remapped IDs to avoid collisions with local
    if !remote_only.is_empty() {
        let mut next_memory_id = local_memories.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        let mut next_tag_id = local_data.tags.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        for rm in &remote_only {
            let new_id = next_memory_id;
            next_memory_id += 1;
            conn.execute(
                "INSERT INTO memories (id, filename, content, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
                turso::params![new_id, rm.filename.as_str(), rm.content.as_str(), rm.created_at.as_str(), rm.updated_at.as_str()],
            ).await?;
            if let Some(tags) = remote_tags_by_memory.get(&rm.id) {
                for t in tags {
                    let new_tag_id = next_tag_id;
                    next_tag_id += 1;
                    conn.execute(
                        "INSERT INTO tags (id, memory_id, facet, value) VALUES (?, ?, ?, ?)",
                        turso::params![new_tag_id, Some(new_id), t.facet.as_str(), t.value.as_str()],
                    ).await?;
                }
            }
            if let Some(e) = remote_embeddings_by_memory.get(&rm.id) {
                conn.execute(
                    "INSERT INTO embeddings (memory_id, embedding, model_version) VALUES (?, ?, ?)",
                    turso::params![new_id, e.embedding.as_slice(), e.model_version.as_str()],
                ).await?;
            }
        }
    }

    let mut all_facets: HashSet<&str> = HashSet::new();
    for f in &local_data.facets { all_facets.insert(f.as_str()); }
    for f in &remote_data.facets { all_facets.insert(f.as_str()); }
    for f in &all_facets {
        conn.execute(
            "INSERT INTO facets (name) VALUES (?)",
            turso::params![*f],
        ).await?;
    }

    conn.execute("COMMIT", ()).await?;

    db.push().await?;

    // Record push time so the next sync can distinguish "our own data echoing
    // back" from "data modified by another agent"
    let _ = std::fs::write(&last_push_file, Utc::now().to_rfc3339());

    if conflicts.is_empty() {
        eprintln!("Synced.");
    } else {
        eprintln!();
        eprintln!("Synced with {} conflict(s). Remote versions saved to:", conflicts.len());
        eprintln!("  {}/", conflicts_dir);
        eprintln!();
        for (local_m, remote_m) in &conflicts {
            let local_tags: Vec<String> = local_tags_by_memory
                .get(&local_m.id)
                .map(|tags| tags.iter().map(|t| format!("{}:{}", t.facet, t.value)).collect())
                .unwrap_or_default();
            let remote_tags: Vec<String> = remote_tags_by_memory
                .get(&remote_m.id)
                .map(|tags| tags.iter().map(|t| format!("{}:{}", t.facet, t.value)).collect())
                .unwrap_or_default();
            eprintln!("  {} (local tags: [{}], remote tags: [{}])",
                remote_m.filename,
                local_tags.join(", "),
                remote_tags.join(", "));
        }
        eprintln!();
        eprintln!("Reconcile conflicts, then run `memfs sync` again.");
    }

    Ok(())
}

struct DbSnapshot {
    memories: Vec<MemoryRow>,
    tags: Vec<TagRow>,
    facets: Vec<String>,
    embeddings: Vec<EmbeddingRow>,
}

struct MemoryRow {
    id: i64,
    filename: String,
    content: String,
    created_at: String,
    updated_at: String,
}

struct TagRow {
    id: i64,
    memory_id: Option<i64>,
    facet: String,
    value: String,
}

struct EmbeddingRow {
    memory_id: i64,
    embedding: Vec<u8>,
    model_version: String,
}

/// Deduplicate memories by filename, keeping the most recently updated version.
/// Multiple writes to the same filename create duplicate rows — resolve them here.
fn dedup_by_filename<'a>(memories: impl Iterator<Item = &'a MemoryRow>) -> Vec<&'a MemoryRow> {
    let mut by_filename: HashMap<&str, &MemoryRow> = HashMap::new();
    for m in memories {
        by_filename
            .entry(m.filename.as_str())
            .and_modify(|existing| {
                if m.updated_at > existing.updated_at {
                    *existing = m;
                }
            })
            .or_insert(m);
    }
    by_filename.into_values().collect()
}

async fn read_all_data(conn: &Connection) -> Result<DbSnapshot> {
    let mut data = DbSnapshot {
        memories: Vec::new(),
        tags: Vec::new(),
        facets: Vec::new(),
        embeddings: Vec::new(),
    };

    // Tables may not exist if DB was wiped by a previous sync attempt
    if let Ok(mut rows) = conn.query("SELECT id, filename, content, created_at, updated_at FROM memories", ()).await {
        while let Some(row) = rows.next().await? {
            data.memories.push(MemoryRow {
                id: row.get::<i64>(0)?,
                filename: row.get::<String>(1)?,
                content: row.get::<String>(2)?,
                created_at: row.get::<String>(3)?,
                updated_at: row.get::<String>(4)?,
            });
        }
    }

    if let Ok(mut rows) = conn.query("SELECT id, memory_id, facet, value FROM tags", ()).await {
        while let Some(row) = rows.next().await? {
            data.tags.push(TagRow {
                id: row.get::<i64>(0)?,
                memory_id: row.get::<Option<i64>>(1).unwrap_or(None),
                facet: row.get::<String>(2)?,
                value: row.get::<String>(3)?,
            });
        }
    }

    data.facets = queries::list_facets(conn).await.unwrap_or_default();

    if let Ok(mut rows) = conn.query("SELECT memory_id, embedding, model_version FROM embeddings", ()).await {
        while let Some(row) = rows.next().await? {
            data.embeddings.push(EmbeddingRow {
                memory_id: row.get::<i64>(0)?,
                embedding: row.get::<Vec<u8>>(1)?,
                model_version: row.get::<String>(2)?,
            });
        }
    }

    Ok(data)
}

/// Create tables and indexes if they don't exist. Safe to call on any connection.
async fn create_tables(conn: &Connection) -> Result<()> {
    let _ = conn.query("PRAGMA journal_mode=WAL", ()).await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            filename TEXT NOT NULL,
            content TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_id INTEGER,
            facet TEXT NOT NULL,
            value TEXT NOT NULL,
            FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
        )",
        (),
    )
    .await?;

    conn.execute("CREATE INDEX IF NOT EXISTS idx_tags_facet_value ON tags (facet, value)", ()).await?;
    conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_unique ON tags (memory_id, facet, value)", ()).await?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_tags_memory_id ON tags (memory_id)", ()).await?;
    conn.execute("CREATE TABLE IF NOT EXISTS facets (name TEXT PRIMARY KEY)", ()).await?;
    conn.execute("CREATE INDEX IF NOT EXISTS idx_memories_filename ON memories (filename)", ()).await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS embeddings (
            memory_id INTEGER PRIMARY KEY,
            embedding BLOB NOT NULL,
            model_version TEXT NOT NULL DEFAULT 'all-MiniLM-L6-v2',
            FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
        )",
        (),
    )
    .await?;

    Ok(())
}

/// Run schema migrations — create tables, indexes, and migrate old schemas.
/// Only call on local connections (destructive DDL would corrupt CDC on sync).
pub async fn migrate(conn: &Connection) -> Result<()> {
    create_tables(conn).await?;

    // Migration: allow NULL memory_id for placeholder tags (previously used 0).
    let mut rows = conn.query(
        "SELECT \"notnull\" FROM pragma_table_info('tags') WHERE name = 'memory_id'", ()
    ).await?;
    if let Some(row) = rows.next().await? {
        let notnull: i64 = row.get_value(0)?.as_integer().copied().unwrap_or(0);
        if notnull == 1 {
            conn.execute("CREATE TABLE tags_new (id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER, facet TEXT NOT NULL, value TEXT NOT NULL, FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE)", ()).await?;
            conn.execute("INSERT INTO tags_new SELECT * FROM tags", ()).await?;
            conn.execute("DROP TABLE tags", ()).await?;
            conn.execute("ALTER TABLE tags_new RENAME TO tags", ()).await?;
            conn.execute("CREATE INDEX IF NOT EXISTS idx_tags_facet_value ON tags (facet, value)", ()).await?;
            conn.execute("CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_unique ON tags (memory_id, facet, value)", ()).await?;
            conn.execute("CREATE INDEX IF NOT EXISTS idx_tags_memory_id ON tags (memory_id)", ()).await?;
            conn.execute("UPDATE tags SET memory_id = NULL WHERE memory_id = 0", ()).await?;
            conn.execute("DELETE FROM memories WHERE id = 0", ()).await?;
        }
    }

    // Clean up junk files from older versions (keep in sync with util::is_junk_file)
    conn.execute(
        "DELETE FROM memories WHERE filename LIKE '.\\_%' ESCAPE '\\' OR filename LIKE '.#%' OR filename LIKE '%.tmp.%' OR filename LIKE '%~'",
        (),
    ).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(id: i64, filename: &str, content: &str, updated_at: &str) -> MemoryRow {
        MemoryRow {
            id,
            filename: filename.to_string(),
            content: content.to_string(),
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn dedup_no_duplicates() {
        let memories = vec![
            mem(1, "a.md", "aaa", "2025-01-01T00:00:00Z"),
            mem(2, "b.md", "bbb", "2025-01-02T00:00:00Z"),
        ];
        let result = dedup_by_filename(memories.iter());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn dedup_keeps_newest() {
        let memories = vec![
            mem(1, "note.md", "old version", "2025-01-01T00:00:00Z"),
            mem(2, "note.md", "new version", "2025-01-02T00:00:00Z"),
        ];
        let result = dedup_by_filename(memories.iter());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "new version");
        assert_eq!(result[0].id, 2);
    }

    #[test]
    fn dedup_keeps_newest_regardless_of_order() {
        let memories = vec![
            mem(2, "note.md", "new version", "2025-01-02T00:00:00Z"),
            mem(1, "note.md", "old version", "2025-01-01T00:00:00Z"),
        ];
        let result = dedup_by_filename(memories.iter());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "new version");
    }

    #[test]
    fn dedup_multiple_filenames_with_duplicates() {
        let memories = vec![
            mem(1, "a.md", "a-old", "2025-01-01T00:00:00Z"),
            mem(2, "b.md", "b-old", "2025-01-01T00:00:00Z"),
            mem(3, "a.md", "a-new", "2025-01-02T00:00:00Z"),
            mem(4, "b.md", "b-new", "2025-01-03T00:00:00Z"),
            mem(5, "c.md", "c-only", "2025-01-01T00:00:00Z"),
        ];
        let result = dedup_by_filename(memories.iter());
        assert_eq!(result.len(), 3);
        let by_name: HashMap<&str, &MemoryRow> = result.iter()
            .map(|m| (m.filename.as_str(), *m))
            .collect();
        assert_eq!(by_name["a.md"].content, "a-new");
        assert_eq!(by_name["b.md"].content, "b-new");
        assert_eq!(by_name["c.md"].content, "c-only");
    }

    #[test]
    fn dedup_empty_input() {
        let memories: Vec<MemoryRow> = vec![];
        let result = dedup_by_filename(memories.iter());
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn dedup_single_entry() {
        let memories = vec![mem(1, "only.md", "content", "2025-01-01T00:00:00Z")];
        let result = dedup_by_filename(memories.iter());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].filename, "only.md");
    }

    #[test]
    fn dedup_three_versions_same_file() {
        let memories = vec![
            mem(1, "note.md", "v1", "2025-01-01T00:00:00Z"),
            mem(2, "note.md", "v2", "2025-01-02T00:00:00Z"),
            mem(3, "note.md", "v3", "2025-01-03T00:00:00Z"),
        ];
        let result = dedup_by_filename(memories.iter());
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "v3");
    }

    #[test]
    fn conflict_file_uses_leaf_name() {
        // DB stores absolute paths like /memories/note.md
        // Conflict files should use just the leaf name
        let filename = "/memories/topics/work/note.md";
        let leaf = std::path::Path::new(filename)
            .file_name()
            .unwrap_or(std::ffi::OsStr::new(filename));
        assert_eq!(leaf, "note.md");
    }

    #[test]
    fn conflict_file_leaf_simple_name() {
        let filename = "note.md";
        let leaf = std::path::Path::new(filename)
            .file_name()
            .unwrap_or(std::ffi::OsStr::new(filename));
        assert_eq!(leaf, "note.md");
    }
}
