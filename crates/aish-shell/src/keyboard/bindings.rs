use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// A combination of key and modifiers that can trigger an action.
///
/// KeyCombination supports:
/// - Single keys (e.g., 'a', Enter, Esc)
/// - Ctrl+key (e.g., Ctrl+C, Ctrl+D)
/// - Alt+key (e.g., Alt+Enter)
/// - Shift+key (e.g., Shift+Tab)
/// - F-keys (F1-F12)
/// - Special keys (arrows, Home/End, PageUp/Down)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyCombination {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl KeyCombination {
    /// Creates a new KeyCombination from a key code and modifiers.
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// Creates a simple key combination without modifiers.
    pub fn simple(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::empty())
    }

    /// Creates a Ctrl+key combination.
    pub fn ctrl(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::CONTROL)
    }

    /// Creates an Alt+key combination.
    pub fn alt(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::ALT)
    }

    /// Creates a Shift+key combination.
    pub fn shift(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::SHIFT)
    }

    /// Checks if this combination matches the given key event.
    fn matches(&self, event: &KeyEvent) -> bool {
        self.code == event.code && self.modifiers == event.modifiers
    }
}

impl From<KeyCode> for KeyCombination {
    fn from(code: KeyCode) -> Self {
        Self::simple(code)
    }
}

/// Registry that maps key combinations to action names.
///
/// KeyBindings provides a way to register keyboard shortcuts and resolve
/// key events to their corresponding actions. Actions are represented as
/// string names that can be dispatched to shell command handlers.
pub struct KeyBindings {
    bindings: HashMap<KeyCombination, String>,
}

impl KeyBindings {
    /// Creates a new empty KeyBindings registry.
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    /// Registers a key combination to trigger the specified action.
    ///
    /// # Arguments
    ///
    /// * `combo` - The key combination to bind
    /// * `action` - The action name to associate with this combination
    ///
    /// # Examples
    ///
    /// ```
    /// use aish_shell::keyboard::bindings::{KeyBindings, KeyCombination};
    /// use crossterm::event::KeyCode;
    ///
    /// let mut bindings = KeyBindings::new();
    /// bindings.bind(KeyCombination::simple(KeyCode::Enter), "submit");
    /// ```
    pub fn bind(&mut self, combo: KeyCombination, action: &str) {
        self.bindings.insert(combo, action.to_string());
    }

    /// Resolves a key event to its registered action, if any.
    ///
    /// # Arguments
    ///
    /// * `event` - The key event to resolve
    ///
    /// # Returns
    ///
    /// * `Some(action)` - The action name registered for this key
    /// * `None` - No action is registered for this key
    ///
    /// # Examples
    ///
    /// ```
    /// use aish_shell::keyboard::bindings::{KeyBindings, KeyCombination};
    /// use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    ///
    /// let mut bindings = KeyBindings::new();
    /// bindings.bind(KeyCombination::ctrl(KeyCode::Char('c')), "interrupt");
    ///
    /// let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    /// assert_eq!(bindings.resolve(&event), Some("interrupt"));
    /// ```
    pub fn resolve(&self, event: &KeyEvent) -> Option<&str> {
        for (combo, action) in &self.bindings {
            if combo.matches(event) {
                return Some(action);
            }
        }
        None
    }

    /// Removes a key binding.
    ///
    /// Returns `true` if a binding was removed, `false` if no binding existed.
    pub fn unbind(&mut self, combo: &KeyCombination) -> bool {
        self.bindings.remove(combo).is_some()
    }

    /// Returns the number of registered bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Returns `true` if no bindings are registered.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self::new()
    }
}

/// Creates a KeyBindings registry with default shell keybindings.
///
/// The default bindings are:
/// - Esc → "cancel"
/// - Ctrl+C → "interrupt"
/// - Ctrl+D → "eof"
/// - Ctrl+L → "clear"
/// - Ctrl+R → "search_history"
/// - Up → "history_prev"
/// - Down → "history_next"
pub fn default_bindings() -> KeyBindings {
    let mut bindings = KeyBindings::new();

    // Esc key
    bindings.bind(KeyCombination::simple(KeyCode::Esc), "cancel");

    // Ctrl combinations
    bindings.bind(KeyCombination::ctrl(KeyCode::Char('c')), "interrupt");
    bindings.bind(KeyCombination::ctrl(KeyCode::Char('d')), "eof");
    bindings.bind(KeyCombination::ctrl(KeyCode::Char('l')), "clear");
    bindings.bind(KeyCombination::ctrl(KeyCode::Char('r')), "search_history");

    // Arrow keys
    bindings.bind(KeyCombination::simple(KeyCode::Up), "history_prev");
    bindings.bind(KeyCombination::simple(KeyCode::Down), "history_next");

    bindings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_combination_creation() {
        let combo = KeyCombination::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(combo.code, KeyCode::Char('a'));
        assert_eq!(combo.modifiers, KeyModifiers::CONTROL);
    }

    #[test]
    fn test_key_combination_simple() {
        let combo = KeyCombination::simple(KeyCode::Enter);
        assert_eq!(combo.code, KeyCode::Enter);
        assert_eq!(combo.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn test_key_combination_from_keycode() {
        let combo: KeyCombination = KeyCode::Esc.into();
        assert_eq!(combo.code, KeyCode::Esc);
        assert_eq!(combo.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn test_key_combination_matches() {
        let combo = KeyCombination::ctrl(KeyCode::Char('c'));
        let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(combo.matches(&event));

        // Different modifier should not match
        let wrong_event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT);
        assert!(!combo.matches(&wrong_event));

        // Different key should not match
        let wrong_key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(!combo.matches(&wrong_key));
    }

    #[test]
    fn test_key_combination_equality() {
        let combo1 = KeyCombination::ctrl(KeyCode::Char('a'));
        let combo2 = KeyCombination::ctrl(KeyCode::Char('a'));
        assert_eq!(combo1, combo2);

        let combo3 = KeyCombination::alt(KeyCode::Char('a'));
        assert_ne!(combo1, combo3);
    }

    #[test]
    fn test_key_combination_hash() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let combo1 = KeyCombination::ctrl(KeyCode::Char('a'));
        let combo2 = KeyCombination::ctrl(KeyCode::Char('a'));

        let mut h1 = DefaultHasher::new();
        let mut h2 = DefaultHasher::new();
        combo1.hash(&mut h1);
        combo2.hash(&mut h2);

        assert_eq!(h1.finish(), h2.finish());
    }

    #[test]
    fn test_key_bindings_new() {
        let bindings = KeyBindings::new();
        assert!(bindings.is_empty());
        assert_eq!(bindings.len(), 0);
    }

    #[test]
    fn test_key_bindings_bind_and_resolve() {
        let mut bindings = KeyBindings::new();
        bindings.bind(KeyCombination::simple(KeyCode::Enter), "submit");

        let event = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(bindings.resolve(&event), Some("submit"));

        // Unbound key should return None
        let other_event = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(bindings.resolve(&other_event), None);
    }

    #[test]
    fn test_key_bindings_unbind() {
        let mut bindings = KeyBindings::new();
        let combo = KeyCombination::simple(KeyCode::Enter);
        bindings.bind(combo.clone(), "submit");

        assert_eq!(bindings.len(), 1);
        assert!(bindings.unbind(&combo));
        assert!(bindings.is_empty());

        // Unbinding non-existent binding returns false
        assert!(!bindings.unbind(&combo));
    }

    #[test]
    fn test_key_bindings_override() {
        let mut bindings = KeyBindings::new();
        let combo = KeyCombination::ctrl(KeyCode::Char('c'));

        bindings.bind(combo.clone(), "action1");
        assert_eq!(bindings.resolve(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some("action1"));

        bindings.bind(combo.clone(), "action2");
        assert_eq!(bindings.resolve(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some("action2"));

        assert_eq!(bindings.len(), 1);
    }

    #[test]
    fn test_key_bindings_default() {
        let bindings = default_bindings();

        // Test default bindings
        let esc_event = KeyEvent::new(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(bindings.resolve(&esc_event), Some("cancel"));

        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(bindings.resolve(&ctrl_c), Some("interrupt"));

        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(bindings.resolve(&ctrl_d), Some("eof"));

        let ctrl_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
        assert_eq!(bindings.resolve(&ctrl_l), Some("clear"));

        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(bindings.resolve(&ctrl_r), Some("search_history"));

        let up_event = KeyEvent::new(KeyCode::Up, KeyModifiers::empty());
        assert_eq!(bindings.resolve(&up_event), Some("history_prev"));

        let down_event = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(bindings.resolve(&down_event), Some("history_next"));
    }

    #[test]
    fn test_key_bindings_modifiers_matter() {
        let mut bindings = KeyBindings::new();
        bindings.bind(KeyCombination::ctrl(KeyCode::Char('a')), "action1");
        bindings.bind(KeyCombination::alt(KeyCode::Char('a')), "action2");

        let ctrl_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let alt_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::ALT);
        let plain_a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());

        assert_eq!(bindings.resolve(&ctrl_a), Some("action1"));
        assert_eq!(bindings.resolve(&alt_a), Some("action2"));
        assert_eq!(bindings.resolve(&plain_a), None);
    }

    #[test]
    fn test_key_combination_shift() {
        let combo = KeyCombination::shift(KeyCode::Tab);
        assert_eq!(combo.code, KeyCode::Tab);
        assert_eq!(combo.modifiers, KeyModifiers::SHIFT);
    }

    #[test]
    fn test_key_combination_f_keys() {
        let f1 = KeyCombination::simple(KeyCode::F(1));
        let f12 = KeyCombination::simple(KeyCode::F(12));

        assert_eq!(f1.code, KeyCode::F(1));
        assert_eq!(f12.code, KeyCode::F(12));
    }

    #[test]
    fn test_key_combination_special_keys() {
        let test_cases = vec![
            (KeyCode::Home, "home"),
            (KeyCode::End, "end"),
            (KeyCode::PageUp, "page_up"),
            (KeyCode::PageDown, "page_down"),
            (KeyCode::Backspace, "backspace"),
            (KeyCode::Delete, "delete"),
            (KeyCode::Insert, "insert"),
            (KeyCode::Tab, "tab"),
        ];

        for (code, _name) in test_cases {
            let combo = KeyCombination::simple(code);
            assert_eq!(combo.code, code);
            assert_eq!(combo.modifiers, KeyModifiers::empty());
        }
    }

    #[test]
    fn test_key_bindings_default_impl() {
        let bindings = KeyBindings::default();
        assert!(bindings.is_empty());
    }
}
