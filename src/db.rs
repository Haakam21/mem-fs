use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use turso::Connection;

use crate::util;

/// Database wrapper supporting both local-only and cloud sync modes.
/// When `MEMFS_TURSO_URL` and `MEMFS_TURSO_TOKEN` are set, uses an embedded
/// replica that syncs with Turso Cloud. Otherwise, uses a local-only database.
pub enum Db {
    Local(turso::Database),
    Sync(turso::sync::Database),
}

impl Db {
    pub fn connect(&self) -> Result<Connection> {
        match self {
            Db::Local(db) => Ok(db.connect()?),
            Db::Sync(db) => {
                // sync::Database::connect is async but returns the same Connection type
                let rt = tokio::runtime::Handle::current();
                Ok(rt.block_on(db.connect())?)
            }
        }
    }

    /// Push local changes to cloud. No-op for local-only mode.
    pub async fn push(&self) {
        if let Db::Sync(db) = self {
            let _ = db.push().await;
        }
    }

    /// Pull cloud changes to local. No-op for local-only mode.
    pub async fn pull(&self) -> Result<bool> {
        match self {
            Db::Sync(db) => Ok(db.pull().await?),
            Db::Local(_) => Ok(false),
        }
    }
}

/// Open (or create) a Turso database. Uses cloud sync if MEMFS_TURSO_URL
/// and MEMFS_TURSO_TOKEN env vars are set, otherwise local-only.
pub async fn open(db_path: &str) -> Result<Db> {
    let path = util::expand_tilde(db_path);

    if let Some(parent) = Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let turso_url = std::env::var("MEMFS_TURSO_URL").ok();
    let turso_token = std::env::var("MEMFS_TURSO_TOKEN").ok();

    match (turso_url, turso_token) {
        (Some(url), Some(token)) => {
            let db = turso::sync::Builder::new_remote(&path)
                .with_remote_url(url)
                .with_auth_token(token)
                .bootstrap_if_empty(true)
                .build()
                .await?;
            Ok(Db::Sync(db))
        }
        _ => {
            let db = turso::Builder::new_local(&path).build().await?;
            Ok(Db::Local(db))
        }
    }
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
