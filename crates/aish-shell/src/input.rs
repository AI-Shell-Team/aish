use crate::types::{CommandCategory, InputIntent};

/// Classify user input into an intent category.
pub fn classify_input(input: &str) -> InputIntent {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return InputIntent::Empty;
    }
    if trimmed.starts_with(';') || trimmed.starts_with('\u{ff1b}') {
        return InputIntent::Ai;
    }
    if trimmed == "help" {
        return InputIntent::Help;
    }
    if trimmed.starts_with('/')
        && trimmed
            .split_whitespace()
            .next()
            .is_some_and(|cmd| matches!(cmd, "/model" | "/setup" | "/plan" | "/token"))
    {
        return InputIntent::SpecialCommand;
    }

    let cmd = trimmed.split_whitespace().next().unwrap_or("");
    match cmd {
        "cd" | "pwd" | "export" | "unset" | "pushd" | "popd" | "dirs" | "history" | "clear"
        | "exit" | "quit" | "su" | "sudo" => InputIntent::BuiltinCommand,
        _ => {
            // Check if the first word looks like a .aish script
            if cmd.ends_with(".aish") {
                InputIntent::ScriptCall
            } else {
                InputIntent::Command
            }
        }
    }
}

/// Extract the AI question text by stripping the leading `;` or full-width semicolon.
pub fn extract_ai_question(input: &str) -> String {
    let trimmed = input.trim();
    if let Some(rest) = trimmed.strip_prefix(';') {
        return rest.trim().to_string();
    }
    if let Some(stripped) = trimmed.strip_prefix('\u{ff1b}') {
        return stripped.trim().to_string();
    }
    trimmed.to_string()
}

/// Classify a command into its execution category.
pub fn classify_command(input: &str) -> CommandCategory {
    let cmd = input.split_whitespace().next().unwrap_or("");
    let basename = cmd.rsplit('/').next().unwrap_or(cmd);
    match basename {
        "cd" | "pushd" | "popd" | "dirs" | "pwd" | "export" | "unset" => {
            CommandCategory::BuiltinStateModify
        }
        "help" | "history" | "clear" | "version" => CommandCategory::BuiltinInfo,
        "su" | "sudo" => CommandCategory::PtyRequired,
        "exit" | "quit" | "logout" => CommandCategory::Rejected,
        _ => CommandCategory::External,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_empty() {
        assert_eq!(classify_input(""), InputIntent::Empty);
        assert_eq!(classify_input("   "), InputIntent::Empty);
    }

    #[test]
    fn test_classify_ai() {
        assert_eq!(classify_input("; hello"), InputIntent::Ai);
        assert_eq!(classify_input("；你好"), InputIntent::Ai);
        assert_eq!(classify_input("  ; spaced"), InputIntent::Ai);
    }

    #[test]
    fn test_classify_help() {
        assert_eq!(classify_input("help"), InputIntent::Help);
    }

    #[test]
    fn test_classify_special() {
        assert_eq!(classify_input("/model gpt-4"), InputIntent::SpecialCommand);
        assert_eq!(classify_input("/setup"), InputIntent::SpecialCommand);
        assert_eq!(classify_input("/plan"), InputIntent::SpecialCommand);
        assert_eq!(classify_input("/plan start"), InputIntent::SpecialCommand);
        assert_eq!(classify_input("/plan status"), InputIntent::SpecialCommand);
        assert_eq!(classify_input("/token"), InputIntent::SpecialCommand);
    }

    #[test]
    fn test_classify_builtin() {
        assert_eq!(classify_input("cd /tmp"), InputIntent::BuiltinCommand);
        assert_eq!(classify_input("pwd"), InputIntent::BuiltinCommand);
        assert_eq!(
            classify_input("export FOO=bar"),
            InputIntent::BuiltinCommand
        );
        assert_eq!(classify_input("exit"), InputIntent::BuiltinCommand);
    }

    #[test]
    fn test_classify_command() {
        assert_eq!(classify_input("ls -la"), InputIntent::Command);
        assert_eq!(classify_input("git status"), InputIntent::Command);
    }

    #[test]
    fn test_extract_ai_question() {
        assert_eq!(
            extract_ai_question("; how do I list files?"),
            "how do I list files?"
        );
        assert_eq!(extract_ai_question("；你好"), "你好");
        assert_eq!(
            extract_ai_question("  ; spaced question  "),
            "spaced question"
        );
    }

    #[test]
    fn test_classify_command_state_modify() {
        assert_eq!(
            classify_command("cd /tmp"),
            CommandCategory::BuiltinStateModify
        );
        assert_eq!(
            classify_command("export FOO=bar"),
            CommandCategory::BuiltinStateModify
        );
        assert_eq!(
            classify_command("pushd /home"),
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
            classify_command("unset FOO"),
            CommandCategory::BuiltinStateModify
        );
    }

    #[test]
    fn test_classify_command_info() {
        assert_eq!(classify_command("help"), CommandCategory::BuiltinInfo);
        assert_eq!(classify_command("history"), CommandCategory::BuiltinInfo);
        assert_eq!(classify_command("clear"), CommandCategory::BuiltinInfo);
        assert_eq!(classify_command("version"), CommandCategory::BuiltinInfo);
    }

    #[test]
    fn test_classify_command_pty_required() {
        assert_eq!(classify_command("su -"), CommandCategory::PtyRequired);
        assert_eq!(classify_command("sudo ls"), CommandCategory::PtyRequired);
        assert_eq!(
            classify_command("/usr/bin/sudo bash"),
            CommandCategory::PtyRequired
        );
    }

    #[test]
    fn test_classify_command_rejected() {
        assert_eq!(classify_command("exit"), CommandCategory::Rejected);
        assert_eq!(classify_command("quit"), CommandCategory::Rejected);
        assert_eq!(classify_command("logout"), CommandCategory::Rejected);
    }

    #[test]
    fn test_classify_command_external() {
        assert_eq!(classify_command("ls -la"), CommandCategory::External);
        assert_eq!(classify_command("git status"), CommandCategory::External);
        assert_eq!(classify_command("cargo build"), CommandCategory::External);
    }
}
