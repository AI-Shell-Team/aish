//! Custom cliclack theme for setup wizard: OpenClaw-style search line and footer hints.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Once;

use aish_i18n::t_with_args;
use cliclack::{set_theme, StringCursor, Theme, ThemeState};

use super::clack_filter;

const S_BAR: &str = "│";

thread_local! {
    static FOOTER_HINT: RefCell<Option<String>> = const { RefCell::new(None) };
    static SEARCH_PREFIX: RefCell<Option<String>> = const { RefCell::new(None) };
    static FILTER_HAYSTACKS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static NO_MATCHES_MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

static INIT_THEME: Once = Once::new();

/// Delegates to cliclack default rendering without re-entering [`AishSetupTheme`].
struct BaseTheme;

impl Theme for BaseTheme {}

struct AishSetupTheme;

/// Searchable list context (mirrors @clack/prompts `autocomplete` search row).
pub(crate) struct SearchContext {
    pub prefix: String,
    pub haystacks: Vec<String>,
    pub no_matches: String,
}

impl AishSetupTheme {
    fn footer_hint() -> String {
        FOOTER_HINT
            .with(|hint| hint.borrow().clone())
            .unwrap_or_default()
    }

    fn search_prefix() -> Option<String> {
        SEARCH_PREFIX.with(|prefix| prefix.borrow().clone())
    }

    fn filter_haystacks() -> Vec<String> {
        FILTER_HAYSTACKS.with(|haystacks| haystacks.borrow().clone())
    }

    fn no_matches_message() -> Option<String> {
        NO_MATCHES_MESSAGE.with(|message| message.borrow().clone())
    }

    fn match_suffix(query: &str, haystacks: &[String]) -> String {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let filtered = clack_filter::count_matches(haystacks, trimmed);
        if filtered == haystacks.len() {
            return String::new();
        }
        let mut args = HashMap::new();
        args.insert("count".to_string(), filtered.to_string());
        let key = if filtered == 1 {
            "cli.setup.search_matches_one"
        } else {
            "cli.setup.search_matches"
        };
        t_with_args(key, &args)
    }
}

impl Theme for AishSetupTheme {
    fn format_footer(&self, state: &ThemeState) -> String {
        if let ThemeState::Error(message) = state {
            if message == "No items" {
                if let Some(text) = Self::no_matches_message() {
                    return Theme::format_footer_with_message(&BaseTheme, state, &text);
                }
            }
        }

        let hint = Self::footer_hint();
        if hint.is_empty() {
            Theme::format_footer(&BaseTheme, state)
        } else {
            Theme::format_footer_with_message(&BaseTheme, state, &hint)
        }
    }

    fn format_input(&self, state: &ThemeState, cursor: &StringCursor) -> String {
        let Some(prefix) = Self::search_prefix() else {
            return Theme::format_input(&BaseTheme, state, cursor);
        };

        let base = BaseTheme;
        let dim = base.placeholder_style(state);
        let input_style = base.input_style(state);
        let bar = base.bar_color(state);
        let haystacks = Self::filter_haystacks();
        let query = cursor.to_string();
        let suffix = Self::match_suffix(&query, &haystacks);
        let cursor_display = base.cursor_with_style(cursor, &input_style);
        let line = format!(
            "{}{}{}",
            dim.apply_to(prefix),
            cursor_display,
            dim.apply_to(suffix)
        );

        format!("{}{}  {}\n", bar.apply_to(S_BAR), "", line)
    }
}

/// Install the setup wizard cliclack theme once per process.
pub(crate) fn ensure_theme() {
    INIT_THEME.call_once(|| set_theme(AishSetupTheme));
}

/// Scoped footer / search context for one cliclack prompt.
pub(crate) struct PromptContextGuard {
    _private: (),
}

impl PromptContextGuard {
    pub fn new(footer: Option<&str>, search: Option<SearchContext>) -> Self {
        ensure_theme();
        FOOTER_HINT.with(|hint| {
            *hint.borrow_mut() = footer.map(str::to_string);
        });
        if let Some(search) = search {
            SEARCH_PREFIX.with(|prefix| *prefix.borrow_mut() = Some(search.prefix));
            FILTER_HAYSTACKS.with(|haystacks| *haystacks.borrow_mut() = search.haystacks);
            NO_MATCHES_MESSAGE.with(|message| *message.borrow_mut() = Some(search.no_matches));
        } else {
            SEARCH_PREFIX.with(|prefix| *prefix.borrow_mut() = None);
            FILTER_HAYSTACKS.with(|haystacks| haystacks.borrow_mut().clear());
            NO_MATCHES_MESSAGE.with(|message| *message.borrow_mut() = None);
        }
        Self { _private: () }
    }
}

impl Drop for PromptContextGuard {
    fn drop(&mut self) {
        FOOTER_HINT.with(|hint| *hint.borrow_mut() = None);
        SEARCH_PREFIX.with(|prefix| *prefix.borrow_mut() = None);
        FILTER_HAYSTACKS.with(|haystacks| haystacks.borrow_mut().clear());
        NO_MATCHES_MESSAGE.with(|message| *message.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_input_with_typed_text_does_not_overflow() {
        ensure_theme();
        SEARCH_PREFIX.with(|prefix| *prefix.borrow_mut() = Some("搜索:".to_string()));
        FILTER_HAYSTACKS.with(|haystacks| {
            haystacks
                .borrow_mut()
                .extend(["openai".to_string(), "anthropic".to_string()]);
        });

        let mut cursor = StringCursor::default();
        cursor.insert('o');

        let rendered = Theme::format_input(&AishSetupTheme, &ThemeState::Active, &cursor);
        assert!(rendered.contains('o'));
        assert!(rendered.contains("搜索:"));
        assert!(!rendered.is_empty());
    }
}
