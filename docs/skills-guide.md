# Skill 配置使用手册

## 概述

AI Shell 支持通过 **Skill** 系统扩展 AI 的能力。Skill 是一种包含专门知识和工作流程的可重用组件，可以帮助 AI 更有效地完成特定任务。


## Skill 源与优先级

Skill 从以下两个源按优先级加载（高到低）：

```
USER (最高优先级)
  └── ~/.config/aish/skills/      # 用户配置目录（或 $AISH_CONFIG_DIR/skills）

CLAUDE (较低优先级)
  └── ~/.claude/skills/           # Claude Desktop 共享的 skills 目录
```

当多个源存在同名 Skill 时，**优先级高的源会覆盖优先级低的源**。



## 安装 Skill

### 方法一：用户级安装（推荐）

将 Skill 目录放置在用户配置目录：

```bash
# 创建 skills 目录（如果不存在）
mkdir -p ~/.config/aish/skills

# 复制你的 skill 到该目录
cp -r your-skill ~/.config/aish/skills/
```

### 方法二：使用 Claude Desktop 的 Skills

如果你已经安装了 Claude Desktop 并配置了 Skills：

```bash
# Skills 会自动从 ~/.claude/skills/ 加载
# 无需额外操作
```

## 使用 Skill

### 自动加载

Skill 会在 AI Shell 启动时自动加载；运行期间通过文件事件监听 `~/.config/aish/skills` 与 `~/.claude/skills` 目录变更，并在**下一次**需要构建工具列表或调用 Skill 时自动重新加载（lazy reload），无需重启。任一 Skill 目录内的文件新增/删除/修改都会触发失效与下次重载；当 skills 目录被新建或删除时，会自动重新建立监听。

### AI 自动调用

AI 会根据任务自动选择合适的 Skill。你可以这样使用：

```bash
# 直接描述需要完成的任务，AI 会自动选择合适的 Skill
aish> 帮我把这个 PDF 转换成文本
# AI 会自动调用 pdf skill

aish> 提交这次的代码修改
# AI 会自动调用 commit skill
```

### 显式调用 Skill

你也可以明确指定使用某个 Skill：

```bash
aish> 使用 pdf skill 处理这个文件
aish> 运行 commit skill，提交信息是 "fix bug"
```

## Skill 调用规范

AI 在调用 Skill 时遵循以下规范：

```json
{
  "skill_name": "skill-name",
  "args": "optional arguments"
}
```

调用示例：
- `skill: "pdf"` - 调用 pdf skill
- `skill: "commit", args: "-m 'Fix bug'"` - 带参数调用
- `skill: "review-pr", args: "123"` - 指定 PR 编号

## 在 Skill 中使用交互式选项（ask_user）

当 Skill 的流程遇到真实歧义，且继续执行前必须由用户确认时，可以调用 `ask_user`。它只在本地交互式终端中接管输入；远程/非交互式场景不会展示本地 TUI。

### ask_user 入参

`ask_user` 不再需要公开传 `kind`。交互类型由入参能力推断：没有 `options` 或 `options` 为空时是文本输入；提供 `options` 时是选项选择。`allow_freeform_input` 控制有选项时是否允许用户输入自定义答案，默认允许。

文本输入示例：

```json
{
  "prompt": "请填写要写入的目标文件路径",
  "title": "需要确认目标路径",
  "placeholder": "例如 docs/notes.md",
  "required": true,
  "allow_cancel": true,
  "min_length": 1
}
```

选项示例：

```json
{
  "prompt": "请选择下一步操作",
  "options": [
    {"value": "minimal", "label": "最小修改", "description": "只处理当前问题", "recommended": true},
    {"value": "complete", "label": "完整处理", "description": "同时补测试和文档"}
  ],
  "default": "minimal",
  "title": "选择处理方式",
  "allow_freeform_input": true,
  "allow_cancel": true
}
```

推荐约定：

- 只在确实需要用户判断时调用，不要用它替代普通状态说明。
- 能列出清晰候选项时优先传 `options`，每个选项都提供稳定的 `value` 和可读的 `label`。
- 当候选项可能不完整时，保留 `allow_freeform_input: true`；如果必须从预设项中选择，显式传 `allow_freeform_input: false`。
- `default` 在有选项时必须匹配某个 `option.value`；文本输入时可作为用户直接回车的默认答案。
- 最多一个选项设置 `recommended: true`。

### ask_user 返回

- 用户选中预设项时，结果 `meta.status` 为 `answered`，`meta.answer_type` 为 `option`，并包含 `value`、`label`、`description`。
- 用户输入自定义文本时，结果 `meta.status` 为 `answered`，`meta.answer_type` 为 `text`，并包含 `value`。
- 用户取消时，结果 `meta.status` 为 `cancelled`。
- 新契约的公开 schema 和返回 meta 不包含 `kind`。旧调用方传入 `kind` 时会被兼容解析，但新 Skill 不应继续依赖它。

### 推荐调用示例

```json
{
  "prompt": "请选择一个水果，或输入列表中没有的水果",
  "title": "水果选择",
  "options": [
    {"value": "apple", "label": "苹果", "recommended": true},
    {"value": "banana", "label": "香蕉"},
    {"value": "orange", "label": "橙子"}
  ],
  "default": "apple",
  "placeholder": "输入自定义水果名称",
  "allow_freeform_input": true,
  "allow_cancel": true
}
```

## 开发新 Skill

### 步骤 1：创建目录结构

```bash
mkdir -p ~/.config/aish/skills/my-skill
cd ~/.config/aish/skills/my-skill
```

### 步骤 2：编写 SKILL.md

```markdown
---
name: my-skill
description: 我的自定义 Skill，用于完成特定任务
license: MIT
compatibility: Python 3.8+
allowed_tools: ["bash_exec", "read_file"]
---

# My Skill 使用指南

这个 Skill 帮助你完成...

## 使用场景

- 场景 1
- 场景 2

## 示例

示例代码和说明...
```

### 元数据字段说明

| 字段 | 必需 | 说明 | 限制 |
|------|------|------|------|
| `name` | ✅ | 技能唯一标识符 | 小写字母/数字/连字符，最大 64 字符 |
| `description` | ✅ | 技能用途描述 | 最大 1024 字符 |
| `license` | ❌ | 许可证名称 | 如 MIT、Apache-2.0 等 |
| `compatibility` | ❌ | 环境要求 | 如 "需要 poppler-utils 包" |
| `allowed_tools` | ❌ | 允许使用的工具列表 | 如 ["bash_exec", "read_file"] |

### 步骤 3：验证 Skill

保存 `SKILL.md` 后无需重启，下一次输入/与 AI 交互时会自动重新加载 Skills。你可以通过以下方式验证：

```bash
aish> 有哪些可用的 skills？
```

## Skill 示例

### 示例 1：Git 提交 Skill

```markdown
---
name: git-commit
description: 智能创建 Git 提交，自动生成提交信息
---

# Git 提交 Skill

自动分析当前更改并创建符合规范的提交信息。

## 工作流程

1. 运行 `git status` 查看更改
2. 运行 `git diff` 查看具体差异
3. 根据更改内容生成提交信息
4. 执行 `git add` 和 `git commit`

## 提交信息格式

遵循约定式提交规范：
类型包括：feat, fix, docs, style, refactor, test, chore
```


### 示例 2：部署 Skill

```markdown
---
name: deploy
description: 自动部署应用到生产环境
license: MIT
compatibility: 需要 Docker 和 kubectl
---

# 部署 Skill

这个技能用于自动化部署应用到 Kubernetes 集群。

## 部署流程

1. 检查当前分支是否为 main
2. 运行测试套件
3. 构建 Docker 镜像
4. 推送到镜像仓库
5. 更新 Kubernetes 部署

## 前置条件

- 已配置 kubectl 上下文
- 有镜像仓库推送权限
- 当前在 Git 仓库根目录
```

### 示例 3：代码审查 Skill

```markdown
---
name: code-review
description: 自动代码审查，检查常见问题和最佳实践
---

# 代码审查 Skill

对代码变更进行自动化审查。

## 检查项

- 代码风格一致性
- 潜在的 bug 和安全问题
- 性能优化建议
- 文档完整性
- 测试覆盖率

## 输出格式

审查结果以 Markdown 格式输出，包含：

1. **概述**：总体评估
2. **问题列表**：按严重程度分类
3. **建议**：具体的改进建议
4. **最佳实践**：相关推荐
```

## Skill 开发最佳实践

### 1. 清晰的命名

使用描述性的技能名称：

```
✅ good: deploy-to-production
❌ bad: thing
```

### 2. 详细的描述

描述应该说明技能做什么，而不是怎么做：

```
✅ good: 部署应用到 Kubernetes 生产环境
❌ bad: 使用 kubectl 和 docker 部署
```

### 3. 合理的工具权限

只声明必需的工具，遵循最小权限原则：

```yaml
allowed_tools: ["read_file"]  # 只读技能
allowed_tools: ["bash_exec", "write_file"]  # 读写技能
```

## 常见问题

**Q: 一个目录可以有多个技能吗？**

A: 不可以。每个目录只能有一个 `SKILL.md` 文件。如果需要多个技能，使用子目录组织：

```
~/.config/aish/skills/
├── deploy/SKILL.md
├── test/SKILL.md
└── backup/SKILL.md
```

**Q: 技能可以调用其他技能吗？**

A: 可以。在 `allowed_tools` 中包含 `"skill"` 即可。

**Q: 如何禁用某个技能？**

A: 删除或重命名对应的 `SKILL.md` 文件，或者移动到非技能目录。

**Q: 技能支持参数化吗？**

A: 支持。在技能内容中说明参数格式，LLM 会根据用户输入解析并传递参数。

**Q: 技能未生效怎么办？**

A: 检查以下几点：

1. **文件位置**：确认 `SKILL.md` 在正确的目录
2. **文件格式**：验证 YAML frontmatter 格式正确
3. **命名冲突**：检查是否有高优先级的同名技能覆盖
4. **语法错误**：运行 `yamllint` 检查 YAML 语法

```bash
# 检查 YAML 语法
yamllint ~/.config/aish/skills/my-skill/SKILL.md
```

## 参考资源

- [agentskills.io 规范](https://agentskills.io/specification)
