use std::fs;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

use aish_tools::fs::{DriftStatus, SnapshotOp, SnapshotStore, SnapshotTag};

#[test]
fn issue_466_undo_detects_drift_after_external_change() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("test.txt");

    // 1. AI writes file from ORIGINAL_VERSION to AI_VERSION
    fs::write(&f, "ORIGINAL_VERSION").unwrap();
    let store = Arc::new(Mutex::new(SnapshotStore::new()));
    store.lock().unwrap().record_read(&f, "ORIGINAL_VERSION");
    let _id = store.lock().unwrap().record_mutation(
        &f,
        Some(b"ORIGINAL_VERSION".to_vec()),
        "AI_VERSION",
        SnapshotOp::Write,
    );
    fs::write(&f, "AI_VERSION").unwrap();

    // 2. External change to EXTERNAL_VERSION (simulating user/editor)
    fs::write(&f, "EXTERNAL_VERSION").unwrap();

    // 3. /undo: peek and check drift
    let result = store.lock().unwrap().peek_undo_last().unwrap();
    let drift = result.check_drift();

    match &drift {
        DriftStatus::Drifted { current } => {
            let c = std::str::from_utf8(current.as_ref().unwrap()).unwrap();
            assert_eq!(c, "EXTERNAL_VERSION");
        }
        DriftStatus::Fresh => panic!("FAIL: Should have detected drift!"),
    }

    // Verify expected_tag is carried
    assert_eq!(result.expected_tag, SnapshotTag::from_content("AI_VERSION"));

    // Without --force, drift stops the restore. Simulate the "proceed=false" path:
    // the file should NOT be overwritten.
    assert_eq!(fs::read_to_string(&f).unwrap(), "EXTERNAL_VERSION");

    // With --force, the restore proceeds despite drift
    result.apply_to_disk(false).unwrap();
    assert_eq!(fs::read_to_string(&f).unwrap(), "ORIGINAL_VERSION");
}

#[test]
fn issue_466_undo_no_drift_proceeds_normally() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("test.txt");

    fs::write(&f, "original").unwrap();
    let store = Arc::new(Mutex::new(SnapshotStore::new()));
    store.lock().unwrap().record_mutation(
        &f,
        Some(b"original".to_vec()),
        "changed",
        SnapshotOp::Write,
    );
    fs::write(&f, "changed").unwrap();

    let result = store.lock().unwrap().peek_undo_last().unwrap();
    let drift = result.check_drift();
    assert!(matches!(drift, DriftStatus::Fresh));

    result.apply_to_disk(false).unwrap();
    assert_eq!(fs::read_to_string(&f).unwrap(), "original");
}

#[test]
fn issue_466_rollback_preflight_detects_drift() {
    let dir = tempdir().unwrap();
    let f = dir.path().join("rb.txt");

    fs::write(&f, "v0").unwrap();
    let store = Arc::new(Mutex::new(SnapshotStore::new()));
    let id1 =
        store
            .lock()
            .unwrap()
            .record_mutation(&f, Some(b"v0".to_vec()), "v1", SnapshotOp::Edit);
    fs::write(&f, "v1").unwrap();
    let _id2 =
        store
            .lock()
            .unwrap()
            .record_mutation(&f, Some(b"v1".to_vec()), "v2", SnapshotOp::Edit);
    fs::write(&f, "v2").unwrap();

    // External change after AI's edit
    fs::write(&f, "external_v2").unwrap();

    // Peek restore to id1 (roll back both edits)
    let actions = store.lock().unwrap().peek_restore(id1).unwrap();
    assert_eq!(actions.len(), 2);

    // Preflight: check all drifts
    let drifts: Vec<DriftStatus> = actions.iter().map(|a| a.check_drift()).collect();
    let drifted_count = drifts
        .iter()
        .filter(|d| matches!(d, DriftStatus::Drifted { .. }))
        .count();
    assert!(
        drifted_count > 0,
        "Should detect drift on at least one action"
    );
}

#[test]
fn issue_466_rollback_preflight_no_false_positive_same_path() {
    // Same path mutated twice, NO external change. Intermediate actions
    // carry older expected tags; checking them against current disk content
    // would false-report drift. Only the newest action per path is a valid
    // drift probe (mirrors the /rollback preflight dedup in app.rs).
    let dir = tempdir().unwrap();
    let f = dir.path().join("multi.txt");

    fs::write(&f, "v0").unwrap();
    let store = Arc::new(Mutex::new(SnapshotStore::new()));
    let id1 =
        store
            .lock()
            .unwrap()
            .record_mutation(&f, Some(b"v0".to_vec()), "v1", SnapshotOp::Edit);
    fs::write(&f, "v1").unwrap();
    let _id2 =
        store
            .lock()
            .unwrap()
            .record_mutation(&f, Some(b"v1".to_vec()), "v2", SnapshotOp::Edit);
    fs::write(&f, "v2").unwrap(); // disk matches newest mutation — no drift

    let actions = store.lock().unwrap().peek_restore(id1).unwrap();
    assert_eq!(actions.len(), 2);

    // Per-path dedup: only the first (newest) action for the path is checked.
    use std::collections::HashSet;
    let mut seen_paths: HashSet<std::path::PathBuf> = HashSet::new();
    let drifted_count = actions
        .iter()
        .filter(|a| seen_paths.insert(a.path.clone()))
        .filter(|a| matches!(a.check_drift(), DriftStatus::Drifted { .. }))
        .count();
    assert_eq!(
        drifted_count, 0,
        "no external change → newest action must be Fresh, no false drift"
    );

    // And the whole batch applies cleanly without force.
    for action in &actions {
        let (ok, msg) = apply_restore_action(action, true, false);
        assert!(ok, "apply failed: {msg}");
    }
    assert_eq!(fs::read_to_string(&f).unwrap(), "v0");
}

#[test]
fn issue_466_rollback_confirmed_drift_not_reblocked() {
    // Drift the user already confirmed at preflight must not be re-blocked
    // by the post-confirmation recheck — only NEW drift during the
    // confirmation window aborts. Mirrors the confirmed_drift exclusion in
    // the /rollback recheck (app.rs).
    let dir = tempdir().unwrap();
    let f = dir.path().join("drifted.txt");

    fs::write(&f, "v0").unwrap();
    let store = Arc::new(Mutex::new(SnapshotStore::new()));
    let id1 =
        store
            .lock()
            .unwrap()
            .record_mutation(&f, Some(b"v0".to_vec()), "v1", SnapshotOp::Edit);
    fs::write(&f, "v1").unwrap();
    // External change → preflight reports drift; user confirms.
    fs::write(&f, "external").unwrap();

    let actions = store.lock().unwrap().peek_restore(id1).unwrap();

    // Preflight: per-path dedup finds the drift.
    use std::collections::HashSet;
    let mut seen_paths: HashSet<std::path::PathBuf> = HashSet::new();
    let drifted: Vec<&aish_tools::fs::UndoResult> = actions
        .iter()
        .filter(|a| seen_paths.insert(a.path.clone()))
        .filter(|a| matches!(a.check_drift(), DriftStatus::Drifted { .. }))
        .collect();
    assert_eq!(drifted.len(), 1, "preflight must detect the drift");

    // User confirmed → recheck excludes the confirmed paths; the same
    // unchanged disk state must NOT be re-blocked.
    let confirmed_drift: HashSet<std::path::PathBuf> =
        drifted.iter().map(|a| a.path.clone()).collect();
    let mut recheck_seen: HashSet<std::path::PathBuf> = HashSet::new();
    let new_drift = actions
        .iter()
        .filter(|a| recheck_seen.insert(a.path.clone()))
        .filter(|a| {
            !confirmed_drift.contains(&a.path)
                && matches!(a.check_drift(), DriftStatus::Drifted { .. })
        })
        .count();
    assert_eq!(
        new_drift, 0,
        "confirmed drift must not be re-blocked by the recheck"
    );

    // The confirmed restore proceeds and overwrites the external change.
    let (ok, msg) = apply_restore_action(&actions[0], true, true);
    assert!(ok, "confirmed restore must proceed: {msg}");
    assert_eq!(fs::read_to_string(&f).unwrap(), "v0");
}

#[test]
fn issue_466_rollback_partial_failure_reporting() {
    // Two actions: the first restore succeeds, the second must genuinely
    // fail with an IO error so the partial-failure counting (success_count /
    // total) is actually exercised. A delete inside a read-only directory
    // reliably fails with EACCES on Linux.
    let dir = tempdir().unwrap();
    let f1 = dir.path().join("f1.txt");
    let f2 = dir.path().join("f2.txt");

    fs::write(&f1, "orig1").unwrap();
    let store = Arc::new(Mutex::new(SnapshotStore::new()));
    let id1 = store.lock().unwrap().record_mutation(
        &f1,
        Some(b"orig1".to_vec()),
        "new1",
        SnapshotOp::Edit,
    );
    fs::write(&f1, "new1").unwrap();
    // f2 is a "created" mutation (prior=None) → undo will delete it
    store
        .lock()
        .unwrap()
        .record_mutation(&f2, None, "created2", SnapshotOp::Write);
    fs::write(&f2, "created2").unwrap();

    let actions = store.lock().unwrap().peek_restore(id1).unwrap();
    assert_eq!(actions.len(), 2);
    // peek_restore returns newest-first: [delete f2, restore f1]. Reverse so
    // the f1 restore runs first, then the f2 delete fails — the same
    // mid-batch failure shape the /rollback loop reports on.
    let ordered: Vec<_> = actions.iter().rev().collect();

    let mut success_count = 0usize;
    let mut all_ok = true;
    let mut last_msg = String::new();
    for (i, action) in ordered.iter().enumerate() {
        // Lock the directory after the first action succeeded: the restore
        // of f1 is an atomic replace (temp file in the same dir), so it
        // needs a writable directory — it must run while writable. The f2
        // delete then fails with EACCES in the read-only dir.
        if i == 1 {
            let mut perms = fs::metadata(dir.path()).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
            fs::set_permissions(dir.path(), perms).unwrap();
        }
        let (ok, msg) = apply_restore_action(action, true, true);
        if ok {
            success_count += 1;
        } else {
            all_ok = false;
            last_msg = msg;
            break;
        }
    }

    // Restore directory permissions so tempdir cleanup succeeds.
    let mut perms = fs::metadata(dir.path()).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(dir.path(), perms).unwrap();

    assert!(!all_ok, "second action (delete in read-only dir) must fail");
    assert_eq!(success_count, 1, "exactly one action succeeded");
    assert!(!last_msg.is_empty(), "failure message must be reported");
    // The succeeded restore wrote f1's prior content back.
    assert_eq!(fs::read_to_string(&f1).unwrap(), "orig1");
    // The failed delete must not have removed the file.
    assert!(f2.exists(), "f2 must still exist after failed delete");
}

/// Mirror of apply_restore_action from app.rs for testing.
fn apply_restore_action(
    result: &aish_tools::fs::UndoResult,
    tolerate_missing: bool,
    force: bool,
) -> (bool, String) {
    use aish_tools::fs::{ApplyError, ApplyOutcome};
    match result.apply_to_disk_checked(tolerate_missing, force) {
        Ok(ApplyOutcome::Restored) => (true, "restored".to_string()),
        Ok(ApplyOutcome::Removed) => (true, "removed".to_string()),
        Err(ApplyError::Drifted) => (false, "drifted".to_string()),
        Err(ApplyError::Io(e)) => (false, e.to_string()),
    }
}
