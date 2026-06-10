//! Integration tests for PTY tab completion (control-pipe JSON).

use std::time::Duration;

use aish_pty::PersistentPty;

fn with_pty<F, R>(f: F) -> R
where
    F: FnOnce(&mut PersistentPty) -> R,
{
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/tmp".to_string());
    let mut pty = PersistentPty::start(&cwd, 24, 80).expect("start");
    let out = f(&mut pty);
    pty.stop();
    out
}

#[test]
fn query_completions_command_prefix() {
    with_pty(|pty| {
        let resp = pty
            .query_completions("gi", 2, Duration::from_secs(5))
            .expect("query");
        assert!(
            resp.candidates
                .iter()
                .any(|c| c.replacement.starts_with("git")),
            "expected git, got {:?}",
            resp.candidates
        );
    });
}

#[test]
fn query_completions_path_prefix() {
    with_pty(|pty| {
        let line = "ls /ho";
        let resp = pty
            .query_completions(line, line.len(), Duration::from_secs(5))
            .expect("query");
        assert!(
            resp.candidates
                .iter()
                .any(|c| c.replacement.starts_with("/home")),
            "expected /home, got {:?}",
            resp.candidates
        );
    });
}

#[test]
fn query_completions_ls_home_lists_children() {
    if !std::path::Path::new("/home").is_dir() {
        return;
    }
    with_pty(|pty| {
        let line = "ls /home/";
        let resp = pty
            .query_completions(line, line.len(), Duration::from_secs(5))
            .expect("query");
        assert!(resp.candidates.len() >= 2, "got {:?}", resp.candidates);
        assert!(resp
            .candidates
            .iter()
            .all(|c| !c.display.starts_with("/home/")));
    });
}

#[test]
fn query_completions_empty_line() {
    with_pty(|pty| {
        let resp = pty
            .query_completions("", 0, Duration::from_secs(5))
            .expect("query");
        assert!(resp.candidates.is_empty());
    });
}

#[test]
fn query_completions_absolute_path_at_word_zero() {
    if !std::path::Path::new("/home").is_dir() {
        return;
    }
    with_pty(|pty| {
        let resp = pty
            .query_completions("/ho", 3, Duration::from_secs(5))
            .expect("query");
        assert!(resp
            .candidates
            .iter()
            .any(|c| c.replacement.starts_with("/home")));

        let resp2 = pty
            .query_completions("/home/", 7, Duration::from_secs(5))
            .expect("query");
        assert!(resp2.candidates.len() >= 2);
    });
}

#[test]
fn query_completions_usr_path_at_word_zero() {
    if !std::path::Path::new("/usr").is_dir() {
        return;
    }
    with_pty(|pty| {
        let resp = pty
            .query_completions("/us", 3, Duration::from_secs(5))
            .expect("query");
        assert!(resp
            .candidates
            .iter()
            .any(|c| c.replacement.starts_with("/usr")));

        let resp2 = pty
            .query_completions("/usr/", 5, Duration::from_secs(5))
            .expect("query");
        assert!(resp2.candidates.len() >= 2);
        assert!(!resp2.candidates.iter().any(|c| c.replacement == "/usr/"));
    });
}

#[test]
fn query_completions_ls_usr_bin_directory() {
    if !std::path::Path::new("/usr/bin").is_dir() {
        return;
    }
    with_pty(|pty| {
        let start = std::time::Instant::now();
        let resp = pty
            .query_completions("ls /usr/bin", 11, Duration::from_secs(5))
            .expect("query");
        assert!(start.elapsed() < Duration::from_millis(800));
        assert!(!resp.candidates.is_empty());
        assert!(resp.candidates.len() <= 100);

        let start2 = std::time::Instant::now();
        let resp2 = pty
            .query_completions("ls /usr/bin/", 12, Duration::from_secs(5))
            .expect("query");
        assert!(start2.elapsed() < Duration::from_millis(1200));
        assert!(resp2.candidates.len() >= 2);
    });
}
