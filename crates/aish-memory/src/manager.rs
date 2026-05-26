use crate::models::MemoryEntry;
use aish_core::{AishError, MemoryCategory};
use chrono::Utc;
use std::io::Write;
use std::path::{Path, PathBuf};

const HEADER: &str = "# Memory\n";

/// Memory manager backed by a single MEMORY.md file.
pub struct MemoryManager {
    memory_file: PathBuf,
    entries: Vec<MemoryEntry>,
    next_id: i64,
}

impl MemoryManager {
    /// Create or open a memory file.
    pub fn new(memory_file: PathBuf) -> aish_core::Result<Self> {
        if !memory_file.exists() {
            // Create parent directories and the file with just the header
            if let Some(parent) = memory_file.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AishError::Memory(format!("cannot create directory {:?}: {}", parent, e))
                })?;
            }
            let mut f = std::fs::File::create(&memory_file)
                .map_err(|e| AishError::Memory(format!("cannot create memory file: {}", e)))?;
            f.write_all(HEADER.as_bytes())
                .map_err(|e| AishError::Memory(format!("cannot write memory header: {}", e)))?;
            return Ok(Self {
                memory_file,
                entries: Vec::new(),
                next_id: 1,
            });
        }

        let entries = parse_file(&memory_file)?;
        let next_id = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;

        Ok(Self {
            memory_file,
            entries,
            next_id,
        })
    }

    /// Return the default memory file path: `~/.config/aish/MEMORY.md`.
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aish")
            .join("MEMORY.md")
    }

    /// Store a new memory entry. Returns the assigned ID.
    /// If an entry with the same content and category already exists, returns
    /// the existing ID without creating a duplicate.
    pub fn store(
        &mut self,
        content: &str,
        category: MemoryCategory,
        source: &str,
        importance: f64,
    ) -> aish_core::Result<i64> {
        let content_trimmed = content.trim();

        // Duplicate detection: same content (case-insensitive) + same category
        let content_lower = content_trimmed.to_lowercase();
        for entry in &self.entries {
            if entry.category == category && entry.content.to_lowercase() == content_lower {
                return Ok(entry.id);
            }
        }

        let now = Utc::now().format("%Y-%m-%d").to_string();
        let id = self.next_id;
        self.next_id += 1;

        let entry = MemoryEntry {
            id,
            source: source.to_string(),
            category,
            content: content_trimmed.to_string(),
            importance,
            tags: String::new(),
            created_at: Some(now.clone()),
            last_accessed_at: Some(now),
            access_count: 0,
        };

        self.entries.push(entry);
        self.persist()?;
        Ok(id)
    }

    /// Recall memories matching a query, sorted by relevance.
    ///
    /// Relevance = (number of matching query words) * importance.
    /// Access stats are updated for matched entries.
    pub fn recall(&mut self, query: &str, limit: usize) -> Vec<&MemoryEntry> {
        let query_words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();

        if query_words.is_empty() {
            // Return entries sorted by importance when query is empty
            let mut indices: Vec<usize> = (0..self.entries.len()).collect();
            indices.sort_by(|a, b| {
                self.entries[*b]
                    .importance
                    .partial_cmp(&self.entries[*a].importance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            return indices
                .into_iter()
                .take(limit)
                .map(|i| &self.entries[i])
                .collect();
        }

        let now = Utc::now().format("%Y-%m-%d").to_string();

        // Compute relevance scores
        let mut scored: Vec<(usize, f64)> = Vec::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            let content_lower = entry.content.to_lowercase();
            let match_count = query_words
                .iter()
                .filter(|w| content_lower.contains(w.as_str()))
                .count() as f64;
            if match_count > 0.0 {
                let score = match_count * entry.importance;
                scored.push((idx, score));
            }
        }

        // Sort by score descending, then by importance as tie-breaker
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    self.entries[b.0]
                        .importance
                        .partial_cmp(&self.entries[a.0].importance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });

        // Update access stats for matched entries
        for (idx, _) in &scored {
            self.entries[*idx].access_count += 1;
            self.entries[*idx].last_accessed_at = Some(now.clone());
        }

        // Persist updated access stats (best-effort)
        let _ = self.persist();

        scored
            .into_iter()
            .take(limit)
            .map(|(idx, _)| &self.entries[idx])
            .collect()
    }

    /// Remove a memory entry by ID. Returns true if found and removed.
    pub fn remove(&mut self, id: i64) -> aish_core::Result<bool> {
        let before = self.entries.len();
        self.entries.retain(|e| e.id != id);
        let removed = self.entries.len() < before;
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    /// List all stored memory entries.
    pub fn list(&self) -> &[MemoryEntry] {
        &self.entries
    }

    /// Generate a system prompt section describing the memory system.
    /// This should be appended to the LLM system prompt when memory is enabled.
    pub fn get_system_prompt_section(&self) -> String {
        format!(
            "## Memory System\n\
             You have persistent long-term memory stored in MEMORY.md.\n\
             1. Before relying on prior preferences, environment details, or project decisions, use the memory tool with action search.\n\
             2. When the user shares an important durable fact, use the memory tool with action store.\n\
             3. Keep stored memories short, factual, and reusable. Avoid saving transient chatter.\n\
             4. The memory file lives in {}.\n",
            self.memory_file.display()
        )
    }

    /// Get the full memory file content for session context injection.
    /// Returns an empty string if no entries exist.
    pub fn get_session_context(&self) -> String {
        if self.entries.is_empty() {
            return String::new();
        }
        let mut out = String::from(HEADER);
        for entry in &self.entries {
            let category = format_category(&entry.category);
            let date = entry.created_at.as_deref().unwrap_or("unknown");
            out.push_str(&format!(
                "\n## [{}] [{}] Source: {} | {}\n{}\n",
                entry.id, category, entry.source, date, entry.content,
            ));
        }
        out
    }

    /// Persist the full entry list to the MEMORY.md file.
    fn persist(&self) -> aish_core::Result<()> {
        let mut out = String::from(HEADER);

        for entry in &self.entries {
            let category = format_category(&entry.category);
            let date = entry.created_at.as_deref().unwrap_or("unknown");
            out.push_str(&format!(
                "\n## [{}] [{}] Source: {} | {}\n{}\n",
                entry.id, category, entry.source, date, entry.content,
            ));
        }

        std::fs::write(&self.memory_file, out)
            .map_err(|e| AishError::Memory(format!("failed to write memory file: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// File format parsing
// ---------------------------------------------------------------------------

/// Parse an existing MEMORY.md file into a list of entries.
fn parse_file(path: &Path) -> aish_core::Result<Vec<MemoryEntry>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| AishError::Memory(format!("cannot read memory file {:?}: {}", path, e)))?;

    let mut entries = Vec::new();
    let mut current_lines: Vec<String> = Vec::new();
    let mut current_meta: Option<ParsedMeta> = None;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            // Flush previous entry
            if let Some(meta) = current_meta.take() {
                let body = current_lines.join("\n").trim().to_string();
                entries.push(build_entry(meta, body));
                current_lines.clear();
            }
            // Parse new header
            if let Some(meta) = parse_header(rest) {
                current_meta = Some(meta);
            }
        } else if current_meta.is_some() {
            current_lines.push(line.to_string());
        }
    }

    // Flush last entry
    if let Some(meta) = current_meta.take() {
        let body = current_lines.join("\n").trim().to_string();
        entries.push(build_entry(meta, body));
    }

    Ok(entries)
}

struct ParsedMeta {
    id: i64,
    category: MemoryCategory,
    source: String,
    date: Option<String>,
}

/// Parse a header line after the leading `## [` has been stripped.
///
/// Expected format: `id] [Category] Source: source | date`
fn parse_header(rest: &str) -> Option<ParsedMeta> {
    // Find `]` to get the id
    let bracket_pos = rest.find(']')?;
    let id: i64 = rest[..bracket_pos].trim().parse().ok()?;
    let rest = &rest[bracket_pos + 1..];

    // Find `[Category]`
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('[')?;
    let bracket2 = rest.find(']')?;
    let category_str = &rest[..bracket2];
    let category = parse_category(category_str)?;
    let rest = &rest[bracket2 + 1..];

    // Find "Source: ..."
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("Source:")?;
    let rest = rest.trim_start();

    // Split on "|"
    let mut parts = rest.splitn(2, '|');
    let source = parts.next().unwrap_or("").trim().to_string();
    let date = parts.next().map(|d| d.trim().to_string());

    Some(ParsedMeta {
        id,
        category,
        source,
        date,
    })
}

fn build_entry(meta: ParsedMeta, content: String) -> MemoryEntry {
    MemoryEntry {
        id: meta.id,
        source: meta.source,
        category: meta.category,
        content,
        importance: 1.0,
        tags: String::new(),
        created_at: meta.date,
        last_accessed_at: None,
        access_count: 0,
    }
}

fn format_category(cat: &MemoryCategory) -> &'static str {
    match cat {
        MemoryCategory::Preference => "Preference",
        MemoryCategory::Environment => "Environment",
        MemoryCategory::Solution => "Solution",
        MemoryCategory::Pattern => "Pattern",
        MemoryCategory::Other => "Other",
    }
}

fn parse_category(s: &str) -> Option<MemoryCategory> {
    match s.trim() {
        "Preference" => Some(MemoryCategory::Preference),
        "Environment" => Some(MemoryCategory::Environment),
        "Solution" => Some(MemoryCategory::Solution),
        "Pattern" => Some(MemoryCategory::Pattern),
        "Other" => Some(MemoryCategory::Other),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let dir = std::env::temp_dir().join("aish_memory_test_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path.clone()).unwrap();

        let id1 = mgr
            .store(
                "I prefer dark theme",
                MemoryCategory::Preference,
                "auto",
                1.0,
            )
            .unwrap();
        let id2 = mgr
            .store(
                "db port is 5432",
                MemoryCategory::Environment,
                "manual",
                0.8,
            )
            .unwrap();

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(mgr.list().len(), 2);

        // Reload from disk
        let mgr2 = MemoryManager::new(path.clone()).unwrap();
        assert_eq!(mgr2.list().len(), 2);
        assert_eq!(mgr2.list()[0].content, "I prefer dark theme");
        assert_eq!(mgr2.list()[1].content, "db port is 5432");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_recall() {
        let dir = std::env::temp_dir().join("aish_memory_test_recall");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        mgr.store(
            "dark theme preference",
            MemoryCategory::Preference,
            "auto",
            1.0,
        )
        .unwrap();
        mgr.store(
            "database port 5432",
            MemoryCategory::Environment,
            "manual",
            0.8,
        )
        .unwrap();
        mgr.store(
            "use rust for systems code",
            MemoryCategory::Pattern,
            "auto",
            0.6,
        )
        .unwrap();

        let results = mgr.recall("dark theme", 10);
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("dark theme"));

        let results = mgr.recall("port", 10);
        assert_eq!(results.len(), 1);
        assert!(results[0].content.contains("port 5432"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_remove() {
        let dir = std::env::temp_dir().join("aish_memory_test_remove");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        mgr.store("entry one", MemoryCategory::Other, "test", 1.0)
            .unwrap();
        let id = mgr
            .store("entry two", MemoryCategory::Other, "test", 1.0)
            .unwrap();
        assert_eq!(mgr.list().len(), 2);

        let removed = mgr.remove(id).unwrap();
        assert!(removed);
        assert_eq!(mgr.list().len(), 1);
        assert_eq!(mgr.list()[0].content, "entry one");

        // Removing non-existent returns false
        let removed = mgr.remove(999).unwrap();
        assert!(!removed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_header() {
        let meta = parse_header("1] [Preference] Source: auto | 2024-01-01").unwrap();
        assert_eq!(meta.id, 1);
        assert_eq!(meta.source, "auto");
        assert_eq!(meta.date.as_deref(), Some("2024-01-01"));
    }

    #[test]
    fn test_default_path() {
        let path = MemoryManager::default_path();
        assert!(path.to_string_lossy().contains("aish"));
        assert!(path.to_string_lossy().contains("MEMORY.md"));
    }

    #[test]
    fn test_duplicate_detection() {
        let dir = std::env::temp_dir().join("aish_memory_test_dup");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        let id1 = mgr
            .store(
                "I prefer dark theme",
                MemoryCategory::Preference,
                "auto",
                1.0,
            )
            .unwrap();
        let id2 = mgr
            .store(
                "I prefer dark theme",
                MemoryCategory::Preference,
                "auto",
                1.0,
            )
            .unwrap();
        // Duplicate should return same ID
        assert_eq!(id1, id2);
        assert_eq!(mgr.list().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_duplicate_case_insensitive() {
        let dir = std::env::temp_dir().join("aish_memory_test_dup_case");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        let id1 = mgr
            .store(
                "Database port 5432",
                MemoryCategory::Environment,
                "auto",
                1.0,
            )
            .unwrap();
        let id2 = mgr
            .store(
                "database PORT 5432",
                MemoryCategory::Environment,
                "auto",
                1.0,
            )
            .unwrap();
        assert_eq!(id1, id2);
        assert_eq!(mgr.list().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_duplicate_different_category_allowed() {
        let dir = std::env::temp_dir().join("aish_memory_test_dup_cat");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        let id1 = mgr
            .store("same content", MemoryCategory::Preference, "auto", 1.0)
            .unwrap();
        let id2 = mgr
            .store("same content", MemoryCategory::Solution, "auto", 1.0)
            .unwrap();
        // Different category = different entry
        assert_ne!(id1, id2);
        assert_eq!(mgr.list().len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_system_prompt_section() {
        let dir = std::env::temp_dir().join("aish_memory_test_sysprompt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mgr = MemoryManager::new(path).unwrap();
        let section = mgr.get_system_prompt_section();
        assert!(section.contains("Memory System"));
        assert!(section.contains("MEMORY.md"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_context_empty() {
        let dir = std::env::temp_dir().join("aish_memory_test_ctx_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mgr = MemoryManager::new(path).unwrap();
        let ctx = mgr.get_session_context();
        assert!(ctx.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_session_context_with_entries() {
        let dir = std::env::temp_dir().join("aish_memory_test_ctx");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        mgr.store("test entry", MemoryCategory::Other, "test", 1.0)
            .unwrap();
        let ctx = mgr.get_session_context();
        assert!(!ctx.is_empty());
        assert!(ctx.contains("test entry"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
