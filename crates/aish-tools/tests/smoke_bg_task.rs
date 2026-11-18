// Throwaway smoke test: exercise the full tool-call path a real LLM turn
// would take: bash(run_in_background) -> task_output -> task_list -> task_stop.
use std::thread;
use std::time::Duration;

use aish_llm::Tool;
use aish_tools::bash::BashTool;
use aish_tools::{TaskListTool, TaskOutputTool, TaskStopTool};

#[test]
fn smoke_full_background_flow() {
    // 1. Start a long-running task in the background.
    let bash = BashTool::new();
    let start = bash.execute(serde_json::json!({
        "command": "for i in 1 2 3 4 5; do echo tick-$i; sleep 0.3; done; echo DONE-MARK",
        "run_in_background": true
    }));
    assert!(start.ok, "start failed: {}", start.output);
    println!("start result:\n{}", start.output);
    let task_id = start.meta.as_ref().unwrap()["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();

    // 2. Immediately read: partial output is fine.
    let out_tool = TaskOutputTool::new();
    let first = out_tool.execute(serde_json::json!({"task_id": task_id}));
    assert!(first.ok);
    println!("first read:\n{}", first.output);

    // 3. Wait for completion, poll again.
    thread::sleep(Duration::from_secs(3));
    let second = out_tool.execute(serde_json::json!({"task_id": task_id}));
    assert!(second.ok);
    println!("second read:\n{}", second.output);
    let combined = format!("{}\n{}", first.output, second.output);
    assert!(combined.contains("tick-1"), "missing early output");
    assert!(combined.contains("DONE-MARK"), "missing completion output");
    assert!(combined.contains("completed"));

    // 4. task_list shows the finished task.
    let list = TaskListTool::new().execute(serde_json::json!({}));
    assert!(list.ok);
    assert!(list.output.contains(&task_id));
    println!("list:\n{}", list.output);

    // 5. Stop flow on a fresh running task.
    let start2 = bash.execute(serde_json::json!({
        "command": "sleep 30",
        "run_in_background": true
    }));
    assert!(start2.ok);
    let id2 = start2.meta.as_ref().unwrap()["task_id"].as_str().unwrap().to_string();
    thread::sleep(Duration::from_millis(200));
    let stop = TaskStopTool::new().execute(serde_json::json!({"task_id": id2}));
    assert!(stop.ok, "stop failed: {}", stop.output);
    println!("stop:\n{}", stop.output);

    // 6. task_list reflects the stopped task.
    let list2 = TaskListTool::new().execute(serde_json::json!({}));
    assert!(list2.output.contains(&id2));
    println!("final list:\n{}", list2.output);
}
