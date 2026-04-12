/// OS/editor junk files that should never be stored as memories.
pub fn is_junk_file(name: &str) -> bool {
    name.starts_with("._") || name.starts_with(".#") || name.contains(".tmp.") || name.ends_with('~')
}

/// Expand `~` at the start of a path to the user's home directory.
pub fn expand_tilde(path: &str) -> String {
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
