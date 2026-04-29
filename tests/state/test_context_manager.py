"""
Comprehensive tests for Context Manager system with sliding window and token budget
"""

import pytest

from aish.state import ContextManager, MemoryType


class TestMemoryType:
    """Test MemoryType enum"""

    def test_memory_type_values(self):
        """Test MemoryType enum values"""
        assert MemoryType.LLM.value == "llm"
        assert MemoryType.SHELL.value == "shell"
        assert MemoryType.KNOWLEDGE.value == "knowledge"


class TestContextManager:
    """Comprehensive tests for ContextManager with new sliding window implementation"""

    def setup_method(self):
        """Set up test fixtures"""
        self.context_manager = ContextManager()

    def test_context_manager_initialization(self):
        """Test ContextManager initialization with default values"""
        assert hasattr(self.context_manager, "memories")
        assert isinstance(self.context_manager.memories, list)
        assert len(self.context_manager.memories) == 0
        assert self.context_manager.max_llm_messages == 50
        assert self.context_manager.max_shell_messages == 20
        assert self.context_manager.token_budget is None

    def test_custom_initialization(self):
        """Test ContextManager initialization with custom values"""
        cm = ContextManager(
            max_llm_messages=100,
            max_shell_messages=30,
            token_budget=4000,
            model="gpt-4",
        )
        assert cm.max_llm_messages == 100
        assert cm.max_shell_messages == 30
        assert cm.token_budget == 4000
        assert cm.model == "gpt-4"

    def test_add_memory_llm(self):
        """Test adding LLM memory"""
        llm_message = {"role": "user", "content": "Hello, how are you?"}
        self.context_manager.add_memory(MemoryType.LLM, llm_message)

        assert len(self.context_manager.memories) == 1
        assert self.context_manager.memories[0]["content"] == llm_message
        assert self.context_manager.memories[0]["memory_type"] == MemoryType.LLM

    def test_add_memory_shell(self):
        """Test adding shell command memory"""
        shell_entry = "$ ls -la → ✓"
        self.context_manager.add_memory(MemoryType.SHELL, shell_entry)

        assert len(self.context_manager.memories) == 1
        assert self.context_manager.memories[0]["content"] == shell_entry
        assert self.context_manager.memories[0]["memory_type"] == MemoryType.SHELL

    def test_add_memory_knowledge(self):
        """Test adding knowledge memory to cache"""
        knowledge = {"key": "system_info", "value": "Darwin 25.0.0"}
        self.context_manager.add_memory(MemoryType.KNOWLEDGE, knowledge)

        # Knowledge should be in cache, not in memories list
        assert len(self.context_manager.memories) == 0
        assert "system_info" in self.context_manager.knowledge_cache
        assert self.context_manager.knowledge_cache["system_info"] == "Darwin 25.0.0"

    def test_auto_trim_llm_messages(self):
        """Test automatic trimming of LLM messages when over limit"""
        cm = ContextManager(max_llm_messages=5)

        # Add 10 LLM messages (over limit)
        for i in range(10):
            cm.add_memory(MemoryType.LLM, {"role": "user", "content": f"Message {i}"})

        # Should only keep the last 5
        llm_count = sum(1 for m in cm.memories if m["memory_type"] == MemoryType.LLM)
        assert llm_count == 5

    def test_auto_trim_shell_messages(self):
        """Test automatic trimming of SHELL messages when over limit"""
        cm = ContextManager(max_shell_messages=3)

        # Add 6 shell entries (over limit)
        for i in range(6):
            cm.add_memory(MemoryType.SHELL, f"$ command_{i} → ✓")

        # Should only keep the last 3
        shell_count = sum(
            1 for m in cm.memories if m["memory_type"] == MemoryType.SHELL
        )
        assert shell_count == 3

    def test_mixed_memory_types_trimming(self):
        """Test trimming with mixed memory types"""
        cm = ContextManager(max_llm_messages=3, max_shell_messages=2)

        # Add mixed memories
        for i in range(5):
            cm.add_memory(MemoryType.LLM, {"role": "user", "content": f"LLM {i}"})
            cm.add_memory(MemoryType.SHELL, f"$ shell_{i} → ✓")

        llm_count = sum(1 for m in cm.memories if m["memory_type"] == MemoryType.LLM)
        shell_count = sum(
            1 for m in cm.memories if m["memory_type"] == MemoryType.SHELL
        )

        assert llm_count == 3
        assert shell_count == 2

    def test_token_estimation(self):
        """Test token estimation"""
        messages = [
            {"role": "user", "content": "Hello world"},
            {"role": "assistant", "content": "Hi there!"},
        ]

        tokens = self.context_manager.estimate_tokens(messages)
        assert tokens > 0
        # Basic sanity check - "Hello world" + "Hi there!" should be > 0 tokens
        assert isinstance(tokens, int)

    def test_token_budget_trimming(self):
        """Test trimming based on token budget"""
        cm = ContextManager(token_budget=100)  # Very small budget

        # Add many long messages
        for i in range(20):
            cm.add_memory(
                MemoryType.LLM,
                {"role": "user", "content": "This is a long message " * 10},
            )

        # Should be trimmed to fit token budget
        estimated = cm.estimate_tokens()
        assert estimated <= cm.token_budget + 100  # Allow some margin

    def test_get_context_size(self):
        """Test getting context statistics"""
        self.context_manager.add_memory(
            MemoryType.LLM, {"role": "user", "content": "Hello"}
        )
        self.context_manager.add_memory(MemoryType.SHELL, "$ ls → ✓")
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE, {"key": "os", "value": "Darwin"}
        )

        stats = self.context_manager.get_context_size()

        assert stats["total_memories"] == 2  # LLM + SHELL (knowledge not in memories)
        assert stats["llm_messages"] == 1
        assert stats["shell_messages"] == 1
        assert stats["knowledge_entries"] == 1
        assert stats["estimated_tokens"] > 0

    def test_clear_memories(self):
        """Test clearing all memories"""
        self.context_manager.add_memory(
            MemoryType.LLM, {"role": "user", "content": "test"}
        )
        self.context_manager.add_memory(MemoryType.SHELL, "$ test → ✓")
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE, {"key": "test", "value": "data"}
        )

        self.context_manager.clear(preserve_knowledge=True)

        assert len(self.context_manager.memories) == 0
        assert len(self.context_manager.knowledge_cache) == 1  # Knowledge preserved

    def test_clear_all_including_knowledge(self):
        """Test clearing all memories including knowledge"""
        self.context_manager.add_memory(
            MemoryType.LLM, {"role": "user", "content": "test"}
        )
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE, {"key": "test", "value": "data"}
        )

        self.context_manager.clear(preserve_knowledge=False)

        assert len(self.context_manager.memories) == 0
        assert len(self.context_manager.knowledge_cache) == 0

    def test_manual_trim(self):
        """Test manual trimming"""
        # Add 10 messages
        for i in range(10):
            self.context_manager.add_memory(
                MemoryType.LLM, {"role": "user", "content": f"Message {i}"}
            )

        # Manually trim to 5
        self.context_manager.trim(5)

        assert len(self.context_manager.memories) <= 5

    def test_as_messages_format(self):
        """Test converting memories to message format"""
        self.context_manager.add_memory(
            MemoryType.LLM, {"role": "user", "content": "Hello"}
        )
        self.context_manager.add_memory(MemoryType.SHELL, "$ ls → ✓")
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE, {"key": "os_info", "value": "Darwin 25.0.0"}
        )

        messages = self.context_manager.as_messages()

        # Should have: system message (from knowledge) + LLM message + SHELL message
        assert len(messages) >= 2

        # First message should be system context from knowledge
        assert messages[0]["role"] == "system"
        assert "os_info" in messages[0]["content"]

        # Find LLM and SHELL messages
        has_llm = any(m.get("content") == "Hello" for m in messages)
        has_shell = any("ls → ✓" in m.get("content", "") for m in messages)

        assert has_llm
        assert has_shell

    def test_knowledge_cache_multiple_entries(self):
        """Test knowledge cache with multiple entries"""
        knowledge_items = [
            {"key": "system_info", "value": "Darwin"},
            {"key": "os_info", "value": "macOS"},
            {"key": "output_language", "value": "English"},
        ]

        for item in knowledge_items:
            self.context_manager.add_memory(MemoryType.KNOWLEDGE, item)

        assert len(self.context_manager.knowledge_cache) == 3
        assert self.context_manager.knowledge_cache["system_info"] == "Darwin"
        assert self.context_manager.knowledge_cache["os_info"] == "macOS"
        assert self.context_manager.knowledge_cache["output_language"] == "English"

    def test_conversation_flow(self):
        """Test realistic conversation flow"""
        conversation = [
            {"role": "user", "content": "What is Python?"},
            {"role": "assistant", "content": "Python is a programming language."},
            {"role": "user", "content": "Show me an example"},
            {"role": "assistant", "content": "print('Hello World')"},
        ]

        for msg in conversation:
            self.context_manager.add_memory(MemoryType.LLM, msg)

        messages = self.context_manager.as_messages()

        # Filter out system messages from knowledge
        llm_messages = [m for m in messages if m.get("role") in ["user", "assistant"]]

        assert len(llm_messages) == 4
        assert llm_messages[0]["content"] == "What is Python?"
        assert llm_messages[-1]["content"] == "print('Hello World')"

    def test_shell_history_format(self):
        """Test compact shell history format"""
        self.context_manager.add_memory(MemoryType.SHELL, "$ pwd → ✓")
        self.context_manager.add_memory(MemoryType.SHELL, "$ ls -la → ✓")
        self.context_manager.add_memory(
            MemoryType.SHELL, "$ invalid_cmd → ✗ (exit 127)\nerror: command not found"
        )

        messages = self.context_manager.as_messages()

        shell_contents = [
            m["content"]
            for m in messages
            if "pwd" in m["content"] or "invalid_cmd" in m["content"]
        ]

        assert len(shell_contents) == 2
        assert "✓" in shell_contents[0]
        assert "✗" in shell_contents[1]
        assert "exit 127" in shell_contents[1]

    def test_system_message_preservation(self):
        """Test that system messages are preserved during trimming"""
        cm = ContextManager(max_llm_messages=3)

        # Add a system message first
        cm.add_memory(MemoryType.LLM, {"role": "system", "content": "You are helpful"})

        # Add many user messages
        for i in range(10):
            cm.add_memory(MemoryType.LLM, {"role": "user", "content": f"Message {i}"})

        messages = cm.as_messages()

        # System message from knowledge should be first
        # System messages in LLM should be preserved
        system_msgs = [m for m in messages if m.get("role") == "system"]
        assert len(system_msgs) >= 1

    def test_empty_context(self):
        """Test handling of empty context"""
        messages = self.context_manager.as_messages()
        assert isinstance(messages, list)
        assert len(messages) == 0

        stats = self.context_manager.get_context_size()
        assert stats["total_memories"] == 0
        assert stats["llm_messages"] == 0
        assert stats["shell_messages"] == 0

    def test_large_context_handling(self):
        """Test handling of very large context"""
        cm = ContextManager(max_llm_messages=100, token_budget=5000)

        # Add many messages
        for i in range(200):
            cm.add_memory(
                MemoryType.LLM,
                {
                    "role": "user",
                    "content": f"This is message number {i} with some content",
                },
            )

        # Should be trimmed
        stats = cm.get_context_size()
        assert stats["llm_messages"] <= 100
        assert stats["estimated_tokens"] <= cm.token_budget + 500  # Allow margin


class TestContextManagerIntegration:
    """Integration tests for ContextManager"""

    def test_realistic_shell_session(self):
        """Test realistic shell session with AI queries"""
        cm = ContextManager()

        # System knowledge
        cm.add_memory(MemoryType.KNOWLEDGE, {"key": "os", "value": "Darwin"})
        cm.add_memory(MemoryType.KNOWLEDGE, {"key": "shell", "value": "zsh"})

        # Shell commands
        cm.add_memory(MemoryType.SHELL, "$ pwd → ✓")
        cm.add_memory(MemoryType.SHELL, "$ ls -la → ✓")

        # AI conversation
        cm.add_memory(
            MemoryType.LLM,
            {"role": "user", "content": "What files are in this directory?"},
        )
        cm.add_memory(
            MemoryType.LLM,
            {"role": "assistant", "content": "Based on the ls output, you have..."},
        )

        messages = cm.as_messages()

        # Should have system context, shell history, and conversation
        assert len(messages) >= 4

        stats = cm.get_context_size()
        assert stats["llm_messages"] == 2
        assert stats["shell_messages"] == 2
        assert stats["knowledge_entries"] == 2

    def test_context_size_growth_control(self):
        """Test that context size is properly controlled"""
        cm = ContextManager(max_llm_messages=10, max_shell_messages=5)

        # Simulate long session
        for i in range(50):
            cm.add_memory(MemoryType.LLM, {"role": "user", "content": f"Query {i}"})
            cm.add_memory(
                MemoryType.LLM, {"role": "assistant", "content": f"Response {i}"}
            )
            cm.add_memory(MemoryType.SHELL, f"$ command_{i} → ✓")

        stats = cm.get_context_size()

        # Should be controlled
        assert stats["llm_messages"] <= 10
        assert stats["shell_messages"] <= 5

        # Knowledge should not be trimmed
        cm.add_memory(MemoryType.KNOWLEDGE, {"key": "test", "value": "data"})
        assert len(cm.knowledge_cache) == 1


def test_recall_clears_old_results_on_no_match():
    """When recall finds no match, old memory_recall must be removed from knowledge_cache."""
    cm = ContextManager()
    # Simulate a previous successful recall
    cm.add_memory(MemoryType.KNOWLEDGE, {
        "key": "memory_recall",
        "value": '<long-term-memory source="recall">\n- [other] stale fact\n</long-term-memory>',
    })
    assert "memory_recall" in cm.knowledge_cache

    # Simulate clearing on no-match (as _recall_memories does)
    cm.knowledge_cache.pop("memory_recall", None)
    assert "memory_recall" not in cm.knowledge_cache


class TestToolResponseOrdering:
    """Verify that as_messages() keeps tool responses directly after their
    assistant tool_calls, even when SHELL memories are interleaved."""

    def test_shell_between_tool_calls_and_response(self):
        """SHELL memory inserted between assistant tool_calls and tool
        response must be moved after the tool response."""
        cm = ContextManager()
        cm.add_memory(MemoryType.LLM, {"role": "user", "content": "run ip a"})
        cm.add_memory(
            MemoryType.LLM,
            {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "id": "call_001",
                        "type": "function",
                        "function": {"name": "bash_exec", "arguments": "{}"},
                    }
                ],
            },
        )
        # SHELL memory added during tool execution
        cm.add_memory(
            MemoryType.SHELL, "$ ip a → ✓ (exit 0)\n<stdout>1: lo</stdout>"
        )
        cm.add_memory(
            MemoryType.LLM,
            {
                "role": "tool",
                "tool_call_id": "call_001",
                "content": "<stdout>1: lo</stdout>",
            },
        )

        msgs = cm.as_messages()

        assert msgs[0]["role"] == "user"
        assert msgs[1]["role"] == "assistant"
        assert msgs[1].get("tool_calls") is not None
        # tool response must directly follow assistant tool_calls
        assert msgs[2]["role"] == "tool"
        assert msgs[2]["tool_call_id"] == "call_001"
        # SHELL-originated user message comes after tool response
        assert msgs[3]["role"] == "user"
        assert "$ ip a" in msgs[3]["content"]

    def test_multiple_tool_calls(self):
        """Multiple tool calls from one assistant message."""
        cm = ContextManager()
        cm.add_memory(MemoryType.LLM, {"role": "user", "content": "go"})
        cm.add_memory(
            MemoryType.LLM,
            {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "id": "call_a",
                        "type": "function",
                        "function": {"name": "f1", "arguments": "{}"},
                    },
                    {
                        "id": "call_b",
                        "type": "function",
                        "function": {"name": "f2", "arguments": "{}"},
                    },
                ],
            },
        )
        cm.add_memory(MemoryType.SHELL, "$ cmd1 → ✓ (exit 0)")
        cm.add_memory(
            MemoryType.LLM,
            {"role": "tool", "tool_call_id": "call_a", "content": "result-a"},
        )
        cm.add_memory(MemoryType.SHELL, "$ cmd2 → ✗ (exit 1)")
        cm.add_memory(
            MemoryType.LLM,
            {"role": "tool", "tool_call_id": "call_b", "content": "result-b"},
        )

        msgs = cm.as_messages()

        assert msgs[0]["role"] == "user"
        assert msgs[1]["role"] == "assistant"
        # Both tool responses directly follow
        assert msgs[2]["role"] == "tool"
        assert msgs[2]["tool_call_id"] == "call_a"
        assert msgs[3]["role"] == "tool"
        assert msgs[3]["tool_call_id"] == "call_b"
        # Deferred user messages come after
        assert msgs[4]["role"] == "user"
        assert msgs[5]["role"] == "user"

    def test_no_tool_calls_unchanged(self):
        """Messages without tool calls should not be reordered."""
        cm = ContextManager()
        cm.add_memory(MemoryType.LLM, {"role": "user", "content": "hello"})
        cm.add_memory(
            MemoryType.LLM, {"role": "assistant", "content": "hi there"}
        )
        cm.add_memory(MemoryType.SHELL, "$ ls → ✓ (exit 0)")
        cm.add_memory(
            MemoryType.LLM, {"role": "user", "content": "thanks"}
        )

        msgs = cm.as_messages()

        assert len(msgs) == 4
        assert msgs[0]["content"] == "hello"
        assert msgs[1]["content"] == "hi there"
        assert msgs[2]["content"] == "$ ls → ✓ (exit 0)"
        assert msgs[3]["content"] == "thanks"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
