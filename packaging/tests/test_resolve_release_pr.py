#!/usr/bin/env python3
"""Unit tests for release PR resolution helpers."""
from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT_PATH = Path(__file__).resolve().parents[2] / "packaging" / "scripts" / "resolve_release_pr.py"
SPEC = importlib.util.spec_from_file_location("resolve_release_pr", SCRIPT_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class ResolveReleasePrTests(unittest.TestCase):
    def test_release_branch_is_candidate(self) -> None:
        self.assertTrue(MODULE.is_release_candidate("release/v0.1.3-prep"))

    def test_release_candidate_label_does_not_trigger(self) -> None:
        self.assertFalse(MODULE.is_release_candidate("feature/foo"))

    def test_regular_branch_is_not_candidate(self) -> None:
        self.assertFalse(MODULE.is_release_candidate("feat/add-thing"))

    def test_parse_version_from_branch(self) -> None:
        self.assertEqual(MODULE.parse_version_from_branch("release/v0.1.3-prep"), "0.1.3")
        self.assertEqual(
            MODULE.parse_version_from_branch("release/v1.0.0-beta.1-prep"),
            "1.0.0-beta.1",
        )
        self.assertIsNone(MODULE.parse_version_from_branch("feature/foo"))


if __name__ == "__main__":
    raise SystemExit(unittest.main())
