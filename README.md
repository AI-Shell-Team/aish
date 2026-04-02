<div align="center">

English | [简体中文](README_CN.md)

---

# AISH

Empower the Shell to think. Evolve Operations.

[![Official Website](https://img.shields.io/badge/Website-aishell.ai-blue.svg)](https://www.aishell.ai)
[![GitHub](https://img.shields.io/badge/GitHub-AI--Shell--Team/aish-black.svg)](https://github.com/AI-Shell-Team/aish/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Platform](https://img.shields.io/badge/platform-linux-lightgrey.svg)](#)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

![](./docs/images/demo_show.gif)

**A Real AI Shell: Complete PTY + Configurable Security & Risk Control**



</div>

---

## Table of Contents

- [Why Choose AISH](#why-choose-aish)
- [Quick Start](#quick-start)
- [Installation](#installation)
- [Uninstallation](#uninstallation)
- [Configuration](#configuration)
- [Usage](#usage)
- [Security & Risk Control](#security--risk-control)
- [Skills (Plugins)](#skills-plugins)
- [Data & Privacy](#data--privacy)
- [Documentation](#documentation)
- [Community & Support](#community--support)
- [Development & Testing](#development--testing)
- [Contributing](#contributing)
- [License](#license)

---

## Why Choose AISH

- **True Interactive Shell**: Full PTY support, runs interactive programs like `vim` / `ssh` / `top`
- **AI Native Integration**: Describe tasks in natural language, generate, explain and execute commands
- **Secure & Controllable**: AI commands have risk grading and confirmation flow; optional sandbox pre-run for change assessment
- **Extensible**: Skills plugin system with hot loading and priority override
- **Low Migration Cost**: Compatible with regular commands and workflows, everything in terminal by default

---

## Feature Comparison

| Feature | AISH | Claude Code |
|---------|------|-------------|
| 🎯 **Core Positioning** | Ops/System Troubleshooting CLI | Development Coding Assistant |
| 🤖 **Multi-Model Support** | ✅ Fully Open | ⚠️ Mainly Claude |
| 🔧 **Sub-Agent System** | ✅ ReAct Diagnostic Agent | ✅ Multiple Agent Types |
| 🧩 **Skills Support** | ✅ Hot Loading | ✅ |
| 🖥️ **Native Terminal Integration** | ✅ Full PTY Support | ⚠️ Limited Support |
| 🛡️ **Security Risk Assessment** | ✅ Security Confirmation | ✅ Security Confirmation |
| 🌐 **Local Model Support** | ✅ Fully Supported | Fully Supported  |
| 📁 **File Operation Tools** | ✅ Minimal Essential Suppport | ✅ Full Support |
| 💰 **Completely Free** | ✅ Open Source | ❌ Paid Service |
| 📊 **Observability** | ✅ Langfuse Optional | ⚠️ Built-in |
| 🌍 **Multi-language Output** | ✅ Auto Detection | ✅ |

---

## Quick Start

### 1) Install and Launch

#### Option 1: One-line install (Recommended)

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

#### Option 2: Manual bundle install

Download the matching `aish-<version>-linux-<arch>.tar.gz` bundle from the official release directory, then run:

```bash
tar -xzf aish-<version>-linux-<arch>.tar.gz
cd aish-<version>-linux-<arch>
sudo ./install.sh
```

Then launch:

```bash
aish
```

Note: `aish` without subcommands is equivalent to `aish run`.

### 2) Use Like a Regular Shell

```bash
aish> ls -la
aish> cd /etc
aish> vim hosts
```

### 3) Let AI Do the Work (Start with ;)

Starting with `;` or `；` enters AI mode:

```bash
aish> ;find files larger than 100M in current directory and sort by size
aish> ;explain this command: tar -czf a.tgz ./dir
```

---

## Installation

### Linux Release Bundle

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

The installer resolves the latest release directory under `https://www.aishell.ai/repo`, downloads the matching bundle for your architecture, and installs `aish`, `aish-sandbox`, and `aish-uninstall` into `/usr/local/bin`.

### Run from Source (Development/Trial)

```bash
uv sync
uv run aish
# or
python -m aish
```

---

## Uninstallation

Uninstall (keep configuration files):

```bash
sudo aish-uninstall
```

Complete uninstallation (also removes system-level security policies):

```bash
sudo aish-uninstall --purge-config
```

Optional: Clean user-level configuration (will clear model/API keys etc.):

```bash
rm -rf ~/.config/aish
```

---

## Configuration

### Configuration File Location

- Default: `~/.config/aish/config.yaml` (or `$XDG_CONFIG_HOME/aish/config.yaml` if `XDG_CONFIG_HOME` is set)

### Priority (High to Low)

1. Command-line arguments
2. Environment variables
3. Configuration file

### Minimal Configuration Example

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
api_key: your_api_key
```

Alternatively via environment variables (more suitable for secrets):

```bash
export AISH_MODEL="openai/deepseek-chat"
export AISH_API_BASE="https://openrouter.ai/api/v1"
export AISH_API_KEY="your_api_key"

```

> Tip: LiteLLM also supports reading vendor-specific environment variables (e.g., `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`).

Interactive configuration (optional):

```bash
aish setup
```

Tool calling compatibility check (confirm selected model/channel supports tool calling):

```bash
aish check-tool-support --model openai/deepseek-chat --api-base https://openrouter.ai/api/v1 --api-key your_api_key
```

Langfuse (optional observability):

1) Enable in configuration:

```yaml
enable_langfuse: true
```

2) Set environment variables:

```bash
export LANGFUSE_PUBLIC_KEY="..."
export LANGFUSE_SECRET_KEY="..."
export LANGFUSE_HOST="https://cloud.langfuse.com"
```

`aish check-langfuse` will run checks when `check_langfuse.py` exists in project root.

---

## Usage

### Common Input Types

| Type | Example | Description |
|:----:|---------|-------------|
| Shell Commands | `ls -la`, `cd /path`, `git status` | Execute regular commands directly |
| AI Requests | `;how to check port usage`, `;find files larger than 100M` | Enter AI mode with `;`/`；` prefix |
| Built-in Commands | `help`, `clear`, `exit`, `quit` | Shell built-in control commands |
| Model Switching | `/model gpt-4` | View or switch model |

### Shell Compatibility (PTY)

```bash
aish> ssh user@host
aish> top
aish> vim /etc/hosts
```

---

## Security & Risk Control

AI Shell only performs security assessment on **AI-generated and ready-to-execute** commands.

### Risk Levels

- **LOW**: Allowed by default
- **MEDIUM**: Confirmation before execution
- **HIGH**: Blocked by default

### Security Policy File Path

Policy files are resolved in this order:
1. `/etc/aish/security_policy.yaml` (system-level)
2. `~/.config/aish/security_policy.yaml` (user-level; auto-generated template if not exists)

### Sandbox Pre-run (Optional, Recommended for Production)

Default policy has sandbox pre-run **disabled**. To enable:

1) Set in security policy:

```yaml
global:
  enable_sandbox: true
```

2) Start privileged sandbox service (systemd):

```bash
sudo systemctl enable --now aish-sandbox.socket
```

Default socket: `/run/aish/sandbox.sock`.
When sandbox is unavailable, it will fallback according to `sandbox_off_action` (BLOCK/CONFIRM/ALLOW) in policy.

---

## Skills (Plugins)

Skills extend AI's domain knowledge and workflows, supporting hot loading and priority override.

Default scan directories and priority:
- `~/.config/aish/skills/` (or `$AISH_CONFIG_DIR/skills`)
- `~/.claude/skills/`

Packaged versions will attempt to copy system-level skills to user directory on first launch (e.g., `/usr/share/aish/skills`).

For more details, see: `docs/skills-guide.md`

---

## Data & Privacy

This project stores the following data locally (for troubleshooting and traceability):

- **Logs**: Default `~/.config/aish/logs/aish.log`
- **Sessions/History**: Default `~/.local/share/aish/sessions.db` (SQLite)
- **Large Output Offload**: Default `~/.local/share/aish/offload/`

Recommendations:
- Don't commit real API keys to repository; prefer environment variables or secret management systems.
- Production environments can combine security policies to limit AI-accessible directory scope.

---

## Documentation

- Configuration Guide: `CONFIGURATION.md`
- Quick Start: `QUICKSTART.md`
- Skills Usage: `docs/skills-guide.md`
- Command Correction Mechanism: `docs/command-interaction-correction.md`

---

## Community & Support

| Link | Description |
|------|-------------|
| [Official Website](https://www.aishell.ai) | Project homepage and more information |
| [GitHub Repository](https://github.com/AI-Shell-Team/aish/) | Source code and issue tracking |
| [GitHub Issues](https://github.com/AI-Shell-Team/aish/issues) | Bug reports |
| [GitHub Discussions](https://github.com/AI-Shell-Team/aish/discussions) | Community discussions |
| [Discord](https://discord.com/invite/Pw2mjZt3) | Join the community |
| [LINUX DO](https://linux.do) | Community friend link |


> If you find AISH helpful, please consider giving us a Star ⭐
>It helps more people discover the project and keeps us motivated to continue improving it.


---

## Development & Testing

```bash
uv sync
uv run aish
uv run pytest
```

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.
---

## License

`LICENSE` (Apache 2.0)
