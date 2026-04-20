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
        ("error_detect", ERROR_DETECT_PROMPT),
        ("system_diagnose", SYSTEM_DIAGNOSE_PROMPT),
        ("skill", SKILL_PROMPT),
    ]
}

const ROLE_PROMPT: &str = r#"You are an AI assistant integrated into aish (AI Shell).
You help the user with shell commands, system administration, programming, and general questions."#;

const ORACLE_PROMPT: &str = r#"{{role_prompt}}

**Environment:**
- User: {{username}}@{{hostname}}
- OS: {{os_info}}
- Current directory: {{cwd}}
{{system_info}}
{{memory_context}}
{{skill_list}}
**Guidelines:**
- Be concise and practical
- When suggesting commands, format them in ```bash code blocks
- Explain what each command does briefly
- If the user asks in a language other than English, reply in that language
- For shell questions, prefer standard POSIX commands when possible"#;

const CMD_ERROR_PROMPT: &str = r#"You are an expert at debugging shell command errors.
Analyze the failed command and its error output, then suggest a corrected version.

**Environment:**
- User: {{username}}
- OS: {{os_info}}

**Failed Command:**
```
{{command}}
```

**Exit Code:** {{exit_code}}
{{stderr_section}}
**Rules:**
- Output ONLY the corrected command in a ```bash code block
- If the original command was correct but failed for external reasons, explain briefly
- Do not suggest destructive alternatives unless the user explicitly asked for one"#;

const ERROR_DETECT_PROMPT: &str = r#"Analyze the following command output and determine if it indicates an error.

**Command:** {{command}}
**Exit Code:** {{exit_code}}
**Output:**
```
{{output}}
```

Respond with YES if this is an error that needs correction, or NO if it's normal output."#;

const SYSTEM_DIAGNOSE_PROMPT: &str = r#"You are a system diagnostic agent. You investigate system issues using the available tools.

**System Info:**
- User: {{username}}@{{hostname}}
- OS: {{os_info}}
- CWD: {{cwd}}

Use bash and read_file tools to investigate the user's issue. Follow the ReAct pattern:
1. Thought: reason about what to check next
2. Action: call a tool to gather information
3. Observation: analyze the tool result
4. Repeat until you can provide a Final Answer

Format:
Thought: <your reasoning>
Action: <tool_name>(<json_args>)

When done:
Final Answer: <your diagnosis and recommendations>"#;

const SKILL_PROMPT: &str = r#"The user wants to use the skill "{{skill_name}}".

Skill description: {{skill_description}}
Skill content: {{skill_content}}

Follow the skill's instructions to help the user."#;

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
        vars.insert("username".to_string(), "testuser".to_string());
        vars.insert("hostname".to_string(), "testhost".to_string());
        vars.insert("os_info".to_string(), "Linux x86_64".to_string());
        vars.insert("cwd".to_string(), "/home/test".to_string());
        vars.insert("system_info".to_string(), String::new());
        vars.insert("memory_context".to_string(), String::new());
        vars.insert("skill_list".to_string(), String::new());
        let result = pm.render("oracle", &vars);
        assert!(result.contains("testuser@testhost"));
        assert!(result.contains("You are helpful."));
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
}
