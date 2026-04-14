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
const DEFAULT_STATE: &str = "~/.memfs/state";
const DEFAULT_DB: &str = "~/.memfs/db";

fn mount_point() -> String {
    env::var("MEMFS_MOUNT").unwrap_or_else(|_| DEFAULT_MOUNT.to_string())
}

fn state_path() -> String {
    env::var("MEMFS_STATE").unwrap_or_else(|_| DEFAULT_STATE.to_string())
}

fn db_path() -> String {
    env::var("MEMFS_DB").unwrap_or_else(|_| DEFAULT_DB.to_string())
}

/// Check whether `path` is an active FUSE mount by consulting the kernel's
/// mount table. `read_dir().is_ok()` lies — it succeeds even when the
/// directory is a plain backing dir with no FUSE filesystem over it.
fn is_fuse_mounted(path: &std::path::Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let target = match path.canonicalize() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let target_str = match target.to_str() {
            Some(s) => s,
            None => return false,
        };
        let mounts = match std::fs::read_to_string("/proc/self/mountinfo") {
            Ok(s) => s,
            Err(_) => return false,
        };
        // /proc/self/mountinfo format: field 5 is mountpoint, field 9 is fstype (varies).
        // We look for any line whose mountpoint matches our target AND whose filesystem
        // type contains "fuse".
        for line in mounts.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 10 {
                continue;
            }
            let mountpoint = parts[4];
            if mountpoint != target_str {
                continue;
            }
            // Fields after " - " separator: fstype, source, super_options
            if let Some(dash_idx) = parts.iter().position(|&p| p == "-") {
                if let Some(fstype) = parts.get(dash_idx + 1) {
                    if fstype.starts_with("fuse") {
                        return true;
                    }
                }
            }
        }
        false
    }
    #[cfg(target_os = "macos")]
    {
        // On macOS fall back to mount(8) — `mount` lists active mounts with
        // their type in parens.
        let Ok(output) = std::process::Command::new("mount").output() else {
            return false;
        };
        let target_str = match path.to_str() {
            Some(s) => s,
            None => return false,
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.contains(target_str) && (line.contains("(osxfuse") || line.contains("(macfuse") || line.contains("(fuse")))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        false
    }
}

#[derive(Parser)]
#[command(name = "memfs", about = "Virtual faceted memory filesystem")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[cfg(test)]
mod is_fuse_mounted_tests {
    use super::is_fuse_mounted;
    use std::path::PathBuf;

    #[test]
    fn returns_false_for_nonexistent_path() {
        let path = PathBuf::from("/nonexistent/memfs/mount/path/abc123");
        assert!(!is_fuse_mounted(&path));
    }

    #[test]
    fn returns_false_for_plain_directory() {
        // /tmp exists on every supported platform and is not a FUSE mount.
        let path = PathBuf::from("/tmp");
        assert!(!is_fuse_mounted(&path));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn returns_false_for_non_mounted_dir_with_content() {
        // Create a real, non-empty directory and confirm we don't mistake
        // it for a FUSE mount just because read_dir would succeed on it.
        // This is the exact bug that caused the pre-0.12.2 silent failure.
        let dir = std::env::temp_dir().join(format!(
            "memfs-is-fuse-mounted-test-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(dir.join("topics"));
        let _ = std::fs::write(dir.join("topics").join("decoy.md"), "decoy content");
        let result = is_fuse_mounted(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!result, "is_fuse_mounted returned true for a plain directory");
    }
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
        /// Show line numbers
        #[arg(short = 'n')]
        line_numbers: bool,
    },
    /// Sync memories with cloud
    Sync,
    /// Initialize MemFS: configure cloud sync, mount, and set up Claude Code
    Init,
    /// Update MemFS to the latest release
    Update,
    /// Uninstall MemFS: unmount, remove binaries and config
    Uninstall {
        /// Also delete database and models
        #[arg(long)]
        purge: bool,
    },
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

fn init() -> anyhow::Result<()> {
    let base = std::env::current_dir()?;
    let data_dir = home_dir().join(".memfs");
    let global_mount = data_dir.join("mount");
    let local_link = base.join("memories");
    let claude_dir = base.join(".claude");

    std::fs::create_dir_all(&data_dir)?;

    // --- Cloud sync (always prompt; blank input keeps existing values) ---
    let settings_path = data_dir.join("settings.json");
    let db_path_str = data_dir.join("db").to_string_lossy().into_owned();
    let existing = crate::settings::load(&db_path_str);
    let existing_url = existing.turso_url;
    let existing_token = existing.turso_token;

    let url_hint = if existing_url.is_some() {
        "Enter to keep"
    } else {
        "Enter to skip"
    };
    eprint!("Turso URL ({}): ", url_hint);
    let mut turso_url_input = String::new();
    std::io::stdin().read_line(&mut turso_url_input)?;
    let turso_url_input = turso_url_input.trim().to_string();

    let turso_url = if turso_url_input.is_empty() {
        existing_url
    } else {
        Some(turso_url_input)
    };

    if let Some(url) = turso_url {
        let token_hint = if existing_token.is_some() {
            "Enter to keep"
        } else {
            "required"
        };
        eprint!("Turso token ({}): ", token_hint);
        let mut turso_token_input = String::new();
        std::io::stdin().read_line(&mut turso_token_input)?;
        let turso_token_input = turso_token_input.trim().to_string();

        let turso_token = if turso_token_input.is_empty() {
            existing_token
        } else {
            Some(turso_token_input)
        };

        if let Some(token) = turso_token {
            let json = format!(
                "{{\"turso_url\":\"{}\",\"turso_token\":\"{}\"}}",
                url, token
            );
            std::fs::write(&settings_path, &json)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &settings_path,
                    std::fs::Permissions::from_mode(0o600),
                )?;
            }

            eprintln!("Cloud sync configured.");
        }
    }

    // --- Mount daemon (shared, only start if not already running) ---
    let db_path = data_dir.join("db");
    let already_mounted = is_fuse_mounted(&global_mount);

    if !already_mounted {
        // On Linux, the FUSE daemon needs `user_allow_other` in
        // /etc/fuse.conf because fuser's AutoUnmount implicitly enables
        // allow_other. Without it, fusermount3 rejects the mount and the
        // systemd service crash-loops silently — callers then write to
        // the backing directory thinking it's the mount. Check upfront
        // with a clear error instead.
        #[cfg(target_os = "linux")]
        {
            let fuse_conf_path = std::path::Path::new("/etc/fuse.conf");
            let has_user_allow_other = std::fs::read_to_string(fuse_conf_path)
                .map(|s| {
                    s.lines().any(|line| {
                        let trimmed = line.trim();
                        !trimmed.starts_with('#') && trimmed == "user_allow_other"
                    })
                })
                .unwrap_or(false);
            if !has_user_allow_other {
                anyhow::bail!(
                    "/etc/fuse.conf is missing `user_allow_other`, which FUSE \
requires when the daemon uses AutoUnmount. Enable it with:\n\n    \
echo user_allow_other | sudo tee -a /etc/fuse.conf\n\nThen re-run `memfs init`."
                );
            }
        }

        stop_mount(&global_mount);
        std::fs::create_dir_all(&global_mount)?;

        // Clean any legacy facet directories from older init versions that
        // seeded them as real files on the backing fs. Those shadow FUSE's
        // own view and cause writes to fall through to the backing dir
        // when the mount fails or isn't active yet.
        for facet in ["people", "topics", "dates", "projects", "sessions"] {
            let _ = std::fs::remove_dir_all(global_mount.join(facet));
        }

        // Seed facet categories in the DB BEFORE starting the FUSE daemon
        // (the daemon holds an exclusive lock on the db once running). This
        // populates the facets TABLE, not the filesystem — the FUSE daemon
        // serves facet dirs dynamically from that table, so they don't
        // become persistent backing dirs that shadow the mount.
        {
            let rt = tokio::runtime::Runtime::new()?;
            let database = rt.block_on(db::open(&db_path.to_string_lossy()))?;
            let conn = database.connect()?;
            rt.block_on(async {
                let _ = db::migrate(&conn).await;
                for facet in ["people", "topics", "dates", "projects", "sessions"] {
                    let _ = queries::create_facet(&conn, facet).await;
                }
            });
        }

        let memfs_bin = dirs_self();
        eprintln!("Starting FUSE daemon...");

        install_service(&memfs_bin, &global_mount, &db_path)?;

        // Poll for up to 5 seconds, verifying the mount is actually a FUSE
        // filesystem (not just a readable directory). Bail loudly if the
        // daemon failed to mount — silent failure is how writes end up in
        // an unindexed backing directory.
        let mut mounted = false;
        for _ in 0..50 {
            if is_fuse_mounted(&global_mount) {
                mounted = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !mounted {
            anyhow::bail!(
                "FUSE mount at {} did not come up after 5 seconds. \
Check `journalctl --user -u memfs.service` for the cause. \
Common fix on Linux: `echo user_allow_other | sudo tee -a /etc/fuse.conf`.",
                global_mount.display()
            );
        }
        eprintln!("Mounted at {}", global_mount.display());
    } else {
        eprintln!("FUSE daemon already running.");
    }

    // --- Symlink ./memories → ~/.memfs/mount ---
    if local_link.exists() || local_link.read_link().is_ok() {
        let _ = std::fs::remove_file(&local_link);
        let _ = std::fs::remove_dir(&local_link);
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&global_mount, &local_link)?;
    eprintln!("Linked ./memories → {}", global_mount.display());

    // --- Claude Code settings ---
    std::fs::create_dir_all(&claude_dir)?;
    let claude_settings = claude_dir.join("settings.json");
    if !claude_settings.exists() {
        std::fs::write(&claude_settings, "{\"autoMemoryEnabled\":false}")?;
    }

    // --- CLAUDE.md ---
    let claude_md = base.join("CLAUDE.md");
    let memories_prefix = "Your memories are in the ./memories directory.";
    let memories_line = "Your memories are in the ./memories directory. At the start of every session, check them for anything relevant. Use `search \"query\"` to find memories by meaning. Save important things you learn to memory. At the end of every session, write a summary of what you did and decided to ./memories/sessions/.";
    if !claude_md.exists() {
        std::fs::write(&claude_md, memories_line)?;
    } else {
        let content = std::fs::read_to_string(&claude_md)?;
        if content.contains(memories_line) {
            // Already up to date
        } else if let Some(start) = content.find(memories_prefix) {
            // Replace old memories line with updated one
            let end = content[start..].find('\n').map(|i| start + i).unwrap_or(content.len());
            let updated = format!("{}{}{}", &content[..start], memories_line, &content[end..]);
            std::fs::write(&claude_md, updated)?;
        } else {
            std::fs::write(&claude_md, format!("{}\n{}", content, memories_line))?;
        }
    }

    eprintln!();
    eprintln!("=== MemFS is ready ===");
    eprintln!("  Memories:  ./memories → {}", global_mount.display());
    eprintln!("  CLAUDE.md: {}", claude_md.display());

    Ok(())
}

fn sync_cmd() -> anyhow::Result<()> {
    let db_p = db_path();
    let settings = settings::load(&db_p);

    let mount = home_dir().join(".memfs/mount");
    eprintln!("Stopping FUSE daemon...");

    // Unload service FIRST to prevent auto-restart, then stop the mount
    let plist = home_dir().join("Library/LaunchAgents/com.memfs.mount.plist");
    if cfg!(target_os = "macos") && plist.exists() {
        let _ = std::process::Command::new("launchctl").args(["unload", "-w"]).arg(&plist).stderr(std::process::Stdio::null()).status();
    } else if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("systemctl").args(["--user", "stop", "memfs"]).status();
    }
    stop_mount(&mount);

    // Sync with cloud
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(db::sync(&db_p, &settings))?;

    // Restart FUSE daemon
    if cfg!(target_os = "macos") && plist.exists() {
        let _ = std::process::Command::new("launchctl").args(["load", "-w"]).arg(&plist).status();
        eprintln!("FUSE daemon restarted.");
    } else if cfg!(target_os = "linux") {
        let _ = std::process::Command::new("systemctl").args(["--user", "start", "memfs"]).status();
        eprintln!("FUSE daemon restarted.");
    }

    Ok(())
}

fn update() -> anyhow::Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let repo = "Haakam21/mem-fs";

    // Get latest release tag via gh CLI
    let output = std::process::Command::new("gh")
        .args(["release", "view", "--repo", repo, "--json", "tagName", "-q", ".tagName"])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("failed to check latest release (is gh CLI installed and authenticated?)");
    }

    let latest_tag = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let latest_version = latest_tag.trim_start_matches('v');

    if latest_version == current {
        eprintln!("Already up to date (v{})", current);
        return Ok(());
    }

    eprintln!("Updating v{} → {}", current, latest_tag);

    // Detect platform
    let artifact = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "memfs-darwin-arm64",
        ("macos", "x86_64") => "memfs-darwin-x86_64",
        ("linux", "x86_64") => "memfs-linux-x86_64",
        (os, arch) => anyhow::bail!("unsupported platform: {}-{}", os, arch),
    };

    let bin_dir = home_dir().join(".memfs");

    // Download memfs binary
    let status = std::process::Command::new("gh")
        .args(["release", "download", "--repo", repo, "--pattern", artifact, "--dir"])
        .arg(&bin_dir)
        .arg("--clobber")
        .status()?;
    if !status.success() {
        anyhow::bail!("failed to download {}", artifact);
    }
    std::fs::rename(bin_dir.join(artifact), bin_dir.join("memfs"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(bin_dir.join("memfs"), std::fs::Permissions::from_mode(0o755))?;
    }

    // Download search binary
    let search_artifact = format!("search-{}", artifact.trim_start_matches("memfs-"));
    let search_dir = home_dir().join(".local/bin");
    std::fs::create_dir_all(&search_dir)?;
    let status = std::process::Command::new("gh")
        .args(["release", "download", "--repo", repo, "--pattern", &search_artifact, "--dir"])
        .arg(&search_dir)
        .arg("--clobber")
        .status()?;
    if status.success() {
        let _ = std::fs::rename(search_dir.join(&search_artifact), search_dir.join("search"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(search_dir.join("search"), std::fs::Permissions::from_mode(0o755));
        }
    }

    eprintln!("Updated to {}", latest_tag);
    Ok(())
}

fn uninstall(purge: bool) -> anyhow::Result<()> {
    let base = std::env::current_dir()?;
    let local_link = base.join("memories");
    let data_dir = home_dir().join(".memfs");
    let global_mount = data_dir.join("mount");

    // Stop service FIRST (prevents restart after unmount)
    eprintln!("Stopping service...");
    if cfg!(target_os = "macos") {
        let plist = home_dir().join("Library/LaunchAgents/com.memfs.mount.plist");
        let _ = std::process::Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&plist)
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_file(&plist);
    } else {
        let _ = std::process::Command::new("systemctl").args(["--user", "disable", "--now", "memfs"]).status();
        let _ = std::fs::remove_file(home_dir().join(".config/systemd/user/memfs.service"));
    }

    // Unmount the global mount point
    stop_mount(&global_mount);
    let _ = std::fs::remove_dir(&global_mount);

    // Remove local symlink
    if local_link.read_link().is_ok() {
        let _ = std::fs::remove_file(&local_link);
    }

    // Remove binaries
    let _ = std::fs::remove_file(home_dir().join(".local/bin/search"));
    eprintln!("Removed service and binaries.");

    // Remove Claude Code config
    let _ = std::fs::remove_file(base.join(".claude/settings.json"));
    let _ = std::fs::remove_dir(base.join(".claude"));

    if purge {
        let _ = std::fs::remove_dir_all(&data_dir);
        eprintln!("Purged ~/.memfs (database, models, config, binary)");
    } else {
        eprintln!("Data preserved at {} (use --purge to delete)", data_dir.display());
    }

    eprintln!("MemFS uninstalled.");
    Ok(())
}

fn home_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}

fn install_service(
    memfs_bin: &std::path::Path,
    mount_path: &std::path::Path,
    db_path: &std::path::Path,
) -> anyhow::Result<()> {
    if cfg!(target_os = "macos") {
        install_launchd(memfs_bin, mount_path, db_path)
    } else {
        install_systemd(memfs_bin, mount_path, db_path)
    }
}

fn install_launchd(
    memfs_bin: &std::path::Path,
    mount_path: &std::path::Path,
    db_path: &std::path::Path,
) -> anyhow::Result<()> {
    let label = "com.memfs.mount";
    let plist_dir = home_dir().join("Library/LaunchAgents");
    std::fs::create_dir_all(&plist_dir)?;
    let plist_path = plist_dir.join(format!("{}.plist", label));

    // Unload existing service if present (suppress error on fresh install)
    let _ = std::process::Command::new("launchctl")
        .args(["unload", "-w"])
        .arg(&plist_path)
        .stderr(std::process::Stdio::null())
        .status();

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>mount</string>
        <string>-f</string>
        <string>{mount}</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>MEMFS_DB</key>
        <string>{db}</string>
    </dict>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/memfs.out.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/memfs.err.log</string>
    <key>ThrottleInterval</key>
    <integer>5</integer>
</dict>
</plist>
"#,
        label = label,
        bin = memfs_bin.display(),
        mount = mount_path.display(),
        db = db_path.display(),
    );

    std::fs::write(&plist_path, plist)?;

    let status = std::process::Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .status()?;

    if !status.success() {
        anyhow::bail!("Failed to load launchd service");
    }

    eprintln!("Installed launchd service (auto-restarts on crash, starts on login)");
    Ok(())
}

fn install_systemd(
    memfs_bin: &std::path::Path,
    mount_path: &std::path::Path,
    db_path: &std::path::Path,
) -> anyhow::Result<()> {
    let unit_dir = home_dir().join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir)?;
    let unit_path = unit_dir.join("memfs.service");

    let unit = format!(
        "[Unit]\nDescription=MemFS FUSE mount\n\n[Service]\nExecStart={bin} mount -f {mount}\nEnvironment=MEMFS_DB={db}\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        bin = memfs_bin.display(),
        mount = mount_path.display(),
        db = db_path.display(),
    );

    std::fs::write(&unit_path, unit)?;

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", "memfs"])
        .status();

    eprintln!("Installed systemd user service (auto-restarts on crash, starts on login)");
    Ok(())
}

/// Kill any existing memfs mount processes and unmount the given path.
fn stop_mount(mount_path: &std::path::Path) {
    if let Ok(output) = std::process::Command::new("pgrep")
        .args(["-f", &format!("memfs mount.*{}", mount_path.display())])
        .output()
    {
        for pid in String::from_utf8_lossy(&output.stdout).lines() {
            let _ = std::process::Command::new("kill").arg(pid.trim()).status();
        }
    }
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("umount")
            .arg(mount_path)
            .stderr(std::process::Stdio::null())
            .status();
    } else {
        let _ = std::process::Command::new("fusermount")
            .arg("-u")
            .arg(mount_path)
            .stderr(std::process::Stdio::null())
            .status();
    }
    std::thread::sleep(std::time::Duration::from_secs(1));
}

/// Get the path to the current running binary.
fn dirs_self() -> std::path::PathBuf {
    std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("memfs"))
}

fn main() {
    let cli = Cli::parse();

    // Init/Mount/Unmount run outside tokio — they manage their own runtime
    match cli.command {
        Commands::Init => {
            if let Err(e) = init() {
                eprintln!("{}", e);
                std::process::exit(1);
            }
            return;
        }
        Commands::Sync => {
            if let Err(e) = sync_cmd() {
                eprintln!("{}", e);
                std::process::exit(1);
            }
            return;
        }
        Commands::Update => {
            if let Err(e) = update() {
                eprintln!("{}", e);
                std::process::exit(1);
            }
            return;
        }
        Commands::Uninstall { purge } => {
            if let Err(e) = uninstall(purge) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
            return;
        }
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
    let db_p = db_path();

    let database = match db::open(&db_p).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("memfs: failed to open database: {}", e);
            std::process::exit(1);
        }
    };
    let conn = match database.connect() {
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

    let settings = settings::load(&db_p);

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
        state_path(),
        mount_point(),
        #[cfg(feature = "search")]
        embedder,
        #[cfg(feature = "search")]
        settings.autotag_threshold,
        #[cfg(feature = "search")]
        settings.autotag_min_memories,
    );

    let result = match command {
        Commands::Cd { path } => {
            let mount = mount_point();
            let target = path.as_deref().unwrap_or(&mount);
            eng.cd(target).await
        }
        Commands::Ls { path, long } => {
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
        Commands::Rm { target, recursive } => match eng.rm(&target, recursive).await {
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
            if query.trim().is_empty() {
                eprintln!("memfs: search: query cannot be empty");
                std::process::exit(1);
            }
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
        Commands::Sync | Commands::Init | Commands::Update | Commands::Uninstall { .. } | Commands::Mount { .. } | Commands::Unmount { .. } => {
            unreachable!()
        }
    };

    if let Err(e) = result {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}
