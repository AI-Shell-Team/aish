use std::ffi::CString;
use std::io;
use std::os::raw::c_int;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::sandbox::error::{SandboxError, SandboxReason};

pub(crate) type UnmountFn = dyn Fn(&Path, c_int) -> io::Result<()> + Send + Sync + 'static;
type MountFn = dyn Fn(&MountCall) -> io::Result<()> + Send + Sync + 'static;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MountSpec {
    Overlay {
        lowerdir: PathBuf,
        upperdir: PathBuf,
        workdir: PathBuf,
        target: PathBuf,
    },
    Bind {
        source: PathBuf,
        target: PathBuf,
        recursive: bool,
    },
    RemountReadonly {
        target: PathBuf,
    },
}

impl MountSpec {
    pub(crate) fn overlay(
        lowerdir: impl Into<PathBuf>,
        upperdir: impl Into<PathBuf>,
        workdir: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
    ) -> Self {
        Self::Overlay {
            lowerdir: lowerdir.into(),
            upperdir: upperdir.into(),
            workdir: workdir.into(),
            target: target.into(),
        }
    }

    pub(crate) fn bind(
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        recursive: bool,
    ) -> Self {
        Self::Bind {
            source: source.into(),
            target: target.into(),
            recursive,
        }
    }

    pub(crate) fn remount_readonly(target: impl Into<PathBuf>) -> Self {
        Self::RemountReadonly {
            target: target.into(),
        }
    }

    pub(crate) fn target(&self) -> &Path {
        match self {
            Self::Overlay { target, .. }
            | Self::Bind { target, .. }
            | Self::RemountReadonly { target } => target,
        }
    }

    pub(crate) fn reason(&self) -> SandboxReason {
        match self {
            Self::Overlay { .. } => SandboxReason::OverlayMountFailed,
            Self::Bind { .. } => SandboxReason::BindMountFailed,
            Self::RemountReadonly { .. } => SandboxReason::RemountRoFailed,
        }
    }

    pub(crate) fn to_mount_call(&self) -> MountCall {
        match self {
            Self::Overlay {
                lowerdir,
                upperdir,
                workdir,
                target,
            } => MountCall {
                source: Some("overlay".to_string()),
                target: target.clone(),
                fstype: Some("overlay".to_string()),
                flags: 0,
                data: Some(format!(
                    "lowerdir={},upperdir={},workdir={}",
                    lowerdir.display(),
                    upperdir.display(),
                    workdir.display()
                )),
            },
            Self::Bind {
                source,
                target,
                recursive,
            } => MountCall {
                source: Some(source.display().to_string()),
                target: target.clone(),
                fstype: None,
                flags: libc::MS_BIND | if *recursive { libc::MS_REC } else { 0 },
                data: None,
            },
            Self::RemountReadonly { target } => MountCall {
                source: None,
                target: target.clone(),
                fstype: None,
                flags: libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
                data: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountCall {
    pub(crate) source: Option<String>,
    pub(crate) target: PathBuf,
    pub(crate) fstype: Option<String>,
    pub(crate) flags: libc::c_ulong,
    pub(crate) data: Option<String>,
}

#[derive(Clone)]
pub(crate) struct MountExecutor {
    mount: Arc<MountFn>,
    unmount: Arc<UnmountFn>,
}

impl MountExecutor {
    pub(crate) fn system() -> Self {
        Self {
            mount: Arc::new(system_mount),
            unmount: Arc::new(system_unmount),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_hooks(
        mount: impl Fn(&MountCall) -> io::Result<()> + Send + Sync + 'static,
        unmount: impl Fn(&Path, c_int) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            mount: Arc::new(mount),
            unmount: Arc::new(unmount),
        }
    }

    pub(crate) fn apply(
        &self,
        spec: &MountSpec,
        mounts: &mut MountStack,
    ) -> Result<(), SandboxError> {
        let call = spec.to_mount_call();
        (self.mount)(&call).map_err(|error| mount_error(spec, error))?;
        if !matches!(spec, MountSpec::RemountReadonly { .. }) {
            mounts.push(MountGuard::active_with_unmount(
                spec.target().to_path_buf(),
                self.unmount.clone(),
            ));
        }
        Ok(())
    }
}

impl Default for MountExecutor {
    fn default() -> Self {
        Self::system()
    }
}

pub(crate) struct MountGuard {
    target: PathBuf,
    active: bool,
    unmount: Arc<UnmountFn>,
}

impl MountGuard {
    pub(crate) fn active(target: impl Into<PathBuf>) -> Self {
        Self::active_with_unmount(target, Arc::new(system_unmount))
    }

    pub(crate) fn active_with_unmount(target: impl Into<PathBuf>, unmount: Arc<UnmountFn>) -> Self {
        Self {
            target: target.into(),
            active: true,
            unmount,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_unmount(
        target: impl Into<PathBuf>,
        unmount: impl Fn(&Path, c_int) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self::active_with_unmount(target, Arc::new(unmount))
    }

    pub(crate) fn target(&self) -> &Path {
        &self.target
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    pub(crate) fn close(&mut self) -> Result<(), SandboxError> {
        if !self.active {
            return Ok(());
        }

        match (self.unmount)(&self.target, 0) {
            Ok(()) => {
                self.active = false;
                Ok(())
            }
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                (self.unmount)(&self.target, libc::MNT_DETACH)
                    .map_err(|lazy_error| cleanup_error(&self.target, "lazy_umount", lazy_error))?;
                self.active = false;
                Ok(())
            }
            Err(error) => Err(cleanup_error(&self.target, "umount", error)),
        }
    }

    pub(crate) fn finish(&mut self) -> Result<(), SandboxError> {
        self.close()
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

#[derive(Default)]
pub(crate) struct MountStack {
    guards: Vec<MountGuard>,
}

impl MountStack {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, guard: MountGuard) {
        self.guards.push(guard);
    }

    pub(crate) fn push_active(&mut self, target: impl Into<PathBuf>) {
        self.push(MountGuard::active(target));
    }

    pub(crate) fn len(&self) -> usize {
        self.guards.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.guards.is_empty()
    }

    pub(crate) fn close(&mut self) -> Result<(), SandboxError> {
        let mut first_error = None;
        let mut remaining = Vec::new();

        while let Some(mut guard) = self.guards.pop() {
            if let Err(error) = guard.close() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                if guard.is_active() {
                    remaining.push(guard);
                }
            }
        }

        remaining.reverse();
        self.guards = remaining;

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub(crate) fn finish(&mut self) -> Result<(), SandboxError> {
        self.close()
    }
}

impl Drop for MountStack {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn system_mount(call: &MountCall) -> io::Result<()> {
    let source = optional_cstring(call.source.as_deref())?;
    let target = path_cstring(&call.target)?;
    let fstype = optional_cstring(call.fstype.as_deref())?;
    let data = optional_cstring(call.data.as_deref())?;

    let source_ptr = source
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    let fstype_ptr = fstype
        .as_ref()
        .map_or(std::ptr::null(), |value| value.as_ptr());
    let data_ptr = data.as_ref().map_or(std::ptr::null(), |value| {
        value.as_ptr().cast::<libc::c_void>()
    });

    let rc = unsafe {
        libc::mount(
            source_ptr,
            target.as_ptr(),
            fstype_ptr,
            call.flags,
            data_ptr,
        )
    };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn system_unmount(target: &Path, flags: c_int) -> io::Result<()> {
    let raw_target = path_cstring(target)?;
    let rc = unsafe { libc::umount2(raw_target.as_ptr(), flags) };

    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn optional_cstring(value: Option<&str>) -> io::Result<Option<CString>> {
    value
        .map(|value| {
            CString::new(value).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "mount value contains nul byte")
            })
        })
        .transpose()
}

fn path_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "mount target contains nul byte",
        )
    })
}

fn mount_error(spec: &MountSpec, error: io::Error) -> SandboxError {
    SandboxError::with_details(
        spec.reason(),
        format!("mount {}: {error}", spec.target().display()),
    )
}

fn cleanup_error(target: &Path, action: &str, error: io::Error) -> SandboxError {
    SandboxError::with_details(
        SandboxReason::SandboxCleanupFailed,
        format!("{action} {}: {error}", target.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::os::raw::c_int;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use super::{MountCall, MountExecutor, MountGuard, MountSpec, MountStack};
    use crate::sandbox::error::SandboxReason;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct UnmountEvent {
        target: String,
        flags: c_int,
    }

    fn recorder(
        events: Arc<Mutex<Vec<UnmountEvent>>>,
        outcomes: Arc<Mutex<VecDeque<io::Result<()>>>>,
    ) -> impl Fn(&Path, c_int) -> io::Result<()> + Send + Sync + 'static {
        move |target, flags| {
            events.lock().unwrap().push(UnmountEvent {
                target: target.display().to_string(),
                flags,
            });
            outcomes.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }
    }

    fn ok_recorder(
        events: Arc<Mutex<Vec<UnmountEvent>>>,
    ) -> impl Fn(&Path, c_int) -> io::Result<()> + Send + Sync + 'static {
        recorder(events, Arc::new(Mutex::new(VecDeque::new())))
    }

    #[test]
    fn mount_spec_builds_overlay_syscall_shape() {
        let spec = MountSpec::overlay("/repo", "/tmp/upper", "/tmp/work", "/tmp/merged");

        let call = spec.to_mount_call();

        assert_eq!(call.source.as_deref(), Some("overlay"));
        assert_eq!(call.fstype.as_deref(), Some("overlay"));
        assert_eq!(call.target, Path::new("/tmp/merged"));
        assert_eq!(
            call.data.as_deref(),
            Some("lowerdir=/repo,upperdir=/tmp/upper,workdir=/tmp/work")
        );
    }

    #[test]
    fn mount_spec_builds_bind_and_remount_syscall_shapes() {
        let bind = MountSpec::bind("/", "/tmp/merged", false).to_mount_call();
        assert_eq!(bind.source.as_deref(), Some("/"));
        assert_eq!(bind.flags, libc::MS_BIND);

        let remount = MountSpec::remount_readonly("/tmp/merged").to_mount_call();
        assert_eq!(remount.source, None);
        assert_eq!(
            remount.flags,
            libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY
        );
    }

    #[test]
    fn mount_executor_pushes_guard_after_successful_mount() {
        let calls = Arc::new(Mutex::new(Vec::<MountCall>::new()));
        let unmounts = Arc::new(Mutex::new(Vec::<String>::new()));
        let executor = MountExecutor::with_hooks(
            {
                let calls = calls.clone();
                move |call| {
                    calls.lock().unwrap().push(call.clone());
                    Ok(())
                }
            },
            {
                let unmounts = unmounts.clone();
                move |target, _flags| {
                    unmounts.lock().unwrap().push(target.display().to_string());
                    Ok(())
                }
            },
        );
        let mut stack = MountStack::new();

        executor
            .apply(&MountSpec::bind("/src", "/dst", false), &mut stack)
            .unwrap();

        assert_eq!(stack.len(), 1);
        assert_eq!(calls.lock().unwrap().len(), 1);
        stack.close().unwrap();
        assert_eq!(unmounts.lock().unwrap().as_slice(), ["/dst"]);
    }

    #[test]
    fn mount_executor_does_not_push_guard_for_remount_readonly() {
        let calls = Arc::new(Mutex::new(Vec::<MountCall>::new()));
        let unmounts = Arc::new(Mutex::new(Vec::<String>::new()));
        let executor = MountExecutor::with_hooks(
            {
                let calls = calls.clone();
                move |call| {
                    calls.lock().unwrap().push(call.clone());
                    Ok(())
                }
            },
            {
                let unmounts = unmounts.clone();
                move |target, _flags| {
                    unmounts.lock().unwrap().push(target.display().to_string());
                    Ok(())
                }
            },
        );
        let mut stack = MountStack::new();

        executor
            .apply(&MountSpec::remount_readonly("/dst"), &mut stack)
            .unwrap();

        assert_eq!(calls.lock().unwrap().len(), 1);
        assert_eq!(stack.len(), 0);
        stack.close().unwrap();
        assert!(unmounts.lock().unwrap().is_empty());
    }

    #[test]
    fn mount_executor_maps_mount_failures_to_spec_reason() {
        let executor = MountExecutor::with_hooks(
            |_call| Err(io::Error::new(io::ErrorKind::PermissionDenied, "no mount")),
            |_target, _flags| Ok(()),
        );
        let mut stack = MountStack::new();

        let error = executor
            .apply(
                &MountSpec::overlay("/repo", "/upper", "/work", "/merged"),
                &mut stack,
            )
            .unwrap_err();

        assert_eq!(error.reason(), SandboxReason::OverlayMountFailed);
        assert_eq!(stack.len(), 0);
    }

    #[test]
    fn mount_guard_close_is_idempotent() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut guard = MountGuard::with_test_unmount("/tmp/aish-one", ok_recorder(events.clone()));

        guard.close().unwrap();
        guard.close().unwrap();
        drop(guard);

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target, "/tmp/aish-one");
        assert_eq!(events[0].flags, 0);
    }

    #[test]
    fn mount_guard_uses_lazy_unmount_when_target_is_busy() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let outcomes = Arc::new(Mutex::new(VecDeque::from([
            Err(io::Error::from_raw_os_error(libc::EBUSY)),
            Ok(()),
        ])));
        let mut guard =
            MountGuard::with_test_unmount("/tmp/aish-busy", recorder(events.clone(), outcomes));

        guard.close().unwrap();

        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].flags, 0);
        assert_eq!(events[1].flags, libc::MNT_DETACH);
    }

    #[test]
    fn mount_stack_closes_deep_to_shallow() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut stack = MountStack::new();
        stack.push(MountGuard::with_test_unmount(
            "/tmp/aish-root",
            ok_recorder(events.clone()),
        ));
        stack.push(MountGuard::with_test_unmount(
            "/tmp/aish-root/deep",
            ok_recorder(events.clone()),
        ));

        stack.close().unwrap();

        let targets: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.target.clone())
            .collect();
        assert_eq!(targets, vec!["/tmp/aish-root/deep", "/tmp/aish-root"]);
        assert!(stack.is_empty());
    }

    #[test]
    fn mount_stack_attempts_remaining_cleanup_after_failure() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let failing_outcomes =
            Arc::new(Mutex::new(VecDeque::from([Err(io::Error::other("boom"))])));
        let mut stack = MountStack::new();
        stack.push(MountGuard::with_test_unmount(
            "/tmp/aish-first",
            ok_recorder(events.clone()),
        ));
        stack.push(MountGuard::with_test_unmount(
            "/tmp/aish-bad",
            recorder(events.clone(), failing_outcomes),
        ));
        stack.push(MountGuard::with_test_unmount(
            "/tmp/aish-last",
            ok_recorder(events.clone()),
        ));

        let error = stack.close().unwrap_err();

        assert_eq!(error.reason(), SandboxReason::SandboxCleanupFailed);
        let targets: Vec<_> = events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event.target.clone())
            .collect();
        assert_eq!(
            targets,
            vec!["/tmp/aish-last", "/tmp/aish-bad", "/tmp/aish-first"]
        );
        assert_eq!(stack.len(), 1);
    }
}
