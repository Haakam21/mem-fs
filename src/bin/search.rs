use anyhow::Result;
use clap::Parser;

// Re-use the library modules from the main crate
use memfs::{db, embeddings, engine, format, path, queries};

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

fn main() {
    let args = Args::parse();

    let rt = tokio::runtime::Runtime::new().unwrap();

    let db_path = std::env::var("MEMFS_DB").unwrap_or_else(|_| "./.memfs.db".to_string());
    let mount_point =
        std::env::var("MEMFS_MOUNT").unwrap_or_else(|_| "/memories".to_string());

    let database = match rt.block_on(db::open(&db_path)) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("search: failed to open database: {}", e);
            std::process::exit(1);
        }
    };
    let conn = match database.connect() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("search: failed to connect: {}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(db::migrate(&conn)) {
        eprintln!("search: migration failed: {}", e);
        std::process::exit(1);
    }

    let embedder = match embeddings::Embedder::load_or_download() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("search: failed to load model: {}", e);
            std::process::exit(1);
        }
    };

    let eng = engine::Engine::new(
        conn,
        String::new(), // no state path needed for search
        mount_point,
        Some(embedder),
    );

    let result = rt.block_on(eng.search(
        &args.query,
        args.path.as_deref(),
        args.threshold,
        args.limit,
    ));

    match result {
        Ok(results) => {
            let output = format::format_search(&results, args.verbose);
            if !output.is_empty() {
                println!("{}", output);
            }
        }
        Err(e) => {
            eprintln!("search: {}", e);
            std::process::exit(1);
        }
    }
}
