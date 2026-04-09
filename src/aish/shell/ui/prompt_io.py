"""Prompt and interactive UI helpers for the shell core."""

from __future__ import annotations

import sys
import termios
import threading
import time
from typing import Any, Optional

from prompt_toolkit.formatted_text import ANSI
from prompt_toolkit.layout.controls import BufferControl, FormattedTextControl
from rich.panel import Panel

from ...i18n import t
from ...terminal.interaction import (InteractionAnswer, InteractionAnswerType,
                                     InteractionKind,
                                     InteractionRequest, InteractionResponse,
                                     InteractionStatus,
                                     apply_interaction_response_to_data)
from ...state.cancellation import CancellationReason
from ..interruption import InterruptAction, PromptConfig, ShellState
from ...llm import LLMCallbackResult, LLMEvent

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

    # Create bottom toolbar for inline hint display.
    from prompt_toolkit.application import get_app_or_none
    from prompt_toolkit.formatted_text import HTML, merge_formatted_text

    def get_bottom_toolbar():
        """Get bottom toolbar content for prompt.

        Shows prompt hints in the bottom toolbar.
        """
        parts = []

        # Get hint message.
        hint = self.interruption_manager.get_prompt_message()
        if hint is None:
            try:
                app = get_app_or_none()
                if app is not None:
                    buffer = app.current_buffer
                    if buffer and len(buffer.document.text) > 0:
                        hint = None
            except Exception:
                pass

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

        # Clear pending correction if user didn't type only semicolon
        # If they typed only ";" or "；", preserve it for the current shell core to handle
        if (
            self._pending_error_correction is not None
            and result.strip() not in (";", "；")
        ):
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

    panel = data.get("panel") if isinstance(data.get("panel"), dict) else {}
    panel_mode = str(panel.get("mode") or data.get("panel_mode", "confirm")).lower()

    # Display confirmation/security notice using rich formatting
    self._display_security_panel(data, panel_mode=panel_mode)

    # Only "confirm" requires interactive user input
    if panel_mode == "confirm":
        remember_command = panel.get("remember_key", data.get("remember_key"))
        allow_remember = bool(panel.get("allow_remember", data.get("allow_remember")))
        return self._get_user_confirmation(
            remember_command=remember_command,
            allow_remember=allow_remember,
        )
    return LLMCallbackResult.CONTINUE


def _prepare_interaction_prompt(shell: Any) -> None:
    self = shell
    self._stop_animation()
    if self.current_live:
        try:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
        finally:
            self.current_live = None
    self._finalize_content_preview()


def _build_interaction_response(
    request: Any,
    selected_value: str | None,
) -> InteractionResponse:
    if isinstance(selected_value, str) and selected_value:
        selected_option = request.get_option_by_value(selected_value)
        answer = InteractionAnswer(
            type=InteractionAnswerType.OPTION
            if selected_option is not None
            else InteractionAnswerType.TEXT,
            value=selected_option.value if selected_option is not None else selected_value,
            label=selected_option.label if selected_option is not None else selected_value,
        )
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.SUBMITTED,
            answer=answer,
        )
    return InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.CANCELLED,
        reason="cancelled",
    )


def _request_allows_custom_input(request: Any) -> bool:
    return request.kind in (
        InteractionKind.CHOICE_OR_TEXT,
        InteractionKind.TEXT_INPUT,
    )


def render_interaction_modal(shell: Any, request: Any) -> InteractionResponse:
    self = shell
    prompt = request.prompt
    options = [option.to_dict() for option in request.options]
    default_value = request.default
    title = str(request.title or t("shell.ask_user.title"))
    allow_cancel = request.allow_cancel
    allow_custom_input = _request_allows_custom_input(request)
    label_text = str(t("shell.ask_user.custom_label"))
    custom_prompt = request.placeholder
    if request.custom is not None:
        label_text = request.custom.label
        custom_prompt = request.custom.placeholder

    values = [(item["value"], item["label"]) for item in options]
    description_by_value = {
        item["value"]: item.get("description", "") for item in options
    }
    if not values and not allow_custom_input:
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.DISMISSED,
            reason="dismissed",
        )

    selected_index = 0
    if default_value:
        for index, (value, _) in enumerate(values):
            if value == default_value:
                selected_index = index
                break

    selected_value: str | None = None
    try:
        from prompt_toolkit import Application
        from prompt_toolkit.buffer import Buffer
        from prompt_toolkit.filters import Condition
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
        from prompt_toolkit.layout.containers import ConditionalContainer
        from prompt_toolkit.layout.dimension import D
        from prompt_toolkit.styles import Style
        from prompt_toolkit.utils import get_cwidth

        try:
            if sys.stdin.isatty():
                termios.tcflush(sys.stdin.fileno(), termios.TCIFLUSH)
        except Exception:
            pass

        custom_buffer = Buffer() if allow_custom_input else None
        if (
            custom_buffer is not None
            and request.kind == InteractionKind.TEXT_INPUT
            and isinstance(default_value, str)
        ):
            custom_buffer.text = default_value
        state = {
            "selected_index": selected_index,
            "custom_active": allow_custom_input and not values,
        }
        custom_placeholder_text = custom_prompt or label_text

        def _is_custom_selected() -> bool:
            return allow_custom_input and bool(state["custom_active"])

        def _sync_focus(event) -> None:
            if allow_custom_input and custom_input_field is not None and _is_custom_selected():
                event.app.layout.focus(custom_input_field)
            else:
                event.app.layout.focus(options_window)

        def _activate_custom_input(event, seed_text: str | None = None) -> None:
            if not allow_custom_input or custom_buffer is None or custom_input_field is None:
                return

            state["custom_active"] = True
            event.app.layout.focus(custom_input_field)
            if seed_text:
                custom_buffer.insert_text(seed_text)
            event.app.invalidate()

        kb = KeyBindings()

        @kb.add("up", eager=True)
        def _move_up(event):
            if _is_custom_selected():
                if values:
                    state["custom_active"] = False
                    state["selected_index"] = min(max(0, len(values) - 1), state["selected_index"])
                    _sync_focus(event)
                    event.app.invalidate()
                return
            if values:
                state["selected_index"] = max(0, state["selected_index"] - 1)
                _sync_focus(event)
                event.app.invalidate()

        @kb.add("down", eager=True)
        def _move_down(event):
            if _is_custom_selected():
                return
            if values and state["selected_index"] < len(values) - 1:
                state["selected_index"] = min(len(values) - 1, state["selected_index"] + 1)
                _sync_focus(event)
                event.app.invalidate()
                return
            if allow_custom_input:
                state["custom_active"] = True
                _sync_focus(event)
                event.app.invalidate()

        @kb.add(
            "tab",
            eager=True,
            filter=Condition(
                lambda: allow_custom_input
                and custom_input_field is not None
                and not _is_custom_selected()
            ),
        )
        def _focus_custom_input(event):
            _activate_custom_input(event)

        @kb.add(
            "s-tab",
            eager=True,
            filter=Condition(lambda: _is_custom_selected() and bool(values)),
        )
        def _focus_options(event):
            state["custom_active"] = False
            _sync_focus(event)
            event.app.invalidate()

        @kb.add(
            "<any>",
            eager=True,
            filter=Condition(
                lambda: allow_custom_input
                and custom_input_field is not None
                and not _is_custom_selected()
            ),
        )
        def _type_to_custom_input(event):
            text = getattr(event, "data", "")
            if not isinstance(text, str):
                return
            if not text.isprintable() or text in {"\r", "\n", "\t"}:
                return
            _activate_custom_input(event, seed_text=text)

        @kb.add("enter", eager=True)
        def _select(event):
            if allow_custom_input and custom_buffer is not None and _is_custom_selected():
                text_value = (custom_buffer.text or "").strip()
                if text_value:
                    event.app.exit(result=text_value)
                    return
                return
            if values:
                event.app.exit(result=values[state["selected_index"]][0])

        if allow_cancel:

            @kb.add("escape", eager=True)
            def _cancel(event):
                event.app.exit(result=None)

            @kb.add("c-c", eager=True)
            def _cancel_ctrl_c(event):
                event.app.exit(result=None)

        prompt_window = Window(
            content=FormattedTextControl(text=prompt),
            wrap_lines=True,
            dont_extend_height=True,
        )

        def _current_max_visible() -> int:
            return self._compute_ask_user_max_visible(
                total_options=max(1, len(values)),
                term_rows=(self._read_terminal_size() or (24, 80))[0],
                allow_custom_input=allow_custom_input,
            )

        def _regular_visible_range() -> tuple[int, int]:
            total_regular = len(values)
            if total_regular <= 0:
                return (0, 0)

            visible_count = _current_max_visible()
            visible_regular = max(1, visible_count)

            selected_regular = min(state["selected_index"], total_regular - 1)
            start_index = max(0, selected_regular - visible_regular + 1)
            if selected_regular < start_index:
                start_index = selected_regular
            if start_index + visible_regular > total_regular:
                start_index = max(0, total_regular - visible_regular)
            end_index = min(total_regular, start_index + visible_regular)
            return (start_index, end_index)

        def _highlight_visible_option_by_digit(event, digit: int) -> None:
            if not values:
                return

            start_index, end_index = _regular_visible_range()
            visible_values = values[start_index:end_index]
            target_offset = digit - 1
            if target_offset < 0 or target_offset >= len(visible_values):
                return

            state["selected_index"] = start_index + target_offset
            state["custom_active"] = False
            _sync_focus(event)
            event.app.invalidate()

        for digit in range(1, 10):

            @kb.add(
                str(digit),
                eager=True,
                filter=Condition(lambda: not _is_custom_selected()),
            )
            def _select_digit(event, digit=digit):
                _highlight_visible_option_by_digit(event, digit)

        def _build_option_lines() -> list[tuple[str, str]]:
            start_index, end_index = _regular_visible_range()

            fragments: list[tuple[str, str]] = []
            if start_index > 0:
                fragments.append(("class:hint", "  ^ 更多选项\n"))

            for index in range(start_index, end_index):
                is_selected = index == state["selected_index"] and not _is_custom_selected()
                value, label = values[index]
                description = description_by_value.get(value, "")
                prefix = ">" if is_selected else " "
                style = "class:option.selected" if is_selected else "class:option"
                fragments.append((style, f"{prefix} {index + 1}. {label}\n"))
                if description:
                    desc_style = (
                        "class:option.description.selected"
                        if is_selected
                        else "class:option.description"
                    )
                    fragments.append((desc_style, f"      {description}\n"))

            if end_index < len(values):
                fragments.append(("class:hint", "  v 更多选项\n"))

            return fragments

        def _visible_option_rows() -> int:
            start_index, end_index = _regular_visible_range()

            rows = max(0, end_index - start_index)
            rows += sum(
                1
                for index in range(start_index, end_index)
                if description_by_value.get(values[index][0], "")
            )
            if start_index > 0:
                rows += 1
            if end_index < len(values):
                rows += 1
            return rows

        def _separator_text() -> str:
            term_cols = (self._read_terminal_size() or (24, 80))[1]
            return "─" * max(20, term_cols - 1)

        options_window = Window(
            content=FormattedTextControl(
                text=_build_option_lines,
                focusable=True,
                show_cursor=False,
            ),
            wrap_lines=True,
            dont_extend_height=True,
            height=lambda: D(
                min=0,
                preferred=_visible_option_rows(),
                max=_visible_option_rows(),
            ),
        )

        custom_row_window = None
        custom_input_field = None
        if allow_custom_input:
            max_label_width = max(8, (self._read_terminal_size() or (24, 80))[1] // 3)
            label_width = min(max(8, get_cwidth(label_text) + 1), max_label_width)
            _ = label_width
            custom_header_window = Window(
                content=FormattedTextControl(
                    text=lambda: [
                        (
                            "class:option.selected" if _is_custom_selected() else "class:option",
                            f"> {label_text}" if _is_custom_selected() else f"  {label_text}",
                        )
                    ],
                    show_cursor=False,
                ),
                dont_extend_height=True,
                wrap_lines=True,
            )

            custom_body_prefix = Window(
                content=FormattedTextControl(text="    "),
                dont_extend_height=True,
                width=D(preferred=4, min=4, max=4),
            )

            if custom_buffer is not None:
                custom_input_field = Window(
                    content=BufferControl(buffer=custom_buffer, focusable=True),
                    height=1,
                    style="class:input",
                )

            custom_placeholder_body = VSplit(
                [
                    custom_body_prefix,
                    Window(
                        content=FormattedTextControl(
                            text=custom_placeholder_text,
                            style="class:option.placeholder",
                        ),
                        dont_extend_height=True,
                        wrap_lines=True,
                    ),
                ],
                padding=0,
            )

            custom_input_body = VSplit(
                [custom_body_prefix, custom_input_field]
                if custom_input_field is not None
                else [custom_body_prefix],
                padding=0,
            )

            custom_row_window = HSplit(
                [
                    custom_header_window,
                    ConditionalContainer(
                        content=custom_placeholder_body,
                        filter=Condition(lambda: not _is_custom_selected()),
                    ),
                    ConditionalContainer(
                        content=custom_input_body,
                        filter=Condition(_is_custom_selected),
                    ),
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

        separator_window_top = Window(
            content=FormattedTextControl(text=_separator_text, style="class:separator"),
            dont_extend_height=True,
            height=1,
        )
        separator_window_bottom = Window(
            content=FormattedTextControl(text=_separator_text, style="class:separator"),
            dont_extend_height=True,
            height=1,
        )

        title_window = Window(
            content=FormattedTextControl(text=f"[ {title} ]", style="class:title"),
            wrap_lines=True,
            dont_extend_height=True,
        )

        body_items = [title_window, separator_window_top, prompt_window, Window(height=1, char="")]
        if values:
            body_items.append(options_window)
        if custom_row_window is not None:
            body_items.append(custom_row_window)
        body_items.append(separator_window_bottom)
        body_items.append(hint_window)

        body = HSplit(body_items, padding=0)

        style = Style.from_dict(
            {
                "title": "bold",
                "separator": "fg:#6c7086",
                "option": "",
                "option.selected": "bold fg:#7dcfff",
                "option.description": "fg:#7a8499",
                "option.description.selected": "fg:#9ccfd8",
                "option.placeholder": "fg:#7a8499",
                "input": "",
                "hint": "fg:#7a8499",
            }
        )

        focus_target = (
            custom_input_field
            if _is_custom_selected() and custom_input_field is not None
            else options_window
        )

        app = Application(
            layout=Layout(body, focused_element=focus_target),
            key_bindings=kb,
            full_screen=False,
            style=style,
            mouse_support=False,
        )
        # This modal runs inside an already active shell UI. Disable CPR probing
        # to avoid noisy warnings on terminals that don't support cursor
        # position requests when the custom input field receives focus.
        if hasattr(app, "output") and hasattr(app.output, "enable_cpr"):
            setattr(app.output, "enable_cpr", False)
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

            threading.Thread(target=_watch_ask_user_resize, daemon=True).start()

        try:
            while True:
                try:
                    selected_value = app.run(in_thread=True)
                except KeyboardInterrupt:
                    selected_value = None
                if selected_value is None and not allow_cancel:
                    continue
                break
        finally:
            resize_stop_event.set()
    except Exception:
        selected_value = None

    return _build_interaction_response(request, selected_value)


def handle_interaction_required(shell: Any, event: LLMEvent) -> LLMCallbackResult:
    """Handle interaction event with modal UI."""
    data = event.data or {}
    request_payload = data.get("interaction_request")
    if not isinstance(request_payload, dict):
        return LLMCallbackResult.CONTINUE
    request = InteractionRequest.from_dict(request_payload)
    _prepare_interaction_prompt(shell)
    response = render_interaction_modal(shell, request)

    try:
        apply_interaction_response_to_data(data, response)
    except Exception:
        pass

    return LLMCallbackResult.CONTINUE


def display_security_panel(shell: Any, data: dict, panel_mode: str = "confirm") -> None:
    """Display rich security panel for AI tool calls."""
    self = shell
    panel = data.get("panel") if isinstance(data.get("panel"), dict) else {}
    panel_mode = str(panel.get("mode") or panel_mode).lower()
    is_blocked = panel_mode == "blocked"
    is_info = panel_mode == "info"

    tool_name = str(data.get("tool_name", "unknown"))
    security_analysis = panel.get("analysis")
    if not isinstance(security_analysis, dict):
        security_analysis = (
            data.get("analysis")
            if isinstance(data.get("analysis"), dict)
            else data.get("security_analysis", {})
        )
    target = panel.get("target", data.get("target"))
    preview = panel.get("preview", data.get("preview"))

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
    elif target:
        content.append(f"[bold]{t('shell.security.label.target')}:[/bold] {target}")

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

    if preview is None:
        tool_args = data.get("tool_args")
        if isinstance(tool_args, dict) and "content" in tool_args:
            raw_content = tool_args.get("content")
            if isinstance(raw_content, str):
                preview = raw_content[:100] + "..." if len(raw_content) > 100 else raw_content

    if preview is not None:
        content.append(
            f"[bold]{t('shell.security.label.content_preview')}:[/bold] {preview}"
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
