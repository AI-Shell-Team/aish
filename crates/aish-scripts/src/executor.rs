use std::collections::HashMap;
use std::process::Command;

use regex::Regex;
use tracing::debug;

use crate::models::Script;

/// Result of executing a .aish script.
#[derive(Debug, Clone)]
pub struct ScriptExecutionResult {
    pub success: bool,
    pub output: String,
    pub error: String,
    pub new_cwd: Option<String>,
    pub env_changes: HashMap<String, String>,
    pub returncode: i32,
}

/// Executes .aish scripts line by line.
pub struct ScriptExecutor {
    /// Optional callback for AI calls within scripts.
    #[allow(clippy::type_complexity)]
    ai_callback: Option<Box<dyn Fn(&str) -> String + Send + Sync>>,
}

impl ScriptExecutor {
    pub fn new() -> Self {
        Self { ai_callback: None }
    }

    pub fn with_ai_callback<F>(self, cb: F) -> Self
    where
        F: Fn(&str) -> String + Send + Sync + 'static,
    {
        Self {
            ai_callback: Some(Box::new(cb)),
        }
    }

    /// Execute a script with the given arguments.
    pub fn execute(&self, script: &Script, args: &[String]) -> ScriptExecutionResult {
        let mut env = self.build_runtime_env(script, args);
        let mut output = String::new();
        let mut env_changes: HashMap<String, String> = HashMap::new();
        let mut current_cwd = env.remove("AISH_CWD").unwrap_or_else(|| "/".to_string());
        let mut returncode = 0;

        let ai_call_re = Regex::new(r#"^\s*ai\s+["']([^"']+)["']\s*$"#).unwrap();
        let return_re = Regex::new(r"^\s*return\s+(.+)$").unwrap();
        let cd_re = Regex::new(r"^\s*cd\s+(.+)$").unwrap();
        let export_re = Regex::new(r"^\s*export\s+([^=]+)=(.*)$").unwrap();
        let ask_re = Regex::new(r#"^\s*ask\s+["']([^"']+)["']\s*$"#).unwrap();

        let lines: Vec<&str> = script.content.lines().collect();
        let mut i = 0;

        // Simple multi-line block detection for if/for/while/case
        let block_start = Regex::new(r"^\s*(if|for|while|case)\b").unwrap();
        let block_end = Regex::new(r"^\s*(fi|done|esac)\b").unwrap();
        let mut in_block = false;
        let mut block_buffer = String::new();

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();

            i += 1;

            // Skip empty lines and comments
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            // Handle multi-line blocks
            if in_block {
                block_buffer.push_str(line);
                block_buffer.push('\n');
                if block_end.is_match(trimmed) {
                    in_block = false;
                    // Execute the whole block as bash
                    let result = self.execute_bash(&block_buffer, &current_cwd);
                    output.push_str(&result.output);
                    if !result.success {
                        returncode = result.returncode;
                    }
                    block_buffer.clear();
                }
                continue;
            }

            if block_start.is_match(trimmed) {
                in_block = true;
                block_buffer.push_str(line);
                block_buffer.push('\n');
                continue;
            }

            // Handle return
            if let Some(caps) = return_re.captures(trimmed) {
                if let Some(val) = caps.get(1) {
                    output.push_str(val.as_str());
                }
                break;
            }

            // Handle ai "prompt"
            if let Some(caps) = ai_call_re.captures(trimmed) {
                if let Some(prompt) = caps.get(1) {
                    if let Some(ref cb) = self.ai_callback {
                        let response = cb(prompt.as_str());
                        env.insert("AISH_LAST_OUTPUT".to_string(), response.clone());
                        output.push_str(&response);
                        output.push('\n');
                    } else {
                        output.push_str("(AI not available)\n");
                    }
                }
                continue;
            }

            // Handle ask "prompt"
            if let Some(caps) = ask_re.captures(trimmed) {
                if let Some(question) = caps.get(1) {
                    println!("\x1b[36m{}\x1b[0m", question.as_str());
                    output.push_str("(asked user)\n");
                }
                continue;
            }

            // Handle cd path
            if let Some(caps) = cd_re.captures(trimmed) {
                if let Some(path) = caps.get(1) {
                    let expanded = expand_path(path.as_str().trim(), &current_cwd);
                    if std::path::Path::new(&expanded).is_dir() {
                        current_cwd = expanded;
                    } else {
                        output.push_str(&format!("cd: {}: not a directory\n", expanded));
                    }
                }
                continue;
            }

            // Handle export KEY=VALUE
            if let Some(caps) = export_re.captures(trimmed) {
                if let (Some(key), Some(value)) = (caps.get(1), caps.get(2)) {
                    let key = key.as_str().trim().to_string();
                    let value = value.as_str().trim().to_string();
                    env.insert(key.clone(), value.clone());
                    env_changes.insert(key, value);
                }
                continue;
            }

            // Execute as bash command
            let result = self.execute_bash(trimmed, &current_cwd);
            output.push_str(&result.output);
            if !result.success && returncode == 0 {
                returncode = result.returncode;
            }
        }

        ScriptExecutionResult {
            success: returncode == 0,
            output,
            error: String::new(),
            new_cwd: Some(current_cwd),
            env_changes,
            returncode,
        }
    }

    /// Execute script synchronously as a bash subprocess (for hooks).
    pub fn execute_sync(&self, script: &Script, args: &[String]) -> ScriptExecutionResult {
        let env = self.build_runtime_env(script, args);

        let result = Command::new("/bin/bash")
            .arg("-c")
            .arg(&script.content)
            .envs(&env)
            .output();

        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let code = output.status.code().unwrap_or(1);

                // Parse [AISH:CWD:path] markers
                let new_cwd = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("[AISH:CWD:"))
                    .map(|s| s.trim_end_matches(']').to_string());

                ScriptExecutionResult {
                    success: code == 0,
                    output: stdout,
                    error: stderr,
                    new_cwd,
                    env_changes: HashMap::new(),
                    returncode: code,
                }
            }
            Err(e) => ScriptExecutionResult {
                success: false,
                output: String::new(),
                error: e.to_string(),
                new_cwd: None,
                env_changes: HashMap::new(),
                returncode: 1,
            },
        }
    }

    /// Build runtime environment variables for script execution.
    fn build_runtime_env(&self, script: &Script, args: &[String]) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        env.insert("AISH_SCRIPT_DIR".to_string(), script.base_dir.clone());
        env.insert("AISH_SCRIPT_NAME".to_string(), script.metadata.name.clone());
        if let Ok(cwd) = std::env::current_dir() {
            env.insert("AISH_CWD".to_string(), cwd.to_string_lossy().to_string());
        }

        // Positional args
        for (i, arg) in args.iter().enumerate() {
            env.insert(format!("AISH_ARG_{}", i), arg.clone());
        }

        // Named args from metadata
        for (i, param) in script.metadata.arguments.iter().enumerate() {
            let value = args.get(i).cloned().or_else(|| param.default.clone());
            if let Some(val) = value {
                env.insert(format!("AISH_ARG_{}", param.name.to_uppercase()), val);
            } else if param.required {
                debug!(
                    target: "aish_scripts",
                    "missing required argument: {}", param.name
                );
            }
        }

        env
    }

    /// Execute a bash command and return the result.
    fn execute_bash(&self, command: &str, cwd: &str) -> BashOutput {
        match Command::new("/bin/bash")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let code = output.status.code().unwrap_or(-1);

                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str(&stderr);
                }

                BashOutput {
                    output: result,
                    returncode: code,
                    success: code == 0,
                }
            }
            Err(e) => BashOutput {
                output: format!("Error: {}", e),
                returncode: 1,
                success: false,
            },
        }
    }
}

impl Default for ScriptExecutor {
    fn default() -> Self {
        Self::new()
    }
}

struct BashOutput {
    output: String,
    returncode: i32,
    success: bool,
}

/// Expand ~ and environment variables in a path.
fn expand_path(path: &str, cwd: &str) -> String {
    let expanded = if path.starts_with('~') {
        let home = dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(rest) = path.strip_prefix('~') {
            format!("{}{}", home, rest)
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };

    // Resolve relative paths
    if std::path::Path::new(&expanded).is_absolute() {
        expanded
    } else {
        format!("{}/{}", cwd.trim_end_matches('/'), expanded)
    }
}
