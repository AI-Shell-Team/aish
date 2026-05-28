use crossterm::event;
use std::time::Duration;

/// Shell event types that can be read from the terminal.
///
/// This enum wraps crossterm events into a more convenient API for shell use.
#[derive(Debug)]
pub enum ShellEvent {
    /// A keyboard key press event. Release and repeat events are filtered out.
    Key(event::KeyEvent),
}

impl ShellEvent {
    /// Parse a crossterm event into a ShellEvent.
    ///
    /// This filters out unsupported event types (like focus gained/lost)
    /// and converts the rest into our simplified event enum.
    fn from_crossterm(evt: event::Event) -> Option<Self> {
        match evt {
            event::Event::Key(key) if key.kind == event::KeyEventKind::Press => {
                Some(ShellEvent::Key(key))
            }
            // Ignore mouse, resize, focus, and other unsupported events
            _ => None,
        }
    }
}

/// Reads a single event from the terminal, optionally with a timeout.
///
/// # Arguments
///
/// * `timeout` - Optional duration to wait for an event. If `None`, blocks
///   indefinitely until an event is available. If `Some(duration)`, waits
///   up to the specified duration.
///
/// # Returns
///
/// * `Ok(Some(event))` - An event was read successfully
/// * `Ok(None)` - Timeout elapsed with no event available
/// * `Err(_)` - An I/O error occurred while reading
///
/// # Examples
///
/// ```ignore
/// use aish_shell::keyboard::events;
/// use std::time::Duration;
///
/// // Block until an event arrives
/// let event = events::read_event(None).unwrap().unwrap();
///
/// // Wait up to 100ms for an event
/// let event = events::read_event(Some(Duration::from_millis(100))).unwrap();
/// ```
pub fn read_event(timeout: Option<Duration>) -> Result<Option<ShellEvent>, std::io::Error> {
    if let Some(duration) = timeout {
        // Use poll with timeout. Loop until a valid (non-filtered) event is found
        // or the timeout elapses. When a filtered event is read, we recalculate
        // the remaining timeout so the total wait does not exceed the original.
        let deadline = std::time::Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if event::poll(remaining)? {
                let evt = event::read()?;
                if let Some(shell_evt) = ShellEvent::from_crossterm(evt) {
                    return Ok(Some(shell_evt));
                }
                // filtered event, continue polling with remaining time
            } else {
                return Ok(None);
            }
        }
    } else {
        // Block indefinitely, looping until a valid (non-filtered) event arrives
        loop {
            let evt = event::read()?;
            if let Some(shell_evt) = ShellEvent::from_crossterm(evt) {
                return Ok(Some(shell_evt));
            }
            // filtered event, read next
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn test_shell_event_from_key_event() {
        let key_event = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
        let shell_event = ShellEvent::from_crossterm(event::Event::Key(key_event));

        assert!(shell_event.is_some());
        match shell_event.unwrap() {
            ShellEvent::Key(key) => {
                assert_eq!(key.code, KeyCode::Char('a'));
                assert_eq!(key.modifiers, KeyModifiers::empty());
            }
            _ => panic!("Expected key event"),
        }
    }

    #[test]
    fn test_shell_event_filters_unsupported() {
        // Focus events should be filtered out
        let focus_event = event::Event::FocusGained;
        let shell_event = ShellEvent::from_crossterm(focus_event);

        assert!(shell_event.is_none());
    }

    #[test]
    fn test_key_event_filters_release_and_repeat() {
        let press = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty());
        assert!(
            ShellEvent::from_crossterm(event::Event::Key(press)).is_some(),
            "Press events should pass through"
        );

        let release = KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::empty(),
            event::KeyEventKind::Release,
        );
        assert!(
            ShellEvent::from_crossterm(event::Event::Key(release)).is_none(),
            "Release events should be filtered"
        );

        let repeat = KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::empty(),
            event::KeyEventKind::Repeat,
        );
        assert!(
            ShellEvent::from_crossterm(event::Event::Key(repeat)).is_none(),
            "Repeat events should be filtered"
        );
    }

    #[test]
    fn test_key_event_with_modifiers() {
        let key_event = KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        );
        let shell_event = ShellEvent::from_crossterm(event::Event::Key(key_event));

        assert!(shell_event.is_some());
        match shell_event.unwrap() {
            ShellEvent::Key(key) => {
                assert_eq!(key.code, KeyCode::Char('c'));
                assert_eq!(key.modifiers, KeyModifiers::CONTROL);
            }
            _ => panic!("Expected key event"),
        }
    }

    #[test]
    fn test_key_event_special_keys() {
        let test_cases = vec![
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Backspace,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::F(1),
            KeyCode::F(5),
            KeyCode::F(12),
        ];

        for code in test_cases {
            let key_event = KeyEvent::new(code, KeyModifiers::empty());
            let shell_event = ShellEvent::from_crossterm(event::Event::Key(key_event));

            assert!(shell_event.is_some(), "Failed for {:?}", code);
            match shell_event.unwrap() {
                ShellEvent::Key(key) => {
                    assert_eq!(key.code, code);
                }
                _ => panic!("Expected key event for {:?}", code),
            }
        }
    }
}
