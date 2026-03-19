---
name: script-manager
description: Manage user scripts (list, edit, delete, show). Use when user wants to see their scripts, edit or delete existing scripts.
allowed-tools: [bash, read_file, write_file]
---

# Script Manager

Help users manage their aish scripts.

## Capabilities

You can help users with:
- **List scripts**: Show all available scripts with descriptions
- **Show script content**: Display the full content of a specific script
- **Edit script**: Open a script in the user's editor
- **Delete script**: Remove a script file
- **Create script directory**: Ensure the scripts directory exists

## Script Directory

Scripts are stored in: `~/.config/aish/scripts/`

Script files use the `.aish` extension.

## Script Format

Each script has YAML frontmatter followed by bash content:

```yaml
---
name: my-script
description: What this script does
arguments:
  - name: input
    description: Input parameter
    default: ""
    required: false
---

# Bash script content here
echo "Hello, $AISH_ARG_INPUT!"
```

## Available Variables in Scripts

- `$AISH_ARG_<NAME>` - Script argument (uppercase)
- `$AISH_SCRIPT_DIR` - Directory containing the script
- `$AISH_CWD` - Current working directory
- `$AISH_LAST_OUTPUT` - Output from previous command

## Built-in Functions in Scripts

- `ai "prompt"` - Call LLM and return response
- `cd <dir>` - Change directory (affects shell)
- `export VAR=val` - Set environment variable
- `ask "prompt"` - Interactive user prompt

## Usage Examples

When user asks:
- "Show me my scripts" or "List scripts" → List all scripts
- "Show the deploy script" → Display deploy.aish content
- "Edit the build script" → Open build.aish in editor
- "Delete the old-test script" → Remove old-test.aish file
