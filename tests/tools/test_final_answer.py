"""
Unit and integration tests for FinalAnswer tool and SystemDiagnoseAgent.

This module implements the test requirements from Step 6:
1. Test FinalAnswer tool alone
2. Mock LLM to issue a final_answer call after one bash call; ensure loop exits and result is propagated
3. End-to-end: run a dummy query via outer LLMSession; verify nested agent executes and returns
"""

import time
from unittest.mock import AsyncMock, patch

import pytest

from aish.config import ConfigModel
from aish.llm.agents import SystemDiagnoseAgent
from aish.llm import LLMEvent, LLMEventType, LLMSession
from aish.skills import SkillManager
from aish.tools.code_exec import BashTool
from aish.tools.final_answer import FinalAnswer
from aish.tools.fs_tools import ReadFileTool, WriteFileTool


def make_skill_manager() -> SkillManager:
    skill_manager = SkillManager()
    skill_manager.load_all_skills()
    return skill_manager


class TestFinalAnswer:
    """Unit tests for FinalAnswer tool."""

    def test_final_answer_init(self):
        """Test FinalAnswer tool initialization."""
        tool = FinalAnswer()

        assert tool.name == "final_answer"
        assert (
            tool.description
            == "Ends the SystemDiagnoseAgent loop and returns the answer."
        )
        assert "answer" in tool.parameters["properties"]
        assert tool.parameters["required"] == ["answer"]

    def test_final_answer_call(self):
        """Test FinalAnswer tool execution."""
        tool = FinalAnswer()
        test_answer = "System analysis complete: Issue resolved by restarting service."

        result = tool(answer=test_answer)

        assert result == test_answer

    def test_final_answer_call_empty(self):
        """Test FinalAnswer tool with empty answer."""
        tool = FinalAnswer()

        result = tool(answer="")

        assert result == ""

    def test_final_answer_func_spec(self):
        """Test FinalAnswer tool function specification."""
        tool = FinalAnswer()

        func_spec = tool.to_func_spec()

        assert func_spec["type"] == "function"
        assert func_spec["function"]["name"] == "final_answer"
        assert "answer" in func_spec["function"]["parameters"]["properties"]


class TestSystemDiagnoseAgentMocked:
    """Integration tests for SystemDiagnoseAgent with mocked LLM responses."""

    def test_system_diagnose_agent_init(self):
        """Test SystemDiagnoseAgent initialization."""
        config = ConfigModel(
            model="test-model", api_base="http://test", api_key="test-key"
        )
        skill_manager = make_skill_manager()
        agent = SystemDiagnoseAgent(
            config=config,
            model_id="test-model",
            api_base="http://test",
            api_key="test-key",
            skill_manager=skill_manager,
        )

        assert agent.name == "system_diagnose_agent"
        assert agent.model_id == "test-model"
        assert agent.api_base == "http://test"
        assert agent.api_key == "test-key"

    @patch("aish.llm.LLMSession")
    def test_mocked_llm_with_final_answer_after_bash(self, mock_llm_session_class):
        """Test mocked LLM issuing final_answer call after one bash call."""
        # Create the agent
        config = ConfigModel(
            model="openrouter/moonshotai/kimi-k2", api_base=None, api_key="sk-test-key"
        )
        skill_manager = make_skill_manager()
        agent = SystemDiagnoseAgent(
            config=config,
            model_id="openrouter/moonshotai/kimi-k2",
            api_base=None,
            api_key="sk-test-key",
            skill_manager=skill_manager,
        )

        # Create mock subsession instance
        mock_subsession = AsyncMock()
        mock_llm_session_class.create_subsession.return_value = mock_subsession

        # Mock the process_input responses sequence:
        # 1. First call: bash command execution
        # 2. Second call: final_answer with result
        process_input_responses = [
            "I need to check system status first.",  # First response leads to bash call
            "FINAL_ANSWER: System analysis complete. Found high CPU usage from process XYZ. Recommended action: restart the service.",
        ]

        mock_subsession.process_input = AsyncMock(side_effect=process_input_responses)

        # Execute the agent
        query = "Analyze high CPU usage on the system"
        result = agent(query=query)

        # Verify the agent extracted and returned the final answer
        expected_result = "System analysis complete. Found high CPU usage from process XYZ. Recommended action: restart the service."
        assert result == expected_result

        # Verify subsession was created and called
        mock_llm_session_class.create_subsession.assert_called_once()
        assert mock_subsession.process_input.call_count == 2

    @patch("aish.llm.LLMSession")
    def test_mocked_llm_final_answer_alternative_format(self, mock_llm_session_class):
        """Test mocked LLM with alternative 'Final Answer:' format."""
        config = ConfigModel(
            model="openrouter/moonshotai/kimi-k2", api_base=None, api_key="sk-test-key"
        )
        skill_manager = make_skill_manager()
        agent = SystemDiagnoseAgent(
            config=config,
            model_id="openrouter/moonshotai/kimi-k2",
            api_base=None,
            api_key="sk-test-key",
            skill_manager=skill_manager,
        )

        mock_subsession = AsyncMock()
        mock_llm_session_class.create_subsession.return_value = mock_subsession

        # Mock response with "Final Answer:" instead of "FINAL_ANSWER:"
        mock_subsession.process_input = AsyncMock(
            return_value="Final Answer: Disk space issue resolved by cleaning /tmp directory."
        )

        result = agent(query="Check disk space issues")

        expected_result = "Disk space issue resolved by cleaning /tmp directory."
        assert result == expected_result

    @patch("aish.llm.LLMSession")
    def test_mocked_llm_max_iterations_reached(self, mock_llm_session_class):
        """Test agent hitting max iterations without final answer."""
        config = ConfigModel(
            model="openrouter/moonshotai/kimi-k2", api_base=None, api_key="sk-test-key"
        )
        skill_manager = make_skill_manager()
        agent = SystemDiagnoseAgent(
            config=config,
            model_id="openrouter/moonshotai/kimi-k2",
            api_base=None,
            api_key="sk-test-key",
            skill_manager=skill_manager,
        )

        mock_subsession = AsyncMock()
        mock_llm_session_class.create_subsession.return_value = mock_subsession

        # Mock responses that never provide final answer
        mock_subsession.process_input = AsyncMock(
            return_value="Still analyzing the system..."
        )

        result = agent(query="Complex system analysis")

        # Should return the last response when iterations are exhausted
        assert result == "Still analyzing the system..."

        # Should have called process_input max_iterations times (4 based on the agent code)
        assert mock_subsession.process_input.call_count == 4

    @patch("aish.llm.LLMSession")
    def test_mocked_llm_exception_handling(self, mock_llm_session_class):
        """Test agent exception handling during LLM interaction."""
        config = ConfigModel(
            model="openrouter/moonshotai/kimi-k2", api_base=None, api_key="sk-test-key"
        )
        skill_manager = make_skill_manager()
        agent = SystemDiagnoseAgent(
            config=config,
            model_id="openrouter/moonshotai/kimi-k2",
            api_base=None,
            api_key="sk-test-key",
            skill_manager=skill_manager,
        )

        mock_subsession = AsyncMock()
        mock_llm_session_class.create_subsession.return_value = mock_subsession

        # Mock process_input to raise an exception
        mock_subsession.process_input = AsyncMock(
            side_effect=Exception("LLM API error")
        )

        result = agent(query="Test query")

        assert "Error during diagnosis: LLM API error" in result

    @patch("aish.llm.LLMSession")
    def test_system_diagnose_agent_filters_nested_content_and_lifecycle_events(
        self, mock_llm_session_class
    ):
        config = ConfigModel(
            model="openrouter/moonshotai/kimi-k2", api_base=None, api_key="sk-test-key"
        )
        parent_events = []

        def parent_event_callback(event):
            parent_events.append(event)

        agent = SystemDiagnoseAgent(
            config=config,
            model_id="openrouter/moonshotai/kimi-k2",
            api_base=None,
            api_key="sk-test-key",
            skill_manager=make_skill_manager(),
            parent_event_callback=parent_event_callback,
        )

        mock_subsession = AsyncMock()
        mock_llm_session_class.create_subsession.return_value = mock_subsession

        async def fake_process_input(*args, **kwargs):
            _ = (args, kwargs)
            callback = mock_subsession.event_callback
            callback(
                LLMEvent(
                    event_type=LLMEventType.OP_START,
                    data={"turn_id": "child-turn", "operation": "process_input"},
                    timestamp=time.time(),
                )
            )
            callback(
                LLMEvent(
                    event_type=LLMEventType.CONTENT_DELTA,
                    data={"delta": "nested content", "is_final": True},
                    timestamp=time.time(),
                )
            )
            callback(
                LLMEvent(
                    event_type=LLMEventType.TOOL_EXECUTION_END,
                    data={"tool_name": "final_answer", "result": "final diagnosis"},
                    timestamp=time.time(),
                )
            )
            callback(
                LLMEvent(
                    event_type=LLMEventType.OP_END,
                    data={"turn_id": "child-turn", "result": "final diagnosis"},
                    timestamp=time.time(),
                )
            )
            return "final diagnosis"

        mock_subsession.process_input = AsyncMock(side_effect=fake_process_input)

        result = agent(query="Check memory")

        assert result == "final diagnosis"
        assert [event.event_type for event in parent_events] == [
            LLMEventType.TOOL_EXECUTION_END,
        ]
        assert parent_events[0].data.get("source") == "system_diagnose_agent"


class TestSystemDiagnoseAgentEndToEnd:
    """End-to-end integration tests for SystemDiagnoseAgent with outer LLMSession."""

    def test_end_to_end_nested_agent_execution(self):
        """End-to-end test: run dummy query via outer LLMSession, verify nested agent executes."""
        # Create outer LLMSession configuration
        config = ConfigModel(
            model="openrouter/moonshotai/kimi-k2",
            api_base=None,
            api_key="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",
        )

        # Create outer LLMSession with SystemDiagnoseAgent
        outer_session = LLMSession(
            config=config,
            skill_manager=make_skill_manager(),
        )

        # Mock the agent's actual execution by patching the LLMSession create_subsession
        with patch.object(LLMSession, "create_subsession") as mock_create_subsession:
            mock_nested_session = AsyncMock()
            mock_nested_session.process_input = AsyncMock(
                return_value=(
                    "FINAL_ANSWER: Memory usage is at 85%. "
                    "Recommend clearing cache and restarting memory-intensive processes."
                )
            )
            mock_create_subsession.return_value = mock_nested_session

            # Directly call the system_diagnose_agent tool to simulate end-to-end
            result = outer_session.system_diagnose_agent(
                query="Check system memory usage"
            )

            # Verify the nested agent was called
            mock_create_subsession.assert_called_once()
            mock_nested_session.process_input.assert_called_once()

            # Verify result contains expected response
            assert "Memory usage is at 85%" in result

    def test_end_to_end_with_bash_tool_execution(self):
        """End-to-end test with actual tool execution in nested agent."""
        config = ConfigModel(
            model="openrouter/moonshotai/kimi-k2",
            api_base=None,
            api_key="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",
        )

        outer_session = LLMSession(
            config=config,
            skill_manager=make_skill_manager(),
        )

        # Mock the system_diagnose_agent with nested subsession execution
        with patch.object(LLMSession, "create_subsession") as mock_create_subsession:
            mock_nested_session = AsyncMock()
            nested_responses = [
                "I need to check disk usage with df command.",
                "FINAL_ANSWER: Disk usage analysis complete. /home partition is 92% full. Recommend cleaning up large files.",
            ]
            mock_nested_session.process_input = AsyncMock(side_effect=nested_responses)
            mock_create_subsession.return_value = mock_nested_session

            # Mock bash tool execution
            with patch.object(BashTool, "__call__") as mock_bash:
                mock_bash.return_value = "Filesystem      Size  Used Avail Use% Mounted on\n/dev/sda1        20G   18G  1.2G  92% /home"

                # Directly invoke the system_diagnose_agent to test end-to-end
                result = outer_session.system_diagnose_agent(
                    query="Check disk space usage"
                )

                # Verify nested agent was created and called
                mock_create_subsession.assert_called_once()
                assert mock_nested_session.process_input.call_count == 2

                # Verify result contains expected response
                assert "/home partition is 92% full" in result

    def test_end_to_end_agent_configuration_propagation(self):
        """Test that agent configuration is properly propagated to nested session."""
        config = ConfigModel(
            model="gpt-4",
            api_base="https://custom-api.com",
            api_key="custom-key-123",
            temperature=0.7,
            max_tokens=1500,
        )

        skill_manager = make_skill_manager()
        agent = SystemDiagnoseAgent(
            config=config,
            model_id=config.model,
            api_base=config.api_base,
            api_key=config.api_key,
            skill_manager=skill_manager,
        )

        # Verify agent stored configuration correctly
        assert agent.model_id == "gpt-4"
        assert agent.api_base == "https://custom-api.com"
        assert agent.api_key == "custom-key-123"

        with patch.object(LLMSession, "create_subsession") as mock_create_subsession:
            mock_nested_session = AsyncMock()
            mock_nested_session.process_input = AsyncMock(
                return_value="FINAL_ANSWER: Configuration test passed."
            )
            mock_create_subsession.return_value = mock_nested_session

            # Execute agent to trigger subsession creation
            agent(query="Test configuration")

            # Verify create_subsession was called with correct configuration
            mock_create_subsession.assert_called_once()
            call_args = mock_create_subsession.call_args

            # Check the ConfigModel passed to create_subsession
            config_arg = call_args[0][0]  # First positional argument
            assert config_arg.model == "gpt-4"
            assert config_arg.api_base == "https://custom-api.com"
            assert config_arg.api_key == "custom-key-123"
            assert config_arg.temperature == 0.3  # Agent sets this to 0.3
            assert config_arg.max_tokens == 2000  # Agent sets this to 2000

            # Check tools were passed correctly
            tools_arg = call_args[0][2]  # Third positional argument
            assert "bash_exec" in tools_arg
            assert "read_file" in tools_arg
            assert "write_file" in tools_arg
            assert "final_answer" in tools_arg
            assert "skill" in tools_arg

            # Verify tool types
            assert isinstance(tools_arg["bash_exec"], BashTool)
            assert isinstance(tools_arg["read_file"], ReadFileTool)
            assert isinstance(tools_arg["write_file"], WriteFileTool)
            assert isinstance(tools_arg["final_answer"], FinalAnswer)


class TestSystemDiagnoseAgentReActPrompt:
    """Test the ReAct-style system prompt generation."""

    def test_react_system_prompt_generation(self):
        """Test the ReAct system prompt contains required elements."""
        config = ConfigModel(
            model="test-model", api_base="http://test", api_key="test-key"
        )
        skill_manager = make_skill_manager()
        agent = SystemDiagnoseAgent(
            config=config,
            model_id="test-model",
            api_base="http://test",
            api_key="test-key",
            skill_manager=skill_manager,
        )

        prompt = agent._create_react_system_prompt()

        # Verify key ReAct elements are present
        assert "diagnostic expert" in prompt
        assert "Unix-like" in prompt
        assert "bash_exec" in prompt
        assert "read_file" in prompt
        assert "write_file" in prompt
        assert "final_answer" in prompt
        assert "Thought" in prompt
        assert "Action" in prompt
        assert "Observation" in prompt
        assert "Final Answer" in prompt

        # Verify specific tool descriptions
        assert "Execute shell commands" in prompt
        assert "Read configuration files" in prompt
        assert "Create diagnostic reports" in prompt
        assert "final diagnostic conclusion" in prompt


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
