//! Regression guards for sub-agent termination semantics (AC-7, AC-13).

#[test]
fn agents_modules_do_not_use_react_or_diagnose_agents() {
    let spawn_src = include_str!("../src/agents/spawn.rs");
    let tool_loop_src = include_str!("../src/agents/tool_loop.rs");
    let outcome_src = include_str!("../src/agents/outcome.rs");
    for (name, src) in [
        ("spawn.rs", spawn_src),
        ("tool_loop.rs", tool_loop_src),
        ("outcome.rs", outcome_src),
    ] {
        assert!(
            !src.contains("ReActAgent::"),
            "{name} must not use ReActAgent (AC-7)"
        );
        assert!(
            !src.contains("DiagnoseAgent::"),
            "{name} must not use DiagnoseAgent (AC-7)"
        );
    }
}

#[test]
fn agents_modules_do_not_persist_transcripts() {
    let spawn_src = include_str!("../src/agents/spawn.rs");
    let tool_loop_src = include_str!("../src/agents/tool_loop.rs");
    let outcome_src = include_str!("../src/agents/outcome.rs");
    for (name, src) in [
        ("spawn.rs", spawn_src),
        ("tool_loop.rs", tool_loop_src),
        ("outcome.rs", outcome_src),
    ] {
        assert!(
            !src.contains("write_transcript"),
            "{name} must not persist sub-agent transcripts (AC-13)"
        );
        assert!(
            !src.contains("std::fs::write"),
            "{name} must not write files during spawn (AC-13)"
        );
    }
}
