import logging

import pytest

from aish.state import logging as logging_utils


@pytest.fixture(autouse=True)
def _isolate_xdg_config_home(tmp_path, monkeypatch: pytest.MonkeyPatch):
    # Ensure tests do not read/write real user config under ~/.config/aish
    # (which makes tests machine-dependent).
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg-config"))


@pytest.fixture(autouse=True)
def _reset_aish_logging_state():
    """Prevent tests from leaking global aish logger configuration.

    CLI tests initialize the shared ``aish`` logger and disable propagation,
    which breaks later ``caplog`` assertions when the full suite runs in one
    process. Snapshot and restore that state around every test.
    """

    logger = logging.getLogger("aish")
    original_handlers = list(logger.handlers)
    original_level = logger.level
    original_propagate = logger.propagate
    original_initialized = logging_utils._LOGGING_INITIALIZED

    yield

    for handler in list(logger.handlers):
        logger.removeHandler(handler)
        if handler not in original_handlers:
            try:
                handler.close()
            except Exception:
                pass

    for handler in original_handlers:
        logger.addHandler(handler)

    logger.setLevel(original_level)
    logger.propagate = original_propagate
    logging_utils._LOGGING_INITIALIZED = original_initialized


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
