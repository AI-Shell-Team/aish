"""
中断管理系统 - 处理 Ctrl+C 和 Esc 中断机制

支持的中断场景：
1. Shell 空闲状态 - 退出确认
2. Shell 输入状态 - 清空输入确认
3. AI 执行状态 - 中断 AI 并恢复输入
4. 命令执行状态 - 中断命令
"""

import threading
import time
from dataclasses import dataclass
from enum import Enum
from typing import Callable, Optional


class ShellState(Enum):
    """Shell 状态枚举"""

    NORMAL = "normal"  # 普通空闲状态
    INPUTTING = "inputting"  # 用户正在输入
    EXIT_PENDING = "exit_pending"  # 退出待确认（Ctrl+C 窗口期）
    CLEAR_PENDING = "clear_pending"  # 清空输入待确认（Esc 窗口期）
    CORRECT_PENDING = "correct_pending"  # 纠错待确认（命令执行失败后）

    # AI 执行状态
    AI_THINKING = "ai_thinking"  # AI 推理中
    SANDBOX_EVAL = "sandbox_eval"  # 沙箱评估中
    COMMAND_EXEC = "command_exec"  # 命令执行中


class InterruptAction(Enum):
    """中断动作枚举"""

    NONE = "none"  # 无动作
    CLEAR_INPUT = "clear_input"  # 清空输入
    REQUEST_EXIT = "request_exit"  # 请求退出（进入确认窗口）
    CONFIRM_EXIT = "confirm_exit"  # 确认退出
    CANCEL_PENDING = "cancel_pending"  # 取消待确认状态
    INTERRUPT_AI = "interrupt_ai"  # 中断 AI
    INTERRUPT_SANDBOX = "interrupt_sandbox"  # 中断沙箱
    INTERRUPT_COMMAND = "interrupt_command"  # 中断命令执行（等待完成）


@dataclass
class PromptConfig:
    """瞬时提示配置"""

    message: str  # 提示消息（纯文本，不含颜色代码）
    window_seconds: float = 2.0  # 确认窗口时长（秒）


class InterruptionManager:
    """
    中断管理器 - 管理 Shell 状态和中断逻辑

    状态转换图：
    NORMAL <-> INPUTTING <-> CLEAR_PENDING
    NORMAL <-> EXIT_PENDING
    NORMAL -> AI_THINKING/SANDBOX_EVAL/COMMAND_EXEC -> NORMAL
    """

    # 确认窗口时长（秒）
    EXIT_WINDOW = 2.0
    CLEAR_WINDOW = 2.0

    def __init__(self) -> None:
        # 当前状态
        self._state = ShellState.NORMAL
        self._state_start_time: Optional[float] = None
        self._state_lock = threading.Lock()

        # 输入缓冲区（用于 AI 中断后恢复）
        self._input_buffer: Optional[str] = None
        self._restore_input = False

        # 当前显示的提示信息
        self._current_prompt: Optional[PromptConfig] = None
        self._prompt_start_time: Optional[float] = None

        # 中断回调函数
        self._on_interrupt_callback: Optional[Callable] = None

        # 最后的 AI 执行状态（用于显示取消消息）
        self._last_ai_state: Optional[ShellState] = None

        # Atomic dedup for interrupt handling (prevents signal handler
        # and input router from both processing the same interrupt)
        self._interrupt_dedup_event = threading.Event()

    @property
    def state(self) -> ShellState:
        """获取当前状态"""
        with self._state_lock:
            return self._state

    def set_state(self, new_state: ShellState) -> None:
        """切换状态"""
        with self._state_lock:
            old_state = self._state
            self._state = new_state
            self._state_start_time = time.time()

            # 保存最后的 AI 执行状态（用于显示取消消息）
            if new_state in (
                ShellState.AI_THINKING,
                ShellState.SANDBOX_EVAL,
                ShellState.COMMAND_EXEC,
            ):
                self._last_ai_state = new_state
            elif new_state == ShellState.NORMAL and old_state in (
                ShellState.AI_THINKING,
                ShellState.SANDBOX_EVAL,
                ShellState.COMMAND_EXEC,
            ):
                # 从 AI 执行状态恢复到 NORMAL 时，保留 _last_ai_state
                pass

    def get_last_ai_state(self) -> Optional[ShellState]:
        """获取最后的 AI 执行状态（用于显示取消消息）"""
        return self._last_ai_state

    def clear_last_ai_state(self) -> None:
        """清除最后的 AI 执行状态"""
        self._last_ai_state = None

    def is_in_window(self, window_seconds: float) -> bool:
        """检查是否在确认窗口期内"""
        with self._state_lock:
            if self._state_start_time is None:
                return False
            return (time.time() - self._state_start_time) < window_seconds

    def get_prompt_message(self) -> Optional[str]:
        """
        获取当前应该显示的提示消息（右侧 rprompt）

        Returns:
            提示消息（HTML格式，带灰色样式和<>包裹），如果不需要显示则返回 None
        """
        current_state = self.state

        # 优先处理 EXIT_PENDING 和 CLEAR_PENDING 状态（使用 show_prompt 设置的提示）
        if current_state in (ShellState.EXIT_PENDING, ShellState.CLEAR_PENDING):
            if self._current_prompt is None:
                # 提示已被清除，恢复状态
                self.set_state(ShellState.NORMAL)
                return None
            # 检查提示是否超时
            if self._prompt_start_time is not None:
                if (
                    time.time() - self._prompt_start_time
                ) >= self._current_prompt.window_seconds:
                    # 提示超时，清除提示并恢复状态
                    self.clear_prompt()
                    self.set_state(ShellState.NORMAL)
                    return None
            message = self._current_prompt.message
            return f"<gray>&lt;{message}&gt;</gray>"

        # CORRECT_PENDING 状态不显示 rprompt，提示显示在左侧
        if current_state == ShellState.CORRECT_PENDING:
            return None

        # 根据 AI 执行状态返回不同的提示
        if current_state == ShellState.AI_THINKING:
            return "<gray>&lt;Interrupted received.&gt;</gray>"
        elif current_state == ShellState.SANDBOX_EVAL:
            return "<gray>&lt;Stopping... finalizing current task.&gt;</gray>"
        elif current_state == ShellState.COMMAND_EXEC:
            return "<gray>&lt;Stopping... finishing current task (this may take a moment)&gt;</gray>"

        return None

    def consume_left_prompt_message(self) -> Optional[str]:
        """
        获取并清除当前应该显示的左侧提示消息（在提示符前面）

        此方法有副作用：会检查超时并自动清除状态。

        Returns:
            提示消息（纯文本），如果不需要显示则返回 None
        """
        current_state = self.state

        if current_state == ShellState.CORRECT_PENDING:
            if self._current_prompt is None:
                self.set_state(ShellState.NORMAL)
                return None
            # 检查提示是否超时
            if self._prompt_start_time is not None:
                if (
                    time.time() - self._prompt_start_time
                ) >= self._current_prompt.window_seconds:
                    self.clear_prompt()
                    self.set_state(ShellState.NORMAL)
                    return None
            # Get the message and clear the state
            message = self._current_prompt.message
            self.clear_prompt()
            self.set_state(ShellState.NORMAL)
            return message

        return None

    def show_prompt(self, config: PromptConfig) -> None:
        """显示瞬时提示"""
        self._current_prompt = config
        self._prompt_start_time = time.time()

    def clear_prompt(self) -> None:
        """清除瞬时提示"""
        self._current_prompt = None
        self._prompt_start_time = None

    def save_input_buffer(self, input_text: str) -> None:
        """保存输入缓冲区（AI 中断前）"""
        self._input_buffer = input_text
        self._restore_input = True

    def get_and_clear_input_buffer(self) -> Optional[str]:
        """获取并清除保存的输入缓冲区"""
        if not self._restore_input:
            return None
        self._restore_input = False
        input_text = self._input_buffer
        self._input_buffer = None  # 清除缓冲区内容
        return input_text

    def set_interrupt_callback(self, callback: Callable) -> None:
        """设置中断回调函数"""
        self._on_interrupt_callback = callback

    def trigger_interrupt(self) -> None:
        """触发中断（调用回调函数）"""
        if self._on_interrupt_callback:
            self._on_interrupt_callback()

    def try_acquire_interrupt(self) -> bool:
        """Atomically acquire interrupt permission (dedup).

        Returns True if this interrupt should be processed,
        False if it's a duplicate. Auto-resets after 500ms.
        """
        if self._interrupt_dedup_event.is_set():
            return False
        self._interrupt_dedup_event.set()
        t = threading.Timer(0.5, self._interrupt_dedup_event.clear)
        t.daemon = True
        t.start()
        return True

    # ===== Ctrl+C 处理逻辑 =====

    def handle_ctrl_c(self, has_input: bool) -> InterruptAction:
        """
        处理 Ctrl+C 按键

        Args:
            has_input: 当前是否有输入内容

        Returns:
            应该执行的中断动作
        """
        current_state = self.state

        # 如果有输入内容，直接清空
        if has_input:
            self.set_state(ShellState.NORMAL)
            self.clear_prompt()
            return InterruptAction.CLEAR_INPUT

        # 检查提示是否超时（如果超时则重置状态）
        current_prompt = self.get_prompt_message()
        if current_prompt is None and current_state == ShellState.EXIT_PENDING:
            # 提示超时，重置状态为 NORMAL，重新开始计数
            self.set_state(ShellState.NORMAL)
            current_state = ShellState.NORMAL

        # 无输入内容时的处理
        if current_state == ShellState.EXIT_PENDING:
            # 在退出确认窗口内，确认退出
            self.clear_prompt()
            return InterruptAction.CONFIRM_EXIT

        elif current_state in (ShellState.NORMAL, ShellState.CLEAR_PENDING):
            # 进入退出确认窗口
            self.set_state(ShellState.EXIT_PENDING)
            self.show_prompt(
                PromptConfig(
                    message="press Ctrl+C again to exit",
                    window_seconds=self.EXIT_WINDOW,
                )
            )
            return InterruptAction.REQUEST_EXIT

        return InterruptAction.NONE

    # ===== Esc 处理逻辑 =====

    def handle_esc(self, has_input: bool) -> InterruptAction:
        """
        处理 Esc 按键

        Args:
            has_input: 当前是否有输入内容

        Returns:
            应该执行的中断动作
        """
        current_state = self.state

        # 在清空确认窗口内，第二次按 Esc 确认清空（不受 has_input 影响）
        if current_state == ShellState.CLEAR_PENDING:
            self.set_state(ShellState.NORMAL)
            self.clear_prompt()
            return InterruptAction.CLEAR_INPUT

        # 无输入内容时，Esc 无作用
        if not has_input:
            return InterruptAction.NONE

        # 有输入内容时的处理（首次按 Esc）
        if current_state in (
            ShellState.NORMAL,
            ShellState.INPUTTING,
            ShellState.EXIT_PENDING,
        ):
            # 进入清空确认窗口
            self.set_state(ShellState.CLEAR_PENDING)
            self.show_prompt(
                PromptConfig(
                    message="press esc again to clear", window_seconds=self.CLEAR_WINDOW
                )
            )
            return InterruptAction.CANCEL_PENDING

        return InterruptAction.NONE

    # ===== 其他按键处理 =====

    def handle_other_key(self) -> InterruptAction:
        """
        处理其他按键（非 Ctrl+C、非 Esc）

        用于取消待确认状态
        """
        current_state = self.state

        if current_state in (ShellState.EXIT_PENDING, ShellState.CLEAR_PENDING):
            # 取消待确认状态，但不清除提示（让它自然超时）
            self.set_state(ShellState.NORMAL)
            return InterruptAction.CANCEL_PENDING

        return InterruptAction.NONE

    # ===== AI 执行期间的中断 =====

    def handle_ai_interrupt(
        self, save_input: bool = False, input_text: str = ""
    ) -> InterruptAction:
        """
        处理 AI 执行期间的 Ctrl+C 中断

        Args:
            save_input: 是否保存当前输入
            input_text: 要保存的输入文本

        Returns:
            应该执行的中断动作
        """
        if save_input and input_text:
            self.save_input_buffer(input_text)

        self.set_state(ShellState.NORMAL)
        self.trigger_interrupt()
        return InterruptAction.INTERRUPT_AI


# 便捷函数
def create_interruption_manager() -> InterruptionManager:
    """创建中断管理器实例"""
    return InterruptionManager()
