//! Append-only AI transcript persistence (issue #530).
//!
//! `/export` must show the complete AI conversation even after the context
//! was compacted or trimmed. The compacted context lives in
//! `sessions.state` (overwritten on every persist), so the transcript is
//! stored separately in the append-only `ai_messages` table. This module
//! bridges [`aish_context::TranscriptRecorder`] to that table.
//!
//! The recorder owns its own SQLite connection: `rusqlite::Connection` is
//! `Send` but not `Sync`, so sharing the shell's `SessionStore` across
//! threads is impossible. WAL mode (enabled by `SessionStore::open`) allows
//! multiple connections to the same database file, mirroring the existing
//! `AuditStore` pattern.
//!
//! The target session UUID lives behind a shared handle so a session switch
//! (`/resume`, `/fork`) can retarget the log without rebuilding the
//! recorder installed inside `AiHandler`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aish_context::TranscriptRecorder;
use aish_session::{SessionContextMessage, SessionStore};

/// Shared, mutable target-session handle for a [`SessionTranscriptRecorder`].
///
/// `/resume` and `/fork` update this in place; every later append lands in
/// the newly active session's transcript.
#[derive(Clone)]
pub struct TranscriptSessionHandle(Arc<Mutex<String>>);

impl TranscriptSessionHandle {
    pub fn new(session_uuid: String) -> Self {
        Self(Arc::new(Mutex::new(session_uuid)))
    }

    pub fn set(&self, session_uuid: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = session_uuid;
    }

    fn get(&self) -> String {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Records every context append into the `ai_messages` table.
pub struct SessionTranscriptRecorder {
    session: TranscriptSessionHandle,
    /// `None` when the session database could not be opened: the recorder
    /// then drops appends (logging each one) instead of writing conversation
    /// content to a fixed shared fallback path such as `/tmp`, where another
    /// local user could pre-create a world-writable file.
    store: Option<Mutex<SessionStore>>,
    /// Appends that could not be written (store unavailable or SQLite
    /// error). `/export` surfaces this as an incompleteness warning — the
    /// transcript is authoritative once non-empty, so silent gaps would be
    /// permanent.
    failed_appends: AtomicUsize,
}

impl SessionTranscriptRecorder {
    /// Open the recorder on the given database path. Opening is infallible
    /// from the caller's perspective: a store that cannot open (disk error,
    /// bad path) degrades to a recorder that logs and drops appends, never
    /// one that breaks the conversation.
    pub fn open(path: Option<&std::path::Path>, session: TranscriptSessionHandle) -> Self {
        let store = match SessionStore::open(path) {
            Ok(store) => Some(Mutex::new(store)),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "transcript recorder could not open session db; appends will be dropped"
                );
                None
            }
        };
        Self {
            session,
            store,
            failed_appends: AtomicUsize::new(0),
        }
    }

    /// Seed the transcript from a legacy session's context snapshot.
    ///
    /// Sessions persisted by older builds have a (possibly compacted)
    /// snapshot but no transcript rows. If such a session is resumed and the
    /// user then says something new, the transcript becomes non-empty and
    /// `/export` would show only that one new turn, dropping the snapshot's
    /// older messages entirely. Importing the snapshot once (only when the
    /// transcript is still empty) keeps the best available history.
    fn seed_from_snapshot(&self, snapshot: &[aish_context::ContextMessage]) {
        if snapshot.is_empty() {
            return;
        }
        let Some(store) = self.store.as_ref() else {
            self.failed_appends
                .fetch_add(snapshot.len(), Ordering::Relaxed);
            return;
        };
        let session_uuid = self.session.get();
        let store = store.lock().unwrap_or_else(|e| e.into_inner());
        // Re-check under the lock: a concurrent turn may have appended the
        // first real message between our caller's check and now.
        match store.get_ai_messages(&session_uuid) {
            Ok(existing) if !existing.is_empty() => return,
            Err(error) => {
                tracing::warn!(
                    %error,
                    session_uuid = %session_uuid,
                    "could not read transcript before seeding; skipping snapshot import"
                );
                return;
            }
            Ok(_) => {}
        }
        let mut failed = 0usize;
        for message in snapshot {
            let entry = SessionContextMessage {
                role: message.role.clone(),
                content: message.content.clone(),
                memory_type: message.memory_type.clone(),
                name: message.name.clone(),
                tool_call_id: message.tool_call_id.clone(),
                tool_calls: message.tool_calls.clone(),
                reasoning_content: message.reasoning_content.clone(),
            };
            if let Err(error) = store.append_ai_message(&session_uuid, &entry) {
                failed += 1;
                tracing::warn!(
                    %error,
                    session_uuid = %session_uuid,
                    "failed to seed AI transcript from snapshot"
                );
            }
        }
        if failed > 0 {
            self.failed_appends.fetch_add(failed, Ordering::Relaxed);
        }
    }

    /// Install this recorder into the handler's context manager.
    pub fn install(self, handler: &mut crate::ai_handler::AiHandler) {
        handler
            .context_manager_mut()
            .set_transcript_recorder(Arc::new(self));
    }
}

impl TranscriptRecorder for SessionTranscriptRecorder {
    fn record(&self, memory_type: &aish_core::MemoryType, message: &aish_context::ContextMessage) {
        let entry = SessionContextMessage {
            role: message.role.clone(),
            content: message.content.clone(),
            memory_type: memory_type.clone(),
            name: message.name.clone(),
            tool_call_id: message.tool_call_id.clone(),
            tool_calls: message.tool_calls.clone(),
            reasoning_content: message.reasoning_content.clone(),
        };
        let session_uuid = self.session.get();
        let Some(store) = self.store.as_ref() else {
            self.failed_appends.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let store = store.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = store.append_ai_message(&session_uuid, &entry) {
            self.failed_appends.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                %error,
                session_uuid = %session_uuid,
                "failed to append AI transcript message"
            );
        }
    }

    fn seed_from_snapshot(&self, snapshot: &[aish_context::ContextMessage]) {
        SessionTranscriptRecorder::seed_from_snapshot(self, snapshot)
    }

    fn failed_appends(&self) -> Option<usize> {
        Some(self.failed_appends.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aish_context::ContextManager;

    fn ctx_msg(role: &str, content: &str) -> aish_context::ContextMessage {
        aish_context::ContextMessage {
            role: role.to_string(),
            content: content.to_string(),
            memory_type: aish_core::MemoryType::Llm,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn appended_messages_reach_the_transcript_table() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");
        let handle = TranscriptSessionHandle::new("sess-a".to_string());
        let recorder = SessionTranscriptRecorder::open(Some(&db_path), handle.clone());

        let mut cm = ContextManager::new();
        cm.set_transcript_recorder(Arc::new(recorder));
        cm.add_memory(
            aish_core::MemoryType::Llm,
            ctx_msg("user", "first question"),
        );
        cm.add_memory(
            aish_core::MemoryType::Llm,
            ctx_msg("assistant", "first answer"),
        );

        let store = SessionStore::open(Some(&db_path)).unwrap();
        let messages = store.get_ai_messages("sess-a").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "first question");
        assert_eq!(messages[1].role, "assistant");
    }

    #[test]
    fn handle_retarget_sends_new_appends_to_new_session() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");
        let handle = TranscriptSessionHandle::new("sess-a".to_string());
        let recorder = SessionTranscriptRecorder::open(Some(&db_path), handle.clone());

        let mut cm = ContextManager::new();
        cm.set_transcript_recorder(Arc::new(recorder));
        cm.add_memory(aish_core::MemoryType::Llm, ctx_msg("user", "before switch"));

        handle.set("sess-b".to_string());
        cm.add_memory(aish_core::MemoryType::Llm, ctx_msg("user", "after switch"));

        let store = SessionStore::open(Some(&db_path)).unwrap();
        let a = store.get_ai_messages("sess-a").unwrap();
        let b = store.get_ai_messages("sess-b").unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].content, "before switch");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].content, "after switch");
    }

    #[test]
    fn compaction_never_touches_the_transcript() {
        // Mirrors the manager-level test but through the real store-backed
        // recorder: compact the context, the table must still hold the
        // original conversation (issue #530 acceptance).
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");
        let handle = TranscriptSessionHandle::new("sess-c".to_string());
        let recorder = SessionTranscriptRecorder::open(Some(&db_path), handle);

        let mut cm = ContextManager::new();
        cm.set_transcript_recorder(Arc::new(recorder));
        cm.add_memory(
            aish_core::MemoryType::Llm,
            ctx_msg("user", "codeword APPLE-001"),
        );
        cm.add_memory(
            aish_core::MemoryType::Llm,
            ctx_msg("assistant", "noted APPLE-001"),
        );

        // Rewrite the in-memory context the way full compaction would.
        cm.replace_messages(vec![ctx_msg(
            "system",
            "<conversation-summary>everything summarized</conversation-summary>",
        )]);

        let store = SessionStore::open(Some(&db_path)).unwrap();
        let messages = store.get_ai_messages("sess-c").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "codeword APPLE-001");
        assert_eq!(messages[1].content, "noted APPLE-001");
    }

    #[test]
    fn seeding_legacy_snapshot_survives_the_first_new_turn() {
        // Review finding on PR #556: a legacy session (snapshot rows, no
        // transcript) resumed and continued must keep its snapshot history
        // in the export — seeding once must not be undone by later appends.
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");
        let handle = TranscriptSessionHandle::new("legacy".to_string());
        let recorder = Arc::new(SessionTranscriptRecorder::open(Some(&db_path), handle));

        let legacy_snapshot = vec![
            aish_context::ContextMessage {
                role: "user".to_string(),
                content: "old question".to_string(),
                memory_type: aish_core::MemoryType::Llm,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
            },
            aish_context::ContextMessage {
                role: "assistant".to_string(),
                content: "old answer".to_string(),
                memory_type: aish_core::MemoryType::Llm,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
            },
        ];

        recorder.seed_from_snapshot(&legacy_snapshot);

        let mut cm = ContextManager::new();
        cm.set_transcript_recorder(recorder.clone());
        cm.add_memory(aish_core::MemoryType::Llm, ctx_msg("user", "new turn"));

        let store = SessionStore::open(Some(&db_path)).unwrap();
        let messages = store.get_ai_messages("legacy").unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].content, "old answer");
        assert_eq!(messages[2].content, "new turn");

        // Seeding twice must not duplicate history.
        recorder.seed_from_snapshot(&legacy_snapshot);
        let store = SessionStore::open(Some(&db_path)).unwrap();
        assert_eq!(store.get_ai_messages("legacy").unwrap().len(), 3);
    }

    #[test]
    fn seeding_is_skipped_when_transcript_already_has_rows() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");
        let handle = TranscriptSessionHandle::new("sess-f".to_string());
        let recorder = SessionTranscriptRecorder::open(Some(&db_path), handle);
        assert_eq!(recorder.failed_appends(), Some(0));

        let mut cm = ContextManager::new();
        cm.set_transcript_recorder(Arc::new(recorder));
        cm.add_memory(aish_core::MemoryType::Llm, ctx_msg("user", "tracked"));
        // Snapshot seed after a real turn: the transcript is non-empty, so
        // the seed must be a no-op (the snapshot would duplicate history).
        cm.seed_transcript_from_snapshot(&[ctx_msg("user", "seeded")]);

        let store = SessionStore::open(Some(&db_path)).unwrap();
        let messages = store.get_ai_messages("sess-f").unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "tracked");
    }
}
