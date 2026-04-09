# AISH Script Examples

This directory contains example scripts for learning purposes.

## Built-in Prompt Themes

For production-ready prompt themes, see the `../themes/` directory:

- **powerline.aish** - Colorful segment-based prompt
- **minimal.aish** - Clean, emoji-based prompt
- **developer.aish** - Feature-rich two-line prompt

See `../themes/THEMES.md` for details.

## Creating Custom Scripts

### Hook Scripts

Hook scripts are special scripts that run at specific events:

| Hook | When it runs | Filename |
|------|--------------|----------|
| Prompt | Before showing prompt | `aish_prompt.aish` |
| Precmd | Before each command | `aish_precmd.aish` |
| Postcmd | After each command | `aish_postcmd.aish` |
| Greeting | On shell startup | `aish_greeting.aish` |

### Installation

Copy your hook script to:
```
~/.config/aish/scripts/hooks/<hook_name>.aish
```

### Example: Simple Prompt

```bash
#!/bin/bash
# ~/.config/aish/scripts/hooks/aish_prompt.aish
dir=$(basename "$AISH_CWD")
echo "🚀 $dir > "
```

### Available Variables

See `../themes/THEMES.md` for the full list of environment variables.
