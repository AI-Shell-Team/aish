from __future__ import annotations

import pytest

from aish.memory.config import MemoryConfig
from aish.memory.manager import MemoryManager
from aish.memory.models import MemoryCategory
from aish.tools.memory_tool import MemoryTool


@pytest.fixture
def memory_tool(tmp_path):
    config = MemoryConfig(data_dir=str(tmp_path / "memory"))
    mgr = MemoryManager(config=config)
    tool = MemoryTool(memory_manager=mgr)
    yield tool
    mgr.close()


def test_tool_spec(memory_tool):
    spec = memory_tool.to_func_spec()
    assert spec["type"] == "function"
    assert spec["function"]["name"] == "memory"
    assert "action" in spec["function"]["parameters"]["properties"]


def test_search_action(memory_tool):
    memory_tool.memory_manager.store(
        content="Redis runs on port 6379",
        category=MemoryCategory.ENVIRONMENT,
        source="explicit",
    )
    result = memory_tool(action="search", query="Redis port")
    assert "6379" in str(result)


def test_store_action(memory_tool):
    result = memory_tool(action="store", content="User prefers dark theme")
    assert "Stored" in str(result) or "stored" in str(result)
    assert "User prefers dark theme" in memory_tool.memory_manager.memory_md.read_text()


def test_list_action(memory_tool):
    memory_tool.memory_manager.store(
        content="Fact one",
        category=MemoryCategory.PATTERN,
        source="explicit",
    )
    memory_tool.memory_manager.store(
        content="Fact two",
        category=MemoryCategory.SOLUTION,
        source="explicit",
    )
    result = memory_tool(action="list")
    assert "Fact" in str(result)


def test_forget_action(memory_tool):
    entry_id = memory_tool.memory_manager.store(
        content="Temporary fact",
        category=MemoryCategory.OTHER,
        source="explicit",
    )
    result = memory_tool(action="forget", memory_id=entry_id)
    assert (
        "Forgot" in str(result)
        or "forgot" in str(result)
        or "removed" in str(result).lower()
    )
    assert "Temporary fact" not in memory_tool.memory_manager.memory_md.read_text()


def test_invalid_action(memory_tool):
    result = memory_tool(action="invalid_action")
    assert "error" in str(result).lower() or "unknown" in str(result).lower()
