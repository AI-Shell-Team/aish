"""Prompt and interactive UI helpers extracted from AIShell."""

from __future__ import annotations

import sys
import termios
import threading
import time
from typing import Any, Optional

from prompt_toolkit.formatted_text import ANSI
from prompt_toolkit.layout.controls import BufferControl, FormattedTextControl
from rich.panel import Panel

from ..cancellation import CancellationReason
from ..i18n import t
from ..interruption import InterruptAction, PromptConfig, ShellState
from ..llm import LLMCallbackResult, LLMEvent

# Maximum recursion depth to prevent infinite loops
_MAX_RECURSION_DEPTH = 10


async def get_user_input(
    shell: Any, prompt_text: Optional[str] = None, _recursion_depth: int = 0
) -> str:
    """Get user input with the configured prompt.

    Args:
        shell: The shell instance
        prompt_text: Optional custom prompt text
        _recursion_depth: Internal recursion depth counter (do not set externally)

    Returns:
        User input string

    Raises:
        RuntimeError: If maximum recursion depth is exceeded
    """
    # Prevent infinite recursion
    if _recursion_depth >= _MAX_RECURSION_DEPTH:
        raise RuntimeError(
            f"Maximum input recursion depth ({_MAX_RECURSION_DEPTH}) exceeded"
        )

    self = shell
    import threading
    import time

    from prompt_toolkit.filters import Condition
    from prompt_toolkit.key_binding import KeyBindings
    from prompt_toolkit.keys import Keys

    # 获取基本提示文本
    base_prompt = prompt_text or self.get_prompt()

    # Check for CORRECT_PENDING state
    # Show the hint and enable Y key detection
    has_pending_correction = self._pending_error_correction is not None
    if has_pending_correction:
        left_prompt_msg = self.interruption_manager.consume_left_prompt_message()
        if left_prompt_msg:
            # Display the hint
            self.console.print(f"[dim]<{left_prompt_msg}>[/dim]")

    # 检查提示是否超时（如果超时则清除相关状态）
    prompt_message = self.interruption_manager.get_prompt_message()
    if prompt_message is None:
        current_state = self.interruption_manager.state
        if current_state in (ShellState.EXIT_PENDING, ShellState.CLEAR_PENDING):
            self.interruption_manager.clear_prompt()
            self.interruption_manager.set_state(ShellState.NORMAL)

    # 检查是否有待恢复的输入缓冲区
    restored_input = self.interruption_manager.get_and_clear_input_buffer()
    default_text = restored_input if restored_input else ""

    # 设置初始状态（仅在非确认状态下设置）
    current_state = self.interruption_manager.state
    if current_state not in (
        ShellState.CLEAR_PENDING,
        ShellState.EXIT_PENDING,
        ShellState.CORRECT_PENDING,
    ):
        if default_text:
            self.interruption_manager.set_state(ShellState.INPUTTING)
        else:
            self.interruption_manager.set_state(ShellState.NORMAL)

    # 用于存储 app 引用和刷新控制
    app_ref = [None]
    refresh_stop_event = threading.Event()

    # Track whether correction was triggered
    correction_triggered = [False]

    # Create bottom toolbar for TUI mode - shows status bar at bottom
    from prompt_toolkit.application import get_app_or_none
    from prompt_toolkit.formatted_text import HTML, merge_formatted_text

    def get_bottom_toolbar():
        """Get bottom toolbar content for prompt.

        Shows status bar with model, mode, cwd and hint information.
        In non-TUI mode, only shows hint on the right side.
        """
        # Build status bar parts
        parts = []

        # TUI mode: show full status bar
        if self._tui_app is not None:
            status = self._tui_app.state.status
            tui_settings = self._tui_app.tui_settings

            # Model name
            if status.model:
                model_display = status.model
                if len(model_display) > 20:
                    model_display = model_display[:17] + "..."
                parts.append(HTML(f'<style fg="gray">[Model: {model_display}]</style>'))

            # Mode (PTY/AI/PLAN) - unified gray style
            parts.append(HTML(f'<style fg="gray">[Mode: {status.mode}]</style>'))

            # Current working directory
            if status.cwd and tui_settings.show_cwd:
                cwd_display = status.cwd
                if len(cwd_display) > 30:
                    cwd_display = "..." + cwd_display[-27:]
                parts.append(HTML(f'<style fg="gray">[{cwd_display}]</style>'))

            # Plan queue progress (compact) - unified gray style
            plan_queue_state = self._tui_app.get_plan_queue_state()
            if plan_queue_state.is_visible:
                completed, total, percent = plan_queue_state.get_progress_summary()
                if total > 0:
                    parts.append(
                        HTML(
                            f'<style fg="gray">[Plan: {completed}/{total} ({percent}%)]</style>'
                        )
                    )

        # Get hint message (for both TUI and non-TUI mode)
        hint = self.interruption_manager.get_prompt_message()
        if hint is None:
            # Check if user has input - hide hint when typing
            try:
                app = get_app_or_none()
                if app is not None:
                    buffer = app.current_buffer
                    if buffer and len(buffer.document.text) > 0:
                        hint = None  # Hide hint when user is typing
            except Exception:
                pass

            # Show default ai_hint if still None (for both TUI and non-TUI mode)
            if hint is None:
                hint = t("shell.prompt.ai_hint")

        if hint:
            # hint format from interruption_manager is like:
            # <gray>&lt;press Ctrl+C again to exit&gt;</gray>
            # We need to convert this to proper prompt_toolkit format
            # Replace HTML entities with their characters
            hint = hint.replace("&lt;", "<").replace("&gt;", ">")
            # Now hint is: <gray><press Ctrl+C again to exit></gray>
            # But <press...> is not a valid tag, so we need to escape the inner < >
            # Replace style tags with placeholders, escape content, then restore
            if "<gray>" in hint and "</gray>" in hint:
                # Extract the content between <gray> and </gray>
                start = hint.find("<gray>") + 6
                end = hint.find("</gray>")
                content = hint[start:end]
                # Escape the content for XML
                content = content.replace("<", "&lt;").replace(">", "&gt;")
                # Rebuild with proper format
                hint = f'<style fg="gray">{content}</style>'
                parts.append(HTML(hint))
            elif "<" in hint and ">" in hint:
                # Has some other HTML tags, use as-is
                parts.append(HTML(hint))
            else:
                # Plain text
                parts.append(HTML(f'<style fg="gray">[{hint}]</style>'))

        # If no parts, return None
        if not parts:
            return None

        # Merge all parts with space separators
        result = []
        for i, part in enumerate(parts):
            if i > 0:
                result.append(HTML(" "))
            result.append(part)

        return merge_formatted_text(result)

    # 创建后台刷新线程
    def refresh_in_background():
        while not refresh_stop_event.is_set():
            time.sleep(0.1)  # 每 100ms 检查一次
            # 检查提示是否超时
            self.interruption_manager.get_prompt_message()
            # 如果 app 可用，强制刷新
            if app_ref[0] is not None:
                try:
                    app_ref[0].invalidate()
                except Exception:
                    # Ignore exceptions during invalidate (app may be closed)
                    pass

    # 启动后台刷新线程
    refresh_thread = threading.Thread(target=refresh_in_background, daemon=True)
    refresh_thread.start()

    # 创建 KeyBindings 用于实时按键检测
    kb = KeyBindings()
    key_action = {"action": None, "has_input": False, "input_text": ""}

    @kb.add(Keys.ControlC)
    def _(event):
        """处理 Ctrl+C"""
        # 保存 app 引用
        if app_ref[0] is None:
            app_ref[0] = event.app

        key_action["input_text"] = event.app.current_buffer.document.text
        key_action["has_input"] = len(key_action["input_text"]) > 0

        if key_action["has_input"]:
            # 有输入：清空缓冲区但不退出 prompt
            event.app.current_buffer.reset()
            key_action["action"] = "ctrl_c_cleared"
        else:
            # 无输入：退出以处理确认逻辑
            key_action["action"] = "ctrl_c"
            event.app.exit(exception=EOFError())

    @kb.add(Keys.Escape)
    def _(event):
        """处理 Esc"""
        # 保存 app 引用
        if app_ref[0] is None:
            app_ref[0] = event.app

        key_action["input_text"] = event.app.current_buffer.document.text
        key_action["has_input"] = len(key_action["input_text"]) > 0
        current_state = self.interruption_manager.state

        if not key_action["has_input"]:
            # 无输入：不做任何事，不退出 prompt
            return

        # 在清空确认窗口内，第二次按 Esc 确认清空（不退出 prompt）
        if current_state == ShellState.CLEAR_PENDING:
            event.app.current_buffer.reset()
            self.interruption_manager.set_state(ShellState.NORMAL)
            self.interruption_manager.clear_prompt()
            # 清除保存的输入缓冲区，防止按 Enter 后恢复
            self.interruption_manager._input_buffer = None
            self.interruption_manager._restore_input = False
            event.app.invalidate()
            # 不设置 action，直接返回，这样 prompt 继续等待输入
            return

        # 有输入且非确认状态：显示确认提示（不退出 prompt）
        self.interruption_manager.set_state(ShellState.CLEAR_PENDING)
        self.interruption_manager.show_prompt(
            PromptConfig(message="press esc again to clear", window_seconds=2.0)
        )
        # 保存输入以便在取消时恢复
        self.interruption_manager.save_input_buffer(key_action["input_text"])
        event.app.invalidate()
        # 不设置 action，直接返回，这样 prompt 继续等待输入

    @kb.add("c-d")
    def _(event):
        """其他键取消待确认状态"""
        # 保存 app 引用
        if app_ref[0] is None:
            app_ref[0] = event.app

        if self.interruption_manager.state in (
            ShellState.EXIT_PENDING,
            ShellState.CLEAR_PENDING,
        ):
            self.interruption_manager.handle_other_key()
            self.interruption_manager.clear_prompt()
            event.app.invalidate()

    # 添加一个通用的事件处理器来捕获 app 引用
    # 使用一个很少使用的键来捕获 app 引用
    @kb.add("c-f12")  # Ctrl+F12，用户几乎不会使用
    def capture_app_ref(event):
        if app_ref[0] is None:
            app_ref[0] = event.app

    # Handle semicolon key for error correction when there's a pending correction
    if has_pending_correction:
        # Create a filter that checks if we still have pending correction
        def has_correction_filter():
            return self._pending_error_correction is not None

        @kb.add(";", filter=Condition(has_correction_filter))
        def handle_correction_key(event):
            """Trigger error correction when semicolon is pressed at line start."""
            if app_ref[0] is None:
                app_ref[0] = event.app
            # Only trigger when cursor is at line start (first character is semicolon)
            # Does not affect semicolon input at other positions
            buffer = event.app.current_buffer
            if buffer.cursor_position == 0:
                correction_triggered[0] = True
                event.app.exit(result="__CORRECT_SEMICOLON__")
            else:
                # Not at line start, insert semicolon normally
                buffer.insert_text(";")

        @kb.add("；", filter=Condition(has_correction_filter))
        def handle_correction_key_cn(event):
            """Trigger error correction when Chinese semicolon is pressed at line start."""
            if app_ref[0] is None:
                app_ref[0] = event.app
            buffer = event.app.current_buffer
            if buffer.cursor_position == 0:
                correction_triggered[0] = True
                event.app.exit(result="__CORRECT_SEMICOLON__")
            else:
                # Not at line start, insert Chinese semicolon normally
                buffer.insert_text("；")

    try:
        # Wrap prompt in ANSI to properly handle escape sequences from custom prompts
        formatted_prompt = ANSI(base_prompt)
        result = await self.session.prompt_async(
            formatted_prompt,
            handle_sigint=False,
            key_bindings=kb,
            bottom_toolbar=get_bottom_toolbar,
            default=default_text,
        )
        if result is None:
            return ""

        # Check if correction was triggered (user pressed Y)
        if correction_triggered[0]:
            return result

        # If we had pending correction but user didn't press Y, clear it
        if self._pending_error_correction is not None:
            self._pending_error_correction = None

        # 检查是否在确认窗口期内按了其他键
        if self.interruption_manager.state in (
            ShellState.EXIT_PENDING,
            ShellState.CLEAR_PENDING,
        ):
            self.interruption_manager.clear_prompt()
            self.interruption_manager.handle_other_key()

        return result
    except EOFError:
        action = key_action.get("action")
        has_input = key_action.get("has_input", False)

        if action == "ctrl_c_cleared":
            return await self.get_user_input(prompt_text, _recursion_depth + 1)

        elif action == "ctrl_c":
            interrupt_action = self.interruption_manager.handle_ctrl_c(has_input)
            if interrupt_action == InterruptAction.CLEAR_INPUT:
                return ""
            elif interrupt_action == InterruptAction.CONFIRM_EXIT:
                self._user_requested_exit = True
                raise KeyboardInterrupt()
            elif interrupt_action == InterruptAction.REQUEST_EXIT:
                return await self.get_user_input(prompt_text, _recursion_depth + 1)

        # If action is None (unexpected EOFError), re-raise to handle properly
        if action is None:
            raise

        return await self.get_user_input(prompt_text, _recursion_depth + 1)
    except KeyboardInterrupt:
        raise
    finally:
        # Stop the background refresh thread before leaving the prompt.
        refresh_stop_event.set()


def handle_tool_confirmation_required(shell: Any, event: LLMEvent) -> LLMCallbackResult:
    """Handle tool confirmation required event."""
    self = shell
    # Stop any active Live/animation before prompting.
    # Otherwise (especially in system_diagnose_agent) the spinner can visually
    # override the confirmation panel/prompt, making it look like we didn't
    # wait for user input.
    self._stop_animation()
    if self.current_live:
        try:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
        finally:
            self.current_live = None

    self._finalize_content_preview()
    data = event.data

    panel_mode = str(data.get("panel_mode", "confirm")).lower()

    # Display confirmation/security notice using rich formatting
    self._display_security_panel(data, panel_mode=panel_mode)

    # Only "confirm" requires interactive user input
    if panel_mode == "confirm":
        tool_name = str(data.get("tool_name", ""))
        remember_command = data.get("command")
        allow_remember = bool(remember_command) and tool_name == "bash_exec"
        return self._get_user_confirmation(
            remember_command=remember_command,
            allow_remember=allow_remember,
        )
    return LLMCallbackResult.CONTINUE


def handle_ask_user_required(shell: Any, event: LLMEvent) -> LLMCallbackResult:
    """Handle ask_user event - show interactive single-choice UI."""
    self = shell
    # Stop any active Live/animation before prompting.
    self._stop_animation()
    if self.current_live:
        try:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
        finally:
            self.current_live = None

    self._finalize_content_preview()

    data = event.data or {}
    prompt = str(data.get("prompt") or "")
    options = data.get("options") if isinstance(data.get("options"), list) else []
    default_value = data.get("default")
    if not isinstance(default_value, str):
        default_value = None
    title = str(data.get("title") or t("shell.ask_user.title"))
    allow_cancel = bool(data.get("allow_cancel", True))
    allow_custom_input = bool(data.get("allow_custom_input", False))
    custom_label_raw = data.get("custom_label")
    custom_label = str(custom_label_raw or t("shell.ask_user.custom_label"))
    if isinstance(custom_label_raw, str) and custom_label_raw.strip():
        label_text = custom_label_raw.strip()
    else:
        label_text = custom_label.strip()

    values: list[tuple[str, str]] = []
    for item in options:
        if not isinstance(item, dict):
            continue
        value = item.get("value")
        label = item.get("label")
        if isinstance(value, str) and value and isinstance(label, str) and label:
            values.append((value, label))

    selected_value: str | None = None
    try:
        from functools import partial

        from prompt_toolkit import Application
        from prompt_toolkit.buffer import Buffer
        from prompt_toolkit.key_binding import KeyBindings, merge_key_bindings
        from prompt_toolkit.key_binding.defaults import load_key_bindings
        from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
        from prompt_toolkit.layout.containers import Float, FloatContainer
        from prompt_toolkit.layout.dimension import D
        from prompt_toolkit.styles import Style
        from prompt_toolkit.utils import get_cwidth
        from prompt_toolkit.widgets import Box, RadioList

        # Flush any pending key presses (e.g., the Enter used to submit the last prompt)
        # to avoid instantly selecting/exiting the dialog.
        try:
            if sys.stdin.isatty():
                termios.tcflush(sys.stdin.fileno(), termios.TCIFLUSH)
        except Exception:
            pass

        radio_list = RadioList(values=values, default=default_value)
        custom_buffer = Buffer() if allow_custom_input else None

        kb = KeyBindings()

        @kb.add("tab", eager=True)
        def _next(event):
            if allow_custom_input and custom_input_field is not None:
                if event.app.layout.has_focus(custom_input_field):
                    event.app.layout.focus(radio_list)
                else:
                    event.app.layout.focus(custom_input_field)

        @kb.add("s-tab", eager=True)
        def _prev(event):
            if allow_custom_input and custom_input_field is not None:
                if event.app.layout.has_focus(custom_input_field):
                    event.app.layout.focus(radio_list)
                else:
                    event.app.layout.focus(custom_input_field)

        @kb.add("enter", eager=True)
        def _select(event):
            if (
                allow_custom_input
                and custom_buffer is not None
                and custom_input_field is not None
            ):
                if event.app.layout.has_focus(custom_input_field):
                    text_value = (custom_buffer.text or "").strip()
                    if text_value:
                        event.app.exit(result=text_value)
                        return
            # Ensure the currently highlighted item becomes the current value.
            try:
                radio_list._handle_enter()
            except Exception:
                pass
            event.app.exit(result=radio_list.current_value)

        if allow_cancel:

            @kb.add("escape", eager=True)
            def _cancel(event):
                event.app.exit(result=None)

        prompt_window = Window(
            content=FormattedTextControl(text=prompt),
            wrap_lines=True,
            dont_extend_height=True,
        )

        def _current_max_visible() -> int:
            return self._compute_ask_user_max_visible(
                total_options=len(values),
                term_rows=(self._read_terminal_size() or (24, 80))[0],
                allow_custom_input=allow_custom_input,
            )

        options_window = Box(
            body=radio_list,
            height=lambda: D(
                min=3,
                preferred=_current_max_visible(),
                max=_current_max_visible(),
            ),
        )

        custom_input_label_window = None
        custom_input_field = None
        if allow_custom_input and custom_buffer is not None:
            term_cols = (self._read_terminal_size() or (24, 80))[1]
            max_label_width = max(8, term_cols // 3)
            label_width = min(get_cwidth(label_text) + 1, max_label_width)
            custom_input_label_window = Window(
                content=FormattedTextControl(
                    text=f"{label_text} ", style="class:input.label"
                ),
                dont_extend_height=True,
                width=D(preferred=label_width, min=label_width),
            )
            custom_input_field = Window(
                content=BufferControl(buffer=custom_buffer, focusable=True),
                height=1,
                style="class:input",
            )
            custom_row = HSplit(
                [
                    VSplit([custom_input_label_window, custom_input_field], padding=1),
                ],
                padding=0,
            )

        hint_text = (
            t("shell.ask_user.hint_custom")
            if allow_custom_input
            else t("shell.ask_user.hint_select")
        )
        hint_window = Window(
            content=FormattedTextControl(text=hint_text, style="class:hint"),
            wrap_lines=True,
            dont_extend_height=True,
        )

        body_items = [prompt_window, options_window]
        if allow_custom_input and custom_buffer is not None:
            body_items.append(custom_row)
        body_items.append(hint_window)

        body = HSplit(body_items, padding=1)

        rounded = {
            "tl": "╭",
            "tr": "╮",
            "bl": "╰",
            "br": "╯",
            "h": "─",
            "v": "│",
        }
        fill = partial(Window, style="class:frame.border")

        top_row_with_title = VSplit(
            [
                fill(width=1, height=1, char=rounded["tl"]),
                fill(char=rounded["h"]),
                fill(width=1, height=1, char=rounded["v"]),
                Window(
                    FormattedTextControl(lambda: f" {title} "),
                    style="class:frame.label",
                    dont_extend_width=True,
                ),
                fill(width=1, height=1, char=rounded["v"]),
                fill(char=rounded["h"]),
                fill(width=1, height=1, char=rounded["tr"]),
            ],
            height=1,
        )

        top_row_without_title = VSplit(
            [
                fill(width=1, height=1, char=rounded["tl"]),
                fill(char=rounded["h"]),
                fill(width=1, height=1, char=rounded["tr"]),
            ],
            height=1,
        )

        top_row = top_row_with_title if title else top_row_without_title

        middle_row = VSplit(
            [
                fill(width=1, char=rounded["v"]),
                body,
                fill(width=1, char=rounded["v"]),
            ]
        )

        bottom_row = VSplit(
            [
                fill(width=1, height=1, char=rounded["bl"]),
                fill(char=rounded["h"]),
                fill(width=1, height=1, char=rounded["br"]),
            ],
            height=1,
        )

        frame_container = HSplit(
            [top_row, middle_row, bottom_row],
            style="class:frame",
        )

        style = Style.from_dict(
            {
                "frame.border": "#5f5f5f",
                "frame.label": "bold #7aa2f7",
                "radio-list": "fg:#c0caf5",
                "radio-selected": "reverse",
                "radio-checked": "bold #7dcfff",
                "hint": "fg:#7a8499",
                "input.label": "fg:#9aa5ce",
                "input": "fg:#c0caf5",
            }
        )

        focus_target = radio_list
        if allow_custom_input and custom_input_field is not None:
            focus_target = custom_input_field

        def _line_count(text: str, width: int) -> int:
            if width <= 0:
                return 1
            count = 1
            current = 0
            for ch in text:
                if ch == "\n":
                    count += 1
                    current = 0
                    continue
                w = get_cwidth(ch)
                if current + w > width:
                    count += 1
                    current = w
                else:
                    current += w
            return max(1, count)

        def _dialog_height() -> int:
            rows, cols = self._read_terminal_size() or (24, 80)
            max_visible = self._compute_ask_user_max_visible(
                total_options=len(values),
                term_rows=rows,
                allow_custom_input=allow_custom_input,
            )
            inner_width = max(20, cols - 4)
            prompt_lines = _line_count(prompt, inner_width)
            hint_lines = _line_count(hint_text, inner_width)
            custom_row_lines = 1 if allow_custom_input else 0
            item_count = 2 + (1 if allow_custom_input else 0) + 1
            padding_lines = max(0, item_count - 1)
            min_height = (
                prompt_lines
                + max_visible
                + custom_row_lines
                + hint_lines
                + padding_lines
                + 2
            )
            return max(6, min(rows, min_height))

        # Overlay the dialog on top of existing content to avoid line shifts on every keypress.
        overlay = FloatContainer(
            content=Window(),
            floats=[
                Float(
                    content=frame_container,
                    left=0,
                    right=0,
                    top=0,
                    height=_dialog_height,
                    transparent=False,
                )
            ],
        )

        app = Application(
            layout=Layout(overlay, focused_element=focus_target),
            key_bindings=merge_key_bindings([load_key_bindings(), kb]),
            full_screen=False,
            style=style,
            mouse_support=True,
        )
        try:
            app.input.flush()
            app.input.flush_keys()
        except Exception:
            pass

        resize_stop_event = threading.Event()
        if self._is_ui_resize_enabled():

            def _watch_ask_user_resize() -> None:
                last_size = self._read_terminal_size()
                while not resize_stop_event.is_set():
                    time.sleep(0.1)
                    current_size = self._read_terminal_size()
                    if current_size is None or current_size == last_size:
                        continue
                    last_size = current_size
                    try:
                        app.invalidate()
                    except Exception:
                        pass

            threading.Thread(
                target=_watch_ask_user_resize,
                daemon=True,
            ).start()

        try:
            while True:
                selected_value = app.run(in_thread=True)
                if selected_value is None and not allow_cancel:
                    continue
                break
        finally:
            resize_stop_event.set()
    except KeyboardInterrupt:
        raise
    except Exception:
        selected_value = None

    # Mutate event.data so LLMSession can read the selection without changing callback return types.
    try:
        data["selected_value"] = selected_value
    except Exception:
        pass

    return LLMCallbackResult.CONTINUE


def handle_ask_user_required_inline(shell: Any, event: LLMEvent) -> LLMCallbackResult:
    """Handle ask_user event with status bar style UI.

    This displays a multi-line selection UI at the bottom of the screen,
    styled like the status bar with gray background.
    """
    self = shell

    # Guard: check if data already has selected_value (prevents duplicate calls)
    if "selected_value" in event.data and event.data["selected_value"] is not None:
        return LLMCallbackResult.CONTINUE

    # Stop any active Live/animation before prompting.
    self._stop_animation()
    if self.current_live:
        try:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
        finally:
            self.current_live = None

    self._finalize_content_preview()

    data = event.data or {}
    prompt = str(data.get("prompt") or "")
    options = data.get("options") if isinstance(data.get("options"), list) else []
    default_value = data.get("default")
    if not isinstance(default_value, str):
        default_value = None
    title = str(data.get("title") or t("shell.ask_user.title"))
    allow_cancel = bool(data.get("allow_cancel", True))

    # Normalize options
    values: list[tuple[str, str]] = []
    for item in options:
        if not isinstance(item, dict):
            continue
        value = item.get("value")
        label = item.get("label")
        if isinstance(value, str) and value and isinstance(label, str) and label:
            values.append((value, label))

    if not values:
        data["selected_value"] = None
        return LLMCallbackResult.CONTINUE

    # Set initial selection
    selected_index = 0
    if default_value:
        for i, (v, _) in enumerate(values):
            if v == default_value:
                selected_index = i
                break

    selected_value: str | None = None

    try:
        from prompt_toolkit import Application
        from prompt_toolkit.formatted_text import HTML
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.layout import HSplit, Layout, Window
        from prompt_toolkit.layout.controls import FormattedTextControl
        from prompt_toolkit.styles import Style

        # Flush any pending key presses
        try:
            if sys.stdin.isatty():
                termios.tcflush(sys.stdin.fileno(), termios.TCIFLUSH)
        except Exception:
            pass

        # Selection state
        state = {"index": selected_index}

        kb = KeyBindings()

        @kb.add("up", eager=True)
        def _up(event):
            state["index"] = max(0, state["index"] - 1)
            event.app.invalidate()

        @kb.add("down", eager=True)
        def _down(event):
            state["index"] = min(len(values) - 1, state["index"] + 1)
            event.app.invalidate()

        @kb.add("enter", eager=True)
        def _select(event):
            event.app.exit(result=values[state["index"]][0])

        if allow_cancel:

            @kb.add("escape", eager=True)
            def _cancel(event):
                event.app.exit(result=None)

        def get_content():
            """Build status bar style content for selection UI."""
            import html as html_module
            from prompt_toolkit.formatted_text import merge_formatted_text

            lines = []

            # Title line with status bar style
            title_text = title or prompt
            if title_text:
                # Escape special characters for HTML
                safe_title = html_module.escape(title_text)
                lines.append(HTML(f'<style fg="cyan" bg="ansiblack">{safe_title}</style>\n'))

            # Option lines with status bar style (gray background)
            for idx, (value, label) in enumerate(values):
                # Escape special characters for HTML
                safe_label = html_module.escape(label)
                if idx == state["index"]:
                    # Selected: cyan foreground, bold, with arrow
                    lines.append(
                        HTML(f'<style fg="cyan" bg="ansiblack" bold="">  ➤ {safe_label}</style>\n')
                    )
                else:
                    # Unselected: dim foreground
                    lines.append(HTML(f'<style fg="gray" bg="ansiblack">    {safe_label}</style>\n'))

            # Hint line with status bar style
            hint_parts = ["  "]
            hint_parts.append("↑↓ Select")
            hint_parts.append(" · Enter Confirm")
            if allow_cancel:
                hint_parts.append(" · Esc Cancel")
            hint = "".join(hint_parts)
            lines.append(HTML(f'<style fg="gray" bg="ansiblack">{hint}</style>'))

            return merge_formatted_text(lines)

        # Create a dynamic control that updates on each render
        control = FormattedTextControl(get_content)
        window = Window(content=control, dont_extend_height=True, style="class:status-bar")
        root = HSplit([window])

        # Style with status bar appearance
        style = Style.from_dict(
            {
                "status-bar": "bg:#1e1e1e",
            }
        )

        app = Application(
            layout=Layout(root),
            key_bindings=kb,
            full_screen=False,
            style=style,
        )

        try:
            app.input.flush()
            app.input.flush_keys()
        except Exception:
            pass

        selected_value = app.run(in_thread=True)

    except KeyboardInterrupt:
        raise
    except Exception:
        selected_value = None

    # Mutate event.data so LLMSession can read the selection
    try:
        event.data["selected_value"] = selected_value
    except Exception:
        pass

    return LLMCallbackResult.CONTINUE


def display_security_panel(shell: Any, data: dict, panel_mode: str = "confirm") -> None:
    """Display rich security panel for AI tool calls."""
    self = shell
    panel_mode = str(panel_mode).lower()
    is_blocked = panel_mode == "blocked"
    is_info = panel_mode == "info"

    tool_name = str(data.get("tool_name", "unknown"))
    security_analysis = data.get("security_analysis", {})

    def _sandbox_reason_value(analysis: object) -> str:
        if not isinstance(analysis, dict):
            return ""
        sandbox_info = analysis.get("sandbox")
        if not isinstance(sandbox_info, dict):
            return ""
        return str(sandbox_info.get("reason") or "")

    def _is_sandbox_enabled(analysis: object) -> bool:
        if not isinstance(analysis, dict):
            return True
        sandbox_info = analysis.get("sandbox")
        return not (
            isinstance(sandbox_info, dict) and sandbox_info.get("enabled") is False
        )

    def _risk_level_value(analysis: object) -> str:
        if not isinstance(analysis, dict):
            return "UNKNOWN"
        return str(analysis.get("risk_level", "UNKNOWN"))

    sandbox_enabled = _is_sandbox_enabled(security_analysis)
    sandbox_reason = _sandbox_reason_value(security_analysis)
    risk_level = _risk_level_value(security_analysis)
    risk_level_upper = risk_level.upper()
    fallback_rule_matched = bool(
        security_analysis.get("fallback_rule_matched")
        if isinstance(security_analysis, dict)
        else False
    )
    fallback_mode = (
        str(security_analysis.get("mode", ""))
        if isinstance(security_analysis, dict)
        else ""
    )

    # UX: for low-risk notices, do not show the security panel at all.
    # Keep confirm/blocked panels so users still have context when needed.
    if is_info and risk_level_upper == "LOW":
        return

    risk_color = {
        "LOW": "green",
        "MEDIUM": "yellow",
        "HIGH": "red",
        "CRITICAL": "red",
    }.get(risk_level_upper, "white")

    if is_blocked:
        title = t("shell.security.title.blocked", tool=tool_name)
        border_style = "red"
    elif is_info:
        title = t("shell.security.title.notice", tool=tool_name)
        border_style = "green" if risk_level_upper == "LOW" else "yellow"
    else:
        title = t("shell.security.title.confirm", tool=tool_name)
        border_style = "yellow"

    content: list[str] = []

    if "command" in data:
        content.append(
            f"[bold]{t('shell.security.label.command')}:[/bold] {data['command']}"
        )

    # Fallback hint: sandbox failed to assess the command, so we cannot determine risk.
    # In this case we ask users to confirm before executing the real command.
    if (
        panel_mode == "confirm"
        and security_analysis
        and not sandbox_enabled
        and not fallback_rule_matched
        and fallback_mode != "command_fallback"
    ):
        if sandbox_reason == "sandbox_execute_failed":
            hint = t("shell.security.fallback.sandbox_execute_failed")
        elif sandbox_reason == "sandbox_timeout":
            hint = t("shell.security.fallback.sandbox_timeout")
        elif sandbox_reason == "sandbox_ipc_timeout":
            hint = t("shell.security.fallback.sandbox_ipc_timeout")
        else:
            hint = t("shell.security.fallback.generic")
        content.append(
            f"[bold]{t('shell.security.label.fallback_hint')}:[/bold] {hint}"
        )

    # For non-bash tools (e.g. write_file), tool-specific confirmation info is carried
    # in generic fields like tool_args/content_preview/content_length.
    tool_args = data.get("tool_args")
    if isinstance(tool_args, dict):
        file_path = tool_args.get("file_path") or tool_args.get("path")
        if file_path:
            content.append(
                f"[bold]{t('shell.security.label.target')}:[/bold] {file_path}"
            )

        if "content" in tool_args:
            raw_content = tool_args.get("content")

            # Prefer the tool-provided preview; otherwise derive a safe preview.
            content_preview = data.get("content_preview")
            if tool_name == "write_file" and isinstance(raw_content, str):
                content_preview = raw_content
            elif content_preview is None and isinstance(raw_content, str):
                content_preview = (
                    raw_content[:100] + "..." if len(raw_content) > 100 else raw_content
                )

            if content_preview is not None:
                content.append(
                    f"[bold]{t('shell.security.label.content_preview')}:[/bold] {content_preview}"
                )

    if security_analysis and (sandbox_enabled or fallback_rule_matched):
        is_low_risk = risk_level_upper == "LOW"
        show_risk_details = is_blocked or (not is_low_risk)

        if show_risk_details:
            content.append(
                f"[bold]{t('shell.security.label.risk_level')}:[/bold] [{risk_color}]{risk_level}[/{risk_color}]"
            )

            reasons = (
                security_analysis.get("reasons", [])
                if isinstance(security_analysis, dict)
                else []
            )
            if reasons or is_blocked:
                content.append(f"[bold]{t('shell.security.label.reasons')}:[/bold]")
                if reasons:
                    for reason in reasons[:3]:
                        content.append(f"  • {reason}")
                else:
                    content.append(
                        f"  [dim]{t('shell.security.label.no_additional_info')}[/dim]"
                    )

            alternatives = (
                security_analysis.get("suggested_alternatives", [])
                if isinstance(security_analysis, dict)
                else []
            )
            if alternatives:
                content.append(
                    f"[bold]{t('shell.security.label.alternatives')}:[/bold]"
                )
                for alt in alternatives[:2]:
                    content.append(f"  💡 {alt}")

            impact_description = (
                security_analysis.get("impact_description", "")
                if isinstance(security_analysis, dict)
                else ""
            )
            if impact_description:
                content.append(
                    f"[bold]{t('shell.security.label.impact')}:[/bold] {impact_description}"
                )

    # Last resort: avoid empty confirmation panels.
    if not content:
        content.append(f"[dim]{t('shell.security.label.no_additional_info')}[/dim]")

    panel_content = "\n".join(content)
    self.console.print(Panel(panel_content, title=title, border_style=border_style))


def get_user_confirmation(
    shell: Any,
    remember_command: Optional[str] = None,
    allow_remember: bool = False,
) -> LLMCallbackResult:
    """Get interactive confirmation from user."""
    self = shell
    """Get interactive confirmation from user"""
    self.console.print(f"\n[bold]{t('shell.security.options_header')}:[/bold]")
    self.console.print(f"  [green]y[/green] - {t('shell.security.option.approve')}")
    if allow_remember:
        self.console.print(
            f"  [green]a[/green] - {t('shell.security.option.approve_remember')}"
        )
    self.console.print(f"  [blue]n/c[/blue] - {t('shell.security.option.cancel')}")

    try:
        import sys
        import termios
        import tty

        self.console.print("\n" + t("shell.security.choice_prompt"), end="")
        sys.stdout.flush()

        # Save terminal settings
        fd = sys.stdin.fileno()
        old_settings = termios.tcgetattr(fd)

        try:
            tty.setraw(sys.stdin.fileno())
            char = sys.stdin.read(1)
            # Check for Ctrl+C (ASCII 3)
            if char == "\x03":
                raise KeyboardInterrupt()
            char = char.lower()
        finally:
            termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)

        # Print feedback and return result
        if char == "a":
            if allow_remember and remember_command:
                print("a")
                self._remember_approved_command(str(remember_command))
                return LLMCallbackResult.APPROVE
            # If "a" isn't offered, treat it as cancel (safe default).
            print("n")
            return LLMCallbackResult.CANCEL
        if char == "y":
            print("y")
            return LLMCallbackResult.APPROVE
        elif char in ["\n", "\r", "n", "c"]:
            if char not in ["\n", "\r"]:
                print(char)
            else:
                print("n")
            return LLMCallbackResult.CANCEL
        else:
            print(char)
            return LLMCallbackResult.CANCEL

    except KeyboardInterrupt:
        # Handle Ctrl+C explicitly - trigger global cancellation
        # 瞬时提示会在 handle_processing_cancelled 中显示
        # 触发全局取消，中断 AI 操作
        self.llm_session.cancellation_token.cancel(
            CancellationReason.USER_INTERRUPT, "User cancelled during tool confirmation"
        )
        return LLMCallbackResult.CANCEL
    except EOFError:
        # Handle EOF separately - this might be from terminal issues, not user intent
        self.console.print("\n[yellow]Input interrupted, cancelling[/yellow]")
        return LLMCallbackResult.CANCEL
    except Exception:
        self.console.print("\n[yellow]Input error, defaulting to cancel[/yellow]")
        return LLMCallbackResult.CANCEL
