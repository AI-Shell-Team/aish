import json
import logging
import os
import threading
import time
import uuid
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import Callable, Optional

import anyio

from aish.llm.agents import SystemDiagnoseAgent
from aish.state.cancellation import CancellationReason, CancellationToken
from aish.config import ConfigModel
from aish.plan import (
    PLAN_ONLY_TOOL_NAMES,
    PLANNING_VISIBLE_TOOL_NAMES,
    READ_ONLY_TOOL_NAMES,
    PlanModeState,
    PlanPhase,
    SIDE_EFFECT_TOOL_NAMES,
    compute_artifact_hash,
    create_new_plan_state,
    decode_plan_state,
    encode_plan_state,
    ensure_plan_artifact,
    is_approved_plan_current,
)
from aish.plan.tools import EnterPlanModeTool, ExitPlanModeTool
from aish.terminal.interaction import (
    InteractionRequest,
    InteractionResponse,
    InteractionStatus,
)
from aish.llm.errors import is_litellm_exception, redact_secrets
from aish.llm.loader import load_litellm
from aish.prompts import PromptManager
from aish.state import ContextManager, MemoryType
from aish.shell.interruption import ShellState
from aish.skills import SkillManager
from aish.tools.base import (
    ToolBase,
    ToolExecutionContext,
    ToolPanelSpec,
    ToolPreflightAction,
    ToolPreflightResult,
)
from aish.tools.code_exec import BashTool, PythonTool
from aish.tools.fs_tools import EditFileTool, ReadFileTool, WriteFileTool
from aish.tools.result import ToolResult
from aish.tools.glob_tool import GlobTool
from aish.tools.grep_tool import GrepTool
from aish.tools.skill import SkillTool, render_skills_reminder_text

logger = logging.getLogger("aish.llm")


class LLMCallbackResult(Enum):
    """Result types for event callbacks"""

    CONTINUE = "continue"  # Default - continue processing
    APPROVE = "approve"  # User approved the action
    DENY = "deny"  # User denied the action
    CANCEL = "cancel"  # User cancelled the operation


class LLMEventType(Enum):
    """
    Event types for LLM interaction.

    Semantics (new event system):
    - OP_*: lifecycle of one public LLMSession call (completion/process_input).
    - GENERATION_*: lifecycle of one underlying model request within an operation.
    - CONTENT_DELTA: assistant visible text stream (streaming or synthesized from non-streaming).
    - REASONING_*: provider "reasoning_content"/internal reasoning stream (if available).
      This is NOT the same as request lifecycle and must not drive "request in progress" UI.
    - TOOL_*: lifecycle of tool execution requested by the model.
    """

    # Operation lifecycle (one user-visible turn)
    OP_START = "op_start"
    OP_END = "op_end"

    # Single model request lifecycle (one actual LLM API call)
    GENERATION_START = "generation_start"
    GENERATION_END = "generation_end"

    # Content streams
    CONTENT_DELTA = "content_delta"
    REASONING_START = "reasoning_start"
    REASONING_DELTA = "reasoning_delta"
    REASONING_END = "reasoning_end"

    TOOL_EXECUTION_START = "tool_execution_start"
    TOOL_EXECUTION_END = "tool_execution_end"
    ERROR = "error"
    TOOL_CONFIRMATION_REQUIRED = "tool_confirmation_required"
    INTERACTION_REQUIRED = "interaction_required"
    CANCELLED = "cancelled"


@dataclass
class LLMEvent:
    """Event data structure for LLM interactions"""

    event_type: LLMEventType
    data: dict
    timestamp: float
    metadata: Optional[dict] = None


class ToolDispatchStatus(Enum):
    EXECUTED = "executed"
    SHORT_CIRCUIT = "short_circuit"
    REJECTED = "rejected"
    CANCELLED = "cancelled"


@dataclass
class ToolDispatchOutcome:
    status: ToolDispatchStatus
    result: ToolResult


def normalize_tool_result(value: object) -> ToolResult:
    if isinstance(value, ToolResult):
        return value
    if isinstance(value, Exception):
        return ToolResult(
            ok=False,
            output=f"Error: {value}",
            meta={"kind": "exception", "exception_type": type(value).__name__},
        )
    if isinstance(value, str):
        return ToolResult(ok=True, output=value)
    return ToolResult(ok=True, output=str(value), meta={"kind": "coerced"})


def ensure_plan_artifact_if_needed(
    plan_state: PlanModeState,
    session_uuid: str,
) -> PlanModeState:
    if plan_state.phase == PlanPhase.NORMAL.value and not plan_state.artifact_path:
        return plan_state.with_updates(
            source_session_uuid=plan_state.source_session_uuid or session_uuid
        )
    return ensure_plan_artifact(plan_state, session_uuid=session_uuid)


def _stream_get_choice_delta(chunk: object) -> tuple[object, object]:
    if isinstance(chunk, dict):
        choice = (chunk.get("choices") or [{}])[0]
        delta = choice.get("delta", {})
        return choice, delta
    choice = chunk.choices[0]  # type: ignore[attr-defined]
    delta = choice.delta  # type: ignore[attr-defined]
    return choice, delta


def _stream_get_delta_value(delta: object, key: str) -> object:
    if isinstance(delta, dict):
        return delta.get(key)
    return getattr(delta, key, None)


def _stream_coerce_message(response: object) -> dict:
    if response is None:
        return {}
    if isinstance(response, dict):
        data = response
    elif hasattr(response, "model_dump"):
        data = response.model_dump()
    elif hasattr(response, "dict"):
        data = response.dict()
    else:
        return {}
    try:
        return data.get("choices", [{}])[0].get("message", {})
    except Exception:
        return {}


class _LLMEventEmitter:
    """Build and emit structured LLM events with minimal callsite noise."""

    def __init__(self, session: "LLMSession", emit_events: bool):
        self._session = session
        self._enabled = bool(emit_events)
        self.operation: str | None = None
        self.turn_id: str | None = None
        self.generation_id: str | None = None
        self.cancelled_reason: str | None = None
        self._reasoning_started = False

    def _emit(self, event_type: LLMEventType, data: dict) -> None:
        if not self._enabled:
            return
        self._session.emit_event(event_type, data)

    def emit_op_start(
        self,
        *,
        operation: str,
        prompt: str | None = None,
        stream: bool | None = None,
    ) -> str:
        self.operation = operation
        self.turn_id = f"turn-{uuid.uuid4().hex[:12]}"
        payload: dict = {"operation": operation, "turn_id": self.turn_id}
        if prompt is not None:
            payload["prompt"] = prompt
        if stream is not None:
            payload["stream"] = stream
        self._emit(LLMEventType.OP_START, payload)
        return self.turn_id

    def emit_op_end(self, *, result: str, cancelled_reason: str | None = None) -> None:
        operation = self.operation or "unknown"
        turn_id = self.turn_id or "unknown"
        reason = (
            cancelled_reason if cancelled_reason is not None else self.cancelled_reason
        )
        self._emit(
            LLMEventType.OP_END,
            {
                "operation": operation,
                "turn_id": turn_id,
                "result": result,
                "cancelled": bool(reason),
                "cancelled_reason": reason,
            },
        )

    def emit_generation_start(self, *, generation_type: str, stream: bool) -> str:
        self.generation_id = f"gen-{uuid.uuid4().hex[:12]}"
        self._emit(
            LLMEventType.GENERATION_START,
            {
                "turn_id": self.turn_id,
                "generation_id": self.generation_id,
                "generation_type": generation_type,
                "stream": stream,
            },
        )
        return self.generation_id

    def emit_generation_end(
        self,
        *,
        status: str,
        finish_reason: str | None = None,
        error_message: str | None = None,
    ) -> None:
        if not self.generation_id:
            return
        payload: dict = {
            "turn_id": self.turn_id,
            "generation_id": self.generation_id,
            "status": status,
        }
        if finish_reason is not None:
            payload["finish_reason"] = finish_reason
        if error_message is not None:
            payload["error_message"] = error_message
        self._emit(LLMEventType.GENERATION_END, payload)

    def emit_content_delta(
        self, *, delta: str, accumulated: str, is_final: bool
    ) -> None:
        if not self.generation_id:
            return
        self._emit(
            LLMEventType.CONTENT_DELTA,
            {
                "turn_id": self.turn_id,
                "generation_id": self.generation_id,
                "delta": delta,
                "accumulated": accumulated,
                "is_final": is_final,
            },
        )

    def emit_reasoning_start(self) -> None:
        if self._reasoning_started or not self.generation_id:
            return
        self._reasoning_started = True
        self._emit(
            LLMEventType.REASONING_START,
            {"turn_id": self.turn_id, "generation_id": self.generation_id},
        )

    def emit_reasoning_delta(self, *, delta: str, accumulated: str) -> None:
        if not self.generation_id:
            return
        if not self._reasoning_started:
            self.emit_reasoning_start()
        self._emit(
            LLMEventType.REASONING_DELTA,
            {
                "turn_id": self.turn_id,
                "generation_id": self.generation_id,
                "delta": delta,
                "accumulated": accumulated,
            },
        )

    def emit_reasoning_end(self) -> None:
        if not self._reasoning_started or not self.generation_id:
            return
        self._reasoning_started = False
        self._emit(
            LLMEventType.REASONING_END,
            {"turn_id": self.turn_id, "generation_id": self.generation_id},
        )

    def emit_cancelled(self, reason: str) -> None:
        if self.cancelled_reason is None:
            self.cancelled_reason = reason
        payload: dict = {"reason": reason, "turn_id": self.turn_id}
        if self.generation_id:
            payload["generation_id"] = self.generation_id
        self._emit(LLMEventType.CANCELLED, payload)

    def emit_error(
        self,
        *,
        error_type: str,
        error_message: str,
        error_details: str | None = None,
    ) -> None:
        payload: dict = {
            "error_type": error_type,
            "error_message": error_message,
            "turn_id": self.turn_id,
        }
        if self.generation_id:
            payload["generation_id"] = self.generation_id
        if error_details is not None:
            payload["error_details"] = error_details
        self._emit(LLMEventType.ERROR, payload)


class LLMSession:
    def __init__(
        self,
        config: ConfigModel,
        skill_manager: SkillManager,
        event_callback: Optional[
            Callable[[LLMEvent], Optional[LLMCallbackResult]]
        ] = None,
        is_command_approved: Optional[Callable[[str], bool]] = None,
        tools_override: Optional[dict[str, ToolBase]] = None,
        cancellation_token: Optional[CancellationToken] = None,
        env_manager=None,
        interruption_manager=None,
        history_manager=None,
        memory_manager=None,
        session_uuid: str | None = None,
        session_state: Optional[dict] = None,
        session_state_updater: Optional[Callable[[dict], None]] = None,
    ):  # noqa: F821
        self.config = config
        self.model = config.model
        self.api_base = config.api_base
        self.api_key = config.api_key
        self.memory_manager = memory_manager
        self.session_uuid = str(session_uuid or uuid.uuid4())
        self._session_state_updater = session_state_updater
        self.plan_state = decode_plan_state(
            (session_state or {}).get("plan_mode"),
            default_source_session_uuid=self.session_uuid,
        )
        self.plan_state = ensure_plan_artifact_if_needed(
            self.plan_state,
            self.session_uuid,
        )

        self.skill_manager = skill_manager
        self.prompt_manager = PromptManager()
        self._skills_version_for_tools = self.skill_manager.skills_version

        # Event callback for new event-driven architecture
        self.event_callback = event_callback

        # Exact-match allowlist for this session (in-memory, non-persisted)
        self.is_command_approved = is_command_approved

        # Cancellation token for graceful interruption
        self.cancellation_token = cancellation_token or CancellationToken()
        self._external_token = cancellation_token  # Keep track if externally provided
        self.interruption_manager = interruption_manager

        # Initialize default tools if no override is provided
        if tools_override is None:
            # 传入一个 lambda 函数来动态获取 cancellation token
            # 这样当 token 被重置时，BashTool 能看到新的 token
            self.bash_tool = BashTool(
                env_manager=env_manager,
                interruption_manager=interruption_manager,
                cancellation_token_ref=lambda: self.cancellation_token,
                history_manager=history_manager,
                offload_settings=getattr(config, "bash_output_offload", None),
            )
            self.python_tool = PythonTool()
            self.read_file_tool = ReadFileTool()
            self.write_file_tool = WriteFileTool()
            self.edit_file_tool = EditFileTool()
            self.glob_tool = GlobTool()
            self.grep_tool = GrepTool()
            from aish.tools.ask_user import AskUserTool

            self.ask_user_tool = AskUserTool(request_interaction=self.request_interaction)
            self.skill_tool = SkillTool(
                skill_manager=self.skill_manager,
                prompt_manager=self.prompt_manager,
            )
            self.enter_plan_mode_tool = EnterPlanModeTool(
                get_plan_state=lambda: self.plan_state,
                begin_new_plan=self.begin_new_plan,
            )
            self.exit_plan_mode_tool = ExitPlanModeTool(
                get_plan_state=lambda: self.plan_state,
                update_plan_state=self.update_plan_state,
                request_interaction=self.request_interaction,
            )
            self.system_diagnose_agent = SystemDiagnoseAgent(
                config=config,
                model_id=self.model,
                api_base=self.api_base,
                api_key=self.api_key,
                skill_manager=self.skill_manager,
                parent_event_callback=self.event_callback,
                cancellation_token=self.cancellation_token.create_child_token(),
                history_manager=history_manager,
                plan_state=self.plan_state,
            )

            self.tools = {
                self.bash_tool.name: self.bash_tool,
                self.python_tool.name: self.python_tool,
                self.read_file_tool.name: self.read_file_tool,
                self.write_file_tool.name: self.write_file_tool,
                self.edit_file_tool.name: self.edit_file_tool,
                self.glob_tool.name: self.glob_tool,
                self.grep_tool.name: self.grep_tool,
                self.ask_user_tool.name: self.ask_user_tool,
                self.enter_plan_mode_tool.name: self.enter_plan_mode_tool,
                self.exit_plan_mode_tool.name: self.exit_plan_mode_tool,
                self.system_diagnose_agent.name: self.system_diagnose_agent,
                self.skill_tool.name: self.skill_tool,
            }

            # Register memory tool if memory manager is provided
            if memory_manager is not None:
                from aish.tools.memory_tool import MemoryTool

                self.memory_tool = MemoryTool(memory_manager=memory_manager)
                self.tools[self.memory_tool.name] = self.memory_tool
        else:
            # Use the provided tool set
            self.tools = tools_override

        self._apply_phase_tool_policies()
        self._persist_plan_state()

        self._tools_spec: Optional[list[dict]] = None

        # Lazy loading for litellm (deferred import to speed up startup)
        self._litellm_module = None
        self._acompletion_func = None
        self._trim_messages_func = None
        self._stream_chunk_builder_func = None

        # Async event for tracking initialization state
        self._initialized = False
        self._init_lock = anyio.Lock()
        self._sync_init_lock = threading.Lock()  # 线程锁，用于同步初始化
        self._init_event = None  # anyio.Event，用于等待后台初始化完成
        self._init_thread_event = threading.Event()  # 线程安全的事件，用于跨线程通信

        # Store langfuse enabled state for cleanup
        # Allow Langfuse to be optional
        self.langfuse_enabled = getattr(config, "enable_langfuse", False)

        # Initialize session tracking for Langfuse
        self.session_id = f"aish-{uuid.uuid4().hex[:8]}-{int(time.time())}"
        self.trace_id = f"trace-{uuid.uuid4().hex[:12]}"
        self.user_id = "aish-user"  # Can be made configurable later
        self.conversation_count = 0

        # Set up Langfuse callbacks only if enabled in config
        if self.langfuse_enabled:
            try:
                litellm = self._get_litellm()
                if litellm is not None:
                    litellm.success_callback = ["langfuse"]
                    litellm.failure_callback = ["langfuse"]
            except Exception:
                # Silently fail if langfuse is not available
                pass

    def _get_litellm(self):
        """延迟加载 litellm 模块，只在首次使用时导入"""
        if self._litellm_module is None:
            litellm = load_litellm()
            if litellm is None:
                logger.error("Failed to import litellm")
                self._litellm_module = False  # 使用 False 标记失败，区别于 None
            else:
                self._litellm_module = litellm
        return self._litellm_module if self._litellm_module is not False else None

    def _get_acompletion(self):
        """延迟加载 acompletion 函数"""
        if self._acompletion_func is None:
            litellm = self._get_litellm()
            if litellm is not None:
                self._acompletion_func = litellm.acompletion
            else:
                from ..i18n import t

                raise RuntimeError(t("errors.litellm_not_installed"))
        return self._acompletion_func

    def _get_trim_messages(self):
        """延迟加载 trim_messages 函数"""
        if self._trim_messages_func is None:
            from litellm.utils import trim_messages

            self._trim_messages_func = trim_messages
        return self._trim_messages_func

    def _get_stream_chunk_builder(self):
        """Lazy load stream_chunk_builder to avoid importing litellm on startup."""
        if self._stream_chunk_builder_func is None:
            litellm = self._get_litellm()
            if litellm is None:
                return None
            self._stream_chunk_builder_func = litellm.stream_chunk_builder
        return self._stream_chunk_builder_func

    def _get_model_provider(self):
        from aish.llm import get_provider_for_model

        return get_provider_for_model(self.model)

    async def _create_completion_response(
        self,
        *,
        messages: list[dict],
        stream: bool,
        tools: Optional[list[dict]] = None,
        tool_choice: str = "auto",
        **kwargs,
    ):
        provider = self._get_model_provider()
        acompletion = self._get_acompletion() if provider.uses_litellm else None
        return await provider.create_completion(
            model=self.model,
            config=self.config,
            api_base=self.api_base,
            api_key=self.api_key,
            messages=messages,
            tools=tools,
            tool_choice=tool_choice,
            stream=stream,
            fallback_completion=acompletion,
            **kwargs,
        )

    def update_model(
        self,
        model: str,
        api_base: Optional[str] = None,
        api_key: Optional[str] = None,
    ) -> None:
        if model:
            self.model = model
        if api_base is not None:
            self.api_base = api_base
        if api_key is not None:
            self.api_key = api_key

        system_agent = getattr(self, "system_diagnose_agent", None)
        if system_agent is not None:
            if model:
                system_agent.model_id = model
            if api_base is not None:
                system_agent.api_base = api_base
            if api_key is not None:
                system_agent.api_key = api_key

    async def _background_initialize(self):
        """后台初始化 litellm 模块，使用独立线程避免阻塞事件循环"""
        if not self._get_model_provider().uses_litellm:
            return

        async with self._init_lock:
            if self._initialized:
                return

            # 创建事件用于通知等待者
            if self._init_event is None:
                self._init_event = anyio.Event()

            # 在独立线程中执行导入，避免阻塞事件循环
            def import_in_thread():
                try:
                    # 在独立线程中导入 litellm
                    self._get_litellm()
                    self._get_acompletion()

                    with self._sync_init_lock:
                        self._initialized = True
                    logger.info(
                        "LLM client initialized successfully in background thread"
                    )

                except Exception as e:
                    logger.warning(
                        f"LLM background initialization failed: {e}, will retry on first use"
                    )
                finally:
                    # 通知主线程导入完成（无论成功或失败）
                    self._init_thread_event.set()

            # 启动独立线程执行导入
            import_thread = threading.Thread(target=import_in_thread, daemon=True)
            import_thread.start()

            # 等待线程启动后再继续
            await anyio.sleep(0.01)

    def _wait_for_init_complete(self):
        """等待后台初始化完成（同步方法）"""
        self._init_thread_event.wait(timeout=10)  # 最多等待 10 秒

    def _sync_initialize(self):
        """同步初始化 litellm，必须在主线程中调用"""
        if self._initialized:
            return

        with self._sync_init_lock:
            if self._initialized:
                return

            try:
                # 在主线程中直接导入 litellm，避免循环导入问题
                self._get_litellm()
                self._get_acompletion()
                self._initialized = True
                logger.info("LLM client initialized successfully")
            except Exception as e:
                logger.error(f"LLM initialization failed: {e}")
                raise RuntimeError(
                    f"Failed to initialize LLM client: {e}. "
                    "Please check if litellm is properly installed."
                ) from e

    async def _ensure_initialized(self):
        """确保 litellm 已初始化，等待后台初始化完成"""
        if not self._get_model_provider().uses_litellm:
            return

        # 如果已经初始化完成，直接返回
        if self._initialized:
            return

        # 如果有后台线程正在初始化，等待它完成
        if self._init_thread_event is not None and not self._init_thread_event.is_set():
            # 等待后台线程完成（使用 to_thread.run_sync 在主线程中等待）
            await anyio.to_thread.run_sync(
                self._wait_for_init_complete, abandon_on_cancel=True
            )

        # 等待后检查是否真的初始化成功
        if self._initialized:
            return

        # 后台任务失败或未启动，同步初始化
        async with self._init_lock:
            if self._initialized:
                return
            # 同步初始化（在主线程中）
            try:
                await anyio.to_thread.run_sync(
                    self._sync_initialize, abandon_on_cancel=True
                )
            except anyio.get_cancelled_exc_class():
                # 用户取消，重新抛出让上层处理
                raise
            except Exception as e:
                logger.error(f"LLM initialization failed: {e}")
                raise

    async def _ensure_initialized_with_retry(
        self, max_retries: int = 5, retry_delay: float = 0.5
    ):
        """
        确保在首次使用 AI 前 litellm 已成功初始化。
        使用重试机制处理 litellm 内部的循环导入问题。

        Args:
            max_retries: 最大重试次数，默认 5 次
            retry_delay: 重试间隔（秒），默认 0.5 秒
        """
        if not self._get_model_provider().uses_litellm or self._initialized:
            return

        last_error = None
        for attempt in range(max_retries):
            try:
                await self._ensure_initialized()
                # 验证初始化是否真正成功
                litellm = self._get_litellm()
                if litellm is not None:
                    # 尝试调用 litellm 的一个函数来验证模块已完全加载
                    # 这会触发任何潜在的循环导入问题
                    import importlib

                    importlib.import_module("litellm.utils")
                    return
                else:
                    last_error = RuntimeError(
                        "litellm module is None after initialization"
                    )
            except ImportError as e:
                # 处理循环导入错误
                if (
                    "partially initialized" in str(e)
                    or "circular import" in str(e).lower()
                ):
                    last_error = e
                    logger.warning(
                        f"LiteLLM circular import detected (attempt {attempt + 1}/{max_retries}), "
                        f"retrying in {retry_delay}s..."
                    )
                    # 重置初始化状态以允许重试
                    with self._sync_init_lock:
                        self._initialized = False
                    await anyio.sleep(retry_delay)
                else:
                    # 其他导入错误直接抛出
                    logger.error(f"LiteLLM import error: {e}")
                    raise RuntimeError(f"Failed to import litellm: {e}") from e
            except Exception as e:
                last_error = e
                logger.warning(
                    f"LiteLLM initialization failed (attempt {attempt + 1}/{max_retries}): {e}"
                )
                # 重置初始化状态以允许重试
                with self._sync_init_lock:
                    self._initialized = False
                await anyio.sleep(retry_delay)

        # 所有重试都失败
        error_msg = (
            f"Failed to initialize LLM client after {max_retries} attempts. "
            f"Last error: {last_error}"
        )
        logger.error(error_msg)
        raise RuntimeError(error_msg) from last_error

    @staticmethod
    def create_subsession(
        config: ConfigModel,
        skill_manager: SkillManager,
        tools: Optional[dict[str, ToolBase]] = None,
        cancellation_token: Optional[CancellationToken] = None,
        plan_state: PlanModeState | None = None,
    ) -> "LLMSession":
        return LLMSession(
            config=config,
            skill_manager=skill_manager,
            tools_override=tools,
            event_callback=None,
            cancellation_token=cancellation_token,
            session_state=encode_plan_state({}, plan_state)
            if plan_state is not None
            else None,
        )

    def _persist_plan_state(self) -> None:
        if self._session_state_updater is None:
            return
        self._session_state_updater(encode_plan_state({}, self.plan_state))

    def update_plan_state(self, next_state: PlanModeState) -> PlanModeState:
        self.plan_state = ensure_plan_artifact_if_needed(next_state, self.session_uuid)
        if hasattr(self, "system_diagnose_agent"):
            self.system_diagnose_agent.plan_state = self.plan_state
        self._apply_phase_tool_policies()
        self.refresh_tools_spec()
        self._persist_plan_state()
        return self.plan_state

    def set_plan_phase(self, phase: str) -> PlanModeState:
        return self.update_plan_state(self.plan_state.with_updates(phase=phase))

    def begin_new_plan(self) -> PlanModeState:
        return self.update_plan_state(
            create_new_plan_state(session_uuid=self.session_uuid)
        )

    def _apply_phase_tool_policies(self) -> None:
        if hasattr(self, "memory_tool"):
            if self.plan_state.phase == PlanPhase.PLANNING.value:
                self.memory_tool.set_allowed_actions({"search", "list"})
            else:
                self.memory_tool.set_allowed_actions(None)

    def _visible_tool_names(self) -> set[str]:
        if self.plan_state.phase == PlanPhase.PLANNING.value:
            visible = set(PLANNING_VISIBLE_TOOL_NAMES)
            if "memory" not in self.tools:
                visible.discard("memory")
            return visible
        return {name for name in self.tools if name not in PLAN_ONLY_TOOL_NAMES}

    def _bound_plan_artifact_path(self) -> Path | None:
        artifact = self.plan_state.artifact
        if artifact is None:
            return None
        try:
            return artifact.expanduser().resolve(strict=False)
        except Exception:
            return artifact.expanduser()

    def _is_bound_plan_artifact_write(self, tool_args: dict[str, object]) -> bool:
        raw_path = tool_args.get("file_path")
        if not isinstance(raw_path, str) or not raw_path.strip():
            return False

        bound_path = self._bound_plan_artifact_path()
        if bound_path is None:
            return False

        try:
            candidate = Path(raw_path).expanduser().resolve(strict=False)
        except Exception:
            return False
        return candidate == bound_path

    def _is_tool_allowed_during_drift(
        self,
        tool_name: str,
        tool_args: dict[str, object],
    ) -> bool:
        if tool_name == "enter_plan_mode":
            return True
        if tool_name not in READ_ONLY_TOOL_NAMES:
            return False
        if tool_name != "memory":
            return True
        action = str(tool_args.get("action") or "").strip().lower()
        return action in {"search", "list"}

    def _plan_runtime_messages(self) -> list[dict]:
        if self.plan_state.phase == PlanPhase.PLANNING.value:
            artifact = self.plan_state.artifact_path or "(unbound)"
            return [
                {
                    "role": "user",
                    "content": (
                        "<system-reminder>\n"
                        "Plan mode is active. Your primary goal is to produce a concrete, actionable plan in the bound plan file.\n"
                        f"Bound plan file: {artifact}\n"
                        "\n"
                        "Workflow:\n"
                        "1. Do targeted research — read only the files directly relevant to the task. Avoid broad exploration.\n"
                        "2. Write an initial draft into the bound plan file early. It is better to write a rough draft and refine it than to research exhaustively before writing.\n"
                        "3. Refine the plan as needed based on findings. Update the bound plan file.\n"
                        "4. When the plan is ready, call exit_plan_mode to present it for user approval.\n"
                        "\n"
                        "Constraints:\n"
                        "- Do not modify workspace files other than the bound plan file.\n"
                        "- Do not run side-effecting shell or python actions, change git state, or write long-term memory.\n"
                        "- Do not use ask_user for routine confirmation. Batch uncertainties and ask only when missing information materially blocks planning.\n"
                        "- Prefer recording assumptions in the plan file instead of interrupting the user.\n"
                        "</system-reminder>"
                    ),
                }
            ]
        if self.plan_state.has_approved_plan_context:
            approved_hash = self.plan_state.approved_artifact_hash or "unapproved"
            approved_artifact = self.plan_state.approved_artifact
            artifact_path = str(approved_artifact) if approved_artifact is not None else "(missing)"
            return [
                {
                    "role": "user",
                    "content": (
                        "<system-reminder>\n"
                        f"An approved plan is active while you are in shell mode. Follow the approved plan artifact at {artifact_path}.\n"
                        f"Approved artifact hash: {approved_hash}\n"
                        "Do not ignore the approved plan without first calling enter_plan_mode and updating approval.\n"
                        "</system-reminder>"
                    ),
                }
            ]
        return []

    def _blocked_in_planning(
        self,
        tool_name: str,
        tool_args: dict,
    ) -> ToolResult | None:
        if tool_name not in self._visible_tool_names():
            return ToolResult(
                ok=False,
                output=(
                    f"Error: tool '{tool_name}' is not available while plan mode is in planning phase"
                ),
                meta={"kind": "plan_mode_blocked", "phase": self.plan_state.phase},
                stop_tool_chain=True,
            )

        if tool_name in {"write_file", "edit_file"}:
            if self._is_bound_plan_artifact_write(tool_args):
                return None
            artifact = self.plan_state.artifact_path or "(unbound)"
            return ToolResult(
                ok=False,
                output=(
                    f"Error: tool '{tool_name}' may only modify the bound planning file during plan mode: {artifact}"
                ),
                meta={"kind": "plan_mode_blocked", "phase": self.plan_state.phase},
                stop_tool_chain=True,
            )

        if tool_name in SIDE_EFFECT_TOOL_NAMES:
            return ToolResult(
                ok=False,
                output=(
                    f"Error: tool '{tool_name}' is blocked during planning. Only research, ask_user, memory search or list, bound plan file edits, and exit_plan_mode are allowed."
                ),
                meta={"kind": "plan_mode_blocked", "phase": self.plan_state.phase},
                stop_tool_chain=True,
            )

        if tool_name == "memory":
            action = str(tool_args.get("action") or "").strip().lower()
            if action not in {"search", "list"}:
                return ToolResult(
                    ok=False,
                    output=(
                        f"Error: memory action '{action}' is blocked during planning. Only search and list are allowed."
                    ),
                    meta={"kind": "plan_mode_blocked", "phase": self.plan_state.phase},
                    stop_tool_chain=True,
                )

        return None

    def _check_execution_plan_drift(self) -> ToolResult | None:
        if not self.plan_state.has_approved_plan_context:
            return None
        if is_approved_plan_current(self.plan_state):
            return None
        approved_artifact = self.plan_state.approved_artifact
        if approved_artifact is None:
            return ToolResult(
                ok=False,
                output="Error: shell mode has no bound approved plan artifact context.",
                meta={"kind": "plan_mode_blocked", "phase": self.plan_state.phase},
                stop_tool_chain=True,
            )
        current_hash = compute_artifact_hash(approved_artifact)
        return ToolResult(
            ok=False,
            output=(
                "Error: the approved plan artifact changed after approval. Return to plan mode, update the artifact, and request approval again."
            ),
            meta={
                "kind": "plan_mode_blocked",
                "phase": self.plan_state.phase,
                "approved_artifact_path": str(approved_artifact),
                "approved_artifact_hash": self.plan_state.approved_artifact_hash,
                "current_artifact_hash": current_hash,
            },
            stop_tool_chain=True,
        )

    def emit_event(
        self, event_type: LLMEventType, data: dict, metadata: Optional[dict] = None
    ) -> Optional[LLMCallbackResult]:
        """Emit an event to the callback if available and return the result"""
        if self.event_callback:
            event = LLMEvent(event_type, data, time.time(), metadata)
            try:
                result = self.event_callback(event)
            except Exception:
                return LLMCallbackResult.CONTINUE
            return result if result is not None else LLMCallbackResult.CONTINUE
        return LLMCallbackResult.CONTINUE

    def request_confirmation(
        self,
        event_type: LLMEventType,
        data: dict,
        timeout_seconds: float = 30.0,
        default_on_timeout: LLMCallbackResult = LLMCallbackResult.DENY,
    ) -> LLMCallbackResult:
        """
        Request user confirmation with timeout handling

        Args:
            event_type: Type of confirmation event
            data: Event data to send
            timeout_seconds: Timeout in seconds (default 30)
            default_on_timeout: Default result if timeout occurs

        Returns:
            LLMCallbackResult from user or default on timeout
        """
        if not self.event_callback:
            return LLMCallbackResult.CONTINUE

        try:
            # For now, we'll use synchronous confirmation
            # In the future, this could be extended to support async timeouts
            result = self.emit_event(event_type, data)

            # Validate result is a confirmation type
            if result in [
                LLMCallbackResult.APPROVE,
                LLMCallbackResult.DENY,
                LLMCallbackResult.CANCEL,
            ]:
                return result
            else:
                # If callback didn't return a valid confirmation, use default
                return default_on_timeout

        except Exception as e:
            print(f"Error during confirmation: {e}")
            # If any error occurs during confirmation, use default
            return default_on_timeout

    def request_interaction(
        self,
        request: InteractionRequest,
    ) -> InteractionResponse:
        """Request a user interaction via the shell UI."""
        # Non-interactive / no UI callback available.
        if not self.event_callback:
            return InteractionResponse(
                interaction_id=request.id,
                status=InteractionStatus.UNAVAILABLE,
                reason="unavailable",
            )

        try:
            import sys

            if not (sys.stdin.isatty() and sys.stdout.isatty()):
                return InteractionResponse(
                    interaction_id=request.id,
                    status=InteractionStatus.UNAVAILABLE,
                    reason="unavailable",
                )
        except Exception:
            # Conservatively treat as unavailable.
            return InteractionResponse(
                interaction_id=request.id,
                status=InteractionStatus.UNAVAILABLE,
                reason="unavailable",
            )

        try:
            event_data = {"interaction_request": request.to_dict()}
            self.emit_event(LLMEventType.INTERACTION_REQUIRED, event_data)
            response_payload = event_data.get("interaction_response")
            if isinstance(response_payload, dict):
                return InteractionResponse.from_dict(response_payload)
            return InteractionResponse(
                interaction_id=request.id,
                status=InteractionStatus.CANCELLED,
                reason="cancelled",
            )
        except KeyboardInterrupt:
            raise
        except Exception:
            return InteractionResponse(
                interaction_id=request.id,
                status=InteractionStatus.UNAVAILABLE,
                reason="error",
            )

    def _get_langfuse_metadata(self, generation_type: str) -> dict:
        """Generate Langfuse metadata for better observability"""
        metadata = {}

        if self.langfuse_enabled:
            self.conversation_count += 1

            metadata = {
                # Trace and session management
                "existing_trace_id": self.trace_id,  # Use existing trace to continue conversation
                "session_id": self.session_id,
                "trace_user_id": self.user_id,
                "trace_name": "aish",
                # Generation identification
                "generation_name": f"aish-{generation_type}-{self.conversation_count}",
                "generation_id": f"gen-{uuid.uuid4().hex[:8]}",
                # Context and categorization
                "tags": ["aish", generation_type],
                "trace_metadata": {
                    "conversation_count": self.conversation_count,
                    "model": self.model,
                    "shell_session": self.session_id,
                    "generation_type": generation_type,
                },
            }

            # Add generation type specific metadata
            if generation_type == "tool_call":
                metadata["tags"].append("function_calling")
                metadata["trace_metadata"]["has_tools"] = True
            elif generation_type == "conversation":
                metadata["tags"].append("chat")
                metadata["trace_metadata"]["is_interactive"] = True

        return metadata

    def reset_cancellation_token(self):
        """Reset the cancellation token for a new operation."""
        if not self._external_token:
            # Only reset if we manage the token internally
            self.cancellation_token = CancellationToken()
            # Update child tokens in tools
            if hasattr(self, "system_diagnose_agent"):
                self.system_diagnose_agent.cancellation_token = (
                    self.cancellation_token.create_child_token()
                )

    async def execute_tool(
        self, tool: ToolBase, tool_name: str, tool_args: dict
    ) -> ToolResult:
        self.emit_event(
            LLMEventType.TOOL_EXECUTION_START,
            {"tool_name": tool_name, "tool_args": tool_args},
        )

        # Handle both sync and async tools
        raw_result = tool(**tool_args)
        if hasattr(raw_result, "__await__"):
            # It's an awaitable, await it
            raw_result = await raw_result

        tool_result = normalize_tool_result(raw_result)
        rendered_result = tool_result.render_for_llm()
        self.emit_event(
            LLMEventType.TOOL_EXECUTION_END,
            {
                "result": rendered_result,
                "result_meta": {
                    "ok": tool_result.ok,
                    "code": tool_result.code,
                    "meta": tool_result.meta,
                },
                "tool_name": tool_name,
                # Include raw ToolResult.data for agents that need structured data
                "result_data": getattr(tool_result, "data", None),
            },
        )
        return tool_result

    def _build_tool_panel_event_data(
        self,
        *,
        tool: ToolBase,
        tool_name: str,
        tool_args: dict,
        panel: ToolPanelSpec,
    ) -> dict:
        panel_payload = panel.to_event_payload()
        data: dict = {
            "tool_name": tool_name,
            "tool_args": tool_args,
            "description": tool.description,
            "panel": panel_payload,
            # Temporary top-level mirror for transition/debugging.
            "panel_mode": panel.mode,
        }
        for key in (
            "target",
            "preview",
            "analysis",
            "allow_remember",
            "remember_key",
            "title",
        ):
            if key in panel_payload:
                data[key] = panel_payload[key]
        return data

    async def pre_execute_tool(
        self, tool_name: str, tool_args: dict
    ) -> ToolDispatchOutcome:
        try:
            if tool_name not in self.tools:
                return ToolDispatchOutcome(
                    status=ToolDispatchStatus.REJECTED,
                    result=ToolResult(
                        ok=False,
                        output=f"Error: Invalid tool name: {tool_name}",
                    ),
                )

            tool = self.tools[tool_name]

            if self.plan_state.phase == PlanPhase.PLANNING.value:
                blocked = self._blocked_in_planning(tool_name, tool_args)
                if blocked is not None:
                    return ToolDispatchOutcome(
                        status=ToolDispatchStatus.SHORT_CIRCUIT,
                        result=blocked,
                    )

            drift = self._check_execution_plan_drift()
            if drift is not None and not self._is_tool_allowed_during_drift(
                tool_name, tool_args
            ):
                return ToolDispatchOutcome(
                    status=ToolDispatchStatus.SHORT_CIRCUIT,
                    result=drift,
                )

            context = ToolExecutionContext(
                cwd=Path(os.getcwd()).resolve(),
                cancellation_token=self.cancellation_token,
                interruption_manager=self.interruption_manager,
                is_approved=self.is_command_approved,
            )
            preflight = await anyio.to_thread.run_sync(
                tool.prepare_invocation, tool_args, context
            )
            if not isinstance(preflight, ToolPreflightResult):
                preflight = ToolPreflightResult()

            if self.cancellation_token and self.cancellation_token.is_cancelled():
                if self.interruption_manager:
                    self.interruption_manager.set_state(ShellState.NORMAL)
                return ToolDispatchOutcome(
                    status=ToolDispatchStatus.CANCELLED,
                    result=ToolResult(
                        ok=False,
                        output="Operation cancelled during security evaluation",
                    ),
                )

            panel = preflight.panel
            if panel is not None and panel.mode == "info":
                self.emit_event(
                    LLMEventType.TOOL_CONFIRMATION_REQUIRED,
                    self._build_tool_panel_event_data(
                        tool=tool,
                        tool_name=tool_name,
                        tool_args=tool_args,
                        panel=panel,
                    ),
                )

            if preflight.action == ToolPreflightAction.SHORT_CIRCUIT:
                if panel is not None and panel.mode == "blocked":
                    self.emit_event(
                        LLMEventType.TOOL_CONFIRMATION_REQUIRED,
                        self._build_tool_panel_event_data(
                            tool=tool,
                            tool_name=tool_name,
                            tool_args=tool_args,
                            panel=panel,
                        ),
                    )
                return ToolDispatchOutcome(
                    status=ToolDispatchStatus.SHORT_CIRCUIT,
                    result=preflight.result
                    or ToolResult(
                        ok=False,
                        output=f"Tool {tool_name} short-circuited without a result",
                    ),
                )

            if preflight.action == ToolPreflightAction.CONFIRM:
                # Auto-approve writes to the bound plan artifact during
                # planning mode – these are host-managed files similar to
                # memory and should not interrupt the user.
                if (
                    self.plan_state.phase == PlanPhase.PLANNING.value
                    and tool_name in {"write_file", "edit_file"}
                    and self._is_bound_plan_artifact_write(tool_args)
                ):
                    return ToolDispatchOutcome(
                        status=ToolDispatchStatus.EXECUTED,
                        result=await self.execute_tool(tool, tool_name, tool_args),
                    )

                confirm_panel = panel or ToolPanelSpec(mode="confirm")
                goon = self.request_confirmation(
                    LLMEventType.TOOL_CONFIRMATION_REQUIRED,
                    self._build_tool_panel_event_data(
                        tool=tool,
                        tool_name=tool_name,
                        tool_args=tool_args,
                        panel=confirm_panel,
                    ),
                    timeout_seconds=30.0,
                    default_on_timeout=LLMCallbackResult.DENY,
                )

                if goon == LLMCallbackResult.APPROVE:
                    return ToolDispatchOutcome(
                        status=ToolDispatchStatus.EXECUTED,
                        result=await self.execute_tool(tool, tool_name, tool_args),
                    )

                if goon == LLMCallbackResult.DENY:
                    return ToolDispatchOutcome(
                        status=ToolDispatchStatus.REJECTED,
                        result=ToolResult(
                            ok=False,
                            output=(
                                f"Tool {tool_name} execution denied by user, you may "
                                "try another method"
                            ),
                        ),
                    )

                if goon == LLMCallbackResult.CANCEL:
                    return ToolDispatchOutcome(
                        status=ToolDispatchStatus.CANCELLED,
                        result=ToolResult(
                            ok=False,
                            output=f"Tool {tool_name} execution cancelled by user",
                        ),
                    )

                return ToolDispatchOutcome(
                    status=ToolDispatchStatus.REJECTED,
                    result=ToolResult(
                        ok=False,
                        output=f"Invalid confirmation result: {goon}",
                    ),
                )

            return ToolDispatchOutcome(
                status=ToolDispatchStatus.EXECUTED,
                result=await self.execute_tool(tool, tool_name, tool_args),
            )
        except KeyboardInterrupt:
            if self.interruption_manager:
                self.interruption_manager.set_state(ShellState.NORMAL)
            return ToolDispatchOutcome(
                status=ToolDispatchStatus.CANCELLED,
                result=ToolResult(
                    ok=False,
                    output="Operation cancelled by user",
                ),
            )
        except Exception as e:
            return ToolDispatchOutcome(
                status=ToolDispatchStatus.REJECTED,
                result=ToolResult(
                    ok=False,
                    output=str(e),
                    meta={"exception_type": type(e).__name__},
                ),
            )

    def _trim_messages(self, messages: list[dict]) -> list[dict]:
        """Trim messages to keep under token limit"""
        if not self._get_model_provider().should_trim_messages:
            return messages
        old_size = len(messages)
        trim_messages = self._get_trim_messages()
        new_messages = trim_messages(messages, model=self.model)
        new_size = len(new_messages)
        if new_size < old_size:
            print(f"trimmed {old_size - new_size} messages")
        return new_messages

    def _sync_skill_tool_from_manager_if_needed(self) -> None:
        """Invalidate cached tools spec when the skill snapshot changes."""
        current_version = self.skill_manager.skills_version
        if current_version == self._skills_version_for_tools:
            return

        self._skills_version_for_tools = current_version
        self._tools_spec = None

    def _reload_skills_at_safe_point(self) -> None:
        try:
            self.skill_manager.reload_if_dirty()
        except Exception:
            pass
        self._sync_skill_tool_from_manager_if_needed()

    def _build_skills_reminder_message(self) -> Optional[dict]:
        """Build a skills reminder message from current loaded skills snapshot."""
        try:
            skills_reminder_text = render_skills_reminder_text(
                self.skill_manager.to_skill_infos()
            )
        except Exception:
            return None
        return {
            "role": "user",
            "content": (
                f"<system-reminder>\n{skills_reminder_text}\n</system-reminder>"
            ),
        }

    def _inject_runtime_messages(
        self, messages: list[dict], runtime_messages: list[dict]
    ) -> list[dict]:
        if not runtime_messages:
            return messages

        insertion_index = 0
        while (
            insertion_index < len(messages)
            and messages[insertion_index].get("role") == "system"
        ):
            insertion_index += 1

        return (
            messages[:insertion_index]
            + runtime_messages
            + messages[insertion_index:]
        )

    def _get_tools_spec(self) -> list[dict]:
        # Lazy reload: file changes only invalidate; next tool-spec build reloads.
        self._reload_skills_at_safe_point()
        if self._tools_spec is None:
            visible = self._visible_tool_names()
            self._tools_spec = [
                t.to_func_spec() for name, t in self.tools.items() if name in visible
            ]
        return self._tools_spec

    # TODO: refresh tools spec when skills are updated, reserved for future use
    def refresh_tools_spec(self) -> list[dict]:
        self._reload_skills_at_safe_point()
        visible = self._visible_tool_names()
        self._tools_spec = [
            t.to_func_spec() for name, t in self.tools.items() if name in visible
        ]
        return self._tools_spec

    def _get_messages_with_system(
        self, context_manager: ContextManager, system_message: Optional[str]
    ) -> list[dict]:
        self._reload_skills_at_safe_point()
        effective_system_message = system_message
        if self.memory_manager is not None:
            memory_prompt = self.memory_manager.get_system_prompt_section()
            if effective_system_message:
                effective_system_message = (
                    f"{effective_system_message}\n\n{memory_prompt}"
                )
            else:
                effective_system_message = memory_prompt
        messages = context_manager.as_messages()
        if effective_system_message:
            if messages and messages[0]["role"] == "system":
                # Merge: keep knowledge context, append system prompt
                existing = messages[0]["content"]
                messages[0]["content"] = f"{existing}\n\n{effective_system_message}"
            else:
                messages.insert(
                    0, {"role": "system", "content": effective_system_message}
                )
        reminder = self._build_skills_reminder_message()
        runtime_messages = self._plan_runtime_messages()
        if reminder is not None:
            runtime_messages.append(reminder)
        if runtime_messages:
            messages = self._inject_runtime_messages(messages, runtime_messages)
        return messages

    async def _handle_tool_calls(
        self,
        tool_calls: list[dict],
        context_manager: ContextManager,
        system_message: Optional[str],
        output: str,
    ) -> tuple[bool, str, list[dict]]:
        tool_call_cancelled = False
        messages = self._get_messages_with_system(context_manager, system_message)

        for tool_call in tool_calls:
            # Cancellation is handled structurally by CancelScope
            tool_name = tool_call["function"]["name"]
            # TODO: For malformed/truncated tool arguments, add a model-side retry flow.
            tool_args = json.loads(tool_call["function"]["arguments"])

            dispatch = await self.pre_execute_tool(tool_name, tool_args)
            tool_result = dispatch.result
            if dispatch.status in {
                ToolDispatchStatus.EXECUTED,
                ToolDispatchStatus.SHORT_CIRCUIT,
            }:
                rendered_result = tool_result.render_for_llm()
                tool_msg = {
                    "role": "tool",
                    "tool_call_id": tool_call["id"],
                    "name": tool_name,
                    "content": rendered_result,
                }
                context_manager.add_memory(MemoryType.LLM, tool_msg)

                for ctx_msg in tool_result.context_messages:
                    context_manager.add_memory(MemoryType.LLM, ctx_msg)

                session_output = self.tools[tool_name].get_session_output(tool_result)
                if session_output is not None:
                    output = session_output

                if tool_result.stop_tool_chain:
                    tool_call_cancelled = True
                    if session_output is None:
                        output = tool_result.output
                    break
            else:
                tool_call_cancelled = True
                rendered_result = tool_result.render_for_llm()
                error_msg = {
                    "role": "tool",
                    "tool_call_id": tool_call["id"],
                    "name": tool_name,
                    "content": f"Error: {rendered_result}",
                }
                context_manager.add_memory(MemoryType.LLM, error_msg)

                if dispatch.status == ToolDispatchStatus.CANCELLED:
                    # 触发 CANCELLED 事件，让 shell 能够显示取消消息
                    self.emit_event(
                        LLMEventType.CANCELLED, {"reason": "tool_cancelled"}
                    )
                    user_msg = {
                        "role": "user",
                        "content": f"Tool {tool_name} execution cancelled by user, wait for user's next request",
                    }
                    context_manager.add_memory(MemoryType.LLM, user_msg)

                output = ""
                break

            messages = self._get_messages_with_system(context_manager, system_message)

        return tool_call_cancelled, output, messages

    async def process_input(
        self,
        prompt: str,
        context_manager: ContextManager,
        system_message: Optional[str] = None,
        history: Optional[list[dict]] = None,
        emit_events: bool = True,
        stream: bool = False,
        **kwargs,
    ) -> str:
        # 确保在首次调用前完成初始化（使用重试机制处理循环导入）
        await self._ensure_initialized_with_retry()

        events = _LLMEventEmitter(self, emit_events)
        events.emit_op_start(operation="process_input", prompt=prompt, stream=stream)

        # Add user prompt to context manager first
        context_manager.add_memory(MemoryType.LLM, {"role": "user", "content": prompt})

        # Get messages from context manager (single source of truth)
        messages = self._get_messages_with_system(context_manager, system_message)

        tools_spec = self._get_tools_spec()
        has_tool_calls = False
        tool_call_count = 0
        output = ""

        try:
            while True:
                # Cancellation is handled structurally by CancelScope

                # Determine generation type based on context
                generation_type = "tool_call" if has_tool_calls else "conversation"
                # Allow callers to pass `stream` via kwargs; explicit arg takes precedence
                merged_kwargs = {**kwargs}
                if "stream" in merged_kwargs:
                    try:
                        stream = bool(merged_kwargs.pop("stream"))
                    except Exception:
                        merged_kwargs.pop("stream")
                actual_stream = stream and self._get_model_provider().supports_streaming

                events.emit_generation_start(
                    generation_type=generation_type, stream=actual_stream
                )

                # Get Langfuse metadata
                langfuse_metadata = self._get_langfuse_metadata(generation_type)

                # Merge with user-provided kwargs
                if langfuse_metadata and "metadata" in merged_kwargs:
                    merged_kwargs["metadata"].update(langfuse_metadata)
                elif langfuse_metadata:
                    merged_kwargs["metadata"] = langfuse_metadata

                messages = self._trim_messages(messages)

                try:
                    # Use AnyIO timeout context for proper cancellation support
                    # Cancellation is handled structurally by CancelScope
                    with anyio.fail_after(300):  # 5 minute timeout
                        # 检查取消令牌，在开始 LLM 请求前
                        if (
                            self.cancellation_token
                            and self.cancellation_token.is_cancelled()
                        ):
                            raise anyio.get_cancelled_exc_class()

                        response = await self._create_completion_response(
                            messages=messages,
                            tools=tools_spec,
                            tool_choice="auto",
                            stream=actual_stream,
                            **merged_kwargs,
                        )

                        if actual_stream:
                            content_acc = ""
                            reasoning_acc = ""
                            stream_chunks: list[object] = []
                            finish_reason = None
                            generation_status = "success"
                            generation_error_message = None
                            content_preview_started = False

                            async for chunk in response:
                                try:
                                    stream_chunks.append(chunk)
                                    choice, delta = _stream_get_choice_delta(chunk)

                                    # Best-effort reasoning extraction (provider-dependent).
                                    reasoning_delta = _stream_get_delta_value(
                                        delta, "reasoning_content"
                                    )
                                    if reasoning_delta is None:
                                        reasoning_delta = _stream_get_delta_value(
                                            delta, "reasoning"
                                        )
                                    if reasoning_delta:
                                        reasoning_acc += str(reasoning_delta)
                                        events.emit_reasoning_delta(
                                            delta=str(reasoning_delta),
                                            accumulated=reasoning_acc,
                                        )

                                    content_delta = _stream_get_delta_value(
                                        delta, "content"
                                    )
                                    if content_delta:
                                        content_acc += str(content_delta)

                                    # Emit CONTENT_DELTA for all content
                                    if content_acc and not content_preview_started:
                                        content_preview_started = True
                                        events.emit_content_delta(
                                            delta=content_acc,
                                            accumulated=content_acc,
                                            is_final=False,
                                        )
                                    elif content_preview_started and content_delta:
                                        events.emit_content_delta(
                                            delta=str(content_delta),
                                            accumulated=content_acc,
                                            is_final=False,
                                        )

                                    finish_reason = (
                                        choice.get("finish_reason")
                                        if isinstance(choice, dict)
                                        else getattr(choice, "finish_reason", None)
                                    )
                                except (
                                    AttributeError,
                                    IndexError,
                                    Exception,
                                ) as e:
                                    events.emit_error(
                                        error_type="streaming_error",
                                        error_message=f"Error processing stream: {e}",
                                        error_details=str(e),
                                    )
                                    generation_status = "error"
                                    generation_error_message = str(e)
                                    break

                            events.emit_reasoning_end()

                            if generation_status == "success":
                                try:
                                    litellm = self._get_litellm()
                                    combined_response = None
                                    if litellm is not None:
                                        stream_chunk_builder = (
                                            self._get_stream_chunk_builder()
                                        )
                                        if stream_chunk_builder is not None:
                                            combined_response = stream_chunk_builder(
                                                chunks=stream_chunks, messages=messages
                                            )
                                    msg = _stream_coerce_message(combined_response)
                                    if not msg:
                                        msg = {
                                            "role": "assistant",
                                            "content": content_acc,
                                        }
                                    tool_calls = msg.get("tool_calls")
                                    if isinstance(tool_calls, list):
                                        missing_ids = []
                                        for index, tool_call in enumerate(tool_calls):
                                            if not isinstance(tool_call, dict):
                                                missing_ids.append(index)
                                                continue
                                            if not tool_call.get("id"):
                                                missing_ids.append(index)
                                        if missing_ids:
                                            raise ValueError(
                                                f"tool_calls missing id at indexes: {missing_ids}"
                                            )
                                    elif msg.get("function_call"):
                                        raise ValueError(
                                            "function_call returned without tool_call id"
                                        )
                                except Exception as e:
                                    events.emit_error(
                                        error_type="stream_chunk_builder_error",
                                        error_message=f"Error building stream chunks: {e}",
                                        error_details=str(e),
                                    )
                                    generation_status = "error"
                                    generation_error_message = str(e)

                            events.emit_generation_end(
                                status=generation_status,
                                finish_reason=finish_reason,
                                error_message=generation_error_message,
                            )

                            if generation_status != "success":
                                output = ""
                                break
                        else:
                            msg = response["choices"][0]["message"]  # type: ignore
                            finish_reason = response["choices"][0]["finish_reason"]  # type: ignore
                except TimeoutError:
                    events.emit_cancelled("llm_timeout")
                    events.emit_generation_end(status="timeout")
                    output = "LLM request timed out"
                    break
                except anyio.get_cancelled_exc_class():
                    events.emit_cancelled("llm_cancelled")
                    events.emit_generation_end(status="cancelled")
                    raise
                except Exception as e:
                    if isinstance(e, Exception) and is_litellm_exception(e):
                        raw_message = str(e) or type(e).__name__
                        events.emit_error(
                            error_type="litellm_error",
                            error_message=raw_message,
                            error_details=redact_secrets(str(e)),
                        )
                        events.emit_generation_end(
                            status="error", error_message=raw_message
                        )
                    else:
                        events.emit_error(
                            error_type="completion_error",
                            error_message=str(e),
                            error_details=redact_secrets(str(e)),
                        )
                        events.emit_generation_end(status="error", error_message=str(e))
                    output = ""
                    break

                # Add assistant message to context manager
                context_manager.add_memory(MemoryType.LLM, msg)

                # Sync messages with context manager after each response
                messages = self._get_messages_with_system(
                    context_manager, system_message
                )

                # 检查是否有工具调用
                tool_calls = msg.get("tool_calls")
                has_tool_calls = tool_calls is not None and len(tool_calls) > 0

                content = msg.get("content")
                if content:
                    if actual_stream:
                        if has_tool_calls and not content_preview_started:
                            events.emit_content_delta(
                                delta=content, accumulated=content, is_final=False
                            )
                        elif not has_tool_calls:
                            output += content
                            events.emit_content_delta(
                                delta=content,
                                accumulated=output,
                                is_final=True,
                            )
                    else:
                        if has_tool_calls:
                            events.emit_content_delta(
                                delta=content, accumulated=content, is_final=False
                            )
                        else:
                            output += content
                            events.emit_content_delta(
                                delta=content,
                                accumulated=output,
                                is_final=True,
                            )

                if has_tool_calls:
                    tool_call_count += 1
                    (
                        tool_call_cancelled,
                        output,
                        messages,
                    ) = await self._handle_tool_calls(
                        tool_calls, context_manager, system_message, output
                    )

                if not actual_stream:
                    events.emit_generation_end(
                        status="success", finish_reason=finish_reason
                    )

                if not has_tool_calls or tool_call_cancelled:
                    break

        except KeyboardInterrupt:
            # Cancel the token for cascading cancellation
            self.cancellation_token.cancel(
                CancellationReason.USER_INTERRUPT, "KeyboardInterrupt received"
            )

            # Emit cancellation event to notify UI about interruption
            events.emit_cancelled("user_interrupt")
            context_manager.add_memory(
                MemoryType.LLM,
                {"role": "user", "content": "reminder: request cancelled by user"},
            )
            output = ""
        finally:
            # Emit operation end event
            events.emit_op_end(result=output)

        return output

    async def completion(
        self,
        prompt: str,
        system_message: Optional[str] = None,
        history: Optional[list[dict]] = None,
        stream: bool = False,
        emit_events: bool = True,
        **kwargs,
    ) -> str:
        # 确保在首次调用前完成初始化（使用重试机制处理循环导入）
        await self._ensure_initialized_with_retry()

        events = _LLMEventEmitter(self, emit_events)
        events.emit_op_start(operation="completion", prompt=prompt, stream=stream)

        messages = history or []

        if system_message:
            # update system message
            if messages and messages[0]["role"] == "system":
                messages[0]["content"] = system_message
            else:
                messages.insert(0, {"role": "system", "content": system_message})

        messages += [{"role": "user", "content": prompt}]

        # Determine generation type and get Langfuse metadata
        generation_type = "conversation"
        langfuse_metadata = self._get_langfuse_metadata(generation_type)

        # Merge with user-provided kwargs
        merged_kwargs = {**kwargs}
        # Allow callers to pass `stream` via kwargs; explicit arg takes precedence
        if "stream" in merged_kwargs:
            try:
                stream = bool(merged_kwargs.pop("stream"))
            except Exception:
                merged_kwargs.pop("stream")
        if langfuse_metadata and "metadata" in merged_kwargs:
            merged_kwargs["metadata"].update(langfuse_metadata)
        elif langfuse_metadata:
            merged_kwargs["metadata"] = langfuse_metadata

        actual_stream = stream and self._get_model_provider().supports_streaming
        events.emit_generation_start(
            generation_type=generation_type, stream=actual_stream
        )

        result = ""
        try:
            # 检查取消令牌，在开始 LLM 请求前
            if self.cancellation_token and self.cancellation_token.is_cancelled():
                raise anyio.get_cancelled_exc_class()

            response = await self._create_completion_response(
                messages=messages,
                stream=actual_stream,
                **merged_kwargs,
            )

            if actual_stream:
                reasoning_acc = ""
                finish_reason = None
                generation_status = "success"
                generation_error_message = None

                async for chunk in response:
                    # Check for cancellation at the start of each iteration
                    if (
                        self.cancellation_token
                        and self.cancellation_token.is_cancelled()
                    ):
                        generation_status = "cancelled"
                        generation_error_message = "User cancelled"
                        break
                    try:
                        # Best-effort reasoning extraction (provider-dependent).
                        reasoning_delta = None
                        try:
                            reasoning_delta = chunk.choices[0].delta.reasoning_content  # type: ignore[attr-defined]
                        except Exception:
                            try:
                                reasoning_delta = chunk.choices[0].delta.reasoning  # type: ignore[attr-defined]
                            except Exception:
                                reasoning_delta = None

                        if reasoning_delta:
                            reasoning_acc += str(reasoning_delta)
                            events.emit_reasoning_delta(
                                delta=str(reasoning_delta),
                                accumulated=reasoning_acc,
                            )

                        content = chunk.choices[0].delta.content  # type: ignore
                        if content:
                            result += content

                        try:
                            finish_reason = chunk.choices[0].finish_reason  # type: ignore[attr-defined]
                        except Exception:
                            pass
                    except (AttributeError, IndexError, Exception) as e:
                        events.emit_error(
                            error_type="streaming_error",
                            error_message=f"Error processing stream: {e}",
                            error_details=str(e),
                        )
                        generation_status = "error"
                        generation_error_message = str(e)
                        break

                events.emit_reasoning_end()

                events.emit_generation_end(
                    status=generation_status,
                    finish_reason=finish_reason,
                    error_message=generation_error_message,
                )
            else:
                # Non-streaming: extract content from the response object
                result = response["choices"][0]["message"].get("content", "")  # type: ignore
                if result:
                    events.emit_content_delta(
                        delta=result, accumulated=result, is_final=True
                    )

                finish_reason = response["choices"][0].get("finish_reason")  # type: ignore
                events.emit_generation_end(
                    status="success", finish_reason=finish_reason
                )

        except anyio.get_cancelled_exc_class():
            events.emit_cancelled("llm_cancelled")
            events.emit_generation_end(status="cancelled")
            raise
        except TimeoutError:
            result = "LLM request timed out"
            events.emit_cancelled("llm_timeout")
            events.emit_generation_end(status="timeout")
        except Exception as e:
            if isinstance(e, Exception) and is_litellm_exception(e):
                raw_message = str(e) or type(e).__name__
                events.emit_error(
                    error_type="litellm_error",
                    error_message=raw_message,
                    error_details=redact_secrets(str(e)),
                )
                events.emit_generation_end(status="error", error_message=raw_message)
            else:
                events.emit_error(
                    error_type="completion_error",
                    error_message=str(e),
                    error_details=redact_secrets(str(e)),
                )
                events.emit_generation_end(status="error", error_message=str(e))
        finally:
            events.emit_op_end(result=result)

        return result

    def cleanup(self):
        """清理资源，特别是 Langfuse"""
        if not self.langfuse_enabled:
            return

        # Reset litellm callbacks to prevent hanging on exit
        litellm = self._get_litellm()
        if litellm is not None:
            try:
                litellm.success_callback = []
                litellm.failure_callback = []
            except Exception:
                pass  # Ignore callback reset errors

        # Fast Langfuse cleanup with aggressive timeout
        try:
            import threading

            from langfuse import Langfuse

            langfuse = Langfuse()

            # Try to flush pending data first (with timeout)
            flush_timeout = 0.3  # 300ms for flush
            flush_success = False

            def flush_with_timeout():
                nonlocal flush_success
                try:
                    langfuse.flush()
                    flush_success = True
                except Exception:
                    pass

            flush_thread = threading.Thread(target=flush_with_timeout, daemon=True)
            flush_thread.start()
            flush_thread.join(timeout=flush_timeout)

            # Quick shutdown with aggressive timeout
            shutdown_timeout = 0.2  # 200ms for shutdown
            shutdown_success = False

            def shutdown_with_timeout():
                nonlocal shutdown_success
                try:
                    langfuse.shutdown()
                    shutdown_success = True
                except Exception:
                    pass

            shutdown_thread = threading.Thread(
                target=shutdown_with_timeout, daemon=True
            )
            shutdown_thread.start()
            shutdown_thread.join(timeout=shutdown_timeout)

            if flush_success and shutdown_success:
                logger.info("Langfuse cleanup completed successfully")
            elif flush_success:
                logger.warning("Langfuse flush completed, shutdown timed out")
            else:
                logger.warning("Langfuse cleanup timed out, forcing exit")

        except Exception:
            logger.exception("Langfuse cleanup error")
            pass  # Ignore all langfuse-related errors to ensure fast exit


if __name__ == "__main__":
    from aish.config import ConfigModel  # Lazy import to avoid circular

    cfg = ConfigModel(model="gpt-3.5-turbo", api_key="test")
    skill_manager = SkillManager()
    skill_manager.load_all_skills()
    llm_session = LLMSession(config=cfg, skill_manager=skill_manager)
