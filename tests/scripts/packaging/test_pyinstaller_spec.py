from pathlib import Path


def test_spec_collects_shell_lazy_submodules():
    spec_path = Path(__file__).resolve().parents[3] / "aish.spec"
    spec_text = spec_path.read_text(encoding="utf-8")

    assert "collect_submodules('aish.shell')" in spec_text
    assert "] + shell_hiddenimports" in spec_text