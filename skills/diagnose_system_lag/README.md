# 系统卡顿诊断工具 (diagnose_system_lag)

快速检测和分析 Linux 系统卡顿问题，智能识别 CPU/内存/Swap/磁盘瓶颈，给出安全的优化建议。

## 🎯 功能特性

- ✅ **全面诊断**: CPU 负载、内存压力、Swap 使用、磁盘空间
- ✅ **智能分析**: 自动识别卡顿原因和瓶颈类型
- ✅ **进程识别**: 智能识别常见应用（WPS、企业微信、浏览器等）
- ✅ **安全建议**: 绝不推荐关闭系统核心服务
- ✅ **中文友好**: 完整中文输出，易于理解

---

## 🚀 快速使用

### 方式一：Claude Code 中使用（推荐）

直接在 Claude Code 中输入：

```
帮我看看系统卡不卡
系统卡顿诊断
检测系统负载
```

### 方式二：命令行使用

```bash
# 运行诊断
bash ~/.claude/skills/diagnose_system_lag/scripts/diagnose.sh

# 查看最新报告
ls -t ~/.claude/skills/diagnose_system_lag/reports/ | head -1
cat ~/.claude/skills/diagnose_system_lag/reports/diagnose-*.txt
```

---

## 📊 诊断内容

### 1. CPU 性能分析
- Load Average（1/5/15 分钟负载）
- CPU 使用率（user/system/idle/iowait）
- 判定 CPU 瓶颈或 I/O 瓶颈

### 2. 内存和 Swap 分析
- 内存使用情况（总量/已用/可用）
- Swap 使用情况
- 高危指标警告（Swap >2GB 或 >50%）

### 3. 磁盘空间分析
- 根分区和 home 分区使用率
- 满盘风险检测（>=90%）

### 4. 进程分析
- TOP 25 内存占用进程
- 智能识别应用类型
- 过滤系统核心服务

---

## 📁 目录结构

```
~/.claude/skills/diagnose_system_lag/
├── SKILL.md          # Skill 定义文件
├── README.md         # 本文件
├── scripts/
│   └── diagnose.sh   # 主诊断脚本
├── config/           # 配置文件
├── logs/             # 运行日志
├── reports/          # 诊断报告输出
└── templates/        # 模板文件
```

---

## 🔍 输出示例

```
═══════════════════════════════════════════════════════════
  系统卡顿诊断报告
  生成时间: 2026-01-06 21:30:00
═══════════════════════════════════════════════════════════

## 📋 诊断结论

**您的系统目前运行流畅，没有明显卡顿现象。**

## 📊 系统资源概览

- Load Average: 1.62, 1.44, 1.58 (CPU 核心数: 8)
- CPU 使用: 28.7% user, 13.9% system, 56.4% idle, 0.0% iowait
- 内存: 14Gi 总 / 7.5Gi 已用 / 7.1Gi 可用
- Swap: 4.0Gi 总 / 170Mi 已用 / 3.8Gi 可用

## 🔍 内存占用 TOP 进程

USER         PID %CPU %MEM    VSZ   RSS TTY      STAT START   TIME COMMAND
weilei     19713  2.5  5.3 5393672 812236 ?      Ssl  19:26   2:20 warp-terminal
weilei     29947  1.7  4.7 2554408 723724 ?      Sl   19:51   1:08 /opt/.../wpp
...
```

---

## 🛠️ 进阶使用

### 查看历史报告

```bash
# 列出所有报告
ls -lh ~/.claude/skills/diagnose_system_lag/reports/

# 查看最新报告
cat $(ls -t ~/.claude/skills/diagnose_system_lag/reports/diagnose-*.txt | head -1)
```

### 定期诊断（Cron 任务）

```bash
# 每小时执行一次诊断
0 * * * * /bin/bash ~/.claude/skills/diagnose_system_lag/scripts/diagnose.sh
```

### 清理旧报告

```bash
# 删除 7 天前的报告
find ~/.claude/skills/diagnose_system_lag/reports/ -name "diagnose-*.txt" -mtime +7 -delete
find ~/.claude/skills/diagnose_system_lag/logs/ -name "diagnose-*.log" -mtime +7 -delete
```

---

## ⚙️ 配置说明

配置文件位置：`config/default.conf`（可选）

当前版本使用默认配置，未来版本将支持自定义：
- CPU 负载阈值
- 内存警告阈值
- Swap 使用警告阈值
- 磁盘空间警告阈值

---

## 🔒 安全保证

本工具**仅收集和分析系统数据，绝不会自动执行任何危险操作**：
- ✅ 只读取系统信息，不修改任何配置
- ✅ 不会自动关闭任何进程
- ✅ 所有优化建议仅供参考，需用户手动执行
- ✅ 绝不建议关闭系统核心服务（Xorg、桌面环境、输入法等）

---

## 📖 支持的系统

- ✅ Deepin 20/23
- ✅ Debian 10/11/12
- ✅ Ubuntu 20.04/22.04/24.04
- ✅ UOS (统信 UOS)

---

## 🆘 故障排查

### Q: 脚本无法执行
```bash
chmod +x ~/.claude/skills/diagnose_system_lag/scripts/diagnose.sh
```

### Q: 报告未生成
检查目录权限：
```bash
ls -ld ~/.claude/skills/diagnose_system_lag/reports/
```

### Q: 数据不准确
确保使用 bash 执行：
```bash
bash ~/.claude/skills/diagnose_system_lag/scripts/diagnose.sh
```

---

**版本**: v2.0.0 | **作者**: user | **许可**: MIT
