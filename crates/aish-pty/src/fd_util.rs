use std::os::fd::{AsRawFd, OwnedFd, RawFd};

use nix::fcntl::{fcntl, FcntlArg, FdFlag};

use aish_core::{AishError, Result};

pub fn set_cloexec(fd: &OwnedFd) -> Result<()> {
    let raw = fd.as_raw_fd();
    fcntl(raw, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
        .map_err(|e| AishError::Pty(format!("fcntl F_SETFD FD_CLOEXEC failed: {e}")))?;
    Ok(())
}

/// Close all inherited parent fds from `start` up to the process's RLIMIT_NOFILE.
/// This is defense-in-depth alongside `FD_CLOEXEC`: CLOEXEC closes fds at
/// `execve` time, but this loop ensures they are closed even if a future fd
/// is added without CLOEXEC, or between `fork` and `execve`.
pub fn close_inherited_fds_from(start: RawFd) {
    let max = soft_fd_limit();
    let mut fd = start;
    while fd < max {
        let _ = nix::unistd::close(fd);
        fd += 1;
    }
}

fn soft_fd_limit() -> RawFd {
    let mut rlim = libc::rlimit {
        rlim_cur: 1024,
        rlim_max: 1024,
    };
    // SAFETY: [Category 8 — FFI] getrlimit writes into rlim, a valid mutable
    // buffer of the expected type.
    unsafe {
        libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim);
    }
    rlim.rlim_cur as RawFd
}
