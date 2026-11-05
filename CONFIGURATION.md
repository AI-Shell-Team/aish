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
| `max_tokens` | integer/null | `null` | 单次回复的输出 token 上限；不设置时请求不带该字段，由 provider 按模型自身上限决定（Anthropic 端点必填，未设置时发 64k）。**0 到 16383 的值会在加载时被删除并打印一次告警**（`null`/`0` 静默删除；负数不合法，解析直接失败）：这么小的上限会把长推理截断成 `incomplete response`，而 16384 是当前在用模型中最低的输出上限（GPT-4o）。`/setting max_tokens` 会直接拒绝低于 16384 的输入。16384 及以上的值原样保留 | `null`、`0` 或 ≥ 16384 |

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
| `context_auto_compact` | object | 见下方 | 自动上下文压缩配置，面向 shell/tool 输出控量 | 可选 |

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

#### /report（无配置项）

`/report` 把最近一次 AI 回答写成可离线打开的 HTML 页面（issue #608）。回答按 Markdown 渲染，页面自包含（内联 CSS、无脚本、无外部资源），生成过程不重跑命令、不调用模型，也不改动对话上下文。

回答下方是工具证据区：按调用顺序列出本轮每次工具调用的参数与输出，用原生 `<details>` 折叠。调用与结果按 `tool_call_id` 配对，结果缺失或乱序不会错位；状态分为 `正常`、`已截断`（在 4 KB 工具输出上限处被截断）、`仅 offload 预览`（完整内容在磁盘上）和 `无结果`。工具输出一律转义进 `<pre>`，不按 Markdown 解析，日志里的 `<script>`、`#` 只会显示为文本。

输出固定为 `$XDG_DATA_HOME/aish/reports/<session-uuid>/report.html`（`~/.local/share/aish/reports`），目录权限 `0700`、文件权限 `0600`。本地图形会话下生成后直接用默认浏览器打开；SSH（`SSH_CONNECTION`/`SSH_TTY`）或无 `DISPLAY`/`WAYLAND_DISPLAY` 时只打印路径和 `scp` 取回方法，不在远端启动浏览器。

没有 `report.*` 配置项：页面只在用户显式执行 `/report` 时生成，不会按轮次自动写文件，因此也没有开关、自动打开或输出目录需要配置。

#### context_auto_compact

自动上下文压缩用于在发送给模型前控制 shell/tool 输出占用。默认策略偏保守：先做确定性的 microcompact，清理旧命令输出、旧 tool result、preview 和冗长 reasoning；只有在 microcompact 后仍接近窗口上限时，才启用模型生成的 full compact summary。full compact 会额外发起一次无工具、低温、非流式的摘要请求，失败时会记录失败次数并回退到最终 trim，不阻塞普通发送路径。

AI Shell 不会根据模型名前缀推断 context window。对于自定义模型或不同 provider，请优先显式设置 `context_auto_compact.context_window_tokens`，或在 `context_auto_compact.model_context_windows` 中为模型 ID 配置精确窗口；否则会使用 `context_token_budget`，再回退到保守默认值。启动时会记录窗口来源、有效 reserve 和触发阈值，便于排查为什么提前压缩。

| 字段 | 类型 | 默认值 | 描述 |
|------|------|--------|------|
| `enabled` | boolean | `true` | 启用自动上下文压力处理 |
| `full_compact_enabled` | boolean | `true` | 允许在高压时调用模型生成结构化 summary；可关闭为 microcompact-only |
| `context_window_tokens` | integer/null | `null` | 显式 context window 覆盖，优先级高于 `context_token_budget` |
| `model_context_windows` | map[string, integer] | `{}` | 按精确模型 ID 配置 context window，优先级低于 `context_window_tokens`，高于 `context_token_budget` |
| `reserved_output_tokens` | integer/null | `null` | 为模型输出和 summary 预留的 token 数 |
| `auto_compact_buffer_tokens` | integer/null | `null` | effective window 内触发 auto compact 的保守缓冲 |
| `warning_buffer_tokens` | integer/null | `null` | warning 阈值相对 auto compact 的提前量 |
| `blocking_buffer_tokens` | integer/null | `null` | blocking 阈值相对 effective window 的保留量 |
| `micro_keep_recent_messages` | integer | `6` | microcompact 时保留的最近消息数量 |
| `shell_keep_recent_commands` | integer | `8` | microcompact 时完整保留的最近 shell 上下文条数 |
| `max_consecutive_failures` | integer | `3` | full compact 连续失败熔断阈值 |
| `summary_max_tokens` | integer | `4000` | 模型生成结构化 summary 的最大输出预算 |

示例：
```yaml
context_auto_compact:
  enabled: true
  full_compact_enabled: true
  context_window_tokens: null
  model_context_windows:
    openai/glm-5.1: 128000
  micro_keep_recent_messages: 6
  shell_keep_recent_commands: 8
  max_consecutive_failures: 3
```

#### mcp

MCP (Model Context Protocol) 客户端配置，用于接入外部 MCP server 提供的工具。协议版本 `2025-11-25`。`enabled` 为总开关；未配置任何 `servers` 时该小节无副作用。单条 server 的解析失败只会产生启动警告，不会阻止 shell 启动。

顶层字段：

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `enabled` | boolean | `true` | 是否启用 MCP 客户端（无 `servers` 时无副作用） | true/false |
| `request_timeout_ms` | integer | `30000` | 默认单请求超时（毫秒），`0` 表示禁用客户端超时 | 值 >= 0 |
| `max_output_bytes` | integer | `65536` | 单次工具调用返回内容在截断前的默认字节上限 | 值 > 0 |
| `servers` | list | `[]` | MCP server 列表 | 每项见下表 |

`servers` 列表项字段：

| 字段 | 类型 | 默认值 | 描述 | 验证规则 |
|------|------|--------|------|----------|
| `name` | string | 无 | server 唯一名称，同时决定 gateway 工具名 `mcp__<name>`（会做小写化与字符清洗） | 不能为空，重复名称会被忽略并告警 |
| `transport` | string | `stdio` | 传输方式，`stdio` 或 `http`；空值等价 `stdio` | `stdio`\|`http`（`streamable-http` 视为 `http`） |
| `enabled` | boolean | `true` | 是否启动该 server | true/false |
| `command` | string/null | `null` | `stdio` 传输要启动的可执行文件 | `stdio` 时必填，否则该 server 被忽略 |
| `args` | list[string] | `[]` | 传给 `command` 的参数 | 可选 |
| `env` | map[string, string] | `{}` | `stdio` 子进程的额外环境变量，值支持 `${VAR}` 展开 | 可选；未设置变量展开为空串并告警 |
| `cwd` | string/null | `null` | `stdio` 子进程的工作目录 | 可选 |
| `url` | string/null | `null` | `http` 传输的端点地址 | `http` 时必填，否则该 server 被忽略 |
| `headers` | map[string, string] | `{}` | `http` 请求的额外头，值支持 `${VAR}` 展开 | 可选；未设置变量展开为空串并告警 |
| `request_timeout_ms` | integer/null | `null` | 该 server 的单请求超时（毫秒）；`null` 继承顶层，`0` 禁用超时 | 值 >= 0 |
| `trust` | string | `ask` | 默认信任级别；空值等价 `ask` | `ask`\|`allow`\|`block` |
| `side_effects` | string | `unknown` | 声明的副作用范围；空值等价 `unknown` | `read`\|`network`\|`write`\|`unknown` |
| `tools` | map[string, string] | `{}` | 按工具名覆盖信任级别 | 每项 value 为 `ask`\|`allow`\|`block` |
| `max_output_bytes` | integer/null | `null` | 该 server 单次调用返回内容的字节上限；`null` 继承顶层 | 值 > 0 |
| `expose` | string | `gateway` | 工具暴露方式：`gateway` = 只注册一个 `mcp__<server>` 网关工具（按需发现，默认）；`tools` = 额外为每个远端工具注册 `mcp__<server>_<tool>`（带真实 schema，启动时最多等待 3s 拉取目录） | `gateway`\|`tools` |

示例：

```yaml
mcp:
  enabled: true
  request_timeout_ms: 30000
  max_output_bytes: 65536
  servers:
    # stdio server：本地文件系统
    - name: fs
      transport: stdio
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
      env:
        LOG_LEVEL: debug
      cwd: /tmp
      trust: ask
      side_effects: read
      tools:
        read_file: allow
        write_file: block
      max_output_bytes: 262144
      expose: gateway            # 或 tools：把每个工具的 schema 直接暴露给模型

    # http server：远程 GitHub
    - name: github
      transport: http
      url: https://api.example.com/mcp
      headers:
        Authorization: "Bearer ${GITHUB_TOKEN}"
      request_timeout_ms: 15000
      trust: ask
      side_effects: network
```

#### /mcp 命令、gateway 工具与信任模型

`expose: tools` 时该 server 还会为每个远端工具注册一个 `mcp__<server>_<tool>`（真实 schema 进入请求，模型可直接调用；启动期最多等 3s 拉目录，未就绪时仍保留 gateway 工具，可 `/mcp reload` 重试）。默认 `expose: gateway` 下，MCP 工具不会在启动时注入提示词：每个启用的 server 只注册一个 gateway 工具 `mcp__<server>`（约 200 字节的静态 schema），模型按需通过 `action` 参数发现与调用工具，因此 server 的工具清单不会固化进上下文。server 发出 `notifications/tools/list_changed` 时客户端会立刻重新拉取目录（期间该 server 的工具按副作用未知处理、需要确认，刷新落地后恢复），因此新增工具无需手动 `/mcp reload` 即可在 `/mcp tools` 与调用中生效；gateway 工具的系统提示片段也随之刷新。

`/mcp` 子命令：

不带参数直接输入 `/mcp` 时会先打印状态，再弹出**九行交互菜单**（面板支持 1-9 数字快选）：添加 MCP server（向导）、工具、测试连接、设置信任、启用/禁用 server、重连 server、重载全部、移除 server、帮助。每行都会再弹出自己的选择面板：先选 server（选项显示为 `名字 [传输] 状态 · 工具数`），再按需选工具或信任级别（`ask`/`allow`/`block`）——「工具」行的首行是「工具列表」（打印该 server 的全部工具），其余行是各个工具，选中即打印 `/mcp tool` 的详情；「设置信任」行的首行是 `*`（整个 server）。「启用/禁用 server」是同一行：按运行时当前状态自动反向操作（启用中就禁用，否则启用）。全程不需要记忆或手敲任何参数，也不需要手改 config.yaml；`Esc` 直接退出菜单，任一步取消都不执行也不写盘。一个 server 都没配置时菜单只给「添加 MCP server（向导）」一行，添加过程逐步提问（名称、传输、命令或 URL、trust、expose、副作用）。需要脚本化或复制输出时，下面的显式子命令仍完全等价可用（`/mcp status`、`/mcp list` 不会弹菜单）。

| 命令 | 说明 |
|------|------|
| `/mcp` | 打印状态，然后弹出交互菜单（见上）；`/mcp status`、`/mcp list` 只打印状态不弹菜单 |
| `/mcp status`、`/mcp list` | 打印每个 server 的传输、连接状态、工具数、信任级别与声明副作用，末尾给出汇总行；未配置任何 server 时提示运行 `/mcp add` |
| `/mcp help` | 显示全部子命令、quick-add 语法与信任模型说明 |
| `/mcp tools [server]` | 列出某个 server 暴露的工具（名称、副作用、描述）；省略 server 时逐个 server 打印 |
| `/mcp tool <server> <tool>` | 打印单个工具详情：描述、声明/上报/生效副作用、信任级别与判定（`verdict`，即现在调用会不会弹确认）、`inputSchema`（超过 4000 字节截断） |
| `/mcp add` | 交互式向导（见下） |
| `/mcp add <name> ...` | quick-add，一次写盘（见下） |
| `/mcp remove <name>`、`/mcp rm <name>` | 删除 server |
| `/mcp enable <name>` | 启用 server（立即连接） |
| `/mcp disable <name>` | 禁用 server（注销其 gateway 工具） |
| `/mcp trust <server> <tool\|*> <ask\|allow\|block>` | 设置单个工具（`*` 表示整个 server）的信任级别 |
| `/mcp test <name>` | 只读连接测试：不改配置、不动运行中的 manager，连接一次、列出前 10 个工具后关闭；失败时给出提示 |
| `/mcp reload` | 按当前配置重连所有 server，逐条报告失败 |
| `/mcp reconnect <server>` | 重连单个 server |

quick-add 示例：

```bash
/mcp add fs --side-effects read --trust allow -- npx -y @modelcontextprotocol/server-filesystem /tmp
/mcp add remote --url https://api.example.com/mcp --token "$TOKEN" --side-effects network --expose tools
```

quick-add flag：`--transport stdio|http`、`--url <url>`、`--token <t>`（生成 `Authorization: Bearer <t>` 头）、`--trust ask|allow|block`、`--side-effects read|network|write|unknown`、`--timeout <ms>`、`--max-output <bytes>`、`--expose gateway|tools`（工具暴露方式，默认 `gateway`，见上表 `expose` 字段）、`--env KEY=VALUE`、`--header KEY=VALUE`；stdio server 用 `-- <command> [args...]` 传命令行（`--url` 与 `-- <command>` 互斥）。

`/mcp add` 不带参数时进入向导，按顺序询问：名称 → 传输方式（stdio 问命令与可选参数；http 问 URL 与可选 token）→ 信任级别（`ask`/`allow`/`block`）→ 工具暴露方式 `expose`（`gateway`/`tools`）→ 声明副作用（`read`/`network`/`write`/`unknown`）→ 确认（显示名字、transport、命令或 URL、trust、expose、副作用）。任一步取消都不会写盘。

未配置任何 server 时提示 `mcp.servers` 为空；已配置但 `enabled: false` 或没有启用的 server 时提示如何启用。shell 未提供 tokio runtime 时 `reload`/`reconnect`/`test` 不可用。`/mcp <子命令> <server>` 的 server 名支持 Tab 补全，候选来自运行中的 manager 与配置文件名字的并集。

变更立即生效：`add`/`remove`/`enable`/`disable`/`trust` 都先**原子重写 config.yaml**（临时文件 + rename；未知键保留，**注释不保留**），再热插拔运行中的 manager 与 gateway 工具——新启用的 server 立即注册工具，删除/禁用的 server 立即注销其工具，trust 变更立即重建该 server 的 gateway 工具（工具持有注册时抓取的设置快照），无需重启 shell。写盘失败会回滚内存配置并打印错误，磁盘与内存不会不一致。

gateway 工具用法（每个 server 一个工具，通过 `action` 区分）：

| `action` | 其他参数 | 作用 |
|----------|----------|------|
| `list` | 无 | 列出该 server 的工具（名称、副作用、描述） |
| `describe` | `tool` | 返回该工具的 `inputSchema`，用于构造参数 |
| `call` | `tool`、`arguments`（对象） | 调用工具并回传结果（受截断与审计规则约束） |

信任模型（`trust` × 生效副作用）：

| `trust` | 生效副作用 | 决策 |
|---------|-----------|------|
| `allow` | `read` | 免确认，自动执行 |
| `allow` | `network` / `write` / `unknown` | 需要用户确认 |
| `ask` | 任意 | 需要用户确认 |
| `block` | 任意 | 直接拒绝 |

生效副作用 = 用户声明的 `side_effects` 与 server 通过工具注解上报的副作用中更危险者；server 的自我声明只会收紧、不会放宽用户声明。每次批准仅对当次精确参数生效，参数变化需重新确认。

判定基于**当前**注解：若客户端还没拿到该工具的注解、或目录已被 server 标记为过期（`notifications/tools/list_changed`），一律按 `unknown` 处理并要求确认——不会退回成「按用户声明放行」。会话内「本次批准并记住」的精确调用不再弹确认，但 `trust: block` 永远优先：先拒绝再考虑记忆。

`${VAR}` 展开：`env` 与 `headers` 的值支持 `${VAR}` 引用进程环境变量；变量未设置时展开为空串并在启动时告警（避免把字面量凭据发给 server）。凭据（env 值、header 值、URL 查询串与 userinfo、`--token` 形式的 argv）不会出现在 `/mcp` 状态输出、日志或审计记录中；endpoint 展示时已脱敏。

`/mcp` 的配置写入通过原始文档合并进行：只更新它负责的键（`mcp.enabled`、`mcp.request_timeout_ms`、`mcp.servers` 及按名字匹配的 server 条目），`mcp` 小节内的扩展键、每个 server 条目的未知键（例如未来版本新增的字段）以及其他小节都原样保留；写入是「临时文件 + rename」的原子替换，且会沿用原文件权限。

传输层硬性约束（防恶意/异常 server）：

- **子进程环境变量**：stdio server 只继承一个白名单（`HOME`、`LOGNAME`、`PATH`、`SHELL`、`TERM`、`USER`、`TMPDIR`、`LANG`、`LC_ALL`、`LC_CTYPE`、`SSL_CERT_FILE`、`SSL_CERT_DIR` 与 `XDG_*` 路径变量），**不会**拿到 `AISH_API_KEY`、云凭据或 `SSH_AUTH_SOCK` 等父进程敏感变量；需要其它变量时请在 `env:` 中显式声明（同名覆盖白名单值）。
- 单帧上限：stdio 的一行与 HTTP 响应体各 8 MiB，超出即按协议错误结束该请求/连接；SSE 单行另有 1 MiB 上限。这样 `max_output_bytes` 之外也不会被对端撑爆内存。
- 握手超时：无论 `request_timeout_ms` 是否设为 `0`，连接握手本身都有 30 秒上限，避免卡死的 server 让客户端永久等待。
- 重定向：HTTP 仅跟随**同源**（scheme + host + 端口）重定向，最多 5 跳；跨源重定向不跟随，直接按 HTTP 状态错误上报，避免把自定义头、`Mcp-Session-Id` 与请求体（含工具参数）发给第三方主机。
- 取消：用户按 Esc（或超时）时客户端会向 server 发送 `notifications/cancelled`（尽力而为，不阻塞取消路径），并按进程组 SIGTERM → SIGKILL 回收 stdio 子进程（含 `npx`/`uvx` 这类启动器留下的孙进程）。
- `/mcp test`：整体（连接 + 列工具 + 关闭）受 20 秒预算约束，超时先请求取消、再给 5 秒收尾，不会卡住 TUI 线程。

## terminal_resize_mode 说明

- `full`：PTY 命令跟随终端 resize（命令运行中每 100 ms 检测一次，命令开始前也会同步）。默认值。
- `pty_only`：仅 PTY 命令跟随 resize。当前在 PTY 命令路径上与 `full` 等价（打开的面板不会在 resize 时主动重绘）。
- `off`：关闭 resize 跟随，命令沿用 PTY 启动时的尺寸（快速回退/排障用）。该开关作用于常驻会话的 PTY 命令；AI bash 工具的一次性 PTY 回退路径不受它控制。

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
# max_tokens: null   # unset = provider's model limit applies (recommended)

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
context_auto_compact:
  enabled: true
  full_compact_enabled: true
  context_window_tokens: null
  model_context_windows: {}
  reserved_output_tokens: null
  auto_compact_buffer_tokens: null
  warning_buffer_tokens: null
  blocking_buffer_tokens: null
  micro_keep_recent_messages: 6
  shell_keep_recent_commands: 8
  max_consecutive_failures: 3
  summary_max_tokens: 4000

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
# max_tokens 留空：gpt-4-turbo-preview 只接受 4096 个输出 token，
# 而不带该字段时 provider 会按模型自身上限决定，不会越界。
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
max_tokens: -1      # ❌ 错误：必须 > 0
max_tokens: 2000    # ⚠️ 低于 16384，加载时会被删除并给出告警

# 正确的配置
temperature: 1.5    # ✅ 正确
max_tokens: null    # ✅ 正确（不发送该字段，由 provider 按模型上限决定）
max_tokens: 65536   # ✅ 正确（显式限制输出长度，≥ 16384）

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
