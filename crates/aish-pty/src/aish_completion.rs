//! PTY-side tab completion: JSON queries and readline Tab forwarding.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg};

use super::PersistentPty;
use crate::control::{decode_control_chunk, BackendControlEvent, CompletionResponse};
use crate::readline_tab::{
    build_readline_tab_payload, clamp_pos, parse_readline_tab_output, ReadlineTabResult,
};

const MASTER_READ_SIZE: usize = 8192;
const CONTROL_READ_SIZE: usize = 4096;
const TAB_IDLE_QUIET: Duration = Duration::from_millis(60);

impl PersistentPty {
    /// Query tab completions via bash `__aish_complete` (control pipe JSON).
    pub fn query_completions(
        &mut self,
        line: &str,
        pos: usize,
        timeout: Duration,
    ) -> aish_core::Result<CompletionResponse> {
        if !self.is_running() {
            return Ok(CompletionResponse::empty());
        }

        let request_id = self
            .next_completion_request_id
            .fetch_add(1, Ordering::SeqCst)
            .saturating_add(1);
        let seq = self.allocate_backend_seq();
        let cmd = format!(
            "__aish_complete {request_id} {} {pos}",
            super::shell_quote_escape(line)
        );

        self.exec_buffer.lock().unwrap().clear();
        self.exec_mode.store(true, Ordering::SeqCst);
        self.send_command(&cmd, Some(seq))?;

        let deadline = Instant::now() + timeout;
        let mut completion = None;

        'wait: while Instant::now() < deadline {
            let mut fds = zero_fd_set();
            set_fd(&mut fds, self.master_fd);
            set_fd(&mut fds, self.control_fd);
            let max_fd = self.master_fd.max(self.control_fd) + 1;

            if select_fds(max_fd, &mut fds, select_tv(deadline, Duration::from_secs(1))) <= 0 {
                continue;
            }

            if fd_isset(self.master_fd, &fds) {
                match self.read_master_into_exec() {
                    ReadMaster::Data => {}
                    ReadMaster::Eof => break,
                    ReadMaster::Ignored => {}
                }
            }

            if fd_isset(self.control_fd, &fds) {
                let events = self.read_control_chunk();
                for event in &events {
                    if matches!(event, BackendControlEvent::ShellExiting { .. }) {
                        self.running.store(false, Ordering::SeqCst);
                    }
                    if let BackendControlEvent::CompletionResult {
                        request_id: rid,
                        word_start,
                        candidates,
                    } = event
                    {
                        if *rid == request_id {
                            completion = Some(CompletionResponse {
                                word_start: *word_start,
                                candidates: candidates.clone(),
                            });
                            break 'wait;
                        }
                    }
                    if let Some(r) = self.command_state.handle_event(event) {
                        if r.command_seq == Some(seq) {
                            break 'wait;
                        }
                    }
                }
                if events.is_empty() && !self.is_running() {
                    break;
                }
            }
        }

        self.drain_master_to_exec_buffer();
        self.exec_mode.store(false, Ordering::SeqCst);
        Ok(completion.unwrap_or_else(CompletionResponse::empty))
    }

    /// Forward Tab to bash readline; capture PTY output without writing to stdout.
    pub fn forward_readline_tab(
        &mut self,
        line: &str,
        pos: usize,
        timeout: Duration,
    ) -> Option<ReadlineTabResult> {
        if !self.is_running() || line.is_empty() {
            return None;
        }

        let pos = clamp_pos(line, pos);
        self.run_hidden_bash_line("set -o emacs");
        self.set_pty_echo(true);
        self.drain_master_silent();

        let half = timeout / 2;
        let cap1 = self.capture_readline_probe(
            &build_readline_tab_payload(line, pos, 1, false),
            half,
        );
        let mut result = parse_readline_tab_output(&cap1, line, pos);
        if !result.is_useful(line) {
            let mut combined = cap1;
            combined.extend_from_slice(&self.capture_readline_probe(b"\t", half));
            result = parse_readline_tab_output(&combined, line, pos);
        }

        self.set_pty_echo(false);
        let _ = self.write_master(b"\x15");
        self.drain_master_silent();
        self.run_hidden_bash_line("set +o emacs; set +o vi");
        result.is_useful(line).then_some(result)
    }

    fn capture_readline_probe(&mut self, payload: &[u8], timeout: Duration) -> Vec<u8> {
        self.drain_master_silent();
        self.exec_buffer.lock().unwrap().clear();
        self.exec_mode.store(true, Ordering::SeqCst);
        if self.write_master(payload).is_err() {
            self.exec_mode.store(false, Ordering::SeqCst);
            return Vec::new();
        }

        let deadline = Instant::now() + timeout;
        let mut last_read = Instant::now();
        while Instant::now() < deadline {
            let mut fds = zero_fd_set();
            set_fd(&mut fds, self.master_fd);
            let tick = deadline.saturating_duration_since(Instant::now()).min(TAB_IDLE_QUIET);
            if select_fds(self.master_fd + 1, &mut fds, timeval_from_duration(tick)) <= 0 {
                if last_read.elapsed() >= TAB_IDLE_QUIET
                    && !self.exec_buffer.lock().unwrap().is_empty()
                {
                    break;
                }
                continue;
            }
            match self.read_master_into_exec() {
                ReadMaster::Data => last_read = Instant::now(),
                ReadMaster::Eof | ReadMaster::Ignored => break,
            }
        }

        self.exec_mode.store(false, Ordering::SeqCst);
        self.exec_buffer.lock().unwrap().clone()
    }

    fn run_hidden_bash_line(&mut self, command: &str) {
        self.drain_master_silent();
        self.drain_control_pipe_raw();
        self.exec_buffer.lock().unwrap().clear();
        self.exec_mode.store(true, Ordering::SeqCst);
        if self.write_master(format!(" {command}\n").as_bytes()).is_err() {
            self.exec_mode.store(false, Ordering::SeqCst);
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
        self.drain_control_pipe_raw();
        self.exec_mode.store(false, Ordering::SeqCst);
        self.drain_master_silent();
    }

    fn set_pty_echo(&self, enabled: bool) {
        let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(self.master_fd) };
        if let Ok(mut term) = tcgetattr(fd) {
            if enabled {
                term.local_flags.insert(LocalFlags::ECHO | LocalFlags::ECHONL);
            } else {
                term.local_flags.remove(LocalFlags::ECHO | LocalFlags::ECHONL);
            }
            let _ = tcsetattr(fd, SetArg::TCSANOW, &term);
        }
    }

    fn read_control_chunk(&mut self) -> Vec<BackendControlEvent> {
        let mut tmp = [0u8; CONTROL_READ_SIZE];
        match unsafe {
            libc::read(
                self.control_fd,
                tmp.as_mut_ptr() as *mut libc::c_void,
                tmp.len(),
            )
        } {
            n if n > 0 => decode_control_chunk(&mut self.control_buffer, &tmp[..n as usize]),
            0 => {
                self.running.store(false, Ordering::SeqCst);
                vec![]
            }
            _ => vec![],
        }
    }

    fn read_master_into_exec(&self) -> ReadMaster {
        if !self.exec_mode.load(Ordering::SeqCst) {
            return ReadMaster::Ignored;
        }
        let mut tmp = [0u8; MASTER_READ_SIZE];
        match unsafe {
            libc::read(
                self.master_fd,
                tmp.as_mut_ptr() as *mut libc::c_void,
                tmp.len(),
            )
        } {
            n if n > 0 => {
                self.exec_buffer
                    .lock()
                    .unwrap()
                    .extend_from_slice(&tmp[..n as usize]);
                ReadMaster::Data
            }
            0 => {
                self.running.store(false, Ordering::SeqCst);
                ReadMaster::Eof
            }
            _ => ReadMaster::Ignored,
        }
    }
}

enum ReadMaster {
    Data,
    Eof,
    Ignored,
}

impl CompletionResponse {
    fn empty() -> Self {
        Self {
            word_start: 0,
            candidates: Vec::new(),
        }
    }
}

fn zero_fd_set() -> libc::fd_set {
    unsafe { std::mem::zeroed() }
}

fn set_fd(set: &mut libc::fd_set, fd: i32) {
    unsafe { libc::FD_SET(fd, set) };
}

fn fd_isset(fd: i32, set: &libc::fd_set) -> bool {
    unsafe { libc::FD_ISSET(fd, set) }
}

fn select_tv(deadline: Instant, max: Duration) -> libc::timeval {
    timeval_from_duration(deadline.saturating_duration_since(Instant::now()).min(max))
}

fn timeval_from_duration(d: Duration) -> libc::timeval {
    libc::timeval {
        tv_sec: d.as_secs().min(1) as libc::c_long,
        tv_usec: (d.subsec_micros() % 1_000_000) as libc::c_long,
    }
}

fn select_fds(max_fd: i32, fds: &mut libc::fd_set, tv: libc::timeval) -> i32 {
    let mut tv = tv;
    loop {
        let sel = unsafe {
            libc::select(
                max_fd,
                fds,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            )
        };
        if sel >= 0 {
            return sel;
        }
        let errno = unsafe { *libc::__errno_location() };
        if errno != libc::EINTR {
            return sel;
        }
    }
}
