//! Integration tests: slash popup behavior against `readline::SLASH_COMMANDS`.

use aish_shell::readline::{EnterPolicy, SlashGroup, SLASH_COMMANDS};
use aish_ui::{SlashCommandEntry, SlashInputOutcome, SlashInputSession};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Build a fully-enabled popup session from the real command table.
fn session_with(input: &str) -> SlashInputSession {
    let group_labels: Vec<String> = SlashGroup::ALL
        .iter()
        .map(|g| format!("group:{}", g.key()))
        .collect();
    let entries: Vec<SlashCommandEntry> = SLASH_COMMANDS
        .iter()
        .map(|c| SlashCommandEntry {
            name: c.name.to_string(),
            desc: format!("{} description", c.name),
            keywords: c.keywords.iter().map(|s| s.to_string()).collect(),
            group_index: SlashGroup::ALL
                .iter()
                .position(|g| *g == c.group)
                .unwrap_or(0),
            fill_only: matches!(c.enter, EnterPolicy::Fill),
            status: None,
        })
        .collect();
    SlashInputSession::new(entries, group_labels, "aish> ".to_string()).with_input(input)
}

#[test]
fn each_builtin_command_enter_executes() {
    for c in SLASH_COMMANDS {
        let mut session = session_with(c.name);
        match session.dispatch_event(key(KeyCode::Enter)) {
            Some(SlashInputOutcome::Command(text)) => {
                assert!(
                    matches!(c.enter, EnterPolicy::Execute),
                    "unexpected Command for {}",
                    c.name
                );
                assert_eq!(text, c.name, "Enter on {}", c.name);
            }
            Some(SlashInputOutcome::Fill(text)) => {
                assert!(
                    matches!(c.enter, EnterPolicy::Fill),
                    "unexpected Fill for {}",
                    c.name
                );
                assert_eq!(text, c.name);
            }
            other => panic!("Enter on {} produced {:?}", c.name, other),
        }
    }
}

#[test]
fn slash_commands_table_has_expected_count() {
    assert_eq!(SLASH_COMMANDS.len(), 24);
}

#[test]
fn slash_commands_have_i18n_descriptions() {
    const LOCALES: &[&str] = &["en-US", "zh-CN", "de-DE", "es-ES", "fr-FR", "ja-JP"];
    for locale in LOCALES {
        aish_i18n::set_locale(locale);
        for c in SLASH_COMMANDS {
            let cmd = c.name.strip_prefix('/').expect("slash command");
            let key = format!("shell.slash.{cmd}");
            let translated = aish_i18n::t(&key);
            assert_ne!(
                translated, key,
                "missing i18n entry for slash command {} (key: {key}) in locale {locale}",
                c.name
            );
        }
    }
}

#[test]
fn group_labels_and_status_reasons_have_i18n_entries() {
    const LOCALES: &[&str] = &["en-US", "zh-CN", "de-DE", "es-ES", "fr-FR", "ja-JP"];
    for locale in LOCALES {
        aish_i18n::set_locale(locale);
        for g in SlashGroup::ALL {
            let key = format!("shell.slash_group.{}", g.key());
            assert_ne!(
                aish_i18n::t(&key),
                key,
                "missing i18n group entry {key} in locale {locale}"
            );
        }
        for key_suffix in [
            "reason_diagnose_no_failure",
            "reason_audit_disabled",
            "reason_undo_empty",
            "reason_pty_daemon_disabled",
            "undo_count",
            "plan_planning",
            "plan_normal",
        ] {
            let key = format!("shell.slash_status.{key_suffix}");
            assert_ne!(
                aish_i18n::t(&key),
                key,
                "missing i18n status entry {key} in locale {locale}"
            );
        }
    }
}

#[test]
fn undo_count_placeholder_substitutes_in_all_locales() {
    const LOCALES: &[&str] = &["en-US", "zh-CN", "de-DE", "es-ES", "fr-FR", "ja-JP"];
    for locale in LOCALES {
        aish_i18n::set_locale(locale);
        let mut args = std::collections::HashMap::new();
        args.insert("count".to_string(), "3".to_string());
        let text = aish_i18n::t_with_args("shell.slash_status.undo_count", &args);
        assert!(
            text.contains('3') && !text.contains("{count}"),
            "undo_count placeholder not substituted in {locale}: {text}"
        );
    }
}
