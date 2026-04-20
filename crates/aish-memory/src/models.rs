use aish_core::MemoryCategory;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: i64,
    pub source: String,
    pub category: MemoryCategory,
    pub content: String,
    pub importance: f64,
    pub tags: String,
    pub created_at: Option<String>,
    pub last_accessed_at: Option<String>,
    pub access_count: i32,
}
