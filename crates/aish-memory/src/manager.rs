use crate::models::{MemoryEntry, MemorySource};
use aish_core::{AishError, MemoryCategory, MemoryScope};
use chrono::{DateTime, Utc};
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
        self.store_with_provenance(
            content,
            category,
            MemoryScope::User,
            MemorySource {
                label: source.to_string(),
                session_uuid: None,
                host: None,
            },
            importance,
            None,
        )
    }

    /// Store a new memory entry with full provenance metadata. Returns the
    /// assigned ID. If an entry with the same content and category already
    /// exists, returns the existing ID without creating a duplicate.
    ///
    /// `ttl_seconds` controls the optional expiry: `None` means no expiry,
    /// `Some(secs)` sets `expires_at` to now + secs.
    pub fn store_with_provenance(
        &mut self,
        content: &str,
        category: MemoryCategory,
        scope: MemoryScope,
        source: MemorySource,
        importance: f64,
        ttl_seconds: Option<u64>,
    ) -> aish_core::Result<i64> {
        let content_trimmed = content.trim();

        // Duplicate detection: same content (case-insensitive) + same category.
        // The confirmed TTL of THIS store wins: a later re-store (e.g. the
        // user confirms `permanent`) must refresh the existing entry's expiry
        // instead of silently keeping the old one.
        let content_lower = content_trimmed.to_lowercase();
        for entry in &mut self.entries {
            if entry.category == category && entry.content.to_lowercase() == content_lower {
                let now = Utc::now();
                entry.expires_at = ttl_seconds.and_then(|secs| {
                    let secs = i64::try_from(secs).ok()?;
                    now.checked_add_signed(chrono::Duration::seconds(secs))
                        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                });
                let id = entry.id;
                self.persist()?;
                return Ok(id);
            }
        }

        let now = Utc::now();
        let now_str = now.format("%Y-%m-%d").to_string();
        let now_rfc = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let id = self.next_id;
        self.next_id += 1;

        let expires_at = ttl_seconds.and_then(|secs| {
            let secs = i64::try_from(secs).ok()?;
            now.checked_add_signed(chrono::Duration::seconds(secs))
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        });

        let entry = MemoryEntry {
            id,
            source: source.label,
            source_session_uuid: source.session_uuid,
            source_host: source.host,
            category,
            scope,
            content: content_trimmed.to_string(),
            importance,
            tags: String::new(),
            created_at: Some(now_str.clone()),
            last_verified_at: Some(now_rfc.clone()),
            expires_at,
            last_accessed_at: Some(now_str),
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
            // Return active entries sorted by importance when query is empty
            let mut indices: Vec<usize> = (0..self.entries.len())
                .filter(|&i| !Self::is_expired(&self.entries[i]))
                .collect();
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
            if Self::is_expired(entry) {
                continue;
            }
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

    /// Re-verify a memory entry by ID: updates `last_verified_at` to today.
    /// If `new_ttl_seconds` is provided, resets `expires_at` to now + ttl.
    /// If `new_ttl_seconds` is None, clears `expires_at` (makes the entry non-expiring).
    /// Returns true if the entry was found and updated.
    pub fn verify(&mut self, id: i64, new_ttl_seconds: Option<u64>) -> aish_core::Result<bool> {
        let now = Utc::now();
        let now_rfc = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let mut found = false;
        for entry in &mut self.entries {
            if entry.id == id {
                entry.last_verified_at = Some(now_rfc.clone());
                match new_ttl_seconds {
                    Some(secs) => {
                        entry.expires_at = i64::try_from(secs)
                            .ok()
                            .and_then(|s| now.checked_add_signed(chrono::Duration::seconds(s)))
                            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());
                    }
                    None => {
                        // Clear expiry — the entry becomes non-expiring.
                        entry.expires_at = None;
                    }
                }
                found = true;
                break;
            }
        }
        if found {
            self.persist()?;
        }
        Ok(found)
    }
    /// Return IDs of all expired entries (expires_at < now).
    pub fn expired_entry_ids(&self) -> Vec<i64> {
        let now = Utc::now();
        self.entries
            .iter()
            .filter(|e| Self::is_expired_inner(e, &now))
            .map(|e| e.id)
            .collect()
    }

    /// Check whether a single entry is expired.
    pub fn is_expired(entry: &MemoryEntry) -> bool {
        Self::is_expired_inner(entry, &Utc::now())
    }

    /// Return IDs of entries that expire within `days` from now (not yet
    /// expired). Used to nudge the user to review/renew before facts vanish.
    pub fn expiring_soon_ids(&self, days: i64) -> Vec<i64> {
        let now = Utc::now();
        let horizon = now + chrono::Duration::days(days);
        self.entries
            .iter()
            .filter(|e| {
                !Self::is_expired_inner(e, &now)
                    && e.expires_at
                        .as_deref()
                        .and_then(|exp| {
                            DateTime::parse_from_rfc3339(exp)
                                .map(|dt| dt.with_timezone(&Utc) <= horizon)
                                .ok()
                                .or_else(|| {
                                    chrono::NaiveDate::parse_from_str(exp, "%Y-%m-%d")
                                        .map(|d| {
                                            d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc()
                                                <= horizon
                                        })
                                        .ok()
                                })
                        })
                        .unwrap_or(false)
            })
            .map(|e| e.id)
            .collect()
    }

    fn is_expired_inner(entry: &MemoryEntry, now: &DateTime<Utc>) -> bool {
        let Some(exp) = &entry.expires_at else {
            return false;
        };
        // Try RFC3339 first, fall back to date-only (%Y-%m-%d)
        if let Ok(dt) = DateTime::parse_from_rfc3339(exp) {
            return dt.with_timezone(&Utc) < *now;
        }
        if let Ok(date) = chrono::NaiveDate::parse_from_str(exp, "%Y-%m-%d") {
            let expiry = date.and_hms_opt(23, 59, 59).unwrap_or_default().and_utc();
            return expiry < *now;
        }
        false
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
             4. Expired memories are not injected; use /memory to review and renew them.\n\
             5. The memory file lives in {}.\n\
             6. CRITICAL: a fact is stored ONLY when the memory tool returns a\n\
             result containing an entry id. NEVER tell the user something is\n\
             recorded/remembered without having actually called the tool in\n\
             THIS turn and seeing its result. Restating an earlier successful\n\
             store does not store anything new: if the user asks to remember\n\
             new or changed content, you must call the tool again.\n",
            self.memory_file.display()
        )
    }

    /// Get the full memory file content for session context injection.
    /// Expired entries are excluded; a summary of expired entries is appended
    /// so the user is prompted to review them.
    /// Returns an empty string if no active entries exist.
    pub fn get_session_context(&self) -> String {
        let active: Vec<&MemoryEntry> = self
            .entries
            .iter()
            .filter(|e| !Self::is_expired(e))
            .collect();
        let expired: Vec<&MemoryEntry> = self
            .entries
            .iter()
            .filter(|e| Self::is_expired(e))
            .collect();

        if active.is_empty() && expired.is_empty() {
            return String::new();
        }

        let mut out = String::from(HEADER);
        for entry in &active {
            let category = format_category(&entry.category);
            out.push_str(&format!(
                "\n## [{}] [{}] [{}]\n{}\n",
                entry.id,
                category,
                format_scope(&entry.scope),
                entry.content,
            ));
        }
        if !expired.is_empty() {
            out.push_str(&format!(
                "\n## Expired Memories ({} — use /memory to review)\n",
                expired.len()
            ));
            for entry in &expired {
                let category = format_category(&entry.category);
                out.push_str(&format!(
                    "  #{} [{}] {} (expired: {})\n",
                    entry.id,
                    category,
                    entry.content,
                    entry.expires_at.as_deref().unwrap_or("?"),
                ));
            }
        }
        out
    }

    /// Persist the full entry list to the MEMORY.md file.
    ///
    /// New format (v2) uses `> key: value` metadata lines under the header:
    /// ```text
    /// ## [id] [Category]
    /// > scope: user
    /// > source: auto
    /// > source_session: <uuid>
    /// > source_host: <host>
    /// > created: 2026-01-01
    /// > verified: 2026-01-01
    /// > expires: 2026-08-01
    /// > importance: 0.8
    /// <content>
    /// ```
    /// Old format (`## [id] [Category] Source: src | date`) is still readable
    /// by the parser for backward compatibility.
    fn persist(&self) -> aish_core::Result<()> {
        let estimated_size = HEADER.len() + self.entries.len() * 200;
        let mut out = String::with_capacity(estimated_size);
        out.push_str(HEADER);

        for entry in &self.entries {
            let category = format_category(&entry.category);
            out.push_str(&format!("\n## [{}] [{}]\n", entry.id, category));
            out.push_str(&format!("> scope: {}\n", format_scope(&entry.scope)));
            out.push_str(&format!("> source: {}\n", entry.source));
            if let Some(uuid) = &entry.source_session_uuid {
                out.push_str(&format!("> source_session: {}\n", uuid));
            }
            if let Some(host) = &entry.source_host {
                out.push_str(&format!("> source_host: {}\n", host));
            }
            if let Some(date) = &entry.created_at {
                out.push_str(&format!("> created: {}\n", date));
            }
            if let Some(date) = &entry.last_verified_at {
                out.push_str(&format!("> verified: {}\n", date));
            }
            if let Some(exp) = &entry.expires_at {
                out.push_str(&format!("> expires: {}\n", exp));
            }
            out.push_str(&format!("> importance: {}\n", entry.importance));
            // Blank line terminates the metadata block so content that starts
            // with `> ` is not parsed as metadata on reload.
            out.push('\n');
            out.push_str(&format!("{}\n", entry.content));
        }

        std::fs::write(&self.memory_file, out)
            .map_err(|e| AishError::Memory(format!("failed to write memory file: {}", e)))
    }
}

// ---------------------------------------------------------------------------
// File format parsing
// ---------------------------------------------------------------------------

/// Parse an existing MEMORY.md file into a list of entries.
///
/// Supports two formats:
/// - **v2**: `## [id] [Category]` header followed by `> key: value` metadata
///   lines, then the content body.
/// - **v1 (legacy)**: `## [id] [Category] Source: src | date` single-line
///   header, followed by the content body.
fn parse_file(path: &Path) -> aish_core::Result<Vec<MemoryEntry>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| AishError::Memory(format!("cannot read memory file {:?}: {}", path, e)))?;

    let mut entries = Vec::new();
    let mut current_body: Vec<String> = Vec::new();
    let mut current_meta: Option<ParsedMeta> = None;
    // Once content lines start after the metadata block, subsequent `> ` lines
    // belong to the content body (e.g. markdown blockquotes), not metadata.
    let mut in_content = false;

    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            // Flush previous entry
            if let Some(meta) = current_meta.take() {
                let body = current_body.join("\n").trim().to_string();
                entries.push(build_entry(meta, body));
                current_body.clear();
            }
            current_meta = parse_header(rest);
            in_content = false;
        } else if !in_content && line.starts_with("> ") {
            // v2 metadata line — attach to current meta if present
            if let Some(meta) = current_meta.as_mut() {
                let kv = &line[2..];
                if let Some((key, value)) = kv.split_once(':') {
                    let value = value.trim().to_string();
                    match key.trim() {
                        "scope" => meta.scope = parse_scope(&value),
                        "source" => meta.source = value,
                        "source_session" => meta.source_session_uuid = Some(value),
                        "source_host" => meta.source_host = Some(value),
                        "created" => meta.created_at = Some(value),
                        "verified" => meta.last_verified_at = Some(value),
                        "expires" => meta.expires_at = Some(value),
                        "importance" => meta.importance = value.parse().unwrap_or(1.0),
                        _ => {}
                    }
                }
            }
        } else if !in_content && line.is_empty() && current_meta.is_some() {
            // Blank line terminates the metadata block; subsequent lines
            // (including `> `-prefixed content) belong to the content body.
            in_content = true;
        } else if current_meta.is_some() {
            in_content = true;
            current_body.push(line.to_string());
        }
    }

    // Flush last entry
    if let Some(meta) = current_meta.take() {
        let body = current_body.join("\n").trim().to_string();
        entries.push(build_entry(meta, body));
    }

    Ok(entries)
}

struct ParsedMeta {
    id: i64,
    category: MemoryCategory,
    scope: MemoryScope,
    source: String,
    source_session_uuid: Option<String>,
    source_host: Option<String>,
    created_at: Option<String>,
    last_verified_at: Option<String>,
    expires_at: Option<String>,
    importance: f64,
}

/// Parse a header line after the leading `## [` has been stripped.
///
/// v2 format: `id] [Category]`
/// v1 format: `id] [Category] Source: source | date`
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
    let rest = &rest[bracket2 + 1..].trim_start();

    // v2 format: nothing after [Category] — metadata comes via `> ` lines
    if rest.is_empty() {
        return Some(ParsedMeta {
            id,
            category,
            scope: MemoryScope::User,
            source: String::new(),
            source_session_uuid: None,
            source_host: None,
            created_at: None,
            last_verified_at: None,
            expires_at: None,
            importance: 1.0,
        });
    }

    // v1 legacy format: `Source: source | date`
    let rest = rest
        .strip_prefix("Source:")
        .map(|r| r.trim_start())
        .unwrap_or("");
    let mut parts = rest.splitn(2, '|');
    let source = parts.next().unwrap_or("").trim().to_string();
    let date = parts.next().map(|d| d.trim().to_string());

    Some(ParsedMeta {
        id,
        category,
        scope: MemoryScope::User,
        source,
        source_session_uuid: None,
        source_host: None,
        created_at: date,
        last_verified_at: None,
        expires_at: None,
        importance: 1.0,
    })
}

fn build_entry(meta: ParsedMeta, content: String) -> MemoryEntry {
    let created = meta
        .created_at
        .clone()
        .unwrap_or_else(|| Utc::now().format("%Y-%m-%d").to_string());
    MemoryEntry {
        id: meta.id,
        source: meta.source,
        source_session_uuid: meta.source_session_uuid,
        source_host: meta.source_host,
        category: meta.category,
        scope: meta.scope,
        content,
        importance: meta.importance,
        tags: String::new(),
        created_at: Some(created),
        last_verified_at: meta.last_verified_at,
        expires_at: meta.expires_at,
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

fn format_scope(scope: &MemoryScope) -> &'static str {
    match scope {
        MemoryScope::User => "user",
        MemoryScope::Host => "host",
        MemoryScope::Project => "project",
    }
}

fn parse_scope(s: &str) -> MemoryScope {
    match s.trim() {
        "host" => MemoryScope::Host,
        "project" => MemoryScope::Project,
        _ => MemoryScope::User,
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
        // v1 legacy format
        let meta = parse_header("1] [Preference] Source: auto | 2024-01-01").unwrap();
        assert_eq!(meta.id, 1);
        assert_eq!(meta.source, "auto");
        assert_eq!(meta.created_at.as_deref(), Some("2024-01-01"));
        assert_eq!(meta.scope, MemoryScope::User);

        // v2 format — metadata comes via `> ` lines, not the header
        let meta = parse_header("2] [Environment]").unwrap();
        assert_eq!(meta.id, 2);
        assert_eq!(meta.source, "");
        assert!(meta.created_at.is_none());
    }

    #[test]
    fn test_v2_roundtrip_with_provenance() {
        let dir = std::env::temp_dir().join("aish_memory_test_v2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path.clone()).unwrap();
        let id = mgr
            .store_with_provenance(
                "db port is 5432",
                MemoryCategory::Environment,
                MemoryScope::Host,
                MemorySource {
                    label: "explicit".to_string(),
                    session_uuid: Some("sess-abc".to_string()),
                    host: Some("prod-srv".to_string()),
                },
                0.9,
                Some(86400),
            )
            .unwrap();
        assert_eq!(id, 1);

        // Reload and verify all fields survive
        let mgr2 = MemoryManager::new(path).unwrap();
        let e = &mgr2.list()[0];
        assert_eq!(e.content, "db port is 5432");
        assert_eq!(e.source, "explicit");
        assert_eq!(e.source_session_uuid.as_deref(), Some("sess-abc"));
        assert_eq!(e.source_host.as_deref(), Some("prod-srv"));
        assert_eq!(e.scope, MemoryScope::Host);
        assert!(e.expires_at.is_some());
        assert!(e.last_verified_at.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_v1_backward_compat() {
        let dir = std::env::temp_dir().join("aish_memory_test_v1_compat");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        // Write old v1 format directly
        std::fs::write(
            &path,
            "# Memory\n\n## [1] [Preference] Source: auto | 2024-01-01\nI prefer dark theme\n",
        )
        .unwrap();

        let mgr = MemoryManager::new(path).unwrap();
        assert_eq!(mgr.list().len(), 1);
        let e = &mgr.list()[0];
        assert_eq!(e.id, 1);
        assert_eq!(e.content, "I prefer dark theme");
        assert_eq!(e.source, "auto");
        assert_eq!(e.created_at.as_deref(), Some("2024-01-01"));
        assert_eq!(e.scope, MemoryScope::User);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_verify_updates_last_verified() {
        let dir = std::env::temp_dir().join("aish_memory_test_verify");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        let id = mgr
            .store("test entry", MemoryCategory::Other, "test", 1.0)
            .unwrap();
        let old_verified = mgr.list()[0].last_verified_at.clone();

        std::thread::sleep(std::time::Duration::from_secs(2));
        assert!(mgr.verify(id, None).unwrap());
        let new_verified = mgr.list()[0].last_verified_at.clone();
        assert_ne!(old_verified, new_verified);

        // Non-existent ID
        assert!(!mgr.verify(999, None).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_expired_filtering() {
        let dir = std::env::temp_dir().join("aish_memory_test_expired");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        // Active entry (no TTL)
        mgr.store("active", MemoryCategory::Other, "test", 1.0)
            .unwrap();
        // Expired entry (TTL = 1 second)
        mgr.store_with_provenance(
            "expired",
            MemoryCategory::Other,
            MemoryScope::User,
            MemorySource {
                label: "test".to_string(),
                session_uuid: None,
                host: None,
            },
            1.0,
            Some(1),
        )
        .unwrap();

        // Wait for it to expire
        std::thread::sleep(std::time::Duration::from_secs(2));

        assert_eq!(mgr.list().len(), 2);
        assert_eq!(mgr.expired_entry_ids().len(), 1);

        // recall should skip expired
        let results = mgr.recall("", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "active");

        let _ = std::fs::remove_dir_all(&dir);
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
    fn test_duplicate_reapplies_confirmed_ttl() {
        let dir = std::env::temp_dir().join("aish_memory_test_dup_ttl");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        // First store: Environment entry with a 7-day TTL.
        let id1 = mgr
            .store_with_provenance(
                "prod db endpoint",
                MemoryCategory::Environment,
                MemoryScope::User,
                MemorySource {
                    label: "auto".to_string(),
                    session_uuid: None,
                    host: None,
                },
                1.0,
                Some(7 * 24 * 3600),
            )
            .unwrap();
        assert!(mgr.list()[0].expires_at.is_some());

        // Re-store the same content as permanent: the confirmed TTL of THIS
        // store must win, clearing the old expiry instead of keeping it.
        let id2 = mgr
            .store_with_provenance(
                "prod db endpoint",
                MemoryCategory::Environment,
                MemoryScope::User,
                MemorySource {
                    label: "explicit".to_string(),
                    session_uuid: None,
                    host: None,
                },
                1.0,
                None,
            )
            .unwrap();
        assert_eq!(id1, id2);
        assert_eq!(mgr.list().len(), 1);
        assert!(mgr.list()[0].expires_at.is_none());

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

    #[test]
    fn test_verify_none_clears_expiry() {
        let dir = std::env::temp_dir().join("aish_memory_test_verify_clears");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        let id = mgr
            .store_with_provenance(
                "temp fact",
                MemoryCategory::Other,
                MemoryScope::User,
                MemorySource {
                    label: "test".to_string(),
                    session_uuid: None,
                    host: None,
                },
                0.8,
                Some(3600), // 1 hour TTL
            )
            .unwrap();
        // Entry has expires_at set
        assert!(mgr.list()[0].expires_at.is_some());

        // verify(id, None) should clear expires_at
        let ok = mgr.verify(id, None).unwrap();
        assert!(ok);
        let entry = mgr.list().iter().find(|e| e.id == id).unwrap();
        assert!(entry.expires_at.is_none(), "expires_at should be cleared");
        assert!(
            entry.last_verified_at.is_some(),
            "last_verified_at should be set"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_content_with_blockquote() {
        // Content containing `> ` lines (e.g. markdown blockquotes) must not
        // be misinterpreted as v2 metadata lines.
        let dir = std::env::temp_dir().join("aish_memory_test_blockquote");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path.clone()).unwrap();
        let content = "User said:\n> This is a quote\n> source: not metadata";
        let id = mgr
            .store_with_provenance(
                content,
                MemoryCategory::Other,
                MemoryScope::User,
                MemorySource {
                    label: "test".to_string(),
                    session_uuid: None,
                    host: None,
                },
                0.8,
                None,
            )
            .unwrap();

        // Reload from disk and verify content is preserved
        let mgr2 = MemoryManager::new(path).unwrap();
        let entry = mgr2.list().iter().find(|e| e.id == id).unwrap();
        assert!(
            entry.content.contains("> This is a quote"),
            "blockquote lost"
        );
        assert!(
            entry.content.contains("> source: not metadata"),
            "metadata-like line lost"
        );
        assert_eq!(entry.source, "test", "source corrupted by content line");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_expiring_soon_ids() {
        let dir = std::env::temp_dir().join("aish_memory_test_expiring_soon");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("MEMORY.md");

        let mut mgr = MemoryManager::new(path).unwrap();
        let soon = mgr
            .store_with_provenance(
                "expires in 3 days",
                MemoryCategory::Environment,
                MemoryScope::User,
                MemorySource {
                    label: "test".to_string(),
                    session_uuid: None,
                    host: None,
                },
                0.8,
                Some(3 * 24 * 3600),
            )
            .unwrap();
        let far = mgr
            .store_with_provenance(
                "expires in 90 days",
                MemoryCategory::Other,
                MemoryScope::User,
                MemorySource {
                    label: "test".to_string(),
                    session_uuid: None,
                    host: None,
                },
                0.8,
                Some(90 * 24 * 3600),
            )
            .unwrap();

        let expiring = mgr.expiring_soon_ids(7);
        assert!(expiring.contains(&soon), "3-day entry must be flagged");
        assert!(!expiring.contains(&far), "90-day entry must not be flagged");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
