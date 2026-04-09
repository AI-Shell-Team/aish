# AI Shell 配置系统

## 概述

AI Shell 使用现代化的配置系统，支持 YAML 格式配置文件和 Pydantic 类型验证，确保配置的正确性和类型安全。

**配置优先级顺序（从高到低）：**
1. 命令行参数 (最高优先级)
2. 环境变量 (中等优先级)
3. 配置文件 (最低优先级)

## 配置文件

### 位置和格式

- **默认配置文件位置**：`~/.config/aish/config.yaml`
- **自定义配置文件**：可通过 `--config` 选项指定任意路径
- **格式**：YAML
- **目录会自动创建**，首次运行时会生成默认配置文件

如果设置了 `XDG_CONFIG_HOME` 环境变量，配置文件位置为 `$XDG_CONFIG_HOME/aish/config.yaml`。

**使用自定义配置文件：**
```bash
# 使用项目特定的配置
aish run --config ./project-config.yaml

# 使用完整路径
aish run --config /path/to/my-config.yaml

# 使用家目录路径
aish run --config ~/work/ai-shell-config.yaml
```

### 主题脚本目录

- 内置主题脚本资源位于 `src/aish/scripts/themes/`
- 用户自定义主题脚本位于 `~/.config/aish/scripts/themes/`

## 完整配置字段说明

### 基础配置

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `model` | string | `""` | LLM 模型名称 | 必填 |
| `api_base` | string/null | `null` | 自定义 API 基础 URL | 可选 |
| `api_key` | string/null | `null` | API 密钥 | 可选，推荐用环境变量 |
| `codex_auth_path` | string/null | `null` | OpenAI Codex 认证文件路径 | 可选 |
| `temperature` | float | `0.7` | 生成温度 | 0.0 ≤ 值 ≤ 2.0 |
| `max_tokens` | integer | `1000` | 最大令牌数 | 值 > 0 |

### 界面配置

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `prompt_style` | string | `"🚀"` | 提示符样式 | 任意字符串 |
| `theme` | string | `"dark"` | 终端主题 | dark/light |
| `auto_suggest` | boolean | `true` | 启用自动建议 | true/false |
| `history_size` | integer | `1000` | 历史记录大小 | 值 > 0 |
| `output_language` | string/null | `null` | AI 响应的输出语言 | 如：Chinese, English，为空则自动检测 |
| `terminal_resize_mode` | string | `"full"` | 终端窗口变化时的跟随策略 | `full`/`pty_only`/`off` |

### 上下文管理配置

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `max_llm_messages` | integer | `50` | LLM 对话消息最大保留数量 | 值 > 0 |
| `max_shell_messages` | integer | `20` | Shell 历史条目最大保留数量 | 值 > 0 |
| `context_token_budget` | integer/null | `null` | 可选的上下文 token 预算限制 | 如：4000，为 null 则仅使用消息数量限制 |
| `enable_token_estimation` | boolean | `true` | 启用基于 tiktoken 的 token 估算 | true/false |

### 工具输出配置

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `pty_output_keep_bytes` | integer | `4096` | PTY stdout/stderr 在内存中保留的最大字节数 | 值 > 0 |

### 安全配置

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `approved_ai_commands` | list[string] | `[]` | 预批准的 AI 命令列表（精确匹配，仅当沙箱可用时生效） | 字符串列表 |

### 可观测性配置

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `enable_langfuse` | boolean | `false` | 启用 Langfuse 集成 | true/false |

### 会话存储配置

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `session_db_path` | string | `$XDG_DATA_HOME/aish/sessions.db` 或 `~/.local/share/aish/sessions.db` | SQLite 数据库路径 |

### 嵌套配置对象

#### bash_output_offload

Bash 工具输出 offload 行为配置：

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `enabled` | boolean | `true` | 是否启用输出 offload | true/false |
| `threshold_bytes` | integer | `1024` | 触发 offload 的字节数阈值 | 值 > 0 |
| `preview_bytes` | integer | `1024` | 预览显示的字节数 | 值 > 0 |
| `base_dir` | string/null | `null` | offload 文件的基础目录，默认使用 XDG 数据路径 | 可选 |
| `write_meta` | boolean | `true` | 是否写入元数据文件 | true/false |

#### tool_arg_preview

工具参数显示的预览/截断规则：

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `enabled` | boolean | `false` (default) / `true` (final_answer) | 是否启用参数预览截断 | true/false |
| `max_lines` | integer | `3` | 最大显示行数 | 值 > 0 |
| `max_chars` | integer | `240` | 最大显示字符数 | 值 > 0 |
| `max_items` | integer | `4` | 最大显示项目数 | 值 > 0 |

支持按工具名称配置不同规则，默认配置：
```yaml
tool_arg_preview:
  default:
    enabled: false
    max_lines: 3
    max_chars: 240
    max_items: 4
  final_answer:
    enabled: true
    max_lines: 3
    max_chars: 240
    max_items: 4
```

## terminal_resize_mode 说明

- `full`：PTY 命令、ask_user 弹层、Live 渲染都跟随终端 resize（默认）
- `pty_only`：仅 PTY 命令跟随 resize，内置 UI 不做主动刷新
- `off`：关闭 resize 跟随逻辑（用于快速回退/排障）

非法值会自动回退为 `full`。

## 默认配置文件示例

```yaml
# LLM 模型配置
model: ""
api_base: null
api_key: null
codex_auth_path: null

# 生成参数
temperature: 0.7
max_tokens: 1000

# 界面配置
prompt_style: "🚀"
theme: dark
auto_suggest: true
history_size: 1000
output_language: null
terminal_resize_mode: full

# 上下文管理
max_llm_messages: 50
max_shell_messages: 20
context_token_budget: null
enable_token_estimation: true

# 工具输出配置
pty_output_keep_bytes: 4096

# 安全配置
approved_ai_commands: []

# 可观测性
enable_langfuse: false

# 会话存储
session_db_path: ~/.local/share/aish/sessions.db

# Bash 输出 offload 配置
bash_output_offload:
  enabled: true
  threshold_bytes: 1024
  preview_bytes: 1024
  base_dir: null
  write_meta: true

# 工具参数预览配置
tool_arg_preview:
  default:
    enabled: false
    max_lines: 3
    max_chars: 240
    max_items: 4
  final_answer:
    enabled: true
    max_lines: 3
    max_chars: 240
    max_items: 4
```

## 环境变量

以下环境变量会覆盖配置文件的相应设置：

### 应用程序配置

| 环境变量 | 描述 | 对应配置字段 |
|----------|------|-------------|
| `AISH_MODEL` | 覆盖默认模型 | `model` |
| `AISH_API_BASE` | 覆盖 API 基础 URL | `api_base` |
| `AISH_API_KEY` | 覆盖 API 密钥 | `api_key` |
| `AISH_CODEX_AUTH_PATH` | 覆盖 OpenAI Codex 认证文件路径 | `codex_auth_path` |

## 模型认证与状态查看

OpenAI Codex 认证可以通过内置命令完成：

```bash
aish models auth --provider openai-codex
```

如果在无图形界面的环境中使用，可以切换到 device-code 流程：

```bash
aish models auth --provider openai-codex --auth-flow device-code
```

查看当前模型对应的 provider、认证状态以及 dashboard 入口：

```bash
aish models usage
```

`aish models usage` 会优先显示 provider 自有认证状态，例如 OpenAI Codex 的本地 OAuth 认证文件；对于基于 API Key 的 provider，它会显示当前是否已在配置文件或 provider 环境变量中配置凭证，并给出对应的 dashboard 地址。

### API 密钥（LiteLLM 支持）

| 环境变量 | 描述 |
|----------|------|
| `OPENAI_API_KEY` | OpenAI 模型 |
| `ANTHROPIC_API_KEY` | Anthropic (Claude) 模型 |
| `GOOGLE_API_KEY` | Google (Gemini) 模型 |
| `DEEPSEEK_API_KEY` | DeepSeek 模型 |
| `COHERE_API_KEY` | Cohere 模型 |
| `HUGGINGFACE_API_KEY` | HuggingFace 模型 |

**推荐做法**：将 API 密钥设置为环境变量而不是配置文件，以提高安全性。

### Langfuse 环境变量

| 环境变量 | 描述 |
|----------|------|
| `LANGFUSE_PUBLIC_KEY` | Langfuse 公钥 |
| `LANGFUSE_SECRET_KEY` | Langfuse 密钥 |
| `LANGFUSE_HOST` | Langfuse 服务器 URL（默认：https://cloud.langfuse.com） |

## 命令行参数

命令行参数具有最高优先级：

| 参数 | 短参数 | 描述 | 示例 |
|------|--------|------|------|
| `--model` | `-m` | 指定 LLM 模型 | `--model gpt-4` |
| `--api-key` | `-k` | 指定 API 密钥 | `--api-key your-key` |
| `--api-base` | `-b` | 指定 API 基础 URL | `--api-base https://api.openai.com/v1` |
| `--config` | `-c` | 指定配置文件路径 | `--config /path/to/config.yaml` |

## 自定义 API 提供商配置

### OpenRouter 配置示例

```yaml
model: openai/gpt-4-turbo-preview
api_base: https://openrouter.ai/api/v1
api_key: sk-or-v1-your-key-here
temperature: 0.7
max_tokens: 2000
```

### 其他兼容 OpenAI API 的服务

```yaml
model: your-custom-model
api_base: https://your-api-provider.com/v1
api_key: your-api-key
```

### 本地模型 (Ollama)

```yaml
model: ollama/llama3
api_base: http://localhost:11434
```

## Langfuse 集成配置

AI Shell 支持可选的 Langfuse 集成，用于 LLM 调用的观测性和分析。

### 启用 Langfuse 集成

默认情况下，Langfuse 集成是**禁用**的。要启用它，需要：

1. **在配置文件中启用**：
```yaml
enable_langfuse: true
```

2. **设置必要的环境变量**：
```bash
export LANGFUSE_PUBLIC_KEY="your-public-key"
export LANGFUSE_SECRET_KEY="your-secret-key"
export LANGFUSE_HOST="https://cloud.langfuse.com"  # 或你的自托管实例
```

### 安全提示

- 始终使用环境变量设置 Langfuse 密钥，不要写入配置文件
- 定期轮换 API 密钥
- 在生产环境中谨慎使用观测性工具

## 数据存储位置

### 配置目录

- 默认：`~/.config/aish/`
- 或：`$XDG_CONFIG_HOME/aish/`

### 数据目录

- 默认：`~/.local/share/aish/`
- 或：`$XDG_DATA_HOME/aish/`

### 日志位置

默认日志路径：`~/.config/aish/logs/aish.log`

### 会话数据库

默认路径：`~/.local/share/aish/sessions.db`

### Bash 输出 Offload

默认路径：`~/.local/share/aish/offload/`

## 配置验证和错误处理

### Pydantic 类型验证

系统使用 Pydantic 进行配置验证：

```yaml
# 无效的温度值（超出范围）
temperature: 3.0  # ❌ 错误：必须 ≤ 2.0

# 无效的最大令牌数
max_tokens: -1    # ❌ 错误：必须 > 0

# 正确的配置
temperature: 1.5  # ✅ 正确
max_tokens: 2000  # ✅ 正确
```

### 配置文件损坏处理

如果配置文件损坏，系统会：
1. 将损坏的文件备份为 `config.yaml.backup`
2. 使用默认配置重新开始

### 配置迁移

系统支持自动配置迁移：
- `sessions.duckdb` 自动迁移为 `sessions.db`
- 移除已弃用的 `verbose` 字段

## 使用示例

### 1. 基础配置（仅配置文件）

```bash
# 系统会自动创建默认配置，然后编辑
nano ~/.config/aish/config.yaml
```

编辑配置文件：
```yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
temperature: 0.8
```

### 2. 使用环境变量

```bash
# 设置环境变量（推荐方式）
export AISH_MODEL=openai/deepseek-chat
export AISH_API_BASE=https://openrouter.ai/api/v1
export OPENAI_API_KEY=your-api-key

# 运行应用
aish run
```

### 3. 使用命令行参数

```bash
# 命令行参数覆盖所有其他配置
aish run \
  --model gpt-4-turbo \
  --api-key your-openai-key
```

### 4. 输出语言配置示例

```yaml
# 配置中文输出
model: openai/deepseek-chat
output_language: Chinese
temperature: 0.7

# 配置英文输出
model: gpt-4
output_language: English
temperature: 0.7

# 使用自动检测（默认）
model: gpt-4
output_language: null  # 系统将根据系统语言环境自动检测
```

### 5. 上下文管理配置示例

```yaml
# 限制上下文大小以节省 token
max_llm_messages: 30
max_shell_messages: 10
context_token_budget: 4000
enable_token_estimation: true
```

### 6. 预批准命令配置示例

```yaml
# 预批准常用的安全命令（仅当沙箱可用时生效）
approved_ai_commands:
  - "ls -la"
  - "pwd"
  - "echo hello"
```

## 故障排除

### 常见问题和解决方案

1. **配置文件格式错误**
   ```bash
   # 验证 YAML 格式
   python -c "import yaml; yaml.safe_load(open('~/.config/aish/config.yaml'))"
   ```

2. **API 密钥问题**
   ```bash
   # 检查 API 密钥
   echo $OPENAI_API_KEY

   # 查看日志
   tail -n 50 ~/.config/aish/logs/aish.log
   ```

3. **权限问题**
   ```bash
   # 检查配置目录权限
   ls -la ~/.config/aish/

   # 修复权限
   chmod 600 ~/.config/aish/config.yaml
   ```

### 查看当前配置

```bash
# 查看配置文件
cat ~/.config/aish/config.yaml

# 查看配置信息
aish info
```

## 安全建议

1. **API 密钥安全**：
   - 优先使用环境变量而不是配置文件
   - 不要将包含 API 密钥的配置文件提交到版本控制
   - 定期轮换 API 密钥

2. **文件权限**：
   ```bash
   chmod 600 ~/.config/aish/config.yaml  # 仅用户可读写
   ```

3. **环境变量管理**：
   ```bash
   # 添加到 shell 配置文件
   echo 'export OPENAI_API_KEY="your-key"' >> ~/.bashrc
   source ~/.bashrc
   ```
