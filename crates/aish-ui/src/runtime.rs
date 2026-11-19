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
        let height = component.desired_height(cols, rows);
        let _guard = TerminalGuard::enter()?;
        // Reserve the panel's rows by scrolling instead of `Viewport::Inline`.
        // Inline's `append_lines` scrolled too, but `Terminal::clear()` then
        // restored the cursor to the bottom of the panel and the next
        // `read_line` redrew the prompt several rows lower, leaving blank
        // lines above it.
        //
        // Shared with the slash/@ popups so the placement and erase policies
        // cannot drift apart: like `Viewport::Inline`, the screen scrolls by
        // the shortfall rather than the panel shrinking or content above it
        // being erased. A failed cursor query docks at the bottom rows.
        let mut anchor_row = crate::slash_input::cursor_anchor_row(rows, height);
        let (mut start_row, mut clamped_height) = {
            let (scroll, start_row, height) =
                crate::slash_input::fit_popup(anchor_row, rows, height);
            // Propagate: a partial scroll leaves the rows the panel is about to
            // claim unfreed, so the clear below would erase command output.
            crate::slash_input::scroll_up(scroll, rows)?;
            (start_row, height)
        };
        // The panel (and the prompt under it) moved up with the scroll, so
        // later refits must anchor here — re-anchoring at the pre-scroll row
        // would scroll again on every resize, including width-only changes.
        anchor_row = start_row;
        // Wipe stale terminal content in the panel region before the first
        // draw: a Fixed viewport only repaints its own cells, so any
        // previously printed command output would otherwise show through the
        // gaps between panel rows. Bounded to the panel's own rows.
        crate::slash_input::clear_rows(start_row, clamped_height)?;
        let area = Rect::new(0, start_row, cols, clamped_height);
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(area),
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
            // A `Viewport::Fixed` is never auto-resized by ratatui, so a
            // window change would leave the panel drawn at stale absolute
            // rows (and the exit cleanup erasing rows that no longer exist).
            // Re-fit it from the stored anchor instead.
            if matches!(event, event::Event::Resize(..)) {
                drop(terminal);
                let (c, r) = terminal::size()?;
                let h = component.desired_height(c, r);
                // Clip the old-region erase to the rows that still exist: a
                // VT cursor move past the bottom edge stops at the last row,
                // so an unclipped erase after a shrink would clear whatever
                // output now sits there.
                let old_end = start_row.saturating_add(clamped_height).min(r);
                if start_row < old_end {
                    let _ = crate::slash_input::clear_rows(start_row, old_end - start_row);
                }
                let (scroll, row, height) = crate::slash_input::fit_popup(anchor_row, r, h);
                // Propagate: a partial scroll leaves the panel's rows
                // unfreed, so clearing them would erase command output.
                crate::slash_input::scroll_up(scroll, r)?;
                let _ = crate::slash_input::clear_rows(row, height);
                start_row = row;
                clamped_height = height;
                // Anchor at the post-scroll row so a width-only resize does
                // not scroll again.
                anchor_row = row;
                terminal = Terminal::with_options(
                    CrosstermBackend::new(io::stdout()),
                    TerminalOptions {
                        viewport: Viewport::Fixed(Rect::new(0, row, c, height)),
                    },
                )?;
                continue;
            }
            match component.handle_event(event) {
                PanelEvent::Continue => continue,
                PanelEvent::Submit(value) => break PanelOutcome::Submitted(value),
                PanelEvent::Cancel => break PanelOutcome::Cancelled,
            }
        };

        // Erase the panel rows and park the cursor back on the panel's first
        // row. `Terminal::clear()` cannot be used here: on ratatui-core 0.1.2
        // it snapshots the cursor by DSR and restores it to wherever the last
        // draw left it (the bottom of the panel), which pushes the next
        // prompt several rows down and leaves blank lines above it. The
        // cleanup is best-effort so a failed write never discards an outcome
        // the component already submitted.
        let _ = crate::slash_input::clear_rows(start_row, clamped_height);
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
