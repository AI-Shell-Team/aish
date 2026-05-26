use crossterm::event;
use std::time::Duration;

/// Shell event types that can be read from the terminal.
///
/// This enum wraps crossterm events into a more convenient API for shell use.
pub enum ShellEvent {
    /// A keyboard key event (press, release, or repeat).
    Key(event::KeyEvent),
    /// A mouse event (click, drag, scroll, etc.).
    Mouse(event::MouseEvent),
    /// Terminal was resized to the given dimensions (columns, rows).
    Resize(u16, u16),
}

impl ShellEvent {
    /// Parse a crossterm event into a ShellEvent.
    ///
    /// This filters out unsupported event types (like focus gained/lost)
    /// and converts the rest into our simplified event enum.
    fn from_crossterm(evt: event::Event) -> Option<Self> {
        match evt {
            event::Event::Key(key) => Some(ShellEvent::Key(key)),
            event::Event::Mouse(mouse) => Some(ShellEvent::Mouse(mouse)),
            event::Event::Resize(cols, rows) => Some(ShellEvent::Resize(cols, rows)),
            // Ignore focus events and other unsupported types
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
        // Use poll with timeout
        if event::poll(duration)? {
            let evt = event::read()?;
            Ok(ShellEvent::from_crossterm(evt))
        } else {
            Ok(None)
        }
    } else {
        // Block indefinitely
        let evt = event::read()?;
        Ok(ShellEvent::from_crossterm(evt))
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
    fn test_shell_event_from_resize() {
        let shell_event = ShellEvent::from_crossterm(event::Event::Resize(80, 24));

        assert!(shell_event.is_some());
        match shell_event.unwrap() {
            ShellEvent::Resize(cols, rows) => {
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
            }
            _ => panic!("Expected resize event"),
        }
    }

    #[test]
    fn test_shell_event_from_mouse() {
        let mouse_event = event::MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::empty(),
        };
        let shell_event = ShellEvent::from_crossterm(event::Event::Mouse(mouse_event));

        assert!(shell_event.is_some());
        match shell_event.unwrap() {
            ShellEvent::Mouse(mouse) => {
                assert_eq!(mouse.column, 10);
                assert_eq!(mouse.row, 5);
            }
            _ => panic!("Expected mouse event"),
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
