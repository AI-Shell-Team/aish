import os
import subprocess
import sys
from pathlib import Path

import pytest

from aish.i18n import _normalize_lang_to_ui_locale


PROJECT_ROOT = Path(__file__).resolve().parents[2]
SRC_ROOT = PROJECT_ROOT / "src"


def _build_subprocess_env(lang: str) -> dict[str, str]:
    env = dict(os.environ)
    env["LANG"] = lang
    python_path = str(SRC_ROOT)
    existing_python_path = env.get("PYTHONPATH")
    if existing_python_path:
        python_path = f"{python_path}{os.pathsep}{existing_python_path}"
    env["PYTHONPATH"] = python_path
    return env


@pytest.mark.parametrize(
    ("lang", "expected_locale"),
    [
        ("zh_CN.UTF-8", "zh-CN"),
        ("de_DE.UTF-8", "de-DE"),
        ("es_ES.UTF-8", "es-ES"),
        ("fr_FR.UTF-8", "fr-FR"),
        ("ja_JP.UTF-8", "ja-JP"),
        ("en_US.UTF-8", "en-US"),
        ("pt_BR.UTF-8", "en-US"),
        ("C", "en-US"),
        (None, "en-US"),
    ],
)
def test_normalize_lang_to_ui_locale(lang, expected_locale):
    assert _normalize_lang_to_ui_locale(lang) == expected_locale


@pytest.mark.parametrize(
    ("lang", "expected_text"),
    [
        ("zh_CN.UTF-8", "内置大模型能力"),
        ("de_DE.UTF-8", "integrierten LLM-Funktionen"),
        ("es_ES.UTF-8", "capacidades de LLM integradas"),
        ("fr_FR.UTF-8", "capacites LLM integrees"),
        ("ja_JP.UTF-8", "LLM 機能を内蔵"),
        ("en_US.UTF-8", "A shell with built-in LLM capabilities"),
    ],
)
def test_help_is_localized_by_lang_env(lang, expected_text):
    env = _build_subprocess_env(lang)

    result = subprocess.run(
        [sys.executable, "-m", "aish.cli", "--help"],
        capture_output=True,
        text=True,
        env=env,
        cwd=PROJECT_ROOT,
        check=False,
    )

    assert result.returncode == 0
    assert expected_text in result.stdout


def test_help_falls_back_to_english_for_unsupported_locale():
    env = _build_subprocess_env("pt_BR.UTF-8")

    result = subprocess.run(
        [sys.executable, "-m", "aish.cli", "--help"],
        capture_output=True,
        text=True,
        env=env,
        cwd=PROJECT_ROOT,
        check=False,
    )

    assert result.returncode == 0
    assert "A shell with built-in LLM capabilities" in result.stdout
