import shutil
from pathlib import Path
from typing import cast
from unittest.mock import AsyncMock, patch

import anyio
import pytest

from aish.config import ConfigModel
from aish.llm import LLMSession
from aish.state import ContextManager, MemoryType
from aish.skills import SkillManager
from aish.skills.hotreload import SkillHotReloadService
from aish.tools.skill import SkillTool


def _write_skill(path: Path, *, name: str, description: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "\n".join(
            [
                "---",
                f"name: {name}",
                f"description: {description}",
                "---",
                "",
                "# Test Skill",
                "Hello",
                "",
            ]
        ),
        encoding="utf-8",
    )


def _extract_skill_tool_description(tools_spec: list[dict]) -> str:
    return next(
        t["function"]["description"]
        for t in tools_spec
        if t.get("function", {}).get("name") == "skill"
    )


def _extract_skills_reminders(messages: list[dict]) -> list[tuple[int, str]]:
    results: list[tuple[int, str]] = []
    for idx, msg in enumerate(messages):
        content = msg.get("content")
        if msg.get("role") != "user" or not isinstance(content, str):
            continue
        if (
            "<system-reminder>" in content
            and "The following skills are available for use with the Skill tool:"
            in content
        ):
            results.append((idx, content))
    if not results:
        raise AssertionError("skills reminder message not found")
    return results


def _persisted_skills_reminders(context_manager: ContextManager) -> list[str]:
    reminders: list[str] = []
    for memory in context_manager.memories:
        if memory.get("memory_type") != MemoryType.LLM:
            continue
        content = memory.get("content")
        if not isinstance(content, dict):
            continue
        body = content.get("content")
        if isinstance(body, str) and "<system-reminder>" in body:
            reminders.append(body)
    return reminders


def test_skill_manager_invalidate_then_reload(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_path = config_dir / "skills" / "my-skill" / "SKILL.md"
    _write_skill(skill_path, name="my-skill", description="v1")

    manager = SkillManager()
    manager.load_all_skills()
    first_version = manager.skills_version

    skill = manager.get_skill("my-skill")
    assert skill is not None
    assert skill.metadata.description == "v1"
    assert manager.is_dirty is False

    _write_skill(skill_path, name="my-skill", description="v2")
    manager.invalidate(skill_path)

    # Lazy: invalidate does not reload immediately.
    assert manager.is_dirty is True
    skill = manager.get_skill("my-skill")
    assert skill is not None
    assert skill.metadata.description == "v1"

    assert manager.reload_if_dirty() is True
    skill = manager.get_skill("my-skill")
    assert skill is not None
    assert skill.metadata.description == "v2"
    assert manager.skills_version == first_version + 1
    assert manager.is_dirty is False
    assert manager.reload_if_dirty() is False


def test_llm_session_skill_tool_refreshes_from_skill_manager(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_path = config_dir / "skills" / "my-skill" / "SKILL.md"
    _write_skill(skill_path, name="my-skill", description="v1")

    manager = SkillManager()
    manager.load_all_skills()

    session = LLMSession(config=ConfigModel(model="test-model"), skill_manager=manager)

    tools_1 = session._get_tools_spec()
    skill_desc_1 = _extract_skill_tool_description(tools_1)
    assert "<skills_count>" not in skill_desc_1
    assert "<available_skills>" not in skill_desc_1

    _write_skill(skill_path, name="my-skill", description="v2")
    manager.invalidate(skill_path)

    tools_2 = session._get_tools_spec()
    skill_desc_2 = _extract_skill_tool_description(tools_2)
    assert skill_desc_2 == skill_desc_1
    assert "<skills_count>" not in skill_desc_2


@pytest.mark.anyio
async def test_process_input_injects_skills_reminder_with_latest_metadata(
    tmp_path, monkeypatch
):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_path = config_dir / "skills" / "my-skill" / "SKILL.md"
    _write_skill(skill_path, name="my-skill", description="v1")

    manager = SkillManager()
    manager.load_all_skills()

    session = LLMSession(config=ConfigModel(model="test-model"), skill_manager=manager)
    context_manager = ContextManager()

    captured_messages: list[list[dict]] = []

    async def fake_acompletion(**kwargs):
        captured_messages.append(kwargs["messages"])
        return {
            "choices": [
                {
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }
            ]
        }

    async def run_once(prompt: str, system_message: str) -> list[dict]:
        with (
            patch.object(session, "_ensure_initialized_with_retry", new=AsyncMock()),
            patch.object(session, "_get_acompletion", return_value=fake_acompletion),
            patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
            patch.object(session, "_get_tools_spec", return_value=[]),
        ):
            await session.process_input(
                prompt=prompt,
                context_manager=context_manager,
                system_message=system_message,
            )
        return captured_messages[-1]

    first_messages = await run_once("question-v1", "sys-v1")
    first_reminders = _extract_skills_reminders(first_messages)
    assert len(first_reminders) == 1
    first_reminder_idx, first_reminder = first_reminders[-1]
    first_prompt_idx = next(
        i
        for i, msg in enumerate(first_messages)
        if msg.get("role") == "user" and msg.get("content") == "question-v1"
    )
    assert first_reminder_idx < first_prompt_idx
    assert "- my-skill: v1" in first_reminder

    _write_skill(skill_path, name="my-skill", description="v2")
    manager.invalidate(skill_path)

    second_messages = await run_once("question-v2", "sys-v2")
    second_reminders = _extract_skills_reminders(second_messages)
    assert len(second_reminders) == 1
    latest_reminder = second_reminders[-1][1]
    assert "Current skills count: 1" in latest_reminder
    assert "- my-skill: v2" in latest_reminder
    assert "- my-skill: v1" not in latest_reminder
    assert _persisted_skills_reminders(context_manager) == []


@pytest.mark.anyio
async def test_process_input_does_not_append_new_skills_reminder_without_dirty(
    tmp_path, monkeypatch
):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_path = config_dir / "skills" / "my-skill" / "SKILL.md"
    _write_skill(skill_path, name="my-skill", description="v1")

    manager = SkillManager()
    manager.load_all_skills()

    session = LLMSession(config=ConfigModel(model="test-model"), skill_manager=manager)
    context_manager = ContextManager()

    captured_messages: list[list[dict]] = []

    async def fake_acompletion(**kwargs):
        captured_messages.append(kwargs["messages"])
        return {
            "choices": [
                {
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }
            ]
        }

    async def run_once(prompt: str, system_message: str) -> list[dict]:
        with (
            patch.object(session, "_ensure_initialized_with_retry", new=AsyncMock()),
            patch.object(session, "_get_acompletion", return_value=fake_acompletion),
            patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
            patch.object(session, "_get_tools_spec", return_value=[]),
        ):
            await session.process_input(
                prompt=prompt,
                context_manager=context_manager,
                system_message=system_message,
            )
        return captured_messages[-1]

    first_messages = await run_once("question-1", "sys-1")
    first_reminders = _extract_skills_reminders(first_messages)
    assert len(first_reminders) == 1

    second_messages = await run_once("question-2", "sys-2")
    second_reminders = _extract_skills_reminders(second_messages)
    assert len(second_reminders) == 1
    latest_reminder = second_reminders[-1][1]
    assert "- my-skill: v1" in latest_reminder


def test_skill_manager_delete_skill_then_reload(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_path = config_dir / "skills" / "my-skill" / "SKILL.md"
    _write_skill(skill_path, name="my-skill", description="v1")

    manager = SkillManager()
    manager.load_all_skills()
    first_version = manager.skills_version

    assert manager.get_skill("my-skill") is not None

    skill_path.unlink()
    manager.invalidate(skill_path)

    assert manager.reload_if_dirty() is True
    assert manager.get_skill("my-skill") is None
    assert manager.skills_version == first_version + 1


@pytest.mark.anyio
async def test_process_input_skills_reminder_reflects_deleted_skill(
    tmp_path, monkeypatch
):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_1 = config_dir / "skills" / "skill-1" / "SKILL.md"
    skill_2 = config_dir / "skills" / "skill-2" / "SKILL.md"
    _write_skill(skill_1, name="skill-1", description="one")
    _write_skill(skill_2, name="skill-2", description="two")

    manager = SkillManager()
    manager.load_all_skills()

    session = LLMSession(config=ConfigModel(model="test-model"), skill_manager=manager)
    context_manager = ContextManager()

    captured_messages: list[list[dict]] = []

    async def fake_acompletion(**kwargs):
        captured_messages.append(kwargs["messages"])
        return {
            "choices": [
                {
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }
            ]
        }

    async def run_once(prompt: str, system_message: str) -> str:
        with (
            patch.object(session, "_ensure_initialized_with_retry", new=AsyncMock()),
            patch.object(session, "_get_acompletion", return_value=fake_acompletion),
            patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
            patch.object(session, "_get_tools_spec", return_value=[]),
        ):
            await session.process_input(
                prompt=prompt,
                context_manager=context_manager,
                system_message=system_message,
            )
        reminders = _extract_skills_reminders(captured_messages[-1])
        return reminders[-1][1]

    reminder_1 = await run_once("question-1", "sys-1")
    assert "- skill-1: one" in reminder_1
    assert "- skill-2: two" in reminder_1

    skill_2.unlink()
    manager.invalidate(skill_2)

    await run_once("question-2", "sys-2")
    reminders_after_delete = _extract_skills_reminders(captured_messages[-1])
    assert len(reminders_after_delete) == 1
    latest_reminder = reminders_after_delete[-1][1]
    assert "Current skills count: 1" in latest_reminder
    assert "- skill-1: one" in latest_reminder
    assert "- skill-2: two" not in latest_reminder
    assert _persisted_skills_reminders(context_manager) == []


@pytest.mark.anyio
async def test_process_input_keeps_skills_reminder_across_tool_rounds(
    tmp_path, monkeypatch
):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_path = config_dir / "skills" / "my-skill" / "SKILL.md"
    _write_skill(skill_path, name="my-skill", description="v1")

    manager = SkillManager()
    manager.load_all_skills()

    session = LLMSession(config=ConfigModel(model="test-model"), skill_manager=manager)
    context_manager = ContextManager()

    captured_messages: list[list[dict]] = []

    responses = iter(
        [
            {
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "Calling tool",
                            "tool_calls": [
                                {
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "bash_exec",
                                        "arguments": "{}",
                                    },
                                }
                            ],
                        },
                        "finish_reason": "tool_calls",
                    }
                ]
            },
            {
                "choices": [
                    {
                        "message": {"role": "assistant", "content": "done"},
                        "finish_reason": "stop",
                    }
                ]
            },
        ]
    )

    async def fake_acompletion(**kwargs):
        captured_messages.append(kwargs["messages"])
        return next(responses)

    with (
        patch.object(session, "_ensure_initialized_with_retry", new=AsyncMock()),
        patch.object(session, "_get_acompletion", return_value=fake_acompletion),
        patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
        patch.object(session, "_get_tools_spec", return_value=[]),
        patch.object(
            session, "_handle_tool_calls", new_callable=AsyncMock
        ) as mock_tool,
    ):

        async def _fake_handle_tool_calls(tool_calls, cm, system_message, output):
            return (
                False,
                "",
                session._get_messages_with_system(cm, system_message),
            )

        mock_tool.side_effect = _fake_handle_tool_calls
        result = await session.process_input(
            prompt="question",
            context_manager=context_manager,
            system_message="sys",
        )

    assert result == "done"
    assert len(captured_messages) == 2
    for request_messages in captured_messages:
        reminder_count = 0
        for msg in request_messages:
            content = msg.get("content")
            if (
                msg.get("role") == "user"
                and isinstance(content, str)
                and "<system-reminder>" in content
                and "The following skills are available for use with the Skill tool:"
                in content
            ):
                reminder_count += 1
                assert "- my-skill: v1" in content
        assert reminder_count == 1


def test_skill_tool_runtime_validation_tracks_deleted_skill(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    skill_1 = config_dir / "skills" / "skill-1" / "SKILL.md"
    skill_2 = config_dir / "skills" / "skill-2" / "SKILL.md"
    _write_skill(skill_1, name="skill-1", description="one")
    _write_skill(skill_2, name="skill-2", description="two")

    manager = SkillManager()
    manager.load_all_skills()
    session = LLMSession(config=ConfigModel(model="test-model"), skill_manager=manager)

    # Prime sync once.
    session._get_tools_spec()
    tool = cast(SkillTool, session.tools["skill"])
    ok_result = tool(skill_name="skill-2")
    assert ok_result.ok is True

    skill_2.unlink()
    manager.invalidate(skill_2)

    # Sync runtime skills snapshot again and validate invocation failure.
    session._get_tools_spec()
    bad_result = tool(skill_name="skill-2")
    assert bad_result.ok is False
    assert "Unknown skill: skill-2" in bad_result.output


def test_skill_manager_loads_symlinked_skill_dir(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"
    config_dir.mkdir()

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    # Put the symlink under CLAUDE skills root to match real usage.
    claude_root = home / ".claude" / "skills"
    claude_root.mkdir(parents=True)

    target_dir = tmp_path / "repo" / "ui-ux-pro-max"
    skill_path = target_dir / "SKILL.md"
    _write_skill(skill_path, name="ui-ux-pro-max", description="v1")

    link_dir = claude_root / "ui-ux-pro-max"
    try:
        link_dir.symlink_to(target_dir, target_is_directory=True)
    except (OSError, NotImplementedError) as exc:
        pytest.skip(f"symlink not supported in test environment: {exc}")

    manager = SkillManager()
    manager.load_all_skills()

    skill = manager.get_skill("ui-ux-pro-max")
    assert skill is not None
    assert skill.metadata.description == "v1"
    assert Path(skill.file_path).name.upper() == "SKILL.MD"


def test_skill_hotreload_discovers_symlink_skill_dirs(tmp_path, monkeypatch):
    import aish.skills.hotreload as hotreload_mod

    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"
    config_dir.mkdir()

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    claude_root = home / ".claude" / "skills"
    claude_root.mkdir(parents=True)

    target_dir = tmp_path / "repo" / "ui-ux-pro-max"
    skill_path = target_dir / "SKILL.md"
    _write_skill(skill_path, name="ui-ux-pro-max", description="v1")

    link_dir = claude_root / "ui-ux-pro-max"
    try:
        link_dir.symlink_to(target_dir, target_is_directory=True)
    except (OSError, NotImplementedError) as exc:
        pytest.skip(f"symlink not supported in test environment: {exc}")

    skill_dirs = hotreload_mod._list_skill_dirs([claude_root])
    assert link_dir in skill_dirs
    assert target_dir.resolve() in skill_dirs


def test_skill_hotreload_adds_symlink_targets_to_watch_roots(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"
    config_dir.mkdir()

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    claude_root = home / ".claude" / "skills"
    claude_root.mkdir(parents=True)

    target_dir = tmp_path / "repo" / "ui-ux-pro-max"
    skill_path = target_dir / "SKILL.md"
    _write_skill(skill_path, name="ui-ux-pro-max", description="v1")

    link_dir = claude_root / "ui-ux-pro-max"
    try:
        link_dir.symlink_to(target_dir, target_is_directory=True)
    except (OSError, NotImplementedError) as exc:
        pytest.skip(f"symlink not supported in test environment: {exc}")

    manager = SkillManager()
    manager.load_all_skills()
    service = SkillHotReloadService(skill_manager=manager, debounce_ms=0)

    extra_roots = service._symlink_watch_roots([claude_root])
    assert target_dir.resolve() in extra_roots


@pytest.mark.anyio
async def test_skill_hotreload_invalidates_when_root_removed(tmp_path, monkeypatch):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"
    config_dir.mkdir()

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    root = config_dir / "skills"
    skill_path = root / "my-skill" / "SKILL.md"
    _write_skill(skill_path, name="my-skill", description="v1")

    manager = SkillManager()
    manager.load_all_skills()
    assert manager.is_dirty is False

    shutil.rmtree(root)

    service = SkillHotReloadService(skill_manager=manager, debounce_ms=0)
    rebuild_event = anyio.Event()

    async def fake_awatch(*args, **kwargs):
        yield {(None, str(root))}

    import aish.skills.hotreload as hotreload_mod

    monkeypatch.setattr(hotreload_mod, "awatch", fake_awatch)

    await service._watch_active_roots([root], rebuild_event)

    assert rebuild_event.is_set()
    assert manager.is_dirty is True


@pytest.mark.anyio
async def test_skill_hotreload_invalidates_when_pending_root_changes(
    tmp_path, monkeypatch
):
    home = tmp_path / "home"
    home.mkdir()
    config_dir = tmp_path / "config"
    config_dir.mkdir()

    monkeypatch.setenv("HOME", str(home))
    monkeypatch.setenv("AISH_CONFIG_DIR", str(config_dir))

    manager = SkillManager()
    manager.load_all_skills()
    assert manager.is_dirty is False

    pending_root = config_dir / "skills"
    watch_dirs = [config_dir]

    service = SkillHotReloadService(skill_manager=manager, debounce_ms=0)
    rebuild_event = anyio.Event()

    async def fake_awatch(*args, **kwargs):
        yield {(None, str(pending_root))}

    import aish.skills.hotreload as hotreload_mod

    monkeypatch.setattr(hotreload_mod, "awatch", fake_awatch)

    await service._watch_pending_roots([pending_root], watch_dirs, rebuild_event)

    assert rebuild_event.is_set()
    assert manager.is_dirty is True
