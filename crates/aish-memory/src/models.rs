use aish_core::{MemoryCategory, MemoryScope};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub source: String,
    pub source_session_uuid: Option<String>,
    pub source_host: Option<String>,
    pub category: MemoryCategory,
    pub scope: MemoryScope,
    pub content: String,
    pub importance: f64,
    pub tags: String,
    pub created_at: Option<String>,
    pub last_verified_at: Option<String>,
    pub expires_at: Option<String>,
    pub last_accessed_at: Option<String>,
    pub access_count: i32,
}

/// Provenance metadata for a memory entry, captured at store time.
#[derive(Debug, Clone)]
pub struct MemorySource {
    /// Human-readable label: "explicit", "auto", "host_note", etc.
    pub label: String,
    /// UUID of the session that created this entry, if available.
    pub session_uuid: Option<String>,
    /// Hostname of the machine the entry originated on, if available.
    pub host: Option<String>,
}
