use std::collections::HashMap;
use std::path::PathBuf;

use crate::template::render_template;

/// Manages prompt templates loaded from disk with embedded fallbacks.
///
/// Templates are stored as `.md` files in a configurable directory
/// (default: `~/.config/aish/prompts/`). If a file is missing, an
/// embedded default is used instead.
pub struct PromptManager {
    dir: PathBuf,
    cache: HashMap<String, String>,
}

impl PromptManager {
    /// Create a new PromptManager that loads templates from `dir`.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            cache: HashMap::new(),
        }
    }

    /// Create with the default XDG prompts directory.
    pub fn default_dir() -> Self {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("aish")
            .join("prompts");
        Self::new(dir)
    }

    /// Load all known templates, populating the cache.
    pub fn load_all(&mut self) {
        for &(name, _) in default_templates() {
            self.load_template(name);
        }
    }

    /// Reload all templates (clears cache and reloads).
    pub fn reload(&mut self) {
        self.cache.clear();
        self.load_all();
    }

    /// Get a template by name, loading from disk or using the embedded default.
    pub fn get(&mut self, name: &str) -> &str {
        if !self.cache.contains_key(name) {
            self.load_template(name);
        }
        self.cache.get(name).map(|s| s.as_str()).unwrap_or("")
    }

    /// Render a template with the given variables.
    pub fn render(&mut self, name: &str, vars: &HashMap<String, String>) -> String {
        let template = self.get(name).to_string();
        render_template(&template, vars)
    }

    /// Render the static core of the oracle template with session-static values.
    /// The CWD is left empty since it changes per call. This produces a stable
    /// string within a session, enabling KV-cache prefix hits across calls.
    pub fn render_static_core(
        &mut self,
        uname_info: &str,
        user_nickname: &str,
        os_info: &str,
        basic_env_info: &str,
        output_language: &str,
    ) -> String {
        let role_prompt = self.get("role").to_string();
        let mut vars = HashMap::new();
        vars.insert("role_prompt".to_string(), role_prompt);
        vars.insert("uname_info".to_string(), uname_info.to_string());
        vars.insert("user_nickname".to_string(), user_nickname.to_string());
        vars.insert("os_info".to_string(), os_info.to_string());
        vars.insert("basic_env_info".to_string(), basic_env_info.to_string());
        vars.insert("output_language".to_string(), output_language.to_string());
        vars.insert("cwd".to_string(), String::new());
        self.render("oracle", &vars)
    }

    /// Format the dynamic environment block with per-call runtime info.
    /// Only CWD changes between calls; appended after the static core so that
    /// the core prefix stays cacheable.
    pub fn render_env_block(&self, cwd: &str) -> String {
        format!("\n**Environment Update:**\n- Current directory: {}", cwd)
    }

    /// Load a single template from disk, falling back to embedded default.
    fn load_template(&mut self, name: &str) {
        let path = self.dir.join(format!("{}.md", name));
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                self.cache.insert(name.to_string(), content);
                return;
            }
        }
        // Fallback to embedded default
        if let Some(default) = default_templates()
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
        {
            self.cache.insert(name.to_string(), default.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Embedded default templates
// ---------------------------------------------------------------------------

fn default_templates() -> &'static [(&'static str, &'static str)] {
    &[
        ("role", ROLE_PROMPT),
        ("oracle", ORACLE_PROMPT),
        ("cmd_error", CMD_ERROR_PROMPT),
        ("failure_diagnose", FAILURE_DIAGNOSE_PROMPT),
        ("skill", SKILL_PROMPT),
    ]
}

const ROLE_PROMPT: &str = r#"# ROLE
 You are **AI-Shell**, a shell with AI capabilities.

You are capable of running Linux commands and tools. You can use the tools to help the user to complete the task or diagnose the problem."#;

const ORACLE_PROMPT: &str = r#"{{role_prompt}}

## System Information
- Runtime environment: {{uname_info}}
- User nickname: {{user_nickname}}
- OS / distro: {{os_info}}
- Basic environment:
{{basic_env_info}}

## Tone and Style
You should be concise, direct, and to the point. When you run a non-trivial bash command, you should explain what the command does and why you are running it, to make sure the user understands what you are doing (this is especially important when you are running a command that will make changes to the user's system).

Remember that your output will be displayed on a command line interface. Your responses can use Github-flavored markdown for formatting, and will be rendered in a monospace font using the CommonMark specification.

Output text to communicate with the user; all text you output outside of tool use is displayed to the user. Only use tools to complete tasks. Never use tools like Bash or code comments as means to communicate with the user during the session.

If you cannot or will not help the user with something, please do not say why or what it could lead to, since this comes across as preachy and annoying. Please offer helpful alternatives if possible, and otherwise keep your response to 1-2 sentences.

Only use emojis if the user explicitly requests it. Avoid using emojis in all communication unless asked.

IMPORTANT: You should minimize output tokens as much as possible while maintaining helpfulness, quality, and accuracy. Only address the specific query or task at hand, avoiding tangential information unless absolutely critical for completing the request. If you can answer in 1-3 sentences or a short paragraph, please do.

IMPORTANT: You should NOT answer with unnecessary preamble or postamble (such as explaining your code or summarizing your action), unless the user asks you to.

IMPORTANT: You should only focus on the last command execution. Previously entered historical commands can only be used as a reference, and the weight will be very low.

IMPORTANT: Keep your responses short, since they will be displayed on a command line interface. You MUST answer concisely with fewer than 4 lines (not including tool use or code generation), unless user asks for detail. Answer the user's question directly, without elaboration, explanation, or details. One word answers are best. Avoid introductions, conclusions, and explanations. You MUST avoid text before/after your response, such as "The answer is <answer>.", "Here is the content of the file..." or "Based on the information provided, the answer is..." or "Here is what I will do next...". Here are some examples to demonstrate appropriate verbosity:

<example>
user: ? 2 + 2
assistant: 4
</example>

<example>
user: ? what is 2+2?
assistant: 4
</example>

<example>
user: ? is 11 a prime number?
assistant: Yes
</example>


IMPORTANT: Response in {{output_language}}.

## Proactiveness
You are allowed to be proactive, but only when the user asks you to do something. You should strive to strike a balance between:
- Doing the right thing when asked, including taking actions and follow-up actions
- try to explore more information from the system, and provide more accurate and concise feedback to the user.
- if the task is not finished or encountered an error, you may try to continue to explore alternative solutions.


## Core Behavior
You can run commands like a shell, but you monitor each command's stdout and stderr; that output becomes context for later turns. Use it to give accurate, concise, high-value feedback — for example, explain why a command failed and suggest a corrected command, or when the user asks in natural language, understand their intent and propose a solution. A previous tool result is only for judgment; do not reject the user's new request based on it.
Tool results and user messages may include <system-reminder> or other tags. Tags contain information from the system. They bear no direct relation to the specific tool results or user messages in which they appear.

## Tool choice
Follow each tool's description in the tool list for routing and delegation. Do not repeat or override those rules here.

### Shell output offload rules (important)
- Long shell output may be offloaded to the filesystem; you will see this in the output (offload tags). When you need full details, read the corresponding offload file.
- `<stdout>`/`<stderr>` may be previews, not the full output.
- When `<offload>` has `status` `offloaded`, full output was written to a file; prefer `stdout_clean_path`/`stderr_clean_path`, and if those are missing or unavailable fall back to `stdout_path`/`stderr_path` (read `meta_path` if needed) rather than concluding from the preview alone.
- When `status` is `inline`, treat in-tag content as the primary output; when `failed`, analyze from the preview and note the offload failure.


## Long-running and interactive commands
When the user wants a **long-running** or **interactive** command, **do not** use the `bash` tool.

### Recognizing long-running / interactive requests
Including but not limited to:
- **Live system monitoring**: e.g. watch processes, CPU usage, memory, I/O, dynamic process display
- **Editors**: open vim/nano or an editor (if the goal is only to change file contents, prefer the edit_file tool instead of starting an interactive editor)
- **Network tools**: connect to servers, continuous ping, remote login, network tests
- **Continuous monitoring**: tail logs live, watch file changes, follow system logs
- **Database clients**: connect to MySQL, PostgreSQL, SQLite, etc.
- **Language REPLs**: Python, Node.js, interactive interpreters
- **Pagers**: browse large files or long documents with less/more
- **Other interactive tools**: tmux/screen sessions, file transfer tools

### Tell the user to run it themselves
<example>
{
    content: "To edit a.txt, run:\n`vim a.txt`",
    role: "assistant",
    tool_calls: null,
    function_call: null,
    provider_specific_fields: {
        refusal: null
    }
}
</example>"#;

const CMD_ERROR_PROMPT: &str = r#"{{role_prompt}}
### Critical rules
1. The **content** field must be a string, never an object.
2. The **tool_calls** field must be an array containing all tool calls.
3. Tool call details must live in the **tool_calls** array, never in **content**.

---


## System Information
- Runtime environment: {{uname_info}}
- User nickname: {{user_nickname}}
- OS / distro: {{os_info}}
- Basic environment:
{{basic_env_info}}
{{remote_env_info}}

## Tone and Style
You should be concise, direct, and to the point. Response in {{output_language}}.

## Task
Given a failed command (return code != 0) and its output, analyze why it failed and provide an accurate fix. If there is no suitable fix, return an empty command string.

Command exit code: {{exit_code}}

### Output format
- Output **exactly one** JSON code block with no extra text (no explanation, prefix, suffix, or Markdown outside the block).
- Wrap the full JSON in a ```json code fence.
- The JSON must be complete and parseable; do not split it outside the fence.
- If there is no suitable fix, use the same JSON shape with an empty `command` string.

```json
{
  "type": "corrected_command",
  "command": "corrected full command or empty string",
  "description": "brief explanation of the fix and what the command does, or why no fix is available"
}
```"#;

const FAILURE_DIAGNOSE_PROMPT: &str = r#"{{role_prompt}}

## System Information
- Runtime environment: {{uname_info}}
- User nickname: {{user_nickname}}
- OS / distro: {{os_info}}
- Basic environment:
{{basic_env_info}}

## Tone and Style
You should be concise, direct, and to the point. Response in {{output_language}}.

## Task
The previous shell command failed. You are in **read-only diagnosis mode**:
- Use grep, glob, read_file, and bash to gather evidence (e.g. which, journalctl, systemctl status)
- **Do not** write files, change configuration, install software, start/stop services, or otherwise mutate system state
- When done, stop tool use and respond with the diagnosis report JSON in your **final assistant message**

## Failure context
- Failed command: {{failed_command}}
- Exit code: {{exit_code}}
- Working directory: {{cwd}}
- Command output:
```
{{command_output}}
```

## Output format
Your final message must contain **only** a JSON object (optionally wrapped in a ```json fence), shaped as:

```json
{
  "type": "diagnose_report",
  "root_cause": "brief failure reason",
  "evidence": ["evidence 1", "evidence 2"],
  "suggested_fix": "suggested fix command or null",
  "verify_commands": ["read-only verify command 1"],
  "risk_notes": "risk notes or null",
  "confidence": "high"
}
```

- `root_cause` and `evidence` (non-empty array) are required
- Default `suggested_fix` to `null`. Only set it when there is **exactly one** clear, uniquely recommended fix
- If temporary vs permanent, edit file A vs file B, multiple packages, or any other plausible alternative exists, you **must** set `suggested_fix` to `null` and describe the options in `risk_notes`. Do **not** pick a default among alternatives just to fill the field
- When set, `suggested_fix` must be one single-line pasteable shell command (no menus, numbered lists, or prose). A short `cmd1 && cmd2` chain is allowed only for that unique fix
- Commands in `verify_commands` must be single-line read-only checks
- `confidence` must be one of: high / medium / low"#;

const SKILL_PROMPT: &str = r#"Base directory for this skill: {{base_dir}}

{{skill_content}}

Skill arguments: {{skill_args}}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_dir_loads() {
        let mut pm = PromptManager::default_dir();
        // Should not panic, even if dir doesn't exist
        let role = pm.get("role").to_string();
        assert!(!role.is_empty());
    }

    #[test]
    fn test_render_oracle() {
        let mut pm = PromptManager::new("/nonexistent");
        let mut vars = HashMap::new();
        vars.insert("role_prompt".to_string(), "You are helpful.".to_string());
        vars.insert(
            "uname_info".to_string(),
            "Linux testhost 6.1.0 x86_64".to_string(),
        );
        vars.insert("user_nickname".to_string(), "testuser".to_string());
        vars.insert("os_info".to_string(), "Linux x86_64".to_string());
        vars.insert("basic_env_info".to_string(), String::new());
        vars.insert("output_language".to_string(), "English".to_string());
        vars.insert("cwd".to_string(), "/home/test".to_string());
        let result = pm.render("oracle", &vars);
        assert!(result.contains("testuser"));
        assert!(result.contains("You are helpful."));
        assert!(result.contains("Linux testhost"));
        assert!(
            !result.contains("工具的选择原则"),
            "oracle should not duplicate per-tool routing removed in Phase B"
        );
        assert!(
            !result.contains("write_file`工具"),
            "oracle should not name individual tool routing rules"
        );
        assert!(
            !contains_cjk(&result),
            "oracle embedded template should be English-only"
        );
        assert!(
            result.contains("Follow each tool's description"),
            "oracle should delegate routing to tool descriptions"
        );
        assert!(
            !result.contains("Sub-agent delegation"),
            "oracle should not duplicate sub-agent routing"
        );
        assert!(
            !result.contains("subagent_type=explore"),
            "oracle should not embed per-tool routing tables"
        );
    }

    fn contains_cjk(text: &str) -> bool {
        text.chars()
            .any(|ch| ('\u{4e00}'..='\u{9fff}').contains(&ch))
    }

    #[test]
    fn test_embedded_prompt_templates_are_english() {
        let mut pm = PromptManager::new("/nonexistent");
        for &(name, _) in default_templates() {
            let template = pm.get(name).to_string();
            assert!(
                !contains_cjk(&template),
                "template {name} should not contain CJK characters"
            );
        }
    }

    #[test]
    fn test_reload_clears_cache() {
        let mut pm = PromptManager::new("/nonexistent");
        let _ = pm.get("role");
        assert!(pm.cache.contains_key("role"));
        pm.reload();
        // After reload, cache should be repopulated
        assert!(pm.cache.contains_key("role"));
    }

    #[test]
    fn test_render_static_core_stable() {
        let mut pm = PromptManager::new("/nonexistent");
        let core1 = pm.render_static_core(
            "Linux test 6.1.0 x86_64",
            "testuser",
            "Linux x86_64",
            "",
            "English",
        );
        let core2 = pm.render_static_core(
            "Linux test 6.1.0 x86_64",
            "testuser",
            "Linux x86_64",
            "",
            "English",
        );
        assert_eq!(core1, core2, "static core must be identical across calls");
        assert!(core1.contains("testuser"));
    }

    #[test]
    fn test_render_env_block_format() {
        let pm = PromptManager::new("/nonexistent");
        let block = pm.render_env_block("/home/alice");
        assert!(block.starts_with("\n**Environment Update:**"));
        assert!(block.contains("/home/alice"));
    }
}
