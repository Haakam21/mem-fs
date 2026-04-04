use anyhow::{bail, Result};

/// A facet:value filter pair (e.g., people:sister, dates:2025-03).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Filter {
    pub facet: String,
    pub value: String,
}

/// A parsed virtual path broken into its facet filters and optional trailing facet.
#[derive(Debug, Clone)]
pub struct ParsedPath {
    /// Completed facet:value pairs extracted from the path.
    pub filters: Vec<Filter>,
    /// If the path ends at a facet category level (odd segment count after mount),
    /// this holds the trailing facet name. None if at root or a value level.
    pub trailing_facet: Option<String>,
}

impl ParsedPath {
    #[cfg(test)]
    pub fn equivalent(&self, other: &ParsedPath) -> bool {
        use std::collections::HashSet;
        if self.filters.len() != other.filters.len() {
            return false;
        }
        if self.trailing_facet != other.trailing_facet {
            return false;
        }
        let a: HashSet<&Filter> = self.filters.iter().collect();
        let b: HashSet<&Filter> = other.filters.iter().collect();
        a == b
    }

    /// Returns true if this path is at the virtual root (/memories with no filters).
    pub fn is_root(&self) -> bool {
        self.filters.is_empty() && self.trailing_facet.is_none()
    }

    /// Returns true if at a facet category level (browsing values).
    pub fn is_facet_level(&self) -> bool {
        self.trailing_facet.is_some()
    }

    #[cfg(test)]
    pub fn is_value_level(&self) -> bool {
        !self.filters.is_empty() && self.trailing_facet.is_none()
    }

}

/// Parse an absolute virtual path into filters and an optional trailing facet.
///
/// Examples:
/// - `/memories` → root (no filters, no trailing facet)
/// - `/memories/people` → trailing_facet = "people", no filters
/// - `/memories/people/sister` → filters = [{people, sister}], no trailing facet
/// - `/memories/people/sister/dates` → filters = [{people, sister}], trailing_facet = "dates"
/// - `/memories/people/sister/dates/2025-03` → filters = [{people, sister}, {dates, 2025-03}]
pub fn parse(absolute_path: &str, mount_point: &str) -> Result<ParsedPath> {
    let normalized = normalize(absolute_path);
    let mount_normalized = normalize(mount_point);

    if !normalized.starts_with(&mount_normalized) {
        bail!(
            "path '{}' is not inside mount point '{}'",
            absolute_path,
            mount_point
        );
    }

    // Get the portion after the mount point
    let remainder = &normalized[mount_normalized.len()..];
    let remainder = remainder.trim_start_matches('/');

    if remainder.is_empty() {
        return Ok(ParsedPath {
            filters: vec![],
            trailing_facet: None,
        });
    }

    let segments: Vec<&str> = remainder.split('/').filter(|s| !s.is_empty()).collect();

    let mut filters = Vec::new();
    let mut trailing_facet = None;
    let mut i = 0;

    while i < segments.len() {
        if i + 1 < segments.len() {
            // Complete pair: facet/value
            filters.push(Filter {
                facet: segments[i].to_string(),
                value: segments[i + 1].to_string(),
            });
            i += 2;
        } else {
            // Odd trailing segment: facet category level
            trailing_facet = Some(segments[i].to_string());
            i += 1;
        }
    }

    Ok(ParsedPath {
        filters,
        trailing_facet,
    })
}

/// Resolve a possibly-relative path against the current virtual CWD,
/// producing an absolute virtual path.
///
/// Rules:
/// - Absolute paths starting with `mount_point` are used directly
/// - Relative paths are resolved against `current_cwd`
/// - `..` pops the last segment
/// - `.` is identity
/// - Paths outside the mount point return an error
pub fn resolve(input: &str, current_cwd: &str, mount_point: &str) -> Result<String> {
    let mount_normalized = normalize(mount_point);

    // Absolute path
    if input.starts_with('/') {
        let normalized = normalize(input);
        if normalized.starts_with(&mount_normalized) {
            return Ok(normalized);
        }
        // Path outside virtual FS
        bail!("path '{}' is outside the virtual filesystem", input);
    }

    // Relative path — resolve against current CWD
    if current_cwd.is_empty() {
        bail!("no virtual working directory set; cannot resolve relative path '{}'", input);
    }

    let mut segments: Vec<&str> = current_cwd
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();

    for part in input.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                // Don't pop past the mount point root
                let mount_depth = mount_normalized
                    .trim_start_matches('/')
                    .split('/')
                    .filter(|s| !s.is_empty())
                    .count();
                if segments.len() > mount_depth {
                    segments.pop();
                }
            }
            segment => segments.push(segment),
        }
    }

    let result = format!("/{}", segments.join("/"));
    let normalized = normalize(&result);

    if !normalized.starts_with(&mount_normalized) {
        bail!("resolved path '{}' is outside the virtual filesystem", normalized);
    }

    Ok(normalized)
}

/// Normalize a path: remove trailing slashes, collapse double slashes.
fn normalize(path: &str) -> String {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if path.starts_with('/') {
        format!("/{}", segments.join("/"))
    } else {
        segments.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNT: &str = "/memories";

    // --- parse tests ---

    #[test]
    fn parse_root() {
        let p = parse("/memories", MOUNT).unwrap();
        assert!(p.filters.is_empty());
        assert!(p.trailing_facet.is_none());
        assert!(p.is_root());
    }

    #[test]
    fn parse_root_trailing_slash() {
        let p = parse("/memories/", MOUNT).unwrap();
        assert!(p.is_root());
    }

    #[test]
    fn parse_facet_level() {
        let p = parse("/memories/people", MOUNT).unwrap();
        assert!(p.filters.is_empty());
        assert_eq!(p.trailing_facet.as_deref(), Some("people"));
        assert!(p.is_facet_level());
    }

    #[test]
    fn parse_single_filter() {
        let p = parse("/memories/people/sister", MOUNT).unwrap();
        assert_eq!(p.filters.len(), 1);
        assert_eq!(p.filters[0].facet, "people");
        assert_eq!(p.filters[0].value, "sister");
        assert!(p.trailing_facet.is_none());
        assert!(p.is_value_level());
    }

    #[test]
    fn parse_filter_plus_trailing_facet() {
        let p = parse("/memories/people/sister/dates", MOUNT).unwrap();
        assert_eq!(p.filters.len(), 1);
        assert_eq!(p.filters[0].facet, "people");
        assert_eq!(p.filters[0].value, "sister");
        assert_eq!(p.trailing_facet.as_deref(), Some("dates"));
    }

    #[test]
    fn parse_two_filters() {
        let p = parse("/memories/people/sister/dates/2025-03", MOUNT).unwrap();
        assert_eq!(p.filters.len(), 2);
        assert_eq!(p.filters[0].facet, "people");
        assert_eq!(p.filters[0].value, "sister");
        assert_eq!(p.filters[1].facet, "dates");
        assert_eq!(p.filters[1].value, "2025-03");
        assert!(p.trailing_facet.is_none());
    }

    #[test]
    fn parse_outside_mount_fails() {
        assert!(parse("/home/user", MOUNT).is_err());
    }

    // --- equivalence tests ---

    #[test]
    fn equivalent_same_order() {
        let a = parse("/memories/people/sister/dates/2025-03", MOUNT).unwrap();
        let b = parse("/memories/people/sister/dates/2025-03", MOUNT).unwrap();
        assert!(a.equivalent(&b));
    }

    #[test]
    fn equivalent_different_order() {
        let a = parse("/memories/people/sister/dates/2025-03", MOUNT).unwrap();
        let b = parse("/memories/dates/2025-03/people/sister", MOUNT).unwrap();
        assert!(a.equivalent(&b));
    }

    #[test]
    fn not_equivalent_different_filters() {
        let a = parse("/memories/people/sister", MOUNT).unwrap();
        let b = parse("/memories/people/mom", MOUNT).unwrap();
        assert!(!a.equivalent(&b));
    }

    #[test]
    fn equivalent_with_trailing_facet() {
        let a = parse("/memories/people/sister/dates", MOUNT).unwrap();
        let b = parse("/memories/dates", MOUNT).unwrap();
        // Different filter counts → not equivalent
        assert!(!a.equivalent(&b));
    }

    // --- resolve tests ---

    #[test]
    fn resolve_absolute_virtual() {
        let result = resolve("/memories/people/sister", "/memories/people", MOUNT).unwrap();
        assert_eq!(result, "/memories/people/sister");
    }

    #[test]
    fn resolve_relative_simple() {
        let result = resolve("sister", "/memories/people", MOUNT).unwrap();
        assert_eq!(result, "/memories/people/sister");
    }

    #[test]
    fn resolve_relative_nested() {
        let result = resolve("dates/2025-03", "/memories/people/sister", MOUNT).unwrap();
        assert_eq!(result, "/memories/people/sister/dates/2025-03");
    }

    #[test]
    fn resolve_dotdot() {
        let result = resolve("..", "/memories/people/sister", MOUNT).unwrap();
        assert_eq!(result, "/memories/people");
    }

    #[test]
    fn resolve_dotdot_at_root() {
        let result = resolve("..", "/memories", MOUNT).unwrap();
        // Should not pop past mount point
        assert_eq!(result, "/memories");
    }

    #[test]
    fn resolve_dotdot_then_down() {
        let result = resolve("../dates/2025-03", "/memories/people/sister", MOUNT).unwrap();
        assert_eq!(result, "/memories/people/dates/2025-03");
    }

    #[test]
    fn resolve_dot() {
        let result = resolve(".", "/memories/people", MOUNT).unwrap();
        assert_eq!(result, "/memories/people");
    }

    #[test]
    fn resolve_outside_virtual_fails() {
        assert!(resolve("/home/user", "/memories/people", MOUNT).is_err());
    }

    #[test]
    fn resolve_no_cwd_fails() {
        assert!(resolve("sister", "", MOUNT).is_err());
    }

    // --- normalize tests ---

    #[test]
    fn normalize_trailing_slash() {
        assert_eq!(normalize("/memories/"), "/memories");
    }

    #[test]
    fn normalize_double_slash() {
        assert_eq!(normalize("/memories//people"), "/memories/people");
    }
}
