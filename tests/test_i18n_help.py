import os
from importlib import import_module, reload

import pytest
from typer.testing import CliRunner

from aish.i18n import get_ui_locale, reset_i18n_for_tests


def _load_cli_app():
    cli_module = import_module("aish.cli")
    cli_module = reload(cli_module)
    return cli_module.app


@pytest.mark.parametrize(
    ("lang", "expected_text", "expected_ui_locale"),
    [
        ("zh_CN.UTF-8", "内置大模型能力的交互式 Shell", "zh-CN"),
        ("en_US.UTF-8", "A shell with built-in LLM capabilities", "en-US"),
        ("de_DE.UTF-8", "Eine Shell mit integrierten LLM-Funktionen", "de-DE"),
        ("fr_FR.UTF-8", "Un shell avec des capacites LLM integrees", "fr-FR"),
        ("es_ES.UTF-8", "Una shell con capacidades LLM integradas", "es-ES"),
        ("ja_JP.UTF-8", "LLM 機能を内蔵したシェル", "ja-JP"),
    ],
)
def test_help_is_localized_by_lang_env(
    monkeypatch, lang: str, expected_text: str, expected_ui_locale: str
):
    runner = CliRunner()

    monkeypatch.setenv("LANG", lang)
    reset_i18n_for_tests()
    app = _load_cli_app()
    result = runner.invoke(app, ["--help"])

    assert result.exit_code == 0
    assert expected_text in result.output
    assert get_ui_locale() == expected_ui_locale


def test_models_auth_login_help_is_localized(monkeypatch):
    runner = CliRunner()

    monkeypatch.setenv("LANG", "de_DE.UTF-8")
    reset_i18n_for_tests()
    app = _load_cli_app()
    result = runner.invoke(app, ["models", "auth", "login", "--help"])

    assert result.exit_code == 0
    assert "Bei einem Provider anmelden und Auth lokal speichern" in result.output


def test_help_falls_back_to_english_for_unsupported_locale(monkeypatch):
    runner = CliRunner()

    monkeypatch.setenv("LANG", "it_IT.UTF-8")
    reset_i18n_for_tests()
    app = _load_cli_app()
    result = runner.invoke(app, ["--help"])

    assert result.exit_code == 0
    assert "A shell with built-in LLM capabilities" in result.output
    assert get_ui_locale() == "en-US"
