//! Event multiplexer for combining multiple async event sources.

use super::reader::{AsyncTerminalEventReader, TerminalEvent};
use tokio::sync::mpsc;

/// Multiplexed event from various sources.
#[derive(Debug, Clone)]
pub enum MultiplexedEvent {
    /// Terminal event (keyboard, mouse, resize).
    Terminal(TerminalEvent),
    /// PTY output (from shell processes).
    PtyOutput(Vec<u8>),
    /// AI response chunk.
    AiChunk(String),
    /// Custom event type.
    Custom(String),
}

/// Event multiplexer that combines multiple async event sources.
pub struct EventMultiplexer {
    rx: mpsc::UnboundedReceiver<MultiplexedEvent>,
    tx: mpsc::UnboundedSender<MultiplexedEvent>,
}

impl EventMultiplexer {
    /// Create a new event multiplexer.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self { rx, tx }
    }

    /// Get a sender that can be used to push events to the multiplexer.
    pub fn sender(&self) -> mpsc::UnboundedSender<MultiplexedEvent> {
        self.tx.clone()
    }

    /// Get the next event from the multiplexer asynchronously.
    pub async fn next(&mut self) -> Option<MultiplexedEvent> {
        self.rx.recv().await
    }

    /// Spawn a background task that reads from a terminal event reader and forwards events.
    pub fn spawn_terminal_reader(
        &mut self,
        mut reader: AsyncTerminalEventReader,
    ) -> tokio::task::JoinHandle<()> {
        let tx = self.sender();
        tokio::spawn(async move {
            while let Ok(event) = reader.next_event().await {
                if tx.send(MultiplexedEvent::Terminal(event)).is_err() {
                    // Channel closed, stop reading
                    break;
                }
            }
        })
    }
}

impl Default for EventMultiplexer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multiplexer_creation() {
        let mux = EventMultiplexer::new();
        let sender = mux.sender();
        // Test that we can send events
        assert!(sender
            .send(MultiplexedEvent::Custom("test".to_string()))
            .is_ok());
    }

    #[tokio::test]
    async fn test_multiplexer_send_receive() {
        let mut mux = EventMultiplexer::new();
        let sender = mux.sender();

        // Send a custom event
        sender
            .send(MultiplexedEvent::Custom("hello".to_string()))
            .unwrap();

        // Receive it
        let event = mux.next().await;
        assert!(matches!(
            event,
            Some(MultiplexedEvent::Custom(s)) if s == "hello"
        ));
    }

    #[tokio::test]
    async fn test_multiple_senders() {
        let mut mux = EventMultiplexer::new();
        let sender1 = mux.sender();
        let sender2 = mux.sender();

        // Send from both senders
        sender1
            .send(MultiplexedEvent::Custom("from sender1".to_string()))
            .unwrap();
        sender2
            .send(MultiplexedEvent::Custom("from sender2".to_string()))
            .unwrap();

        // Receive both (order may vary)
        let mut received = Vec::new();
        for _ in 0..2 {
            if let Some(event) = mux.next().await {
                received.push(event);
            }
        }

        assert_eq!(received.len(), 2);
        assert!(received
            .iter()
            .any(|e| matches!(e, MultiplexedEvent::Custom(s) if s == "from sender1")));
        assert!(received
            .iter()
            .any(|e| matches!(e, MultiplexedEvent::Custom(s) if s == "from sender2")));
    }

    #[tokio::test]
    async fn test_pty_output_event() {
        let mut mux = EventMultiplexer::new();
        let sender = mux.sender();

        let output = vec![b'h', b'e', b'l', b'l', b'o'];
        sender
            .send(MultiplexedEvent::PtyOutput(output.clone()))
            .unwrap();

        let event = mux.next().await;
        assert!(matches!(
            event,
            Some(MultiplexedEvent::PtyOutput(data)) if data == output
        ));
    }

    #[tokio::test]
    async fn test_ai_chunk_event() {
        let mut mux = EventMultiplexer::new();
        let sender = mux.sender();

        sender
            .send(MultiplexedEvent::AiChunk("AI response".to_string()))
            .unwrap();

        let event = mux.next().await;
        assert!(matches!(
            event,
            Some(MultiplexedEvent::AiChunk(s)) if s == "AI response"
        ));
    }

    #[tokio::test]
    async fn test_terminal_event_conversion() {
        let terminal_event = TerminalEvent::Resize { cols: 80, rows: 24 };
        let mux_event = MultiplexedEvent::Terminal(terminal_event);

        assert!(matches!(
            mux_event,
            MultiplexedEvent::Terminal(TerminalEvent::Resize { cols: 80, rows: 24 })
        ));
    }

    #[tokio::test]
    #[ignore] // May fail in CI environments without a terminal
    async fn test_spawn_terminal_reader() {
        let mut mux = EventMultiplexer::new();
        let reader = AsyncTerminalEventReader::new();

        // Spawn the reader task
        let handle = mux.spawn_terminal_reader(reader);

        // The reader is now running in the background
        // We can't easily test it without actually pressing keys,
        // but we can verify it started without crashing
        assert!(!handle.is_finished());

        // Abort the task to clean up
        handle.abort();
    }

    #[test]
    fn test_multiplexer_default() {
        let mux = EventMultiplexer::default();
        let _sender = mux.sender();
    }
}
