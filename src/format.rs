use crate::engine::LsEntry;
use crate::queries::{GrepResult, Memory};

/// Format `ls` output in short (columnar) mode.
pub fn format_ls(entries: &[LsEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let names: Vec<String> = entries
        .iter()
        .map(|e| {
            if e.is_dir {
                format!("{}/", e.name)
            } else {
                e.name.clone()
            }
        })
        .collect();

    // Simple space-separated output (like `ls` with columns)
    let mut output = String::new();
    let max_width = 80;
    let col_width = names.iter().map(|n| n.len()).max().unwrap_or(0) + 2;
    let cols = (max_width / col_width).max(1);

    for (i, name) in names.iter().enumerate() {
        if i > 0 && i % cols == 0 {
            output.push('\n');
        }
        output.push_str(&format!("{:<width$}", name, width = col_width));
    }

    output.trim_end().to_string()
}

/// Format `ls -l` output in long mode.
pub fn format_ls_long(entries: &[LsEntry]) -> String {
    if entries.is_empty() {
        return String::new();
    }

    let mut lines = Vec::new();
    for e in entries {
        if e.is_dir {
            lines.push(format!("drwxr-xr-x  -  -  {}/", e.name));
        } else {
            let updated = e.updated_at.as_deref().unwrap_or("-");
            // Trim to just date+time
            let date = if updated.len() >= 16 {
                &updated[..16]
            } else {
                updated
            };
            lines.push(format!(
                "-rw-r--r--  {}  {:>5}  {}",
                date,
                e.content_len,
                e.name
            ));
        }
    }

    lines.join("\n")
}

/// Format `cat` output — tags header + content.
pub fn format_cat(memory: &Memory) -> String {
    let tags_str: Vec<String> = memory
        .tags
        .iter()
        .map(|t| format!("{}:{}", t.facet, t.value))
        .collect();

    format!("--- tags: {} ---\n{}", tags_str.join(", "), memory.content)
}

/// Format `grep` results.
pub fn format_grep(results: &[GrepResult], files_only: bool, line_numbers: bool) -> String {
    if results.is_empty() {
        return String::new();
    }

    if files_only {
        let mut seen = std::collections::HashSet::new();
        let mut lines = Vec::new();
        for r in results {
            if seen.insert(&r.filename) {
                lines.push(r.filename.clone());
            }
        }
        return lines.join("\n");
    }

    let lines: Vec<String> = results
        .iter()
        .map(|r| {
            if line_numbers {
                format!("{}:{}:{}", r.filename, r.line_number, r.line)
            } else {
                format!("{}:{}", r.filename, r.line)
            }
        })
        .collect();

    lines.join("\n")
}

/// Format `find` results — one path per line.
pub fn format_find(paths: &[String]) -> String {
    paths.join("\n")
}
