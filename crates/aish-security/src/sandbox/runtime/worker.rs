use std::ffi::CStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::sandbox::error::{SandboxError, SandboxReason};
use crate::sandbox::runtime::collect::collect_changes;
use crate::sandbox::runtime::executor::SandboxRunGuard;
use crate::sandbox::runtime::mount::MountExecutor;
use crate::sandbox::runtime::overlay::{
    read_host_mount_points_under, setup_overlay_plan, OverlayPlan, OverlayPlanBuilder,
    OverlayStrategy,
};
use crate::sandbox::types::{PayloadIdentity, SandboxDeadline, SandboxResult, SandboxRunContext};
use crate::sudo::strip_sudo_prefix;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const BASH_PATH: &str = "/usr/bin/bash";
const SETPRIV_PATH: &str = "/usr/bin/setpriv";
const SINGLE_REPO_RUNTIME_BIND_ROOTS: [&str; 5] = ["/usr", "/bin", "/lib", "/lib64", "/etc"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PayloadCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SandboxWorkerSuccessResponse {
    ok: bool,
    result: SandboxResult,
}

#[derive(Debug, Serialize)]
struct SandboxWorkerFailureResponse {
    ok: bool,
    reason: SandboxReason,
    error: String,
}

#[derive(Debug, Deserialize)]
struct RawSandboxWorkerResponse {
    ok: Option<bool>,
    result: Option<SandboxResult>,
    reason: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
struct TruncatedOutput {
    text: String,
    truncated: bool,
}

#[derive(Debug)]
struct ProcessOutput {
    exit_status: ExitStatus,
    stdout: TruncatedOutput,
    stderr: TruncatedOutput,
}

#[derive(Debug)]
struct ResolvedPayloadExecution {
    command: String,
    payload_identity: PayloadIdentity,
}

pub(crate) trait WorkerRuntime {
    fn prepare_mount_namespace(&self) -> Result<(), SandboxError>;

    fn host_submounts(&self, repo_root: &Path) -> Vec<PathBuf>;

    fn mount_executor(&self) -> MountExecutor;

    fn spawn_payload(&self, command: &PayloadCommand) -> Result<Child, SandboxError>;
}

#[derive(Default, Clone)]
pub(crate) struct SystemWorkerRuntime;

impl WorkerRuntime for SystemWorkerRuntime {
    fn prepare_mount_namespace(&self) -> Result<(), SandboxError> {
        let unshare_rc = unsafe { libc::unshare(libc::CLONE_NEWNS) };
        if unshare_rc != 0 {
            return Err(SandboxError::with_details(
                SandboxReason::SandboxUnavailable,
                std::io::Error::last_os_error().to_string(),
            ));
        }

        let root = std::ffi::CString::new("/").expect("static path");
        let remount_rc = unsafe {
            libc::mount(
                std::ptr::null(),
                root.as_ptr(),
                std::ptr::null(),
                (libc::MS_REC | libc::MS_PRIVATE) as libc::c_ulong,
                std::ptr::null(),
            )
        };
        if remount_rc != 0 {
            return Err(SandboxError::with_details(
                SandboxReason::SandboxUnavailable,
                std::io::Error::last_os_error().to_string(),
            ));
        }

        Ok(())
    }

    fn host_submounts(&self, repo_root: &Path) -> Vec<PathBuf> {
        read_host_mount_points_under(repo_root)
    }

    fn mount_executor(&self) -> MountExecutor {
        MountExecutor::system()
    }

    fn spawn_payload(&self, command: &PayloadCommand) -> Result<Child, SandboxError> {
        let mut child = std::process::Command::new(&command.program);
        child
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        child.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SandboxError::with_details(SandboxReason::CommandNotFound, command.program.clone())
            } else {
                SandboxError::with_details(SandboxReason::SandboxExecuteFailed, error.to_string())
            }
        })
    }
}

pub(crate) fn execute_worker_context(
    context: &SandboxRunContext,
) -> Result<SandboxResult, SandboxError> {
    execute_worker_context_with_runtime(context, &SystemWorkerRuntime)
}

pub(crate) fn execute_worker_context_with_runtime(
    context: &SandboxRunContext,
    runtime: &impl WorkerRuntime,
) -> Result<SandboxResult, SandboxError> {
    let resolved = resolve_payload_execution(context)?;
    let temp_root = create_temp_root()?;
    let deadline = SandboxDeadline::from_timeout_s(context.request.timeout_s, context.limits);
    let host_submounts = runtime.host_submounts(&context.request.repo_root);
    let plan =
        OverlayPlanBuilder::new(&context.request.repo_root, &context.request.cwd, &temp_root)
            .with_host_submounts(host_submounts)
            .build()?;
    let payload = build_payload_command(&plan, &resolved.command, resolved.payload_identity);

    let mut run_guard = SandboxRunGuard::new();
    runtime.prepare_mount_namespace()?;
    setup_overlay_plan(&plan, &runtime.mount_executor(), &mut run_guard)?;

    let child = runtime.spawn_payload(&payload)?;
    let output = wait_for_child_output(
        child,
        deadline,
        context.limits.stdout_bytes,
        context.limits.stderr_bytes,
    )
    .map_err(|error| {
        let _ = run_guard.close();
        error
    })?;

    let collected = collect_changes(&plan, context.limits).map_err(|error| {
        let _ = run_guard.close();
        error
    })?;

    run_guard.finish()?;

    Ok(SandboxResult {
        exit_code: output.exit_status.code().unwrap_or(-1),
        stdout: output.stdout.text,
        stderr: output.stderr.text,
        changes: collected.changes,
        stdout_truncated: output.stdout.truncated,
        stderr_truncated: output.stderr.truncated,
        changes_truncated: collected.truncated,
    })
}

pub(crate) fn spawn_worker_process(
    worker_program: &Path,
    context: &SandboxRunContext,
) -> Result<SandboxResult, SandboxError> {
    let mut child = std::process::Command::new(worker_program)
        .arg("--sandbox-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SandboxError::with_details(
                    SandboxReason::CommandNotFound,
                    worker_program.display().to_string(),
                )
            } else {
                SandboxError::with_details(SandboxReason::SandboxExecuteFailed, error.to_string())
            }
        })?;

    let mut stdin = child.stdin.take().ok_or_else(|| {
        SandboxError::with_details(SandboxReason::SandboxExecuteFailed, "missing_worker_stdin")
    })?;
    serde_json::to_writer(&mut stdin, context).map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxExecuteFailed, error.to_string())
    })?;
    stdin.write_all(b"\n").map_err(|error| {
        SandboxError::with_details(SandboxReason::SandboxExecuteFailed, error.to_string())
    })?;
    drop(stdin);

    let deadline = SandboxDeadline::from_timeout_s(context.request.timeout_s, context.limits);
    let output = wait_for_child_output(
        child,
        deadline,
        context.limits.response_bytes,
        context.limits.stderr_bytes,
    )?;

    decode_worker_response(&output.stdout.text)
}

pub(crate) fn run_worker_from_reader_writer(
    reader: &mut impl Read,
    writer: &mut impl Write,
) -> std::io::Result<()> {
    let response = match decode_worker_request(reader) {
        Ok(context) => match execute_worker_context(&context) {
            Ok(result) => serde_json::to_vec(&SandboxWorkerSuccessResponse { ok: true, result }),
            Err(error) => serde_json::to_vec(&SandboxWorkerFailureResponse {
                ok: false,
                reason: error.reason(),
                error: error
                    .details()
                    .unwrap_or_else(|| error.reason().as_str())
                    .to_string(),
            }),
        },
        Err(error) => serde_json::to_vec(&SandboxWorkerFailureResponse {
            ok: false,
            reason: error.reason(),
            error: error
                .details()
                .unwrap_or_else(|| error.reason().as_str())
                .to_string(),
        }),
    }
    .map_err(std::io::Error::other)?;

    writer.write_all(&response)?;
    writer.write_all(b"\n")?;
    Ok(())
}

pub(crate) fn run_sandbox_worker_stdio() -> std::io::Result<()> {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    run_worker_from_reader_writer(&mut stdin, &mut stdout)
}

pub(crate) struct WorkerHandle {
    child: Option<Child>,
}

impl WorkerHandle {
    pub(crate) fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    pub(crate) fn poll_status(&mut self) -> Result<Option<ExitStatus>, SandboxError> {
        let Some(child) = &mut self.child else {
            return Ok(None);
        };

        let status = child.try_wait().map_err(worker_error)?;
        if status.is_some() {
            self.child = None;
        }
        Ok(status)
    }

    pub(crate) fn wait(&mut self) -> Result<Option<ExitStatus>, SandboxError> {
        let Some(child) = &mut self.child else {
            return Ok(None);
        };

        let status = child.wait().map_err(worker_error)?;
        self.child = None;
        Ok(Some(status))
    }

    pub(crate) fn kill_and_wait(&mut self) -> Result<Option<ExitStatus>, SandboxError> {
        let Some(child) = &mut self.child else {
            return Ok(None);
        };

        if let Some(status) = child.try_wait().map_err(worker_error)? {
            self.child = None;
            return Ok(Some(status));
        }

        child.kill().map_err(worker_error)?;
        let status = child.wait().map_err(worker_error)?;
        self.child = None;
        Ok(Some(status))
    }

    pub(crate) fn close(&mut self) -> Result<Option<ExitStatus>, SandboxError> {
        self.kill_and_wait()
    }

    pub(crate) fn is_active(&self) -> bool {
        self.child.is_some()
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn decode_worker_request(reader: &mut impl Read) -> Result<SandboxRunContext, SandboxError> {
    let mut raw = String::new();
    reader.read_to_string(&mut raw).map_err(|error| {
        SandboxError::with_details(SandboxReason::BadRequest, error.to_string())
    })?;

    if raw.trim().is_empty() {
        return Err(SandboxError::with_details(
            SandboxReason::BadRequest,
            "empty_stdin",
        ));
    }

    serde_json::from_str::<SandboxRunContext>(&raw)
        .map_err(|_| SandboxError::with_details(SandboxReason::BadRequest, "invalid_json"))
}

fn decode_worker_response(raw: &str) -> Result<SandboxResult, SandboxError> {
    let response: RawSandboxWorkerResponse = serde_json::from_str(raw).map_err(|_| {
        SandboxError::with_details(SandboxReason::SandboxFailed, "invalid_worker_json")
    })?;

    match response.ok {
        Some(true) => response.result.ok_or_else(|| {
            SandboxError::with_details(SandboxReason::SandboxFailed, "missing_worker_result")
        }),
        Some(false) => {
            let raw_reason = response
                .reason
                .unwrap_or_else(|| "sandbox_failed".to_string());
            let reason =
                SandboxReason::from_wire(&raw_reason).unwrap_or(SandboxReason::SandboxFailed);
            Err(SandboxError::with_details(
                reason,
                response.error.unwrap_or(raw_reason),
            ))
        }
        None => Err(SandboxError::with_details(
            SandboxReason::SandboxFailed,
            "missing_worker_ok",
        )),
    }
}

fn resolve_payload_execution(
    context: &SandboxRunContext,
) -> Result<ResolvedPayloadExecution, SandboxError> {
    let (stripped, sudo_detected, ok) = strip_sudo_prefix(&context.request.command);
    if sudo_detected && !ok {
        return Err(SandboxError::with_details(
            SandboxReason::SandboxExecuteFailed,
            "sudo_without_command",
        ));
    }

    let payload_identity = match context.payload_identity {
        Some(payload_identity) => payload_identity,
        None => {
            let request_identity = context.request_identity.ok_or_else(|| {
                SandboxError::with_details(
                    SandboxReason::SandboxExecuteFailed,
                    "missing_request_identity",
                )
            })?;
            PayloadIdentity::from_request_identity(request_identity, sudo_detected)
        }
    };

    Ok(ResolvedPayloadExecution {
        command: if sudo_detected {
            stripped
        } else {
            context.request.command.clone()
        },
        payload_identity,
    })
}

fn build_payload_command(
    plan: &OverlayPlan,
    command: &str,
    payload_identity: PayloadIdentity,
) -> PayloadCommand {
    let mut args = vec![
        "--bind".to_string(),
        plan.merged_root.display().to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--chdir".to_string(),
        plan.sandbox_cwd.display().to_string(),
    ];

    append_runtime_bind_mounts(plan, &mut args);
    append_payload_home(&mut args, payload_identity);

    match payload_identity {
        PayloadIdentity::User { uid, gid } => {
            args.extend([
                SETPRIV_PATH.to_string(),
                "--reuid".to_string(),
                uid.to_string(),
                "--regid".to_string(),
                gid.to_string(),
                "--clear-groups".to_string(),
                "--inh-caps=-all".to_string(),
                BASH_PATH.to_string(),
                "-lc".to_string(),
                command.to_string(),
            ]);
        }
        PayloadIdentity::Root => {
            args.extend([
                BASH_PATH.to_string(),
                "-lc".to_string(),
                command.to_string(),
            ]);
        }
    }

    PayloadCommand {
        program: "bwrap".to_string(),
        args,
    }
}

fn append_payload_home(args: &mut Vec<String>, payload_identity: PayloadIdentity) {
    args.extend([
        "--setenv".to_string(),
        "HOME".to_string(),
        payload_home(payload_identity),
    ]);
}

fn payload_home(payload_identity: PayloadIdentity) -> String {
    match payload_identity {
        PayloadIdentity::Root => "/root".to_string(),
        PayloadIdentity::User { uid, .. } => {
            resolve_user_home(uid).unwrap_or_else(|| "/tmp".to_string())
        }
    }
}

fn resolve_user_home(uid: u32) -> Option<String> {
    let mut pwd = libc::passwd {
        pw_name: std::ptr::null_mut(),
        pw_passwd: std::ptr::null_mut(),
        pw_uid: 0,
        pw_gid: 0,
        pw_gecos: std::ptr::null_mut(),
        pw_dir: std::ptr::null_mut(),
        pw_shell: std::ptr::null_mut(),
    };
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 4096];

    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buffer.as_mut_ptr() as *mut libc::c_char,
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }

    Some(
        unsafe { CStr::from_ptr(pwd.pw_dir) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn append_runtime_bind_mounts(plan: &OverlayPlan, args: &mut Vec<String>) {
    if plan.strategy != OverlayStrategy::SingleRepo {
        return;
    }

    for root in SINGLE_REPO_RUNTIME_BIND_ROOTS {
        if !Path::new(root).exists() {
            continue;
        }

        args.extend(["--ro-bind".to_string(), root.to_string(), root.to_string()]);
    }
}

fn create_temp_root() -> Result<PathBuf, SandboxError> {
    let base = std::env::temp_dir();
    let pid = std::process::id();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..32_u32 {
        let path = base.join(format!("aish-sandbox-{pid}-{timestamp}-{attempt}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(SandboxError::with_details(
                    SandboxReason::OverlayPermFailed,
                    error.to_string(),
                ))
            }
        }
    }

    Err(SandboxError::with_details(
        SandboxReason::OverlayPermFailed,
        "failed_to_create_temp_root",
    ))
}

fn wait_for_child_output(
    mut child: Child,
    deadline: SandboxDeadline,
    stdout_limit: usize,
    stderr_limit: usize,
) -> Result<ProcessOutput, SandboxError> {
    let stdout_reader = spawn_output_reader(child.stdout.take(), stdout_limit);
    let stderr_reader = spawn_output_reader(child.stderr.take(), stderr_limit);
    let mut handle = WorkerHandle::new(child);

    let exit_status = loop {
        if let Some(status) = handle.poll_status()? {
            break status;
        }
        if deadline.is_expired() {
            let _ = handle.close();
            let _ = join_output_reader(stdout_reader);
            let _ = join_output_reader(stderr_reader);
            return Err(SandboxError::from_reason(SandboxReason::SandboxTimeout));
        }
        thread::sleep(PROCESS_POLL_INTERVAL);
    };

    Ok(ProcessOutput {
        exit_status,
        stdout: join_output_reader(stdout_reader)?,
        stderr: join_output_reader(stderr_reader)?,
    })
}

fn spawn_output_reader(
    reader: Option<impl Read + Send + 'static>,
    limit: usize,
) -> JoinHandle<Result<TruncatedOutput, SandboxError>> {
    thread::spawn(move || {
        let Some(mut reader) = reader else {
            return Ok(TruncatedOutput {
                text: String::new(),
                truncated: false,
            });
        };

        let mut collected = Vec::new();
        let mut truncated = false;
        let mut chunk = [0_u8; 8192];
        loop {
            let read = reader.read(&mut chunk).map_err(|error| {
                SandboxError::with_details(SandboxReason::SandboxExecuteFailed, error.to_string())
            })?;
            if read == 0 {
                break;
            }

            if collected.len() < limit {
                let remaining = limit - collected.len();
                let take = read.min(remaining);
                collected.extend_from_slice(&chunk[..take]);
                if take < read {
                    truncated = true;
                }
            } else {
                truncated = true;
            }
        }

        Ok(TruncatedOutput {
            text: String::from_utf8_lossy(&collected).into_owned(),
            truncated,
        })
    })
}

fn join_output_reader(
    handle: JoinHandle<Result<TruncatedOutput, SandboxError>>,
) -> Result<TruncatedOutput, SandboxError> {
    handle.join().map_err(|_| {
        SandboxError::with_details(
            SandboxReason::SandboxExecuteFailed,
            "reader_thread_panicked",
        )
    })?
}

fn worker_error(error: std::io::Error) -> SandboxError {
    SandboxError::with_details(SandboxReason::SandboxCleanupFailed, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex};

    use tempfile::tempdir;

    use super::{
        build_payload_command, execute_worker_context_with_runtime, run_worker_from_reader_writer,
        spawn_worker_process, PayloadCommand, WorkerHandle, WorkerRuntime, BASH_PATH, SETPRIV_PATH,
    };
    use crate::sandbox::error::SandboxReason;
    use crate::sandbox::runtime::mount::{MountCall, MountExecutor};
    use crate::sandbox::types::{
        PayloadIdentity, RequestIdentity, SandboxLimits, SandboxRunContext, SandboxRunRequest,
    };

    #[derive(Clone)]
    struct FakeRuntime {
        seen_commands: Arc<Mutex<Vec<PayloadCommand>>>,
        script: &'static str,
    }

    impl WorkerRuntime for FakeRuntime {
        fn prepare_mount_namespace(&self) -> Result<(), crate::sandbox::error::SandboxError> {
            Ok(())
        }

        fn host_submounts(&self, _repo_root: &Path) -> Vec<PathBuf> {
            Vec::new()
        }

        fn mount_executor(&self) -> MountExecutor {
            MountExecutor::with_hooks(|_call: &MountCall| Ok(()), |_target, _flags| Ok(()))
        }

        fn spawn_payload(
            &self,
            command: &PayloadCommand,
        ) -> Result<Child, crate::sandbox::error::SandboxError> {
            self.seen_commands.lock().unwrap().push(command.clone());
            Command::new("sh")
                .arg("-c")
                .arg(self.script)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| {
                    crate::sandbox::error::SandboxError::with_details(
                        SandboxReason::SandboxExecuteFailed,
                        error.to_string(),
                    )
                })
        }
    }

    fn sample_context(command: &str) -> SandboxRunContext {
        SandboxRunContext {
            request: SandboxRunRequest {
                id: "req-1".to_string(),
                command: command.to_string(),
                cwd: PathBuf::from("/repo"),
                repo_root: PathBuf::from("/repo"),
                client_pid: 42,
                timeout_s: 1.0,
            },
            limits: SandboxLimits::default(),
            socket_path: None,
            request_identity: Some(RequestIdentity::from_peer_credentials(42, 1000, 1001)),
            payload_identity: None,
        }
    }

    #[test]
    fn worker_handle_wait_collects_exit_status_and_disarms_drop() {
        let child = Command::new("sh").arg("-c").arg("exit 7").spawn().unwrap();
        let mut handle = WorkerHandle::new(child);

        let status = handle.wait().unwrap().unwrap();

        assert_eq!(status.code(), Some(7));
        assert!(!handle.is_active());
        assert!(handle.close().unwrap().is_none());
    }

    #[test]
    fn worker_handle_close_kills_running_worker_and_is_idempotent() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let mut handle = WorkerHandle::new(child);
        let child_id = handle.id();

        let status = handle.close().unwrap().unwrap();

        assert!(child_id.is_some());
        assert!(!status.success());
        assert!(!handle.is_active());
        assert!(handle.close().unwrap().is_none());
    }

    #[test]
    fn build_payload_command_uses_setpriv_for_user_payload() {
        let temp = tempdir().unwrap();
        let plan = crate::sandbox::runtime::overlay::OverlayPlanBuilder::new(
            "/repo",
            "/repo",
            temp.path(),
        )
        .build()
        .unwrap();

        let command = build_payload_command(
            &plan,
            "echo hi",
            PayloadIdentity::User {
                uid: 1000,
                gid: 1001,
            },
        );

        assert_eq!(command.program, "bwrap");
        assert!(command.args.iter().any(|arg| arg == SETPRIV_PATH));
        assert!(command.args.iter().any(|arg| arg == "--reuid"));
        assert!(command.args.windows(3).any(|window| {
            window[0] == "--setenv" && window[1] == "HOME" && window[2] != "/root"
        }));
        assert!(command.args.windows(3).any(|window| {
            window[0] == "--ro-bind" && window[1] == "/usr" && window[2] == "/usr"
        }));
        assert_eq!(command.args.last().map(String::as_str), Some("echo hi"));
    }

    #[test]
    fn build_payload_command_omits_setpriv_for_root_payload() {
        let temp = tempdir().unwrap();
        let plan = crate::sandbox::runtime::overlay::OverlayPlanBuilder::new(
            "/repo",
            "/repo",
            temp.path(),
        )
        .build()
        .unwrap();

        let command = build_payload_command(&plan, "echo hi", PayloadIdentity::Root);

        assert_eq!(command.program, "bwrap");
        assert!(!command.args.iter().any(|arg| arg == SETPRIV_PATH));
        assert!(command.args.iter().any(|arg| arg == BASH_PATH));
        assert!(command.args.windows(3).any(|window| {
            window[0] == "--setenv" && window[1] == "HOME" && window[2] == "/root"
        }));
        assert_eq!(command.args.last().map(String::as_str), Some("echo hi"));
    }

    #[test]
    fn run_worker_from_reader_writer_returns_bad_request_for_invalid_json() {
        let mut input = "{bad json".as_bytes();
        let mut output = Vec::new();

        run_worker_from_reader_writer(&mut input, &mut output).unwrap();

        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\"ok\":false"));
        assert!(text.contains("\"reason\":\"bad_request\""));
        assert!(text.contains("invalid_json"));
    }

    #[test]
    fn execute_worker_context_uses_root_identity_for_sudo_and_truncates_output() {
        let seen_commands = Arc::new(Mutex::new(Vec::new()));
        let runtime = FakeRuntime {
            seen_commands: seen_commands.clone(),
            script: "printf 'abcdef'; printf 'xyz' >&2",
        };
        let mut context = sample_context("sudo echo hi");
        context.limits.stdout_bytes = 4;
        context.limits.stderr_bytes = 2;

        let result = execute_worker_context_with_runtime(&context, &runtime).unwrap();

        assert_eq!(result.stdout, "abcd");
        assert_eq!(result.stderr, "xy");
        assert!(result.stdout_truncated);
        assert!(result.stderr_truncated);
        let command = seen_commands.lock().unwrap().pop().unwrap();
        assert!(!command.args.iter().any(|arg| arg == "setpriv"));
        assert_eq!(command.args.last().map(String::as_str), Some("echo hi"));
    }

    #[test]
    fn execute_worker_context_times_out_running_payload() {
        let seen_commands = Arc::new(Mutex::new(Vec::new()));
        let runtime = FakeRuntime {
            seen_commands,
            script: "sleep 1",
        };
        let mut context = sample_context("echo hi");
        context.request.timeout_s = 0.05;
        context.limits.timeout_min_s = 0;
        context.limits.timeout_max_s = 1;

        let error = execute_worker_context_with_runtime(&context, &runtime).unwrap_err();

        assert_eq!(error.reason(), SandboxReason::SandboxTimeout);
    }

    #[test]
    fn execute_worker_context_rejects_sudo_without_command() {
        let seen_commands = Arc::new(Mutex::new(Vec::new()));
        let runtime = FakeRuntime {
            seen_commands,
            script: "printf ok",
        };

        let error = execute_worker_context_with_runtime(&sample_context("sudo -u root"), &runtime)
            .unwrap_err();

        assert_eq!(error.reason(), SandboxReason::SandboxExecuteFailed);
        assert_eq!(error.details(), Some("sudo_without_command"));
    }

    #[test]
    fn spawn_worker_process_round_trips_with_fake_program() {
        let temp = tempdir().unwrap();
        let script_path = temp.path().join("fake-worker.sh");
        fs::write(
            &script_path,
            "#!/bin/sh\ncat >/dev/null\nprintf '{\"ok\":true,\"result\":{\"exit_code\":0,\"stdout\":\"worker\",\"stderr\":\"\",\"changes\":[]}}\n'\n",
        )
        .unwrap();
        let mut perms = fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).unwrap();

        let result = spawn_worker_process(&script_path, &sample_context("echo hi")).unwrap();

        assert_eq!(result.exit_code, 0);
        assert_eq!(result.stdout, "worker");
    }
}
