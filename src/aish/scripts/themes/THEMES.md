# Prompt Themes

内置 3 种 prompt 主题。

## 使用方法

```bash
# 复制主题到 hooks 目录
cp <aish>/src/aish/scripts/themes/compact.aish ~/.config/aish/scripts/themes/aish_prompt.aish
```

---

## 主题

### Compact (推荐)
紧凑型彩色 prompt。

```
:~/n/x/g/aish|standalone● +1 ↑2 ➜
```

**颜色系统**:

| 元素 | 颜色 |
|------|------|
| 路径 | 蓝色 |
| 分支 | 品红色 (detached: 灰色) |
| ● 绿色 | 干净 |
| ● 黄色 | 有暂存 |
| ● 红色 | 有修改无暂存 |
| ● 青色 | 仅有未跟踪 |
| ↑↓ | 青色 (ahead/behind) |
| ➜ | 绿色 (成功) / 红色 (失败) |

**符号说明**:

| 符号 | 含义 |
|------|------|
| `●` | Git 状态指示器 |
| `+N` | 暂存文件数 |
| `~N` | 修改文件数 |
| `↑N` | 领先远程 N 提交 |
| `↓N` | 落后远程 N 提交 |
| `🐍` | 虚拟环境 |
| `➜` | 正常提示符 |
| `➜➜` | 命令失败 |

---

### Developer
双行开发型 prompt。

```
17:01 aish main [+1 ~11 ?9] ●
❯
```

---

### Powerline
单行彩色分段 prompt。

```
 aish | main +1 ~11 | ● >
```

---

## 可用环境变量

| 变量 | 说明 |
|------|------|
| `AISH_CWD` | 当前工作目录 |
| `AISH_EXIT_CODE` | 上条命令退出码 |
| `AISH_GIT_REPO` | "1" 表示在 git 仓库 |
| `AISH_GIT_BRANCH` | 当前分支 |
| `AISH_GIT_STATUS` | clean/staged/dirty |
| `AISH_GIT_STAGED` | 暂存文件数 |
| `AISH_GIT_MODIFIED` | 修改文件数 |
| `AISH_GIT_UNTRACKED` | 未跟踪文件数 |
| `AISH_GIT_AHEAD` | 领先远程提交数 |
| `AISH_GIT_BEHIND` | 落后远程提交数 |
| `AISH_VIRTUAL_ENV` | 虚拟环境名称 |

---

## 自定义

创建 `~/.config/aish/scripts/hooks/aish_prompt.aish`:

```bash
#!/bin/bash
# 最小示例
dir=$(basename "$AISH_CWD")
if [[ "$AISH_GIT_REPO" == "1" ]]; then
    echo "🚀 $dir ($AISH_GIT_BRANCH) > "
else
    echo "🚀 $dir > "
fi
```
