//! Integration tests: slash popup behavior against `readline::SLASH_COMMANDS`.

use aish_shell::readline::SLASH_COMMANDS;
use aish_ui::{SlashInputOutcome, SlashInputSession};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn session_with(input: &str) -> SlashInputSession {
    let commands: Vec<(String, String)> = SLASH_COMMANDS
        .iter()
        .map(|(name, desc)| (name.to_string(), desc.to_string()))
        .collect();
    SlashInputSession::new(commands, "aish> ".to_string()).with_input(input)
}

#[test]
fn each_builtin_command_enter_executes() {
    for (name, _) in SLASH_COMMANDS {
        let mut session = session_with(name);
        assert_eq!(
            session.dispatch_event(key(KeyCode::Enter)),
            Some(SlashInputOutcome::Command(name.to_string())),
            "Enter on {name}",
        );
    }
}

#[test]
fn slash_commands_table_has_expected_count() {
    assert_eq!(SLASH_COMMANDS.len(), 13);
}
