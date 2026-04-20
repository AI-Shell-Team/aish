use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::Watcher;

use crate::manager::SkillManager;

/// Events sent from the filesystem watcher thread to the owner.
enum WatcherCommand {
    /// The watcher should shut down.
    Shutdown,
}

/// Optional hot-reloader that watches skill directories and notifies the
/// [`SkillManager`] when files change.
///
/// Construct this separately — it is **not** required for basic
/// `SkillManager` operation.
///
/// # Usage
///
/// ```ignore
/// let mut mgr = SkillManager::new();
/// mgr.load_all_skills()?;
/// let mut reloader = SkillHotReloader::new(mgr.get_skill_dirs());
/// reloader.start();
/// // … later, in your event loop:
/// let changed = reloader.take_changes();
/// for path in &changed {
///     let _ = mgr.reload_skill(path);
/// }
/// // when done:
/// reloader.stop();
/// ```
pub struct SkillHotReloader {
    /// Channel used to send control messages to the watcher thread.
    cmd_tx: mpsc::Sender<WatcherCommand>,
    /// Channel used by the watcher thread to report settled changes.
    change_rx: mpsc::Receiver<Vec<PathBuf>>,
    /// Handle to the background thread so we can join on stop.
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl SkillHotReloader {
    /// Create a new hot-reloader that will watch the given directories.
    ///
    /// This does **not** start watching — call [`start`](Self::start) afterwards.
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (change_tx, change_rx) = mpsc::channel();

        let thread_handle = std::thread::Builder::new()
            .name("aish-skill-watcher".into())
            .spawn(move || {
                watcher_loop(dirs, cmd_rx, change_tx);
            })
            .expect("failed to spawn skill-watcher thread");

        Self {
            cmd_tx,
            change_rx,
            thread_handle: Some(thread_handle),
        }
    }

    /// Start watching. This is a no-op if the watcher is already running
    /// (the watcher starts when the thread spawns).
    pub fn start(&self) {
        // The watcher is started eagerly in `new()`. This method exists for
        // API clarity and future extensibility (e.g. pause/resume).
    }

    /// Stop the watcher and wait for the background thread to exit.
    pub fn stop(&mut self) {
        let _ = self.cmd_tx.send(WatcherCommand::Shutdown);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }

    /// Drain all accumulated change batches and return the deduplicated set
    /// of file paths that changed since the last call.
    ///
    /// This is non-blocking: if nothing changed an empty vec is returned.
    pub fn take_changes(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        while let Ok(batch) = self.change_rx.try_recv() {
            paths.extend(batch);
        }
        paths.sort();
        paths.dedup();
        paths
    }

    /// Apply pending changes to a [`SkillManager`].
    ///
    /// For each changed path the method determines whether it is a create /
    /// modify (reload) or a delete (remove), and calls the appropriate
    /// manager method. Returns the list of skill names that were affected.
    pub fn apply_changes(&self, manager: &mut SkillManager) -> Vec<String> {
        let paths = self.take_changes();
        let mut affected = Vec::new();

        for path in &paths {
            if is_skill_file(path) {
                if path.exists() {
                    // File was created or modified.
                    match manager.reload_skill(path) {
                        Ok(()) => {
                            if let Some(skill) = manager.get_skill_by_path(path) {
                                affected.push(skill.metadata.name.clone());
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to reload skill {:?}: {}", path, e);
                        }
                    }
                } else {
                    // File was deleted — remove by path lookup.
                    if let Some(name) = manager.find_skill_name_by_path(path) {
                        manager.remove_skill(&name);
                        affected.push(name);
                    }
                }
            }
        }

        affected
    }
}

impl Drop for SkillHotReloader {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Core watcher loop run in the background thread.
fn watcher_loop(
    dirs: Vec<PathBuf>,
    cmd_rx: mpsc::Receiver<WatcherCommand>,
    change_tx: mpsc::Sender<Vec<PathBuf>>,
) {
    // Create the notify watcher.
    let (fs_tx, fs_rx) = mpsc::channel();

    let mut watcher = match notify::RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            if let Ok(event) = res {
                // Forward paths for any relevant event kind.
                let paths: Vec<PathBuf> = event.paths;
                if !paths.is_empty() {
                    let _ = fs_tx.send(paths);
                }
            }
        },
        notify::Config::default().with_poll_interval(Duration::from_secs(2)),
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("Failed to create file watcher: {}", e);
            return;
        }
    };

    // Register directories.
    for dir in &dirs {
        if dir.is_dir() {
            if let Err(e) = watcher.watch(dir, notify::RecursiveMode::Recursive) {
                tracing::warn!("Cannot watch {:?}: {}", dir, e);
            }
        }
    }

    tracing::info!("Skill hot-reloader watching {} directories", dirs.len());

    // Event loop: debounce filesystem events and forward settled batches.
    let mut pending: HashSet<PathBuf> = HashSet::new();

    loop {
        // Use a small timeout so we can check the command channel regularly.
        match fs_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(paths) => {
                // New filesystem events arrived — add to pending set and
                // reset the debounce timer by draining all available.
                for p in paths {
                    if is_skill_file(&p) || is_skill_dir_parent(&p) {
                        pending.insert(p);
                    }
                }
                // Drain any queued events immediately.
                while let Ok(extra) = fs_rx.try_recv() {
                    for p in extra {
                        if is_skill_file(&p) || is_skill_dir_parent(&p) {
                            pending.insert(p);
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // If we have pending paths whose debounce interval has
                // elapsed, forward them.
                if !pending.is_empty() {
                    let batch: Vec<PathBuf> = pending.drain().collect();
                    if change_tx.send(batch).is_err() {
                        // Receiver dropped — exit.
                        return;
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Watcher dropped — exit.
                return;
            }
        }

        // Check for control commands (non-blocking).
        match cmd_rx.try_recv() {
            Ok(WatcherCommand::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                // Flush any remaining pending changes.
                if !pending.is_empty() {
                    let batch: Vec<PathBuf> = pending.drain().collect();
                    let _ = change_tx.send(batch);
                }
                tracing::info!("Skill hot-reloader shutting down");
                return;
            }
            // All events are handled via fs_rx; no other WatcherCommand variants.
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }
}

/// Whether a path looks like a SKILL.md file.
fn is_skill_file(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().eq_ignore_ascii_case("SKILL.md"))
        .unwrap_or(false)
}

/// Whether a path could be a parent directory of skill files.
///
/// We accept any directory event so that newly created SKILL.md files inside
/// new subdirectories are discovered.
fn is_skill_dir_parent(_path: &Path) -> bool {
    // Be conservative — only react to SKILL.md files.
    // Directory-level events will naturally trigger child file events
    // when SKILL.md is created inside them.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_skill_file() {
        assert!(is_skill_file(Path::new("skills/foo/SKILL.md")));
        assert!(is_skill_file(Path::new("skills/foo/skill.md")));
        assert!(is_skill_file(Path::new("SKILL.MD")));
        assert!(!is_skill_file(Path::new("skills/foo/readme.md")));
        assert!(!is_skill_file(Path::new("skills/foo/")));
    }
}
