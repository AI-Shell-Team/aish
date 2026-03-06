from textwrap import dedent

from aish.tools.base import ToolBase

THINK_DESCRIPTION = """"
Use this tool to think about something. It will not obtain new information or change anything, 
but just append the thought to the log. Use it when complex reasoning or reflection is needed.

Before taking any action or responding to the user after receiving tool results, use the think tool as a scratchpad to:
- List the specific rules that apply to the current request
- Check if all required information is collected
- Verify that the planned action complies with all policies
- Iterate over tool results for correctness 

"""

THOUGHT_DESCRIPTION = """
Your thought goes here. This can be structured reasoning, step-by-step analysis,
policy verification, or any other mental process that helps with problem-solving.
"""


class ThinkTool(ToolBase):
    def __init__(self):
        super().__init__(
            name="think",
            description=dedent(THINK_DESCRIPTION),
            parameters={
                "type": "object",
                "properties": {
                    "thought": {
                        "type": "string",
                        "description": dedent(THOUGHT_DESCRIPTION),
                    }
                },
                "required": ["thought"],
            },
        )

    def __call__(self, thought: str) -> str:
        return thought
