//! Terminal stream helpers shared by the PTY relay paths.
//!
//! aish relays bytes between a child PTY and the real terminal, so it owns two
//! responsibilities the kernel's line discipline does not split for us:
//!
//! * **Child → terminal**: terminal device queries (`ESC[6n` cursor position,
//!   `ESC[c` device attributes, `ESC[?…$p` mode reports, OSC colour queries,
//!   …) must reach the real terminal verbatim. Full-screen programs measure the
//!   surface, anchor their live region and detect host-side resizes with the
//!   answers; deleting the requests from their output silently degrades them
//!   (omp logs `TSP: terminal did not confirm the surface` / `resize anchor …
//!   cpr=timeout` and repaints its viewport against a stale model).
//! * **Terminal → child**: the answers come back on aish's stdin. A
//!   line-oriented program that never asked for them would echo or execute them
//!   as garbage, so report-shaped bytes are dropped from stdin until the child
//!   has shown that it inspects the terminal (full-screen setup or a device
//!   query) — after that they are exactly what the child is waiting for.
//!
//! [`normalize_crlf`] handles a separate relay artifact: the child's PTY slave
//! and the real terminal both apply `OPOST|ONLCR`, so a newline crossing both
//! layers is translated twice (the terminal receives `\r\r\n`).

use std::borrow::Cow;
use std::time::{Duration, Instant};

/// Longest incomplete escape sequence held back while filtering stdin.
/// Report sequences are short; anything longer is not a report.
const MAX_REPORT_PREFIX: usize = 32;

/// Tail of child output kept for TUI fingerprint detection. A setup sequence can
/// straddle two reads (`ESC[?25` + `l`), so detection looks at the previous tail
/// plus the head of the new chunk; 32 bytes covers the longest needle.
const TUI_DETECT_TAIL: usize = 32;

/// Longest a possible report prefix may be withheld before it is handed to the
/// child anyway.
///
/// Real terminal reports are relayed back-to-back (microseconds, at most a few
/// milliseconds through a multiplexer), so this only ever fires for input that
/// is *not* a report — a lone Escape keypress, or a paste whose tail happens to
/// look like a report prefix. Bounding it keeps such input from being withheld
/// until the next read.
pub(crate) const HELD_PREFIX_MAX_HOLD: Duration = Duration::from_millis(500);

/// Private-mode setups and queries (`ESC [ ? …`): full-screen programs turn
/// these on to take over the screen or ask the terminal what it supports.
/// Matched after the `ESC [ ?` prefix has been consumed.
const TUI_PRIVATE: &[&[u8]] = &[
    b"1049h", b"1047h", b"47h", // alternate screen
    b"25l", // hide cursor
    b"1000h", b"1002h", b"1003h", b"1006h", b"1015h", // mouse tracking
    b"2004h", // bracketed paste
    b"2026h", // synchronized output
    b"2031h", b"2048h",  // mode notifications
    b"2;1;0S", // keyboard/surface capability probe
];

/// Remaining leads: cursor-position report (`ESC [ 6 n`), device attributes
/// (`ESC [ c`, `ESC [ > c`, `ESC [ = c`), APC strings (`ESC _`, e.g. omp's
/// terminal-surface handshake) and OSC colour queries (`ESC ] 11 ; ?`).
const TUI_LEADS: &[&[u8]] = &[
    b"\x1b[6n",
    b"\x1b[c",
    b"\x1b[>c",
    b"\x1b[=c",
    b"\x1b_",
    b"\x1b]10;?",
    b"\x1b]11;?",
    b"\x1b]4;",
];

/// True when `data` contains an escape sequence that only full-screen / TUI
/// programs emit. Used to decide whether the real terminal's answers must be
/// forwarded to the child.
///
/// Single pass: an ESC-heavy stream (colored `ls`, a build log) costs one byte
/// scan plus a handful of short prefix comparisons per escape, so the relay
/// stays at raw-throughput speed. Matching every needle at every offset instead
/// cost ~7x on a 4 MB ESC-dense stream (measured), which is why the leads are
/// grouped and dispatched on the byte after `ESC`.
pub(crate) fn looks_like_tui_output(data: &[u8]) -> bool {
    let mut offset = 0;
    while let Some(found) = data[offset..].iter().position(|&byte| byte == 0x1b) {
        let pos = offset + found;
        if tui_sequence_at(&data[pos..]) {
            return true;
        }
        offset = pos + 1;
    }
    false
}

/// Does a TUI-only escape sequence start at the beginning of `rest`?
fn tui_sequence_at(rest: &[u8]) -> bool {
    if let Some(tail) = rest.strip_prefix(b"\x1b[?") {
        return TUI_PRIVATE.iter().any(|needle| tail.starts_with(needle));
    }
    TUI_LEADS.iter().any(|lead| rest.starts_with(lead))
}

/// Final bytes of a terminal *report* (the answer to a device query), plus the
/// `$y` form used by mode reports (`ESC[?2026;1$y`).
fn is_report_final(byte: u8) -> bool {
    matches!(byte, b'R' | b'c' | b'n' | b't' | b'y' | b'S')
}

/// Parse `ESC [ <private?> <digits/;/:>* <final>` and return its length when it
/// is a complete report sequence.
///
/// Requiring at least one digit keeps real key sequences out of the set: arrows
/// (`ESC[A`), function keys (`ESC[11~`) and modified keys (`ESC[1;5A`) have no
/// digit-terminated report shape, and reports (`ESC[12;40R`, `ESC[?1;2c`,
/// `ESC[>0;276;0c`, `ESC[?2026;1$y`) always carry parameters.
fn report_len(seq: &[u8]) -> Option<usize> {
    if seq.len() < 4 || seq[0] != 0x1b || seq[1] != b'[' {
        return None;
    }
    let mut i = 2;
    if matches!(seq[i], b'?' | b'>' | b'=' | b'<' | b'!') {
        i += 1;
    }
    let digits_start = i;
    while i < seq.len() && (seq[i].is_ascii_digit() || seq[i] == b';' || seq[i] == b':') {
        i += 1;
    }
    let has_digit = seq[digits_start..i].iter().any(u8::is_ascii_digit);
    if !has_digit || i >= seq.len() {
        return None;
    }
    let final_byte = seq[i];
    // `$y` mode report: the `$` sits between the parameters and the final byte.
    if final_byte == b'$' {
        if i + 1 < seq.len() && seq[i + 1] == b'y' {
            return Some(i + 2);
        }
        return None;
    }
    if is_report_final(final_byte) {
        return Some(i + 1);
    }
    None
}

/// True when `seq` may still turn into a report once more bytes arrive, i.e. it
/// is a well-formed report prefix with nothing but parameters so far.
fn is_report_prefix(seq: &[u8]) -> bool {
    if seq.len() < 2 || seq[0] != 0x1b || seq[1] != b'[' {
        return false;
    }
    let mut i = 2;
    if i >= seq.len() {
        return true;
    }
    if matches!(seq[i], b'?' | b'>' | b'=' | b'<' | b'!') {
        i += 1;
    }
    seq[i..]
        .iter()
        .all(|&b| b.is_ascii_digit() || b == b';' || b == b':' || b == b'$')
}

/// Streaming filter for bytes read off the real terminal's stdin.
///
/// While [`Self::drop_reports`] is armed, complete report sequences are dropped
/// and incomplete prefixes are held until the next read (or [`Self::flush`]).
/// Once the child shows TUI behaviour the filter disarms and every byte is
/// forwarded, because the child is waiting for those answers.
///
/// Before it disarms, a *pasted* sequence that happens to look exactly like a
/// terminal report (`ESC[1;2R`, …) is dropped as well; a pasted report is
/// byte-identical to the leak this filter exists to prevent, so there is no way
/// to tell them apart. Programs that paste at all turn on bracketed paste
/// (`ESC[?2004h`), which disarms the filter first.
pub(crate) struct TerminalStreamFilter {
    drop_reports: bool,
    pending: Vec<u8>,
    /// When `pending` started, for [`Self::release_expired`].
    held_since: Option<Instant>,
    /// Trailing child-output bytes, for fingerprints split across reads.
    detect_tail: [u8; TUI_DETECT_TAIL],
    detect_tail_len: usize,
}

impl TerminalStreamFilter {
    pub(crate) fn new() -> Self {
        Self {
            drop_reports: true,
            pending: Vec::new(),
            held_since: None,
            detect_tail: [0; TUI_DETECT_TAIL],
            detect_tail_len: 0,
        }
    }

    /// Create a filter that never drops reports (interactive sessions such as
    /// ssh/telnet forward everything by definition).
    pub(crate) fn passthrough() -> Self {
        Self {
            drop_reports: false,
            pending: Vec::new(),
            held_since: None,
            detect_tail: [0; TUI_DETECT_TAIL],
            detect_tail_len: 0,
        }
    }

    /// Feed child output so the filter can disarm for full-screen programs.
    ///
    /// A sequence can straddle two reads, so the previous tail is checked
    /// together with the head of this chunk as well as the chunk itself.
    pub(crate) fn observe_child_output(&mut self, data: &[u8]) {
        if !self.drop_reports || data.is_empty() {
            return;
        }
        if looks_like_tui_output(data) || self.boundary_matches(data) {
            self.drop_reports = false;
            self.detect_tail_len = 0;
            return;
        }
        let keep = data.len().min(TUI_DETECT_TAIL);
        self.detect_tail[..keep].copy_from_slice(&data[data.len() - keep..]);
        self.detect_tail_len = keep;
    }

    /// True when a TUI fingerprint spans the previous tail and this chunk's head.
    fn boundary_matches(&self, data: &[u8]) -> bool {
        if self.detect_tail_len == 0 {
            return false;
        }
        let head_len = data.len().min(TUI_DETECT_TAIL);
        let mut window = [0u8; 2 * TUI_DETECT_TAIL];
        window[..self.detect_tail_len].copy_from_slice(&self.detect_tail[..self.detect_tail_len]);
        window[self.detect_tail_len..self.detect_tail_len + head_len]
            .copy_from_slice(&data[..head_len]);
        looks_like_tui_output(&window[..self.detect_tail_len + head_len])
    }

    /// Filter one stdin chunk, returning the bytes to forward to the child.
    pub(crate) fn filter(&mut self, data: &[u8]) -> Vec<u8> {
        if data.is_empty() && self.pending.is_empty() {
            return Vec::new();
        }
        if !self.drop_reports {
            self.held_since = None;
            if self.pending.is_empty() {
                return data.to_vec();
            }
            let mut out = std::mem::take(&mut self.pending);
            out.extend_from_slice(data);
            return out;
        }
        let scan_input: Cow<'_, [u8]> = if self.pending.is_empty() {
            Cow::Borrowed(data)
        } else {
            let mut merged = std::mem::take(&mut self.pending);
            merged.extend_from_slice(data);
            Cow::Owned(merged)
        };
        self.scan(&scan_input)
    }

    /// Hand held bytes to the child once they have been withheld for longer
    /// than `max_hold`: a report that has not completed by then is not a report
    /// (see [`HELD_PREFIX_MAX_HOLD`]). Returns the bytes to forward, if any.
    pub(crate) fn release_expired(&mut self, max_hold: Duration) -> Vec<u8> {
        let expired = self
            .held_since
            .is_some_and(|since| since.elapsed() >= max_hold);
        if !expired {
            return Vec::new();
        }
        self.held_since = None;
        std::mem::take(&mut self.pending)
    }

    /// Drop bytes still held when the command ends: injecting a report-shaped
    /// prefix into the shell prompt would be worse than losing it.
    pub(crate) fn flush(&mut self) -> Vec<u8> {
        self.held_since = None;
        std::mem::take(&mut self.pending)
    }

    fn scan(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        self.held_since = None;
        while i < data.len() {
            if data[i] == 0x1b {
                let rest = &data[i..];
                if let Some(len) = report_len(rest) {
                    i += len;
                    continue;
                }
                // Hold a possible report prefix for the next read: an ESC alone
                // (a multiplexer can relay the answer in several writes, so the
                // rest may arrive in the next chunk), or a well-formed prefix
                // that is still short enough to be a report. Anything else —
                // including a prefix longer than any real report — is forwarded
                // verbatim.
                let holdable =
                    rest.len() == 1 || (is_report_prefix(rest) && rest.len() <= MAX_REPORT_PREFIX);
                if holdable {
                    if self.pending.is_empty() {
                        self.held_since = Some(Instant::now());
                    }
                    self.pending.extend_from_slice(rest);
                    return out;
                }
            }
            out.push(data[i]);
            i += 1;
        }
        out
    }
}

impl Default for TerminalStreamFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Collapse the redundant CR of a `\r\n` pair.
///
/// The child's PTY slave translates `\n` to `\r\n` (`OPOST|ONLCR`), and aish
/// writes those bytes to the real terminal, whose line discipline applies the
/// same translation again — so the terminal receives `\r\r\n`. Writing the pair
/// as a bare `\n` restores a single translation and makes the relayed stream
/// match what the terminal would have received had the child been attached to
/// it directly (a bare `\n` from a program that put its own tty in raw mode
/// keeps working: the terminal's own `ONLCR` provides the CR).
///
/// A trailing `\r` is left alone; it reaches the terminal as-is and the extra
/// CR of a pair split across two writes stays harmless.
///
/// Premise: the real terminal's line discipline has `OPOST|ONLCR`, i.e. it
/// turns a bare `\n` into CR+LF. That is the default, and every relay path that
/// puts a tty in raw mode re-enables it explicitly (see `send_command_interactive`
/// and `execute_command`); without the premise the terminal would move down a
/// line without returning to column 0.
pub(crate) fn normalize_crlf(data: &[u8]) -> Cow<'_, [u8]> {
    if !data.windows(2).any(|pair| pair == b"\r\n") {
        return Cow::Borrowed(data);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\r' && data.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(data[i]);
        i += 1;
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_detection_matches_full_screen_setup() {
        assert!(looks_like_tui_output(b"\x1b[?1049hframe"));
        assert!(looks_like_tui_output(b"\x1b[?25lhidden"));
        assert!(looks_like_tui_output(b"\x1b[?2004h"));
        assert!(looks_like_tui_output(b"\x1b[?2026hframe\x1b[?2026l"));
        assert!(looks_like_tui_output(b"\x1b[?1006h"));
        assert!(looks_like_tui_output(b"\x1b[>c"));
        assert!(looks_like_tui_output(b"x\x1b_apc;payload\x1b\\"));
    }

    #[test]
    fn tui_detection_ignores_line_oriented_output() {
        assert!(!looks_like_tui_output(b"total 4\ndrwxr-xr-x 2 user user\n"));
        assert!(!looks_like_tui_output(b"\x1b[31mred\x1b[0m text"));
        assert!(!looks_like_tui_output(b"\rprogress 10%\x1b[K"));
    }

    #[test]
    fn tui_detection_fast_path_without_esc() {
        assert!(!looks_like_tui_output(b"plain text"));
    }

    #[test]
    fn filter_drops_complete_reports() {
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"\x1b[12;40R").len(), 0);
        assert_eq!(f.filter(b"\x1b[?1;2c").len(), 0);
        assert_eq!(f.filter(b"\x1b[>0;276;0c").len(), 0);
        assert_eq!(f.filter(b"\x1b[?2026;1$y").len(), 0);
    }

    #[test]
    fn filter_keeps_real_keystrokes() {
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"ls -la\n"), b"ls -la\n");
        assert_eq!(f.filter(b"\x1b[A"), b"\x1b[A");
        assert_eq!(f.filter(b"\x1b[11~"), b"\x1b[11~");
        assert_eq!(f.filter(b"\x1b[1;5A"), b"\x1b[1;5A");
        assert_eq!(f.filter(b"\x1b[Z"), b"\x1b[Z");
        assert_eq!(f.filter(b"\x1b."), b"\x1b.");
    }

    #[test]
    fn filter_holds_incomplete_report_until_next_chunk() {
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"a\x1b[24;"), b"a");
        assert_eq!(f.filter(b"80Rb"), b"b");
    }

    #[test]
    fn filter_holds_lone_esc_until_the_rest_arrives() {
        // A multiplexer can relay a report in several writes: the ESC alone must
        // not reach a line-oriented child, and the completed report is dropped.
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"\x1b"), Vec::<u8>::new());
        assert_eq!(f.filter(b"[1;17R"), Vec::<u8>::new());
    }

    #[test]
    fn filter_releases_lone_esc_when_it_is_not_a_report() {
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"\x1b"), Vec::<u8>::new());
        assert_eq!(f.filter(b"x"), b"\x1bx");
    }

    #[test]
    fn filter_does_not_hold_a_trailing_esc_after_disarm() {
        let mut f = TerminalStreamFilter::new();
        f.observe_child_output(b"\x1b[?25l");
        assert_eq!(f.filter(b"\x1b"), b"\x1b");
    }

    #[test]
    fn held_prefix_is_released_after_the_hold_bound() {
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"\x1b"), Vec::<u8>::new());
        // Not yet expired.
        assert_eq!(f.release_expired(Duration::from_secs(60)), Vec::<u8>::new());
        // Expired: the byte goes to the child instead of waiting for a read.
        assert_eq!(f.release_expired(Duration::ZERO), b"\x1b");
        assert_eq!(f.release_expired(Duration::ZERO), Vec::<u8>::new());
        // Later input is forwarded normally.
        assert_eq!(f.filter(b"x"), b"x");
    }

    #[test]
    fn held_prefix_release_does_not_fire_while_a_report_is_completing() {
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"a\x1b[12;"), b"a");
        // The continuation arrives before the bound: the report is still dropped.
        assert_eq!(f.release_expired(Duration::from_secs(60)), Vec::<u8>::new());
        assert_eq!(f.filter(b"40R"), Vec::<u8>::new());
    }

    #[test]
    fn tui_detection_handles_a_sequence_split_across_reads() {
        let mut f = TerminalStreamFilter::new();
        f.observe_child_output(b"\x1b[?25");
        assert_eq!(f.filter(b"\x1b[12;40R"), Vec::<u8>::new());
        f.observe_child_output(b"l");
        // Now disarmed: the child is a full-screen program and needs its answers.
        assert_eq!(f.filter(b"\x1b[12;40R"), b"\x1b[12;40R");
    }

    #[test]
    fn tui_detection_tail_does_not_match_unrelated_output() {
        let mut f = TerminalStreamFilter::new();
        f.observe_child_output(b"plain line one\n");
        f.observe_child_output(b"plain line two\n");
        assert_eq!(f.filter(b"\x1b[12;40R"), Vec::<u8>::new());
    }

    #[test]
    fn filter_forwards_everything_after_tui_detection() {
        let mut f = TerminalStreamFilter::new();
        f.observe_child_output(b"\x1b[?25l");
        assert_eq!(f.filter(b"\x1b[12;40R"), b"\x1b[12;40R");
    }

    #[test]
    fn passthrough_filter_never_drops() {
        let mut f = TerminalStreamFilter::passthrough();
        assert_eq!(f.filter(b"\x1b[12;40R"), b"\x1b[12;40R");
    }

    #[test]
    fn flush_releases_held_prefix() {
        let mut f = TerminalStreamFilter::new();
        assert_eq!(f.filter(b"\x1b[24;"), Vec::<u8>::new());
        assert_eq!(f.flush(), b"\x1b[24;");
        assert!(f.flush().is_empty());
    }

    #[test]
    fn normalize_crlf_collapses_only_pairs() {
        assert_eq!(&normalize_crlf(b"A\r\nB\r\n")[..], b"A\nB\n");
        assert_eq!(
            &normalize_crlf(b"frame\r\x1b[2Knext")[..],
            b"frame\r\x1b[2Knext"
        );
        assert_eq!(&normalize_crlf(b"tail\r")[..], b"tail\r");
        assert!(matches!(normalize_crlf(b"no newlines"), Cow::Borrowed(_)));
    }
}
