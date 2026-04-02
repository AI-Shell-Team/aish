<div align="center">

[English](README.md) | 简体中文

---

# AISH

让 Shell 拥有思考力，让运维由此进化

[![Official Website](https://img.shields.io/badge/官网-aishell.ai-blue.svg)](https://www.aishell.ai)
[![GitHub](https://img.shields.io/badge/GitHub-AI--Shell--Team/aish-black.svg)](https://github.com/AI-Shell-Team/aish/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Platform](https://img.shields.io/badge/platform-linux-lightgrey.svg)](#)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

![](./docs/images/demo_show.gif)

**一个真实可用的 AI Shell：完整 PTY + 可配置的安全与风险控制**

</div>

---

## 目录

- [为什么选择 AISH](#为什么选择-ai-shell)
- [快速开始](#快速开始)
- [安装](#安装)
- [卸载](#卸载)
- [配置](#配置)
- [使用方式](#使用方式)
- [安全与风险控制](#安全与风险控制)
- [Skills（插件）](#skills插件)
- [数据与隐私](#数据与隐私)
- [文档](#文档)
- [开发与测试](#开发与测试)
- [贡献](#贡献)
- [许可证](#许可证)

---

## 为什么选择 AISH

- **真正的交互式 Shell**：完整 PTY 支持，可运行 `vim` / `ssh` / `top` 等交互程序
- **AI 原生集成**：用自然语言描述任务，生成、解释并执行命令
- **安全可控**：AI 命令有风险分级与确认流程；可选沙箱预跑做变更评估
- **可扩展**：Skills 插件系统，支持热加载与优先级覆盖
- **低迁移成本**：兼容常规命令与工作流，默认在终端里完成

---

## 功能对比

| 特性 | AISH | Claude Code |
|------|------|-------------|
| 🎯 **核心定位** | 运维/系统排障 CLI | 开发编码助手 |
| 🤖 **多模型支持** | ✅ 完全开放 | ⚠️ 主要 Claude |
| 🔧 **子代理系统** | ✅ ReAct 诊断代理 | ✅ 多类型代理 |
| 🧩 **Skills 支持** | ✅ 热加载 | ✅ |
| 🖥️ **原生终端集成** | ✅ PTY 完整支持 | ⚠️ 有限支持 |
| 🛡️ **安全风险评估** | ✅ 安全确认 | ✅ 安全确认 |
| 🌐 **本地模型支持** | ✅ 完全支持 | ✅ 完全支持 |
| 📁 **文件操作工具** | ✅ 核心能力支持 | ✅ 完整支持 |
| 💰 **完全免费** | ✅ 开源 | ❌ 付费服务 |
| 📊 **可观测性** | ✅ Langfuse 可选 | ⚠️ 内置 |
| 🌍 **多语言输出** | ✅ 自动检测 | ✅ |

## 快速开始

### 1) 安装并启动

#### 方式一：一键安装（推荐）

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

#### 方式二：手动安装二进制 bundle

先从官方发布目录下载对应架构的 `aish-<version>-linux-<arch>.tar.gz`，然后执行：

```bash
tar -xzf aish-<version>-linux-<arch>.tar.gz
cd aish-<version>-linux-<arch>
sudo ./install.sh
```

然后启动：

```bash
aish
```

说明：`aish` 无子命令时等价于 `aish run`。

### 2) 像普通 Shell 一样用

```bash
aish> ls -la
aish> cd /etc
aish> vim hosts
```

### 3) 用 AI 做事（首字符输入 ；）

以 `;` 或 `；` 开头会进入 AI 模式：

```bash
aish> ;查找当前目录大于 100M 的文件并按大小排序
aish> ;解释一下这个命令：tar -czf a.tgz ./dir
```

---

## 安装

### Linux 二进制 Bundle

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

该安装器会在 `https://www.aishell.ai/repo` 下解析最新发布目录，下载与你当前架构匹配的 bundle，并把 `aish`、`aish-sandbox` 和 `aish-uninstall` 安装到 `/usr/local/bin`。

### 从源码运行（开发/试用）

```bash
uv sync
uv run aish
# 或
python -m aish
```

---

## 卸载

卸载（保留配置文件）：

```bash
sudo aish-uninstall
```

彻底卸载（同时删除系统级安全策略）：

```bash
sudo aish-uninstall --purge-config
```

可选：清理用户级配置（会清空模型/API Key 等）：

```bash
rm -rf ~/.config/aish
```

---

## 配置

### 配置文件位置

- 默认：`~/.config/aish/config.yaml`（如设置了 `XDG_CONFIG_HOME`，则在 `$XDG_CONFIG_HOME/aish/config.yaml`）

### 优先级（从高到低）

1. 命令行参数
2. 环境变量
3. 配置文件

### 最小配置示例

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
api_key: your_api_key
```

也可通过环境变量（更适合放置密钥）：

```bash
export AISH_MODEL="openai/deepseek-chat"
export AISH_API_BASE="https://openrouter.ai/api/v1"
export AISH_API_KEY="your_api_key"

```

> 提示：LiteLLM 也支持读取特定厂商的环境变量（如 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY`）。

交互式配置（可选）：

```bash
aish setup
```

工具调用兼容性检查（确认所选模型/渠道支持 tool calling）：

```bash
aish check-tool-support --model openai/deepseek-chat --api-base https://openrouter.ai/api/v1 --api-key your_api_key
```

Langfuse（可选观测性）：

1) 在配置里打开：

```yaml
enable_langfuse: true
```

2) 设置环境变量：

```bash
export LANGFUSE_PUBLIC_KEY="..."
export LANGFUSE_SECRET_KEY="..."
export LANGFUSE_HOST="https://cloud.langfuse.com"
```

`aish check-langfuse` 会在项目根目录存在 `check_langfuse.py` 时执行检查。

---

## 使用方式

### 常用输入类型

| 类型 | 示例 | 说明 |
|:----:|------|------|
| Shell 命令 | `ls -la`、`cd /path`、`git status` | 直接执行常规命令 |
| AI 请求 | `;如何查看端口占用`、`；查找大于100M的文件` | 以 `;`/`；` 前缀进入 AI 模式 |
| 内置命令 | `help`、`clear`、`exit`、`quit` | Shell 内置控制命令 |
| 模型切换 | `/model gpt-4` | 查看或切换模型 |

### Shell 兼容性（PTY）

```bash
aish> ssh user@host
aish> top
aish> vim /etc/hosts
```

---

## 安全与风险控制

AI Shell 仅对 **AI 生成并准备执行** 的命令进行安全评估。

### 风险分级

- **LOW**：默认放行
- **MEDIUM**：执行前确认
- **HIGH**：默认阻断

### 安全策略文件路径

策略文件按以下顺序解析：
1. `/etc/aish/security_policy.yaml`（系统级）
2. `~/.config/aish/security_policy.yaml`（用户级；若不存在会自动生成模板）

### 沙箱预跑（可选，推荐生产启用）

默认策略 **未开启** 沙箱预跑。启用方法：

1) 在安全策略中设置：

```yaml
global:
  enable_sandbox: true
```

2) 启动特权沙箱服务（systemd）：

```bash
sudo systemctl enable --now aish-sandbox.socket
```

Socket 默认：`/run/aish/sandbox.sock`。
沙箱不可用时，会按策略里的 `sandbox_off_action`（BLOCK/CONFIRM/ALLOW）兜底。

---

## Skills（插件）

Skills 用于扩展 AI 的专用知识与工作流，支持热加载与覆盖优先级。

默认扫描目录与优先级：
- `~/.config/aish/skills/`（或 `$AISH_CONFIG_DIR/skills`）
- `~/.claude/skills/`

打包版本会在首次启动时尝试把系统级技能复制到用户目录（如 `/usr/share/aish/skills`）。

更多说明见：`docs/skills-guide.md`

---

## 数据与隐私

本项目会在本地存储以下数据（便于排障与可追溯性）：

- **日志**：默认 `~/.config/aish/logs/aish.log`
- **会话/历史**：默认 `~/.local/share/aish/sessions.db`（SQLite）
- **大输出 offload**：默认 `~/.local/share/aish/offload/`

建议：
- 不要把真实 API Key 提交到仓库；优先使用环境变量或密钥管理系统。
- 生产环境可结合安全策略限制 AI 触达目录范围。

---

## 文档

- 配置说明：`CONFIGURATION.md`
- 快速入门：`QUICKSTART.md`
- Skills 使用：`docs/skills-guide.md`
- 命令纠错机制：`docs/command-interaction-correction.md`

---

## 社区与支持

| 链接 | 说明 |
|------|------|
| [官方网站](https://www.aishell.ai) | 项目官网与更多信息 |
| [GitHub 仓库](https://github.com/AI-Shell-Team/aish/) | 源码与问题反馈 |
| [GitHub Issues](https://github.com/AI-Shell-Team/aish/issues) | Bug 报告 |
| [GitHub Discussions](https://github.com/AI-Shell-Team/aish/discussions) | 社区讨论 |
| [Discord](https://discord.com/invite/Pw2mjZt3) | 加入社区 |
| [LINUX DO 社区](https://linux.do) | 友链 |


> 如果 AISH 对你有帮助，欢迎给项目点一个 Star ⭐  
> 这会帮助更多人发现它，也是我们持续迭代的重要动力。



---

## 开发与测试

```bash
uv sync
uv run aish
uv run pytest
```

---

## 贡献

请参阅 [CONTRIBUTING.md](CONTRIBUTING.md) 了解贡献指南。
---

## 许可证

`LICENSE`（Apache 2.0）
