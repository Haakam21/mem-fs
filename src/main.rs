mod db;
#[cfg(feature = "search")]
mod embeddings;
mod engine;
mod format;
mod fuse;
mod path;
mod queries;
mod settings;
mod state;
mod util;

use clap::{Parser, Subcommand};
use std::env;
use std::io::Read;

const DEFAULT_MOUNT: &str = "/memories";
const DEFAULT_STATE: &str = "./.memfs/state";
const DEFAULT_DB: &str = "./.memfs/db";

fn mount_point() -> String {
    env::var("MEMFS_MOUNT").unwrap_or_else(|_| DEFAULT_MOUNT.to_string())
}

fn state_path() -> String {
    env::var("MEMFS_STATE").unwrap_or_else(|_| DEFAULT_STATE.to_string())
}

fn db_path() -> String {
    env::var("MEMFS_DB").unwrap_or_else(|_| DEFAULT_DB.to_string())
}

#[derive(Parser)]
#[command(name = "memfs", about = "Virtual faceted memory filesystem")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Change virtual working directory
    Cd {
        /// Target path
        path: Option<String>,
    },
    /// List contents at current or specified path
    Ls {
        /// Target path
        path: Option<String>,
        /// Long format
        #[arg(short = 'l')]
        long: bool,
        /// Show all (compatibility, ignored)
        #[arg(short = 'a')]
        all: bool,
    },
    /// Print current virtual working directory
    Pwd,
    /// Display memory content
    Cat {
        /// Filename(s) to display
        files: Vec<String>,
    },
    /// Create facet categories or values
    Mkdir {
        /// Path to create
        path: String,
        /// Create parent directories as needed
        #[arg(short = 'p')]
        parents: bool,
    },
    /// Remove a memory or untag
    Rm {
        /// Target to remove
        target: String,
        /// Recursive (untag facet value)
        #[arg(short = 'r')]
        recursive: bool,
        /// Force (no confirmation)
        #[arg(short = 'f')]
        force: bool,
    },
    /// Retag a memory (move between facet values)
    Mv {
        /// Source path
        source: String,
        /// Destination path
        dest: String,
    },
    /// Add an additional tag to a memory
    Cp {
        /// Source path
        source: String,
        /// Destination path
        dest: String,
    },
    /// Create a new memory
    Write {
        /// Filename for the new memory
        filename: String,
        /// Content (reads stdin if omitted)
        content: Option<String>,
    },
    /// Append to an existing memory
    Append {
        /// Filename to append to
        filename: String,
        /// Content to append (reads stdin if omitted)
        content: Option<String>,
    },
    /// Search memory content with regex
    Grep {
        /// Search pattern
        pattern: String,
        /// Path scope
        path: Option<String>,
        /// Case insensitive
        #[arg(short = 'i')]
        ignore_case: bool,
        /// List filenames only
        #[arg(short = 'l')]
        files_only: bool,
        /// Recursive (searches all in scope, default behavior)
        #[arg(short = 'r')]
        recursive: bool,
        /// Show line numbers
        #[arg(short = 'n')]
        line_numbers: bool,
    },
    /// Sync memories with cloud
    Sync,
    /// Mount as FUSE filesystem
    Mount {
        /// Mount point path
        mountpoint: String,
        /// Run in foreground (don't daemonize)
        #[arg(short = 'f', long)]
        foreground: bool,
    },
    /// Unmount FUSE filesystem
    Unmount {
        /// Mount point to unmount
        mountpoint: String,
    },
    /// Semantic search by meaning
    Search {
        /// Natural language query
        query: String,
        /// Path scope
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
    },
    /// Generate embeddings for all memories
    Reindex {
        /// Scope (optional path)
        path: Option<String>,
    },
    /// Search by filename/metadata
    Find {
        /// Path scope
        path: Option<String>,
        /// Filename pattern
        #[arg(long)]
        name: Option<String>,
        /// Type filter (d for directories, f for files)
        #[arg(long = "type")]
        file_type: Option<String>,
        /// Modified within N days (negative = within, positive = older than)
        #[arg(long)]
        mtime: Option<i64>,
    },
}

/// Read content from stdin (for write/append when no content arg given).
fn read_stdin() -> String {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).unwrap_or_default();
    buf.trim_end().to_string()
}

fn main() {
    let cli = Cli::parse();

    // Mount/Unmount run outside tokio — they create their own runtime
    match cli.command {
        Commands::Mount {
            mountpoint,
            foreground,
        } => {
            if let Err(e) = fuse::mount(&db_path(), &mount_point(), &mountpoint, foreground) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
            return;
        }
        Commands::Unmount { mountpoint } => {
            if let Err(e) = fuse::unmount(&mountpoint) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
            return;
        }
        other => {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(run_command(other));
        }
    }
}

async fn run_command(command: Commands) {
    let db = db_path();
    let settings = settings::load(&db);

    let database = match db::open(&db, &settings).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("memfs: failed to open database: {}", e);
            std::process::exit(1);
        }
    };
    let db = std::sync::Arc::new(database);
    let conn = match db.connect().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("memfs: failed to connect to database: {}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = db::migrate(&conn).await {
        eprintln!("memfs: migration failed: {}", e);
        std::process::exit(1);
    }

    #[cfg(feature = "search")]
    let embedder = match &command {
        Commands::Search { .. } | Commands::Reindex { .. } => {
            match embeddings::Embedder::load_or_download() {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("memfs: failed to load embedding model: {}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => embeddings::Embedder::try_load().unwrap_or(None),
    };

    let eng = engine::Engine::new(
        conn,
        db.clone(),
        state_path(),
        mount_point(),
        #[cfg(feature = "search")]
        embedder,
    );

    let result = match command {
        Commands::Cd { path } => {
            let mount = mount_point();
            let target = path.as_deref().unwrap_or(&mount);
            eng.cd(target).await
        }
        Commands::Ls { path, long, .. } => {
            match eng.ls(path.as_deref()).await {
                Ok(entries) => {
                    let output = if long {
                        format::format_ls_long(&entries)
                    } else {
                        format::format_ls(&entries)
                    };
                    if !output.is_empty() {
                        println!("{}", output);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        Commands::Pwd => match eng.pwd() {
            Ok(cwd) => {
                println!("{}", cwd);
                Ok(())
            }
            Err(e) => Err(e),
        },
        Commands::Cat { files } => {
            if files.is_empty() {
                Err(anyhow::anyhow!("memfs: cat: missing filename"))
            } else {
                let mut first = true;
                let mut last_err = None;
                for file in &files {
                    match eng.cat(file).await {
                        Ok(mem) => {
                            if !first {
                                println!();
                            }
                            println!("{}", format::format_cat(&mem));
                            first = false;
                        }
                        Err(e) => {
                            eprintln!("{}", e);
                            last_err = Some(e);
                        }
                    }
                }
                match last_err {
                    Some(e) => Err(e),
                    None => Ok(()),
                }
            }
        }
        Commands::Mkdir { path, parents } => eng.mkdir(&path, parents).await,
        Commands::Rm {
            target,
            recursive,
            force: _,
        } => match eng.rm(&target, recursive).await {
            Ok(msg) => {
                println!("{}", msg);
                Ok(())
            }
            Err(e) => Err(e),
        },
        Commands::Mv { source, dest } => eng.mv(&source, &dest).await,
        Commands::Cp { source, dest } => eng.cp(&source, &dest).await,
        Commands::Write { filename, content } => {
            let text = content.unwrap_or_else(read_stdin);
            eng.write(&filename, &text).await
        }
        Commands::Append { filename, content } => {
            let text = content.unwrap_or_else(read_stdin);
            eng.append(&filename, &text).await
        }
        Commands::Grep {
            pattern,
            path,
            ignore_case,
            files_only,
            line_numbers,
            ..
        } => match eng.grep(&pattern, path.as_deref(), ignore_case).await {
            Ok(results) => {
                let output = format::format_grep(&results, files_only, line_numbers);
                if !output.is_empty() {
                    println!("{}", output);
                }
                Ok(())
            }
            Err(e) => Err(e),
        },
        Commands::Find {
            path,
            name,
            file_type,
            mtime,
        } => {
            match eng
                .find(
                    path.as_deref(),
                    name.as_deref(),
                    file_type.as_deref(),
                    mtime,
                )
                .await
            {
                Ok(paths) => {
                    let output = format::format_find(&paths);
                    if !output.is_empty() {
                        println!("{}", output);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        #[cfg(feature = "search")]
        Commands::Search {
            query,
            path,
            threshold,
            limit,
            verbose,
        } => {
            let threshold = threshold.unwrap_or(settings.search_threshold);
            let limit = limit.unwrap_or(settings.search_limit);
            match eng.search(&query, path.as_deref(), threshold, limit).await {
                Ok(results) => {
                    let output = format::format_search(&results, verbose);
                    if !output.is_empty() {
                        println!("{}", output);
                    }
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        #[cfg(not(feature = "search"))]
        Commands::Search { .. } | Commands::Reindex { .. } => {
            Err(anyhow::anyhow!("memfs: built without search feature"))
        }
        #[cfg(feature = "search")]
        Commands::Reindex { path } => match eng.reindex(path.as_deref()).await {
            Ok(count) => {
                println!("Reindexed {} memories", count);
                Ok(())
            }
            Err(e) => Err(e),
        },
        Commands::Sync => {
            match db.pull().await {
                Ok(true) => println!("Synced from cloud"),
                Ok(false) => println!("Already up to date"),
                Err(e) => {
                    eprintln!("memfs: sync failed: {}", e);
                    std::process::exit(1);
                }
            }
            db.push().await;
            Ok(())
        }
        Commands::Mount { .. } | Commands::Unmount { .. } => unreachable!(),
    };

    if let Err(e) = result {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}
