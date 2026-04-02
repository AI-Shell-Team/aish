import pytest


@pytest.fixture(autouse=True)
def _isolate_xdg_config_home(tmp_path, monkeypatch: pytest.MonkeyPatch):
    # Ensure tests do not read/write real user config under ~/.config/aish
    # (which makes tests machine-dependent).
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg-config"))


@pytest.fixture
def anyio_backend() -> str:
    # 配置所有 @pytest.mark.anyio 测试使用 asyncio 后端
    return "asyncio"


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--run-live-smoke",
        action="store_true",
        default=False,
        help="run opt-in live smoke tests that require real provider access",
    )


def pytest_collection_modifyitems(
    config: pytest.Config, items: list[pytest.Item]
) -> None:
    if config.getoption("--run-live-smoke"):
        return

    skip_live = pytest.mark.skip(
        reason="need --run-live-smoke to run live provider smoke tests"
    )
    for item in items:
        if "live_smoke" in item.keywords:
            item.add_marker(skip_live)
