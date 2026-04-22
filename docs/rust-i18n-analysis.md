# AISH Rust 代码国际化（i18n）全面分析报告

## 一、执行摘要

当前 AISH 项目已建立基础 i18n 框架（`aish_i18n` crate），但大部分用户可见的字符串仍为硬编码英文。本报告分析了所有需要国际化的内容，并提供了完整的改进方案。

### 关键发现

- **已有框架**: `aish_i18n` 提供 `t()` 和 `t_with_args()` 函数，支持 6 种语言
- **当前使用**: 仅 `aish-shell` 和 `aish-cli` 的部分功能使用 i18n
- **待改进**: 约 **200+** 处用户可见的硬编码英文字符串需要国际化

---

## 二、Crate 级别分析

### 2.1 aish-tools ⚠️ 高优先级

**用户可见程度**: 极高（工具执行结果直接展示给用户）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `bash.rs:120` | `"Failed to execute: {}"` | `tools.bash.execute_failed` |
| `bash.rs:70` | `"Missing 'command' parameter"` | `tools.bash.missing_command` |
| `bash.rs:137` | `"[...{} bytes truncated...]\n{}"` | `tools.bash.output_truncated` |
| `fs.rs:50` | `"Failed to read {}: {}"` | `tools.fs.read_failed` |
| `fs.rs:56` | `"File {} is {} bytes, exceeding the {} byte (32KB) limit"` | `tools.fs.file_too_large` |
| `fs.rs:68` | `"Failed to decode {} as UTF-8: {}"` | `tools.fs.decode_failed` |
| `fs.rs:76` | `"(empty file)"` | `tools.fs.empty_file` |
| `fs.rs:86` | `"Offset {} exceeds file length ({})"` | `tools.fs.offset_exceeds_length` |
| `fs.rs:163` | `"Failed to create parent dirs: {}"` | `tools.fs.create_dirs_failed` |
| `fs.rs:168` | `"Wrote {} bytes to {}"` | `tools.fs.write_success` |
| `fs.rs:169` | `"Failed to write {}: {}"` | `tools.fs.write_failed` |
| `fs.rs:231` | `"Failed to read {}: {}"` | `tools.fs.edit_read_failed` |
| `fs.rs:235` | `"'old_string' not found in {}"` | `tools.fs.old_string_not_found` |
| `fs.rs:244` | `"'old_string' appears {} times in {} - use replace_all=true or provide more context"` | `tools.fs.old_string_ambiguous` |
| `fs.rs:253` | `"Edited {}"` | `tools.fs.edit_success` |
| `fs.rs:254` | `"Failed to write {}: {}"` | `tools.fs.edit_write_failed` |
| `ask_user.rs:13` | `"(type custom answer)"` | `tools.ask_user.custom_input_label` |
| `ask_user.rs:121` | `"Unknown kind: {}"` | `tools.ask_user.unknown_kind` |
| `ask_user.rs:185` | `"Esc to cancel, Enter to select"` | `tools.ask_user.help_select_with_cancel` |
| `ask_user.rs:187` | `"Enter to select"` | `tools.ask_user.help_select_no_cancel` |
| `ask_user.rs:214` | `"Enter your answer:"` | `tools.ask_user.custom_input_prompt` |
| `ask_user.rs:216` | `"Esc to go back"` | `tools.ask_user.custom_input_help_cancel` |
| `ask_user.rs:218` | `"Esc to go back to options"` | `tools.ask_user.custom_input_help_no_cancel` |
| `ask_user.rs:284` | `"Answer is required"` | `tools.ask_user.answer_required` |
| `ask_user.rs:288` | `"Answer too short (min {} characters)"` | `tools.ask_user.answer_too_short` |
| `ask_user.rs:292` | `"User input: {}"` | `tools.ask_user.user_input_prefix` |
| `ask_user.rs:324` | `"Failed to read user input"` | `tools.ask_user.read_input_failed` |
| `memory_tool.rs:107` | `"No matching memories found."` | `tools.memory.no_results` |
| `memory_tool.rs:125` | `"Stored as memory #{}."` | `tools.memory.stored` |
| `memory_tool.rs:133` | `"Forgot memory #{}."` | `tools.memory.forgot` |
| `memory_tool.rs:135` | `"Memory #{} not found."` | `tools.memory.not_found` |
| `memory_tool.rs:141` | `"No memories yet."` | `tools.memory.empty` |
| `memory_tool.rs:149` | `"Unknown action: {}. Use search/store/forget/list."` | `tools.memory.unknown_action` |
| `python.rs:100` | `"Python 3 is not installed or not in PATH."` | `tools.python.not_installed` |
| `python.rs:102` | `"Failed to execute Python: {}"` | `tools.python.execute_failed` |
| `python.rs:87` | `"Python code executed successfully with no output."` | `tools.python.no_output` |
| `grep_tool.rs:78` | `"Error: invalid regex pattern: {}"` | `tools.grep.invalid_regex` |
| `glob_tool.rs:96` | `"Error: invalid glob pattern: {}"` | `tools.glob.invalid_glob` |

#### 工具描述国际化

工具的 `description()` 方法返回的英文描述也需要国际化：

```rust
// 当前
fn description(&self) -> &str {
    "Execute a bash command and return the output. Use this tool to run shell commands."
}

// 建议改为
fn description(&self) -> &str {
    &aish_i18n::t("tools.bash.description")
}
```

---

### 2.2 aish-pty ⚠️ 中优先级

**用户可见程度**: 中（主要是内部错误消息）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `executor.rs:116` | `"failed to openpty: {}"` | `pty.openpty_failed` |
| `executor.rs:129` | `"failed to create stderr pipe: {}"` | `pty.stderr_pipe_failed` |
| `executor.rs:164` | `"fork failed: {}"` | `pty.fork_failed` |
| `executor.rs:339` | `"select error: errno {}"` | `pty.select_error` |
| `executor.rs:115` | `"failed to create offload dir: {}"` | `pty.offload_dir_failed` |
| `executor.rs:121` | `"failed to create overflow file: {}"` | `pty.overflow_file_failed` |
| `state_capture.rs:39` | `"CWD:{}"` | `pty.state_cwd` |
| `rc_wrapper.rs:60` | `"fork failed: {}"` | `pty.rc_fork_failed` |

**注意**: PTY 错误消息多数通过 `AishError::Pty` 传递到上层，应在错误展示层进行国际化。

---

### 2.3 aish-llm ⚠️ 高优先级

**用户可见程度**: 高（LLM 请求错误会直接展示）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `client.rs:69` | `"Connection failed: {}"` | `llm.connection_failed` |
| `client.rs:119` | `"LiteLLM completion error: {}"` | `llm.litellm_completion_error` |
| `client.rs:123` | `"JSON serialization error: {}"` | `llm.json_serialization_error` |
| `client.rs:206` | `"JSON parse error: {}"` | `llm.json_parse_error` |
| `client.rs:279` | `"Connection failed: {}"` | `llm.connection_failed` |
| `client.rs:289` | `"Server returned status {}"` | `llm.server_error_status` |
| `client.rs:392` | `"JSON parse error: {}"` | `llm.json_parse_error` |
| `client.rs:409` | `"API error {}: {}"` | `llm.api_error` |
| `client.rs:411` | `"API error {}: {}\n{}"` | `llm.api_error_with_hint` |
| `models.rs:23` | `"Failed to build HTTP client: {}"` | `llm.http_client_build_failed` |
| `models.rs:39` | `"Bearer {}"` | `llm.bearer_auth` (保留格式) |
| `models.rs:47` | `"Failed to parse response JSON: {}"` | `llm.response_parse_error` |
| `models.rs:51` | `"HTTP {}"` | `llm.http_status` |
| `models.rs:61-65` | 详细错误消息 | `llm.provider_specific_error` |
| `models.rs:74` | `"Failed to build HTTP client: {}"` | `llm.http_client_build_failed` |
| `models.rs:85` | `"Failed to reach Ollama at {}: {}"` | `llm.ollama_unreachable` |
| `models.rs:88` | `"Ollama returned HTTP {}"` | `llm.ollama_http_error` |
| `models.rs:94` | `"Failed to parse Ollama response: {}"` | `llm.ollama_parse_error` |
| `session.rs:160` | `"Unknown tool: {}"` | `llm.unknown_tool` |
| `session.rs:478` | `"Stream error: {}"` | `llm.stream_error` |
| `session.rs:492` | `"Stream error: {}"` | `llm.stream_error` |
| `session.rs:684` | `"Tool execution denied: {}"` | `llm.tool_execution_denied` |
| `agent.rs:366` | `"Error executing tool '{}': {}"` | `llm.tool_execution_error` |
| `agent.rs:371` | `"Observation ({}): {}\n"` | `llm.tool_observation` |

---

### 2.4 aish-shell ⚠️ 高优先级

**用户可见程度**: 极高（主交互界面）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `app.rs:243` | `"error: {}"` | `shell.general_error` |
| `app.rs:407` | `"思考: {:.1}s"` | `shell.thinking_time` |
| `app.rs:559` | `"Analyzing environment..."` | `shell.analyzing_environment` |
| `app.rs:587` | `"  {} {}"` | `shell.check_status` |
| `app.rs:604` | 错误消息 | `shell.security_message` |
| `app.rs:609` | 已使用 i18n | ✅ |
| `app.rs:621` | 提示消息 | `shell.prompt_prefix` |
| `app.rs:643-660` | 确认对话框 | `shell.confirm_dialog.*` |
| `app.rs:776` | `"Failed to initialize readline: {}"` | `shell.readline_init_failed` |
| `app.rs:844` | `"=== Plan Mode ==="` | `shell.plan_mode_enabled` |
| `app.rs:845` | `"Type ; followed by your planning request."` | `shell.plan_mode_hint` |
| `app.rs:848` | `"Exited plan mode."` | `shell.plan_mode_disabled` |
| `app.rs:860` | `"Readline error: {}"` | `shell.readline_error` |
| `app.rs:907` | 纠正命令显示 | `shell.corrected_command` |
| `app.rs:926` | `"Interrupted"` | `shell.interrupted` |
| `app.rs:1000` | `"✓ Plan approved. Implementation tools are now available."` | `shell.plan_approved` |
| `app.rs:1001` | 详细提示 | `shell.plan_approved_hint` |
| `app.rs:1012` | `"→ Changes requested. Re-entering plan mode..."` | `shell.plan_changes_requested` |
| `app.rs:1045` | `"  Feedback sent to AI. Type ; to continue planning."` | `shell.plan_feedback_sent` |
| `app.rs:1049` | `"Plan review cancelled. Plan mode exited."` | `shell.plan_review_cancelled` |
| `app.rs:1050` | `"Use /plan to start a new planning session."` | `shell.plan_review_hint` |
| `app.rs:1198` | `"Unknown special command: {}"` | `shell.unknown_command` |
| `app.rs:1213` | `"Usage: /model [model-name]"` | `shell.model_usage` |
| `app.rs:1241` | `"Warning: could not save config: {}"` | `shell.config_save_warning` |

#### TUI 界面国际化

`tui.rs` 中的选择对话框需要完全国际化：

```rust
// tui.rs:161-180
println!("\x1b[1m{}\x1b[0m", title);           // tui.title
println!("\x1b[36m{}\x1b[0m", question);      // tui.question
println!("  \x1b[33m{}.\x1b[0m {} - {}", ...); // tui.option_with_desc
println!("  \x1b[33m{}.\x1b[0m {}", ...);     // tui.option_simple
println!("  \x1b[33m0.\x1b[0m \x1b[2m(type custom answer)\x1b[0m"); // tui.custom_option
println!("  \x1b[2m(press Enter with empty input to cancel)\x1b[0m"); // tui.cancel_hint
```

---

### 2.5 aish-cli ⚠️ 高优先级

**用户可见程度**: 高（命令行工具）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `models_auth.rs:52` | 认证提示 | `cli.auth_provider_prompt` |
| `models_auth.rs:56` | `"Supported: {}"` | `cli.auth_supported_providers` |
| `models_auth.rs:62` | `"--provider is required."` | `cli.auth_provider_required` |
| `models_auth.rs:63` | `"Example: aish models auth --provider openai-codex"` | `cli.auth_provider_example` |
| `models_auth.rs:68` | `"Models Auth: {}"` | `cli.auth_title` |
| `models_auth.rs:71` | `"Checking for existing auth state..."` | `cli.auth_checking_existing` |
| `models_auth.rs:74-75` | OAuth 提示 | `cli.auth_oauth_not_implemented` |
| `models_auth.rs:78` | `"(--no-open-browser: skipping browser)"` | `cli.auth_no_browser` |
| `models_auth.rs:85` | `"Failed to read token."` | `cli.auth_token_read_failed` |
| `models_auth.rs:91` | `"Token cannot be empty."` | `cli.auth_token_empty` |
| `models_auth.rs:114` | `"Auth configured for {}."` | `cli.auth_configured` |
| `models_auth.rs:116` | `"Default model set to: {}"` | `cli.auth_default_model_set` |
| `models_auth.rs:120` | `"Failed to save config: {}"` | `cli.auth_save_config_failed` |
| `uninstall.rs:154` | 卸载提示 | `cli.uninstall_prompt` |
| `uninstall.rs:167` | 文件列表 | `cli.uninstall_files` |
| `uninstall.rs:343` | 完整卸载提示 | `cli.uninstall_purge_warning` |
| `uninstall.rs:351` | 移除成功/失败 | `cli.uninstall_file_result` |
| `uninstall.rs:373` | `"AI Shell Uninstall"` | `cli.uninstall_title` |
| `uninstall.rs:376` | `"Installation method: {}"` | `cli.uninstall_method` |
| `uninstall.rs:379` | `"--purge: ALL config, data and cache files will be removed."` | `cli.uninstall_purge_description` |
| `uninstall.rs:393` | `"Cancelled."` | `cli.uninstall_cancelled` |
| `uninstall.rs:398` | `"Uninstalling..."` | `cli.uninstall_progress` |
| `uninstall.rs:406` | `"Could not detect installation method."` | `cli.uninstall_unknown_method` |
| `uninstall.rs:407` | `"Attempting to remove current binary..."` | `cli.uninstall_attempting_removal` |
| `uninstall.rs:413` | `"Uninstall failed: {}"` | `cli.uninstall_failed` |
| `uninstall.rs:416` | `"Package removed."` | `cli.uninstall_success` |
| `update.rs:91` | `"HTTP client error: {}"` | `cli.update_http_error` |
| `update.rs:108` | `"Failed to check for updates: {}"` | `cli.update_check_failed` |
| `update.rs:120` | `"Failed to parse releases: {}"` | `cli.update_parse_failed` |
| `update.rs:127` | `"Failed to parse release: {}"` | `cli.update_release_parse_failed` |
| `update.rs:175` | `"Download failed: {}"` | `cli.update_download_failed` |
| `update.rs:192` | `"Failed to create file: {}"` | `cli.update_file_create_failed` |
| `update.rs:201` | `"Download read error: {}"` | `cli.update_download_read_error` |
| `update.rs:206` | `"Write error: {}"` | `cli.update_write_error` |
| `update.rs:240` | `"Open error: {}"` | `cli.update_open_error` |
| `update.rs:245` | `"Read error: {}"` | `cli.update_read_error` |
| `update.rs:265` | `"Failed to create temp dir: {}"` | `cli.update_temp_dir_failed` |
| `update.rs:313` | `"Failed to create extract dir: {}"` | `cli.update_extract_dir_failed` |
| `update.rs:323` | `"Failed to run tar: {}"` | `cli.update_tar_failed` |
| `update.rs:344` | `"Failed to run install script: {}"` | `cli.update_install_script_failed` |

---

### 2.6 aish-security ⚠️ 中优先级

**用户可见程度**: 中（安全警告消息）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `sandbox.rs:89` | `"mkdir overlay: {}"` | `security.sandbox_mkdir_failed` |
| `sandbox.rs:126` | `"bwrap exec: {}"` | `security.sandbox_exec_failed` |
| `sandbox.rs:152` | `"exec: {}"` | `security.sandbox_exec_error` |
| `policy.rs:54` | `"failed to read policy file {:?}: {}"` | `security.policy_read_failed` |
| `policy.rs:58` | `"failed to parse policy YAML: {}"` | `security.policy_parse_failed` |
| `policy.rs:130` | 命中规则消息 | `security.policy_rule_matched` |
| `policy.rs:139` | 命中规则消息 | `security.policy_rule_matched_dir` |
| `policy.rs:153` | 命中规则消息 | `security.policy_rule_matched_write` |
| `policy.rs:172` | 命中危险模式 | `security.policy_dangerous_pattern` |
| `policy.rs:183` | 引用系统路径 | `security.policy_system_path` |
| `manager.rs:93` | `"matched policy rule: {}"` | `security.manager_rule_matched` |
| `manager.rs:131` | 风险评估消息 | `security.manager_risk_assessment` |
| `manager.rs:152` | 沙箱检测消息 | `security.manager_sandbox_detection` |
| `manager.rs:164` | 沙箱检测消息 | `security.manager_sandbox_detection_detail` |
| `sandbox_daemon.rs:121` | `"failed to create socket directory: {}"` | `security.daemon_socket_dir_failed` |
| `sandbox_daemon.rs:129` | Socket 地址显示 | `security.daemon_socket_address` |
| `sandbox_daemon.rs:147` | `"failed to set non-blocking: {}"` | `security.daemon_nonblock_failed` |
| `sandbox_daemon.rs:210` | `"failed to read request: {}"` | `security.daemon_read_failed` |
| `sandbox_daemon.rs:218` | `"failed to parse request: {}"` | `security.daemon_parse_failed` |
| `sandbox_daemon.rs:325` | `"failed to create overlay: {}"` | `security.daemon_overlay_failed` |
| `sandbox_daemon.rs:356` | `"bwrap execution failed: {}"` | `security.daemon_bwrap_failed` |
| `sandbox_daemon.rs:411` | `"failed to serialize response: {}"` | `security.daemon_serialize_failed` |
| `sandbox_daemon.rs:415` | `"failed to send response: {}"` | `security.daemon_send_failed` |
| `sandbox_daemon.rs:418` | `"failed to flush response: {}"` | `security.daemon_flush_failed` |

---

### 2.7 aish-skills ⚠️ 低优先级

**用户可见程度**: 低（主要是开发/调试信息）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `validator.rs:43` | `"{}: skill name is required"` | `skills.validator.name_required` |
| `validator.rs:51` | `"{}: skill description is required"` | `skills.validator.description_required` |
| `validator.rs:58` | `"{}: skill body is empty"` | `skills.validator.body_empty` |
| `validator.rs:67` | 长度警告 | `skills.validator.description_too_long` |
| `validator.rs:75` | 正则警告 | `skills.validator.invalid_regex` |
| `validator.rs:84` | 示例警告 | `skills.validator.no_examples` |
| `validator.rs:108` | 元数据警告 | `skills.validator.missing_metadata` |
| `manager.rs:121` | `"Invalid frontmatter regex: {}"` | `skills.manager.invalid_regex` |
| `manager.rs:135` | `"Invalid YAML frontmatter: {}"` | `skills.manager.invalid_yaml` |

---

### 2.8 aish-scripts ⚠️ 中优先级

**用户可见程度**: 中（脚本执行错误）

#### 需要国际化的内容

| 文件 | 原文 | 建议的 key |
|------|------|-----------|
| `executor.rs:125` | 问题提示 | `scripts.question_prompt` |
| `executor.rs:138` | `"cd: {}: not a directory\n"` | `scripts.cd_not_directory` |
| `executor.rs:277` | `"Error: {}"` | `scripts.execution_error` |
| `loader.rs:108` | `"read error: {}"` | `scripts.read_error` |
| `loader.rs:134` | `"YAML parse error: {}"` | `scripts.yaml_parse_error` |
| `loader.rs:155` | `"metadata parse error: {}"` | `scripts.metadata_parse_error` |

---

### 2.9 其他 Crates

以下 crate 基本不需要国际化（内部实现或已有完整 i18n）：

- ✅ `aish-core`: 仅类型定义
- ✅ `aish-config`: 配置加载，错误已通过 i18n 处理
- ✅ `aish-i18n`: i18n 实现本身
- ✅ `aish-context`: 内部上下文管理
- ✅ `aish-memory`: 记忆系统
- ✅ `aish-prompts`: 提示词模板
- ✅ `aish-session`: 会话持久化

---

## 三、国际化 Key 命名规范

### 3.1 命名结构

```
<crate>.<module>.<specific>.<variant>
```

### 3.2 示例

```
tools.bash.execute_failed         ✅ 好
tools.bash.error                  ❌ 太模糊
bash_execute_failed              ❌ 缺少 crate 前缀

shell.confirm_dialog.title        ✅ 层级清晰
shell.confirm_title              ❌ 模糊
```

### 3.3 特殊规则

1. **错误消息**: 使用 `_error` 或 `_failed` 后缀
2. **成功消息**: 使用 `_success` 后缀
3. **警告消息**: 使用 `_warning` 后缀
4. **占位符**: 使用描述性名称如 `{file}`, `{count}`, `{path}`
5. **UI 元素**: 使用 `_title`, `_prompt`, `_hint` 后缀

---

## 四、实施计划

### 阶段 1: 扩展翻译文件（1-2 天）

1. 在 `en-US.yaml` 和 `zh-CN.yaml` 中添加所有新的 keys
2. 确保所有 keys 都有英文和中文翻译
3. 保持 YAML 结构的一致性

### 阶段 2: 更新 aish-tools（2-3 天）

1. 修改所有工具使用 `aish_i18n::t()` 或 `aish_i18n::t_with_args()`
2. 更新工具的 `description()` 方法
3. 添加单元测试验证 i18n 集成

### 阶段 3: 更新 aish-llm（1-2 天）

1. 替换所有错误消息使用 i18n
2. 更新 LLM 客户端错误处理
3. 确保流式错误消息也使用 i18n

### 阶段 4: 更新 aish-shell（3-4 天）

1. 替换所有 `println!` 和 `eprintln!` 中的硬编码字符串
2. 更新 TUI 对话框
3. 处理 ANSI 颜色码与 i18n 的结合

### 阶段 5: 更新 aish-cli（2-3 天）

1. 更新所有 CLI 命令的错误消息
2. 确保帮助文本使用 i18n
3. 处理 clap 的帮助文本国际化

### 阶段 6: 更新其他 crates（2-3 天）

1. aish-pty, aish-security, aish-skills, aish-scripts
2. 优先处理用户可见的消息
3. 内部调试消息可保持英文

### 阶段 7: 测试与验证（2-3 天）

1. 测试所有语言的显示效果
2. 验证占位符替换正确
3. 检查 ANSI 颜色码兼容性
4. 性能测试（确保 i18n 不影响性能）

---

## 五、技术实施细节

### 5.1 工具代码改造示例

```rust
// Before
impl Tool for BashTool {
    fn execute(&self, args: serde_json::Value) -> ToolResult {
        // ...
        Err(e) => ToolResult::error(format!("Failed to execute: {}", e)),
    }
}

// After
use aish_i18n::t_with_args;
use std::collections::HashMap;

impl Tool for BashTool {
    fn execute(&self, args: serde_json::Value) -> ToolResult {
        // ...
        Err(e) => {
            let mut args = HashMap::new();
            args.insert("error".to_string(), e.to_string());
            ToolResult::error(t_with_args("tools.bash.execute_failed", &args))
        },
    }
}
```

### 5.2 工具描述改造

```rust
// Before
impl Tool for BashTool {
    fn description(&self) -> &str {
        "Execute a bash command and return the output. Use this tool to run shell commands."
    }
}

// After
impl Tool for BashTool {
    fn description(&self) -> &str {
        // Store translated description as static to avoid repeated lookups
        static DESC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        DESC.get_or_init(|| aish_i18n::t("tools.bash.description"))
    }
}
```

### 5.3 ANSI 颜色码处理

```rust
// Before
println!("\x1b[1;33m=== Plan Mode ===\x1b[0m");

// After - Option 1: 包含颜色码在翻译中
println!("{}", t("shell.plan_mode_enabled")); 
// en-US: "\x1b[1;33m=== Plan Mode ===\x1b[0m"
// zh-CN: "\x1b[1;33m=== 计划模式 ===\x1b[0m"

// After - Option 2: 分离颜色和文本（推荐）
use colored::Colorize;
println!("{}", t("shell.plan_mode_enabled").bold().yellow());
// en-US: "=== Plan Mode ==="
// zh-CN: "=== 计划模式 ==="
```

### 5.4 Clap 帮助文本国际化

```rust
// Before
Command::new("models")
    .about("Manage models and provider auth")
    .subcommand(
        SubCommand::with_name("auth")
            .about("Authenticate with a provider")
    )

// After
Command::new("models")
    .about(&aish_i18n::t("cli.models.help"))
    .subcommand(
        SubCommand::with_name("auth")
            .about(&aish_i18n::t("cli.models.auth.help"))
    )
```

---

## 六、测试策略

### 6.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aish_i18n::set_locale;

    #[test]
    fn test_bash_error_chinese() {
        set_locale("zh-CN");
        let result = BashTool::new().execute(serde_json::json!({
            "command": ""
        }));
        assert!(!result.ok);
        assert!(result.output.contains("缺少") || result.output.contains("Missing"));
    }

    #[test]
    fn test_bash_error_english() {
        set_locale("en-US");
        let result = BashTool::new().execute(serde_json::json!({
            "command": ""
        }));
        assert!(!result.ok);
        assert!(result.output.contains("Missing"));
    }
}
```

### 6.2 集成测试

1. 测试完整用户流程在不同语言下的表现
2. 验证错误消息的语言切换
3. 检查占位符替换的正确性

---

## 七、性能考虑

### 7.1 缓存策略

对于频繁访问的翻译（如工具描述），使用 `OnceLock` 缓存：

```rust
use std::sync::OnceLock;

fn tool_description() -> &'static str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| aish_i18n::t("tools.bash.description"))
}
```

### 7.2 避免频繁查找

```rust
// Bad - 每次调用都查找
fn description(&self) -> &str {
    &aish_i18n::t("tools.bash.description")
}

// Good - 缓存结果
fn description(&self) -> &str {
    static DESC: OnceLock<String> = OnceLock::new();
    DESC.get_or_init(|| aish_i18n::t("tools.bash.description"))
}
```

---

## 八、未解决问题与建议

### 8.1 动态错误消息

某些错误消息是动态构建的，需要更灵活的处理：

```rust
// Example
let error = format!("Error in phase {}: {}", phase_name, error_message);
```

建议：使用分层 key 结构
```
errors.phase.{phase_name}.{error_type}
```

### 8.2 工具调用参数

LLM 工具调用的参数 schema（`description` 字段）也需要国际化，但这可能影响 LLM 理解。

建议：
1. 保持参数描述为英文（LLM 训练数据主要是英文）
2. 仅国际化用户可见的工具执行结果

### 8.3 多语言混合

当前实现可能导致用户界面是中文，但 LLM 返回的工具描述是英文。

建议：在提示词中明确告诉 AI 使用用户的语言回复。

---

## 九、总结

### 9.1 工作量估算

- **翻译文件扩展**: 1-2 天
- **aish-tools 改造**: 2-3 天
- **aish-llm 改造**: 1-2 天
- **aish-shell 改造**: 3-4 天
- **aish-cli 改造**: 2-3 天
- **其他 crates 改造**: 2-3 天
- **测试与验证**: 2-3 天

**总计**: 约 15-20 工作日

### 9.2 优先级建议

1. **高优先级**: aish-tools, aish-shell, aish-cli（用户直接可见）
2. **中优先级**: aish-llm, aish-security（错误消息）
3. **低优先级**: aish-pty, aish-skills, aish-scripts（开发/调试信息）

### 9.3 成功标准

- [ ] 所有用户可见的错误消息都支持 i18n
- [ ] 所有 UI 文本都支持 i18n
- [ ] 英文和中文翻译完整且准确
- [ ] 单元测试覆盖所有 i18n 调用
- [ ] 性能无明显下降（< 5%）
- [ ] 代码审查通过

---

## 附录 A: 翻译文件片段示例

```yaml
# en-US.yaml
tools:
  bash:
    description: "Execute a bash command and return the output. Use this tool to run shell commands."
    missing_command: "Missing 'command' parameter"
    execute_failed: "Failed to execute: {error}"
    output_truncated: "[...{bytes} bytes truncated...]\n{tail}"

# zh-CN.yaml
tools:
  bash:
    description: "执行 bash 命令并返回输出。使用此工具运行 shell 命令。"
    missing_command: "缺少 'command' 参数"
    execute_failed: "执行失败: {error}"
    output_truncated: "[...截断了 {bytes} 字节...]\n{tail}"
```

---

**报告生成时间**: 2026-04-22
**分析范围**: AISH Rust 重写分支 (rust-rewrite)
**预计完成时间**: 15-20 工作日
