use std::fs;
use std::path::{Path, PathBuf};

use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::runtime::mount::MountStack;
use crate::sandbox::runtime::worker::WorkerHandle;

#[derive(Default)]
pub(crate) struct SandboxRunGuard {
    mounts: MountStack,
    worker: Option<WorkerHandle>,
    temp_dirs: Vec<PathBuf>,
}

impl SandboxRunGuard {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn mounts_mut(&mut self) -> &mut MountStack {
        &mut self.mounts
    }

    pub(crate) fn set_worker(&mut self, worker: WorkerHandle) {
        self.worker = Some(worker);
    }

    pub(crate) fn take_worker(&mut self) -> Option<WorkerHandle> {
        self.worker.take()
    }

    pub(crate) fn push_temp_dir(&mut self, path: impl Into<PathBuf>) {
        self.temp_dirs.push(path.into());
    }

    pub(crate) fn close(&mut self) -> Result<(), SandboxError> {
        let mut first_error = None;

        if let Some(worker) = &mut self.worker {
            match worker.close() {
                Ok(_) => self.worker = None,
                Err(error) => {
                    first_error = Some(error);
                    if !worker.is_active() {
                        self.worker = None;
                    }
                }
            }
        }

        if let Err(error) = self.mounts.close() {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }

        let mut remaining_temp_dirs = Vec::new();
        while let Some(path) = self.temp_dirs.pop() {
            if let Err(error) = remove_temp_dir(&path) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                remaining_temp_dirs.push(path);
            }
        }
        remaining_temp_dirs.reverse();
        self.temp_dirs = remaining_temp_dirs;

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(crate) fn finish(&mut self) -> Result<(), SandboxError> {
        self.close()
    }
}

impl Drop for SandboxRunGuard {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn remove_temp_dir(path: &Path) -> Result<(), SandboxError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(SandboxError::with_details(
            SandboxReason::SandboxCleanupFailed,
            format!("remove_temp_dir {}: {error}", path.display()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::os::raw::c_int;
    use std::path::Path;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::SandboxRunGuard;
    use crate::sandbox::error::SandboxReason;
    use crate::sandbox::runtime::mount::MountGuard;
    use crate::sandbox::runtime::worker::WorkerHandle;

    fn recording_unmount(
        events: Arc<Mutex<Vec<String>>>,
    ) -> impl Fn(&Path, c_int) -> io::Result<()> + Send + Sync + 'static {
        move |target, _flags| {
            events.lock().unwrap().push(target.display().to_string());
            Ok(())
        }
    }

    #[test]
    fn sandbox_run_guard_cleans_worker_mounts_and_temp_dirs() {
        let temp = tempdir().unwrap();
        let temp_path = temp.path().join("run");
        std::fs::create_dir(&temp_path).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let child = Command::new("sleep").arg("30").spawn().unwrap();

        let mut guard = SandboxRunGuard::new();
        guard.push_temp_dir(temp_path.clone());
        guard.mounts_mut().push(MountGuard::with_test_unmount(
            "/tmp/aish-root",
            recording_unmount(events.clone()),
        ));
        guard.mounts_mut().push(MountGuard::with_test_unmount(
            "/tmp/aish-root/deep",
            recording_unmount(events.clone()),
        ));
        guard.set_worker(WorkerHandle::new(child));

        guard.close().unwrap();

        assert!(!temp_path.exists());
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["/tmp/aish-root/deep", "/tmp/aish-root"]
        );
        assert!(guard.take_worker().is_none());
        guard.close().unwrap();
    }

    #[test]
    fn sandbox_run_guard_reports_cleanup_errors_after_attempting_all_resources() {
        let temp = tempdir().unwrap();
        let temp_path = temp.path().join("run");
        std::fs::create_dir(&temp_path).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));

        let mut guard = SandboxRunGuard::new();
        guard.push_temp_dir(temp_path.clone());
        guard.mounts_mut().push(MountGuard::with_test_unmount(
            "/tmp/aish-bad",
            move |target, _flags| {
                events.lock().unwrap().push(target.display().to_string());
                Err(io::Error::other("boom"))
            },
        ));

        let error = guard.close().unwrap_err();

        assert_eq!(error.reason(), SandboxReason::SandboxCleanupFailed);
        assert!(!temp_path.exists());
    }
}
