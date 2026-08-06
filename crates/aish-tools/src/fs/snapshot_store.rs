//! Session-scoped snapshot store for file edit anchoring and rollback.
//!
//! Two responsibilities, combining content-anchoring with an aish-native
//! rollback layer:
//!
//! 1. **TAG anchoring (optimistic concurrency)**: `read_file` stamps a file
//!    with a 4-hex content-derived tag; `edit_file` verifies the tag still
//!    matches the on-disk content before applying. A mismatch means the file
//!    drifted since the model last read it — the edit is rejected and the
//!    model must re-read.
//!
//! 2. **Rollback chain**: every `edit_file`/`write_file` mutation pushes the
//!    prior content into an append-only history, enabling `/undo` and
//!    `restore` to revert AI-made file changes.
//!
//! The tag is a non-cryptographic content hash truncated to 16 bits. By the
//! birthday paradox, ~300 distinct file versions in a session give a ~50%
//! collision chance, so a tag match is necessary but not sufficient — stale
//! detection is a safety net, never a guarantee.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// A 4-uppercase-hex snapshot tag derived from file content (FNV-1a 64-bit,
/// avalanche-finalized, truncated to 16 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotTag(u16);

impl SnapshotTag {
    /// Compute a tag from text content (UTF-8). Delegates to [`from_bytes`].
    ///
    /// Non-cryptographic: a collision only causes a missed stale detection
    /// (never data loss). The murmur3-style finalizer in [`from_bytes`] keeps
    /// the 16-bit output well-distributed so small edits almost always flip it.
    pub fn from_content(content: &str) -> Self {
        Self::from_bytes(content.as_bytes())
    }

    /// Compute a tag from raw bytes. Works for binary files (non-UTF-8) so
    /// rollback tags stay stable regardless of content type.
    pub fn from_bytes(content: &[u8]) -> Self {
        let mut hash: u64 = 0xcbf29ce484222325;
        for &byte in content {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        // Avalanche finalizer (murmur3-style) so every bit reacts to small
        // input changes. Plain FNV-1a leaves high bits insensitive to single-
        // byte deltas, causing collisions on near-identical content.
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51afd7ed558ccd);
        hash ^= hash >> 33;
        Self(hash as u16)
    }

    /// Render as 4 uppercase hex chars, e.g. `0A3B`.
    pub fn as_hex(self) -> String {
        format!("{:04X}", self.0)
    }
}

impl fmt::Display for SnapshotTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_hex())
    }
}

impl FromStr for SnapshotTag {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 4 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("expected 4 hex chars, got {:?}", s));
        }
        Ok(Self(u16::from_str_radix(s, 16).map_err(|e| e.to_string())?))
    }
}

/// Kind of mutation that produced a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotOp {
    Edit,
    Write,
}

impl fmt::Display for SnapshotOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotOp::Edit => f.write_str("edit"),
            SnapshotOp::Write => f.write_str("write"),
        }
    }
}

/// One recorded file state, pushed before a mutation is applied.
#[derive(Debug, Clone)]
pub struct FileSnapshot {
    pub id: u64,
    pub path: PathBuf,
    /// Content before the mutation (raw bytes, so binary files roll back too).
    /// `None` means the file did not exist yet (it was created by the
    /// mutation); restoring deletes it.
    pub prior_content: Option<Vec<u8>>,
    /// Tag of the content AFTER the mutation (for quick re-read skipping).
    pub tag: SnapshotTag,
    pub op: SnapshotOp,
    pub ts: SystemTime,
}

/// What an undo/restore must write back to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoResult {
    pub path: PathBuf,
    /// Bytes to write. `None` means delete the file (it was newly created).
    pub content: Option<Vec<u8>>,
    pub snapshot_id: u64,
}

/// What [`UndoResult::apply_to_disk`] did to the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Wrote prior bytes back to the file.
    Restored,
    /// Deleted the file (the undone mutation had created it).
    Removed,
}

impl UndoResult {
    /// Apply this restore action to disk: write the prior bytes back, or
    /// delete the file when `content` is `None` (the mutation created it).
    ///
    /// `tolerate_missing` makes a delete of an already-absent file succeed —
    /// used by batch `/rollback` so a partial-restore retry converges instead
    /// of wedging on `NotFound`. Single-step undo passes `false` so an
    /// unexpected absence surfaces as an error.
    pub fn apply_to_disk(&self, tolerate_missing: bool) -> std::io::Result<ApplyOutcome> {
        match &self.content {
            Some(bytes) => {
                std::fs::write(&self.path, bytes)?;
                Ok(ApplyOutcome::Restored)
            }
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(ApplyOutcome::Removed),
                Err(e) if tolerate_missing && e.kind() == std::io::ErrorKind::NotFound => {
                    Ok(ApplyOutcome::Removed)
                }
                Err(e) => Err(e),
            },
        }
    }
}

/// Maximum rollback entries kept in memory. Older entries are dropped (FIFO)
/// to bound memory on long sessions with many large-file edits.
const MAX_HISTORY: usize = 200;

/// Canonicalize a path for use as a stable store key. Falls back to the
/// lexical form when canonicalization fails (e.g. the file does not exist
/// yet, so symlinks cannot be resolved).
fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Session-scoped store: latest tag per file plus a bounded rollback chain.
#[derive(Debug, Default)]
pub struct SnapshotStore {
    /// Latest known tag per canonical path, updated on read/edit/write.
    tags: HashMap<PathBuf, SnapshotTag>,
    /// Bounded rollback chain (newest last; oldest dropped past MAX_HISTORY).
    history: Vec<FileSnapshot>,
    next_id: u64,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a read: compute the tag, remember it, and return it so the
    /// caller can emit a `[path#TAG]` header.
    pub fn record_read(&mut self, path: &Path, content: &str) -> SnapshotTag {
        let key = normalize_path(path);
        let tag = SnapshotTag::from_content(content);
        self.tags.insert(key, tag);
        tag
    }

    /// Latest tag remembered for a path (from a prior read/edit/write).
    /// Returns `None` when the path was never observed through the store.
    pub fn current_tag(&self, path: &Path) -> Option<SnapshotTag> {
        self.tags.get(&normalize_path(path)).copied()
    }

    /// Whether `disk_content` still matches the tag last recorded for `path`
    /// (from a read/edit/write). Returns `true` when fresh, or when the path
    /// was never observed (no baseline — allow the edit through).
    ///
    /// Server-side drift check: even without a model-supplied tag, a file that
    /// changed since the last `read_file` is detected and must be re-read.
    pub fn is_fresh(&self, path: &Path, disk_content: &str) -> bool {
        match self.tags.get(&normalize_path(path)) {
            Some(remembered) => SnapshotTag::from_content(disk_content) == *remembered,
            None => true,
        }
    }

    /// Record a mutation (edit or write). Pushes the prior content into the
    /// rollback chain and refreshes the remembered tag to the new content.
    /// Returns the snapshot id assigned to this entry.
    ///
    /// `prior_content` is the file's bytes before the mutation (`None` if the
    /// file did not exist).
    pub fn record_mutation(
        &mut self,
        path: &Path,
        prior_content: Option<Vec<u8>>,
        new_content: &str,
        op: SnapshotOp,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let key = normalize_path(path);
        let tag = SnapshotTag::from_content(new_content);
        let snapshot = FileSnapshot {
            id,
            path: key.clone(),
            prior_content,
            tag,
            op,
            ts: SystemTime::now(),
        };
        self.history.push(snapshot);
        // Bound memory: drop the oldest entry when over capacity.
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.tags.insert(key, tag);
        id
    }

    /// Peek the restore action for the most recent mutation WITHOUT removing
    /// it. Use this to attempt the disk restore first; call
    /// [`commit_undo_last`] only after the restore succeeds, so a failed IO
    /// does not lose the snapshot.
    pub fn peek_undo_last(&self) -> Option<UndoResult> {
        let snap = self.history.last()?;
        Some(Self::build_undo_result(snap))
    }

    /// Peek the restore action for the most recent mutation on a specific
    /// path, without removing it.
    pub fn peek_undo_last_for(&self, path: &Path) -> Option<UndoResult> {
        let key = normalize_path(path);
        let snap = self.history.iter().rev().find(|s| s.path == key)?;
        Some(Self::build_undo_result(snap))
    }

    /// Build a restore action from a snapshot without mutating store state.
    fn build_undo_result(snap: &FileSnapshot) -> UndoResult {
        UndoResult {
            path: snap.path.clone(),
            content: snap.prior_content.clone(),
            snapshot_id: snap.id,
        }
    }

    /// Commit the undo by removing the most recent mutation and rewinding the
    /// remembered tag. Call ONLY after the disk restore for the peeked action
    /// succeeded. `snapshot_id` must match the peeked entry's id; returns
    /// `None` (without mutating) when a newer mutation has landed in between,
    /// so a stale peek is rejected rather than consuming the wrong snapshot.
    pub fn commit_undo_last(&mut self, snapshot_id: u64) -> Option<FileSnapshot> {
        let snap = self.history.last()?;
        if snap.id != snapshot_id {
            return None;
        }
        let snap = self.history.pop()?;
        self.rewind_tag(&snap);
        Some(snap)
    }

    /// Commit undo for a specific path. Call only after a successful restore.
    /// `snapshot_id` must match the peeked entry's id; returns `None` (without
    /// mutating) when a newer mutation on this path has landed in between.
    pub fn commit_undo_last_for(&mut self, path: &Path, snapshot_id: u64) -> Option<FileSnapshot> {
        let key = normalize_path(path);
        let idx = self.history.iter().rposition(|s| s.path == key)?;
        if self.history[idx].id != snapshot_id {
            return None;
        }
        let snap = self.history.remove(idx);
        self.rewind_tag(&snap);
        Some(snap)
    }

    /// Rewind the remembered tag to a snapshot's prior content. If the file
    /// was created by the mutation (no prior), forget the tag entirely.
    fn rewind_tag(&mut self, snap: &FileSnapshot) {
        match &snap.prior_content {
            Some(prior) => {
                self.tags
                    .insert(snap.path.clone(), SnapshotTag::from_bytes(prior));
            }
            None => {
                self.tags.remove(&snap.path);
            }
        }
    }

    /// Convenience: peek + commit with no disk IO between (atomic undo).
    /// Prefer [`peek_undo_last`] + [`commit_undo_last`] when a disk restore
    /// must succeed before consuming the history entry.
    pub fn undo_last(&mut self) -> Option<UndoResult> {
        let result = self.peek_undo_last()?;
        self.commit_undo_last(result.snapshot_id);
        Some(result)
    }

    /// Convenience: peek + commit for a specific path (atomic undo).
    pub fn undo_last_for(&mut self, path: &Path) -> Option<UndoResult> {
        let result = self.peek_undo_last_for(path)?;
        self.commit_undo_last_for(path, result.snapshot_id);
        Some(result)
    }

    /// Peek ALL restore actions needed to roll back to before snapshot `id`,
    /// in reverse chronological order (newest first). Each action must be
    /// applied to disk; call [`commit_restore`] only after all succeed.
    ///
    /// Rolling back to `id` reverts `id` and every newer mutation — every
    /// file touched in that range is restored to its prior content. A single
    /// file edited multiple times in the range yields multiple actions; apply
    /// them in the returned order so the earliest mutation's prior wins.
    pub fn peek_restore(&self, id: u64) -> Option<Vec<UndoResult>> {
        let pos = self.history.iter().position(|s| s.id == id)?;
        Some(
            self.history[pos..]
                .iter()
                .rev()
                .map(Self::build_undo_result)
                .collect(),
        )
    }

    /// Commit: drop the target and everything newer, rewinding the remembered
    /// tag for every affected file. Rewinding in reverse (newest first) lets
    /// the earliest mutation's prior win per file. Call ONLY after all disk
    /// restores for the peeked actions succeeded.
    pub fn commit_restore(&mut self, id: u64) -> Option<()> {
        let pos = self.history.iter().position(|s| s.id == id)?;
        let drained: Vec<FileSnapshot> = self.history.split_off(pos);
        for snap in drained.iter().rev() {
            self.rewind_tag(snap);
        }
        Some(())
    }

    /// Convenience: peek + commit with no disk IO between (atomic restore).
    pub fn restore(&mut self, id: u64) -> Option<Vec<UndoResult>> {
        let result = self.peek_restore(id)?;
        self.commit_restore(id);
        Some(result)
    }

    /// Immutable view of the full rollback chain (newest last).
    pub fn history(&self) -> &[FileSnapshot] {
        &self.history
    }

    /// Number of recorded mutations.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Clear all history and remembered tags (e.g. on session reset).
    pub fn clear(&mut self) {
        self.tags.clear();
        self.history.clear();
    }
}

/// Shared, thread-safe handle to a [`SnapshotStore`] for injection into
/// multiple tools (read_file / edit_file / write_file) that must observe the
/// same session state. Constructed once per session, cloned into each tool.
///
/// **Concurrency model:** the shell drives tools sequentially (only `Agent`
/// sub-agent batches run concurrently, and those use isolated sub-sessions),
/// so the peek → disk-IO → commit sequences in `/undo` and `/rollback` never
/// interleave with a concurrent mutation. The `Mutex` guards data integrity
/// for the rare shared-access path (e.g. inherited tools), not because callers
/// race in practice.
pub type SharedSnapshotStore = Arc<Mutex<SnapshotStore>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_4_hex_and_stable() {
        let t = SnapshotTag::from_content("hello world");
        let hex = t.as_hex();
        assert_eq!(hex.len(), 4);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(t, SnapshotTag::from_content("hello world"));
    }

    #[test]
    fn tag_differs_for_different_content() {
        assert_ne!(
            SnapshotTag::from_content("foo"),
            SnapshotTag::from_content("bar")
        );
    }

    #[test]
    fn tag_roundtrips_through_str() {
        let t = SnapshotTag::from_content("payload");
        let parsed: SnapshotTag = t.as_hex().parse().unwrap();
        assert_eq!(t, parsed);
    }

    #[test]
    fn tag_rejects_bad_input() {
        assert!("XYZ".parse::<SnapshotTag>().is_err());
        assert!("ZZZZ".parse::<SnapshotTag>().is_err());
        assert!("12345".parse::<SnapshotTag>().is_err());
    }

    #[test]
    fn tag_works_for_binary_bytes() {
        // H2: binary content must also produce a stable, distinct tag.
        let a = SnapshotTag::from_bytes(&[0x00, 0xFF, 0x80]);
        let b = SnapshotTag::from_bytes(&[0x00, 0xFF, 0x81]);
        assert_ne!(a, b);
        assert_eq!(a, SnapshotTag::from_bytes(&[0x00, 0xFF, 0x80]));
    }

    #[test]
    fn record_read_remembers_tag() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        let t = store.record_read(p, "body");
        assert_eq!(store.current_tag(p), Some(t));
    }

    #[test]
    fn is_fresh_detects_drift() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        store.record_read(p, "v1");
        assert!(store.is_fresh(p, "v1"));
        assert!(!store.is_fresh(p, "v2"));
    }

    #[test]
    fn is_fresh_allows_unobserved_path() {
        // No baseline (file never read) → treated as fresh; edits are allowed.
        let store = SnapshotStore::new();
        assert!(store.is_fresh(Path::new("/tmp/never_read.txt"), "anything"));
    }

    #[test]
    fn record_mutation_pushes_history_and_refreshes_tag() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        store.record_read(p, "v1");
        let id = store.record_mutation(p, Some(b"v1".to_vec()), "v2", SnapshotOp::Edit);
        assert_eq!(store.history_len(), 1);
        assert_eq!(store.current_tag(p), Some(SnapshotTag::from_content("v2")));
        assert_eq!(store.history()[0].id, id);
    }

    #[test]
    fn undo_last_restores_prior_content() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        store.record_mutation(p, Some(b"old".to_vec()), "new", SnapshotOp::Edit);
        let r = store.undo_last().unwrap();
        assert_eq!(r.path, p);
        assert_eq!(r.content.as_deref(), Some(b"old".as_slice()));
        assert_eq!(store.current_tag(p), Some(SnapshotTag::from_content("old")));
        assert!(store.history().is_empty());
    }

    #[test]
    fn undo_last_for_created_file_returns_none_content() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/new.txt");
        store.record_mutation(p, None, "created", SnapshotOp::Write);
        let r = store.undo_last().unwrap();
        assert_eq!(r.content, None);
        assert!(store.current_tag(p).is_none());
    }

    #[test]
    fn undo_last_for_targets_specific_path() {
        let mut store = SnapshotStore::new();
        let a = Path::new("/tmp/a.txt");
        let b = Path::new("/tmp/b.txt");
        store.record_mutation(a, Some(b"a0".to_vec()), "a1", SnapshotOp::Edit);
        store.record_mutation(b, Some(b"b0".to_vec()), "b1", SnapshotOp::Edit);
        let r = store.undo_last_for(b).unwrap();
        assert_eq!(r.path, b);
        assert_eq!(store.history_len(), 1);
        assert_eq!(store.history()[0].path, a);
    }

    #[test]
    fn restore_drops_newer_mutations() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        let id0 = store.record_mutation(p, Some(b"v0".to_vec()), "v1", SnapshotOp::Edit);
        store.record_mutation(p, Some(b"v1".to_vec()), "v2", SnapshotOp::Edit);
        store.record_mutation(p, Some(b"v2".to_vec()), "v3", SnapshotOp::Edit);
        let actions = store.restore(id0).unwrap();
        // Reverse order: [v2_prior, v1_prior, v0_prior]. Target's prior last.
        assert_eq!(actions.len(), 3);
        assert_eq!(
            actions.last().unwrap().content.as_deref(),
            Some(b"v0".as_slice())
        );
        assert!(store.history().is_empty());
    }

    #[test]
    fn restore_unknown_id_returns_none() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        store.record_mutation(p, Some(b"v0".to_vec()), "v1", SnapshotOp::Edit);
        assert!(store.restore(999).is_none());
    }
    #[test]
    fn restore_rolls_back_all_files_in_range() {
        // Regression: restore to a point must revert EVERY file changed at
        // or after that point — not just the target file.
        let mut store = SnapshotStore::new();
        let tf = Path::new("/tmp/testfile");
        let af = Path::new("/tmp/a");
        let id1 = store.record_mutation(tf, Some(b"t0".to_vec()), "t1", SnapshotOp::Edit);
        store.record_mutation(tf, Some(b"t1".to_vec()), "t2", SnapshotOp::Edit);
        store.record_mutation(af, Some(b"a0".to_vec()), "a1", SnapshotOp::Edit);
        let actions = store.restore(id1).unwrap();
        // 3 actions in reverse: a->a0, testfile->t1, testfile->t0.
        assert_eq!(actions.len(), 3);
        let has_a = actions
            .iter()
            .any(|a| a.path == af && a.content.as_deref() == Some(b"a0".as_slice()));
        let has_tf_t0 = actions
            .iter()
            .any(|a| a.path == tf && a.content.as_deref() == Some(b"t0".as_slice()));
        assert!(has_a, "a file rollback missing");
        assert!(has_tf_t0, "testfile earliest prior missing");
        assert!(store.history().is_empty());
    }

    #[test]
    fn undo_on_empty_history_returns_none() {
        let mut store = SnapshotStore::new();
        assert!(store.undo_last().is_none());
    }

    #[test]
    fn clear_resets_everything() {
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        store.record_read(p, "x");
        store.record_mutation(p, Some(b"x".to_vec()), "y", SnapshotOp::Edit);
        store.clear();
        assert!(store.current_tag(p).is_none());
        assert!(store.history().is_empty());
    }

    #[test]
    fn peek_then_failed_io_keeps_history() {
        // H1: peek must not consume history; only commit does. A failed disk
        // restore between peek and commit leaves the snapshot recoverable.
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        store.record_mutation(p, Some(b"old".to_vec()), "new", SnapshotOp::Edit);
        let peeked = store.peek_undo_last().unwrap();
        assert_eq!(peeked.content.as_deref(), Some(b"old".as_slice()));
        // History still intact after peek (simulating a failed IO before commit).
        assert_eq!(store.history_len(), 1);
        // Now commit succeeds — history consumed only here.
        store.commit_undo_last(peeked.snapshot_id);
        assert!(store.history().is_empty());
    }

    #[test]
    fn history_is_bounded() {
        // M2: history must not exceed MAX_HISTORY.
        let mut store = SnapshotStore::new();
        let p = Path::new("/tmp/a.txt");
        for i in 0..(MAX_HISTORY + 50) {
            store.record_mutation(
                p,
                Some(b"prior".to_vec()),
                &format!("v{i}"),
                SnapshotOp::Edit,
            );
        }
        assert_eq!(store.history_len(), MAX_HISTORY);
        // Oldest entries dropped; the first kept entry is not id 0.
        assert!(store.history()[0].id > 0);
    }
}
