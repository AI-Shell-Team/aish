use crate::doctor::checker::{CheckItem, CheckResult, Checker, FixResult};
use std::path::PathBuf;

const WAL_SIZE_WARN_MB: f64 = 100.0;

pub struct SessionChecker {
    db_path: PathBuf,
}

impl SessionChecker {
    pub fn new() -> Self {
        Self {
            db_path: dirs::data_local_dir()
                .unwrap_or_else(|| std::env::temp_dir().join("aish-fallback"))
                .join("aish/sessions.db"),
        }
    }

    fn wal_path(&self) -> PathBuf {
        let mut p = self.db_path.as_os_str().to_owned();
        p.push("-wal");
        PathBuf::from(p)
    }

    fn check_db(&self) -> Option<CheckItem> {
        match aish_session::SessionStore::open(Some(&self.db_path)) {
            Ok(store) => {
                let count = store.list_sessions(1).map(|s| s.len()).ok();
                match count {
                    Some(n) => Some(CheckItem::pass(
                        "db_readable",
                        format!(
                            "SQLite database: {} ({} sessions)",
                            self.db_path.display(),
                            n
                        ),
                    )),
                    None => Some(CheckItem::pass(
                        "db_readable",
                        format!("SQLite database: {} (opened OK)", self.db_path.display()),
                    )),
                }
            }
            Err(e) => Some(
                CheckItem::warn(
                    "db_readable",
                    format!("Database exists but unreadable: {}", e),
                )
                .hint("Consider backing up and removing the database file"),
            ),
        }
    }
}

impl Checker for SessionChecker {
    fn name(&self) -> &str {
        "Session Store"
    }

    fn check(&self) -> Vec<CheckResult> {
        let mut items = Vec::new();

        if self.db_path.exists() {
            if let Some(item) = self.check_db() {
                items.push(item);
            }

            let wal_path = self.wal_path();
            if wal_path.exists() {
                if let Ok(metadata) = std::fs::metadata(&wal_path) {
                    let size_mb = metadata.len() as f64 / 1_048_576.0;
                    let is_large = size_mb > WAL_SIZE_WARN_MB;
                    let mut item =
                        CheckItem::pass("wal_size", format!("WAL file size: {:.1}MB", size_mb));
                    if is_large {
                        item.status = crate::doctor::CheckStatus::Warn;
                        item.fixable = true;
                        item.hint = Some("Large WAL may indicate missed checkpoints".to_string());
                    }
                    items.push(item);
                }
            }
        } else {
            items.push(
                CheckItem::warn("exists", "No session database found")
                    .hint("Will be created on first session"),
            );
        }

        vec![CheckResult::from_items(self.name(), items)]
    }

    fn fix(&self, item: &CheckItem) -> FixResult {
        if item.name == "wal_size" {
            // Use SQLite checkpoint instead of deleting the WAL file.
            // Deleting the WAL directly can cause data loss if there are
            // uncommitted transactions. A checkpoint safely flushes the WAL
            // contents back into the main database file.
            match rusqlite::Connection::open(&self.db_path) {
                Ok(conn) => match conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                    Ok(()) => FixResult {
                        success: true,
                        message: "WAL checkpoint completed (database flushed)".to_string(),
                    },
                    Err(e) => FixResult {
                        success: false,
                        message: format!("Checkpoint failed: {}", e),
                    },
                },
                Err(e) => FixResult {
                    success: false,
                    message: format!("Failed to open database for checkpoint: {}", e),
                },
            }
        } else {
            FixResult {
                success: false,
                message: "Cannot fix this item".to_string(),
            }
        }
    }

    fn box_clone(&self) -> Box<dyn Checker> {
        Box::new(Self::new())
    }
}
