from pydantic import Field

from aish.skills.models import SkillMetadataInfo
from aish.tools.base import ToolBase
from aish.tools.result import ToolResult

DESCRIPTION_TEMPLATE = """
Execute a skill within the main conversation

<skills_instructions>
When users ask you to perform tasks, check if any of the available skills can help complete the task more effectively. Skills provide specialized capabilities and domain knowledge.

How to invoke:
- Use this tool with the skill name and optional arguments
- Examples:
  - `skill: "pdf"` - invoke the pdf skill
  - `skill: "commit", args: "-m 'Fix bug'"` - invoke with arguments
  - `skill: "review-pr", args: "123"` - invoke with arguments

Important:
- Available skills are listed in system-reminder messages in the conversation
- When a skill matches the user's request, this is a BLOCKING REQUIREMENT: invoke the relevant Skill tool BEFORE generating any other response about the task
- NEVER just announce or mention a skill in your text response without actually calling this tool
- This is a BLOCKING REQUIREMENT: invoke the relevant Skill tool BEFORE generating any other response about the task
- Do not invoke a skill that is already running
- If a skill requires user feedback/choice, use the `ask_user` tool. You can enable custom input via `allow_custom_input`. If the user cancels or UI is unavailable, the task pauses
</skills_instructions>
"""


def render_skills_list_for_reminder(skills: list[SkillMetadataInfo]) -> str:
    if not skills:
        return "- none: No skills available"
    return "\n".join(
        [
            f"- {skill.name}: {skill.description.replace(chr(10), ' ')}"
            for skill in skills
        ]
    )


def render_skills_reminder_text(skills: list[SkillMetadataInfo]) -> str:
    skills_list = render_skills_list_for_reminder(skills)
    return (
        "The following skills are available for use with the Skill tool:\n"
        f"{skills_list}"
    )


class SkillTool(ToolBase):
    skills: list[SkillMetadataInfo] = Field(default_factory=list)

    def __init__(self, skills: list[SkillMetadataInfo]):
        super().__init__(
            name="skill",
            description="",
            parameters={
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": 'The skill name. E.g., "pdf", "commit", "lead-research", etc.',
                    },
                    "args": {
                        "type": "string",
                        "description": "Optional arguments for the skill",
                    },
                },
                "required": ["skill_name"],
            },
        )
        self.skills = list(skills)
        self._refresh_metadata()

    def _refresh_metadata(self) -> None:
        self.description = self._render_description()

    def _render_description(self) -> str:
        return DESCRIPTION_TEMPLATE

    def to_func_spec(self) -> dict:
        self._refresh_metadata()
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        }

    # TODO: implement the skill tool
    def __call__(self, skill_name: str, *args, **kwargs) -> ToolResult:
        if not skill_name or not skill_name.strip():
            return ToolResult(ok=False, output="Error: skill_name is required")

        available_skills = {skill.name for skill in self.skills}
        if skill_name not in available_skills:
            available = (
                ", ".join(sorted(available_skills)) if available_skills else "none"
            )
            return ToolResult(
                ok=False,
                output=f"Error: Unknown skill: {skill_name}. Available skills: {available}",
            )
        return ToolResult(ok=True, output=f"Launching skill: {skill_name}")
