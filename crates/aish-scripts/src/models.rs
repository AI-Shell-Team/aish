use serde::{Deserialize, Serialize};

/// A single script argument definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default = "default_true")]
    pub required: bool,
}

fn default_true() -> bool {
    true
}

fn default_version() -> String {
    "1.0.0".to_string()
}

fn default_script_type() -> String {
    "command".to_string()
}

/// Metadata parsed from YAML frontmatter in a .aish file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScriptMetadata {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub arguments: Vec<ScriptArgument>,
    #[serde(default = "default_script_type")]
    pub r#type: String,
    #[serde(default)]
    pub hook_event: Option<String>,
}

/// A parsed .aish script.
#[derive(Debug, Clone)]
pub struct Script {
    pub metadata: ScriptMetadata,
    pub content: String,
    pub file_path: String,
    pub base_dir: String,
}

impl Script {
    pub fn name(&self) -> &str {
        &self.metadata.name
    }

    pub fn is_hook(&self) -> bool {
        self.metadata.r#type == "hook"
    }

    pub fn hook_event(&self) -> Option<&str> {
        if self.is_hook() {
            self.metadata.hook_event.as_deref()
        } else {
            None
        }
    }
}
