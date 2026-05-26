use std::collections::HashMap;
use std::path::PathBuf;

use crate::loader::ScriptLoader;
use crate::models::Script;

/// Thread-safe registry of loaded .aish scripts.
pub struct ScriptRegistry {
    scripts: HashMap<String, Script>,
    loader: ScriptLoader,
    dirty: bool,
    version: u64,
}

impl ScriptRegistry {
    pub fn new(scripts_dir: Option<PathBuf>) -> Self {
        let loader = ScriptLoader::new(scripts_dir);
        let mut reg = Self {
            scripts: HashMap::new(),
            loader,
            dirty: true,
            version: 0,
        };
        let _ = reg.load_all_scripts();
        reg
    }

    /// Force-load all scripts from disk.
    pub fn load_all_scripts(&mut self) -> usize {
        let scripts = self.loader.scan_scripts();
        let count = scripts.len();
        self.scripts.clear();
        for script in scripts {
            self.scripts.insert(script.metadata.name.clone(), script);
        }
        self.dirty = false;
        self.version += 1;
        count
    }

    /// Mark the registry as dirty for lazy reload.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Reload scripts if the registry is dirty.
    /// Returns true if a reload happened.
    pub fn reload_if_dirty(&mut self) -> bool {
        if self.dirty {
            self.load_all_scripts();
            true
        } else {
            false
        }
    }

    /// Current version counter (increments on each reload).
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn has_script(&self, name: &str) -> bool {
        self.scripts.contains_key(name)
    }

    pub fn get_script(&self, name: &str) -> Option<&Script> {
        self.scripts.get(name)
    }

    pub fn list_scripts(&self) -> Vec<&Script> {
        self.scripts.values().collect()
    }

    pub fn get_script_names(&self) -> Vec<&str> {
        self.scripts.keys().map(|s| s.as_str()).collect()
    }

    /// Register a script directly (useful for testing).
    pub fn register(&mut self, script: Script) {
        self.scripts.insert(script.metadata.name.clone(), script);
    }

    /// Filter scripts by hook event.
    pub fn get_hook_scripts(&self, event: &str) -> Vec<&Script> {
        self.scripts
            .values()
            .filter(|s| s.is_hook() && s.hook_event() == Some(event))
            .collect()
    }
}
