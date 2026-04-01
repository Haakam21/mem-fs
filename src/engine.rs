use anyhow::{bail, Result};
use turso::Connection;

use crate::path::{self, Filter, ParsedPath};
use crate::queries::{self, Memory};
use crate::state;

/// Core engine that ties together path parsing, state management, and database queries.
pub struct Engine {
    pub conn: Connection,
    pub state_path: String,
    pub mount_point: String,
}

/// An entry returned by `ls` — either a directory (facet/value) or a file (memory).
#[derive(Debug)]
pub struct LsEntry {
    pub name: String,
    pub is_dir: bool,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub tag_count: usize,
    pub content_len: usize,
}

impl Engine {
    pub fn new(conn: Connection, state_path: String, mount_point: String) -> Self {
        Self {
            conn,
            state_path,
            mount_point,
        }
    }

    /// Get the current parsed path from state.
    pub fn current_path(&self) -> Result<Option<ParsedPath>> {
        match state::read(&self.state_path)? {
            Some(cwd) => Ok(Some(path::parse(&cwd, &self.mount_point)?)),
            None => Ok(None),
        }
    }

    /// Get the current virtual CWD string, or None if not in virtual FS.
    pub fn current_cwd(&self) -> Result<Option<String>> {
        state::read(&self.state_path)
    }

    /// Resolve a path argument (absolute or relative) to an absolute virtual path.
    pub fn resolve_path(&self, input: &str) -> Result<String> {
        let cwd = self.current_cwd()?.unwrap_or_default();
        path::resolve(input, &cwd, &self.mount_point)
    }

    // --- Navigation ---

    /// Change directory. Validates the path exists before updating state.
    pub async fn cd(&self, target: &str) -> Result<()> {
        let resolved = self.resolve_path(target)?;
        let parsed = path::parse(&resolved, &self.mount_point)?;

        // Validate: each facet must exist, each value must exist for its facet
        for filter in &parsed.filters {
            if !queries::facet_exists(&self.conn, &filter.facet).await? {
                bail!("memfs: cd: no such facet or value: '{}'", filter.facet);
            }
            if !queries::value_exists(&self.conn, &filter.facet, &filter.value).await? {
                bail!("memfs: cd: no such facet or value: '{}'", filter.value);
            }
        }
        if let Some(ref facet) = parsed.trailing_facet {
            if !queries::facet_exists(&self.conn, facet).await? {
                bail!("memfs: cd: no such facet or value: '{}'", facet);
            }
        }

        state::write(&self.state_path, &resolved)?;
        Ok(())
    }

    /// List contents at the given path (or current CWD).
    pub async fn ls(&self, target: Option<&str>) -> Result<Vec<LsEntry>> {
        let resolved = match target {
            Some(t) => self.resolve_path(t)?,
            None => self
                .current_cwd()?
                .unwrap_or_else(|| self.mount_point.clone()),
        };
        let parsed = path::parse(&resolved, &self.mount_point)?;

        let mut entries = Vec::new();

        if parsed.is_root() {
            // At root: show all facet categories as directories
            let facets = queries::list_facets(&self.conn).await?;
            for f in facets {
                entries.push(LsEntry {
                    name: f,
                    is_dir: true,
                    created_at: None,
                    updated_at: None,
                    tag_count: 0,
                    content_len: 0,
                });
            }
        } else if parsed.is_facet_level() {
            // At facet category: show values for this facet (scoped by existing filters)
            let facet = parsed.trailing_facet.as_ref().unwrap();
            let values = queries::list_values(&self.conn, facet, &parsed.filters).await?;
            for v in values {
                entries.push(LsEntry {
                    name: v,
                    is_dir: true,
                    created_at: None,
                    updated_at: None,
                    tag_count: 0,
                    content_len: 0,
                });
            }
        } else {
            // At filter level: show remaining facets as dirs + matching memories as files
            let remaining = queries::remaining_facets(&self.conn, &parsed.filters).await?;
            for f in remaining {
                entries.push(LsEntry {
                    name: f,
                    is_dir: true,
                    created_at: None,
                    updated_at: None,
                    tag_count: 0,
                    content_len: 0,
                });
            }

            let memories = queries::list_memories(&self.conn, &parsed.filters).await?;
            for m in memories {
                entries.push(LsEntry {
                    name: m.filename,
                    is_dir: false,
                    created_at: Some(m.created_at),
                    updated_at: Some(m.updated_at),
                    tag_count: m.tags.len(),
                    content_len: m.content.len(),
                });
            }
        }

        Ok(entries)
    }

    /// Print current working directory.
    pub fn pwd(&self) -> Result<String> {
        match self.current_cwd()? {
            Some(cwd) => Ok(cwd),
            None => bail!("memfs: not in virtual filesystem"),
        }
    }

    /// Display a memory's content.
    pub async fn cat(&self, filename: &str) -> Result<Memory> {
        let parsed = self.current_path()?;
        let filters: Vec<Filter> = match &parsed {
            Some(p) => p.filters.clone(),
            None => vec![],
        };
        match queries::get_memory(&self.conn, filename, &filters).await? {
            Some(m) => Ok(m),
            None => bail!("memfs: cat: {}: No such memory", filename),
        }
    }

    // --- Mutation ---

    /// Create facet categories and/or values.
    /// For values, we insert a placeholder tag (memory_id=0) so they show up in navigation.
    pub async fn mkdir(&self, target: &str, parents: bool) -> Result<()> {
        let resolved = self.resolve_path(target)?;
        let parsed = path::parse(&resolved, &self.mount_point)?;

        if parents {
            // Create all facets and register all values in the path
            for filter in &parsed.filters {
                queries::create_facet(&self.conn, &filter.facet).await?;
                queries::ensure_value(&self.conn, &filter.facet, &filter.value).await?;
            }
            if let Some(ref facet) = parsed.trailing_facet {
                queries::create_facet(&self.conn, facet).await?;
            }
        } else if parsed.is_facet_level() {
            // Creating a facet category
            let facet = parsed.trailing_facet.as_ref().unwrap();
            queries::create_facet(&self.conn, facet).await?;
        } else if !parsed.filters.is_empty() {
            // Creating a value under a facet
            let last = parsed.filters.last().unwrap();
            if !queries::facet_exists(&self.conn, &last.facet).await? {
                bail!(
                    "memfs: mkdir: cannot create '{}': facet '{}' does not exist (use -p)",
                    target,
                    last.facet
                );
            }
            queries::ensure_value(&self.conn, &last.facet, &last.value).await?;
        } else {
            bail!("memfs: mkdir: cannot create '{}'", target);
        }

        Ok(())
    }

    /// Remove a memory or untag a facet value.
    pub async fn rm(&self, target: &str, recursive: bool) -> Result<String> {
        let resolved = self.resolve_path(target)?;
        let parsed = path::parse(&resolved, &self.mount_point)?;

        if recursive {
            // rm -r /memories/facet/value → untag all memories from this value
            if let Some(last) = parsed.filters.last() {
                let count =
                    queries::untag_all(&self.conn, &last.facet, &last.value).await?;
                return Ok(format!(
                    "Removed tag {}:{} from {} memories",
                    last.facet, last.value, count
                ));
            }
            bail!("memfs: rm: cannot remove root");
        }

        // Non-recursive: target should be a filename
        // Extract filename from the path (last segment if it looks like a file)
        let filename = resolved
            .rsplit('/')
            .next()
            .unwrap_or("");

        let filters: Vec<Filter> = match self.current_path()? {
            Some(p) => p.filters.clone(),
            None => vec![],
        };

        match queries::get_memory(&self.conn, filename, &filters).await? {
            Some(m) => {
                queries::delete_memory(&self.conn, m.id).await?;
                Ok(format!("Deleted '{}'", filename))
            }
            None => bail!("memfs: rm: '{}': No such memory", filename),
        }
    }

    /// Retag a memory: move from one facet:value to another.
    pub async fn mv(&self, source: &str, dest: &str) -> Result<()> {
        let src_resolved = self.resolve_path(source)?;
        let dst_resolved = self.resolve_path(dest)?;
        let src_parsed = path::parse(&src_resolved, &self.mount_point)?;
        let dst_parsed = path::parse(&dst_resolved, &self.mount_point)?;

        // Extract filename from source (last segment)
        let filename = src_resolved.rsplit('/').next().unwrap_or("");
        if filename.is_empty() {
            bail!("memfs: mv: missing filename in source path");
        }

        // Find the memory
        let mem = queries::get_memory(&self.conn, filename, &src_parsed.filters)
            .await?
            .ok_or_else(|| anyhow::anyhow!("memfs: mv: '{}': No such memory", filename))?;

        // Determine what changed between source and dest paths
        // Find the differing filter
        let src_filters: std::collections::HashSet<(String, String)> = src_parsed
            .filters
            .iter()
            .map(|f| (f.facet.clone(), f.value.clone()))
            .collect();
        let dst_filters: std::collections::HashSet<(String, String)> = dst_parsed
            .filters
            .iter()
            .map(|f| (f.facet.clone(), f.value.clone()))
            .collect();

        // Tags to remove (in source but not dest)
        for (facet, value) in src_filters.difference(&dst_filters) {
            queries::remove_tag(&self.conn, mem.id, facet, value).await?;
        }
        // Tags to add (in dest but not source)
        for (facet, value) in dst_filters.difference(&src_filters) {
            queries::add_tag(&self.conn, mem.id, facet, value).await?;
        }

        Ok(())
    }

    /// Add an additional tag to a memory (copy to a new facet path).
    pub async fn cp(&self, source: &str, dest: &str) -> Result<()> {
        let src_resolved = self.resolve_path(source)?;
        let dst_resolved = self.resolve_path(dest)?;
        let src_parsed = path::parse(&src_resolved, &self.mount_point)?;
        let dst_parsed = path::parse(&dst_resolved, &self.mount_point)?;

        let filename = src_resolved.rsplit('/').next().unwrap_or("");
        if filename.is_empty() {
            bail!("memfs: cp: missing filename in source path");
        }

        let mem = queries::get_memory(&self.conn, filename, &src_parsed.filters)
            .await?
            .ok_or_else(|| anyhow::anyhow!("memfs: cp: '{}': No such memory", filename))?;

        // Add all destination tags that aren't already present
        for filter in &dst_parsed.filters {
            queries::add_tag(&self.conn, mem.id, &filter.facet, &filter.value).await?;
        }

        Ok(())
    }

    /// Create a new memory with content, auto-tagged from current CWD.
    /// Also ensures all facets/values in the current path exist.
    pub async fn write(&self, filename: &str, content: &str) -> Result<()> {
        let tags: Vec<Filter> = match self.current_path()? {
            Some(p) => p.filters.clone(),
            None => vec![],
        };
        // Ensure facets and values exist
        for tag in &tags {
            queries::create_facet(&self.conn, &tag.facet).await?;
            queries::ensure_value(&self.conn, &tag.facet, &tag.value).await?;
        }
        queries::create_memory(&self.conn, filename, content, &tags).await?;
        Ok(())
    }

    /// Append content to an existing memory.
    pub async fn append(&self, filename: &str, content: &str) -> Result<()> {
        let filters: Vec<Filter> = match self.current_path()? {
            Some(p) => p.filters.clone(),
            None => vec![],
        };
        queries::append_memory(&self.conn, filename, content, &filters).await
    }

    // --- Search ---

    /// Grep memory content for a pattern.
    pub async fn grep(
        &self,
        pattern: &str,
        scope: Option<&str>,
        ignore_case: bool,
    ) -> Result<Vec<queries::GrepResult>> {
        let filters: Vec<Filter> = if let Some(scope_path) = scope {
            let resolved = self.resolve_path(scope_path)?;
            let parsed = path::parse(&resolved, &self.mount_point)?;
            parsed.filters
        } else {
            match self.current_path()? {
                Some(p) => p.filters.clone(),
                None => vec![],
            }
        };

        let memories = queries::list_memories(&self.conn, &filters).await?;

        let re = if ignore_case {
            regex::RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()?
        } else {
            regex::Regex::new(pattern)?
        };

        let mut results = Vec::new();
        for mem in &memories {
            for (i, line) in mem.content.lines().enumerate() {
                if re.is_match(line) {
                    results.push(queries::GrepResult {
                        filename: mem.filename.clone(),
                        line_number: i + 1,
                        line: line.to_string(),
                    });
                }
            }
        }

        Ok(results)
    }

    /// Find memories by filename pattern.
    pub async fn find(
        &self,
        scope: Option<&str>,
        name_pattern: Option<&str>,
        file_type: Option<&str>,
        mtime_days: Option<i64>,
    ) -> Result<Vec<String>> {
        let (filters, base_path) = if let Some(scope_path) = scope {
            let resolved = self.resolve_path(scope_path)?;
            let parsed = path::parse(&resolved, &self.mount_point)?;
            (parsed.filters, resolved)
        } else {
            let cwd = self.current_cwd()?.unwrap_or_else(|| self.mount_point.clone());
            let parsed = path::parse(&cwd, &self.mount_point)?;
            (parsed.filters, cwd)
        };

        let mut results = Vec::new();

        // -type d: list facets and values as directories
        if file_type == Some("d") {
            let facets = queries::list_facets(&self.conn).await?;
            for f in &facets {
                results.push(format!("{}/{}", base_path, f));
                let values = queries::list_values(&self.conn, f, &filters).await?;
                for v in values {
                    results.push(format!("{}/{}/{}", base_path, f, v));
                }
            }
            return Ok(results);
        }

        // Find by name pattern
        let pattern = name_pattern.unwrap_or("*");
        let memories = queries::find_memories(&self.conn, pattern, &filters).await?;

        let now = chrono::Utc::now();

        for mem in memories {
            // Apply mtime filter if specified
            if let Some(days) = mtime_days {
                if let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&mem.updated_at) {
                    let age = now.signed_duration_since(updated);
                    if days < 0 {
                        // -mtime -N: modified within last N days
                        if age.num_days() > -days {
                            continue;
                        }
                    } else {
                        // -mtime +N: modified more than N days ago
                        if age.num_days() < days {
                            continue;
                        }
                    }
                }
            }
            results.push(format!("{}/{}", self.mount_point, mem.filename));
        }

        Ok(results)
    }
}
