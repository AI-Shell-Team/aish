use std::collections::BTreeMap;

/// Token usage for a single day, persisted to disk.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DailyRecord {
    input: u64,
    output: u64,
    requests: u64,
}

/// Persistent store for token usage, stored as daily buckets in a JSON file.
///
/// File location: `~/.config/aish/token_stats.json`
/// Format: `{ "2026-04-20": {"input":12000,"output":3000,"requests":8}, ... }`
///
/// Records older than 7 days are automatically pruned on load and save.
pub struct TokenUsageStore {
    path: std::path::PathBuf,
    records: BTreeMap<String, DailyRecord>,
    /// How much of this session's in-memory stats have already been recorded.
    session_recorded_input: u64,
    session_recorded_output: u64,
    session_recorded_requests: u64,
}

impl TokenUsageStore {
    /// Retention period in days.
    const RETENTION_DAYS: u64 = 7;

    /// Create or open the store at the given path.
    pub fn open(path: std::path::PathBuf) -> Self {
        let mut store = Self {
            path,
            records: BTreeMap::new(),
            session_recorded_input: 0,
            session_recorded_output: 0,
            session_recorded_requests: 0,
        };
        store.load();
        store
    }

    /// Return the default path for the token usage store.
    pub fn default_path() -> std::path::PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("aish")
            .join("token_stats.json")
    }

    /// Record the delta between the session's cumulative stats and what we've already recorded.
    pub fn record_session_delta(
        &mut self,
        total_input: u64,
        total_output: u64,
        request_count: u64,
    ) {
        let delta_input = total_input.saturating_sub(self.session_recorded_input);
        let delta_output = total_output.saturating_sub(self.session_recorded_output);
        let delta_requests = request_count.saturating_sub(self.session_recorded_requests);

        if delta_input == 0 && delta_output == 0 && delta_requests == 0 {
            return;
        }

        self.session_recorded_input = total_input;
        self.session_recorded_output = total_output;
        self.session_recorded_requests = request_count;

        let today = Self::today_string();
        let entry = self.records.entry(today).or_insert(DailyRecord {
            input: 0,
            output: 0,
            requests: 0,
        });
        entry.input += delta_input;
        entry.output += delta_output;
        entry.requests += delta_requests;

        self.save();
    }

    /// Aggregate all records within the retention window into TokenStats.
    pub fn stats(&self) -> aish_llm::TokenStats {
        let cutoff = Self::cutoff_date();
        let mut stats = aish_llm::TokenStats::default();
        for (date, record) in &self.records {
            if date >= &cutoff {
                stats.total_input += record.input;
                stats.total_output += record.output;
                stats.request_count += record.requests;
            }
        }
        stats
    }

    fn today_string() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    fn cutoff_date() -> String {
        let cutoff = chrono::Local::now() - chrono::Duration::days(Self::RETENTION_DAYS as i64);
        cutoff.format("%Y-%m-%d").to_string()
    }

    fn load(&mut self) {
        if let Ok(data) = std::fs::read_to_string(&self.path) {
            if let Ok(loaded) = serde_json::from_str::<BTreeMap<String, DailyRecord>>(&data) {
                self.records = loaded;
            }
        }
        self.prune();
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string(&self.records) {
            let _ = std::fs::write(&self.path, data);
        }
    }

    fn prune(&mut self) {
        let cutoff = Self::cutoff_date();
        self.records.retain(|date, _| date >= &cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_session_delta() {
        let dir = std::env::temp_dir().join("aish_test_token_store");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("token_stats.json");
        let _ = std::fs::remove_file(&path); // clean slate

        let mut store = TokenUsageStore::open(path.clone());

        // Record 100/50/2
        store.record_session_delta(100, 50, 2);
        // Record delta: 50/30/1
        store.record_session_delta(150, 80, 3);
        // No delta
        store.record_session_delta(150, 80, 3);

        let stats = store.stats();
        assert_eq!(stats.total_input, 150);
        assert_eq!(stats.total_output, 80);
        assert_eq!(stats.request_count, 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_persistence_across_instances() {
        let dir = std::env::temp_dir().join("aish_test_token_persist");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("token_stats.json");
        let _ = std::fs::remove_file(&path);

        // First instance records data
        let mut store1 = TokenUsageStore::open(path.clone());
        store1.record_session_delta(200, 100, 5);

        // Second instance loads from disk
        let store2 = TokenUsageStore::open(path.clone());
        let stats = store2.stats();
        assert_eq!(stats.total_input, 200);
        assert_eq!(stats.total_output, 100);
        assert_eq!(stats.request_count, 5);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
