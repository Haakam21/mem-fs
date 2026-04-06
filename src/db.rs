use anyhow::Result;
use std::path::Path;
use turso::Connection;

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

    let db = turso::sync::Builder::new_remote(&path)
        .with_remote_url(&url)
        .with_auth_token(&token)
        .bootstrap_if_empty(true)
        .build()
        .await?;

    let pulled = db.pull().await?;
    db.push().await?;

    if pulled {
        eprintln!("Synced from cloud.");
    } else {
        eprintln!("Already up to date.");
    }

    Ok(())
}

/// Run schema migrations — create tables and indexes if they don't exist.
pub async fn migrate(conn: &Connection) -> Result<()> {
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
            memory_id INTEGER NOT NULL,
            facet TEXT NOT NULL,
            value TEXT NOT NULL,
            FOREIGN KEY (memory_id) REFERENCES memories(id) ON DELETE CASCADE
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_tags_facet_value ON tags (facet, value)",
        (),
    )
    .await?;

    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_tags_unique ON tags (memory_id, facet, value)",
        (),
    )
    .await?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_tags_memory_id ON tags (memory_id)",
        (),
    )
    .await?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS facets (
            name TEXT PRIMARY KEY
        )",
        (),
    )
    .await?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_memories_filename ON memories (filename)",
        (),
    )
    .await?;

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
