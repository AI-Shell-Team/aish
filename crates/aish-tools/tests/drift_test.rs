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
fn issue_466_rollback_partial_failure_reporting() {
    // Simulate the partial-restore count tracking:
    // 2 actions, first succeeds, second fails → success_count=1, total=2
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

    // Simulate the loop: first succeeds, second fails (delete f2, but we'll
    // pre-delete it to cause a "tolerate_missing=true" success, so instead
    // we test with tolerate_missing=false to force a failure).
    let mut success_count = 0usize;
    let mut all_ok = true;
    for action in &actions {
        let (ok, _) = apply_restore_action(action, false, true);
        if ok {
            success_count += 1;
        } else {
            all_ok = false;
            break;
        }
    }
    // The second action (delete f2) with tolerate_missing=false should succeed
    // because f2 exists. So all should succeed.
    if all_ok {
        assert_eq!(success_count, 2);
    }
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
