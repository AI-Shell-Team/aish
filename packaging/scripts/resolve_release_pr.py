#!/usr/bin/env python3
"""Validate release PR context for Release Preparation workflows."""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path


RELEASE_BRANCH_RE = re.compile(r"^release/v(?P<version>.+)-prep$")


def _write_github_output(path: Path, outputs: dict[str, str]) -> None:
    with path.open("a", encoding="utf-8") as handle:
        for key, value in outputs.items():
            handle.write(f"{key}={value}\n")


def is_release_candidate(head_ref: str) -> bool:
    return RELEASE_BRANCH_RE.fullmatch(head_ref) is not None


def parse_version_from_branch(head_ref: str) -> str | None:
    match = RELEASE_BRANCH_RE.fullmatch(head_ref)
    if not match:
        return None
    return match.group("version")


def fetch_pull_request(pr_number: int, repository: str) -> dict:
    try:
        output = subprocess.check_output(
            [
                "gh",
                "pr",
                "view",
                str(pr_number),
                "--repo",
                repository,
                "--json",
                "headRefName,headRepository,mergeable,mergeStateStatus",
            ],
            stderr=subprocess.STDOUT,
            text=True,
        )
    except subprocess.CalledProcessError as exc:
        raise SystemExit(exc.output.strip() or f"Failed to load PR #{pr_number} from {repository}.") from exc

    payload = json.loads(output)
    if not isinstance(payload, dict):
        raise SystemExit(f"Unexpected PR payload for #{pr_number}.")
    return payload


def validate_release_pr(pr_number: int, repository: str) -> dict[str, str]:
    pull_request = fetch_pull_request(pr_number, repository)

    head_ref = pull_request["headRefName"]
    head_repository = pull_request.get("headRepository") or {}
    head_repo = head_repository.get("nameWithOwner")
    mergeable = pull_request.get("mergeable")
    merge_state = pull_request.get("mergeStateStatus")

    if not head_repo:
        raise SystemExit(
            f"PR #{pr_number}'s source repository is unavailable (e.g., fork deleted); "
            "cannot validate origin."
        )

    if not is_release_candidate(head_ref):
        raise SystemExit(
            "PR #{pr} is not a release candidate. Use head branch release/vX.Y.Z-prep.".format(
                pr=pr_number
            )
        )

    if head_repo != repository:
        raise SystemExit(
            "Release PR must be opened from an upstream branch ({repo}), not a fork ({head}).\n"
            "Push release/vX.Y.Z-prep to upstream and create a same-repository PR.".format(
                repo=repository,
                head=head_repo,
            )
        )

    if mergeable == "CONFLICTING" or merge_state == "DIRTY":
        raise SystemExit(
            "PR #{pr} is not mergeable ({state}). Resolve conflicts before Release Preparation can run.".format(
                pr=pr_number,
                state=merge_state or mergeable,
            )
        )

    branch_version = parse_version_from_branch(head_ref)
    outputs = {
        "checkout_ref": f"refs/pull/{pr_number}/merge",
        "pr_number": str(pr_number),
        "head_ref": head_ref,
    }
    if branch_version is not None:
        outputs["version"] = branch_version
    return outputs


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate release PR context.")
    parser.add_argument("--pr-number", required=True, type=int)
    parser.add_argument(
        "--repository",
        default=os.environ.get("GITHUB_REPOSITORY", ""),
        help="GitHub repository in owner/name form",
    )
    parser.add_argument("--github-output", help="Path to append GitHub Actions outputs")
    parser.add_argument("--print-json", action="store_true", help="Print resolved context as JSON")
    args = parser.parse_args()

    if not args.repository:
        raise SystemExit("Missing repository. Set GITHUB_REPOSITORY or pass --repository.")

    outputs = validate_release_pr(args.pr_number, args.repository)

    if args.github_output:
        _write_github_output(Path(args.github_output), outputs)
    if args.print_json:
        print(json.dumps(outputs, indent=2))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
