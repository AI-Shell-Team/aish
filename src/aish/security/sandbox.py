"""
Sandbox core: data types, executor and high-level wrapper.
"""

from __future__ import annotations

import os
import re
import stat
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any, List, Optional, Tuple

from .sandbox_types import FsChange, SandboxResult, SandboxSecurityResult

_MOUNTINFO_ESC_RE = re.compile(r"\\([0-7]{3})")

# 统一的 IPC socket 路径常量（供 daemon 与客户端引用）
DEFAULT_SANDBOX_SOCKET_PATH = Path("/run/aish/sandbox.sock")


def strip_sudo_prefix(command: str) -> tuple[str, bool, bool]:
    """Strip leading sudo prefix and flags.

    Returns (stripped_command, sudo_detected, ok).
    """

    raw = command or ""
    raw_l = raw.lstrip()
    if not raw_l.startswith("sudo ") and raw_l != "sudo":
        return command, False, True

    def _is_space(ch: str) -> bool:
        return ch.isspace()

    def _skip_ws(s: str, idx: int) -> int:
        while idx < len(s) and _is_space(s[idx]):
            idx += 1
        return idx

    def _read_token(s: str, idx: int) -> tuple[str, int]:
        """Read one shell-like token starting at idx.

        This is a minimal POSIX-ish tokenizer that understands quotes and
        backslash escapes well enough for parsing sudo options.
        """

        idx = _skip_ws(s, idx)
        if idx >= len(s):
            return "", idx

        out: list[str] = []
        in_squote = False
        in_dquote = False
        while idx < len(s):
            ch = s[idx]
            if not in_squote and not in_dquote and _is_space(ch):
                break
            if ch == "'" and not in_dquote:
                in_squote = not in_squote
                idx += 1
                continue
            if ch == '"' and not in_squote:
                in_dquote = not in_dquote
                idx += 1
                continue
            if ch == "\\" and not in_squote:
                # In double quotes and unquoted text, backslash escapes next char.
                if idx + 1 < len(s):
                    out.append(s[idx + 1])
                    idx += 2
                    continue
            out.append(ch)
            idx += 1

        return "".join(out), idx

    # Parse using raw_l to preserve original shell operators (&&, ||, |, ;, ...)
    # and quoting in the stripped command tail.
    idx = 0
    token, idx2 = _read_token(raw_l, idx)
    if token != "sudo":
        return command, False, True
    idx = idx2

    options_with_value = {"-u", "--user", "-g", "--group", "-h", "-p", "--prompt"}
    while True:
        idx = _skip_ws(raw_l, idx)
        if idx >= len(raw_l):
            return "", True, False

        opt, opt_end = _read_token(raw_l, idx)
        if opt == "":
            return "", True, False

        if opt == "--":
            idx = opt_end
            break

        if opt.startswith("-"):
            # Options with attached values: -uuser, -ggroup, --user=user, --group=group
            if opt in options_with_value:
                idx = opt_end
                _val, idx = _read_token(raw_l, idx)
                continue
            if opt.startswith("-u") and opt != "-u":
                idx = opt_end
                continue
            if opt.startswith("-g") and opt != "-g":
                idx = opt_end
                continue
            if (
                opt.startswith("--user=")
                or opt.startswith("--group=")
                or opt.startswith("--prompt=")
            ):
                idx = opt_end
                continue

            idx = opt_end
            continue

        # First non-option token is the command.
        break

    stripped = raw_l[idx:].lstrip()
    if not stripped:
        return "", True, False
    return stripped, True, True


def _unescape_mountinfo_path(value: str) -> str:
    # mountinfo encodes special chars using octal escapes like "\040" for space.
    # Ref: proc(5)
    return _MOUNTINFO_ESC_RE.sub(lambda m: chr(int(m.group(1), 8)), value)


def _read_host_mount_points_under(repo_root: Path) -> list[Path]:
    """Return host mount points under repo_root (excluding repo_root).

    This reads the *host* mount table (outside bwrap) so we can replicate mount
    topology inside the sandbox view. Order is shallow-to-deep to avoid later
    mounts hiding earlier ones.
    """

    repo_root = repo_root.resolve()

    try:
        mountinfo = Path("/proc/self/mountinfo").read_text(
            encoding="utf-8", errors="replace"
        )
    except OSError:
        return []

    mps: set[Path] = set()
    for line in mountinfo.splitlines():
        parts = line.split()
        if len(parts) < 10:
            continue
        mp_raw = parts[4]
        mp = Path(_unescape_mountinfo_path(mp_raw))
        if mp == repo_root:
            continue
        if mp.is_relative_to(repo_root):
            mps.add(mp)

    # Avoid pseudo filesystems / special mounts that bwrap provides separately.
    # Also skip anything under them (e.g. /dev/hugepages).
    skip_roots = (Path("/proc"), Path("/sys"), Path("/dev"))
    filtered: set[Path] = set()
    for p in mps:
        if any(p == root or p.is_relative_to(root) for root in skip_roots):
            continue
        filtered.add(p)
    mps = filtered

    # shallow-to-deep order prevents a later shallow mount hiding a deeper one.
    return sorted(mps, key=lambda p: (len(p.parts), str(p)))


class SandboxUnavailableError(RuntimeError):
    """Raised when sandbox cannot be constructed or executed.

    This is a controlled failure signal that callers should catch and degrade to
    "require manual confirmation" rather than terminating the process.
    """

    def __init__(self, reason: str, *, details: Optional[str] = None) -> None:
        self.reason = reason
        self.details = details
        msg = reason if details is None else f"{reason}: {details}"
        super().__init__(msg)


@dataclass
class SandboxConfig:
    """沙箱执行配置。

    Attributes:
    repo_root: 逻辑上的工程根路径，用于 FsChange 路径归一化。
    enable_overlay: 是否启用 overlayfs（默认 True）。
    readonly_binds: 只读挂载白名单列表，每项为 (host_path, sandbox_path)。
    readwrite_binds: 可选的读写挂载白名单列表，每项为 (host_path, sandbox_path)。
    """

    repo_root: Path
    enable_overlay: bool = True
    readonly_binds: List[Tuple[Path, Path]] | None = None
    readwrite_binds: List[Tuple[Path, Path]] | None = None


def run_cmd(cmd: List[str], **kwargs: Any) -> subprocess.CompletedProcess[str]:
    """执行命令并返回 CompletedProcess。

    这里保持实现尽量简单，方便后续替换为更健壮的封装。
    """

    try:
        proc = subprocess.run(cmd, text=True, capture_output=True, **kwargs)
        return proc
    except subprocess.TimeoutExpired as exc:
        timeout_s = kwargs.get("timeout")
        details = f"timeout_s={timeout_s}" if timeout_s is not None else "timeout"
        raise SandboxUnavailableError("sandbox_timeout", details=details) from exc
    except FileNotFoundError as exc:
        tool = cmd[0] if cmd else "<unknown>"
        raise SandboxUnavailableError("command_not_found", details=tool) from exc


class SandboxExecutor:
    """在沙箱环境中模拟执行命令并收集副作用的执行器骨架。

    注意：
    - 该类目前为接口占位，真实实现应复用/迁移 sandbox_demo 中的
      bubblewrap + overlayfs 逻辑；
    - 上层风险引擎只依赖 :meth:`simulate` 的返回值。
    """

    def __init__(self, config: SandboxConfig) -> None:
        self._config = config

    def _mount_overlay(
        self, lowerdir: Path, upperdir: Path, workdir: Path, merged: Path
    ) -> None:
        """Mount an overlayfs.

        NOTE: This typically requires root or appropriate kernel settings.
        """

        options = f"lowerdir={lowerdir},upperdir={upperdir},workdir={workdir}"
        cmd = [
            "mount",
            "-t",
            "overlay",
            "overlay",
            "-o",
            options,
            str(merged),
        ]
        proc = run_cmd(cmd)
        if proc.returncode != 0:
            stderr = (proc.stderr or "").strip()
            first_line = (stderr.splitlines() or [""])[0].strip()
            raise SandboxUnavailableError(
                "overlay_mount_failed",
                details=(first_line or f"exit_code={proc.returncode}"),
            )

    def _remount_bind_readonly(self, target: Path) -> None:
        """Remount an existing bind mount as read-only."""

        cmd = ["mount", "-o", "remount,ro,bind", str(target)]
        proc = run_cmd(cmd)
        if proc.returncode != 0:
            stderr = (proc.stderr or "").strip()
            first_line = (stderr.splitlines() or [""])[0].strip()
            raise SandboxUnavailableError(
                "remount_ro_failed",
                details=(first_line or f"exit_code={proc.returncode}"),
            )

    def _list_system_root_overlay_targets(self) -> list[Path]:
        """List top-level directories under / suitable for per-dir overlay.

        We skip special pseudo filesystems that bwrap provides separately.
        """

        skip = {"proc", "sys", "dev"}
        targets: list[Path] = []
        try:
            with os.scandir("/") as it:
                for entry in it:
                    if entry.name in skip:
                        continue
                    try:
                        if not entry.is_dir(follow_symlinks=False):
                            continue
                        if entry.is_symlink():
                            continue
                    except OSError:
                        continue
                    targets.append(Path("/") / entry.name)
        except OSError:
            return []
        return sorted(targets, key=lambda p: str(p))

    def _bind_mount(self, source: Path, target: Path) -> None:
        cmd = ["mount", "--bind", str(source), str(target)]
        proc = run_cmd(cmd)
        if proc.returncode != 0:
            stderr = (proc.stderr or "").strip()
            first_line = (stderr.splitlines() or [""])[0].strip()
            raise SandboxUnavailableError(
                "bind_mount_failed",
                details=(first_line or f"exit_code={proc.returncode}"),
            )

    def _prepare_overlay_dirs_for_user(
        self, *, upperdir: Path, workdir: Path, uid: int, gid: int
    ) -> None:
        """Ensure overlay upperdir/workdir are writable by the payload user.

        When the sandbox is created by a privileged daemon but the payload is
        executed as an unprivileged uid/gid (via bwrap --uid/--gid), the kernel
        will create whiteouts/upper files using that uid's credentials.
        If upperdir/workdir are owned by root with 0755, writes/deletes will
        fail in the sandbox pre-run and we would miss FS changes (risk false LOW).
        """

        try:
            os.chown(upperdir, uid, gid)
            os.chown(workdir, uid, gid)
            # Keep tight perms; only the requesting user needs access.
            os.chmod(upperdir, 0o700)
            os.chmod(workdir, 0o700)
        except OSError as exc:
            raise SandboxUnavailableError(
                "overlay_perm_failed", details=str(exc)
            ) from exc

    def _sync_overlay_upper_root_metadata(
        self, *, lowerdir: Path, upperdir: Path
    ) -> None:
        """Sync upperdir root metadata with lowerdir root metadata.

        For per-top-level overlays (repo_root == '/'), the overlay root inode may
        effectively inherit metadata from upperdir root. If upperdir keeps default
        0755, directories like /tmp lose sticky/world-writable semantics and tools
        that drop privileges (e.g. apt helper user) cannot create temp files.
        """

        try:
            st = lowerdir.stat()
            if os.geteuid() == 0:
                os.chown(upperdir, st.st_uid, st.st_gid)
            os.chmod(upperdir, stat.S_IMODE(st.st_mode))
        except OSError as exc:
            raise SandboxUnavailableError(
                "overlay_perm_failed",
                details=f"sync upper metadata failed: lowerdir={lowerdir}, upperdir={upperdir}, error={exc}",
            ) from exc

    def _mount_overlay_submounts(
        self,
        *,
        repo_root: Path,
        merged: Path,
        tmpdir: Path,
        run_as_uid: Optional[int] = None,
        run_as_gid: Optional[int] = None,
    ) -> list[tuple[Path, Path]]:
        """Replicate host submounts under repo_root inside merged.

        Returns a list of (mount_point, upperdir) for later fs change collection.
        """

        submounts = _read_host_mount_points_under(repo_root)
        if not submounts:
            return []

        overlays: list[tuple[Path, Path]] = []
        upper_base = tmpdir / "upper_submounts"
        work_base = tmpdir / "work_submounts"
        upper_base.mkdir(parents=True, exist_ok=True)
        work_base.mkdir(parents=True, exist_ok=True)

        # Mount overlays in shallow-to-deep order so deeper mounts aren't hidden.
        for mp in submounts:
            # Avoid overlay recursion: if tmpdir lives under mp (e.g. mp=/run and
            # tmpdir=/run/aish-sandbox-xxxx), then mounting an overlay at merged/<rel>
            # would create a mountpoint inside the lowerdir tree for that submount.
            try:
                if tmpdir.is_relative_to(mp):
                    continue
            except Exception:
                pass

            try:
                rel = mp.relative_to(repo_root)
            except ValueError:
                continue

            # Encode mount point path into a safe directory name.
            encoded = "_".join([p for p in mp.parts if p != "/"])
            if not encoded:
                encoded = "root"
            upperdir = upper_base / encoded
            workdir = work_base / encoded
            upperdir.mkdir(parents=True, exist_ok=True)
            workdir.mkdir(parents=True, exist_ok=True)

            if run_as_uid is not None and run_as_gid is not None:
                self._prepare_overlay_dirs_for_user(
                    upperdir=upperdir,
                    workdir=workdir,
                    uid=int(run_as_uid),
                    gid=int(run_as_gid),
                )

            target = merged / rel
            try:
                target.mkdir(parents=True, exist_ok=True)
            except OSError:
                # If target isn't a directory or cannot be created, skip.
                continue

            options = f"lowerdir={mp},upperdir={upperdir},workdir={workdir}"
            cmd = [
                "mount",
                "-t",
                "overlay",
                "overlay",
                "-o",
                options,
                str(target),
            ]
            proc = run_cmd(cmd)
            if proc.returncode != 0:
                continue

            overlays.append((mp, upperdir))

        return overlays

    def _collect_fs_changes(
        self, lowerdir: Path, upperdir: Path, repo_root: Path
    ) -> List[FsChange]:
        """基于 overlay upperdir 内容推导 created/modified/deleted 路径。"""

        changes: List[FsChange] = []

        upper_files: set[Path] = set()
        opaque_dirs: set[Path] = set()
        deleted_paths: set[str] = set()

        def _record_deleted(path_value: str) -> None:
            if path_value in deleted_paths:
                return
            deleted_paths.add(path_value)
            changes.append(FsChange(path=path_value, kind="deleted"))

        def _build_meta_detail(
            upper_path: Path, lower_path: Path
        ) -> Optional[dict[str, str]]:
            """Return chmod/chown deltas when both sides exist."""

            try:
                up = upper_path.lstat()
                low = lower_path.lstat()
            except OSError:
                return None

            detail: dict[str, str] = {}
            if stat.S_IMODE(up.st_mode) != stat.S_IMODE(low.st_mode):
                detail["mode"] = (
                    f"{stat.S_IMODE(low.st_mode):o}->{stat.S_IMODE(up.st_mode):o}"
                )
            if up.st_uid != low.st_uid:
                detail["uid"] = f"{low.st_uid}->{up.st_uid}"
            if up.st_gid != low.st_gid:
                detail["gid"] = f"{low.st_gid}->{up.st_gid}"

            return detail or None

        for root, dirs, files in os.walk(upperdir):
            root_path = Path(root)

            try:
                flag = os.getxattr(root, b"trusted.overlay.opaque")  # type: ignore[attr-defined]
                if flag == b"y":
                    try:
                        rel_dir = root_path.relative_to(upperdir)
                        opaque_dirs.add(rel_dir)
                    except ValueError:
                        pass
            except (OSError, AttributeError):
                pass

            for name in list(dirs):
                dir_path = root_path / name
                target_rel = dir_path.relative_to(upperdir)

                lower_path = lowerdir / target_rel
                try:
                    logical_rel_to_repo = lower_path.relative_to(repo_root)
                    logical_rel_str = str(logical_rel_to_repo)
                except ValueError:
                    logical_rel_str = str(lower_path)

                if lower_path.exists():
                    detail = _build_meta_detail(dir_path, lower_path)
                    if detail:
                        changes.append(
                            FsChange(
                                path=logical_rel_str, kind="modified", detail=detail
                            )
                        )
                else:
                    changes.append(FsChange(path=logical_rel_str, kind="created"))

            for name in list(files):
                file_path = root_path / name

                if name.startswith(".wh."):
                    target_name = name[len(".wh.") :]
                    target_rel = (root_path / target_name).relative_to(upperdir)
                else:
                    try:
                        st = file_path.lstat()
                    except FileNotFoundError:
                        continue

                    is_char = stat.S_ISCHR(st.st_mode)
                    is_whiteout_inode = (
                        is_char
                        and getattr(os, "makedev", None) is not None
                        and st.st_rdev == os.makedev(0, 0)
                    )

                    target_rel = (root_path / name).relative_to(upperdir)
                    if not is_whiteout_inode:
                        upper_files.add(target_rel)

                lower_path = lowerdir / target_rel

                try:
                    logical_rel_to_repo = lower_path.relative_to(repo_root)
                    logical_rel_str = str(logical_rel_to_repo)
                except ValueError:
                    logical_rel_str = str(lower_path)

                is_whiteout = False
                if name.startswith(".wh."):
                    is_whiteout = True
                else:
                    try:
                        st = file_path.lstat()
                        is_whiteout = (
                            stat.S_ISCHR(st.st_mode)
                            and getattr(os, "makedev", None) is not None
                            and st.st_rdev == os.makedev(0, 0)
                        )
                    except FileNotFoundError:
                        is_whiteout = False

                if is_whiteout:
                    _record_deleted(logical_rel_str)
                    if lower_path.is_dir():
                        for root2, _dirs2, files2 in os.walk(lower_path):
                            root2_path = Path(root2)
                            rel_from_lower = root2_path.relative_to(lowerdir)
                            for name2 in files2:
                                entry_rel = rel_from_lower / name2
                                try:
                                    logical_rel_to_repo = (
                                        lowerdir / entry_rel
                                    ).relative_to(repo_root)
                                    logical_rel_str = str(logical_rel_to_repo)
                                except ValueError:
                                    logical_rel_str = str(lowerdir / entry_rel)
                                _record_deleted(logical_rel_str)
                else:
                    if lower_path.exists():
                        detail = _build_meta_detail(file_path, lower_path)
                        changes.append(
                            FsChange(
                                path=logical_rel_str, kind="modified", detail=detail
                            )
                        )
                    else:
                        changes.append(FsChange(path=logical_rel_str, kind="created"))

        for rel_dir in opaque_dirs:
            lower_dir_path = lowerdir / rel_dir
            if not lower_dir_path.exists():
                continue

            for root, _dirs, files in os.walk(lower_dir_path):
                root_path = Path(root)
                rel_from_lower = root_path.relative_to(lowerdir)

                for name in files:
                    entry_rel = rel_from_lower / name
                    if entry_rel in upper_files:
                        continue

                    lower_path = lowerdir / entry_rel
                    try:
                        logical_rel_to_repo = lower_path.relative_to(repo_root)
                        logical_rel_str = str(logical_rel_to_repo)
                    except ValueError:
                        logical_rel_str = str(lower_path)

                    _record_deleted(logical_rel_str)

        return changes

    def _umount(self, path: Path) -> None:
        cmd = ["umount", str(path)]
        proc = run_cmd(cmd)
        if proc.returncode == 0:
            return

        stderr = (proc.stderr or "").strip()
        if "target is busy" in stderr:
            lazy = run_cmd(["umount", "-l", str(path)])
            if lazy.returncode == 0:
                return

        print(f"[sandbox] WARNING: failed to umount {path}", file=sys.stderr)
        if stderr:
            print(stderr, file=sys.stderr)

    def _run_in_bubblewrap(
        self,
        lower_root: Path,
        upperdir: Path,
        workdir: Path,
        work_subdir: Path,
        command: str,
        *,
        run_as_uid: Optional[int] = None,
        run_as_gid: Optional[int] = None,
        timeout_s: Optional[float] = None,
    ) -> subprocess.CompletedProcess:
        """在 bubblewrap 中执行给定命令。

        这里采用 "overlay merged 作为根 /" 的模型：

        - 通过 ``--bind <merged> /`` 将 overlay 合成视图作为容器内根文件系统；
        - 根据 readonly_binds/readwrite_binds 白名单追加额外挂载（会覆盖 overlay 中对应路径）；
        - 命令在给定的 ``work_subdir`` 下执行，此时任何以 "/" 开头的绝对路径
          都会作用在 overlay 视图上，从而确保 FsChange 可以感知到变更。
        """

        bwrap_cmd: List[str] = ["bwrap"]

        # overlay 的具体挂载由 _mount_overlay 完成；此处仅将其作为根 / 暴露给命令。
        merged_root = workdir.parent / "merged"
        bwrap_cmd.extend(
            [
                "--bind",
                str(merged_root),
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
            ]
        )

        # 追加只读挂载白名单；如果未配置则使用一组安全的默认值。
        readonly_binds = self._config.readonly_binds
        if readonly_binds is None:
            # 当 repo_root 为系统根 "/" 时，我们会在 merged 中复刻子挂载点并用 overlay 覆盖；
            # 此时再 ro-bind /usr 等会把 overlay 结果盖掉，导致变更不可观测。
            if self._config.repo_root.resolve() == Path("/"):
                readonly_binds = []
            else:
                readonly_binds = [
                    (Path("/usr"), Path("/usr")),
                    (Path("/bin"), Path("/bin")),
                    (Path("/lib"), Path("/lib")),
                    (Path("/lib64"), Path("/lib64")),
                ]

        for host_path, sandbox_path in readonly_binds:
            bwrap_cmd.extend(
                [
                    "--ro-bind",
                    str(host_path),
                    str(sandbox_path),
                ]
            )

        # 追加读写挂载白名单（可选）。
        if self._config.readwrite_binds:
            for host_path, sandbox_path in self._config.readwrite_binds:
                bwrap_cmd.extend(
                    [
                        "--bind",
                        str(host_path),
                        str(sandbox_path),
                    ]
                )

        # 将工作目录切换到目标目录并执行命令。
        #
        # IMPORTANT:
        # - 在非 setuid 的 bubblewrap 环境里，直接使用 bwrap --uid/--gid 往往会失败
        #   （表现为 exit_code=1，且没有 fs_changes），导致风险误判为 LOW。
        # - 这里改为让 bwrap 以 root 完成 mount/ns 设置，然后用 setpriv 将 payload
        #   降权到对端 uid/gid。
        bwrap_cmd.extend(["--chdir", str(work_subdir)])

        if run_as_uid is not None or run_as_gid is not None:
            if run_as_uid is None or run_as_gid is None:
                raise SandboxUnavailableError(
                    "bubblewrap_failed",
                    details="run_as_uid/run_as_gid must both be set",
                )
            bwrap_cmd.extend(
                [
                    "setpriv",
                    "--reuid",
                    str(int(run_as_uid)),
                    "--regid",
                    str(int(run_as_gid)),
                    "--clear-groups",
                    "--inh-caps=-all",
                    "bash",
                    "-lc",
                    command,
                ]
            )
        else:
            bwrap_cmd.extend(["bash", "-lc", command])

        proc = (
            run_cmd(bwrap_cmd, timeout=timeout_s)
            if timeout_s is not None
            else run_cmd(bwrap_cmd)
        )
        return proc

    def _run_in_overlay_sandbox(
        self,
        command: str,
        repo_root: Path,
        cwd: Path,
        *,
        run_as_uid: Optional[int] = None,
        run_as_gid: Optional[int] = None,
        timeout_s: Optional[float] = None,
    ) -> SandboxResult:
        repo_root = repo_root.resolve()
        cwd = cwd.resolve()
        if not cwd.is_relative_to(repo_root):
            print("[sandbox] ERROR: cwd must be inside the repo_root.", file=sys.stderr)
            raise SandboxUnavailableError(
                "cwd_outside_repo_root",
                details=f"cwd={cwd}, repo_root={repo_root}",
            )

        # Special handling for repo_root == '/':
        # Overlay-mounting the entire '/' is fragile across kernels (e.g. 20
        # kernel 4.19 may reject it with "overlayfs: overlapping upperdir path").
        # Instead:
        # 1) bind-mount '/' to merged and remount it read-only;
        # 2) overlay each top-level directory under '/' onto merged/<dir>.
        # This keeps the payload isolated while still allowing us to observe writes.
        tmp_parent: str | None = None
        if repo_root == Path("/"):
            shm = Path("/dev/shm")
            if shm.is_dir() and os.access(shm, os.W_OK | os.X_OK):
                tmp_parent = str(shm)

        with tempfile.TemporaryDirectory(
            prefix="aish-sandbox-",
            dir=tmp_parent,
            ignore_cleanup_errors=True,
        ) as tmpdir_str:
            tmpdir = Path(tmpdir_str)
            workdir = tmpdir / "work"
            merged = tmpdir / "merged"
            workdir.mkdir(parents=True, exist_ok=True)
            merged.mkdir(parents=True, exist_ok=True)

            overlays: list[tuple[Path, Path]] = []

            if repo_root == Path("/"):
                # Base root: readonly bind mount
                self._bind_mount(Path("/"), merged)
                self._remount_bind_readonly(merged)

                # Overlay each top-level directory to allow writes and observe changes.
                overlay_targets = self._list_system_root_overlay_targets()
                upper_base = tmpdir / "upper_rootdirs"
                work_base = tmpdir / "work_rootdirs"
                upper_base.mkdir(parents=True, exist_ok=True)
                work_base.mkdir(parents=True, exist_ok=True)

                for lower in overlay_targets:
                    rel = lower.relative_to(Path("/"))
                    target = merged / rel

                    encoded = "_".join([p for p in lower.parts if p != "/"]) or "root"
                    upperdir = upper_base / encoded
                    ovl_workdir = work_base / encoded
                    upperdir.mkdir(parents=True, exist_ok=True)
                    ovl_workdir.mkdir(parents=True, exist_ok=True)

                    if run_as_uid is not None and run_as_gid is not None:
                        self._prepare_overlay_dirs_for_user(
                            upperdir=upperdir,
                            workdir=ovl_workdir,
                            uid=int(run_as_uid),
                            gid=int(run_as_gid),
                        )
                    else:
                        # Root-mode simulation (e.g. stripped sudo): preserve target
                        # directory metadata such as /tmp 1777.
                        self._sync_overlay_upper_root_metadata(
                            lowerdir=lower, upperdir=upperdir
                        )

                    try:
                        target.mkdir(parents=True, exist_ok=True)
                    except OSError as exc:
                        raise SandboxUnavailableError(
                            "overlay_mount_failed",
                            details=f"target={target}: mkdir failed: {exc}",
                        ) from exc

                    try:
                        self._mount_overlay(
                            lowerdir=lower,
                            upperdir=upperdir,
                            workdir=ovl_workdir,
                            merged=target,
                        )
                    except SandboxUnavailableError as exc:
                        detail = exc.details or str(exc)
                        raise SandboxUnavailableError(
                            "overlay_mount_failed",
                            details=f"lowerdir={lower}, target={target}: {detail}",
                        ) from exc

                    overlays.append((lower, upperdir))
            else:
                upperdir = tmpdir / "upper"
                upper_workdir = tmpdir / "work"
                upperdir.mkdir(parents=True, exist_ok=True)
                upper_workdir.mkdir(parents=True, exist_ok=True)

                if run_as_uid is not None and run_as_gid is not None:
                    self._prepare_overlay_dirs_for_user(
                        upperdir=upperdir,
                        workdir=upper_workdir,
                        uid=int(run_as_uid),
                        gid=int(run_as_gid),
                    )

                self._mount_overlay(
                    lowerdir=repo_root,
                    upperdir=upperdir,
                    workdir=upper_workdir,
                    merged=merged,
                )
                overlays.append((repo_root, upperdir))

            try:
                rel_cwd = cwd.relative_to(repo_root)
                # 现在 overlay merged 已作为根 / 暴露给 bubblewrap，
                # 因此工作目录应直接为 "/<rel_cwd>"，避免使用不再存在的 /work 前缀。
                # 例如：repo_root=/, cwd=/ 则 sandbox_cwd="/"；
                #       repo_root=/root/workspace, cwd=/root/workspace/aish
                #       则 sandbox_cwd="/aish"。
                sandbox_cwd = Path("/") / rel_cwd

                proc = self._run_in_bubblewrap(
                    lower_root=repo_root,
                    upperdir=tmpdir,
                    workdir=workdir,
                    work_subdir=sandbox_cwd,
                    command=command,
                    run_as_uid=run_as_uid,
                    run_as_gid=run_as_gid,
                    timeout_s=timeout_s,
                )

                fs_changes: list[FsChange] = []
                for lower, upper in overlays:
                    fs_changes.extend(
                        self._collect_fs_changes(
                            lowerdir=lower, upperdir=upper, repo_root=repo_root
                        )
                    )

                return SandboxResult(
                    exit_code=proc.returncode,
                    stdout=proc.stdout,
                    stderr=proc.stderr,
                    changes=fs_changes,
                )
            finally:
                # Unmount overlays first (deep-to-shallow), then the root bind mount.
                try:
                    overlay_targets = sorted(
                        (
                            merged / lower.relative_to(repo_root)
                            for (lower, _upper) in overlays
                            if repo_root == Path("/")
                        ),
                        key=lambda p: (len(p.parts), str(p)),
                        reverse=True,
                    )
                    for target in overlay_targets:
                        self._umount(target)
                except Exception:
                    pass
                self._umount(merged)

    def simulate(
        self,
        command: str,
        cwd: Path,
        *,
        run_as_uid: Optional[int] = None,
        run_as_gid: Optional[int] = None,
        timeout_s: Optional[float] = None,
    ) -> SandboxResult:
        """在沙箱中执行命令并返回结果及文件变更列表。"""

        return self._run_in_overlay_sandbox(
            command=command,
            repo_root=self._config.repo_root,
            cwd=cwd,
            run_as_uid=run_as_uid,
            run_as_gid=run_as_gid,
            timeout_s=timeout_s,
        )


__all__ = [
    "FsChange",
    "SandboxResult",
    "SandboxConfig",
    "SandboxExecutor",
    "SandboxSecurityResult",
    "SandboxUnavailableError",
]
