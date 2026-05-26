//! TUI backend management for alternate screen and terminal handling.
//!
//! Provides `TuiBackend` which manages the crossterm terminal setup,
//! alternate screen mode, and proper cleanup on exit.

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend as RatatuiCrosstermBackend;
use std::io::{self, Stdout};

/// Error type for TUI backend operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during TUI backend operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// IO error from terminal operations.
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

/// Manages the TUI backend including alternate screen and terminal state.
///
/// `TuiBackend` handles entering alternate screen mode, enabling raw mode
/// for immediate input, and provides a draw interface for rendering
/// ratatui frames. Automatically restores the terminal on drop.
pub struct TuiBackend {
    /// The ratatui terminal instance.
    terminal: ratatui::Terminal<RatatuiCrosstermBackend<Stdout>>,
}

impl TuiBackend {
    /// Create a new TUI backend and enter alternate screen mode.
    ///
    /// This enables:
    /// - Alternate screen (buffered output separate from main terminal)
    /// - Raw mode (disable line buffering, echo, etc.)
    /// - Mouse capture
    ///
    /// Returns an error if stdout is not a TTY.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use aish_shell::tui::TuiBackend;
    ///
    /// let mut backend = TuiBackend::new()?;
    /// backend.draw(|frame| {
    ///     // Render UI components using frame
    /// });
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new() -> Result<Self> {
        // Enable raw mode for immediate input processing
        // This will fail if not a TTY, which is fine
        enable_raw_mode()?;

        // Enter alternate screen and enable mouse capture
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture
        )?;

        // Create the ratatui terminal with crossterm backend
        let backend = RatatuiCrosstermBackend::new(stdout);
        let terminal = ratatui::Terminal::new(backend)?;

        Ok(Self { terminal })
    }

    /// Draw a single frame to the terminal.
    ///
    /// The provided closure receives a mutable reference to the current
    /// frame and should render UI components using ratatui's drawing API.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use aish_shell::tui::TuiBackend;
    /// # let mut backend = TuiBackend::new()?;
    /// backend.draw(|frame| {
    ///     use ratatui::{widgets::Paragraph, text::Text, layout::Rect};
    ///
    ///     let paragraph = Paragraph::new("Hello, ratatui!");
    ///     frame.render_widget(paragraph, frame.size());
    /// });
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn draw<F>(&mut self, f: F) -> Result<()>
    where
        F: FnOnce(&mut ratatui::Frame),
    {
        self.terminal.draw(f)?;
        Ok(())
    }

    /// Manually restore the terminal state.
    ///
    /// This exits alternate screen mode, disables raw mode, and
    /// disables mouse capture. After calling this, the backend
    /// cannot be used for further drawing.
    ///
    /// Note that this is also called automatically on drop.
    pub fn restore(mut self) -> Result<()> {
        self.restore_inner()?;
        Ok(())
    }

    /// Internal implementation of terminal restoration.
    fn restore_inner(&mut self) -> Result<()> {
        // Ensure we're in normal terminal mode
        if crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
            disable_raw_mode()?;
        }

        // Restore terminal state
        execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;

        Ok(())
    }
}

impl Drop for TuiBackend {
    fn drop(&mut self) {
        // Best-effort restoration during drop
        let _ = self.restore_inner();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let io_err = Error::Io(io::Error::new(io::ErrorKind::NotFound, "test"));
        assert_eq!(io_err.to_string(), "IO error: test");
    }

    #[test]
    fn test_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
        let error: Error = io_err.into();
        assert!(matches!(error, Error::Io(_)));
    }
}
