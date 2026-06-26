//! Bash-style multi-column completion listing with a custom pager.

use std::io::{self, Write};
use std::time::Instant;

use rustyline::completion::Pair;
use unicode_width::UnicodeWidthStr;

/// Match rustyline default: ask before listing very large sets.
pub const COMPLETION_PROMPT_LIMIT: usize = 100;

const MORE_PROMPT: &str = "--More--";

/// Format candidates into display strings (deduped, sorted like bash columns).
pub fn display_strings(pairs: &[Pair]) -> Vec<String> {
    let mut out: Vec<String> = pairs.iter().map(|p| listing_display(&p.display)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Short names for column layout (basename for paths, keep trailing `/` on dirs).
fn listing_display(raw: &str) -> String {
    let s = raw.trim_end();
    if !s.contains('/') {
        return s.to_string();
    }
    if s.ends_with('/') {
        let trimmed = s.trim_end_matches('/');
        if let Some(base) = trimmed.rsplit('/').next().filter(|b| !b.is_empty()) {
            return format!("{base}/");
        }
        return s.to_string();
    }
    s.rsplit('/').next().unwrap_or(s).to_string()
}

/// Print a bash-style column listing with paging. Returns `Ok(false)` if the user
/// declined "display all", or stopped early at `--More--` with `q` / `n` / Backspace.
///
/// Caller must `Cmd::Repaint` afterward so rustyline redraws the prompt + input line;
/// manual stdout redraw corrupts the line when the prompt contains ANSI sequences.
pub fn print_completion_list(pairs: &[Pair]) -> io::Result<bool> {
    let displays = display_strings(pairs);
    if displays.is_empty() {
        return Ok(true);
    }

    if displays.len() > COMPLETION_PROMPT_LIMIT && !prompt_display_all(displays.len())? {
        return Ok(false);
    }

    drain_stdin_typeahead();
    let (cols, rows) = terminal_size();
    let layout_rows = format_column_rows(&displays, cols as usize);
    page_rows(&layout_rows, rows)?;
    drain_stdin_typeahead();
    Ok(true)
}

/// Drop buffered keys so rustyline / the pager do not mis-read them.
fn drain_stdin_typeahead() {
    let stdin_fd = libc::STDIN_FILENO;
    let mut buf = [0u8; 256];
    let deadline = Instant::now() + std::time::Duration::from_millis(200);
    loop {
        if Instant::now() >= deadline {
            break;
        }
        let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut fds);
            libc::FD_SET(stdin_fd, &mut fds);
        }
        let mut tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 20_000,
        };
        let ready = unsafe {
            libc::select(
                stdin_fd + 1,
                &mut fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if ready <= 0 {
            break;
        }
        let n = unsafe { libc::read(stdin_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

/// Prefer rustyline's cached size, then ioctl / `$LINES` / `$COLUMNS`.
fn terminal_size() -> (u16, u16) {
    let (mut cols, mut rows) = crate::readline::current_terminal_size();
    for fd in [libc::STDOUT_FILENO, libc::STDIN_FILENO, libc::STDERR_FILENO] {
        if let Some((c, r)) = winsize_ioctl(fd) {
            cols = cols.max(c);
            rows = rows.max(r);
        }
    }
    if let Ok(n) = std::env::var("COLUMNS") {
        if let Ok(n) = n.parse::<u16>() {
            cols = cols.max(n);
        }
    }
    if let Ok(n) = std::env::var("LINES") {
        if let Ok(n) = n.parse::<u16>() {
            rows = rows.max(n);
        }
    }
    // Do not clamp cols to 80 minimum: on a narrow terminal that forces line wrap
    // and looks like a single-column list. Fall back only when size is unknown.
    let cols = if cols == 0 { 80 } else { cols.min(512) };
    let rows = if rows == 0 { 24 } else { rows.min(5000) };
    (cols, rows)
}

fn winsize_ioctl(fd: i32) -> Option<(u16, u16)> {
    unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) != 0 {
            return None;
        }
        let cols = if size.ws_col == 0 {
            80
        } else {
            size.ws_col as u16
        };
        let rows = if size.ws_row == 0 {
            u16::MAX
        } else {
            size.ws_row as u16
        };
        Some((cols, rows))
    }
}

/// Lines of candidates to print before `--More--` (reserve one row for the prompt).
fn page_size(terminal_rows: u16, content_rows: usize) -> usize {
    let rows = terminal_rows as usize;
    if rows == 0 || rows > 5000 {
        return content_rows.max(1);
    }
    rows.saturating_sub(1).max(1)
}

fn prompt_display_all(count: usize) -> io::Result<bool> {
    let mut stdout = io::stdout();
    write!(stdout, "\r\nDisplay all {count} possibilities? (y/n) ")?;
    stdout.flush()?;

    loop {
        match read_pager_byte()? {
            PagerByte::Char(b'y') | PagerByte::Char(b'Y') => {
                stdout.flush()?;
                // Drop a trailing Enter users often press after `y`.
                drain_stdin_typeahead();
                return Ok(true);
            }
            PagerByte::Char(b'n') | PagerByte::Char(b'N') | PagerByte::Backspace => {
                stdout.flush()?;
                drain_stdin_typeahead();
                return Ok(false);
            }
            PagerByte::Interrupt => {
                stdout.flush()?;
                drain_stdin_typeahead();
                return Ok(false);
            }
            PagerByte::Ignored | PagerByte::Char(_) => {}
        }
    }
}

/// Column-major layout (same strategy as bash / GNU readline / rustyline).
pub fn format_column_rows(items: &[String], cols: usize) -> Vec<String> {
    if items.is_empty() {
        return Vec::new();
    }

    let cols = cols.max(1);
    let min_col_pad = 2;
    let max_width = items
        .iter()
        .map(|s| s.width())
        .max()
        .unwrap_or(0)
        .saturating_add(min_col_pad)
        .min(cols);
    let num_cols = (cols / max_width.max(1)).max(1);
    let num_rows = items.len().div_ceil(num_cols);

    let mut rows = Vec::with_capacity(num_rows);
    for row in 0..num_rows {
        let mut line = String::new();
        for col in 0..num_cols {
            let idx = col * num_rows + row;
            if idx >= items.len() {
                continue;
            }
            let item = &items[idx];
            line.push_str(item);
            if (col + 1) * num_rows + row < items.len() {
                let pad = max_width.saturating_sub(item.width());
                line.extend(std::iter::repeat_n(' ', pad));
            }
        }
        rows.push(line);
    }
    rows
}

fn page_rows(rows: &[String], terminal_rows: u16) -> io::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    let mut stdout = io::stdout();
    let page_size = page_size(terminal_rows, rows.len());
    let mut lines_on_page = 0usize;
    let mut reuse_more_line = false;

    for row in rows {
        while lines_on_page >= page_size {
            write!(stdout, "\n{MORE_PROMPT}")?;
            stdout.flush()?;
            drain_stdin_typeahead();
            match read_more_action()? {
                MoreAction::NextPage => lines_on_page = 0,
                MoreAction::NextLine => lines_on_page = page_size.saturating_sub(1),
                MoreAction::Stop => {
                    dismiss_more_prompt(&mut stdout)?;
                    return Ok(());
                }
            }
            erase_more_prompt(&mut stdout)?;
            reuse_more_line = true;
        }
        if reuse_more_line {
            write!(stdout, "{row}")?;
            reuse_more_line = false;
        } else {
            writeln!(stdout)?;
            write!(stdout, "{row}")?;
        }
        stdout.flush()?;
        lines_on_page += 1;
    }

    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

/// Clear `--More--` text but keep the line for the next candidate row.
fn erase_more_prompt(stdout: &mut io::Stdout) -> io::Result<()> {
    write!(stdout, "\r\x1b[2K")?;
    stdout.flush()?;
    Ok(())
}

/// Clear and remove the `--More--` pager line when the user stops (q / Ctrl+C).
fn dismiss_more_prompt(stdout: &mut io::Stdout) -> io::Result<()> {
    write!(stdout, "\r\x1b[2K\x1b[M")?;
    stdout.flush()?;
    Ok(())
}

enum MoreAction {
    NextPage,
    NextLine,
    Stop,
}

enum PagerByte {
    Char(u8),
    Backspace,
    Interrupt,
    Ignored,
}

/// Map one raw byte to a pager key (escape sequences are skipped in `read_stdin_byte`).
fn read_pager_byte() -> io::Result<PagerByte> {
    let byte = read_stdin_byte()?;
    match byte {
        b'\n' | b'\r' => Ok(PagerByte::Char(b'\n')),
        0x03 => Ok(PagerByte::Interrupt),
        0x7f | 0x08 => Ok(PagerByte::Backspace),
        0x01..=0x1a | 0x7e => Ok(PagerByte::Ignored),
        b => Ok(PagerByte::Char(b)),
    }
}

fn read_stdin_byte() -> io::Result<u8> {
    let mut buf = [0u8; 1];
    loop {
        let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, 1) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        let byte = buf[0];
        if byte == 0x1b {
            skip_escape_sequence()?;
            continue;
        }
        return Ok(byte);
    }
}

fn skip_escape_sequence() -> io::Result<()> {
    let mut buf = [0u8; 1];
    if !stdin_has_data(50_000)? {
        return Ok(());
    }
    let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, 1) };
    if n <= 0 {
        return Ok(());
    }
    if buf[0] == b'[' {
        loop {
            if !stdin_has_data(100_000)? {
                break;
            }
            let n =
                unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, 1) };
            if n <= 0 {
                break;
            }
            if (0x40..=0x7e).contains(&buf[0]) {
                break;
            }
        }
    }
    Ok(())
}

fn stdin_has_data(timeout_usec: libc::suseconds_t) -> io::Result<bool> {
    let stdin_fd = libc::STDIN_FILENO;
    let mut fds: libc::fd_set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::FD_ZERO(&mut fds);
        libc::FD_SET(stdin_fd, &mut fds);
    }
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: timeout_usec,
    };
    let ready = unsafe {
        libc::select(
            stdin_fd + 1,
            &mut fds,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut tv,
        )
    };
    if ready < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ready > 0)
}

/// Keys match GNU readline / rustyline `page_completions`.
fn read_more_action() -> io::Result<MoreAction> {
    loop {
        match read_pager_byte()? {
            PagerByte::Char(b' ') | PagerByte::Char(b'y') | PagerByte::Char(b'Y') => {
                return Ok(MoreAction::NextPage);
            }
            PagerByte::Char(b'\n') | PagerByte::Char(b'\r') => {
                return Ok(MoreAction::NextLine);
            }
            PagerByte::Char(b'q')
            | PagerByte::Char(b'Q')
            | PagerByte::Char(b'n')
            | PagerByte::Char(b'N')
            | PagerByte::Backspace
            | PagerByte::Interrupt => {
                return Ok(MoreAction::Stop);
            }
            PagerByte::Ignored | PagerByte::Char(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_column_rows_multi_column_for_binaries() {
        let items: Vec<String> = vec![
            "arj",
            "arj-register",
            "as",
            "autoconf",
            "automake-1.17",
            "bash",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let cols = 120;
        let rows = format_column_rows(&items, cols);
        assert_eq!(rows.len(), 1, "expected one layout row: {rows:?}");
        assert!(rows[0].contains("arj"));
        assert!(rows[0].contains("bash"));
        assert!(rows[0].width() <= cols);
    }

    #[test]
    fn format_column_rows_respects_narrow_terminal() {
        let items: Vec<String> = (0..12).map(|i| format!("cmd{i}")).collect();
        let cols = 40;
        let rows = format_column_rows(&items, cols);
        assert!(rows.len() < items.len());
        for row in &rows {
            assert!(row.width() <= cols, "row {} wider than {cols}", row.width());
        }
    }

    #[test]
    fn listing_display_uses_basename() {
        assert_eq!(listing_display("/usr/bin/arj"), "arj");
        assert_eq!(listing_display("/usr/bin/foo/"), "foo/");
    }

    #[test]
    fn display_strings_dedupes_and_sorts() {
        let pairs = vec![
            Pair {
                display: "git".into(),
                replacement: "git ".into(),
            },
            Pair {
                display: "git".into(),
                replacement: "git ".into(),
            },
            Pair {
                display: "grep".into(),
                replacement: "grep ".into(),
            },
        ];
        assert_eq!(display_strings(&pairs), vec!["git", "grep"]);
    }

    #[test]
    fn page_size_matches_rustyline() {
        assert_eq!(page_size(0, 50), 50);
        assert_eq!(page_size(u16::MAX, 50), 50);
        assert_eq!(page_size(24, 50), 23);
        assert_eq!(page_size(61, 200), 60);
    }
}
