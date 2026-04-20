use std::collections::VecDeque;

/// Simple history-based autosuggest engine.
/// Maintains a ring buffer of recent commands and finds prefix matches.
pub struct AutoSuggest {
    history: VecDeque<String>,
    max_size: usize,
}

impl AutoSuggest {
    pub fn new(max_size: usize) -> Self {
        Self {
            history: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    pub fn add(&mut self, command: &str) {
        let command = command.trim().to_string();
        if command.is_empty() {
            return;
        }
        // Remove duplicate if exists
        self.history.retain(|c| c != &command);
        // Add to front
        self.history.push_front(command);
        // Trim to max size
        while self.history.len() > self.max_size {
            self.history.pop_back();
        }
    }

    pub fn suggest(&self, input: &str) -> Option<&str> {
        let input = input.trim();
        if input.is_empty() {
            return None;
        }
        for entry in &self.history {
            if entry.starts_with(input) && entry != input {
                return Some(entry.as_str());
            }
        }
        None
    }

    pub fn load_from_iter<I: IntoIterator<Item = String>>(&mut self, commands: I) {
        for cmd in commands {
            self.add(&cmd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_match() {
        let mut sug = AutoSuggest::new(100);
        sug.add("git status");
        sug.add("git push");
        sug.add("cargo build");
        assert_eq!(sug.suggest("git"), Some("git push"));
        assert_eq!(sug.suggest("cargo"), Some("cargo build"));
    }

    #[test]
    fn test_no_exact_match() {
        let mut sug = AutoSuggest::new(100);
        sug.add("ls");
        // Input equals history entry exactly, should return None
        assert_eq!(sug.suggest("ls"), None);
        // No prefix match at all
        assert_eq!(sug.suggest("git"), None);
    }

    #[test]
    fn test_empty_input() {
        let mut sug = AutoSuggest::new(100);
        sug.add("git status");
        assert_eq!(sug.suggest(""), None);
        assert_eq!(sug.suggest("   "), None);
    }

    #[test]
    fn test_dedup_recent_first() {
        let mut sug = AutoSuggest::new(100);
        sug.add("git status");
        sug.add("git push");
        // Adding again moves it to the front
        sug.add("git status");
        assert_eq!(sug.suggest("git"), Some("git status"));
    }

    #[test]
    fn test_max_size_limit() {
        let mut sug = AutoSuggest::new(3);
        sug.add("cmd1");
        sug.add("cmd2");
        sug.add("cmd3");
        sug.add("cmd4");
        // Oldest (cmd1) should be evicted
        assert_eq!(sug.suggest("cmd1"), None);
        assert_eq!(sug.suggest("cmd4"), None); // exact match returns None
        assert_eq!(sug.suggest("cmd"), Some("cmd4"));
        // Verify size is exactly 3
        assert_eq!(sug.history.len(), 3);
    }

    #[test]
    fn test_load_from_iter() {
        let mut sug = AutoSuggest::new(100);
        sug.load_from_iter(vec![
            "git pull".to_string(),
            "git push".to_string(),
            "cargo test".to_string(),
        ]);
        assert_eq!(sug.suggest("git"), Some("git push"));
        assert_eq!(sug.suggest("cargo"), Some("cargo test"));
        assert_eq!(sug.suggest("car"), Some("cargo test"));
    }
}
