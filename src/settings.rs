use std::path::Path;

use crate::util;

#[cfg(feature = "search")]
const DEFAULT_SEARCH_THRESHOLD: f32 = 0.3;
#[cfg(feature = "search")]
const DEFAULT_SEARCH_LIMIT: usize = 10;
#[cfg(feature = "search")]
const DEFAULT_AUTOTAG_THRESHOLD: f32 = 0.5;
#[cfg(feature = "search")]
const DEFAULT_AUTOTAG_MIN_MEMORIES: usize = 3;

/// MemFS settings loaded from .memfs/settings.json.
pub struct Settings {
    pub turso_url: Option<String>,
    pub turso_token: Option<String>,
    #[cfg(feature = "search")]
    pub search_threshold: f32,
    #[cfg(feature = "search")]
    pub search_limit: usize,
    #[cfg(feature = "search")]
    pub autotag_threshold: f32,
    #[cfg(feature = "search")]
    pub autotag_min_memories: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            turso_url: None,
            turso_token: None,
            #[cfg(feature = "search")]
            search_threshold: DEFAULT_SEARCH_THRESHOLD,
            #[cfg(feature = "search")]
            search_limit: DEFAULT_SEARCH_LIMIT,
            #[cfg(feature = "search")]
            autotag_threshold: DEFAULT_AUTOTAG_THRESHOLD,
            #[cfg(feature = "search")]
            autotag_min_memories: DEFAULT_AUTOTAG_MIN_MEMORIES,
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
        Ok(c) if c.contains('{') => c,
        Ok(_) => {
            eprintln!("warning: settings.json is not valid JSON, using defaults");
            return Settings::default();
        }
        Err(_) => return Settings::default(),
    };

    Settings {
        turso_url: extract_string(&content, "turso_url"),
        turso_token: extract_string(&content, "turso_token"),
        #[cfg(feature = "search")]
        search_threshold: extract_number(&content, "search_threshold")
            .filter(|&t: &f32| (0.0..=1.0).contains(&t))
            .unwrap_or(DEFAULT_SEARCH_THRESHOLD),
        #[cfg(feature = "search")]
        search_limit: extract_number::<usize>(&content, "search_limit")
            .unwrap_or(DEFAULT_SEARCH_LIMIT),
        #[cfg(feature = "search")]
        autotag_threshold: extract_number(&content, "autotag_threshold")
            .filter(|&t: &f32| (0.0..=1.0).contains(&t))
            .unwrap_or(DEFAULT_AUTOTAG_THRESHOLD),
        #[cfg(feature = "search")]
        autotag_min_memories: extract_number::<usize>(&content, "autotag_min_memories")
            .unwrap_or(DEFAULT_AUTOTAG_MIN_MEMORIES),
    }
}

/// Find the JSON value for a given key, returning the trimmed text after the colon.
fn find_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\"", key);
    let idx = json.find(&pattern)?;
    let after_key = &json[idx + pattern.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    Some(after_colon.trim_start())
}

fn extract_string(json: &str, key: &str) -> Option<String> {
    let value = find_value(json, key)?;
    let after_quote = value.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    // Strip internal whitespace (handles multi-line JSON values like long tokens)
    let raw = &after_quote[..end];
    Some(raw.chars().filter(|c| !c.is_whitespace()).collect())
}

#[cfg(feature = "search")]
fn extract_number<T: std::str::FromStr>(json: &str, key: &str) -> Option<T> {
    let value = find_value(json, key)?;
    let end = value
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(value.len());
    value[..end].parse().ok()
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
        assert_eq!(extract_number::<usize>(json, "search_limit").unwrap(), 20);
    }

    #[test]
    fn missing_key_returns_none() {
        let json = r#"{"other": "val"}"#;
        assert!(extract_string(json, "turso_url").is_none());
        assert!(extract_number::<f32>(json, "search_threshold").is_none());
    }

    #[test]
    fn multiline_string_value() {
        let json = "{\n  \"turso_token\": \"eyJhb\n  GciOi\n  JFZ\"\n}";
        assert_eq!(extract_string(json, "turso_token").unwrap(), "eyJhbGciOiJFZ");
    }

    #[test]
    fn defaults_when_no_file() {
        let settings = load("/nonexistent/.memfs/db");
        assert_eq!(settings.search_threshold, 0.3);
        assert_eq!(settings.search_limit, 10);
        assert!(settings.turso_url.is_none());
    }

    #[test]
    fn invalid_threshold_uses_default() {
        // > 1.0
        let val = extract_number::<f32>(r#"{"search_threshold": 1.5}"#, "search_threshold")
            .filter(|&t| (0.0..=1.0).contains(&t));
        assert!(val.is_none());
        // < 0.0
        let val = extract_number::<f32>(r#"{"search_threshold": -0.1}"#, "search_threshold")
            .filter(|&t| (0.0..=1.0).contains(&t));
        assert!(val.is_none());
    }
}
