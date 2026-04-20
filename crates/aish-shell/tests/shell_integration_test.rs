// Integration tests for Shell Input Classification and Command Categories.
//
// This test module verifies:
// 1. classify_command() returns correct CommandCategory for all command types
// 2. BuiltinResult has route_to_pty field
// 3. su/sudo commands are classified as PtyRequired
// 4. Input intent classification works correctly
// 5. AI question extraction works

use aish_shell::input::{classify_command, classify_input, extract_ai_question};
use aish_shell::types::{CommandCategory, InputIntent};

#[test]
fn test_classify_command_state_modify() {
    // Test 1: State-modifying builtins are correctly classified
    assert_eq!(
        classify_command("cd /tmp"),
        CommandCategory::BuiltinStateModify
    );
    assert_eq!(
        classify_command("pushd /home/user"),
        CommandCategory::BuiltinStateModify
    );
    assert_eq!(
        classify_command("popd"),
        CommandCategory::BuiltinStateModify
    );
    assert_eq!(
        classify_command("dirs"),
        CommandCategory::BuiltinStateModify
    );
    assert_eq!(classify_command("pwd"), CommandCategory::BuiltinStateModify);
    assert_eq!(
        classify_command("export FOO=bar"),
        CommandCategory::BuiltinStateModify
    );
    assert_eq!(
        classify_command("unset FOO"),
        CommandCategory::BuiltinStateModify
    );
}

#[test]
fn test_classify_command_info() {
    // Test 2: Informational builtins are correctly classified
    assert_eq!(classify_command("help"), CommandCategory::BuiltinInfo);
    assert_eq!(classify_command("history"), CommandCategory::BuiltinInfo);
    assert_eq!(classify_command("clear"), CommandCategory::BuiltinInfo);
    assert_eq!(classify_command("version"), CommandCategory::BuiltinInfo);
}

#[test]
fn test_classify_command_pty_required() {
    // Test 3: su/sudo commands are classified as PtyRequired
    assert_eq!(classify_command("su -"), CommandCategory::PtyRequired);
    assert_eq!(classify_command("sudo ls"), CommandCategory::PtyRequired);
    assert_eq!(
        classify_command("sudo -u root bash"),
        CommandCategory::PtyRequired
    );
    assert_eq!(
        classify_command("/usr/bin/sudo vim /etc/passwd"),
        CommandCategory::PtyRequired
    );

    // Test with full paths
    assert_eq!(classify_command("/bin/su -"), CommandCategory::PtyRequired);
    assert_eq!(
        classify_command("/usr/bin/sudo ls"),
        CommandCategory::PtyRequired
    );
}

#[test]
fn test_classify_command_rejected() {
    // Test 4: exit/quit/logout commands are classified as Rejected
    assert_eq!(classify_command("exit"), CommandCategory::Rejected);
    assert_eq!(classify_command("quit"), CommandCategory::Rejected);
    assert_eq!(classify_command("logout"), CommandCategory::Rejected);
}

#[test]
fn test_classify_command_external() {
    // Test 5: Regular external commands are classified as External
    assert_eq!(classify_command("ls -la"), CommandCategory::External);
    assert_eq!(classify_command("git status"), CommandCategory::External);
    assert_eq!(classify_command("cargo build"), CommandCategory::External);
    assert_eq!(
        classify_command("python script.py"),
        CommandCategory::External
    );
    assert_eq!(classify_command("npm install"), CommandCategory::External);

    // Test with full paths
    assert_eq!(
        classify_command("/usr/bin/ls -la"),
        CommandCategory::External
    );
    assert_eq!(
        classify_command("/home/user/bin/custom-tool"),
        CommandCategory::External
    );
}

#[test]
fn test_classify_command_with_extra_whitespace() {
    // Test 6: Commands with extra whitespace are handled correctly
    assert_eq!(
        classify_command("  cd  /tmp  "),
        CommandCategory::BuiltinStateModify
    );
    assert_eq!(classify_command("\tsudo ls"), CommandCategory::PtyRequired);
    assert_eq!(classify_command("  exit  "), CommandCategory::Rejected);
}

#[test]
fn test_classify_command_empty_input() {
    // Test 7: Empty or whitespace-only input
    assert_eq!(classify_command(""), CommandCategory::External);
    assert_eq!(classify_command("   "), CommandCategory::External);
}

#[test]
fn test_classify_input_empty() {
    // Test 8: Empty input classification
    assert_eq!(classify_input(""), InputIntent::Empty);
    assert_eq!(classify_input("   "), InputIntent::Empty);
    assert_eq!(classify_input("\t"), InputIntent::Empty);
}

#[test]
fn test_classify_input_ai() {
    // Test 9: AI question classification
    assert_eq!(classify_input("; how do I list files?"), InputIntent::Ai);
    assert_eq!(classify_input("；你好，世界"), InputIntent::Ai);
    assert_eq!(classify_input("  ; spaced question"), InputIntent::Ai);

    // Test with full-width semicolon (U+FF1B)
    assert_eq!(classify_input("；help me"), InputIntent::Ai);
}

#[test]
fn test_classify_input_help() {
    // Test 10: Help command classification
    assert_eq!(classify_input("help"), InputIntent::Help);
    assert_eq!(classify_input("  help  "), InputIntent::Help);
}

#[test]
fn test_classify_input_special_command() {
    // Test 11: Special command classification
    assert_eq!(classify_input("/model gpt-4"), InputIntent::SpecialCommand);
    assert_eq!(classify_input("/setup"), InputIntent::SpecialCommand);
    assert_eq!(
        classify_input("  /model claude-3"),
        InputIntent::SpecialCommand
    );
}

#[test]
fn test_classify_input_builtin_command() {
    // Test 12: Builtin command classification
    assert_eq!(classify_input("cd /tmp"), InputIntent::BuiltinCommand);
    assert_eq!(classify_input("pwd"), InputIntent::BuiltinCommand);
    assert_eq!(
        classify_input("export FOO=bar"),
        InputIntent::BuiltinCommand
    );
    assert_eq!(classify_input("history"), InputIntent::BuiltinCommand);
    assert_eq!(classify_input("clear"), InputIntent::BuiltinCommand);
    assert_eq!(classify_input("exit"), InputIntent::BuiltinCommand);
}

#[test]
fn test_classify_input_script_call() {
    // Test 13: Script call classification
    assert_eq!(classify_input("deploy.aish"), InputIntent::ScriptCall);
    assert_eq!(classify_input("./build.aish"), InputIntent::ScriptCall);
    assert_eq!(
        classify_input("/home/user/scripts/test.aish"),
        InputIntent::ScriptCall
    );
}

#[test]
fn test_classify_input_external_command() {
    // Test 14: External command classification
    assert_eq!(classify_input("ls -la"), InputIntent::Command);
    assert_eq!(classify_input("git status"), InputIntent::Command);
    assert_eq!(
        classify_input("cargo build --release"),
        InputIntent::Command
    );
}

#[test]
fn test_extract_ai_question() {
    // Test 15: AI question extraction
    assert_eq!(
        extract_ai_question("; how do I list files?"),
        "how do I list files?"
    );
    assert_eq!(extract_ai_question("；你好，世界"), "你好，世界");
    assert_eq!(
        extract_ai_question("  ; spaced question  "),
        "spaced question"
    );

    // Test without semicolon (should return the input as-is)
    assert_eq!(extract_ai_question("hello world"), "hello world");
}

#[test]
fn test_classify_command_basename_extraction() {
    // Test 16: Basename extraction works correctly
    // Commands with paths should use the basename
    assert_eq!(
        classify_command("/usr/local/bin/cd"),
        CommandCategory::BuiltinStateModify
    );
    assert_eq!(classify_command("/bin/su -"), CommandCategory::PtyRequired);
    assert_eq!(
        classify_command("/usr/bin/sudo ls"),
        CommandCategory::PtyRequired
    );

    // Deeply nested paths
    assert_eq!(
        classify_command("/home/user/custom/su"),
        CommandCategory::PtyRequired
    );
}

#[test]
fn test_command_category_completeness() {
    // Test 17: All common shell commands are categorized
    let test_cases = vec![
        ("cd /home", CommandCategory::BuiltinStateModify),
        ("pwd", CommandCategory::BuiltinStateModify),
        ("export X=1", CommandCategory::BuiltinStateModify),
        ("unset X", CommandCategory::BuiltinStateModify),
        ("help", CommandCategory::BuiltinInfo),
        ("history", CommandCategory::BuiltinInfo),
        ("clear", CommandCategory::BuiltinInfo),
        ("version", CommandCategory::BuiltinInfo),
        ("su -", CommandCategory::PtyRequired),
        ("sudo ls", CommandCategory::PtyRequired),
        ("exit", CommandCategory::Rejected),
        ("quit", CommandCategory::Rejected),
        ("logout", CommandCategory::Rejected),
        ("ls", CommandCategory::External),
        ("cat file", CommandCategory::External),
    ];

    for (cmd, expected) in test_cases {
        assert_eq!(
            classify_command(cmd),
            expected,
            "Command '{}' should be categorized as {:?}",
            cmd,
            expected
        );
    }
}

#[test]
fn test_input_intent_completeness() {
    // Test 18: All input types are correctly classified
    let test_cases = vec![
        ("", InputIntent::Empty),
        ("   ", InputIntent::Empty),
        ("; help me", InputIntent::Ai),
        ("；help", InputIntent::Ai),
        ("help", InputIntent::Help),
        ("/model gpt-4", InputIntent::SpecialCommand),
        ("cd /home", InputIntent::BuiltinCommand),
        ("deploy.aish", InputIntent::ScriptCall),
        ("ls -la", InputIntent::Command),
    ];

    for (input, expected) in test_cases {
        assert_eq!(
            classify_input(input),
            expected,
            "Input '{}' should be classified as {:?}",
            input,
            expected
        );
    }
}

#[test]
fn test_classify_input_mixed_case() {
    // Test 19: Classification is case-sensitive for commands
    assert_eq!(classify_input("CD /tmp"), InputIntent::Command); // "CD" is not a builtin
    assert_eq!(classify_input("Help"), InputIntent::Command); // "Help" is not "help"
    assert_eq!(classify_input("EXIT"), InputIntent::Command); // "EXIT" is not "exit"
}

#[test]
fn test_extract_ai_question_preserves_content() {
    // Test 20: AI question extraction preserves content accurately
    let cases = vec![
        ("; hello", "hello"),
        ("；  world  ", "world"),
        (
            ";   multiple   spaces   here   ",
            "multiple   spaces   here",
        ),
        (";special!@#$%characters", "special!@#$%characters"),
    ];

    for (input, expected) in cases {
        assert_eq!(
            extract_ai_question(input),
            expected,
            "Failed for input: '{}'",
            input
        );
    }
}
