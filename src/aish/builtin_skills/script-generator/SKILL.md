---
name: script-generator
description: Generate script templates based on user requirements. Use when user wants to create a new script or automate a task.
allowed-tools: [bash, read_file, write_file]
---

# Script Generator

Generate aish script templates based on user requirements.

## How to Use

1. Ask the user what task they want to automate
2. Understand the requirements (arguments, steps, AI assistance needs)
3. Generate an appropriate script with YAML frontmatter
4. Save to `~/.config/aish/scripts/<name>.aish`
5. The script will be auto-loaded and available as a command

## Script Template Structure

```yaml
---
name: script-name
description: Brief description of what this script does
arguments:
  - name: arg1
    description: First argument
    default: ""
    required: false
  - name: arg2
    description: Second argument
    default: "default_value"
    required: false
---

# Script body - Bash commands by default
echo "Processing with $AISH_ARG_ARG1 and $AISH_ARG_ARG2..."

# AI call - uses LLM to analyze
ai "Analyze this data and suggest improvements"

# Conditional logic
if [[ "$AISH_ARG_ARG1" == "production" ]]; then
    ai "Review these steps for production safety"
fi
```

## Built-in Script Features

| Feature | Syntax | Description |
|---------|--------|-------------|
| AI call | `ai "prompt"` | Call LLM and return response |
| Change dir | `cd <path>` | Change directory (affects shell) |
| Set env | `export VAR=val` | Set environment variable |
| User input | `ask "prompt"` | Interactive user prompt |
| Exit | `return "value"` | Return result and exit |

## Common Script Patterns

### Deployment Script
```yaml
---
name: deploy
description: Deploy application to server
arguments:
  - name: env
    description: Target environment
    default: staging
  - name: version
    description: Version to deploy
    required: true
---

echo "Deploying $AISH_ARG_VERSION to $AISH_ARG_ENV..."
cd ~/projects/$AISH_ARG_ENV
git pull origin main
./build.sh
./deploy.sh $AISH_ARG_VERSION
ai "Verify the deployment is successful and report status"
```

### Git Workflow Script
```yaml
---
name: feature-start
description: Start a new feature branch
arguments:
  - name: name
    description: Feature name (kebab-case)
    required: true
---

git checkout main
git pull origin main
git checkout -b feature/$AISH_ARG_NAME
echo "✅ Created feature branch: feature/$AISH_ARG_NAME"
```

### System Check Script
```yaml
---
name: syscheck
description: Run system diagnostics
arguments: []
---

echo "Running system diagnostics..."
df -h
free -m
docker ps --format "table {{.Names}}\t{{.Status}}"
ai "Analyze this system status and report any concerns"
```

## Generation Process

1. **Understand Requirements**
   - What task needs automation?
   - What arguments are needed?
   - Should AI assist in any step?

2. **Choose Script Name**
   - Use kebab-case (e.g., `deploy-prod`, `git-sync`)
   - Should be descriptive and memorable

3. **Define Arguments**
   - Required vs optional
   - Default values
   - Descriptions for help text

4. **Write Script Body**
   - Start with validation if needed
   - Use bash for system commands
   - Use `ai "prompt"` for AI assistance
   - Handle errors appropriately

5. **Save and Register**
   - Save to `~/.config/aish/scripts/<name>.aish`
   - Script auto-loads on next command
   - User can run it immediately by name
