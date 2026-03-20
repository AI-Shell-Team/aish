"""PlanAgent for creating and executing plans in Plan mode."""

import asyncio
import logging
import os
import threading
from typing import TYPE_CHECKING, Optional

from aish.cancellation import CancellationReason, CancellationToken
from aish.config import ConfigModel
from aish.context_manager import ContextManager
from aish.plans.manager import PlanManager
from aish.plans.models import Plan, PlanStatus, StepStatus
from aish.prompts import PromptManager
from aish.skills import SkillManager
from aish.tools.base import ToolBase
from aish.tools.code_exec import BashTool
from aish.tools.final_answer import FinalAnswer
from aish.tools.fs_tools import ReadFileTool, WriteFileTool, EditFileTool
from aish.tools.plan_tools import (
    FinalizePlanTool,
    PlanCompleteTool,
    StepCompleteTool,
    StepFailedTool,
    StepSkipTool,
)
from aish.tools.result import ToolResult
from aish.utils import get_output_language, get_system_info

if TYPE_CHECKING:
    from aish.shell import AIShell

logger = logging.getLogger(__name__)


class PlanAgent(ToolBase):
    """Agent for creating and executing plans with user confirmation."""

    name: str = "plan_agent"
    description: str = """
    Creates and executes plans for complex tasks with user confirmation.
    Use this agent when you need to break down a complex task into clear, executable steps.
    The agent will:
    1. Research the current state
    2. Generate a structured plan
    3. Ask for user confirmation
    4. Execute the plan if approved (in background thread)
    """
    parameters: dict = {
        "type": "object",
        "properties": {
            "task": {
                "type": "string",
                "description": "The complex task to create a plan for. Describe what you want to accomplish.",
            },
            "auto_execute": {
                "type": "boolean",
                "description": "If true, skip confirmation and execute immediately (default: false)",
            },
        },
        "required": ["task"],
    }

    def __init__(
        self,
        config: ConfigModel,
        model_id: str,
        skill_manager: SkillManager,
        plan_manager: PlanManager,
        shell: Optional["AIShell"] = None,
        api_base: str | None = None,
        api_key: str | None = None,
        parent_event_callback=None,
        cancellation_token: CancellationToken | None = None,
        history_manager=None,
        **data,
    ):
        super().__init__(**data)

        self.model_id = model_id
        self.api_base = api_base
        self.api_key = api_key
        self.skill_manager = skill_manager
        self.plan_manager = plan_manager
        self.shell = shell
        self.parent_event_callback = parent_event_callback
        self.cancellation_token = cancellation_token or CancellationToken()
        self.history_manager = history_manager

        self.uname_info = get_system_info("uname -a")
        self.os_info = get_system_info("cat /etc/issue 2>/dev/null") or "N/A"
        self.output_language = get_output_language(config)

        self.prompt_manager = PromptManager()

        # Track active execution threads
        self._execution_threads: dict[str, threading.Thread] = {}
        self._threads_lock = threading.Lock()

    def cleanup_completed_threads(self) -> None:
        """Remove completed threads from tracking to prevent memory leak."""
        with self._threads_lock:
            completed = [
                plan_id for plan_id, thread in self._execution_threads.items()
                if not thread.is_alive()
            ]
            for plan_id in completed:
                del self._execution_threads[plan_id]

    def get_active_execution_count(self) -> int:
        """Get the number of active plan executions.

        Returns:
            Number of active execution threads
        """
        with self._threads_lock:
            return sum(1 for t in self._execution_threads.values() if t.is_alive())

    def __call__(self, task: str, auto_execute: bool = False):
        """Execute planning using a subsession.

        Args:
            task: The task to create a plan for
            auto_execute: If true, skip confirmation and execute immediately

        Returns:
            Coroutine that resolves to the plan result
        """
        async def async_wrapper():
            try:
                return await self._async_call(task, auto_execute)
            except asyncio.CancelledError:
                reason = self.cancellation_token.get_cancellation_reason()
                return f"Planning cancelled ({reason.value if reason else 'unknown reason'})"
            except Exception as e:
                return f"Error during planning: {str(e)}"

        try:
            asyncio.get_running_loop()
        except RuntimeError:
            import anyio
            return anyio.run(async_wrapper)
        return async_wrapper()

    async def _async_call(self, task: str, auto_execute: bool = False) -> str:
        """Async implementation of planning."""
        from aish.config import ConfigModel
        from aish.llm import LLMEventType, LLMSession
        from aish.tools.skill import SkillTool

        # Phase 1: Clarify requirements with user
        clarified_task = await self._clarify_requirements(task)

        # Create config for subsession
        config = ConfigModel(
            model=self.model_id,
            api_base=self.api_base,
            api_key=self.api_key,
            temperature=0.3,
            max_tokens=2000,
        )

        # Create read-only tools for planning
        tools = {
            "bash_exec": BashTool(history_manager=self.history_manager),
            "read_file": ReadFileTool(),
            "finalize_plan": FinalizePlanTool(),
            "skill": SkillTool(skill_manager=self.skill_manager, prompt_manager=self.prompt_manager),
            "final_answer": FinalAnswer(),
        }

        context_manager = ContextManager()

        # Build subsession with child cancellation token
        child_token = self.cancellation_token.create_child_token()
        subsession = LLMSession.create_subsession(
            config, self.skill_manager, tools, child_token
        )

        # Get system prompt for planning
        system_message = self.prompt_manager.substitute_template(
            "plan_agent",
            user_nickname=os.getenv("USER", "user"),
            uname_info=self.uname_info,
            os_info=self.os_info,
            output_language=self.output_language,
            basic_env_info="",
        )

        # Initial message with the clarified task
        initial_message = f"Create a plan for the following task:\n\n{clarified_task}"

        # Track for finalize_plan result
        finalized_plan = None

        def event_proxy_callback(event):
            nonlocal finalized_plan

            # Check for finalize_plan completion
            if (
                event.event_type == LLMEventType.TOOL_EXECUTION_END
                and hasattr(event, "data")
                and event.data
                and event.data.get("tool_name") == "finalize_plan"
            ):
                result_data = event.data.get("result_data")
                if result_data and isinstance(result_data, dict):
                    finalized_plan = result_data.get("plan")
                else:
                    result = event.data.get("result")
                    if result and isinstance(result, ToolResult):
                        finalized_plan = result.data.get("plan") if result.data else None

            # Handle cancellation
            if event.event_type == LLMEventType.CANCELLED:
                if not self.cancellation_token.is_cancelled():
                    self.cancellation_token.cancel(
                        CancellationReason.PARENT_CANCELLED,
                        "Cancelled by parent session",
                    )

            # Forward to parent
            if self.parent_event_callback:
                modified_data = event.data.copy() if event.data else {}
                modified_data["source"] = "plan_agent"

                from aish.llm import LLMEvent

                forwarded_event = LLMEvent(
                    event_type=event.event_type,
                    data=modified_data,
                    timestamp=event.timestamp,
                    metadata=event.metadata,
                )

                return self.parent_event_callback(forwarded_event)

            from aish.llm import LLMCallbackResult
            return LLMCallbackResult.CONTINUE

        original_callback = subsession.event_callback
        subsession.event_callback = event_proxy_callback

        try:
            # Run ReAct loop for planning
            max_iterations = 6
            iteration = 0
            current_prompt = initial_message
            last_response = ""

            while iteration < max_iterations and finalized_plan is None:
                iteration += 1

                context = context_manager.as_messages()

                response = await subsession.process_input(
                    prompt=current_prompt,
                    context_manager=context_manager,
                    system_message=system_message if iteration == 1 else None,
                    history=context,
                )

                last_response = response

                # Check for finalize_plan in response
                if finalized_plan is not None:
                    plan = Plan.from_dict(finalized_plan)
                    self.plan_manager.save_plan(plan)

                    # Ask user for confirmation (unless auto_execute is true)
                    if auto_execute:
                        return await self._approve_and_execute(plan)
                    else:
                        return await self._ask_and_execute(plan)

                # Continue if no plan yet
                if iteration < max_iterations:
                    current_prompt = "Continue your planning. When ready, use the finalize_plan tool to present your plan."

            return last_response or "Planning completed without a finalized plan."

        except Exception as e:
            return f"Error during planning: {str(e)}"
        finally:
            subsession.event_callback = original_callback

    async def _clarify_requirements(self, task: str) -> str:
        """Clarify requirements with user before planning.

        Args:
            task: The original task description

        Returns:
            Clarified task with user's additional requirements
        """
        # Check if ask_user is available
        if not self.shell or not hasattr(self.shell, "request_user_choice"):
            return task

        # Ask user about their preferences and constraints
        data = {
            "prompt": f"Creating plan for: **{task[:80]}{'...' if len(task) > 80 else ''}**\n\nAny special requirements?",
            "options": [
                {"value": "proceed", "label": "Proceed (default)"},
                {"value": "simplify", "label": "Keep it simple"},
                {"value": "detailed", "label": "More detailed"},
                {"value": "custom", "label": "Add constraints"},
            ],
            "default": "proceed",
            "title": "Plan Requirements",
            "allow_cancel": True,
            "allow_custom_input": True,
            "custom_label": "Describe requirements",
            "custom_prompt": "Describe your specific requirements:",
        }

        # Run request_user_choice in a thread to avoid blocking async context
        import asyncio
        import concurrent.futures

        loop = asyncio.get_running_loop()
        with concurrent.futures.ThreadPoolExecutor() as executor:
            selected, status = await loop.run_in_executor(
                executor, self.shell.request_user_choice, data
            )

        if status == "selected" and selected:
            if selected == "proceed":
                return task
            elif selected == "simplify":
                return f"{task}\n\n**User preference**: Keep the plan as simple as possible. Focus on essential steps only."
            elif selected == "detailed":
                return f"{task}\n\n**User preference**: Create a detailed step-by-step plan with thorough explanations."
            else:
                # Custom input
                return f"{task}\n\n**User requirements**: {selected}"

        return task

    async def _ask_and_execute(self, plan: Plan) -> str:
        """Ask user for confirmation and execute if approved."""
        plan_summary = f"""**Plan**: {plan.title}
**Steps**: {len(plan.steps)}

{plan.to_markdown()[:800]}"""

        # Check if ask_user is available
        if not self.shell or not hasattr(self.shell, "request_user_choice"):
            return f"""Plan created successfully!

{plan_summary}

Plan ID: {plan.plan_id}
Status: {plan.status.value}

Note: Interactive confirmation not available. Plan saved for manual execution."""

        # Ask user
        data = {
            "prompt": f"Plan created. Execute it?\n\n{plan_summary[:500]}",
            "options": [
                {"value": "execute", "label": "Execute now"},
                {"value": "review", "label": "Save only"},
                {"value": "cancel", "label": "Discard"},
            ],
            "default": "execute",
            "title": "Execute Plan?",
            "allow_cancel": True,
        }

        # Run request_user_choice in a thread to avoid blocking async context
        import asyncio
        import concurrent.futures

        loop = asyncio.get_running_loop()
        with concurrent.futures.ThreadPoolExecutor() as executor:
            selected, status = await loop.run_in_executor(
                executor, self.shell.request_user_choice, data
            )

        if status == "selected" and selected:
            if selected == "execute":
                return await self._approve_and_execute(plan)
            elif selected == "cancel":
                # Delete the plan
                self.plan_manager.delete_plan(plan.plan_id)
                return "Plan cancelled and discarded."
            else:
                # Review only
                return f"""Plan saved for review.

{plan_summary}

Plan ID: {plan.plan_id}
Status: {plan.status.value}

To execute later, use: plan_agent(task="resume plan {plan.plan_id}")"""
        else:
            # User cancelled or error - just save the plan
            return f"""Plan created and saved.

{plan_summary}

Plan ID: {plan.plan_id}
Status: {plan.status.value}"""

    async def _approve_and_execute(self, plan: Plan) -> str:
        """Approve the plan and start execution in background thread."""
        # Update plan status to APPROVED
        plan.status = PlanStatus.APPROVED
        self.plan_manager.save_plan(plan)

        # Clean up completed threads before starting new one
        self.cleanup_completed_threads()

        # Start execution in background thread
        thread = threading.Thread(
            target=self._execute_plan_in_thread,
            args=(plan.plan_id,),
            daemon=True,
            name=f"plan-executor-{plan.plan_id}",
        )
        with self._threads_lock:
            self._execution_threads[plan.plan_id] = thread
        thread.start()

        return f"""Plan approved and execution started!

**Title**: {plan.title}
**Plan ID**: {plan.plan_id}
**Steps**: {len(plan.steps)}

Execution is running in the background. The status bar will show progress.

To check status: The plan is being executed step by step.
To pause/stop: Use Ctrl+C or close the application."""

    def _execute_plan_in_thread(self, plan_id: str) -> None:
        """Execute plan in a background thread."""
        import anyio

        try:
            # Run the async execution in the thread
            anyio.run(self._execute_plan_async, plan_id)
        except Exception:
            # Log error and update plan status
            plan = self.plan_manager.load_plan(plan_id)
            if plan:
                plan.status = PlanStatus.FAILED
                self.plan_manager.save_plan(plan)

    async def _execute_plan_async(self, plan_id: str) -> None:
        """Async plan execution."""
        from aish.config import ConfigModel
        from aish.llm import LLMEventType, LLMSession
        from aish.tools.skill import SkillTool

        plan = self.plan_manager.load_plan(plan_id)
        if not plan:
            return

        # Update status to IN_PROGRESS
        plan.status = PlanStatus.IN_PROGRESS
        self.plan_manager.save_plan(plan)

        # Update TUI if available
        self._show_plan_in_tui(plan)

        # Create execution config
        config = ConfigModel(
            model=self.model_id,
            api_base=self.api_base,
            api_key=self.api_key,
            temperature=0.3,
            max_tokens=2000,
        )

        # Create execution tools
        tools = {
            "bash_exec": BashTool(history_manager=self.history_manager),
            "read_file": ReadFileTool(),
            "write_file": WriteFileTool(),
            "edit_file": EditFileTool(),
            "step_complete": StepCompleteTool(),
            "step_skip": StepSkipTool(),
            "step_failed": StepFailedTool(),
            "plan_complete": PlanCompleteTool(),
            "skill": SkillTool(skill_manager=self.skill_manager, prompt_manager=self.prompt_manager),
            "final_answer": FinalAnswer(),
        }

        context_manager = ContextManager()
        child_token = self.cancellation_token.create_child_token()
        subsession = LLMSession.create_subsession(
            config, self.skill_manager, tools, child_token
        )

        # Track step status updates
        step_status_updates: dict[int, str] = {}

        def event_proxy_callback(event):
            # Track step completion events
            if event.event_type == LLMEventType.TOOL_EXECUTION_END:
                tool_name = event.data.get("tool_name") if event.data else ""
                if tool_name in ("step_complete", "step_skip", "step_failed"):
                    result = event.data.get("result")
                    if result and isinstance(result, ToolResult):
                        step_num = result.data.get("step_number") if result.data else None
                        status = result.data.get("status") if result.data else ""
                        if step_num and status:
                            step_status_updates[step_num] = status
                            self._update_step_status(plan_id, step_num, status)

            # Forward to parent
            if self.parent_event_callback:
                modified_data = event.data.copy() if event.data else {}
                modified_data["source"] = "plan_executor"

                from aish.llm import LLMEvent

                forwarded_event = LLMEvent(
                    event_type=event.event_type,
                    data=modified_data,
                    timestamp=event.timestamp,
                    metadata=event.metadata,
                )
                return self.parent_event_callback(forwarded_event)

            from aish.llm import LLMCallbackResult
            return LLMCallbackResult.CONTINUE

        subsession.event_callback = event_proxy_callback

        try:
            # Get system message
            current_step = plan.get_step(plan.current_step)
            if not current_step:
                return

            system_message = self._build_execution_system_message(plan, current_step)
            initial_message = self._build_step_message(current_step)

            max_iterations = 4
            iteration = 0
            current_prompt = initial_message

            while iteration < max_iterations:
                iteration += 1

                context = context_manager.as_messages()
                await subsession.process_input(
                    prompt=current_prompt,
                    context_manager=context_manager,
                    system_message=system_message,
                    history=context,
                )

                # Check if current step was completed
                if plan.current_step in step_status_updates:
                    status = step_status_updates[plan.current_step]

                    if status == "completed":
                        next_step = self._get_next_step(plan)
                        if next_step:
                            plan.current_step = next_step.number
                            self.plan_manager.save_plan(plan)
                            self._update_tui_step(plan.current_step - 1, "completed")

                            current_step = next_step
                            system_message = self._build_execution_system_message(plan, current_step)
                            current_prompt = self._build_step_message(current_step)
                            iteration = 0
                            step_status_updates = {}
                            continue
                        else:
                            plan.status = PlanStatus.COMPLETED
                            self.plan_manager.save_plan(plan)
                            return

                    elif status == "skipped":
                        next_step = self._get_next_step(plan)
                        if next_step:
                            plan.current_step = next_step.number
                            self.plan_manager.save_plan(plan)
                            self._update_tui_step(plan.current_step - 1, "skipped")

                            current_step = next_step
                            system_message = self._build_execution_system_message(plan, current_step)
                            current_prompt = f"Step skipped. {self._build_step_message(current_step)}"
                            iteration = 0
                            step_status_updates = {}
                            continue
                        else:
                            plan.status = PlanStatus.COMPLETED
                            self.plan_manager.save_plan(plan)
                            return

                    elif status == "failed":
                        plan.status = PlanStatus.FAILED
                        self.plan_manager.save_plan(plan)
                        return

                if iteration < max_iterations:
                    current_prompt = "Continue executing the current step. Use step_complete when done."

        except Exception as e:
            logger.exception("Plan execution failed for plan %s: %s", plan.plan_id, e)
            plan.status = PlanStatus.FAILED
            self.plan_manager.save_plan(plan)
        finally:
            # Hide plan queue in TUI
            if self.shell and hasattr(self.shell, "_tui_app"):
                self.shell._tui_app.hide_plan_queue()

    def _show_plan_in_tui(self, plan: Plan) -> None:
        """Show plan in TUI status bar."""
        if self.shell and hasattr(self.shell, "_tui_app"):

            self.shell._tui_app.set_mode("PLAN")

            steps_data = [
                {
                    "number": s.number,
                    "title": s.title,
                    "status": s.status.value,
                }
                for s in plan.steps
            ]
            self.shell._tui_app.show_plan_queue(
                plan_id=plan.plan_id,
                plan_title=plan.title,
                steps=steps_data,
                current_step=plan.current_step,
            )

        # Also print plan header to console for visibility
        if self.shell:
            try:
                from rich.console import Console
                console = Console()
                console.print(
                    f"\n[bold yellow]📋 Plan: {plan.title}[/bold yellow]"
                )
                console.print(f"[dim]Steps: {len(plan.steps)}[/dim]")
            except Exception:
                pass

    def _update_tui_step(self, step_number: int, status: str) -> None:
        """Update TUI step status."""
        if self.shell and hasattr(self.shell, "_tui_app"):
            from aish.tui.types import StepStatus as TUIStepStatus

            status_map = {
                "pending": TUIStepStatus.PENDING,
                "in_progress": TUIStepStatus.IN_PROGRESS,
                "completed": TUIStepStatus.COMPLETED,
                "skipped": TUIStepStatus.SKIPPED,
                "failed": TUIStepStatus.FAILED,
            }

            tui_status = status_map.get(status)
            if tui_status:
                self.shell._tui_app.update_plan_step(step_number, tui_status)

        # Also print step update to console for visibility in background thread
        if self.shell:
            try:
                from rich.console import Console
                console = Console()

                # Get step title from plan via TUI public method
                plan_id = None
                if hasattr(self.shell, "_tui_app") and self.shell._tui_app is not None:
                    plan_queue_state = self.shell._tui_app.get_plan_queue_state()
                    if plan_queue_state and plan_queue_state.plan_id:
                        plan_id = plan_queue_state.plan_id

                step_title = f"Step {step_number}"
                if plan_id:
                    plan = self.plan_manager.load_plan(plan_id)
                    if plan:
                        step = plan.get_step(step_number)
                        if step:
                            step_title = f"Step {step_number}: {step.title}"

                status_colors = {
                    "completed": "green",
                    "in_progress": "yellow",
                    "skipped": "dim",
                    "failed": "red",
                    "pending": "dim",
                }
                color = status_colors.get(status, "white")
                status_icons = {
                    "completed": "✓",
                    "in_progress": "⋯",
                    "skipped": "○",
                    "failed": "✗",
                    "pending": "○",
                }
                icon = status_icons.get(status, "•")

                console.print(f"[{color}]{icon} {step_title} [{color}][/{color}]")
            except Exception:
                pass

    def _update_step_status(self, plan_id: str, step_number: int, status: str) -> None:
        """Update step status in storage."""
        plan = self.plan_manager.load_plan(plan_id)
        if plan:
            step = plan.get_step(step_number)
            if step:
                status_map = {
                    "completed": StepStatus.COMPLETED,
                    "skipped": StepStatus.SKIPPED,
                    "failed": StepStatus.FAILED,
                }
                step.status = status_map.get(status, step.status)
                self.plan_manager.save_plan(plan)

    def _get_next_step(self, plan: Plan):
        """Get the next pending step."""
        for step in plan.steps:
            if step.status in (StepStatus.PENDING, StepStatus.FAILED):
                return step
        return None

    def _build_execution_system_message(self, plan: Plan, step) -> str:
        """Build system message for execution."""
        import json

        step_details = json.dumps(
            {
                "number": step.number,
                "title": step.title,
                "description": step.description,
                "commands": step.commands,
                "expected_outcome": step.expected_outcome,
                "verification": step.verification,
                "status": step.status.value,
            },
            indent=2,
        )

        return self.prompt_manager.substitute_template(
            "build_agent",
            user_nickname=os.getenv("USER", "user"),
            uname_info=self.uname_info,
            os_info=self.os_info,
            output_language=self.output_language,
            basic_env_info="",
            plan_title=plan.title,
            plan_description=plan.description,
            current_step=step.number,
            total_steps=len(plan.steps),
            current_step_details=step_details,
        )

    def _build_step_message(self, step) -> str:
        """Build message for step execution."""
        return f"""Execute Step {step.number}: {step.title}

{step.description}

Commands to execute:
{chr(10).join(f'  - {cmd}' for cmd in step.commands) if step.commands else '  (No specific commands - use your judgment)'}

Expected outcome: {step.expected_outcome or 'See step description'}
Verification: {step.verification or 'N/A'}

Begin execution now. Use step_complete when finished, step_skip if not applicable, or step_failed if there's an error."""
