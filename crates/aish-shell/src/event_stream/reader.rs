//! Async terminal event reader using crossterm's event-stream feature.

use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use std::io;

/// Terminal event types that can be received asynchronously.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// Key event.
    Key(crossterm::event::KeyEvent),
    /// Mouse event.
    Mouse(crossterm::event::MouseEvent),
    /// Terminal resize event.
    Resize { cols: u16, rows: u16 },
}

impl From<Event> for TerminalEvent {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) => TerminalEvent::Key(key),
            Event::Mouse(mouse) => TerminalEvent::Mouse(mouse),
            Event::Resize(cols, rows) => TerminalEvent::Resize { cols, rows },
            // crossterm also has FocusGained/FocusLost, but we ignore them for now
            _ => panic!("Unsupported crossterm event: {:?}", event),
        }
    }
}

/// Async terminal event reader that wraps crossterm's EventStream.
pub struct AsyncTerminalEventReader {
    stream: EventStream,
}

impl AsyncTerminalEventReader {
    /// Create a new async terminal event reader.
    pub fn new() -> Self {
        Self {
            stream: EventStream::new(),
        }
    }

    /// Get the next terminal event asynchronously.
    pub async fn next_event(&mut self) -> io::Result<TerminalEvent> {
        match self.stream.next().await {
            Some(Ok(event)) => Ok(TerminalEvent::from(event)),
            Some(Err(e)) => Err(e),
            None => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Event stream closed",
            )),
        }
    }
}

impl Default for AsyncTerminalEventReader {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_event_conversion() {
        // Test KeyEvent conversion
        let key_event = crossterm::event::KeyEvent {
            code: crossterm::event::KeyCode::Char('a'),
            modifiers: crossterm::event::KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        let term_event = TerminalEvent::from(Event::Key(key_event.clone()));
        match term_event {
            TerminalEvent::Key(k) => {
                assert_eq!(k.code, key_event.code);
                assert_eq!(k.modifiers, key_event.modifiers);
            }
            _ => panic!("Expected Key event"),
        }

        // Test Resize conversion
        let term_event = TerminalEvent::from(Event::Resize(80, 24));
        match term_event {
            TerminalEvent::Resize { cols, rows } => {
                assert_eq!(cols, 80);
                assert_eq!(rows, 24);
            }
            _ => panic!("Expected Resize event"),
        }
    }

    #[test]
    #[ignore] // Cannot create EventStream without a proper terminal
    fn test_async_reader_default() {
        let reader = AsyncTerminalEventReader::default();
        // Just test that it can be created
        let _ = reader;
    }
}
