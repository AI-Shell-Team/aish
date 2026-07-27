//! Small text-presentation helpers shared by the shell and the CLI.

use std::sync::LazyLock;

use regex::Regex;

/// Strip ANSI/VT escape sequences and stray C0 control characters from
/// attacker-controlled text before printing it during a security review.
///
/// Without this, a malicious `SKILL.md` could embed ANSI escapes to repaint the
/// screen, hide lines, or forge a "no issues found" verdict at exactly the
/// moment the user decides whether to trust a freshly installed skill.
///
/// The regex is compiled once and cached (matching the workspace convention);
/// afterward, every non-escape character is kept unless it is a C0 control —
/// newline and tab are preserved so the review output stays readable.
pub fn strip_ansi_escapes(s: &str) -> String {
    static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
        // OSC (ESC ] ... BEL or ST), CSI (ESC [ ... final byte), and any other
        // ESC + single final byte (0x30-0x7E) — covers ESC 7/8 (DECSC/DECRC),
        // ESC = / > (keypad mode), not just the @-_ sub-range.
        Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[0-~]")
            .expect("valid ansi-strip regex")
    });
    ANSI_RE
        .replace_all(s, "")
        .chars()
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::strip_ansi_escapes;

    #[test]
    fn strips_csi_color_sequences() {
        // A red/bold "no issues" line must collapse to plain text so a
        // malicious skill cannot paint a forged green verdict.
        assert_eq!(
            strip_ansi_escapes("\x1b[31;1mno issues found\x1b[0m"),
            "no issues found"
        );
    }

    #[test]
    fn strips_osc_title_sequence() {
        // OSC sets the terminal title then resets; both must vanish.
        assert_eq!(strip_ansi_escapes("\x1b]0;fake\x07clean"), "clean");
        assert_eq!(strip_ansi_escapes("\x1b]2;title\x1b\\clean"), "clean");
    }

    #[test]
    fn strips_bare_escape_then_char() {
        assert_eq!(strip_ansi_escapes("\x1b7keep\x1b8"), "keep");
    }

    #[test]
    fn preserves_newline_tab_and_utf8() {
        assert_eq!(strip_ansi_escapes("line1\n\tliñe2"), "line1\n\tliñe2");
    }

    #[test]
    fn drops_other_c0_controls() {
        // Backspace/vertical-tab (used to overprint or scroll) are removed.
        assert_eq!(strip_ansi_escapes("a\x08b\x0bc"), "abc");
    }
}
