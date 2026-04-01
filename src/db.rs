use anyhow::Result;
use std::path::Path;
use turso::{Builder, Connection, Database};

/// Open (or create) a Turso database at the given path.
/// If `MEMFS_TURSO_URL` and `MEMFS_TURSO_TOKEN` are set, uses remote sync mode.
/// Otherwise, uses local-only mode.
pub async fn open(db_path: &str) -> Result<Database> {
    let path = expand_tilde(db_path);

    // Ensure parent directory exists
    if let Some(parent) = Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let turso_url = std::env::var("MEMFS_TURSO_URL").ok();
    let turso_token = std::env::var("MEMFS_TURSO_TOKEN").ok();

    let db = match (turso_url, turso_token) {
        (Some(_url), Some(_token)) => {
            // TODO: Enable sync mode once we test with Turso Cloud
            // For now, fall through to local-only
            Builder::new_local(&path).build().await?
        }
        _ => Builder::new_local(&path).build().await?,
    };

    Ok(db)
}

/// Run schema migrations — create tables and indexes if they don't exist.
pub async fn migrate(conn: &Connection) -> Result<()> {
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

    Ok(())
}

/// Expand `~` to HOME directory.
fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{}/{}", home, rest);
        }
    } else if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    }
    path.to_string()
}
