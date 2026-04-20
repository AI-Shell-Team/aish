// Suppress clippy lints that fire on Rust 1.95 stable but not on older versions.
#![allow(
    clippy::type_complexity,
    clippy::redundant_closure,
    clippy::match_like_matches_macro,
    clippy::option_as_ref_deref,
    clippy::field_reassign_with_default,
    clippy::len_zero,
    clippy::borrowed_box,
    clippy::new_without_default,
    clippy::needless_borrow,
    clippy::manual_strip,
    clippy::too_many_arguments
)]

pub mod manager;

pub use manager::I18nManager;

use std::cell::RefCell;
use std::collections::HashMap;

// Thread-local I18nManager, lazily initialised on first use.
thread_local! {
    static MANAGER: RefCell<I18nManager> = RefCell::new(I18nManager::new());
}

/// Translate a dot-separated key using the thread-local [`I18nManager`].
///
/// Returns the translated value, or the key itself when the key is missing.
///
/// # Example
/// ```ignore
/// use aish_i18n::t;
/// let text = t("cli.app_help");
/// ```
pub fn t(key: &str) -> String {
    MANAGER.with(|m| m.borrow().t(key))
}

/// Translate a key and substitute `{variable}` placeholders.
///
/// # Example
/// ```ignore
/// use aish_i18n::t_with_args;
/// use std::collections::HashMap;
///
/// let mut args = HashMap::new();
/// args.insert("version".to_string(), "0.2.0".to_string());
/// let text = t_with_args("shell.welcome2.header", &args);
/// ```
pub fn t_with_args(key: &str, args: &HashMap<String, String>) -> String {
    MANAGER.with(|m| m.borrow().t_with_args(key, args))
}

/// Re-initialise the thread-local manager for a specific locale.
///
/// This is useful when the user switches locale at runtime (e.g. via a
/// command or environment change).
pub fn set_locale(locale: &str) {
    MANAGER.with(|m| {
        *m.borrow_mut() = I18nManager::new_with_locale(locale);
    });
}
