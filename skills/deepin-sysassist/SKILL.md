---
name: deepin-sysassist
version: 1.0.0
description: 深度系统辅助工具集 - 系统监控、体检、日志分析和基线配置管理（兼容多种 Linux 发行版）
author: deepin
triggers:
  - deepin-sysassist
  - monitor
  - syscheck
  - 系统体检
  - 系统监控
  - panalysis
  - nanalysis
  - 进程日志分析
  - 网络日志分析
  - baselineconfig
  - 基线配置
  - system health check
  - log analysis
platforms:
  - linux
distributions:
  - deepin
  - debian
  - ubuntu
  - rhel
  - centos
  - fedora
---

# Deepin SysAssist - 深度系统辅助工具集

## 📋 功能概述

模拟深度（Deepin）系统辅助工具的核心功能，提供完整的系统监控、体检、日志分析和基线配置管理能力。兼容多种 Linux 发行版。

**核心工具模块：**
- ✅ **monitor** - 系统实时监控（CPU、内存、磁盘、网络、进程）
- ✅ **syscheck** - 系统全面体检（硬件、软件、配置、安全）
- ✅ **panalysis** - 进程日志分析（异常检测、资源占用、故障定位）
- ✅ **nanalysis** - 网络日志分析（连接追踪、流量统计、异常检测）
- ✅ **baselineconfig** - 基线配置管理（配置备份、对比、合规检查）

---

## 🎯 使用场景

### 场景1：实时系统监控
```
用户: 启动系统监控
助手: 正在启动 deepin-sysassist monitor...
      [实时显示 CPU、内存、磁盘、网络状态]
      [异常告警]
```

### 场景2：全面系统体检
```
用户: 做个系统体检
助手: 执行 syscheck 全面检查...
      [硬件检测]
      [软件状态]
      [安全审计]
      [性能评估]
      [生成体检报告]
```

### 场景3：日志分析
```
用户: 分析最近的进程异常
助手: 执行 panalysis 进程日志分析...
      [检测到 3 个异常进程]
      [CPU 飙升事件: nginx worker]
      [OOM 事件: java]
```

---

## 🛠️ 工具详解

### 1. monitor - 系统监控

**功能**：实时监控系统关键指标

**监控项**：
- 📊 **CPU 监控**
  - 使用率（user/system/iowait/idle）
  - 每核心使用率
  - 负载平均值（1/5/15分钟）
  - TOP 进程

- 💾 **内存监控**
  - 物理内存使用率
  - Swap 使用率
  - 缓存/缓冲区
  - 内存大户进程

- 💿 **磁盘监控**
  - 磁盘空间使用率
  - Inode 使用率
  - I/O 统计（读写速度、IOPS）
  - 磁盘健康状态（SMART）

- 🌐 **网络监控**
  - 网络流量（入站/出站）
  - 连接数统计（TCP/UDP）
  - 网络错误率
  - TOP 网络进程

- 🔄 **进程监控**
  - 进程数统计
  - 僵尸进程
  - 线程数
  - 文件描述符使用

**告警规则**：
- CPU 使用率 > 90% (持续 2 分钟)
- 内存可用 < 10%
- 磁盘使用率 > 90%
- Swap 使用 > 50%
- 网络丢包率 > 1%

**使用方式**：
```bash
# 实时监控（前台运行）
monitor

# 后台服务模式（推荐）
systemctl start deepin-sysassist-monitor

# 查看监控状态
systemctl status deepin-sysassist-monitor

# 查看监控日志
journalctl -u deepin-sysassist-monitor -f
```

**输出格式**：
- 终端实时刷新（类似 htop）
- JSON 格式（用于集成）
- 告警日志（syslog）

---

### 2. syscheck - 系统体检

**功能**：全面的系统健康检查

**体检模块**：

#### 2.1 硬件检查
- CPU 温度、频率
- 内存错误（ECC）
- 磁盘健康（SMART 状态）
- 硬件错误日志（dmesg）
- BIOS/UEFI 版本
- PCI 设备状态

#### 2.2 软件检查
- 系统版本和补丁级别
- 关键服务状态
- 软件包完整性
- 依赖关系检查
- 可用更新统计

#### 2.3 配置检查
- 内核参数（sysctl）
- 系统限制（ulimit）
- 网络配置
- 防火墙规则
- SELinux/AppArmor 状态

#### 2.4 安全检查
- 用户权限审计
- SUID/SGID 文件
- 开放端口扫描
- 弱密码检测
- 安全补丁状态
- 审计日志完整性

#### 2.5 性能检查
- 系统负载趋势
- 资源瓶颈识别
- I/O 性能测试
- 网络延迟测试

**使用方式**：
```bash
# 完整体检
syscheck

# 快速体检（跳过性能测试）
syscheck --quick

# 仅硬件检查
syscheck --hardware

# 仅安全检查
syscheck --security

# 生成 HTML 报告
syscheck --output-html
```

**报告格式**：
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  系统体检报告 - Deepin SysAssist v1.0
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📌 系统信息
   操作系统: Deepin 23 / Ubuntu 22.04 LTS
   内核: 6.1.0-13-amd64
   主机名: deepin-01
   运行时间: 45 天

📊 体检结果摘要
   总检查项: 85
   通过: 78 (✓)
   警告: 5 (⚠️)
   失败: 2 (✗)

   健康度评分: 89/100

⚠️  发现的问题

[警告] 磁盘空间不足
   /dev/sda1 使用率 87%
   → 建议清理或扩容

[失败] SELinux 已禁用
   → 建议启用以提高安全性

[详细报告...]
```

---

### 3. panalysis - 进程日志分析

**功能**：分析进程相关日志，识别异常行为

**分析维度**：

#### 3.1 进程异常检测
- 进程崩溃（Segmentation Fault）
- 进程被杀（OOM Killer）
- 进程僵死
- 进程重启循环
- 异常退出码

#### 3.2 资源占用分析
- CPU 使用率飙升
- 内存泄漏检测
- 文件描述符耗尽
- 线程数爆炸
- 磁盘 I/O 异常

#### 3.3 时间线分析
- 进程启动/停止时间线
- 资源使用趋势
- 异常事件关联分析

#### 3.4 根因分析
- 依赖服务故障
- 资源竞争
- 配置错误
- 外部因素（网络、磁盘）

**数据源**：
- `/var/log/syslog` 或 `/var/log/messages`
- `journalctl` 输出
- `/proc/[pid]/` 信息
- `dmesg` 内核日志
- 应用程序日志

**使用方式**：
```bash
# 分析最近 24 小时
panalysis

# 指定时间范围
panalysis --since "2026-01-01 00:00" --until "2026-01-05 23:59"

# 分析特定进程
panalysis --process nginx

# 分析特定 PID
panalysis --pid 1234

# 导出分析报告
panalysis --output /tmp/process_analysis.md
```

**输出示例**：
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  进程日志分析报告
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📅 分析时间范围: 2026-01-04 00:00 - 2026-01-05 18:30
📊 分析事件总数: 1,248 条

⚠️  检测到 5 个异常事件

[CRITICAL] OOM Killer 触发
   时间: 2026-01-05 03:42:15
   被杀进程: java (PID 15423)
   内存占用: 11.2GB
   触发原因: 系统内存耗尽
   → 建议: 增加 JVM heap 限制或增加物理内存

[HIGH] 进程频繁重启
   进程: nginx worker
   重启次数: 23 次/小时
   时间: 2026-01-05 10:00 - 11:00
   → 建议: 检查配置文件和错误日志

[MEDIUM] CPU 使用率异常
   进程: mysql
   峰值: 98%
   持续时间: 15 分钟
   时间: 2026-01-05 14:30
   → 建议: 分析慢查询日志

[详细分析...]
```

---

### 4. nanalysis - 网络日志分析

**功能**：分析网络日志，识别连接异常和安全威胁

**分析维度**：

#### 4.1 连接分析
- 连接数趋势
- TOP 连接源 IP
- TOP 目标端口
- 连接状态分布（ESTABLISHED/TIME_WAIT/CLOSE_WAIT）
- 短连接/长连接比例

#### 4.2 流量分析
- 流量统计（入站/出站）
- 异常流量峰值
- 大流量传输记录
- 协议分布（TCP/UDP/ICMP）

#### 4.3 安全分析
- 端口扫描检测
- SYN Flood 攻击
- 暴力破解尝试（SSH/FTP/MySQL）
- 异常 IP 来源
- DDoS 攻击特征

#### 4.4 性能分析
- 网络延迟统计
- 丢包率分析
- 重传率
- 带宽利用率

**数据源**：
- `/var/log/firewall.log`
- `iptables` / `nftables` 日志
- `ss` / `netstat` 快照
- `tcpdump` / `wireshark` 抓包
- 应用访问日志（nginx/apache）

**使用方式**：
```bash
# 分析最近网络日志
nanalysis

# 指定日志文件
nanalysis --log /var/log/firewall.log

# 分析特定 IP
nanalysis --ip 192.168.1.100

# 分析特定端口
nanalysis --port 22

# 安全威胁分析
nanalysis --security

# 生成流量报告
nanalysis --traffic-report
```

**输出示例**：
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  网络日志分析报告
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📅 分析时间: 2026-01-05 00:00 - 18:30
📊 总连接数: 45,892 条
📈 总流量: 入站 12.3GB / 出站 8.7GB

⚠️  检测到 3 个安全威胁

[CRITICAL] SSH 暴力破解攻击
   攻击源: 45.76.123.45
   尝试次数: 1,247 次
   时间: 2026-01-05 02:00 - 04:30
   状态: 已被防火墙阻断
   → 建议: 添加 IP 到永久黑名单

[HIGH] 端口扫描行为
   扫描源: 103.45.67.89
   扫描端口: 21, 22, 23, 80, 443, 3306, 8080
   时间: 2026-01-05 15:20
   → 建议: 封禁该 IP 并检查防火墙规则

[MEDIUM] 异常流量峰值
   时间: 2026-01-05 10:00
   入站流量: 2.1GB/分钟
   源 IP: 多个（疑似 DDoS）
   → 建议: 启用流量清洗服务

📊 连接统计

TOP 5 连接源 IP:
  1. 192.168.1.50    8,234 次
  2. 10.0.0.25       5,123 次
  3. 172.16.0.100    3,891 次
  ...

TOP 5 目标端口:
  1. 443 (HTTPS)     15,234 次
  2. 80 (HTTP)       12,456 次
  3. 22 (SSH)         2,891 次
  ...
```

---

### 5. baselineconfig - 基线配置管理

**功能**：系统配置基线管理，确保配置合规性

**核心功能**：

#### 5.1 配置备份
- 系统配置文件备份
- 配置变更追踪
- 版本管理（类似 Git）
- 定时自动备份

#### 5.2 基线定义
- 标准配置模板
- 合规性规则
- 安全基线（等保、CIS Benchmark）
- 性能优化基线

#### 5.3 配置对比
- 当前配置 vs 基线配置
- 配置漂移检测
- 变更历史追踪
- Diff 可视化

#### 5.4 配置修复
- 一键恢复到基线
- 配置项批量修改
- 回滚到历史版本

#### 5.5 合规检查
- 等保 2.0 合规检查
- CIS Benchmark 检查
- 企业内部规范检查
- 生成合规报告

**管理的配置类型**：
```
系统配置:
  - /etc/sysctl.conf (内核参数)
  - /etc/security/limits.conf (系统限制)
  - /etc/fstab (挂载点)

网络配置:
  - /etc/network/interfaces (网络接口)
  - /etc/resolv.conf (DNS)
  - /etc/hosts

安全配置:
  - /etc/ssh/sshd_config (SSH)
  - /etc/pam.d/* (认证)
  - /etc/sudoers (sudo 权限)
  - /etc/selinux/config (SELinux)

服务配置:
  - /etc/nginx/nginx.conf
  - /etc/mysql/my.cnf
  - /etc/systemd/system/*
```

**使用方式**：
```bash
# 创建当前配置基线
baselineconfig --create baseline-v1.0

# 查看所有基线
baselineconfig --list

# 对比当前配置与基线
baselineconfig --compare baseline-v1.0

# 检查配置合规性
baselineconfig --check-compliance --standard cis-benchmark

# 恢复到基线配置
baselineconfig --restore baseline-v1.0

# 查看配置变更历史
baselineconfig --history /etc/ssh/sshd_config

# 导出基线配置
baselineconfig --export baseline-v1.0 --output /tmp/baseline.tar.gz
```

**输出示例**：
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
  配置基线对比报告
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

📅 对比时间: 2026-01-05 18:30
📋 基线版本: baseline-v1.0 (2025-12-01)
📊 检查项: 128

⚠️  检测到 8 个配置漂移

[HIGH] SSH 配置变更
   文件: /etc/ssh/sshd_config
   变更项: PermitRootLogin
   基线值: no
   当前值: yes
   风险: 允许 root 直接登录存在安全风险
   → 建议: 恢复为 no

[MEDIUM] 内核参数漂移
   文件: /etc/sysctl.conf
   变更项: net.core.somaxconn
   基线值: 1024
   当前值: 128
   影响: 可能导致高并发时连接队列不足
   → 建议: 恢复为 1024

[LOW] Nginx 配置变更
   文件: /etc/nginx/nginx.conf
   变更项: worker_processes
   基线值: 4
   当前值: 8
   影响: 无（性能优化）
   → 建议: 可保留

[详细对比...]

📈 合规性评分: 92/100
```

---

## 🚀 快速开始

### 安装 Skill

Skill 已安装到：`~/.claude/skills/deepin-sysassist/`

### 基础使用

#### 在 Claude Code 中触发
```
# 系统监控
帮我启动系统监控

# 系统体检
做个系统体检

# 日志分析
分析进程日志
分析网络日志

# 基线配置
创建配置基线
```

#### 命令行使用
```bash
# 系统监控
bash ~/.claude/skills/deepin-sysassist/scripts/monitor.sh

# 系统体检
bash ~/.claude/skills/deepin-sysassist/scripts/syscheck.sh

# 进程日志分析
bash ~/.claude/skills/deepin-sysassist/scripts/panalysis.sh

# 网络日志分析
bash ~/.claude/skills/deepin-sysassist/scripts/nanalysis.sh

# 基线配置
bash ~/.claude/skills/deepin-sysassist/scripts/baselineconfig.sh
```

---

## 📊 集成与自动化

### 1. Systemd 服务模式

创建监控服务：
```bash
# NOTE: systemd does not expand `~` or `$HOME` in ExecStart. Use an absolute path
# to the monitor script (aish seeds skills to ~/.config/aish/skills/deepin-sysassist/).
sudo tee /etc/systemd/system/deepin-sysassist-monitor.service <<EOF
[Unit]
Description=Deepin System Assistant Monitor Service
After=network.target

[Service]
Type=simple
User=root
ExecStart=/bin/bash /home/root/.config/aish/skills/deepin-sysassist/scripts/monitor.sh --daemon
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable deepin-sysassist-monitor
sudo systemctl start deepin-sysassist-monitor
```

### 2. Cron 定时任务

每日自动体检：
```bash
# 每天凌晨 2 点执行体检
0 2 * * * /bin/bash ~/.claude/skills/deepin-sysassist/scripts/syscheck.sh --silent --output /var/log/syscheck-$(date +\%Y\%m\%d).log
```

每小时日志分析：
```bash
# 每小时分析一次日志
0 * * * * /bin/bash ~/.claude/skills/deepin-sysassist/scripts/panalysis.sh --auto
0 * * * * /bin/bash ~/.claude/skills/deepin-sysassist/scripts/nanalysis.sh --auto
```

### 3. 告警集成

配置告警通知：
```bash
# 邮件告警
export ALERT_EMAIL="admin@example.com"

# 企业微信告警
export WECHAT_WEBHOOK="https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=xxx"

# 钉钉告警
export DINGTALK_WEBHOOK="https://oapi.dingtalk.com/robot/send?access_token=xxx"
```

---

## 🎨 自定义配置

### 监控阈值配置

编辑 `~/.claude/skills/deepin-sysassist/config/monitor.conf`:
```ini
[thresholds]
cpu_warning = 80
cpu_critical = 95
memory_warning = 80
memory_critical = 95
disk_warning = 85
disk_critical = 95

[intervals]
check_interval = 5
report_interval = 60
```

### 体检规则配置

编辑 `~/.claude/skills/deepin-sysassist/config/syscheck_rules.yaml`:
```yaml
hardware_checks:
  - cpu_temperature: max 80C
  - disk_smart: check all

security_checks:
  - ssh_root_login: deny
  - selinux_status: enforcing
  - firewall_status: enabled
```

---

## 📖 参考资源

- **Deepin 官网**: https://www.deepin.org/
- **Ubuntu 文档**: https://ubuntu.com/server/docs
- **Linux 性能优化**: http://www.brendangregg.com/
- **CIS Benchmark**: https://www.cisecurity.org/cis-benchmarks/

---

## 🆘 故障排除

### Q: 监控服务启动失败
```bash
# 检查日志
journalctl -u deepin-sysassist-monitor -n 50

# 检查权限
ls -l ~/.claude/skills/deepin-sysassist/scripts/monitor.sh
chmod +x ~/.claude/skills/deepin-sysassist/scripts/*.sh
```

### Q: 体检报告生成失败
```bash
# 检查磁盘空间
df -h /tmp

# 手动指定输出目录
syscheck --output /home/user/reports/
```

---

## 🔄 版本历史

- **v1.0.0** (2026-01-05): 初始版本
  - 实现 monitor, syscheck, panalysis, nanalysis, baselineconfig
  - 支持多种 Linux 发行版
  - 集成告警和报告功能

---

**Made with ❤️ for Deepin Users**
