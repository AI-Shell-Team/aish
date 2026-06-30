# SOS Report Analyzer - 快速参考卡

## 🚀 快速启动（最简单）

```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/quickstart.sh
```

**或者直接在 Claude Code 中输入**：
```
帮我做个系统诊断
```

---

## 📋 常用命令

### 1. 检查依赖
```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/check_dependencies.sh
```

### 2. 收集数据
```bash
# 普通用户
bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh

# Root 用户（推荐）
sudo bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

### 3. 分析数据
```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/analyze.sh /tmp/sosreport-TIMESTAMP/
```

### 4. 查看报告
```bash
cat /tmp/sosreport-TIMESTAMP/analysis_report.md
```

---

## 🔧 安装依赖

### Debian/Ubuntu
```bash
sudo apt update
sudo apt install sysstat iotop smartmontools dmidecode pciutils usbutils
```

### RHEL/CentOS/Fedora
```bash
sudo dnf install sysstat iotop smartmontools dmidecode pciutils usbutils
```

---

## 📊 收集的数据类型

| 模块 | 内容 | 文件位置 |
|------|------|----------|
| **系统** | OS、CPU、内存、硬件 | `system/` |
| **内核** | 版本、模块、启动参数 | `kernel/` |
| **日志** | systemd、syslog、审计 | `logs/` |
| **网络** | IP、路由、防火墙、DNS | `network/` |
| **存储** | 磁盘、LVM、文件系统 | `storage/` |
| **内存** | 使用率、性能、进程 | `memory/` |
| **服务** | systemd 单元、进程树 | `services/` |
| **安全** | SELinux、SSH、审计 | `security/` |
| **软件包** | RPM/DEB、仓库 | `packages/` |
| **容器** | Docker、Podman | `custom/` |

---

## 🔍 智能检测项

### 性能问题
- ✅ 磁盘空间不足 (>95%, >85%, >75%)
- ✅ Inode 耗尽 (>90%)
- ✅ 内存压力 (<10% 可用 + swap 使用)
- ✅ CPU iowait 过高 (>30%, >15%)

### 服务异常
- ✅ 失败的 systemd 服务
- ✅ 僵尸进程累积

### 网络问题
- ✅ 网络接口丢包/错误

### 安全风险
- ✅ SELinux 禁用
- ✅ SELinux AVC 拒绝
- ✅ SSH root 登录
- ✅ SSH 密码认证

---

## 🎯 使用场景

| 场景 | 命令 |
|------|------|
| **生产故障排查** | `sudo bash quickstart.sh` |
| **定期健康检查** | 定时任务运行 `collect_all.sh` |
| **升级前评估** | 收集基线数据保存 |
| **性能调优** | 分析 CPU、内存、I/O 数据 |
| **安全审计** | 检查 `security/` 目录 |

---

## 📁 输出文件

### 收集后
```
/tmp/sosreport-TIMESTAMP/          # 原始数据目录
/tmp/sosreport-hostname-TIMESTAMP.tar.gz  # 压缩包
```

### 分析后
```
/tmp/sosreport-TIMESTAMP/analysis_report.md  # Markdown 报告
```

---

## 🧹 清理数据

```bash
# 删除所有报告
rm -rf /tmp/sosreport-*

# 只删除 7 天前的
find /tmp -name "sosreport-*" -mtime +7 -delete
```

---

## 🆘 快速故障排除

| 问题 | 解决方案 |
|------|----------|
| **无执行权限** | `chmod +x ~/.claude/skills/sosreport-analyzer/scripts/*.sh` |
| **缺少工具** | 运行 `check_dependencies.sh` 查看安装命令 |
| **磁盘空间不足** | 修改脚本中的 `REPORT_DIR` 变量 |
| **权限不足** | 使用 `sudo` 运行 |
| **Claude 未触发** | 使用触发词: "sosreport"、"系统诊断"、"system diagnostic" |

---

## 📞 获取帮助

```bash
# 查看完整文档
cat ~/.claude/skills/sosreport-analyzer/README.md

# 查看使用指南
cat ~/.claude/skills/sosreport-analyzer/USAGE_GUIDE.md

# 查看 Skill 定义
cat ~/.claude/skills/sosreport-analyzer/SKILL.md
```

---

## 💡 Pro 技巧

### 远程收集
```bash
ssh user@server 'bash -s' < ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

### 定时任务
```bash
# 每周一 8:00 执行
0 8 * * 1 /bin/bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

### 批量服务器
```bash
for server in srv1 srv2 srv3; do
    ssh $server 'sudo bash -s' < collect_all.sh
done
```

---

**版本**: v1.0.0 | **平台**: Linux (Debian/Ubuntu/RHEL/CentOS/Fedora)
