// Integration tests for Plan Mode state machine and lifecycle.
//
// This test module verifies:
// 1. Plan Mode state transitions (Normal <-> Planning)
// 2. Tool visibility filtering based on phase
// 3. Plan artifact directory creation
// 4. Plan mode tools (EnterPlanModeTool, ExitPlanModeTool)

use aish_core::plan::{
    build_default_plan_template, bump_draft_revision, create_new_plan_state, ensure_plan_artifact,
    filter_tools_for_phase, generate_plan_id, get_plan_artifacts_dir, PLANNING_VISIBLE_TOOLS,
    READ_ONLY_TOOLS, SIDE_EFFECT_TOOLS,
};
use aish_core::types::{PlanApprovalStatus, PlanModeState, PlanPhase};
use std::fs;

#[test]
fn test_plan_mode_lifecycle() {
    // Test 1: Default state is Normal phase
    let default_state = PlanModeState::default();
    assert_eq!(default_state.phase, PlanPhase::Normal);
    assert_eq!(default_state.approval_status, PlanApprovalStatus::Draft);

    // Test 2: create_new_plan_state creates Planning phase
    let planning_state = create_new_plan_state("test-session-uuid");
    assert_eq!(planning_state.phase, PlanPhase::Planning);
    assert_eq!(planning_state.source_session_uuid, "test-session-uuid");
    assert!(planning_state.plan_id.is_some());
    assert_eq!(planning_state.draft_revision, 0);
    assert_eq!(planning_state.approval_status, PlanApprovalStatus::Draft);

    // Test 3: Plan ID generation produces unique 12-character IDs
    let id1 = generate_plan_id();
    let id2 = generate_plan_id();
    assert_eq!(id1.len(), 12);
    assert_eq!(id2.len(), 12);
    assert_ne!(id1, id2);

    // Test 4: Bump draft revision works
    let mut state = create_new_plan_state("test");
    assert_eq!(state.draft_revision, 0);
    bump_draft_revision(&mut state);
    assert_eq!(state.draft_revision, 1);
    bump_draft_revision(&mut state);
    assert_eq!(state.draft_revision, 2);
}

#[test]
fn test_tool_visibility_filtering() {
    // Test 1: During Normal phase, all tools are visible
    let all_tools = vec![
        "read_file".to_string(),
        "bash_exec".to_string(),
        "write_file".to_string(),
        "edit_file".to_string(),
        "grep".to_string(),
        "glob".to_string(),
    ];

    let normal_filtered = filter_tools_for_phase(&all_tools, &PlanPhase::Normal, None);
    assert_eq!(normal_filtered.len(), 6);
    assert!(normal_filtered.contains(&"bash_exec".to_string()));

    // Test 2: During Planning phase, only planning-visible tools are allowed
    let planning_filtered = filter_tools_for_phase(&all_tools, &PlanPhase::Planning, None);
    // bash_exec should be filtered out during planning
    assert_eq!(planning_filtered.len(), 5);
    assert!(planning_filtered.contains(&"read_file".to_string()));
    assert!(planning_filtered.contains(&"write_file".to_string()));
    assert!(planning_filtered.contains(&"edit_file".to_string()));
    assert!(planning_filtered.contains(&"grep".to_string()));
    assert!(planning_filtered.contains(&"glob".to_string()));
    assert!(!planning_filtered.contains(&"bash_exec".to_string()));

    // Test 3: Verify PLANNING_VISIBLE_TOOLS constants
    assert!(PLANNING_VISIBLE_TOOLS.contains(&"read_file"));
    assert!(PLANNING_VISIBLE_TOOLS.contains(&"glob"));
    assert!(PLANNING_VISIBLE_TOOLS.contains(&"grep"));
    assert!(PLANNING_VISIBLE_TOOLS.contains(&"write_file"));
    assert!(PLANNING_VISIBLE_TOOLS.contains(&"edit_file"));
    assert!(PLANNING_VISIBLE_TOOLS.contains(&"exit_plan_mode"));
    assert!(!PLANNING_VISIBLE_TOOLS.contains(&"bash_exec"));

    // Test 4: Verify SIDE_EFFECT_TOOLS and READ_ONLY_TOOLS constants
    assert!(SIDE_EFFECT_TOOLS.contains(&"bash_exec"));
    assert!(SIDE_EFFECT_TOOLS.contains(&"write_file"));
    assert!(READ_ONLY_TOOLS.contains(&"read_file"));
    assert!(!READ_ONLY_TOOLS.contains(&"bash_exec"));
}

#[test]
fn test_plan_artifact_directory_creation() {
    // Get the plan artifacts directory
    let plans_dir = get_plan_artifacts_dir();
    assert!(plans_dir.ends_with("plans"));

    // Create a temporary test state
    let mut state = create_new_plan_state("test-session");

    // Ensure artifact directory and file are created
    let result = ensure_plan_artifact(&mut state);
    assert!(result.is_ok());
    assert!(state.artifact_path.is_some());

    let artifact_path = state.artifact_path.unwrap();
    assert!(artifact_path.contains("test-session"));
    assert!(artifact_path.contains("plan.md"));

    // Verify the artifact file exists
    assert!(fs::metadata(&artifact_path).is_ok());

    // Verify the artifact has default template content
    let content = fs::read_to_string(&artifact_path).unwrap();
    assert!(content.contains("# Plan"));
    assert!(content.contains("## Overview"));
    assert!(content.contains("## Implementation Steps"));

    // Clean up
    let _ = fs::remove_file(&artifact_path);
    let plan_dir = std::path::Path::new(&artifact_path).parent().unwrap();
    if plan_dir.exists() && plan_dir != plans_dir {
        let _ = fs::remove_dir_all(plan_dir);
    }
}

#[test]
fn test_default_plan_template() {
    let template = build_default_plan_template();
    assert!(template.contains("# Plan"));
    assert!(template.contains("## Overview"));
    assert!(template.contains("## Context"));
    assert!(template.contains("## Implementation Steps"));
    assert!(template.contains("## Testing Plan"));
    assert!(template.contains("## Notes"));
}

#[test]
fn test_plan_mode_state_fields() {
    let state = create_new_plan_state("session-123");

    // Verify all expected fields are present
    assert_eq!(state.phase, PlanPhase::Planning);
    assert!(state.plan_id.is_some());
    assert!(state.artifact_path.is_none()); // Not set until ensure_plan_artifact is called
    assert_eq!(state.draft_revision, 0);
    assert_eq!(state.approval_status, PlanApprovalStatus::Draft);
    assert!(state.summary.is_none());
    assert!(state.approved_artifact_path.is_none());
    assert!(state.approved_revision.is_none());
    assert!(state.approved_artifact_hash.is_none());
    assert!(state.approval_feedback_summary.is_none());
    assert_eq!(state.source_session_uuid, "session-123");
    assert!(!state.updated_at.is_empty());
}

#[test]
fn test_tool_filtering_with_empty_list() {
    let empty_tools = vec![];
    let filtered_normal = filter_tools_for_phase(&empty_tools, &PlanPhase::Normal, None);
    assert_eq!(filtered_normal.len(), 0);

    let filtered_planning = filter_tools_for_phase(&empty_tools, &PlanPhase::Planning, None);
    assert_eq!(filtered_planning.len(), 0);
}

#[test]
fn test_tool_filtering_with_unknown_tools() {
    let tools = vec![
        "unknown_tool_1".to_string(),
        "read_file".to_string(),
        "unknown_tool_2".to_string(),
    ];

    // Normal phase: all tools pass through
    let normal_filtered = filter_tools_for_phase(&tools, &PlanPhase::Normal, None);
    assert_eq!(normal_filtered.len(), 3);

    // Planning phase: only known planning tools pass through
    let planning_filtered = filter_tools_for_phase(&tools, &PlanPhase::Planning, None);
    assert_eq!(planning_filtered.len(), 1);
    assert_eq!(planning_filtered[0], "read_file");
}
