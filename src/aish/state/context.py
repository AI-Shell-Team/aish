from enum import Enum
from typing import Any, Optional


class MemoryType(Enum):
    LLM = "llm"
    SHELL = "shell"
    KNOWLEDGE = "knowledge"  # from system and external sources


class ContextManager:
    """
    Context manager with sliding window and token budget control.

    Features:
    - Automatic context trimming based on token budget
    - Separate limits for LLM and SHELL memories
    - KNOWLEDGE memories are always preserved
    - Token estimation for context size tracking
    """

    def __init__(
        self,
        max_llm_messages: int = 50,
        max_shell_messages: int = 20,
        token_budget: Optional[int] = None,
        model: str = "gpt-3.5-turbo",
        enable_token_estimation: bool = True,
    ):
        """
        Initialize context manager.

        Args:
            max_llm_messages: Maximum number of LLM conversation messages (default: 50)
            max_shell_messages: Maximum number of SHELL history entries (default: 20)
            token_budget: Optional token budget limit (if None, only message count limits apply)
            model: Model name for token estimation (default: gpt-3.5-turbo)
            enable_token_estimation: Whether to use tiktoken for token estimation (default: True)
        """
        self.memories = []
        self.max_llm_messages = max_llm_messages
        self.max_shell_messages = max_shell_messages
        self.token_budget = token_budget
        self.model = model
        self.enable_token_estimation = enable_token_estimation

        # Cache system knowledge (never trimmed)
        self.knowledge_cache: dict[str, Any] = {}

        # Initialize tokenizer lazily to avoid startup overhead.
        self.encoding = None
        self._tokenizer_initialized = False

    def _ensure_encoding(self) -> None:
        if self._tokenizer_initialized or not self.enable_token_estimation:
            return
        self._tokenizer_initialized = True
        try:
            import tiktoken
        except Exception:
            self.encoding = None
            return

        try:
            self.encoding = tiktoken.encoding_for_model(self.model)
        except Exception:
            # Fallback to cl100k_base for unknown models
            try:
                self.encoding = tiktoken.get_encoding("cl100k_base")
            except Exception:
                # Token estimation is a best-effort feature; avoid hard failure when
                # the encoding data is unavailable (e.g., offline/test environments).
                self.encoding = None

    def _estimate_text_tokens(self, text: str) -> int:
        """Best-effort token estimation without requiring network access."""
        if not text:
            return 0

        self._ensure_encoding()
        if self.encoding is not None:
            try:
                return len(self.encoding.encode(text))
            except Exception:
                pass

        # Fallback heuristic: ~4 chars/token for English, keeps monotonic growth.
        return max(1, len(text) // 4)

    def set_model(self, model: str) -> None:
        if not model:
            return
        self.model = model
        # Reset tokenizer cache so the new model's encoding is used.
        self.encoding = None
        self._tokenizer_initialized = False

    def add_memory(
        self,
        memory_type: MemoryType,
        content: Any,
    ):
        """Add a memory and automatically trim if needed."""
        if memory_type == MemoryType.KNOWLEDGE:
            # Knowledge is cached separately and never trimmed
            if isinstance(content, dict) and "key" in content:
                self.knowledge_cache[content["key"]] = content.get("value")
            return

        msg = {"content": content, "memory_type": memory_type}
        self.memories.append(msg)

        # Auto-trim if limits exceeded
        self._auto_trim()

    def _auto_trim(self):
        """Automatically trim memories based on limits."""
        # Count memories by type
        llm_count = sum(1 for m in self.memories if m["memory_type"] == MemoryType.LLM)
        shell_count = sum(
            1 for m in self.memories if m["memory_type"] == MemoryType.SHELL
        )

        # Trim LLM messages if over limit
        if llm_count > self.max_llm_messages:
            llm_to_remove = llm_count - self.max_llm_messages
            self._trim_by_type(MemoryType.LLM, llm_to_remove)

        # Trim SHELL messages if over limit
        if shell_count > self.max_shell_messages:
            shell_to_remove = shell_count - self.max_shell_messages
            self._trim_by_type(MemoryType.SHELL, shell_to_remove)

        # Check token budget if specified
        if self.token_budget:
            current_tokens = self.estimate_tokens()
            if current_tokens > self.token_budget:
                self._trim_to_token_budget()

    def _trim_by_type(self, memory_type: MemoryType, count: int):
        """Remove oldest memories of a specific type, preserving system messages."""
        removed = 0
        new_memories = []

        for memory in self.memories:
            if memory["memory_type"] == memory_type and removed < count:
                # Preserve system messages in LLM type
                content = memory["content"]
                if (
                    memory_type == MemoryType.LLM
                    and isinstance(content, dict)
                    and content.get("role") == "system"
                ):
                    new_memories.append(memory)
                    continue

                removed += 1
                continue
            new_memories.append(memory)

        self.memories = new_memories

    def _trim_to_token_budget(self):
        """Trim memories to fit within token budget."""
        while (
            self.token_budget is not None
            and self.estimate_tokens() > self.token_budget
            and len(self.memories) > 0
        ):
            # Find oldest non-system memory
            removed = False
            for i, memory in enumerate(self.memories):
                content = memory["content"]
                # Preserve system messages in LLM memories
                if (
                    memory["memory_type"] == MemoryType.LLM
                    and isinstance(content, dict)
                    and content.get("role") == "system"
                ):
                    continue
                self.memories.pop(i)
                removed = True
                break

            # If only system messages left, can't trim further
            if not removed:
                break

    def estimate_tokens(self, messages: Optional[list[dict]] = None) -> int:
        """
        Estimate token count for messages.

        Args:
            messages: Optional list of messages to estimate (if None, uses current context)

        Returns:
            Estimated token count
        """
        if messages is None:
            messages = self.as_messages()

        total_tokens = 0
        for message in messages:
            content = message.get("content", "")
            if isinstance(content, str):
                total_tokens += self._estimate_text_tokens(content)
            # Add overhead for message structure
            total_tokens += 4

        return total_tokens

    def get_context_size(self) -> dict[str, int]:
        """Get context statistics."""
        llm_count = sum(1 for m in self.memories if m["memory_type"] == MemoryType.LLM)
        shell_count = sum(
            1 for m in self.memories if m["memory_type"] == MemoryType.SHELL
        )
        knowledge_count = len(self.knowledge_cache)
        token_count = self.estimate_tokens()

        return {
            "total_memories": len(self.memories),
            "llm_messages": llm_count,
            "shell_messages": shell_count,
            "knowledge_entries": knowledge_count,
            "estimated_tokens": token_count,
        }

    def clear(self, preserve_knowledge: bool = True):
        """
        Clear all memories.

        Args:
            preserve_knowledge: If True, keep knowledge cache (default: True)
        """
        self.memories = []
        if not preserve_knowledge:
            self.knowledge_cache = {}

    def trim(self, max_messages: int):
        """
        Manually trim to specified number of messages.

        Args:
            max_messages: Maximum number of messages to keep
        """
        if len(self.memories) > max_messages:
            # Keep the most recent messages, preserving system messages
            system_messages = [
                m
                for m in self.memories
                if isinstance(m["content"], dict)
                and m["content"].get("role") == "system"
            ]
            other_messages = [m for m in self.memories if m not in system_messages]

            # Keep system + most recent others
            keep_others = max_messages - len(system_messages)
            if keep_others > 0:
                self.memories = system_messages + other_messages[-keep_others:]
            else:
                self.memories = system_messages[:max_messages]

    def as_messages(self) -> list[dict]:
        """Convert memories to message format for LLM."""
        messages = []

        # Add knowledge as system context if available
        if self.knowledge_cache:
            knowledge_summary = "\n".join(
                [f"{k}: {v}" for k, v in self.knowledge_cache.items()]
            )
            messages.append(
                {"role": "system", "content": f"System Context:\n{knowledge_summary}"}
            )

        for memory in self.memories:
            if memory["memory_type"] == MemoryType.LLM:
                messages.append(memory["content"])

            elif memory["memory_type"] == MemoryType.SHELL:
                # Compact shell history format
                messages.append({"role": "user", "content": memory["content"]})

        return messages
