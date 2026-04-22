# AI Shell - Quick Start Guide

## 🚀 Installation

### Method 1: One-line Install (Recommended)

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

### Method 2: Manual Bundle Install

Download the matching `aish-<version>-linux-<arch>.tar.gz` bundle from the GitHub Release assets for your target version: `https://github.com/AI-Shell-Team/aish/releases`, then run:

```bash
tar -xzf aish-<version>-linux-<arch>.tar.gz
cd aish-<version>-linux-<arch>
sudo ./install.sh
```

### Method 3: Run from Source

```bash
cd /path/to/aish
uv sync
uv run aish
# or
python -m aish
```

## 🎯 Basic Usage

### Start the Shell

```bash
aish          # Equivalent to 'aish run'
aish run      # Explicit run command
```

### Shell Commands (Direct Execution)

All regular shell commands work directly:

```bash
aish> ls -la
aish> cd /path/to/directory
aish> git status
aish> vim /etc/hosts
aish> ssh user@host
```

### AI Commands (Prefix with `;`)

Start with `;` or `；` to enter AI mode:

```bash
aish> ;How do I find all Python files recursively?
aish> ;Explain this command: tar -czf a.tgz ./dir
aish> ;Find files larger than 100M in current directory
aish> ；查找当前目录下占用空间最大的10个文件
```

### Built-in Commands

| Command | Description |
|---------|-------------|
| `help` | Show help message |
| `clear` | Clear screen |
| `exit` / `quit` | Exit the shell |
| `/model` | View or switch model |

## 🔧 CLI Commands

| Command | Description | Example |
|---------|-------------|---------|
| `aish` | Run the shell (default) | `aish` |
| `aish run` | Run the shell | `aish run --model gpt-4` |
| `aish info` | Show information | `aish info` |

### CLI Options for `run`

```bash
aish run --model <model>        # -m: Specify model
aish run --api-key <key>        # -k: Specify API key
aish run --api-base <url>       # -b: Specify API base URL
aish run --config <file>        # -c: Use custom config file
```

## ⚙️ Configuration

### Configuration File Location

Default: `~/.config/aish/config.yaml`

(Or `$XDG_CONFIG_HOME/aish/config.yaml` if `XDG_CONFIG_HOME` is set)

### Configuration Priority

1. **Command line arguments** (highest)
2. **Environment variables**
3. **Config file** (lowest)

### Minimal Configuration

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
api_key: your_api_key
```

### Environment Variables

```bash
export AISH_MODEL="openai/deepseek-chat"
export AISH_API_BASE="https://openrouter.ai/api/v1"
export AISH_API_KEY="your_api_key"
```

> Note: LiteLLM also supports provider-specific env vars like `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`.

### Full Configuration Options

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat      # LLM model to use
api_base: https://openrouter.ai/api/v1  # Custom API base URL
api_key: your_api_key            # API key (prefer env var)
temperature: 0.7                 # Temperature for LLM responses
max_tokens: 1000                 # Maximum tokens for responses
prompt_style: "🚀"               # Prompt style character
theme: dark                      # Shell theme (dark/light)
auto_suggest: true               # Enable auto-suggestions
history_size: 1000               # Maximum history size
output_language: null            # Output language (null for auto-detect)
enable_langfuse: false           # Enable Langfuse observability
```

## 🔄 Different Models and Providers

### OpenRouter (100+ Models)

```bash
# Via CLI
aish run --model openai/gpt-4-turbo-preview --api-base https://openrouter.ai/api/v1

# Via config
model: openai/gpt-4-turbo-preview
api_base: https://openrouter.ai/api/v1
```

### Direct Providers

```bash
# OpenAI
aish run --model gpt-4

# Anthropic Claude
aish run --model claude-3-sonnet-20240229

# Google Gemini
aish run --model gemini-pro

# DeepSeek
aish run --model deepseek-chat
```

### Local Models

```yaml
model: ollama/llama3
api_base: http://localhost:11434
```

## 🛡️ Security and Risk Control

AI Shell performs security assessment only for **AI-generated commands**.

### Risk Levels

| Level | Behavior |
|-------|----------|
| **LOW** | Allowed by default |
| **MEDIUM** | Requires confirmation |
| **HIGH** | Blocked by default |

### Security Policy Files

Priority order:
1. `/etc/aish/security_policy.yaml` (system-wide)
2. `~/.config/aish/security_policy.yaml` (user-level; auto-generated if missing)

### Sandbox Preview (Optional)

Enable in security policy:
```yaml
global:
  enable_sandbox: true
```

Start sandbox service:
```bash
sudo systemctl enable --now aish-sandbox.socket
```

## 🧩 Skills (Plugins)

Skills extend AI capabilities with specialized knowledge.

### Skills Directories

- `~/.config/aish/skills/` (or `$AISH_CONFIG_DIR/skills`)
- `~/.claude/skills/`

### Documentation

See `docs/skills-guide.md` for details.

## 📁 Data and Privacy

### Local Storage

| Type | Default Location |
|------|-----------------|
| Logs | `~/.config/aish/logs/aish.log` |
| Sessions | `~/.local/share/aish/sessions.db` |
| Output offload | `~/.local/share/aish/offload/` |

### Best Practices

- Use environment variables for API keys
- Set proper file permissions: `chmod 600 ~/.config/aish/config.yaml`
- Don't commit API keys to version control

## 🧪 Development and Testing

```bash
# Setup development environment
uv sync --dev

# Run tests
uv run pytest tests/ -v

# Run with coverage
uv run pytest --cov=aish

# Code quality
uv run ruff check src/ tests/
uv run ruff format src/ tests/
uv run mypy src/

# Build wheel package
uv build

# Build standalone binary
./build.sh
```

## 📊 Configuration Examples

### Basic Setup

```yaml
model: gpt-4
temperature: 0.8
enable_langfuse: false
```

### OpenRouter Setup

```yaml
model: openai/gpt-4-turbo-preview
api_base: https://openrouter.ai/api/v1
temperature: 0.7
max_tokens: 2000
```

### With Environment Variables

```bash
# Add to ~/.bashrc or ~/.zshrc
export OPENAI_API_KEY="your-key"
export AISH_MODEL="openai/gpt-4-turbo-preview"
export AISH_API_BASE="https://openrouter.ai/api/v1"
```

## 🆘 Troubleshooting

### Check Configuration

```bash
cat ~/.config/aish/config.yaml
aish info
```

### Verify Installation

```bash
aish --version
aish info
```

### Reinstall Dependencies

```bash
uv sync --reinstall
```

### View Logs

```bash
tail -n 50 ~/.config/aish/logs/aish.log
```

## 🔒 Security Best Practices

1. Use environment variables for API keys
2. Set proper file permissions: `chmod 600 config.yaml`
3. Don't commit API keys to version control
4. Use OpenRouter for unified key management
5. Rotate keys regularly

## 📞 Support

| Resource | Link |
|----------|------|
| Official Website | https://aishell.ai |
| GitHub Repository | https://github.com/AI-Shell-Team/aish/ |
| Bug Reports | https://github.com/AI-Shell-Team/aish/issues |
| Community Discussions | https://github.com/AI-Shell-Team/aish/discussions |
