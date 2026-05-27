use std::{
    io::{self, Write},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use crossterm::{cursor, event, execute, terminal};
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal, TerminalOptions, Viewport};
use thiserror::Error;

static TERMINAL_INPUT_CLAIMS: AtomicUsize = AtomicUsize::new(0);

#[must_use]
#[derive(Debug)]
pub struct TerminalInputClaim;

pub fn claim_terminal_input() -> TerminalInputClaim {
    TERMINAL_INPUT_CLAIMS.fetch_add(1, Ordering::SeqCst);
    TerminalInputClaim
}

pub fn terminal_input_active() -> bool {
    TERMINAL_INPUT_CLAIMS.load(Ordering::SeqCst) > 0
}

impl Drop for TerminalInputClaim {
    fn drop(&mut self) {
        TERMINAL_INPUT_CLAIMS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub trait PanelComponent {
    type Output;

    fn desired_height(&self, terminal_width: u16, terminal_height: u16) -> u16;
    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect);
    fn handle_event(&mut self, event: event::Event) -> PanelEvent<Self::Output>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEvent<T> {
    Continue,
    Submit(T),
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelOutcome<T> {
    Submitted(T),
    Cancelled,
}

#[derive(Debug, Error)]
pub enum PanelError {
    #[error("terminal I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PanelRuntime;

impl PanelRuntime {
    pub fn new() -> Self {
        Self
    }

    pub fn run<C>(&self, mut component: C) -> Result<PanelOutcome<C::Output>, PanelError>
    where
        C: PanelComponent,
    {
        let _input_claim = claim_terminal_input();
        let (cols, rows) = terminal::size()?;
        let height = component.desired_height(cols, rows).clamp(1, rows.max(1));
        let _guard = TerminalGuard::enter()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        drain_pending_events()?;

        let outcome = loop {
            terminal.draw(|frame| component.render(frame, frame.area()))?;

            let event = event::read()?;
            match component.handle_event(event) {
                PanelEvent::Continue => continue,
                PanelEvent::Submit(value) => break PanelOutcome::Submitted(value),
                PanelEvent::Cancel => break PanelOutcome::Cancelled,
            }
        };

        let _ = terminal.clear();
        Ok(outcome)
    }
}

fn drain_pending_events() -> io::Result<()> {
    while event::poll(Duration::from_millis(0))? {
        let _ = event::read()?;
    }
    Ok(())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        if let Err(err) = execute!(io::stdout(), cursor::Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show);
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_input_claim_tracks_nested_guards() {
        assert!(!terminal_input_active());

        {
            let _first_claim = claim_terminal_input();
            assert!(terminal_input_active());

            {
                let _second_claim = claim_terminal_input();
                assert!(terminal_input_active());
            }

            assert!(terminal_input_active());
        }

        assert!(!terminal_input_active());
    }
}
