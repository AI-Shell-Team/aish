from __future__ import annotations

from aish.memory.models import MemoryEntry, MemoryCategory


def test_memory_entry_creation():
    entry = MemoryEntry(
        id=1,
        source="daily:2026-04-03",
        category=MemoryCategory.ENVIRONMENT,
        content="Production DB on port 5432",
        importance=0.8,
    )
    assert entry.id == 1
    assert entry.category == MemoryCategory.ENVIRONMENT
    assert entry.importance == 0.8


def test_memory_category_values():
    assert MemoryCategory.PREFERENCE.value == "preference"
    assert MemoryCategory.ENVIRONMENT.value == "environment"
    assert MemoryCategory.SOLUTION.value == "solution"
    assert MemoryCategory.PATTERN.value == "pattern"
    assert MemoryCategory.OTHER.value == "other"
