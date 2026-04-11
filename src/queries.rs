use anyhow::{bail, Result};
use chrono::Utc;
use turso::Connection;

use crate::path::Filter;

/// A memory record from the database.
#[derive(Debug, Clone)]
pub struct Memory {
    pub id: i64,
    pub filename: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
    pub tags: Vec<Filter>,
}

/// A single grep match result.
#[derive(Debug)]
pub struct GrepResult {
    pub filename: String,
    pub line_number: usize,
    pub line: String,
}

// --- Facet operations ---

/// List all known facet categories.
pub async fn list_facets(conn: &Connection) -> Result<Vec<String>> {
    let mut rows = conn.query("SELECT name FROM facets ORDER BY name", ()).await?;
    let mut facets = Vec::new();
    while let Some(row) = rows.next().await? {
        let name: String = row.get_value(0)?.as_text().cloned().unwrap_or_default();
        facets.push(name);
    }
    Ok(facets)
}

/// Check if a facet category exists.
pub async fn facet_exists(conn: &Connection, name: &str) -> Result<bool> {
    let mut rows = conn
        .query("SELECT 1 FROM facets WHERE name = ?1", [name])
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Create a facet category (idempotent).
pub async fn create_facet(conn: &Connection, name: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO facets (name) VALUES (?1)",
        [name],
    )
    .await?;
    Ok(())
}

/// Delete a facet category.
pub async fn delete_facet(conn: &Connection, name: &str) -> Result<()> {
    conn.execute("DELETE FROM facets WHERE name = ?1", [name])
        .await?;
    Ok(())
}

// --- Value operations ---

/// Check if a specific facet:value combination exists (either via tags or placeholders).
pub async fn value_exists(conn: &Connection, facet: &str, value: &str) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM tags WHERE facet = ?1 AND value = ?2 LIMIT 1",
            [facet, value],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Ensure a facet:value exists by inserting a placeholder tag if needed.
/// Placeholder tags use memory_id = NULL.
pub async fn ensure_value(conn: &Connection, facet: &str, value: &str) -> Result<()> {
    if value_exists(conn, facet, value).await? {
        return Ok(());
    }
    create_facet(conn, facet).await?;
    conn.execute(
        "INSERT INTO tags (memory_id, facet, value) VALUES (NULL, ?1, ?2)",
        [facet, value],
    )
    .await?;
    Ok(())
}

/// List all values for a facet that have at least one matching memory given current filters.
pub async fn list_values(
    conn: &Connection,
    facet: &str,
    filters: &[Filter],
) -> Result<Vec<String>> {
    if filters.is_empty() {
        let mut rows = conn
            .query(
                "SELECT DISTINCT value FROM tags WHERE facet = ?1 ORDER BY value",
                [facet],
            )
            .await?;
        let mut values = Vec::new();
        while let Some(row) = rows.next().await? {
            let val: String = row.get_value(0)?.as_text().cloned().unwrap_or_default();
            values.push(val);
        }
        return Ok(values);
    }

    // Two-step: get matching memory IDs, then find values for this facet on those memories
    let memory_ids = get_matching_memory_ids(conn, filters).await?;
    if memory_ids.is_empty() {
        return Ok(vec![]);
    }

    let (id_placeholders, mut id_params) = build_id_in_clause(&memory_ids, 0);
    let facet_param_idx = memory_ids.len() + 1;
    let sql = format!(
        "SELECT DISTINCT value FROM tags WHERE facet = ?{} AND memory_id IN ({}) ORDER BY value",
        facet_param_idx, id_placeholders
    );
    id_params.push(turso::Value::from(facet));

    let mut rows = conn.query(&sql, id_params).await?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().await? {
        let val: String = row.get_value(0)?.as_text().cloned().unwrap_or_default();
        values.push(val);
    }
    Ok(values)
}

// --- Memory operations ---

/// List memories matching all given filters.
pub async fn list_memories(conn: &Connection, filters: &[Filter]) -> Result<Vec<Memory>> {
    let memories = if filters.is_empty() {
        let mut rows = conn
            .query(
                "SELECT id, filename, content, created_at, updated_at FROM memories ORDER BY filename",
                (),
            )
            .await?;
        let mut mems = Vec::new();
        while let Some(row) = rows.next().await? {
            mems.push(row_to_memory(&row)?);
        }
        mems
    } else {
        let memory_ids = get_matching_memory_ids(conn, filters).await?;
        if memory_ids.is_empty() {
            return Ok(vec![]);
        }
        let (id_placeholders, id_params) = build_id_in_clause(&memory_ids, 0);
        let sql = format!(
            "SELECT id, filename, content, created_at, updated_at FROM memories WHERE id IN ({}) ORDER BY filename",
            id_placeholders
        );
        let mut rows = conn.query(&sql, id_params).await?;
        let mut mems = Vec::new();
        while let Some(row) = rows.next().await? {
            mems.push(row_to_memory(&row)?);
        }
        mems
    };

    let ids: Vec<i64> = memories.iter().map(|m| m.id).collect();
    let mut tags_map = get_tags_batch(conn, &ids).await?;
    let mut result = Vec::new();
    for mut mem in memories {
        mem.tags = tags_map.remove(&mem.id).unwrap_or_default();
        result.push(mem);
    }
    Ok(result)
}

/// Get a single memory by filename within the given filter scope.
pub async fn get_memory(
    conn: &Connection,
    filename: &str,
    filters: &[Filter],
) -> Result<Option<Memory>> {
    let mem = if filters.is_empty() {
        let mut rows = conn
            .query(
                "SELECT id, filename, content, created_at, updated_at FROM memories WHERE filename = ?1",
                [filename],
            )
            .await?;
        match rows.next().await? {
            Some(row) => Some(row_to_memory(&row)?),
            None => None,
        }
    } else {
        let memory_ids = get_matching_memory_ids(conn, filters).await?;
        if memory_ids.is_empty() {
            return Ok(None);
        }
        let (id_placeholders, mut id_params) = build_id_in_clause(&memory_ids, 0);
        let fname_idx = memory_ids.len() + 1;
        let sql = format!(
            "SELECT id, filename, content, created_at, updated_at FROM memories WHERE filename = ?{} AND id IN ({})",
            fname_idx, id_placeholders
        );
        id_params.push(turso::Value::from(filename));
        let mut rows = conn.query(&sql, id_params).await?;
        match rows.next().await? {
            Some(row) => Some(row_to_memory(&row)?),
            None => None,
        }
    };

    match mem {
        Some(mut m) => {
            m.tags = get_tags(conn, m.id).await?;
            Ok(Some(m))
        }
        None => Ok(None),
    }
}

/// Create a new memory with content and tags.
pub async fn create_memory(
    conn: &Connection,
    filename: &str,
    content: &str,
    tags: &[Filter],
) -> Result<i64> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO memories (filename, content, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
        [filename, content, &now, &now],
    )
    .await?;

    // Get the last inserted id
    let mut rows = conn.query("SELECT last_insert_rowid()", ()).await?;
    let id = match rows.next().await? {
        Some(row) => row.get_value(0)?.as_integer().copied().unwrap_or(0),
        None => bail!("failed to get last insert rowid"),
    };

    // Insert tags
    for tag in tags {
        conn.execute(
            "INSERT INTO tags (memory_id, facet, value) VALUES (?1, ?2, ?3)",
            turso::params![id, tag.facet.as_str(), tag.value.as_str()],
        )
        .await?;
        // Ensure facet exists
        create_facet(conn, &tag.facet).await?;
    }

    Ok(id)
}

/// Append content to an existing memory.
pub async fn append_memory(
    conn: &Connection,
    filename: &str,
    content: &str,
    filters: &[Filter],
) -> Result<()> {
    let mem = get_memory(conn, filename, filters).await?;
    match mem {
        Some(m) => {
            let now = Utc::now().to_rfc3339();
            let new_content = format!("{}\n{}", m.content, content);
            conn.execute(
                "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3",
                turso::params![new_content.as_str(), now.as_str(), m.id],
            )
            .await?;
            Ok(())
        }
        None => bail!("memfs: no such memory: '{}'", filename),
    }
}

/// Delete a memory, its tags, and its embedding.
pub async fn delete_memory(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM embeddings WHERE memory_id = ?1", turso::params![id])
        .await?;
    conn.execute("DELETE FROM tags WHERE memory_id = ?1", turso::params![id])
        .await?;
    conn.execute("DELETE FROM memories WHERE id = ?1", turso::params![id])
        .await?;
    Ok(())
}

#[cfg(feature = "search")]
pub async fn delete_embedding(conn: &Connection, memory_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM embeddings WHERE memory_id = ?1",
        turso::params![memory_id],
    )
    .await?;
    Ok(())
}

#[cfg(feature = "search")]
pub async fn upsert_embedding(
    conn: &Connection,
    memory_id: i64,
    embedding: &[u8],
    model_version: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO embeddings (memory_id, embedding, model_version) VALUES (?1, ?2, ?3) \
         ON CONFLICT(memory_id) DO UPDATE SET embedding=excluded.embedding, model_version=excluded.model_version",
        turso::params![memory_id, embedding, model_version],
    )
    .await?;
    Ok(())
}

#[cfg(feature = "search")]
#[derive(Debug)]
pub struct SearchResult {
    pub filename: String,
    pub score: f32,
    pub content: String,
}

#[cfg(feature = "search")]
pub async fn list_memory_embeddings(
    conn: &Connection,
    filters: &[Filter],
) -> Result<Vec<(i64, String, String, Vec<u8>)>> {
    if filters.is_empty() {
        let mut rows = conn
            .query(
                "SELECT m.id, m.filename, m.content, e.embedding \
                 FROM memories m JOIN embeddings e ON e.memory_id = m.id \
                 ORDER BY m.filename",
                (),
            )
            .await?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().await? {
            results.push((
                row.get_value(0)?.as_integer().copied().unwrap_or(0),
                row.get_value(1)?.as_text().cloned().unwrap_or_default(),
                row.get_value(2)?.as_text().cloned().unwrap_or_default(),
                row.get_value(3)?.as_blob().cloned().unwrap_or_default(),
            ));
        }
        return Ok(results);
    }

    let memory_ids = get_matching_memory_ids(conn, filters).await?;
    if memory_ids.is_empty() {
        return Ok(vec![]);
    }
    let (id_placeholders, id_params) = build_id_in_clause(&memory_ids, 0);
    let sql = format!(
        "SELECT m.id, m.filename, m.content, e.embedding \
         FROM memories m JOIN embeddings e ON e.memory_id = m.id \
         WHERE m.id IN ({}) ORDER BY m.filename",
        id_placeholders
    );
    let mut rows = conn.query(&sql, id_params).await?;
    let mut results = Vec::new();
    while let Some(row) = rows.next().await? {
        results.push((
            row.get_value(0)?.as_integer().copied().unwrap_or(0),
            row.get_value(1)?.as_text().cloned().unwrap_or_default(),
            row.get_value(2)?.as_text().cloned().unwrap_or_default(),
            row.get_value(3)?.as_blob().cloned().unwrap_or_default(),
        ));
    }
    Ok(results)
}

/// Remove a specific tag from a memory.
pub async fn remove_tag(conn: &Connection, memory_id: i64, facet: &str, value: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM tags WHERE memory_id = ?1 AND facet = ?2 AND value = ?3",
        turso::params![memory_id, facet, value],
    )
    .await?;
    Ok(())
}

/// Add a tag to a memory (idempotent).
pub async fn add_tag(conn: &Connection, memory_id: i64, facet: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO tags (memory_id, facet, value) VALUES (?1, ?2, ?3)",
        turso::params![memory_id, facet, value],
    )
    .await?;
    create_facet(conn, facet).await?;
    Ok(())
}

/// Remove all tags with a given facet:value (for `rm -r /memories/facet/value`).
/// Does NOT delete the memories themselves.
pub async fn untag_all(conn: &Connection, facet: &str, value: &str) -> Result<u64> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM tags WHERE facet = ?1 AND value = ?2",
            [facet, value],
        )
        .await?;
    let count = match rows.next().await? {
        Some(row) => row.get_value(0)?.as_integer().copied().unwrap_or(0) as u64,
        None => 0,
    };
    conn.execute(
        "DELETE FROM tags WHERE facet = ?1 AND value = ?2",
        [facet, value],
    )
    .await?;
    Ok(count)
}

/// Get facet categories that can still narrow results given current filters.
/// Excludes facets already filtered on.
pub async fn remaining_facets(conn: &Connection, filters: &[Filter]) -> Result<Vec<String>> {
    let excluded: std::collections::HashSet<String> =
        filters.iter().map(|f| f.facet.clone()).collect();

    if filters.is_empty() {
        return list_facets(conn).await;
    }

    // Step 1: get matching memory IDs
    let memory_ids = get_matching_memory_ids(conn, filters).await?;
    if memory_ids.is_empty() {
        return Ok(vec![]);
    }

    // Step 2: get distinct facets from those memories, excluding already-filtered facets
    let (id_placeholders, turso_params) = build_id_in_clause(&memory_ids, 0);
    let sql = format!(
        "SELECT DISTINCT facet FROM tags WHERE memory_id IN ({}) ORDER BY facet",
        id_placeholders
    );

    let mut rows = conn.query(&sql, turso_params).await?;
    let mut facets = Vec::new();
    while let Some(row) = rows.next().await? {
        let name: String = row.get_value(0)?.as_text().cloned().unwrap_or_default();
        if !excluded.contains(&name) {
            facets.push(name);
        }
    }
    Ok(facets)
}

/// Get a single memory by its database ID (used by FUSE read path).
pub async fn get_memory_by_id(conn: &Connection, id: i64) -> Result<Option<Memory>> {
    let mut rows = conn
        .query(
            "SELECT id, filename, content, created_at, updated_at FROM memories WHERE id = ?1",
            turso::params![id],
        )
        .await?;
    match rows.next().await? {
        Some(row) => {
            let mut m = row_to_memory(&row)?;
            m.tags = get_tags(conn, m.id).await?;
            Ok(Some(m))
        }
        None => Ok(None),
    }
}

/// Update a memory's content by ID (used by FUSE write flush).
pub async fn update_memory_content(conn: &Connection, id: i64, content: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE memories SET content = ?1, updated_at = ?2 WHERE id = ?3",
        turso::params![content, now.as_str(), id],
    )
    .await?;
    Ok(())
}

/// Rename a memory's filename by ID (used by FUSE rename).
pub async fn rename_memory(conn: &Connection, id: i64, new_filename: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE memories SET filename = ?1, updated_at = ?2 WHERE id = ?3",
        turso::params![new_filename, now.as_str(), id],
    )
    .await?;
    Ok(())
}

/// Get a memory by filename that has any tag under the given facet,
/// scoped by existing filters. Used by FUSE lookup at facet-level.
pub async fn get_memory_by_facet(
    conn: &Connection,
    filename: &str,
    facet: &str,
    filters: &[Filter],
) -> Result<Option<Memory>> {
    if filters.is_empty() {
        let mut rows = conn
            .query(
                "SELECT DISTINCT m.id, m.filename, m.content, m.created_at, m.updated_at \
                 FROM memories m JOIN tags t ON t.memory_id = m.id \
                 WHERE m.filename = ?1 AND t.facet = ?2 LIMIT 1",
                [filename, facet],
            )
            .await?;
        match rows.next().await? {
            Some(row) => {
                let mut m = row_to_memory(&row)?;
                m.tags = get_tags(conn, m.id).await?;
                Ok(Some(m))
            }
            None => Ok(None),
        }
    } else {
        let memory_ids = get_matching_memory_ids(conn, filters).await?;
        if memory_ids.is_empty() {
            return Ok(None);
        }
        let (id_placeholders, mut id_params) = build_id_in_clause(&memory_ids, 0);
        let fname_idx = memory_ids.len() + 1;
        let facet_idx = memory_ids.len() + 2;
        let sql = format!(
            "SELECT DISTINCT m.id, m.filename, m.content, m.created_at, m.updated_at \
             FROM memories m JOIN tags t ON t.memory_id = m.id \
             WHERE m.id IN ({}) AND m.filename = ?{} AND t.facet = ?{} LIMIT 1",
            id_placeholders, fname_idx, facet_idx
        );
        id_params.push(turso::Value::from(filename));
        id_params.push(turso::Value::from(facet));
        let mut rows = conn.query(&sql, id_params).await?;
        match rows.next().await? {
            Some(row) => {
                let mut m = row_to_memory(&row)?;
                m.tags = get_tags(conn, m.id).await?;
                Ok(Some(m))
            }
            None => Ok(None),
        }
    }
}

/// List memory stubs that have any tag under the given facet,
/// scoped by existing filters. Used by FUSE readdir at facet-level.
pub async fn list_memory_stubs_by_facet(
    conn: &Connection,
    facet: &str,
    filters: &[Filter],
) -> Result<Vec<MemoryStub>> {
    if filters.is_empty() {
        let mut rows = conn
            .query(
                "SELECT DISTINCT m.id, m.filename FROM memories m \
                 JOIN tags t ON t.memory_id = m.id \
                 WHERE t.facet = ?1 ORDER BY m.filename",
                [facet],
            )
            .await?;
        let mut mems = Vec::new();
        while let Some(row) = rows.next().await? {
            mems.push(MemoryStub {
                id: row.get_value(0)?.as_integer().copied().unwrap_or(0),
                filename: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
            });
        }
        Ok(mems)
    } else {
        let memory_ids = get_matching_memory_ids(conn, filters).await?;
        if memory_ids.is_empty() {
            return Ok(vec![]);
        }
        let (id_placeholders, mut id_params) = build_id_in_clause(&memory_ids, 0);
        let facet_idx = memory_ids.len() + 1;
        let sql = format!(
            "SELECT DISTINCT m.id, m.filename FROM memories m \
             JOIN tags t ON t.memory_id = m.id \
             WHERE m.id IN ({}) AND t.facet = ?{} ORDER BY m.filename",
            id_placeholders, facet_idx
        );
        id_params.push(turso::Value::from(facet));
        let mut rows = conn.query(&sql, id_params).await?;
        let mut mems = Vec::new();
        while let Some(row) = rows.next().await? {
            mems.push(MemoryStub {
                id: row.get_value(0)?.as_integer().copied().unwrap_or(0),
                filename: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
            });
        }
        Ok(mems)
    }
}

/// Get an untagged memory by filename (for root-level lookup).
pub async fn get_untagged_memory(conn: &Connection, filename: &str) -> Result<Option<Memory>> {
    let mut rows = conn
        .query(
            "SELECT m.id, m.filename, m.content, m.created_at, m.updated_at \
             FROM memories m LEFT JOIN tags t ON t.memory_id = m.id \
             WHERE m.filename = ?1 AND t.id IS NULL LIMIT 1",
            [filename],
        )
        .await?;
    match rows.next().await? {
        Some(row) => {
            let m = row_to_memory(&row)?;
            Ok(Some(m))
        }
        None => Ok(None),
    }
}

/// List memories that have no tags (for root-level display).
pub async fn list_untagged_memory_stubs(conn: &Connection) -> Result<Vec<MemoryStub>> {
    let mut rows = conn
        .query(
            "SELECT m.id, m.filename FROM memories m \
             LEFT JOIN tags t ON t.memory_id = m.id \
             WHERE t.id IS NULL ORDER BY m.filename",
            (),
        )
        .await?;
    let mut mems = Vec::new();
    while let Some(row) = rows.next().await? {
        mems.push(MemoryStub {
            id: row.get_value(0)?.as_integer().copied().unwrap_or(0),
            filename: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
        });
    }
    Ok(mems)
}

/// Lightweight result for readdir — id + filename only, no content or tags.
pub struct MemoryStub {
    pub id: i64,
    pub filename: String,
}

/// List memory id + filename without loading content or tags. Used by FUSE readdir.
pub async fn list_memory_stubs(conn: &Connection, filters: &[Filter]) -> Result<Vec<MemoryStub>> {
    if filters.is_empty() {
        let mut rows = conn
            .query("SELECT id, filename FROM memories ORDER BY filename", ())
            .await?;
        let mut mems = Vec::new();
        while let Some(row) = rows.next().await? {
            mems.push(MemoryStub {
                id: row.get_value(0)?.as_integer().copied().unwrap_or(0),
                filename: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
            });
        }
        return Ok(mems);
    }

    let memory_ids = get_matching_memory_ids(conn, filters).await?;
    if memory_ids.is_empty() {
        return Ok(vec![]);
    }
    let (id_placeholders, id_params) = build_id_in_clause(&memory_ids, 0);
    let sql = format!(
        "SELECT id, filename FROM memories WHERE id IN ({}) ORDER BY filename",
        id_placeholders
    );
    let mut rows = conn.query(&sql, id_params).await?;
    let mut mems = Vec::new();
    while let Some(row) = rows.next().await? {
        mems.push(MemoryStub {
            id: row.get_value(0)?.as_integer().copied().unwrap_or(0),
            filename: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
        });
    }
    Ok(mems)
}

// --- Lightweight queries (skip unnecessary data) ---

/// Lightweight result for grep — content only, no tags.
pub struct MemoryContent {
    pub filename: String,
    pub content: String,
}

/// List memory filename + content without loading tags. Used by grep.
pub async fn list_memory_contents(conn: &Connection, filters: &[Filter]) -> Result<Vec<MemoryContent>> {
    if filters.is_empty() {
        let mut rows = conn
            .query("SELECT filename, content FROM memories ORDER BY filename", ())
            .await?;
        let mut mems = Vec::new();
        while let Some(row) = rows.next().await? {
            mems.push(MemoryContent {
                filename: row.get_value(0)?.as_text().cloned().unwrap_or_default(),
                content: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
            });
        }
        return Ok(mems);
    }

    let memory_ids = get_matching_memory_ids(conn, filters).await?;
    if memory_ids.is_empty() {
        return Ok(vec![]);
    }
    let (id_placeholders, id_params) = build_id_in_clause(&memory_ids, 0);
    let sql = format!(
        "SELECT filename, content FROM memories WHERE id IN ({}) ORDER BY filename",
        id_placeholders
    );
    let mut rows = conn.query(&sql, id_params).await?;
    let mut mems = Vec::new();
    while let Some(row) = rows.next().await? {
        mems.push(MemoryContent {
            filename: row.get_value(0)?.as_text().cloned().unwrap_or_default(),
            content: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
        });
    }
    Ok(mems)
}

/// Lightweight result for find — metadata only, no content or tags.
pub struct MemoryMeta {
    pub filename: String,
    pub updated_at: String,
}

/// Find memories by filename pattern, returning only metadata. Used by find.
pub async fn find_memory_metadata(
    conn: &Connection,
    name_pattern: &str,
    filters: &[Filter],
) -> Result<Vec<MemoryMeta>> {
    let like_pattern = glob_to_like(name_pattern);

    if filters.is_empty() {
        let mut rows = conn
            .query(
                "SELECT filename, updated_at FROM memories WHERE filename LIKE ?1 ORDER BY filename",
                [like_pattern.as_str()],
            )
            .await?;
        let mut mems = Vec::new();
        while let Some(row) = rows.next().await? {
            mems.push(MemoryMeta {
                filename: row.get_value(0)?.as_text().cloned().unwrap_or_default(),
                updated_at: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
            });
        }
        return Ok(mems);
    }

    let memory_ids = get_matching_memory_ids(conn, filters).await?;
    if memory_ids.is_empty() {
        return Ok(vec![]);
    }
    let (id_placeholders, mut id_params) = build_id_in_clause(&memory_ids, 0);
    let like_idx = memory_ids.len() + 1;
    let sql = format!(
        "SELECT filename, updated_at FROM memories WHERE filename LIKE ?{} AND id IN ({}) ORDER BY filename",
        like_idx, id_placeholders
    );
    id_params.push(turso::Value::from(like_pattern.as_str()));
    let mut rows = conn.query(&sql, id_params).await?;
    let mut mems = Vec::new();
    while let Some(row) = rows.next().await? {
        mems.push(MemoryMeta {
            filename: row.get_value(0)?.as_text().cloned().unwrap_or_default(),
            updated_at: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
        });
    }
    Ok(mems)
}

// --- Helpers ---

/// Get all tags for a memory.
async fn get_tags(conn: &Connection, memory_id: i64) -> Result<Vec<Filter>> {
    let mut rows = conn
        .query(
            "SELECT facet, value FROM tags WHERE memory_id = ?1 ORDER BY facet, value",
            turso::params![memory_id],
        )
        .await?;
    let mut tags = Vec::new();
    while let Some(row) = rows.next().await? {
        let facet: String = row.get_value(0)?.as_text().cloned().unwrap_or_default();
        let value: String = row.get_value(1)?.as_text().cloned().unwrap_or_default();
        tags.push(Filter { facet, value });
    }
    Ok(tags)
}

/// Batch-fetch tags for multiple memories in a single query.
async fn get_tags_batch(conn: &Connection, memory_ids: &[i64]) -> Result<std::collections::HashMap<i64, Vec<Filter>>> {
    use std::collections::HashMap;

    if memory_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let (id_placeholders, id_params) = build_id_in_clause(memory_ids, 0);
    let sql = format!(
        "SELECT memory_id, facet, value FROM tags WHERE memory_id IN ({}) ORDER BY memory_id, facet, value",
        id_placeholders
    );

    let mut rows = conn.query(&sql, id_params).await?;
    let mut map: HashMap<i64, Vec<Filter>> = HashMap::new();
    while let Some(row) = rows.next().await? {
        let mid = row.get_value(0)?.as_integer().copied().unwrap_or(0);
        let facet: String = row.get_value(1)?.as_text().cloned().unwrap_or_default();
        let value: String = row.get_value(2)?.as_text().cloned().unwrap_or_default();
        map.entry(mid).or_default().push(Filter { facet, value });
    }
    Ok(map)
}

/// Get memory IDs that match ALL given filters.
/// Uses GROUP BY + HAVING COUNT approach (compatible with Turso, no subqueries).
async fn get_matching_memory_ids(conn: &Connection, filters: &[Filter]) -> Result<Vec<i64>> {
    if filters.is_empty() {
        // Return all memory IDs
        let mut rows = conn
            .query("SELECT id FROM memories ORDER BY id", ())
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get_value(0)?.as_integer().copied().unwrap_or(0));
        }
        return Ok(ids);
    }

    let mut conditions = Vec::new();
    let mut params = Vec::new();

    for (i, filter) in filters.iter().enumerate() {
        let base = i * 2 + 1;
        conditions.push(format!("(facet = ?{} AND value = ?{})", base, base + 1));
        params.push(filter.facet.clone());
        params.push(filter.value.clone());
    }

    let sql = format!(
        "SELECT memory_id FROM tags WHERE memory_id IS NOT NULL AND ({}) GROUP BY memory_id HAVING COUNT(DISTINCT facet || ':' || value) = {}",
        conditions.join(" OR "),
        filters.len()
    );
    let turso_params: Vec<turso::Value> = params.iter().map(|s| turso::Value::from(s.as_str())).collect();

    let mut rows = conn.query(&sql, turso_params).await?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await? {
        ids.push(row.get_value(0)?.as_integer().copied().unwrap_or(0));
    }
    Ok(ids)
}

/// Build a WHERE IN clause for a list of IDs.
fn build_id_in_clause(ids: &[i64], param_offset: usize) -> (String, Vec<turso::Value>) {
    let placeholders: Vec<String> = (0..ids.len())
        .map(|i| format!("?{}", param_offset + i + 1))
        .collect();
    let params: Vec<turso::Value> = ids.iter().map(|id| turso::Value::from(*id)).collect();
    (placeholders.join(", "), params)
}

/// Convert a glob pattern (*, ?) to SQL LIKE pattern (%, _).
fn glob_to_like(pattern: &str) -> String {
    pattern.replace('*', "%").replace('?', "_")
}

/// Extract a Memory struct from a database row.
fn row_to_memory(row: &turso::Row) -> Result<Memory> {
    Ok(Memory {
        id: row.get_value(0)?.as_integer().copied().unwrap_or(0),
        filename: row.get_value(1)?.as_text().cloned().unwrap_or_default(),
        content: row.get_value(2)?.as_text().cloned().unwrap_or_default(),
        created_at: row.get_value(3)?.as_text().cloned().unwrap_or_default(),
        updated_at: row.get_value(4)?.as_text().cloned().unwrap_or_default(),
        tags: Vec::new(), // loaded separately
    })
}
