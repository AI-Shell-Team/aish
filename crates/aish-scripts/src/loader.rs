use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::{debug, warn};

use crate::models::{Script, ScriptMetadata};

/// Load .aish script files from the filesystem.
pub struct ScriptLoader {
    scripts_dir: Option<PathBuf>,
}

impl ScriptLoader {
    pub fn new(scripts_dir: Option<PathBuf>) -> Self {
        Self { scripts_dir }
    }

    /// Return the scripts directory, defaulting to `~/.config/aish/scripts/`.
    pub fn get_scripts_dir(&self) -> PathBuf {
        self.scripts_dir.clone().unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("aish")
                .join("scripts")
        })
    }

    /// Scan the scripts directory and return a map of name -> Script.
    pub fn scan_scripts(&self) -> Vec<Script> {
        let dir = self.get_scripts_dir();
        if !dir.exists() {
            debug!(target: "aish_scripts", "scripts directory does not exist: {:?}", dir);
            return Vec::new();
        }

        let mut scripts = Vec::new();
        let mut seen_names = HashSet::new();

        for path in iter_script_files(&dir) {
            match parse_script_file(&path) {
                Ok(script) => {
                    let name = script.name().to_string();
                    if seen_names.insert(name.clone()) {
                        scripts.push(script);
                    } else {
                        debug!(target: "aish_scripts", "duplicate script name '{}', skipping {:?}", name, path);
                    }
                }
                Err(e) => {
                    warn!(target: "aish_scripts", "failed to parse {:?}: {}", path, e);
                }
            }
        }

        scripts
    }
}

/// Walk a directory tree, yielding paths to `.aish` files.
/// Detects symlink cycles using (device, inode) tracking.
fn iter_script_files(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    walk_dir(root, &mut result, &mut visited);
    result
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>, visited: &mut HashSet<(u64, u64)>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            // Skip .git directories
            if path.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue;
            }

            if path.is_dir() {
                // Symlink cycle detection
                if let Ok(meta) = path.metadata() {
                    let key = get_dev_ino(&meta);
                    if visited.contains(&key) {
                        continue;
                    }
                    visited.insert(key);
                }
                walk_dir(&path, out, visited);
            } else if path.extension().map(|e| e == "aish").unwrap_or(false) {
                out.push(path);
            }
        }
    }
}

#[cfg(unix)]
fn get_dev_ino(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn get_dev_ino(_meta: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

/// Parse a single .aish file.
pub fn parse_script_file(path: &Path) -> Result<Script, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read error: {}", e))?;
    let file_path = path.to_string_lossy().to_string();
    let base_dir = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let (metadata, body) = extract_frontmatter(&content, path)?;

    Ok(Script {
        metadata,
        content: body,
        file_path,
        base_dir,
    })
}

/// Extract YAML frontmatter and body from .aish file content.
fn extract_frontmatter(content: &str, path: &Path) -> Result<(ScriptMetadata, String), String> {
    let re = regex::Regex::new(r"(?s)^---\s*\n(.*?)\n---\s*\n").unwrap();

    if let Some(caps) = re.captures(content) {
        let yaml_str = caps.get(1).unwrap().as_str();
        let body = content[caps.get(0).unwrap().end()..].to_string();

        let mut meta: serde_yaml::Value =
            serde_yaml::from_str(yaml_str).map_err(|e| format!("YAML parse error: {}", e))?;

        // Extract name from frontmatter or use filename
        let name = if let Some(name_val) = meta.get("name").and_then(|v| v.as_str()) {
            name_val.to_string()
        } else {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        };

        // Set name in the YAML if not present
        if let Some(mapping) = meta.as_mapping_mut() {
            mapping.insert(
                serde_yaml::Value::String("name".to_string()),
                serde_yaml::Value::String(name),
            );
        }

        let metadata: ScriptMetadata =
            serde_yaml::from_value(meta).map_err(|e| format!("metadata parse error: {}", e))?;

        Ok((metadata, body))
    } else {
        // No frontmatter: use filename as name
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let metadata = ScriptMetadata {
            name,
            description: String::new(),
            version: "1.0.0".to_string(),
            arguments: Vec::new(),
            r#type: "command".to_string(),
            hook_event: None,
        };

        Ok((metadata, content.to_string()))
    }
}
