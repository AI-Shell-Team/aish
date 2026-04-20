// Plan mode state machine and artifact management.
//
// This module implements the plan mode lifecycle:
// - Enter planning phase with restricted tool visibility
// - Create and manage plan artifacts (markdown files)
// - Track approval state and revisions
// - Create snapshots of approved plans

use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::types::{PlanApprovalStatus, PlanModeState, PlanPhase};

/// Tools visible during planning phase.
///
/// During planning, the AI can only use read-only tools plus
/// write_file/edit_file for creating the plan artifact.
pub const PLANNING_VISIBLE_TOOLS: &[&str] = &[
    "read_file",
    "glob",
    "grep",
    "ask_user",
    "memory",
    "write_file",
    "edit_file",
    "exit_plan_mode",
];

/// Tools visible when awaiting user approval.
///
/// When the plan has been submitted and is awaiting user approval,
/// only read-only tools plus ask_user and exit_plan_mode are available.
pub const AWAITING_APPROVAL_VISIBLE_TOOLS: &[&str] = &["read_file", "ask_user", "exit_plan_mode"];

/// Tools with side effects (for reference).
pub const SIDE_EFFECT_TOOLS: &[&str] = &["bash_exec", "python_exec", "write_file", "edit_file"];

/// Read-only tools (safe for planning phase).
pub const READ_ONLY_TOOLS: &[&str] = &["read_file", "glob", "grep", "ask_user", "memory"];

// ---------------------------------------------------------------------------
// Plan templates
// ---------------------------------------------------------------------------

/// A structured plan template for guiding plan creation.
#[derive(Debug, Clone)]
pub struct PlanTemplate {
    pub name: String,
    pub description: String,
    pub content: String,
}

/// Get all available plan templates.
pub fn get_available_templates() -> Vec<PlanTemplate> {
    vec![
        PlanTemplate {
            name: "default".to_string(),
            description:
                "Standard plan template with overview, context, steps, and testing sections."
                    .to_string(),
            content: build_default_plan_template(),
        },
        PlanTemplate {
            name: "bugfix".to_string(),
            description: "Bug fix template: Reproduce, Diagnose, Fix, Verify.".to_string(),
            content: build_bugfix_plan_template(),
        },
        PlanTemplate {
            name: "feature".to_string(),
            description: "Feature template: Requirements, Design, Implementation, Testing."
                .to_string(),
            content: build_feature_plan_template(),
        },
    ]
}

/// Build the bugfix plan template.
fn build_bugfix_plan_template() -> String {
    r#"# Bug Fix Plan

## Bug Description
What is the observed bug? Include error messages, stack traces, or symptoms.

## Reproduce
Steps to reliably reproduce the bug:
1. Prerequisites / environment setup
2. Exact steps to trigger the bug
3. Expected vs actual behavior

## Diagnosis
- Root cause analysis
- Relevant code paths and files
- Why the bug occurs

## Fix
1. Changes to make
2. Files to modify
3. Edge cases to handle

## Verification
- How to confirm the fix works
- Regression tests to add
- Manual testing steps

## Notes
Any additional context, related issues, or follow-ups.
"#
    .to_string()
}

/// Build the feature plan template.
fn build_feature_plan_template() -> String {
    r#"# Feature Plan

## Requirements
- Functional requirements (what the feature must do)
- Non-functional requirements (performance, security, etc.)
- Acceptance criteria

## Design
- High-level architecture / approach
- Key data structures and interfaces
- Integration points with existing code

## Implementation Steps
1. Step 1: Setup / scaffolding
2. Step 2: Core logic
3. Step 3: Integration
4. Step 4: Edge cases and error handling
5. Step 5: Documentation

## Testing Plan
- Unit tests (what to test, expected coverage)
- Integration tests
- Manual testing scenarios

## Rollout
- Feature flags / configuration
- Migration steps (if any)
- Monitoring and observability

## Notes
Assumptions, open questions, and risks.
"#
    .to_string()
}

/// Generate a new plan ID (first 12 chars of UUID v4).
pub fn generate_plan_id() -> String {
    let uuid = Uuid::new_v4();
    format!("{:x}", uuid.as_hyphenated())[0..12].to_string()
}

/// Create a new plan mode state in the planning phase.
pub fn create_new_plan_state(session_uuid: &str) -> PlanModeState {
    PlanModeState {
        phase: PlanPhase::Planning,
        plan_id: Some(generate_plan_id()),
        artifact_path: None,
        draft_revision: 0,
        approval_status: PlanApprovalStatus::Draft,
        summary: None,
        approved_artifact_path: None,
        approved_revision: None,
        approved_artifact_hash: None,
        approval_feedback_summary: None,
        source_session_uuid: session_uuid.to_string(),
        updated_at: utc_now_iso(),
    }
}

/// Ensure the plan artifact file exists.
///
/// Creates the plan directory and artifact file if they don't exist.
/// Returns an error if file creation fails.
pub fn ensure_plan_artifact(state: &mut PlanModeState) -> Result<(), std::io::Error> {
    if state.artifact_path.is_some() {
        return Ok(()); // Already exists
    }

    let plan_id = state
        .plan_id
        .as_ref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Plan ID not set"))?;

    let plans_dir = get_plan_artifacts_dir();
    let plan_dir = plans_dir.join(format!("{}-{}", state.source_session_uuid, plan_id));

    fs::create_dir_all(&plan_dir)?;

    let artifact_path = plan_dir.join("plan.md");
    let artifact_path_str = artifact_path.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid artifact path")
    })?;

    // Create the file with default template if it doesn't exist
    if !artifact_path.exists() {
        let mut file = fs::File::create(&artifact_path)?;
        file.write_all(build_default_plan_template().as_bytes())?;
    }

    state.artifact_path = Some(artifact_path_str.to_string());
    state.updated_at = utc_now_iso();
    Ok(())
}

/// Increment the draft revision number.
pub fn bump_draft_revision(state: &mut PlanModeState) {
    state.draft_revision += 1;
    state.updated_at = utc_now_iso();
}

/// Create an approved snapshot of the plan artifact.
///
/// Copies the current plan artifact to a snapshot directory
/// with its SHA256 hash as part of the path.
///
/// Returns the path to the snapshot file.
pub fn create_approved_snapshot(state: &mut PlanModeState) -> Result<PathBuf, std::io::Error> {
    let artifact_path = state
        .artifact_path
        .as_ref()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "No artifact path"))?;

    let hash = compute_artifact_hash(Path::new(artifact_path)).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Failed to compute hash")
    })?;

    let plans_dir = get_plan_artifacts_dir();
    let snapshot_dir = plans_dir.join("approved").join(&hash);

    fs::create_dir_all(&snapshot_dir)?;

    let snapshot_path = snapshot_dir.join("plan.md");
    fs::copy(artifact_path, &snapshot_path)?;

    let snapshot_path_str = snapshot_path.to_str().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid snapshot path")
    })?;

    state.approved_artifact_path = Some(snapshot_path_str.to_string());
    state.approved_revision = Some(state.draft_revision);
    state.approved_artifact_hash = Some(hash);
    state.updated_at = utc_now_iso();

    Ok(snapshot_path)
}

/// Compute SHA256 hash of a file's contents.
pub fn compute_artifact_hash(path: &Path) -> Option<String> {
    let contents = fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    Some(format!("{:x}", hasher.finalize()))
}

/// Read the text content of a plan artifact.
pub fn read_artifact_text(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|_| format!("(Failed to read artifact: {})", path))
}

/// Filter tool names based on the current plan phase and approval status.
///
/// During planning, only tools in PLANNING_VISIBLE_TOOLS are allowed.
/// When awaiting user approval, only tools in AWAITING_APPROVAL_VISIBLE_TOOLS are allowed.
/// During normal mode, all tools are visible.
pub fn filter_tools_for_phase(
    tool_names: &[String],
    phase: &PlanPhase,
    approval_status: Option<&PlanApprovalStatus>,
) -> Vec<String> {
    match phase {
        PlanPhase::Normal => tool_names.to_vec(),
        PlanPhase::Planning => {
            // If awaiting approval, use the restricted set
            if let Some(PlanApprovalStatus::AwaitingUser) = approval_status {
                tool_names
                    .iter()
                    .filter(|name| AWAITING_APPROVAL_VISIBLE_TOOLS.contains(&name.as_str()))
                    .cloned()
                    .collect()
            } else {
                tool_names
                    .iter()
                    .filter(|name| PLANNING_VISIBLE_TOOLS.contains(&name.as_str()))
                    .cloned()
                    .collect()
            }
        }
    }
}

/// Format feedback when the user requests changes to the plan.
///
/// This generates a clear message for the AI to understand what
/// needs to be revised in the plan.
pub fn format_changes_requested_feedback(feedback: &str, revision: u32) -> String {
    format!(
        "User requested changes to plan (revision {}):\n{}\n\nPlease revise the plan addressing this feedback.",
        revision, feedback
    )
}

/// Build the default plan template.
pub fn build_default_plan_template() -> String {
    r#"# Plan

## Overview
Brief description of what this plan covers.

## Context
Background information and constraints.

## Implementation Steps
1. First step
2. Second step
3. ...

## Testing Plan
How will this be tested?

## Notes
Any additional notes or considerations.
"#
    .to_string()
}

/// Get the directory where plan artifacts are stored.
///
/// Returns `~/.config/aish/plans/`
pub fn get_plan_artifacts_dir() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config_dir.join("aish").join("plans")
}

/// Get current UTC time in ISO 8601 format.
pub fn utc_now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Format as ISO 8601 (simplified, without timezone for now)
    format!("{}", now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_plan_id() {
        let id1 = generate_plan_id();
        let id2 = generate_plan_id();
        assert_eq!(id1.len(), 12);
        assert_eq!(id2.len(), 12);
        assert_ne!(id1, id2); // Should be unique
    }

    #[test]
    fn test_create_new_plan_state() {
        let state = create_new_plan_state("test-session-uuid");
        assert_eq!(state.phase, PlanPhase::Planning);
        assert_eq!(state.source_session_uuid, "test-session-uuid");
        assert!(state.plan_id.is_some());
        assert_eq!(state.draft_revision, 0);
        assert_eq!(state.approval_status, PlanApprovalStatus::Draft);
    }

    #[test]
    fn test_bump_draft_revision() {
        let mut state = create_new_plan_state("test");
        assert_eq!(state.draft_revision, 0);
        bump_draft_revision(&mut state);
        assert_eq!(state.draft_revision, 1);
        bump_draft_revision(&mut state);
        assert_eq!(state.draft_revision, 2);
    }

    #[test]
    fn test_filter_tools_normal() {
        let tools = vec![
            "read_file".to_string(),
            "bash_exec".to_string(),
            "write_file".to_string(),
        ];
        let filtered = filter_tools_for_phase(&tools, &PlanPhase::Normal, None);
        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"bash_exec".to_string()));
    }

    #[test]
    fn test_filter_tools_planning() {
        let tools = vec![
            "read_file".to_string(),
            "bash_exec".to_string(),
            "grep".to_string(),
            "write_file".to_string(),
        ];
        let filtered = filter_tools_for_phase(&tools, &PlanPhase::Planning, None);
        // bash_exec should be filtered out during planning
        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"read_file".to_string()));
        assert!(filtered.contains(&"grep".to_string()));
        assert!(filtered.contains(&"write_file".to_string()));
        assert!(!filtered.contains(&"bash_exec".to_string()));
    }

    #[test]
    fn test_filter_tools_awaiting_approval() {
        let tools = vec![
            "read_file".to_string(),
            "bash_exec".to_string(),
            "grep".to_string(),
            "write_file".to_string(),
            "ask_user".to_string(),
            "exit_plan_mode".to_string(),
        ];
        let filtered = filter_tools_for_phase(
            &tools,
            &PlanPhase::Planning,
            Some(&PlanApprovalStatus::AwaitingUser),
        );
        // Only read_file, ask_user, exit_plan_mode should be visible
        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"read_file".to_string()));
        assert!(filtered.contains(&"ask_user".to_string()));
        assert!(filtered.contains(&"exit_plan_mode".to_string()));
        assert!(!filtered.contains(&"grep".to_string()));
        assert!(!filtered.contains(&"write_file".to_string()));
        assert!(!filtered.contains(&"bash_exec".to_string()));
    }

    #[test]
    fn test_filter_tools_changes_requested_still_planning() {
        // When ChangesRequested, the AI should still be in Planning phase
        // with the normal planning tool set (not the restricted awaiting set)
        let tools = vec![
            "read_file".to_string(),
            "bash_exec".to_string(),
            "grep".to_string(),
            "write_file".to_string(),
        ];
        let filtered = filter_tools_for_phase(
            &tools,
            &PlanPhase::Planning,
            Some(&PlanApprovalStatus::ChangesRequested),
        );
        // Should use the normal planning set, not the restricted awaiting set
        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"read_file".to_string()));
        assert!(filtered.contains(&"grep".to_string()));
        assert!(filtered.contains(&"write_file".to_string()));
    }

    #[test]
    fn test_format_changes_requested_feedback() {
        let msg = format_changes_requested_feedback("Add more test cases", 2);
        assert!(msg.contains("revision 2"));
        assert!(msg.contains("Add more test cases"));
        assert!(msg.contains("revise the plan"));
    }

    #[test]
    fn test_build_default_plan_template() {
        let template = build_default_plan_template();
        assert!(template.contains("# Plan"));
        assert!(template.contains("## Overview"));
        assert!(template.contains("## Implementation Steps"));
        assert!(template.contains("## Testing Plan"));
    }

    #[test]
    fn test_get_available_templates() {
        let templates = get_available_templates();
        assert_eq!(templates.len(), 3);

        // Check template names
        let names: Vec<&str> = templates.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"default"));
        assert!(names.contains(&"bugfix"));
        assert!(names.contains(&"feature"));

        // Each template should have non-empty content
        for t in &templates {
            assert!(!t.description.is_empty());
            assert!(t.content.starts_with('#'));
        }
    }

    #[test]
    fn test_bugfix_template_content() {
        let templates = get_available_templates();
        let bugfix = templates.iter().find(|t| t.name == "bugfix").unwrap();
        assert!(bugfix.content.contains("Reproduce"));
        assert!(bugfix.content.contains("Diagnosis"));
        assert!(bugfix.content.contains("Fix"));
        assert!(bugfix.content.contains("Verification"));
    }

    #[test]
    fn test_feature_template_content() {
        let templates = get_available_templates();
        let feature = templates.iter().find(|t| t.name == "feature").unwrap();
        assert!(feature.content.contains("Requirements"));
        assert!(feature.content.contains("Design"));
        assert!(feature.content.contains("Implementation"));
        assert!(feature.content.contains("Testing"));
    }

    #[test]
    fn test_compute_artifact_hash() {
        use std::io::Write;
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_hash.txt");

        {
            let mut f = fs::File::create(&test_file).unwrap();
            f.write_all(b"test content").unwrap();
        }

        let hash = compute_artifact_hash(&test_file);
        assert!(hash.is_some());
        assert_eq!(hash.unwrap().len(), 64); // SHA256 is 64 hex chars

        fs::remove_file(&test_file).ok();
    }

    #[test]
    fn test_read_artifact_text() {
        use std::io::Write;
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_read.txt");

        {
            let mut f = fs::File::create(&test_file).unwrap();
            f.write_all(b"hello world").unwrap();
        }

        let content = read_artifact_text(test_file.to_str().unwrap());
        assert_eq!(content, "hello world");

        fs::remove_file(&test_file).ok();
    }

    #[test]
    fn test_planning_visible_tools_constants() {
        assert!(PLANNING_VISIBLE_TOOLS.contains(&"read_file"));
        assert!(PLANNING_VISIBLE_TOOLS.contains(&"exit_plan_mode"));
        assert!(!PLANNING_VISIBLE_TOOLS.contains(&"bash_exec"));
    }

    #[test]
    fn test_awaiting_approval_visible_tools_constants() {
        assert!(AWAITING_APPROVAL_VISIBLE_TOOLS.contains(&"read_file"));
        assert!(AWAITING_APPROVAL_VISIBLE_TOOLS.contains(&"ask_user"));
        assert!(AWAITING_APPROVAL_VISIBLE_TOOLS.contains(&"exit_plan_mode"));
        assert!(!AWAITING_APPROVAL_VISIBLE_TOOLS.contains(&"write_file"));
        assert!(!AWAITING_APPROVAL_VISIBLE_TOOLS.contains(&"grep"));
        assert!(!AWAITING_APPROVAL_VISIBLE_TOOLS.contains(&"bash_exec"));
    }
}
