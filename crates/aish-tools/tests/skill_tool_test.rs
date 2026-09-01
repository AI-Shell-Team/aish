use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use aish_llm::{LoopStatus, SpawnResult, Tool, ToolStrategy};
use aish_skills::SkillExecutionContext;
use aish_tools::{SkillInfo, SkillSpawnFn, SkillSpawnRequest, SkillTool};

fn skill(
    context: SkillExecutionContext,
    agent: Option<&str>,
    allowed_tools: Option<Vec<&str>>,
) -> SkillInfo {
    SkillInfo {
        name: "host-diagnose".to_string(),
        content: "Inspect the host with the provided read-only tools.".to_string(),
        description: "Diagnose host performance".to_string(),
        base_dir: "/tmp/host-diagnose".to_string(),
        context,
        agent: agent.map(str::to_string),
        allowed_tools: allowed_tools
            .map(|tools| tools.into_iter().map(str::to_string).collect::<Vec<_>>()),
        quarantined: false,
    }
}

fn make_tool(skill: SkillInfo, spawn_fn: Option<SkillSpawnFn>) -> SkillTool {
    let skill_name = skill.name.clone();
    let lookup = Box::new(move |name: &str| (name == skill_name).then(|| skill.clone()));
    let list = Box::new(|| vec!["host-diagnose".to_string()]);
    match spawn_fn {
        Some(spawn_fn) => SkillTool::with_spawn_fn(lookup, list, spawn_fn),
        None => SkillTool::new(lookup, list),
    }
}

#[test]
fn inline_skill_keeps_legacy_expansion_behavior() {
    let tool = make_tool(skill(SkillExecutionContext::Inline, None, None), None);

    let result = tool.execute(serde_json::json!({
        "skill_name": "host-diagnose",
        "args": "check disk pressure"
    }));

    assert!(result.ok);
    assert!(result.output.contains("Inspect the host"));
}

#[test]
fn sync_execute_rejects_subagent_skill_instead_of_inline_expand() {
    let tool = make_tool(
        skill(SkillExecutionContext::SubAgent, Some("troubleshoot"), None),
        None,
    );

    let result = tool.execute(serde_json::json!({
        "skill_name": "host-diagnose",
        "args": "check disk pressure"
    }));

    assert!(!result.ok);
    assert!(result.output.contains("context=subagent"));
    assert!(result.output.contains("async"));
    assert!(!result.output.contains("Inspect the host"));
}

#[tokio::test]
async fn subagent_skill_spawns_with_builtin_and_intersected_tools() {
    let captured = Arc::new(Mutex::new(None::<SkillSpawnRequest>));
    let captured_for_spawn = Arc::clone(&captured);
    let spawn_fn: SkillSpawnFn = Arc::new(move |_session, request| {
        *captured_for_spawn.lock().unwrap() = Some(request);
        Box::pin(async {
            Ok(SpawnResult {
                text: "disk IO is saturated".to_string(),
                status: LoopStatus::Complete,
                ..Default::default()
            })
        }) as Pin<Box<dyn Future<Output = Result<SpawnResult, String>> + Send>>
    });
    let tool = make_tool(
        skill(
            SkillExecutionContext::SubAgent,
            Some("troubleshoot"),
            Some(vec!["bash", "read_file", "write_file"]),
        ),
        Some(spawn_fn),
    );
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);

    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "skill_name": "host-diagnose",
                "args": "check disk pressure"
            }),
            &session,
        )
        .await;

    assert!(result.ok);
    assert_eq!(result.output, "disk IO is saturated");
    let request = captured.lock().unwrap().take().expect("spawn request");
    assert_eq!(request.definition.subagent_type, "troubleshoot");
    assert_eq!(request.prompt, "check disk pressure");
    assert_eq!(request.skill_name, "host-diagnose");
    assert_eq!(request.context_messages.len(), 1);
    assert!(
        request
            .definition
            .tool_execution_policy
            .enforce_read_only_bash
    );
    assert_eq!(request.context_messages[0].role, "user");
    assert!(request.context_messages[0]
        .content
        .as_ref()
        .and_then(|content| content.as_text_str())
        .is_some_and(|content| content.contains("Inspect the host")));
    let ToolStrategy::Allowlist(tools) = request.definition.tool_strategy else {
        panic!("skill-constrained troubleshoot must use an allowlist");
    };
    assert_eq!(tools, vec!["read_file", "bash"]);
}

#[tokio::test]
async fn general_purpose_skill_allowlist_does_not_make_bash_read_only() {
    let captured = Arc::new(Mutex::new(None::<SkillSpawnRequest>));
    let captured_for_spawn = Arc::clone(&captured);
    let spawn_fn: SkillSpawnFn = Arc::new(move |_session, request| {
        *captured_for_spawn.lock().unwrap() = Some(request);
        Box::pin(async {
            Ok(SpawnResult {
                text: "updated".to_string(),
                status: LoopStatus::Complete,
                ..Default::default()
            })
        })
    });
    let tool = make_tool(
        skill(
            SkillExecutionContext::SubAgent,
            Some("general-purpose"),
            Some(vec!["bash", "write_file"]),
        ),
        Some(spawn_fn),
    );
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);

    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "skill_name": "host-diagnose",
                "args": "update the generated configuration"
            }),
            &session,
        )
        .await;

    assert!(result.ok);
    let request = captured.lock().unwrap().take().expect("spawn request");
    assert!(
        !request
            .definition
            .tool_execution_policy
            .enforce_read_only_bash
    );
}

#[tokio::test]
async fn subagent_skill_loads_inline_when_already_in_subagent() {
    let spawn_fn: SkillSpawnFn = Arc::new(move |_session, _request| {
        Box::pin(async { panic!("nested skill must not spawn another sub-agent") })
    });
    let tool = make_tool(
        skill(SkillExecutionContext::SubAgent, Some("troubleshoot"), None),
        Some(spawn_fn),
    );
    let parent = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);
    let sub = parent.create_subsession();

    let result = tool
        .execute_async_in_session(
            serde_json::json!({
                "skill_name": "host-diagnose",
                "args": "check disk pressure"
            }),
            &sub,
        )
        .await;

    assert!(result.ok);
    assert!(result.output.contains("Inspect the host"));
}

#[tokio::test]
async fn subagent_skill_requires_task_args_in_parent_session() {
    let tool = make_tool(
        skill(SkillExecutionContext::SubAgent, Some("troubleshoot"), None),
        None,
    );
    let session = aish_llm::LlmSession::new("http://localhost", "key", "model", None, None);

    let result = tool
        .execute_async_in_session(serde_json::json!({"skill_name": "host-diagnose"}), &session)
        .await;

    assert!(!result.ok);
    assert!(result.output.contains("args"));
    assert!(result.output.contains("user's task"));
}
