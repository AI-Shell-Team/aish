/// A single collapsed output record in session history.
#[derive(Clone, Debug)]
pub struct ExpandRecord {
    /// The bash command that produced this output.
    pub command: String,
    /// Full output content.
    pub output: String,
    /// Time string for display (e.g. "12:30:15").
    pub time: String,
    /// Total line count.
    pub line_count: usize,
}

/// Session-scoped history of collapsed bash outputs.
/// Destroyed when the shell exits.
pub struct ExpandHistory {
    records: Vec<ExpandRecord>,
    /// Maximum number of records to keep. Older records are evicted.
    max_records: usize,
}

/// Default maximum records kept in history.
const DEFAULT_MAX_RECORDS: usize = 50;

/// Maximum size of a single record's output (64KB).
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

impl ExpandHistory {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
            max_records: DEFAULT_MAX_RECORDS,
        }
    }

    /// Add a new record. Returns the record index.
    /// Evicts oldest records if over the limit.
    pub fn add(&mut self, command: String, mut output: String) -> usize {
        if output.len() > MAX_OUTPUT_BYTES {
            let mut end = MAX_OUTPUT_BYTES;
            while end > 0 && !output.is_char_boundary(end) {
                end -= 1;
            }
            output.truncate(end);
            output.push_str("\n... (truncated)");
        }
        let line_count = output.lines().count();
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.records.push(ExpandRecord {
            command,
            output,
            time,
            line_count,
        });
        // Evict oldest records if over the limit.
        if self.records.len() > self.max_records {
            let excess = self.records.len() - self.max_records;
            self.records.drain(0..excess);
        }
        self.records.len() - 1
    }

    /// Number of records.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether history is empty.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Clone all records (for use without holding the mutex).
    pub fn clone_records(&self) -> Vec<ExpandRecord> {
        self.records.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_retrieve() {
        let mut h = ExpandHistory::new();
        assert!(h.is_empty());
        h.add("ls -la".into(), "line1\nline2\nline3".into());
        assert_eq!(h.len(), 1);
        let records = h.clone_records();
        assert_eq!(records[0].command, "ls -la");
        assert_eq!(records[0].line_count, 3);
    }

    #[test]
    fn truncation_at_multibyte_boundary() {
        let mut h = ExpandHistory::new();
        // Build a string slightly over 64KB where the byte boundary falls
        // inside a multi-byte character.
        // Each CJK char is 3 bytes; 64*1024 / 3 = 21845.33, so 21846 chars
        // exceeds the limit and the truncation point lands mid-character.
        let cjk: String = "中".repeat(21846);
        assert!(cjk.len() > MAX_OUTPUT_BYTES);
        h.add("cat big.txt".into(), cjk.clone());
        let records = h.clone_records();
        assert!(records[0].output.len() <= MAX_OUTPUT_BYTES + 20);
        assert!(records[0].output.ends_with("... (truncated)"));
        // Verify no panic — the string is valid UTF-8 after truncation.
        let _ = records[0].output.chars().count();
    }

    #[test]
    fn multiple_records() {
        let mut h = ExpandHistory::new();
        h.add("cmd1".into(), "out1".into());
        h.add("cmd2".into(), "out2".into());
        h.add("cmd3".into(), "out3".into());
        assert_eq!(h.len(), 3);
        let records = h.clone_records();
        assert_eq!(records[1].command, "cmd2");
    }
}
