# Deepin SysAssist - 深度系统辅助工具集

模拟深度（Deepin）系统辅助工具的完整功能，提供系统监控、体检、日志分析和基线配置管理。

## 🎯 功能概览

| 工具 | 命令 | 功能说明 |
|------|------|---------|
| **monitor** | `monitor.sh` | 实时监控 CPU、内存、磁盘、网络、进程状态 |
| **syscheck** | `syscheck.sh` | 全面系统体检（硬件、软件、配置、安全） |
| **panalysis** | `panalysis.sh` | 进程日志分析（OOM、崩溃、资源占用） |
| **nanalysis** | `nanalysis.sh` | 网络日志分析（连接统计、流量、安全威胁） |
| **baselineconfig** | `baselineconfig.sh` | 基线配置管理（备份、对比、合规检查） |

---

## 🚀 快速开始

### 方式一：Claude Code 自动触发

直接在 Claude Code 中输入：
```
启动系统监控
做个系统体检
分析进程日志
分析网络日志
创建配置基线
```

### 方式二：命令行执行

```bash
# 1. 系统监控（实时刷新）
bash ~/.claude/skills/deepin-sysassist/scripts/monitor.sh

# 2. 系统体检
bash ~/.claude/skills/deepin-sysassist/scripts/syscheck.sh

# 3. 进程日志分析
bash ~/.claude/skills/deepin-sysassist/scripts/panalysis.sh

# 4. 网络日志分析
bash ~/.claude/skills/deepin-sysassist/scripts/nanalysis.sh

# 5. 基线配置管理
bash ~/.claude/skills/deepin-sysassist/scripts/baselineconfig.sh --create baseline-v1
```

---

## 📊 详细使用指南

完整的使用文档请查看 SKILL.md 文件：
```bash
cat ~/.claude/skills/deepin-sysassist/SKILL.md
```

---

## 📁 目录结构

```
~/.claude/skills/deepin-sysassist/
├── SKILL.md                # Skill 定义文件
├── README.md               # 本文件
├── scripts/
│   ├── monitor.sh          # 系统监控
│   ├── syscheck.sh         # 系统体检
│   ├── panalysis.sh        # 进程日志分析
│   ├── nanalysis.sh        # 网络日志分析
│   └── baselineconfig.sh   # 基线配置
├── config/                 # 配置目录
├── logs/                   # 日志目录
├── reports/                # 报告输出
└── templates/              # 模板目录
```

---

**版本**: v1.0.0 | **系统**: Deepin/Debian/Ubuntu/RHEL/CentOS/Fedora
