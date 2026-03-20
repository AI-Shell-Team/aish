---
name: prompt-designer
description: Design and customize prompts for aish shell. Use when user wants to modify their shell prompt, describe desired prompt style, or switch between prompt themes. Triggers on keywords like "prompt", "提示符", "主题".
allowed-tools: [bash, read_file, write_file]
---

# Prompt Designer

Help users design and customize prompts for aish shell.

## Workflow

### Step 1: Check or Initialize

```bash
# Check if user has a prompt file
if [[ ! -f ~/.config/aish/scripts/hooks/aish_prompt.aish ]]; then
    # Create from template
    mkdir -p ~/.config/aish/scripts/hooks
    cp <aish-src>/src/aish/scripts/templates/aish_prompt.aish ~/.config/aish/scripts/hooks/
fi

# Read current content
cat ~/.config/aish/scripts/hooks/aish_prompt.aish
```

### Step 2: Modify or Regenerate

**Modify existing:**
- Read current file
- Apply only requested changes
- Preserve existing customizations

**Regenerate (when user asks):**
- "重写", "重新生成", "create new"
- Generate completely new prompt

## Template Location

```
<aish-src>/src/aish/scripts/templates/aish_prompt.aish
```

## User Hook Location

```
~/.config/aish/scripts/hooks/aish_prompt.aish
```

## Environment Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `AISH_CWD` | Current working directory | `/home/user/project` |
| `AISH_EXIT_CODE` | Last command exit code | `0`, `1`, `127` |
| `AISH_GIT_REPO` | "1" if in git repository | `1` or unset |
| `AISH_GIT_BRANCH` | Current branch name | `main`, `feature-x` |
| `AISH_GIT_STATUS` | Repository status | `clean`, `staged`, `dirty` |
| `AISH_GIT_STAGED` | Number of staged files | `3` |
| `AISH_GIT_MODIFIED` | Number of modified files | `11` |
| `AISH_GIT_UNTRACKED` | Number of untracked files | `9` |
| `AISH_GIT_AHEAD` | Commits ahead of upstream | `2` |
| `AISH_GIT_BEHIND` | Commits behind upstream | `1` |
| `AISH_VIRTUAL_ENV` | Virtual environment name | `.venv`, `myenv` |

## ANSI Colors

```bash
R=$'\033[0m'      # Reset
B=$'\033[1m'      # Bold
D=$'\033[2m'      # Dim
RD=$'\033[31m'    # Red
G=$'\033[32m'     # Green
Y=$'\033[33m'     # Yellow
BL=$'\033[34m'    # Blue
M=$'\033[35m'     # Magenta
C=$'\033[36m'     # Cyan
```

## Default Output

```
:~/n/x/g/aish|main● +1 ↑2 ➜
```

## Example Interactions

| User Request | Action |
|--------------|--------|
| "修改 prompt" | Init from template if needed → Read → Modify |
| "把路径改成红色" | Read → Change BL to RD in path section |
| "添加时间显示" | Read → Add `$(date +%H:%M)` at start |
| "重写 prompt" | Generate fresh from scratch |

## Tips

- Always use `printf '%s'` not `echo`
- Use `$'...'` for ANSI color variables
- Handle missing variables: `${VAR:-default}`
- Keep execution under 100ms
