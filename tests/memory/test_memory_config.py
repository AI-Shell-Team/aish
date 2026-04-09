from __future__ import annotations

from aish.memory.config import MemoryConfig


def test_memory_config_defaults():
    config = MemoryConfig()
    assert config.enabled is True
    assert config.recall_limit == 5
    assert config.recall_token_budget == 512
    assert config.auto_recall is True
    assert config.auto_retain is True
    assert "aish/memory" in config.data_dir or "memory" in config.data_dir


def test_memory_config_custom():
    config = MemoryConfig(
        enabled=False,
        recall_limit=10,
        recall_token_budget=1024,
        auto_retain=False,
    )
    assert config.enabled is False
    assert config.recall_limit == 10
    assert config.recall_token_budget == 1024
    assert config.auto_retain is False


def test_config_model_has_memory_field():
    from aish.config import ConfigModel

    config = ConfigModel()
    assert hasattr(config, "memory")
    assert isinstance(config.memory, MemoryConfig)
