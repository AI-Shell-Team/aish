use std::collections::HashMap;
use std::process::Command;

use tracing::debug;

use crate::executor::ScriptExecutor;
use crate::registry::ScriptRegistry;

/// Manages hook scripts (aish_prompt, aish_greeting, etc.)
pub struct HookManager {
    registry: ScriptRegistry,
    #[allow(dead_code)] // reserved for future hook execution via executor
    executor: ScriptExecutor,
}

/// Well-known hook event names.
pub const HOOK_PROMPT: &str = "prompt";
pub const HOOK_GREETING: &str = "greeting";
pub const HOOK_PRECMD: &str = "precmd";
pub const HOOK_POSTCMD: &str = "postcmd";

impl HookManager {
    pub fn new(registry: ScriptRegistry) -> Self {
        Self {
            registry,
            executor: ScriptExecutor::new(),
        }
    }

    /// Check if a hook script exists for the given event.
    pub fn has_hook(&self, event: &str) -> bool {
        let hook_name = format!("aish_{}", event);
        self.registry.has_script(&hook_name)
    }

    /// Get the hook script for the given event.
    pub fn get_hook(&self, event: &str) -> Option<crate::models::Script> {
        let hook_name = format!("aish_{}", event);
        self.registry.get_script(&hook_name).cloned()
    }

    /// Run the prompt hook, returning a custom prompt string if available.
    pub fn run_prompt_hook(&self, cwd: &str, exit_code: i32) -> Option<String> {
        let hook = self.get_hook(HOOK_PROMPT)?;
        let env = self.build_prompt_env(cwd, exit_code);
        self.execute_hook(&hook, &env)
    }

    /// Build environment variables for the prompt hook.
    fn build_prompt_env(&self, cwd: &str, exit_code: i32) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        env.insert("AISH_CWD".to_string(), cwd.to_string());
        env.insert("AISH_EXIT_CODE".to_string(), exit_code.to_string());

        // Git status detection
        let git_check = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(cwd)
            .output();

        if let Ok(output) = git_check {
            if output.status.success() {
                env.insert("AISH_GIT_REPO".to_string(), "1".to_string());

                // Branch name
                if let Ok(branch_out) = Command::new("git")
                    .args(["branch", "--show-current"])
                    .current_dir(cwd)
                    .output()
                {
                    let branch = String::from_utf8_lossy(&branch_out.stdout)
                        .trim()
                        .to_string();
                    if !branch.is_empty() {
                        env.insert("AISH_GIT_BRANCH".to_string(), branch);
                    }
                }

                // Git status
                if let Ok(status_out) = Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(cwd)
                    .output()
                {
                    let status_str = String::from_utf8_lossy(&status_out.stdout);
                    let staged = status_str
                        .lines()
                        .filter(|l| {
                            let first = l.chars().next().unwrap_or(' ');
                            matches!(first, 'M' | 'A' | 'D' | 'R' | 'C')
                        })
                        .count();
                    let modified = status_str
                        .lines()
                        .filter(|l| {
                            let bytes = l.as_bytes();
                            bytes.len() > 1 && matches!(bytes[1], b'M' | b'D')
                        })
                        .count();
                    let untracked = status_str.lines().filter(|l| l.starts_with("??")).count();

                    env.insert("AISH_GIT_STAGED".to_string(), staged.to_string());
                    env.insert("AISH_GIT_MODIFIED".to_string(), modified.to_string());
                    env.insert("AISH_GIT_UNTRACKED".to_string(), untracked.to_string());

                    let status = if staged > 0 {
                        "staged"
                    } else if modified > 0 || untracked > 0 {
                        "dirty"
                    } else {
                        "clean"
                    };
                    env.insert("AISH_GIT_STATUS".to_string(), status.to_string());
                }

                // Ahead/behind
                if let Ok(ab_out) = Command::new("git")
                    .args(["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
                    .current_dir(cwd)
                    .output()
                {
                    let ab = String::from_utf8_lossy(&ab_out.stdout).trim().to_string();
                    let parts: Vec<&str> = ab.split_whitespace().collect();
                    if parts.len() == 2 {
                        env.insert("AISH_GIT_BEHIND".to_string(), parts[0].to_string());
                        env.insert("AISH_GIT_AHEAD".to_string(), parts[1].to_string());
                    }
                }
            }
        }

        // Virtual environment detection
        if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
            // Skip aish's own .venv
            if !venv.contains("aish") {
                let name = std::path::Path::new(&venv)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown");
                env.insert("AISH_VIRTUAL_ENV".to_string(), name.to_string());
            }
        } else if let Ok(conda) = std::env::var("CONDA_DEFAULT_ENV") {
            env.insert("AISH_VIRTUAL_ENV".to_string(), conda);
        }

        env
    }

    /// Run the greeting hook, returning the greeting string if available.
    pub fn run_greeting_hook(&self) -> Option<String> {
        let hook = self.get_hook(HOOK_GREETING)?;
        self.execute_hook(&hook, &std::env::vars().collect())
    }

    /// Run the pre-command hook with the command about to be executed.
    pub fn run_precmd_hook(&self, command: &str) -> Option<String> {
        let hook = self.get_hook(HOOK_PRECMD)?;
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("AISH_COMMAND".to_string(), command.to_string());
        self.execute_hook(&hook, &env)
    }

    /// Run the post-command hook with the command that was executed and its exit code.
    pub fn run_postcmd_hook(&self, command: &str, exit_code: i32) -> Option<String> {
        let hook = self.get_hook(HOOK_POSTCMD)?;
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.insert("AISH_COMMAND".to_string(), command.to_string());
        env.insert("AISH_EXIT_CODE".to_string(), exit_code.to_string());
        self.execute_hook(&hook, &env)
    }

    /// Execute a hook script and return its stdout if successful.
    fn execute_hook(
        &self,
        hook: &crate::models::Script,
        env: &HashMap<String, String>,
    ) -> Option<String> {
        let result = Command::new("/bin/bash")
            .arg("-c")
            .arg(&hook.content)
            .envs(env)
            .output();

        match result {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if stdout.is_empty() {
                    None
                } else {
                    Some(stdout)
                }
            }
            Ok(output) => {
                debug!(
                    target: "aish_hooks",
                    "hook '{}' exited with {:?}",
                    hook.metadata.name,
                    output.status.code()
                );
                None
            }
            Err(e) => {
                debug!(target: "aish_hooks", "hook '{}' failed: {}", hook.metadata.name, e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ScriptRegistry;

    fn make_registry_with_hook(event: &str, content: &str) -> ScriptRegistry {
        let mut reg = ScriptRegistry::new(None);
        let hook_name = format!("aish_{}", event);
        reg.register(crate::models::Script {
            metadata: crate::models::ScriptMetadata {
                name: hook_name,
                description: format!("test {} hook", event),
                hook_event: Some(event.to_string()),
                ..Default::default()
            },
            content: content.to_string(),
            file_path: format!("/tmp/test_{}", event),
            base_dir: "/tmp".to_string(),
        });
        reg
    }

    #[test]
    fn test_has_hook() {
        let reg = make_registry_with_hook("greeting", "echo hello");
        let mgr = HookManager::new(reg);
        assert!(mgr.has_hook("greeting"));
        assert!(!mgr.has_hook("nonexistent"));
    }

    #[test]
    fn test_run_greeting_hook_success() {
        let reg = make_registry_with_hook("greeting", "echo 'Welcome to aish!'");
        let mgr = HookManager::new(reg);
        let result = mgr.run_greeting_hook();
        assert!(result.is_some());
        assert!(result.unwrap().contains("Welcome to aish!"));
    }

    #[test]
    fn test_run_greeting_hook_no_hook() {
        let reg = ScriptRegistry::new(None);
        let mgr = HookManager::new(reg);
        let result = mgr.run_greeting_hook();
        assert!(result.is_none());
    }

    #[test]
    fn test_run_precmd_hook_with_command() {
        let reg = make_registry_with_hook("precmd", "echo \"about to run: $AISH_COMMAND\"");
        let mgr = HookManager::new(reg);
        let result = mgr.run_precmd_hook("ls -la");
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.contains("about to run:"));
    }

    #[test]
    fn test_run_postcmd_hook_with_exit_code() {
        let reg = make_registry_with_hook("postcmd", "echo \"exit code: $AISH_EXIT_CODE\"");
        let mgr = HookManager::new(reg);
        let result = mgr.run_postcmd_hook("ls", 0);
        assert!(result.is_some());
        let output = result.unwrap();
        assert!(output.contains("exit code: 0"));
    }

    #[test]
    fn test_run_precmd_hook_no_hook() {
        let reg = ScriptRegistry::new(None);
        let mgr = HookManager::new(reg);
        assert!(mgr.run_precmd_hook("ls").is_none());
    }

    #[test]
    fn test_run_postcmd_hook_no_hook() {
        let reg = ScriptRegistry::new(None);
        let mgr = HookManager::new(reg);
        assert!(mgr.run_postcmd_hook("ls", 0).is_none());
    }
}
