use std::path::{Path, PathBuf};

use crate::util;

/// MemFS settings loaded from .memfs/settings.json.
pub struct Settings {
    pub turso_url: Option<String>,
    pub turso_token: Option<String>,
    pub search_threshold: f32,
    pub search_limit: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            turso_url: None,
            turso_token: None,
            search_threshold: 0.3,
            search_limit: 10,
        }
    }
}

/// Load settings from .memfs/settings.json next to the given DB path,
/// falling back to defaults for any missing values.
pub fn load(db_path: &str) -> Settings {
    let path = util::expand_tilde(db_path);
    let config_path = match Path::new(&path).parent() {
        Some(p) => p.join("settings.json"),
        None => return Settings::default(),
    };

    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return Settings::default(),
    };

    let defaults = Settings::default();
    Settings {
        turso_url: extract_string(&content, "turso_url"),
        turso_token: extract_string(&content, "turso_token"),
        search_threshold: extract_number(&content, "search_threshold")
            .unwrap_or(defaults.search_threshold),
        search_limit: extract_number::<f32>(&content, "search_limit")
            .map(|n| n as usize)
            .unwrap_or(defaults.search_limit),
    }
}

/// Find .memfs/settings.json by walking up from a directory.
pub fn load_from_dir(start_dir: &Path) -> Settings {
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join(".memfs").join("settings.json");
        if candidate.exists() {
            let db_path = dir.join(".memfs").join("db");
            return load(db_path.to_str().unwrap_or(""));
        }
        if !dir.pop() {
            return Settings::default();
        }
    }
}

fn extract_string(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let idx = json.find(&pattern)?;
    let after_key = &json[idx + pattern.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();
    let after_quote = after_colon.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    Some(after_quote[..end].to_string())
}

fn extract_number<T: std::str::FromStr>(json: &str, key: &str) -> Option<T> {
    let pattern = format!("\"{}\"", key);
    let idx = json.find(&pattern)?;
    let after_key = &json[idx + pattern.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_colon = after_colon.trim_start();
    // Extract number (digits, dots, minus)
    let end = after_colon
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(after_colon.len());
    after_colon[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_string_value() {
        let json = r#"{"turso_url": "libsql://test.turso.io", "other": "val"}"#;
        assert_eq!(
            extract_string(json, "turso_url").unwrap(),
            "libsql://test.turso.io"
        );
    }

    #[test]
    fn extract_number_float() {
        let json = r#"{"search_threshold": 0.5, "other": 1}"#;
        assert_eq!(extract_number::<f32>(json, "search_threshold").unwrap(), 0.5);
    }

    #[test]
    fn extract_number_int() {
        let json = r#"{"search_limit": 20}"#;
        assert_eq!(extract_number::<f32>(json, "search_limit").unwrap(), 20.0);
    }

    #[test]
    fn missing_key_returns_none() {
        let json = r#"{"other": "val"}"#;
        assert!(extract_string(json, "turso_url").is_none());
        assert!(extract_number::<f32>(json, "search_threshold").is_none());
    }

    #[test]
    fn defaults_when_no_file() {
        let settings = load("/nonexistent/.memfs/db");
        assert_eq!(settings.search_threshold, 0.3);
        assert_eq!(settings.search_limit, 10);
        assert!(settings.turso_url.is_none());
    }
}
