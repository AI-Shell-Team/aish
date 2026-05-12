use serde::{Deserialize, Serialize};

/// Auto-detected system information for a remote host.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub kernel: String,
    pub shell: String,
    pub user: String,
    pub home: String,
    pub tools: Vec<String>,
    pub package_manager: String,
    pub locale: String,
}


/// A user-authored note about a remote host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostNote {
    pub id: u64,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub added: chrono::DateTime<chrono::Utc>,
    pub content: String,
}

/// Per-host profile combining auto-detected info and user notes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostProfile {
    pub host_key: String,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub first_seen: chrono::DateTime<chrono::Utc>,
    #[serde(with = "chrono::serde::ts_seconds")]
    pub last_updated: chrono::DateTime<chrono::Utc>,
    #[serde(default = "default_cache_ttl")]
    pub probe_cache_ttl: u64,
    #[serde(default)]
    pub system: SystemInfo,
    #[serde(default)]
    pub notes: Vec<HostNote>,
}

fn default_cache_ttl() -> u64 {
    604800 // 7 days
}

impl HostProfile {
    pub fn new(host_key: &str) -> Self {
        let now = chrono::Utc::now();
        Self {
            host_key: host_key.to_string(),
            first_seen: now,
            last_updated: now,
            probe_cache_ttl: default_cache_ttl(),
            system: SystemInfo::default(),
            notes: Vec::new(),
        }
    }

    pub fn probe_is_stale(&self) -> bool {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(self.last_updated)
            .num_seconds();
        elapsed as u64 > self.probe_cache_ttl || self.system.os.is_empty()
    }

    pub fn add_note(&mut self, content: String) -> u64 {
        // Truncate notes longer than 1000 bytes at a valid char boundary
        let content = if content.len() > 1000 {
            let mut s = content;
            let mut end = 1000;
            while end > 0 && !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
            s
        } else {
            content
        };
        let id = self.notes.last().map_or(1, |n| n.id + 1);
        self.notes.push(HostNote {
            id,
            added: chrono::Utc::now(),
            content,
        });
        while self.notes.len() > 100 {
            self.notes.remove(0);
        }
        self.last_updated = chrono::Utc::now();
        id
    }

    pub fn remove_notes(&mut self, keyword: &str) -> usize {
        let keyword_lower = keyword.to_lowercase();
        let before = self.notes.len();
        self.notes.retain(|n| !n.content.to_lowercase().contains(&keyword_lower));
        let removed = before - self.notes.len();
        if removed > 0 {
            self.last_updated = chrono::Utc::now();
        }
        removed
    }

    pub fn format_display(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Host: {}", self.host_key));
        lines.push(format!(
            "First seen: {}",
            self.first_seen.format("%Y-%m-%d %H:%M UTC")
        ));
        lines.push(format!(
            "Last updated: {}",
            self.last_updated.format("%Y-%m-%d %H:%M UTC")
        ));

        if !self.system.os.is_empty() {
            lines.push(String::new());
            lines.push("[System Profile]".to_string());
            lines.push(format!("  OS: {}", self.system.os));
            lines.push(format!("  Kernel: {}", self.system.kernel));
            lines.push(format!("  Shell: {}", self.system.shell));
            lines.push(format!("  User: {} (home: {})", self.system.user, self.system.home));
            if !self.system.tools.is_empty() {
                lines.push(format!("  Tools: {}", self.system.tools.join(", ")));
            }
            if !self.system.package_manager.is_empty() {
                lines.push(format!("  Package manager: {}", self.system.package_manager));
            }
            if !self.system.locale.is_empty() {
                lines.push(format!("  Locale: {}", self.system.locale));
            }
        }

        if !self.notes.is_empty() {
            lines.push(String::new());
            lines.push("[Notes]".to_string());
            for note in &self.notes {
                lines.push(format!(
                    "  #{} [{}] {}",
                    note.id,
                    note.added.format("%Y-%m-%d"),
                    note.content
                ));
            }
        }

        lines.join("\n")
    }

    pub fn format_for_prompt(&self) -> String {
        let mut sections = Vec::new();

        if !self.system.os.is_empty() {
            sections.push(format!(
                "**Remote Host Dossier ({host_key}):**\n\nSystem Profile:\n- {os}, kernel {kernel}\n- Shell: {shell}, User: {user}\n- Available: {tools}\n- Package manager: {pkg}",
                host_key = self.host_key,
                os = self.system.os,
                kernel = self.system.kernel,
                shell = self.system.shell,
                user = self.system.user,
                tools = if self.system.tools.is_empty() {
                    "none detected".to_string()
                } else {
                    self.system.tools.join(", ")
                },
                pkg = if self.system.package_manager.is_empty() {
                    "unknown".to_string()
                } else {
                    self.system.package_manager.clone()
                },
            ));
        }

        if !self.notes.is_empty() {
            let notes_text = self
                .notes
                .iter()
                .map(|n| format!("- {}", n.content))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("Notes:\n{notes_text}"));
        }

        sections.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_profile() {
        let p = HostProfile::new("root@192.168.1.100");
        assert_eq!(p.host_key, "root@192.168.1.100");
        assert!(p.system.os.is_empty());
        assert!(p.notes.is_empty());
        assert_eq!(p.probe_cache_ttl, 604800);
    }

    #[test]
    fn test_add_note() {
        let mut p = HostProfile::new("test");
        let id = p.add_note("hello world".to_string());
        assert_eq!(id, 1);
        assert_eq!(p.notes.len(), 1);
        assert_eq!(p.notes[0].content, "hello world");
    }

    #[test]
    fn test_add_multiple_notes() {
        let mut p = HostProfile::new("test");
        p.add_note("first".to_string());
        let id2 = p.add_note("second".to_string());
        assert_eq!(id2, 2);
        assert_eq!(p.notes.len(), 2);
    }

    #[test]
    fn test_remove_notes_by_keyword() {
        let mut p = HostProfile::new("test");
        p.add_note("k8s master".to_string());
        p.add_note("database backup".to_string());
        p.add_note("k8s worker".to_string());
        let removed = p.remove_notes("k8s");
        assert_eq!(removed, 2);
        assert_eq!(p.notes.len(), 1);
        assert_eq!(p.notes[0].content, "database backup");
    }

    #[test]
    fn test_remove_notes_case_insensitive() {
        let mut p = HostProfile::new("test");
        p.add_note("K8S MASTER".to_string());
        let removed = p.remove_notes("k8s");
        assert_eq!(removed, 1);
    }

    #[test]
    fn test_remove_notes_no_match() {
        let mut p = HostProfile::new("test");
        p.add_note("hello".to_string());
        let removed = p.remove_notes("xyz");
        assert_eq!(removed, 0);
        assert_eq!(p.notes.len(), 1);
    }

    #[test]
    fn test_probe_is_stale_when_empty() {
        let p = HostProfile::new("test");
        assert!(p.probe_is_stale());
    }

    #[test]
    fn test_probe_is_stale_when_fresh() {
        let mut p = HostProfile::new("test");
        p.system.os = "Ubuntu 22.04".to_string();
        p.last_updated = chrono::Utc::now();
        assert!(!p.probe_is_stale());
    }

    #[test]
    fn test_format_display() {
        let mut p = HostProfile::new("root@10.0.0.1");
        p.system.os = "Ubuntu 22.04".to_string();
        p.system.shell = "/bin/bash".to_string();
        p.add_note("test note".to_string());
        let display = p.format_display();
        assert!(display.contains("root@10.0.0.1"));
        assert!(display.contains("Ubuntu 22.04"));
        assert!(display.contains("test note"));
    }

    #[test]
    fn test_format_for_prompt() {
        let mut p = HostProfile::new("root@10.0.0.1");
        p.system.os = "Ubuntu 22.04".to_string();
        p.system.kernel = "5.15.0".to_string();
        p.system.shell = "/bin/bash".to_string();
        p.system.user = "root".to_string();
        p.system.tools = vec!["python3".to_string(), "docker".to_string()];
        p.add_note("k8s master".to_string());
        let prompt = p.format_for_prompt();
        assert!(prompt.contains("root@10.0.0.1"));
        assert!(prompt.contains("python3, docker"));
        assert!(prompt.contains("k8s master"));
    }

    #[test]
    fn test_notes_prune_at_limit() {
        let mut p = HostProfile::new("test");
        for i in 0..105 {
            p.add_note(format!("note {}", i));
        }
        assert_eq!(p.notes.len(), 100);
    }

    #[test]
    fn test_add_note_truncation() {
        let mut p = HostProfile::new("test");
        let long_content = "x".repeat(1500);
        p.add_note(long_content.clone());
        assert_eq!(p.notes[0].content.len(), 1000);
        assert_eq!(p.notes[0].content, "x".repeat(1000));
    }

    #[test]
    fn test_add_note_no_truncation_under_limit() {
        let mut p = HostProfile::new("test");
        let content = "x".repeat(999);
        p.add_note(content.clone());
        assert_eq!(p.notes[0].content.len(), 999);
    }
}
