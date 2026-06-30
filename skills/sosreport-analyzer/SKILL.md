---
name: sosreport-analyzer
version: 1.0.0
description: 一种 Linux 系统诊断和分析工具，收集系统配置、日志、性能数据并生成诊断报告
author: deepin
triggers:
  - sosreport
  - system diagnostic
  - 系统诊断
  - 收集日志
  - troubleshooting
  - 故障排查
  - system info
  - 系统信息收集
platforms:
  - linux
distributions:
  - debian
  - ubuntu
  - rhel
  - centos
  - fedora
  - rocky
  - alma
---

# SOS Report Analyzer Skill

## 📋 功能概述

自动收集 Linux 系统的配置、日志、性能数据，并进行智能分析和问题诊断。

**核心能力：**
- ✅ 跨发行版兼容（Debian/Ubuntu/RHEL/CentOS/Fedora）
- ✅ 系统信息收集（硬件、内核、发行版）
- ✅ 日志聚合与分析（systemd、syslog、应用日志）
- ✅ 网络配置诊断（接口、路由、防火墙、DNS）
- ✅ 存储状态检查（磁盘、LVM、文件系统、挂载点）
- ✅ 服务状态监控（systemd units、进程树）
- ✅ 性能数据采集（CPU、内存、I/O、网络）
- ✅ 安全配置审计（SELinux/AppArmor、防火墙、审计日志）
- ✅ 智能问题检测与建议

---

## 🎯 使用场景

### 场景1：生产环境故障排查
```
用户: 生产服务器响应缓慢，帮我做个系统诊断
助手: 我将使用 sosreport-analyzer 收集系统数据...
      [执行数据收集]
      [分析报告]
      发现问题：
      1. CPU iowait 高达 45%，磁盘 /dev/sda 存在性能瓶颈
      2. 内存使用 92%，swap 频繁使用
      3. systemd-journald 服务占用 2GB 日志空间
```

### 场景2：系统健康检查
```
用户: 定期巡检，生成系统健康报告
助手: 正在收集系统状态...
      [生成 HTML/Markdown 报告]
```

### 场景3：迁移前评估
```
用户: 准备将 deepin 迁移到 UOS，先做个系统扫描
助手: 扫描兼容性问题...
      [检测内核模块、配置文件、软件包兼容性]
```

---

## 📊 数据收集模块

### 1. 系统基础信息（System Overview）

**收集内容：**
```bash
# 发行版信息
- /etc/os-release
- lsb_release -a
- uname -a

# 硬件信息
- lscpu (CPU 架构、核心数、NUMA 拓扑)
- lsmem (内存配置、分布)
- dmidecode (BIOS、主板、硬件序列号)
- lspci (PCI 设备)
- lsusb (USB 设备)
- lsblk (块设备)

# 虚拟化检测
- systemd-detect-virt
- virt-what
```

**分析重点：**
- CPU 型号、频率、漏洞缓解状态
- 内存容量、NUMA 节点分布
- 虚拟化平台识别（KVM、VMware、AWS、Azure）

---

### 2. 内核与启动（Kernel & Boot）

**收集内容：**
```bash
# 内核信息
- uname -r
- cat /proc/cmdline
- cat /proc/version
- lsmod (已加载模块)
- dmesg (启动日志)

# 启动配置
- /boot/grub*/grub.cfg
- /etc/default/grub
- systemctl list-jobs
- systemd-analyze blame (启动时间分析)
```

**分析重点：**
- 内核 taint 状态（污染模块）
- 启动参数异常（如 nomodeset、quiet）
- 启动性能瓶颈服务

---

### 3. 日志聚合（Logging）

**收集内容：**
```bash
# Systemd Journal
- journalctl --no-pager --since "7 days ago" -p err
- journalctl --disk-usage
- journalctl -u <critical-services> --since "24 hours ago"

# 传统日志（如果存在）
- /var/log/messages (RHEL)
- /var/log/syslog (Debian/Ubuntu)
- /var/log/auth.log
- /var/log/secure
- /var/log/audit/audit.log

# 应用日志
- /var/log/nginx/
- /var/log/apache2/
- /var/log/mysql/
```

**智能分析：**
- 错误日志聚合（ERROR、CRITICAL、FATAL）
- 异常模式识别（OOM killer、segfault、I/O errors）
- 重复错误统计（Top 10 错误消息）
- 时间线关联分析

---

### 4. 网络配置（Networking）

**收集内容：**
```bash
# 网络接口
- ip addr show
- ip link show
- ip route show
- ip -s link (流量统计)

# 网络连接
- ss -tunap (所有连接)
- netstat -i (接口统计)
- arp -n

# DNS 配置
- cat /etc/resolv.conf
- cat /etc/hosts
- cat /etc/nsswitch.conf

# 防火墙
- iptables -L -n -v (如果使用)
- nft list ruleset (nftables)
- firewall-cmd --list-all (firewalld)
- ufw status verbose (Ubuntu)

# 网络服务
- systemctl status NetworkManager
- nmcli connection show
```

**诊断检查：**
- 接口错误率（RX/TX errors、dropped packets）
- MTU 配置一致性
- DNS 解析测试
- 默认网关可达性
- 防火墙规则冲突检测

---

### 5. 存储与文件系统（Storage）

**收集内容：**
```bash
# 磁盘信息
- lsblk -f
- fdisk -l
- parted -l
- blkid

# 文件系统
- df -h
- df -i (inode 使用率)
- mount

# LVM (如果使用)
- pvs, vgs, lvs
- pvdisplay, vgdisplay, lvdisplay

# RAID (如果使用)
- cat /proc/mdstat
- mdadm --detail /dev/md*

# 磁盘健康
- smartctl -a /dev/sda (需要 smartmontools)

# I/O 性能
- iostat -x 1 5
- iotop -b -n 3 (需要 root)
```

**风险检测：**
- 磁盘空间使用 > 90%
- Inode 使用 > 80%
- SMART 错误计数
- RAID 降级状态
- LVM 快照空间不足

---

### 6. 内存与性能（Memory & Performance）

**收集内容：**
```bash
# 内存状态
- free -h
- cat /proc/meminfo
- slabtop -o (内核 slab 缓存)
- vmstat 1 5

# CPU 性能
- top -b -n 3
- mpstat -P ALL 1 3
- uptime

# 进程信息
- ps aux --sort=-%mem | head -20
- ps aux --sort=-%cpu | head -20
- pstree -p

# Swap 使用
- swapon -s
- cat /proc/swaps
```

**性能分析：**
- CPU 利用率分解（user/system/iowait）
- 内存泄漏迹象（slab 异常增长）
- Top 内存/CPU 消耗进程
- Swap 使用趋势

---

### 7. 服务与进程（Services）

**收集内容：**
```bash
# Systemd 服务
- systemctl list-units --type=service --all
- systemctl --failed
- systemctl list-timers

# 关键服务状态
- systemctl status sshd
- systemctl status NetworkManager
- systemctl status firewalld

# 进程树
- pstree -a
- ps -eo pid,ppid,cmd,stat,start
```

**健康检查：**
- 失败的服务单元
- 重启次数异常的服务
- 僵尸进程检测
- 孤儿进程识别

---

### 8. 安全配置（Security）

**收集内容：**
```bash
# SELinux (RHEL/CentOS)
- getenforce
- sestatus
- ausearch -m AVC -ts recent (AVС 拒绝日志)

# AppArmor (Debian/Ubuntu)
- aa-status
- aa-enabled

# 审计系统
- auditctl -l
- aureport --summary

# 登录记录
- last -20
- lastb -20 (失败登录)
- who
- w

# SSH 配置
- cat /etc/ssh/sshd_config (敏感项模糊化)

# 用户与权限
- cat /etc/passwd
- cat /etc/group
- find / -perm -4000 -type f 2>/dev/null (SUID 文件)
```

**安全审计：**
- SELinux/AppArmor 拒绝事件
- 异常登录尝试
- SUID/SGID 可疑文件
- 弱 SSH 配置检测

---

### 9. 软件包管理（Packages）

**收集内容：**
```bash
# RHEL/CentOS/Fedora
- rpm -qa
- dnf repolist
- dnf history
- dnf check

# Debian/Ubuntu
- dpkg -l
- apt list --installed
- apt-cache policy

# 内核包
- rpm -qa | grep kernel (RHEL)
- dpkg -l | grep linux-image (Debian)
```

**检查项：**
- 损坏的包依赖
- 可用更新统计
- 仓库配置验证
- 内核版本不一致

---

### 10. 定制化收集（Custom Collectors）

**可扩展模块：**
```bash
# 数据库
- MySQL/PostgreSQL 配置和状态
- 数据库连接数、慢查询

# Web 服务器
- Nginx/Apache 配置检查
- 访问日志统计

# 容器
- docker info
- docker ps -a
- podman info

# 监控代理
- Prometheus node_exporter 指标
- Zabbix agent 状态
```

---

## 🔍 智能分析引擎

### 问题检测规则库

#### 1. 性能问题
```yaml
- rule: high_cpu_iowait
  condition: iowait > 30%
  severity: HIGH
  message: "CPU iowait 过高，可能存在磁盘 I/O 瓶颈"
  suggestions:
    - "检查 iostat 输出，定位慢速磁盘"
    - "使用 iotop 识别 I/O 密集型进程"
    - "考虑使用 SSD 或优化文件系统参数"

- rule: memory_pressure
  condition: available_memory < 10% AND swap_used > 50%
  severity: CRITICAL
  message: "内存压力严重，系统频繁使用 swap"
  suggestions:
    - "识别内存泄漏进程并重启"
    - "增加物理内存"
    - "调整 vm.swappiness 参数"

- rule: disk_space_critical
  condition: disk_usage > 95%
  severity: CRITICAL
  message: "磁盘空间严重不足"
  suggestions:
    - "清理日志文件：journalctl --vacuum-time=7d"
    - "删除旧内核：dnf remove --oldinstallonly"
    - "扩展文件系统或添加新磁盘"
```

#### 2. 服务异常
```yaml
- rule: failed_services
  condition: systemctl --failed count > 0
  severity: MEDIUM
  message: "存在失败的 systemd 服务"
  suggestions:
    - "使用 journalctl -u <service> 查看失败原因"
    - "检查服务依赖关系"

- rule: zombie_processes
  condition: zombie_count > 5
  severity: LOW
  message: "检测到僵尸进程"
  suggestions:
    - "识别父进程并修复其信号处理逻辑"
    - "必要时重启父进程"
```

#### 3. 网络问题
```yaml
- rule: packet_drops
  condition: rx_dropped > 1% OR tx_dropped > 1%
  severity: MEDIUM
  message: "网络接口丢包严重"
  suggestions:
    - "检查网卡驱动版本"
    - "增加 ring buffer: ethtool -G eth0 rx 4096"
    - "调整 net.core.netdev_max_backlog"
```

#### 4. 安全风险
```yaml
- rule: selinux_disabled
  condition: selinux_status == "disabled"
  severity: HIGH
  message: "SELinux 已禁用，系统安全性降低"
  suggestions:
    - "编辑 /etc/selinux/config 启用 SELinux"
    - "需要重启系统生效"

- rule: ssh_root_login
  condition: sshd_config contains "PermitRootLogin yes"
  severity: MEDIUM
  message: "允许 root 直接 SSH 登录存在安全风险"
  suggestions:
    - "修改 /etc/ssh/sshd_config: PermitRootLogin no"
    - "重启 sshd: systemctl restart sshd"
```

---

## 📤 输出格式

### 1. 终端摘要（Console Summary）
```
═══════════════════════════════════════════════════
  系统诊断报告 - SOS Report Analyzer v1.0
═══════════════════════════════════════════════════

📌 系统信息
   OS: Ubuntu 22.04.3 LTS
   Kernel: 5.15.0-91-generic
   Platform: VMware Virtual Platform
   Uptime: 45 days, 3:12:34

⚠️  检测到 3 个问题

[CRITICAL] 磁盘空间严重不足
   /dev/sda1 使用率: 97% (仅剩 1.2GB)
   → 建议: journalctl --vacuum-time=7d

[HIGH] CPU iowait 过高
   当前 iowait: 42%
   → 建议: 检查 /dev/sda 性能，考虑迁移到 SSD

[MEDIUM] 存在 2 个失败的服务
   - systemd-networkd.service
   - docker.service
   → 建议: journalctl -u systemd-networkd

✅ 健康检查
   [✓] 内存使用正常 (62%)
   [✓] 网络接口无丢包
   [✓] SELinux 状态正常
   [✓] 无僵尸进程

📊 完整报告已保存到:
   /tmp/sosreport-20260105-143022.tar.gz
```

### 2. Markdown 报告（Detailed Report）
```markdown
# 系统诊断报告

**生成时间:** 2026-01-05 14:30:22
**主机名:** prod-web-01
**报告版本:** sosreport-analyzer v1.0

## 执行摘要

本次诊断发现 **3 个关键问题** 和 **5 个优化建议**。

### 严重问题
1. ❌ 磁盘空间不足（/dev/sda1 97%）
2. ⚠️  CPU iowait 异常（42%）
3. ⚠️  2 个服务失败

### 系统概览
- **操作系统:** Ubuntu 22.04.3 LTS (Jammy Jellyfish)
- **内核:** 5.15.0-91-generic
- **CPU:** Intel Xeon E5-2680 v4 @ 2.40GHz (8 cores)
- **内存:** 16GB (使用 62%)
- **运行时间:** 45 天

[详细章节...]
```

### 3. JSON 数据（Machine-Readable）
```json
{
  "metadata": {
    "version": "1.0.0",
    "timestamp": "2026-01-05T14:30:22Z",
    "hostname": "prod-web-01",
    "collector": "sosreport-analyzer"
  },
  "system": {
    "os": {
      "distribution": "Ubuntu",
      "version": "22.04.3 LTS",
      "codename": "Jammy Jellyfish"
    },
    "kernel": "5.15.0-91-generic",
    "hardware": {
      "cpu": {
        "model": "Intel Xeon E5-2680 v4",
        "cores": 8,
        "threads": 16
      },
      "memory": {
        "total_gb": 16,
        "used_percent": 62
      }
    }
  },
  "issues": [
    {
      "severity": "CRITICAL",
      "category": "storage",
      "title": "Disk space critical",
      "details": "/dev/sda1 usage: 97%",
      "recommendations": [
        "journalctl --vacuum-time=7d",
        "Clean old kernels",
        "Expand filesystem"
      ]
    }
  ],
  "health_checks": {
    "memory": "PASS",
    "network": "PASS",
    "services": "WARN",
    "security": "PASS"
  }
}
```

---

## 🛠️ 使用方法

### 基础用法
```bash
# 在 Claude Code 中触发
帮我做个系统诊断
运行 sosreport 收集系统信息
系统健康检查
```

### 高级选项
```bash
# 指定收集模块
只收集网络和存储信息

# 深度分析模式
做个完整的系统诊断，包括性能分析

# 导出特定格式
生成 JSON 格式的诊断报告
```

### 定期巡检
```bash
# 创建 cron 任务（需要用户确认）
每周一早上 8 点自动生成系统报告
```

---

## 📦 依赖工具

### 必需工具（核心功能）
- `systemctl` (systemd)
- `journalctl` (日志)
- `ip` / `ss` (网络)
- `df` / `lsblk` (存储)
- `top` / `ps` (进程)

### 可选工具（增强功能）
- `iostat` / `iotop` (I/O 分析) - sysstat 包
- `smartctl` (磁盘健康) - smartmontools 包
- `dmidecode` (硬件信息) - 需要 root
- `auditctl` (审计) - auditd 包
- `docker` / `podman` (容器信息)

### 自动检测缺失工具
如果缺少可选工具，skill 会：
1. 记录警告但继续执行
2. 在报告中说明哪些模块被跳过
3. 提供安装命令建议

---

## 🔒 隐私与安全

### 数据脱敏
自动模糊化敏感信息：
- IP 地址（保留网段，模糊主机部分）
- MAC 地址（保留厂商前缀）
- 主机名（可选）
- 密码和密钥文件内容

### 权限要求
- **普通用户:** 可收集大部分信息
- **Root 用户:** 获取完整硬件信息、审计日志、I/O 详情

### 数据存储
- 默认保存到 `/tmp/sosreport-<timestamp>.tar.gz`
- 报告保存 7 天后自动清理
- 不会上传到任何外部服务器

---

## 🧩 扩展性

### 自定义收集器
在 `scripts/custom/` 目录下添加脚本：

```bash
# scripts/custom/collect_mysql.sh
#!/bin/bash
# 收集 MySQL 状态

if command -v mysql &>/dev/null; then
    mysql -e "SHOW STATUS" > mysql_status.txt
    mysql -e "SHOW VARIABLES" > mysql_variables.txt
fi
```

### 自定义分析规则
编辑 `templates/rules.yaml` 添加检测规则。

---

## 📖 参考资源

- **Red Hat sosreport 文档:** https://access.redhat.com/solutions/3592
- **Systemd 文档:** https://www.freedesktop.org/software/systemd/man/
- **Linux Performance Tools:** http://www.brendangregg.com/linuxperf.html

---

## 🆘 故障排除

### Q: 报告生成失败
```bash
# 检查磁盘空间
df -h /tmp

# 检查权限
ls -ld /tmp

# 手动运行收集脚本
bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

### Q: 某些模块被跳过
```bash
# 检查缺失的工具
~/.claude/skills/sosreport-analyzer/scripts/check_dependencies.sh

# 安装推荐工具（Debian/Ubuntu）
sudo apt install sysstat smartmontools

# 安装推荐工具（RHEL/CentOS）
sudo dnf install sysstat smartmontools
```

---

## 🎯 最佳实践

1. **定期巡检**: 每周生成一次基线报告
2. **事件驱动**: 性能异常时立即收集数据
3. **版本控制**: 保存历史报告以便对比趋势
4. **团队共享**: 将报告附加到故障工单
5. **自动化**: 集成到监控告警工作流

---

**版本历史:**
- v1.0.0 (2026-01-05): 初始版本
