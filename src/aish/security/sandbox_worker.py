from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from .sandbox import SandboxConfig, SandboxExecutor, SandboxUnavailableError


def _error(reason: str, error: str) -> dict[str, Any]:
    return {"ok": False, "reason": reason, "error": error}


def main() -> int:
    raw = sys.stdin.read()
    if not raw:
        print(json.dumps(_error("bad_request", "empty_stdin"), ensure_ascii=False))
        return 0

    try:
        req = json.loads(raw)
    except Exception:
        print(json.dumps(_error("bad_request", "invalid_json"), ensure_ascii=False))
        return 0

    if not isinstance(req, dict):
        print(
            json.dumps(_error("bad_request", "request_not_object"), ensure_ascii=False)
        )
        return 0

    command = req.get("command")
    cwd = req.get("cwd")
    repo_root = req.get("repo_root")
    sim_uid = req.get("sim_uid")
    sim_gid = req.get("sim_gid")
    timeout_s = req.get("timeout_s")

    if (
        not isinstance(command, str)
        or not isinstance(cwd, str)
        or not isinstance(repo_root, str)
    ):
        print(json.dumps(_error("bad_request", "missing_fields"), ensure_ascii=False))
        return 0

    try:
        cwd_path = Path(cwd).resolve()
        root_path = Path(repo_root).resolve()

        run_uid = int(sim_uid) if sim_uid is not None else None
        run_gid = int(sim_gid) if sim_gid is not None else None
        run_timeout = float(timeout_s) if timeout_s is not None else None

        cfg = SandboxConfig(repo_root=root_path)
        executor = SandboxExecutor(cfg)
        result = executor.simulate(
            command,
            cwd=cwd_path,
            run_as_uid=run_uid,
            run_as_gid=run_gid,
            timeout_s=run_timeout,
        )

        resp = {
            "ok": True,
            "result": {
                "exit_code": int(result.exit_code),
                "stdout": result.stdout or "",
                "stderr": result.stderr or "",
                "changes": [
                    {"path": c.path, "kind": c.kind} for c in (result.changes or [])
                ],
            },
        }
        print(json.dumps(resp, ensure_ascii=False))
    except SandboxUnavailableError as exc:
        detail = exc.details or str(exc)
        print(json.dumps(_error(exc.reason, str(detail)), ensure_ascii=False))
    except Exception as exc:
        print(
            json.dumps(
                _error("server_error", f"{type(exc).__name__}: {exc}"),
                ensure_ascii=False,
            )
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
