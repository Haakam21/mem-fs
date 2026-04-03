use anyhow::Result;
use std::path::Path;
use turso::Connection;

use crate::util;

/// Database wrapper supporting both local-only and cloud sync modes.
pub enum Db {
    Local(turso::Database),
    Sync(turso::sync::Database),
}

impl Db {
    pub async fn connect(&self) -> Result<Connection> {
        match self {
            Db::Local(db) => Ok(db.connect()?),
            Db::Sync(db) => Ok(db.connect().await?),
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

/// Read Turso credentials from .memfs/settings.json (next to the DB file).
/// Returns (url, token) if both are present, otherwise (None, None).
fn turso_config(db_path: &str) -> (Option<String>, Option<String>) {
    let config_path = match Path::new(db_path).parent() {
        Some(p) => p.join("settings.json"),
        None => return (None, None),
    };

    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    // Minimal JSON parsing — look for "turso_url" and "turso_token" string values
    let url = extract_json_string(&content, "turso_url");
    let token = extract_json_string(&content, "turso_token");
    (url, token)
}

/// Extract a string value from a JSON object by key (simple, no full parser needed).
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let idx = json.find(&pattern)?;
    let after_key = &json[idx + pattern.len()..];
    // Skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();
    // Extract quoted string value
    let after_quote = after_colon.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    Some(after_quote[..end].to_string())
}

/// Open (or create) a Turso database. Uses cloud sync if .memfs/settings.json
/// contains turso_url and turso_token, otherwise local-only.
pub async fn open(db_path: &str) -> Result<Db> {
    let path = util::expand_tilde(db_path);

    if let Some(parent) = Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let (turso_url, turso_token) = turso_config(&path);

    match (turso_url, turso_token) {
        (Some(url), Some(token)) => {
            let db = turso::sync::Builder::new_remote(&path)
                .with_remote_url(url)
                .with_auth_token(token)
                .bootstrap_if_empty(true)
                .build()
                .await?;
            // Pull latest from cloud on open
            let _ = db.pull().await;
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
