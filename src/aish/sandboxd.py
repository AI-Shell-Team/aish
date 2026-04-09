from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path

from aish.security.sandbox_daemon import (
    DEFAULT_SANDBOX_SOCKET_PATH,
    SandboxDaemon,
    SandboxDaemonConfig,
)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="aish-sandboxd", description="Privileged sandbox daemon for aish"
    )
    parser.add_argument(
        "--sandbox-worker",
        action="store_true",
        help=argparse.SUPPRESS,
    )
    parser.add_argument(
        "--socket-path",
        default=str(DEFAULT_SANDBOX_SOCKET_PATH),
        help="Unix domain socket path (ignored when systemd socket activation is used)",
    )

    args = parser.parse_args(argv)

    if args.sandbox_worker:
        from aish.security import sandbox_worker

        return sandbox_worker.main()

    if os.geteuid() != 0:
        print("aish-sandboxd must run as root", file=sys.stderr)
        return 2

    daemon = SandboxDaemon(SandboxDaemonConfig(socket_path=Path(args.socket_path)))
    daemon.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())