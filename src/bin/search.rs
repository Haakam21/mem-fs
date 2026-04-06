use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

use memfs::embeddings::Embedder;
use memfs::queries::SearchResult;
use memfs::settings;

#[derive(Parser)]
#[command(name = "search", about = "Search memories by meaning")]
struct Args {
    /// Natural language query
    query: String,

    /// Path scope (narrow to a facet, e.g. ./memories/people/sister)
    path: Option<String>,

    /// Minimum similarity threshold (0.0-1.0, default from settings.json)
    #[arg(short = 't', long)]
    threshold: Option<f32>,

    /// Maximum number of results (default from settings.json)
    #[arg(short = 'k', long)]
    limit: Option<usize>,

    /// Show full content
    #[arg(short = 'v', long)]
    verbose: bool,
}

fn main() {
    let args = Args::parse();

    // Find the .memfs directory (walk up from CWD)
    let memfs_dir = match find_memfs_dir() {
        Some(d) => d,
        None => {
            eprintln!("search: no .memfs directory found");
            std::process::exit(1);
        }
    };

    let db_path = memfs_dir.join("db");
    let settings = settings::load(db_path.to_str().unwrap_or(""));
    let threshold = args.threshold.unwrap_or(settings.search_threshold);
    let limit = args.limit.unwrap_or(settings.search_limit);

    // Copy DB + WAL to a temp location to bypass the FUSE daemon's file lock.
    // This gives us a point-in-time snapshot of all committed data.
    let temp_dir = std::env::temp_dir().join(format!("memfs_search_{}", std::process::id()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let temp_db = temp_dir.join("db");
    if std::fs::copy(&db_path, &temp_db).is_err() {
        let _ = std::fs::remove_dir_all(&temp_dir);
        eprintln!("search: failed to copy database");
        std::process::exit(1);
    }
    // Copy WAL + SHM if they exist (needed to see uncommitted data)
    for suffix in ["db-wal", "db-shm"] {
        let src = db_path.with_file_name(suffix);
        if src.exists() {
            let _ = std::fs::copy(&src, temp_dir.join(suffix));
        }
    }

    let conn = match rusqlite::Connection::open_with_flags(
        &temp_db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            eprintln!("search: failed to open database: {}", e);
            std::process::exit(1);
        }
    };

    let embedder = match Embedder::load_or_download() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("search: failed to load model: {}", e);
            std::process::exit(1);
        }
    };

    if args.query.trim().is_empty() {
        eprintln!("search: query cannot be empty");
        std::process::exit(1);
    }

    let query_embedding = match embedder.embed(&args.query) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("search: failed to embed query: {}", e);
            std::process::exit(1);
        }
    };

    // Parse scope path into facet filters for scoped search
    let scope_filters = args.path.as_deref().and_then(|p| parse_scope(p));

    // Query embeddings from DB
    let rows = match load_embeddings(&conn, scope_filters.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("search: {}", e);
            std::process::exit(1);
        }
    };

    // Rank by cosine similarity
    let mut results: Vec<SearchResult> = rows
        .iter()
        .filter_map(|(filename, content, emb_bytes)| {
            let emb = Embedder::deserialize_embedding(emb_bytes).ok()?;
            let score = Embedder::cosine_similarity(&query_embedding, &emb);
            if score >= threshold {
                Some(SearchResult {
                    filename: filename.clone(),
                    score,
                    content: content.clone(),
                })
            } else {
                None
            }
        })
        .collect();

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(limit);

    if results.is_empty() && rows.is_empty() {
        eprintln!("search: no embeddings found. Write some memories first, or run `memfs reindex`.");
    }

    let output = memfs::format::format_search(&results, args.verbose);
    if !output.is_empty() {
        println!("{}", output);
    }

    drop(conn);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

/// Find the .memfs data directory at ~/.memfs/
fn find_memfs_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join(".memfs");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scope_single_filter() {
        let result = parse_scope("./memories/people/sister").unwrap();
        assert_eq!(result, vec![("people".to_string(), "sister".to_string())]);
    }

    #[test]
    fn parse_scope_multiple_filters() {
        let result = parse_scope("/memories/people/sister/topics/cooking").unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], ("people".to_string(), "sister".to_string()));
        assert_eq!(result[1], ("topics".to_string(), "cooking".to_string()));
    }

    #[test]
    fn parse_scope_just_memories() {
        assert!(parse_scope("./memories").is_none());
    }

    #[test]
    fn parse_scope_trailing_facet_ignored() {
        // Odd segment after memories = facet-level, no complete filter
        let result = parse_scope("./memories/people");
        assert!(result.is_none());
    }

    #[test]
    fn parse_scope_no_memories_in_path() {
        assert!(parse_scope("/some/random/path").is_none());
    }

    #[test]
    fn parse_scope_absolute_path() {
        let result = parse_scope("/tmp/install/memories/dates/2025-03").unwrap();
        assert_eq!(result, vec![("dates".to_string(), "2025-03".to_string())]);
    }
}

/// Extract facet:value pairs from a scope path.
/// e.g., "./memories/people/sister" → [("people", "sister")]
fn parse_scope(path: &str) -> Option<Vec<(String, String)>> {
    // Find "memories" in the path and take segments after it
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mem_idx = parts.iter().position(|&s| s == "memories")?;
    let segments = &parts[mem_idx + 1..];

    let mut filters = Vec::new();
    let mut i = 0;
    while i + 1 < segments.len() {
        filters.push((segments[i].to_string(), segments[i + 1].to_string()));
        i += 2;
    }

    if filters.is_empty() {
        None
    } else {
        Some(filters)
    }
}

/// Load embeddings from the DB, optionally filtered by facet:value pairs.
fn load_embeddings(
    conn: &rusqlite::Connection,
    filters: Option<&[(String, String)]>,
) -> Result<Vec<(String, String, Vec<u8>)>> {
    match filters {
        None => {
            // All memories with embeddings
            let mut stmt = conn.prepare(
                "SELECT m.filename, m.content, e.embedding \
                 FROM memories m JOIN embeddings e ON e.memory_id = m.id \
                 ORDER BY m.filename",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        }
        Some(filters) => {
            // Two-step: get matching IDs, then fetch embeddings
            let mut conditions = Vec::new();
            let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
            for (i, (facet, value)) in filters.iter().enumerate() {
                let base = i * 2 + 1;
                conditions.push(format!("(facet = ?{} AND value = ?{})", base, base + 1));
                params.push(Box::new(facet.clone()));
                params.push(Box::new(value.clone()));
            }

            let id_sql = format!(
                "SELECT memory_id FROM tags WHERE memory_id > 0 AND ({}) \
                 GROUP BY memory_id HAVING COUNT(DISTINCT facet || ':' || value) = {}",
                conditions.join(" OR "),
                filters.len()
            );

            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut id_stmt = conn.prepare(&id_sql)?;
            let ids: Vec<i64> = id_stmt
                .query_map(param_refs.as_slice(), |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            if ids.is_empty() {
                return Ok(vec![]);
            }

            let placeholders: Vec<String> = (1..=ids.len()).map(|i| format!("?{}", i)).collect();
            let sql = format!(
                "SELECT m.filename, m.content, e.embedding \
                 FROM memories m JOIN embeddings e ON e.memory_id = m.id \
                 WHERE m.id IN ({}) ORDER BY m.filename",
                placeholders.join(", ")
            );

            let id_params: Vec<Box<dyn rusqlite::types::ToSql>> =
                ids.iter().map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>).collect();
            let id_refs: Vec<&dyn rusqlite::types::ToSql> =
                id_params.iter().map(|p| p.as_ref()).collect();

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt
                .query_map(id_refs.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        }
    }
}
