use anyhow::Result;
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

/// Sync local database with Turso Cloud. Opens a temporary sync connection,
/// pulls remote changes, pushes local changes, then closes. The FUSE daemon
/// must NOT be running (it holds the DB lock).
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

    // Remove stale sync metadata so the sync builder starts fresh
    for suffix in &["-info", "-changes", "-wal-revert"] {
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

    // Re-insert all data through the sync connection so the CDC engine
    // tracks every row for push. Use create_tables (not migrate) to avoid
    // running destructive DDL through the CDC connection.
    let conn: Connection = db.connect().await?;
    create_tables(&conn).await?;

    // Filter out junk files (._*, .#*, .tmp.*, *~) before syncing
    let memories: Vec<&MemoryRow> = local_data.memories.iter()
        .filter(|m| !util::is_junk_file(&m.filename))
        .collect();
    eprintln!("  Pushing {} memories...", memories.len());
    conn.execute("BEGIN", ()).await?;
    // Clear remote data before full re-insert (delete children first for FK)
    conn.execute("DELETE FROM embeddings", ()).await?;
    conn.execute("DELETE FROM tags", ()).await?;
    conn.execute("DELETE FROM memories", ()).await?;
    conn.execute("DELETE FROM facets", ()).await?;
    for m in &memories {
        conn.execute(
            "INSERT INTO memories (id, filename, content, created_at, updated_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET filename=excluded.filename, content=excluded.content, \
             created_at=excluded.created_at, updated_at=excluded.updated_at",
            turso::params![m.id, m.filename.as_str(), m.content.as_str(), m.created_at.as_str(), m.updated_at.as_str()],
        ).await?;
    }
    // Skip placeholder tags and tags referencing junk memories
    let synced_ids: std::collections::HashSet<i64> = memories.iter().map(|m| m.id).collect();
    for t in local_data.tags.iter().filter(|t| t.memory_id.map_or(false, |id| synced_ids.contains(&id))) {
        conn.execute(
            "INSERT INTO tags (id, memory_id, facet, value) VALUES (?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET memory_id=excluded.memory_id, facet=excluded.facet, value=excluded.value",
            turso::params![t.id, t.memory_id, t.facet.as_str(), t.value.as_str()],
        ).await?;
    }
    for f in &local_data.facets {
        conn.execute(
            "INSERT INTO facets (name) VALUES (?) ON CONFLICT(name) DO NOTHING",
            turso::params![f.as_str()],
        ).await?;
    }
    for e in local_data.embeddings.iter().filter(|e| synced_ids.contains(&e.memory_id)) {
        conn.execute(
            "INSERT INTO embeddings (memory_id, embedding, model_version) VALUES (?, ?, ?) \
             ON CONFLICT(memory_id) DO UPDATE SET embedding=excluded.embedding, model_version=excluded.model_version",
            turso::params![e.memory_id, e.embedding.as_slice(), e.model_version.as_str()],
        ).await?;
    }
    conn.execute("COMMIT", ()).await?;

    db.push().await?;
    eprintln!("Synced.");

    Ok(())
}

struct LocalData {
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

async fn read_all_data(conn: &Connection) -> Result<LocalData> {
    let mut data = LocalData {
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

    // Clean up junk files (._*, .#*, .tmp.*, *~) from older versions
    conn.execute(
        "DELETE FROM memories WHERE filename LIKE '.\\_%' ESCAPE '\\' OR filename LIKE '.#%' OR filename LIKE '%.tmp.%' OR filename LIKE '%~'",
        (),
    ).await?;

    Ok(())
}
