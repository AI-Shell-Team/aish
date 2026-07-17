use std::{
    io::{self, Write},
    time::Duration,
};

use crossterm::{cursor, event, execute, terminal};
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal, TerminalOptions, Viewport};
use thiserror::Error;

pub trait PanelComponent {
    type Output;

    fn desired_height(&self, terminal_width: u16, terminal_height: u16) -> u16;
    fn render(&self, frame: &mut ratatui::Frame<'_>, area: Rect);
    fn handle_event(&mut self, event: event::Event) -> PanelEvent<Self::Output>;
    /// Periodic redraw interval for animated panels. When `Some`, the runtime
    /// polls for input with this timeout and, on timeout, calls [`tick`](Self::tick)
    /// then redraws — enabling animations (e.g. a shimmer sweep). `None` (the
    /// default) means the panel only redraws on input events.
    fn tick_interval(&self) -> Option<Duration> {
        None
    }
    /// Advance animation state by one tick. Called by the runtime whenever no
    /// input arrives within [`tick_interval`](Self::tick_interval). Default no-op.
    fn tick(&mut self) {}
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

            // Animated panels request a tick interval; block for at most that
            // long so we redraw on timeout (advancing the animation) even
            // without input. Non-animated panels block indefinitely on input.
            let event = match component.tick_interval() {
                Some(interval) => {
                    if event::poll(interval)? {
                        event::read()?
                    } else {
                        component.tick();
                        continue;
                    }
                }
                None => event::read()?,
            };
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
