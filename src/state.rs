use anyhow::Result;
use std::fs;
use std::path::Path;

use crate::util;

/// Read the current virtual working directory from the state file.
/// Returns `None` if the file doesn't exist or is empty (not in virtual FS).
pub fn read(state_path: &str) -> Result<Option<String>> {
    let path = util::expand_tilde(state_path);
    if !Path::new(&path).exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)?;
    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed))
    }
}

/// Write the current virtual working directory to the state file.
pub fn write(state_path: &str, cwd: &str) -> Result<()> {
    let path = util::expand_tilde(state_path);
    if let Some(parent) = Path::new(&path).parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, cwd)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_state_path() -> String {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir();
        let file = dir.join(format!(
            "memfs_test_state_{}_{}",
            std::process::id(),
            n
        ));
        file.to_string_lossy().to_string()
    }

    #[test]
    fn read_nonexistent_returns_none() {
        let path = temp_state_path();
        assert!(read(&path).unwrap().is_none());
    }

    #[test]
    fn write_then_read() {
        let path = temp_state_path();
        write(&path, "/memories/people/sister").unwrap();
        let result = read(&path).unwrap();
        assert_eq!(result.as_deref(), Some("/memories/people/sister"));
        std::fs::remove_file(&path).ok();
    }
}
