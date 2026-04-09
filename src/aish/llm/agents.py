import asyncio
import os

import anyio

from aish.state.cancellation import CancellationReason, CancellationToken
from aish.config import ConfigModel
from aish.prompts import PromptManager
from aish.state import ContextManager
from aish.skills import SkillManager
from aish.tools.base import ToolBase
from aish.system_info import get_output_language, get_system_info


class SystemDiagnoseAgent(ToolBase):
    # Pydantic model fields (class attributes)
    name: str = "system_diagnose_agent"
    description: str = """
    Advanced log analysis and system diagnosis agent that can read files, analyze patterns, and provide detailed diagnostic reports
    """
    parameters: dict = {
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "The diagnostic query or system issue to analyze. Describe the problem, symptoms, or specific logs to investigate.",
            }
        },
        "required": ["query"],
    }

    def __init__(
        self,
        config: ConfigModel,
        model_id: str,
        skill_manager: SkillManager,
        api_base: str | None = None,
        api_key: str | None = None,
        parent_event_callback=None,
        cancellation_token: CancellationToken | None = None,
        history_manager=None,
        **data,
    ):
        # Initialize the Pydantic BaseModel with the class attributes
        super().__init__(**data)

        # Store LLM configuration for creating subsessions
        self.model_id = model_id
        self.api_base = api_base
        self.api_key = api_key
        self.skill_manager = skill_manager
        self.parent_event_callback = parent_event_callback
        self.cancellation_token = cancellation_token or CancellationToken()
        self.history_manager = history_manager

        self.uname_info = get_system_info("uname -a")
        self.os_info = get_system_info("cat /etc/issue 2>/dev/null") or "N/A"
        self.output_language = get_output_language(config)

        self.prompt_manager = PromptManager()

    def __call__(self, query: str):
        """
        Execute system diagnosis using ReAct-style loop with a subsession.

        Returns a coroutine that LLMSession.execute_tool will await.

        Args:
            query: The diagnostic query or system issue to analyze

        Returns:
            Coroutine[Any, Any, str]: A coroutine that resolves to the diagnostic result
        """

        # Create an async wrapper for tool execution.
        async def async_wrapper():
            try:
                return await self._async_call(query)
            except anyio.get_cancelled_exc_class():
                reason = self.cancellation_token.get_cancellation_reason()
                return f"Diagnosis cancelled ({reason.value if reason else 'unknown reason'})"
            except Exception as e:
                return f"Error during diagnosis: {str(e)}"

        # If we're already in an event loop, return coroutine for the caller to await.
        try:
            asyncio.get_running_loop()
        except RuntimeError:
            return anyio.run(async_wrapper)
        return async_wrapper()

    def get_session_output(self, result) -> str | None:
        return result.render_for_llm()

    def _create_react_system_prompt(self) -> str:
        """Generate a ReAct-style system prompt for diagnostics."""
        base_prompt = self.prompt_manager.substitute_template(
            "system_diagnose",
            user_nickname=os.getenv("USER", "user"),
            uname_info=self.uname_info,
            os_info=self.os_info,
            output_language=self.output_language,
            basic_env_info="",
        )
        react_prompt = """
Follow the ReAct format when reasoning:
Thought: describe your reasoning process
Action: choose a tool and provide arguments
Observation: summarize tool output
Final Answer: provide the final diagnostic conclusion

When ready, use the final_answer tool to deliver your final diagnostic conclusion.
"""
        return f"{base_prompt}\n{react_prompt}"

    async def _async_call(self, query: str) -> str:
        """
        Async implementation of system diagnosis using process_input.

        Args:
            query: The diagnostic query or system issue to analyze

        Returns:
            str: The final diagnostic result
        """
        # Import here to avoid circular imports
        from aish.config import ConfigModel
        from aish.llm import LLMEventType, LLMSession
        from aish.tools.code_exec import BashTool
        from aish.tools.final_answer import FinalAnswer
        from aish.tools.fs_tools import (EditFileTool, ReadFileTool,
                                         WriteFileTool)
        from aish.tools.skill import SkillTool

        # Create a config-derived configuration for the subsession
        config = ConfigModel(
            model=self.model_id,
            api_base=self.api_base,
            api_key=self.api_key,
            temperature=0.3,
            max_tokens=2000,
        )

        # Create tools for the subsession
        tools = {
            "bash_exec": BashTool(history_manager=self.history_manager),
            "read_file": ReadFileTool(),
            "write_file": WriteFileTool(),
            "edit_file": EditFileTool(),
            "final_answer": FinalAnswer(),
            "skill": SkillTool(skill_manager=self.skill_manager, prompt_manager=self.prompt_manager),
        }

        context_manager = ContextManager()
        system_message = self.prompt_manager.substitute_template(
            "system_diagnose",
            user_nickname=os.getenv("USER", "user"),
            uname_info=self.uname_info,
            os_info=self.os_info,
            output_language=self.output_language,
        )

        # Build subsession with child cancellation token
        child_token = self.cancellation_token.create_child_token()
        subsession = LLMSession.create_subsession(
            config, self.skill_manager, tools, child_token
        )

        # Compose initial message with user query
        initial_message = f"User's Problem: {query}"

        # Track if we got a final answer from the final_answer tool
        final_answer_result = None

        # Create event proxy callback that forwards events to parent and handles final_answer
        def event_proxy_callback(event):
            nonlocal final_answer_result

            # Check for final_answer tool completion
            if (
                event.event_type == LLMEventType.TOOL_EXECUTION_END
                and hasattr(event, "data")
                and event.data
                and event.data.get("tool_name") == "final_answer"
            ):
                # Capture the result from final_answer tool
                final_answer_result = event.data.get("result")

            # Handle cancellation events
            if event.event_type == LLMEventType.CANCELLED:
                # If parent cancelled us, propagate to our cancellation token
                if not self.cancellation_token.is_cancelled():
                    self.cancellation_token.cancel(
                        CancellationReason.PARENT_CANCELLED,
                        "Cancelled by parent session",
                    )

            # Forward event to parent callback if available
            if self.parent_event_callback:
                # Add source information to the event data
                modified_data = event.data.copy() if event.data else {}
                modified_data["source"] = "system_diagnose_agent"

                # Create a new event with the modified data
                from aish.llm import LLMEvent

                forwarded_event = LLMEvent(
                    event_type=event.event_type,
                    data=modified_data,
                    timestamp=event.timestamp,
                    metadata=event.metadata,
                )

                # Forward to parent
                return self.parent_event_callback(forwarded_event)

            # Return CONTINUE if no parent callback
            from aish.llm import LLMCallbackResult

            return LLMCallbackResult.CONTINUE

        # Store original callback and set our event proxy callback
        original_callback = subsession.event_callback
        subsession.event_callback = event_proxy_callback

        try:
            # Run ReAct loop until we get final_answer
            max_iterations = 4
            iteration = 0
            current_prompt = initial_message
            last_response = ""

            while iteration < max_iterations and final_answer_result is None:
                iteration += 1

                # Cancellation is handled structurally by CancelScope

                context = context_manager.as_messages()

                # Use process_input for proper callback and context management
                response = await subsession.process_input(
                    prompt=current_prompt,
                    context_manager=context_manager,
                    system_message=system_message if iteration == 1 else None,
                    history=context,
                )

                last_response = response

                # Accept explicit "FINAL_ANSWER:" style responses for compatibility.
                final_text = self._extract_final_answer_text(response)
                if final_text is not None:
                    return final_text

                # If we got a final answer from the tool callback, return it
                if final_answer_result is not None:
                    return final_answer_result

                # Cancellation is handled structurally by CancelScope

                # If no final answer yet, prepare for next iteration
                if iteration < max_iterations:
                    current_prompt = "Continue with your analysis. Remember to use the final_answer tool when you have completed your diagnostic conclusion."

            # If we exit loop without final_answer, return the last response
            return (
                final_answer_result
                or last_response
                or "Unable to complete diagnosis within iteration limit."
            )

        except Exception as e:
            return f"Error during diagnosis: {str(e)}"
        finally:
            # Restore original callback
            subsession.event_callback = original_callback

    def _extract_final_answer_text(self, response: str) -> str | None:
        """Extract a final answer from a plain-text response if present."""
        if not response:
            return None
        markers = ["FINAL_ANSWER:", "Final Answer:", "Final answer:"]
        for marker in markers:
            if marker in response:
                return response.split(marker, 1)[1].strip()
        return None
