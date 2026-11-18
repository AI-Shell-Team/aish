//! LLM tool wrappers over the background task registry:
//! `task_output`, `task_list`, `task_stop`.

use aish_llm::{PreflightResult, Tool, ToolResult};

use super::{list_tasks, task_output, task_stop, TaskStatus, DEFAULT_READ_BYTES};

fn t(key: &str, args: &[(&str, &str)]) -> String {
    let map: std::collections::HashMap<String, String> = args
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    aish_i18n::t_with_args(key, &map)
}

// Cached descriptions (translated once).
static DESC_OUTPUT: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| t("tools.task_output.description", &[]));
static DESC_LIST: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| t("tools.task_list.description", &[]));
static DESC_STOP: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| t("tools.task_stop.description", &[]));

// ---------------------------------------------------------------------------
// TaskOutputTool
// ---------------------------------------------------------------------------

pub struct TaskOutputTool;

impl Default for TaskOutputTool {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskOutputTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for TaskOutputTool {
    fn name(&self) -> &str {
        "task_output"
    }

    fn description(&self) -> &str {
        &DESC_OUTPUT
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Task id returned when the command was started with run_in_background"
                },
                "max_bytes": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum bytes of new output to return (default 4096). Poll again to read more."
                }
            },
            "required": ["task_id"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
            return ToolResult::error(t("tools.task_output.missing_task_id", &[]));
        };
        let max_bytes = args
            .get("max_bytes")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).min(256 * 1024))
            .unwrap_or(DEFAULT_READ_BYTES);

        match task_output(task_id, max_bytes) {
            Ok((text, cursor, status)) => {
                let header = match status {
                    TaskStatus::Running => {
                        t("tools.task_output.running", &[("cursor", &cursor.to_string())])
                    }
                    _ => t(
                        "tools.task_output.finished",
                        &[("status", &status.to_string()), ("cursor", &cursor.to_string())],
                    ),
                };
                let body = if text.is_empty() {
                    t("tools.task_output.no_new_output", &[])
                } else {
                    text
                };
                ToolResult {
                    ok: true,
                    output: format!("<status>{header}</status>\n{body}"),
                    meta: Some(serde_json::json!({
                        "task_id": task_id,
                        "cursor": cursor,
                        "status": status.to_string(),
                    })),
                }
            }
            Err(e) => ToolResult::error(e),
        }
    }
}

// ---------------------------------------------------------------------------
// TaskListTool
// ---------------------------------------------------------------------------

pub struct TaskListTool;

impl Default for TaskListTool {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskListTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for TaskListTool {
    fn name(&self) -> &str {
        "task_list"
    }

    fn description(&self) -> &str {
        &DESC_LIST
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let tasks = list_tasks();
        if tasks.is_empty() {
            return ToolResult {
                ok: true,
                output: t("tools.task_list.empty", &[]),
                meta: None,
            };
        }
        let mut lines = Vec::with_capacity(tasks.len());
        for task in &tasks {
            let exit = task
                .exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_string());
            lines.push(format!(
                "{}\t{}\texit={}\t{}",
                task.task_id,
                task.status,
                exit,
                task.command
            ));
        }
        ToolResult {
            ok: true,
            output: format!("task_id\tstatus\texit\tcommand\n{}", lines.join("\n")),
            meta: Some(serde_json::json!({
                "count": tasks.len(),
                "running": tasks.iter().filter(|t| t.status == TaskStatus::Running).count(),
            })),
        }
    }
}

// ---------------------------------------------------------------------------
// TaskStopTool
// ---------------------------------------------------------------------------

pub struct TaskStopTool;

impl Default for TaskStopTool {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskStopTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        "task_stop"
    }

    fn description(&self) -> &str {
        &DESC_STOP
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "Task id to stop (from task_list or the spawn result)"
                }
            },
            "required": ["task_id"]
        })
    }

    fn preflight(&self, _args: &serde_json::Value) -> PreflightResult {
        PreflightResult::Allow
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let Some(task_id) = args.get("task_id").and_then(|v| v.as_str()) else {
            return ToolResult::error(t("tools.task_stop.missing_task_id", &[]));
        };
        match task_stop(task_id) {
            Ok(code) => ToolResult {
                ok: true,
                output: t(
                    "tools.task_stop.stopped",
                    &[("task_id", task_id), ("exit_code", &code.to_string())],
                ),
                meta: Some(serde_json::json!({"task_id": task_id, "exit_code": code})),
            },
            Err(e) => ToolResult::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task;

    #[test]
    fn task_output_tool_reports_missing_param() {
        let tool = TaskOutputTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(!result.ok);
    }

    #[test]
    fn task_output_tool_unknown_task() {
        let tool = TaskOutputTool::new();
        let result = tool.execute(serde_json::json!({"task_id": "does-not-exist"}));
        assert!(!result.ok);
        assert!(result.output.contains("does-not-exist"));
    }

    #[test]
    fn task_list_tool_empty_or_items() {
        let tool = TaskListTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(result.ok);
    }

    #[test]
    fn task_list_tool_lists_spawned_task() {
        let id = task::spawn_background("echo list-me").unwrap();
        let tool = TaskListTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(result.ok);
        assert!(result.output.contains(&id), "should list {id}");
        assert!(result.output.contains("list-me"));
    }

    #[test]
    fn task_output_tool_reads_spawned_output() {
        let id = task::spawn_background("echo tool-out-42").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let tasks = task::list_tasks();
            let done = tasks
                .iter()
                .any(|t| t.task_id == id && t.status != TaskStatus::Running);
            if done || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let tool = TaskOutputTool::new();
        let result = tool.execute(serde_json::json!({"task_id": id}));
        assert!(result.ok);
        assert!(result.output.contains("tool-out-42"), "got: {}", result.output);
        assert!(result.output.contains("completed"));
    }

    #[test]
    fn task_stop_tool_missing_param() {
        let tool = TaskStopTool::new();
        let result = tool.execute(serde_json::json!({}));
        assert!(!result.ok);
    }
}
