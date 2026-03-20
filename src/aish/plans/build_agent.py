"""BuildAgent for executing plans in Plan mode."""

import asyncio
import os

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
from aish.tools.fs_tools import EditFileTool, ReadFileTool, WriteFileTool
from aish.tools.plan_tools import (
    PlanCompleteTool,
    StepCompleteTool,
    StepFailedTool,
    StepSkipTool,
)
from aish.tools.result import ToolResult
from aish.utils import get_output_language, get_system_info


class BuildAgent(ToolBase):
    """Agent for executing approved plans step by step."""

    # Pydantic model fields
    name: str = "build_agent"
    description: str = """
    Executes approved plans step by step with resume capability.
    Use this agent to run a plan that has been created and approved.
    The agent will execute each step's commands and verify success before proceeding.
    """
    parameters: dict = {
        "type": "object",
        "properties": {
            "plan_id": {
                "type": "string",
                "description": "The ID of the plan to execute (obtained from plan_agent)",
            },
            "start_from_step": {
                "type": "integer",
                "description": "Optional step number to start from (for resume/partial execution)",
            },
        },
        "required": ["plan_id"],
    }

    def __init__(
        self,
        config: ConfigModel,
        model_id: str,
        skill_manager: SkillManager,
        plan_manager: PlanManager,
        shell=None,  # Reference to AIShell for TUI updates
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

    def __call__(self, plan_id: str, start_from_step: int | None = None):
        """Execute plan using a subsession.

        Args:
            plan_id: The plan ID to execute
            start_from_step: Optional step number to start from

        Returns:
            Coroutine that resolves to the execution result
        """
        async def async_wrapper():
            try:
                return await self._async_call(plan_id, start_from_step)
            except asyncio.CancelledError:
                reason = self.cancellation_token.get_cancellation_reason()
                return f"Execution cancelled ({reason.value if reason else 'unknown reason'})"
            except Exception as e:
                return f"Error during execution: {str(e)}"

        try:
            asyncio.get_running_loop()
        except RuntimeError:
            import anyio
            return anyio.run(async_wrapper)
        return async_wrapper()

    async def _async_call(
        self, plan_id: str, start_from_step: int | None = None
    ) -> str:
        """Async implementation of plan execution."""
        from aish.config import ConfigModel
        from aish.llm import LLMEventType, LLMSession
        from aish.tools.skill import SkillTool

        # Load the plan
        plan = self.plan_manager.load_plan(plan_id)
        if plan is None:
            return f"Error: Plan not found: {plan_id}"

        if plan.status != PlanStatus.APPROVED:
            return f"Error: Plan is not approved. Current status: {plan.status.value}"

        # Set starting step
        if start_from_step is not None:
            plan.current_step = start_from_step

        # Update plan status to in_progress
        plan.status = PlanStatus.IN_PROGRESS
        self.plan_manager.save_plan(plan)

        # Update TUI if available
        if self.shell and hasattr(self.shell, "_tui_app"):
            self.shell._tui_app.set_mode("PLAN")

            # Show plan queue
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

        # Create config for subsession
        config = ConfigModel(
            model=self.model_id,
            api_base=self.api_base,
            api_key=self.api_key,
            temperature=0.3,
            max_tokens=2000,
        )

        # Create tools for execution
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

        # Build subsession with child cancellation token
        child_token = self.cancellation_token.create_child_token()
        subsession = LLMSession.create_subsession(
            config, self.skill_manager, tools, child_token
        )

        # Build system message for current step
        current_step = plan.get_step(plan.current_step)
        if current_step is None:
            return "Error: No valid step found to execute"

        system_message = self._build_system_message(plan, current_step)

        # Initial message
        initial_message = self._build_initial_message(plan, current_step)

        # Track step status updates
        step_status_updates = {}

        def event_proxy_callback(event):
            nonlocal step_status_updates

            # Track step completion events
            if event.event_type == LLMEventType.TOOL_EXECUTION_END:
                tool_name = event.data.get("tool_name") if event.data else ""
                if tool_name in ("step_complete", "step_skip", "step_failed"):
                    result = event.data.get("result")
                    if result and isinstance(result, ToolResult):
                        step_num = result.data.get("step_number") if result.data else None
                        status = result.data.get("status") if result.data else ""
                        if step_num:
                            step_status_updates[step_num] = status
                            self._update_step_in_plan(plan, step_num, status, result)

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
                modified_data["source"] = "build_agent"

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
            # Execute current step
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

                # Check if step was completed
                if plan.current_step in step_status_updates:
                    status = step_status_updates[plan.current_step]

                    if status == "completed":
                        # Move to next step
                        next_step = self._get_next_step(plan)
                        if next_step:
                            plan.current_step = next_step.number
                            self.plan_manager.save_plan(plan)

                            # Update TUI
                            self._update_tui_step(plan.current_step - 1, "completed")

                            # Build new system message for next step
                            current_step = next_step
                            system_message = self._build_system_message(plan, current_step)
                            current_prompt = f"Proceed to Step {current_step.number}: {current_step.title}"

                            # Reset for next step
                            iteration = 0
                            step_status_updates = {}
                            continue
                        else:
                            # All steps complete
                            plan.status = PlanStatus.COMPLETED
                            self.plan_manager.save_plan(plan)
                            return f"Plan '{plan.title}' completed successfully!"

                    elif status == "skipped":
                        next_step = self._get_next_step(plan)
                        if next_step:
                            plan.current_step = next_step.number
                            self.plan_manager.save_plan(plan)
                            self._update_tui_step(plan.current_step - 1, "skipped")

                            current_step = next_step
                            system_message = self._build_system_message(plan, current_step)
                            current_prompt = f"Step skipped. Proceed to Step {current_step.number}: {current_step.title}"

                            iteration = 0
                            step_status_updates = {}
                            continue
                        else:
                            plan.status = PlanStatus.COMPLETED
                            self.plan_manager.save_plan(plan)
                            return f"Plan '{plan.title}' completed (some steps were skipped)!"

                    elif status == "failed":
                        plan.status = PlanStatus.FAILED
                        self.plan_manager.save_plan(plan)
                        self._update_tui_step(plan.current_step, "failed")
                        return f"Plan execution failed at step {plan.current_step}. You can resume with: build_agent(plan_id='{plan.plan_id}', start_from_step={plan.current_step})"

                # Continue iteration
                if iteration < max_iterations:
                    current_prompt = "Continue executing the current step. Use step_complete when done, step_skip if not applicable, or step_failed if there's an error."

            # Check completion
            progress = plan.get_progress_summary()
            return f"Execution in progress. Step {plan.current_step} of {len(plan.steps)}. Progress: {progress['completed']}/{progress['total']} completed. Resume with: build_agent(plan_id='{plan.plan_id}')"

        except Exception as e:
            plan.status = PlanStatus.FAILED
            self.plan_manager.save_plan(plan)
            return f"Error during execution: {str(e)}"
        finally:
            subsession.event_callback = original_callback
            # Hide plan queue when execution completes
            if self.shell and hasattr(self.shell, "_tui_app"):
                self.shell._tui_app.hide_plan_queue()

    def _build_system_message(self, plan: Plan, step) -> str:
        """Build system message for current step."""
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

    def _build_initial_message(self, plan: Plan, step) -> str:
        """Build initial message for step execution."""
        return f"""Execute Step {step.number}: {step.title}

{step.description}

Commands to execute:
{chr(10).join(f'  - {cmd}' for cmd in step.commands) if step.commands else '  (No specific commands - use your judgment)'}

Expected outcome: {step.expected_outcome or 'See step description'}
Verification: {step.verification or 'N/A'}

Begin execution now. Use step_complete when finished, step_skip if not applicable, or step_failed if there's an error."""

    def _get_next_step(self, plan: Plan):
        """Get the next pending step."""
        for step in plan.steps:
            if step.status in (StepStatus.PENDING, StepStatus.FAILED):
                return step
        return None

    def _update_step_in_plan(
        self, plan: Plan, step_number: int, status: str, result: ToolResult
    ):
        """Update step status in plan."""
        step = plan.get_step(step_number)
        if not step:
            return

        status_map = {
            "completed": StepStatus.COMPLETED,
            "skipped": StepStatus.SKIPPED,
            "failed": StepStatus.FAILED,
        }

        step.status = status_map.get(status, step.status)

        if status == "failed":
            step.error_message = result.data.get("error_message", "") if result.data else ""

        self.plan_manager.save_plan(plan)

    def _update_tui_step(self, step_number: int, status: str):
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
