//! Non-interactive cliclack log/outro helpers for the setup wizard.

use super::clack_theme;

pub fn step(message: &str) {
    clack_theme::ensure_theme();
    let _ = cliclack::log::step(message);
}

pub fn success(message: &str) {
    clack_theme::ensure_theme();
    let _ = cliclack::log::success(message);
}

pub fn error(message: &str) {
    clack_theme::ensure_theme();
    let _ = cliclack::log::error(message);
}

pub fn outro(message: &str) {
    clack_theme::ensure_theme();
    let _ = cliclack::outro(message);
}

/// Run blocking work under a cliclack spinner, then clear it before the caller logs the result.
pub fn with_spinner<T>(message: &str, work: impl FnOnce() -> T) -> T {
    clack_theme::ensure_theme();
    let spinner = cliclack::spinner();
    spinner.start(message);
    let result = work();
    spinner.clear();
    result
}
