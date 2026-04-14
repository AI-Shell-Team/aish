from pathlib import Path
from string import Template
from typing import Dict

try:
    from importlib.resources import as_file, files
except ImportError:
    # Python < 3.9 compatibility - this would require adding importlib_resources as dependency
    # For now, fall back to filesystem only
    files = None
    as_file = None


class PromptManager:
    def __init__(self) -> None:
        self.prompts: Dict[str, str] = {}
        self._load_prompts()

    def _load_prompts(self) -> None:
        """Load all prompt templates from the prompts directory"""
        if files is not None and as_file is not None:
            try:
                # Try to load from package resources (works when installed)
                prompts_pkg = files("aish") / "prompts"
                if prompts_pkg.is_dir():
                    for prompt_file in prompts_pkg.iterdir():
                        if prompt_file.name.endswith(".md"):
                            prompt_name = prompt_file.name[:-3]  # Remove .md extension
                            with as_file(prompt_file) as path:
                                with open(path, "r", encoding="utf-8") as f:
                                    self.prompts[prompt_name] = f.read()

                    # inject role prompt into other prompts
                    role = self.prompts["role"]
                    for prompt_name in self.prompts:
                        if prompt_name != "role":
                            self.prompts[prompt_name] = self._partial_format(
                                self.prompts[prompt_name], role=role
                            )
                    return
            except (FileNotFoundError, ModuleNotFoundError):
                pass

        # Fall back to filesystem loading (development mode)
        self._load_from_filesystem()

    def _partial_format(self, template_content: str, **kwargs) -> str:
        """Safely format a template string with only some parameters provided using string.Template"""
        template = Template(template_content)
        return template.safe_substitute(**kwargs)

    def substitute_template(self, template_name: str, **kwargs) -> str:
        """Substitute variables in a named template using string.Template"""
        template_content = self.get_prompt(template_name)
        template = Template(template_content)
        return template.safe_substitute(**kwargs)

    def _load_from_filesystem(self):
        """Load prompts from filesystem (development mode)"""
        current_dir = Path(__file__).parent
        prompts_dir = current_dir / "prompts"

        if prompts_dir.exists():
            for prompt_file in prompts_dir.glob("*.md"):
                prompt_name = prompt_file.stem  # filename without extension
                with open(prompt_file, "r", encoding="utf-8") as f:
                    self.prompts[prompt_name] = f.read()

            # inject role prompt into other prompts (filesystem mode)
            if "role" in self.prompts:
                role = self.prompts["role"]
                for prompt_name in self.prompts:
                    if prompt_name != "role":
                        self.prompts[prompt_name] = self._partial_format(
                            self.prompts[prompt_name], role=role
                        )

    def add_prompt(self, name: str, prompt: str):
        """Add a prompt manually"""
        self.prompts[name] = prompt

    def get_prompt(self, name: str) -> str:
        """Get a prompt by name"""
        return self.prompts.get(name, "")

    def list_prompts(self) -> list[str]:
        """List all available prompt names"""
        return list(self.prompts.keys())

    def reload_prompts(self):
        """Reload all prompts from files"""
        self.prompts.clear()
        self._load_prompts()
