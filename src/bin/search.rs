use anyhow::{bail, Result};
use clap::Parser;
use std::path::{Path, PathBuf};

use memfs::embeddings::Embedder;

#[derive(Parser)]
#[command(name = "search", about = "Search memories by meaning")]
struct Args {
    /// Natural language query
    query: String,

    /// Path scope (narrow to a facet, e.g. ./memories/people/sister)
    path: Option<String>,

    /// Minimum similarity threshold (0.0-1.0)
    #[arg(short = 't', long, default_value = "0.3")]
    threshold: f32,

    /// Maximum number of results
    #[arg(short = 'k', long, default_value = "10")]
    limit: usize,

    /// Show full content
    #[arg(short = 'v', long)]
    verbose: bool,
}

struct MemoryFile {
    path: String,
    content: String,
    embedding: Vec<f32>,
}

fn main() {
    let args = Args::parse();

    let search_dir = match &args.path {
        Some(p) => PathBuf::from(p),
        None => {
            // Walk up from CWD looking for a directory containing "memories/"
            match find_memories_dir() {
                Some(d) => d,
                None => {
                    eprintln!("search: no memories directory found. Specify a path.");
                    std::process::exit(1);
                }
            }
        }
    };

    if !search_dir.is_dir() {
        eprintln!("search: {}: not a directory", search_dir.display());
        std::process::exit(1);
    }

    let embedder = match Embedder::load_or_download() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("search: failed to load model: {}", e);
            std::process::exit(1);
        }
    };

    let query_embedding = match embedder.embed(&args.query) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("search: failed to embed query: {}", e);
            std::process::exit(1);
        }
    };

    // Recursively find all files in the search directory
    let files = match collect_files(&search_dir) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("search: {}", e);
            std::process::exit(1);
        }
    };

    // Deduplicate files with identical content (same memory appears at
    // multiple paths in the faceted filesystem)
    let mut seen_content = std::collections::HashSet::new();
    let files: Vec<_> = files
        .into_iter()
        .filter(|(_, content)| seen_content.insert(content.clone()))
        .collect();

    // Embed each file and rank
    let mut results: Vec<(String, f32, String)> = Vec::new();
    for (path, content) in &files {
        if content.trim().is_empty() {
            continue;
        }
        if let Ok(embedding) = embedder.embed(content) {
            let score = Embedder::cosine_similarity(&query_embedding, &embedding);
            if score >= args.threshold {
                results.push((path.clone(), score, content.clone()));
            }
        }
    }

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(args.limit);

    for (path, score, content) in &results {
        if args.verbose {
            println!("--- {} ({:.2}) ---\n{}", path, score, content);
        } else {
            let preview = content.lines().next().unwrap_or("(empty)");
            let preview = if preview.len() > 80 {
                format!("{}...", &preview[..77])
            } else {
                preview.to_string()
            };
            println!("{} ({:.2}): {}", path, score, preview);
        }
    }
}

/// Walk up from CWD looking for a directory containing "memories/".
fn find_memories_dir() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("memories");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Recursively collect all regular files under a directory.
/// Only reads .md and .txt files, skips directories and hidden files.
fn collect_files(dir: &Path) -> Result<Vec<(String, String)>> {
    let mut files = Vec::new();
    collect_files_recursive(dir, dir, &mut files)?;
    Ok(files)
}

fn collect_files_recursive(
    base: &Path,
    dir: &Path,
    files: &mut Vec<(String, String)>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden files/dirs
        if name_str.starts_with('.') {
            continue;
        }

        if path.is_dir() {
            collect_files_recursive(base, &path, files)?;
        } else if path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let relative = path.strip_prefix(base).unwrap_or(&path);
                files.push((relative.to_string_lossy().to_string(), content));
            }
        }
    }
    Ok(())
}
