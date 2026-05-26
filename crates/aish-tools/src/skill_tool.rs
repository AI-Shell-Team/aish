use aish_llm::{Tool, ToolResult};

/// Callback type for looking up a skill by name.
pub type SkillLookupFn = Box<dyn Fn(&str) -> Option<SkillInfo> + Send + Sync>;
pub type SkillListFn = Box<dyn Fn() -> Vec<String> + Send + Sync>;

/// Skill information returned by the lookup callback.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub content: String,
    pub description: String,
    pub base_dir: String,
}

/// Tool for invoking skill plugins within the AI conversation.
pub struct SkillTool {
    lookup: SkillLookupFn,
    list: SkillListFn,
}

impl SkillTool {
    pub fn new(lookup: SkillLookupFn, list: SkillListFn) -> Self {
        Self { lookup, list }
    }

    /// Create a no-op skill tool that always returns "no skills".
    pub fn noop() -> Self {
        Self {
            lookup: Box::new(|_| None),
            list: Box::new(Vec::new),
        }
    }
}

impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Execute a skill within the main conversation. Skills provide specialized capabilities \
         and domain knowledge. When a skill matches the user's request, invoke this tool BEFORE \
         generating any other response."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "skill_name": {
                    "type": "string",
                    "description": "The skill name to invoke. E.g., 'commit', 'review-pr', etc."
                },
                "args": {
                    "type": "string",
                    "description": "Optional arguments for the skill"
                }
            },
            "required": ["skill_name"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let skill_name = match args.get("skill_name").and_then(|v| v.as_str()) {
            Some(n) => n.trim(),
            None => return ToolResult::error("Missing 'skill_name' parameter"),
        };
        let user_args = args.get("args").and_then(|v| v.as_str()).unwrap_or("");

        if skill_name.is_empty() {
            return ToolResult::error("Skill name cannot be empty");
        }

        match (self.lookup)(skill_name) {
            Some(skill) => {
                // Render template: replace {{args}} with user args and {{skill_name}} with name
                let rendered = skill
                    .content
                    .replace("{{args}}", user_args)
                    .replace("{{ skill_name }}", &skill.name);

                ToolResult {
                    ok: true,
                    output: rendered,
                    meta: Some(serde_json::json!({
                        "skill_name": skill.name,
                        "description": skill.description,
                    })),
                }
            }
            None => {
                let available = (self.list)();
                let available_str = if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                };
                ToolResult::error(format!(
                    "Skill '{}' not found. Available skills: {}",
                    skill_name, available_str
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(name: &str, description: &str, content: &str) -> SkillInfo {
        SkillInfo {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            base_dir: "/tmp".to_string(),
        }
    }

    fn make_tool(skills: Vec<SkillInfo>) -> SkillTool {
        let skills_clone = skills.clone();
        let lookup =
            Box::new(move |name: &str| skills_clone.iter().find(|s| s.name == name).cloned());
        let names: Vec<String> = skills.iter().map(|s| s.name.clone()).collect();
        let list = Box::new(move || names.clone());
        SkillTool::new(lookup, list)
    }

    #[test]
    fn test_skill_execute_found() {
        let tool = make_tool(vec![make_skill(
            "greet",
            "Greets the user",
            "Hello, {{args}}! Welcome to {{ skill_name }}.",
        )]);
        let result = tool.execute(serde_json::json!({
            "skill_name": "greet",
            "args": "Alice"
        }));
        assert!(result.ok);
        assert!(result.output.contains("Hello, Alice!"));
        assert!(result.output.contains("Welcome to greet."));
        let meta = result.meta.unwrap();
        assert_eq!(meta["skill_name"], "greet");
        assert_eq!(meta["description"], "Greets the user");
    }

    #[test]
    fn test_skill_execute_not_found() {
        let tool = make_tool(vec![make_skill("greet", "Greets the user", "Hello!")]);
        let result = tool.execute(serde_json::json!({
            "skill_name": "missing"
        }));
        assert!(!result.ok);
        assert!(result.output.contains("'missing' not found"));
        assert!(result.output.contains("greet"));
    }

    #[test]
    fn test_skill_execute_no_name() {
        let tool = SkillTool::noop();
        let result = tool.execute(serde_json::json!({}));
        assert!(!result.ok);
        assert!(result.output.contains("Missing 'skill_name'"));
    }
}
