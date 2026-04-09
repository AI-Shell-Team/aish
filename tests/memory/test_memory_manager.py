from __future__ import annotations

import pytest

from aish.memory.config import MemoryConfig
from aish.memory.manager import MemoryManager
from aish.memory.models import MemoryCategory


@pytest.fixture
def memory_manager(tmp_path):
    config = MemoryConfig(data_dir=str(tmp_path / "memory"))
    mgr = MemoryManager(config=config)
    yield mgr
    mgr.close()


def test_init_creates_directories(tmp_path):
    config = MemoryConfig(data_dir=str(tmp_path / "memory"))
    mgr = MemoryManager(config=config)
    assert (tmp_path / "memory").is_dir()
    mgr.close()


def test_init_creates_memory_md(memory_manager):
    assert memory_manager.memory_md.exists()
    content = memory_manager.memory_md.read_text()
    assert "Long-term Memory" in content
    assert "## Preferences" in content
    assert "## Other" in content


def test_store_and_retrieve(memory_manager):
    entry_id = memory_manager.store(
        content="Production DB on port 5432",
        category=MemoryCategory.ENVIRONMENT,
        source="explicit",
    )
    assert entry_id > 0

    results = memory_manager.recall("production database", limit=5)
    assert len(results) >= 1
    assert any("5432" in r.content for r in results)


def test_recall_returns_empty_for_no_match(memory_manager):
    results = memory_manager.recall("nonexistent query xyz", limit=5)
    assert len(results) == 0


def test_recall_respects_limit(memory_manager):
    for i in range(10):
        memory_manager.store(
            content=f"Test fact number {i} about servers",
            category=MemoryCategory.ENVIRONMENT,
            source="explicit",
        )
    results = memory_manager.recall("servers", limit=3)
    assert len(results) <= 3


def test_store_writes_to_memory_md(memory_manager):
    memory_manager.store(
        content="Test fact for markdown memory",
        category=MemoryCategory.OTHER,
        source="explicit",
    )
    content = memory_manager.memory_md.read_text()
    assert "Test fact for markdown memory" in content


def test_get_session_context_empty(memory_manager):
    ctx = memory_manager.get_session_context()
    assert ctx == ""


def test_get_session_context_with_memory_md(memory_manager):
    memory_manager.store(
        content="User prefers vim",
        category=MemoryCategory.PREFERENCE,
        source="explicit",
    )
    ctx = memory_manager.get_session_context()
    assert "User prefers vim" in ctx


def test_delete_memory(memory_manager):
    entry_id = memory_manager.store(
        content="Fact to delete",
        category=MemoryCategory.OTHER,
        source="explicit",
    )
    memory_manager.delete(entry_id)
    results = memory_manager.recall("Fact to delete", limit=5)
    assert len(results) == 0
    assert "Fact to delete" not in memory_manager.memory_md.read_text()


def test_list_recent(memory_manager):
    for i in range(5):
        memory_manager.store(
            content=f"Recent fact {i}",
            category=MemoryCategory.PATTERN,
            source="explicit",
        )
    recent = memory_manager.list_recent(limit=3)
    assert len(recent) <= 3
    assert recent[0].id > recent[-1].id


def test_get_system_prompt_section(memory_manager):
    section = memory_manager.get_system_prompt_section()
    assert "Memory System" in section
    assert "search" in section
    assert "MEMORY.md" in section


def test_delete_also_removes_from_memory_md(memory_manager):
    entry_id = memory_manager.store(
        content="Permanent fact to delete",
        category=MemoryCategory.SOLUTION,
        source="explicit",
    )
    mem_text = memory_manager.memory_md.read_text()
    assert "Permanent fact to delete" in mem_text

    memory_manager.delete(entry_id)

    results = memory_manager.recall("Permanent fact to delete", limit=5)
    assert len(results) == 0
    mem_text = memory_manager.memory_md.read_text()
    assert "Permanent fact to delete" not in mem_text


def test_recall_text_truncation(memory_manager):
    long_content = "environment configuration " * 100
    memory_manager.store(
        content=long_content,
        category=MemoryCategory.ENVIRONMENT,
        source="explicit",
    )

    results = memory_manager.recall("environment", limit=5)
    assert len(results) >= 1

    # Build recall text the same way _recall_memories does
    lines = ['<long-term-memory source="recall">']
    for r in results:
        lines.append(f"- [{r.category.value}] {r.content}")
    lines.append("</long-term-memory>")
    full_text = "\n".join(lines)

    # Truncate with budget (4 chars/token heuristic)
    budget = 50
    max_chars = budget * 4
    if len(full_text) > max_chars:
        truncated = full_text[:max_chars].rstrip() + "\n</long-term-memory>"
    else:
        truncated = full_text

    assert len(truncated) <= max_chars + len("\n</long-term-memory>")
    assert truncated.endswith("</long-term-memory>")


def test_store_deduplicates_same_category_and_content(memory_manager):
    first_id = memory_manager.store(
        content="User prefers concise answers",
        category=MemoryCategory.PREFERENCE,
        source="explicit",
    )
    second_id = memory_manager.store(
        content="User prefers concise answers",
        category=MemoryCategory.PREFERENCE,
        source="auto",
    )

    assert first_id == second_id
    assert memory_manager.memory_md.read_text().count("User prefers concise answers") == 1
