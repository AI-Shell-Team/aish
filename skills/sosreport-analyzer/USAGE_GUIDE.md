# SOS Report Analyzer Skill - 使用指南

## ✅ 安装验证

Skill 已成功安装到：`~/.claude/skills/sosreport-analyzer/`

### 目录结构
```
~/.claude/skills/sosreport-analyzer/
├── SKILL.md              # Skill 定义文件（Claude 自动加载）
├── README.md             # 完整使用文档
├── scripts/              # 可执行脚本
│   ├── collect_all.sh    # 数据收集脚本
│   ├── analyze.sh        # 智能分析脚本
│   ├── check_dependencies.sh  # 依赖检查
│   └── quickstart.sh     # 一键启动脚本
└── templates/            # 模板目录（可扩展）
```

---

## 🚀 三种使用方式

### 方式一：Claude Code 自动触发（最简单）

在 Claude Code 中直接对话：

```
你: 帮我做个系统诊断

或者:

你: 运行 sosreport 收集系统信息

或者:

你: 系统健康检查，看看有什么问题
```

Claude 会自动：
1. 识别 sosreport 触发词
2. 执行数据收集脚本
3. 分析数据
4. 生成诊断报告

---

### 方式二：快速启动脚本（推荐）

一键完成所有步骤：

```bash
# 普通用户运行（部分功能受限）
bash ~/.claude/skills/sosreport-analyzer/scripts/quickstart.sh

# Root 用户运行（推荐，获取完整信息）
sudo bash ~/.claude/skills/sosreport-analyzer/scripts/quickstart.sh
```

交互流程：
1. ✅ 自动检查系统依赖
2. ✅ 收集系统数据（约 30-60 秒）
3. ✅ 智能分析并生成报告
4. ✅ 显示诊断结果和建议

---

### 方式三：手动分步执行（高级用户）

#### 步骤1: 检查依赖

```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/check_dependencies.sh
```

输出示例：
```
核心工具检查（必需）:
  [✓] systemctl
  [✓] journalctl
  [✓] ip
  ...

增强工具检查（推荐）:
  [✓] iostat
  [!] iotop (推荐安装)
  ...
```

#### 步骤2: 收集数据

```bash
# 普通用户
bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh

# Root 用户（推荐）
sudo bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

收集完成后会显示：
```
数据收集完成！

  📁 原始数据: /tmp/sosreport-20260105-181234
  📦 压缩包: /tmp/sosreport-hostname-20260105-181234.tar.gz
  📊 大小: 2.3M
```

#### 步骤3: 分析数据

```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/analyze.sh /tmp/sosreport-20260105-181234/
```

会生成：
- 终端彩色摘要报告
- Markdown 详细报告（`analysis_report.md`）

---

## 📖 实际使用示例

### 示例1: 服务器性能问题排查

**场景**: 生产服务器响应缓慢

```bash
# 1. 快速收集数据
sudo bash ~/.claude/skills/sosreport-analyzer/scripts/quickstart.sh

# 2. 查看分析结果
# 输出可能显示:
# [HIGH] CPU iowait 过高 (42%)
#    → 建议: 检查 iostat 定位慢速磁盘

# 3. 深入分析 I/O 问题
cat /tmp/sosreport-*/storage/iostat.txt

# 4. 查看 I/O 密集型进程
cat /tmp/sosreport-*/memory/ps-cpu.txt
```

### 示例2: 系统定期巡检

**场景**: 每周一自动生成健康报告

```bash
# 创建 cron 任务
sudo crontab -e

# 添加:
0 8 * * 1 /bin/bash "$HOME/.config/aish/skills/sosreport-analyzer/scripts/collect_all.sh" > /var/log/sosreport-weekly.log 2>&1
```

### 示例3: 故障工单附件

**场景**: 提交故障工单时需要附加系统信息

```bash
# 1. 收集数据
sudo bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh

# 2. 上传压缩包
# /tmp/sosreport-hostname-TIMESTAMP.tar.gz

# 3. 在工单中附加 analysis_report.md
```

### 示例4: 升级前系统评估

**场景**: RHEL 7 升级到 RHEL 8 前的系统扫描

```bash
# 1. 收集完整数据
sudo bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh

# 2. 检查关键配置
cat /tmp/sosreport-*/kernel/grub.cfg
cat /tmp/sosreport-*/packages/rpm-qa.txt

# 3. 保存基线
tar czf ~/sosreport-baseline-rhel7.tar.gz /tmp/sosreport-*

# 4. 升级后对比
# 再次运行收集，对比两次报告
```

---

## 🔍 输出文件详解

### 数据收集目录结构

```
/tmp/sosreport-20260105-181234/
├── metadata.json              # 元数据（主机名、时间、内核版本等）
├── collection.log             # 收集过程日志
│
├── system/                    # 系统基础信息
│   ├── os-release.txt         # 发行版信息
│   ├── lscpu.txt              # CPU 详情
│   ├── lsmem.txt              # 内存配置
│   ├── dmidecode.txt          # 硬件详情（需要 root）
│   └── virtualization.txt     # 虚拟化平台检测
│
├── kernel/                    # 内核与启动
│   ├── uname.txt              # 内核版本
│   ├── cmdline.txt            # 启动参数
│   ├── modules.txt            # 已加载模块
│   ├── dmesg.txt              # 启动日志
│   ├── systemd-analyze.txt    # 启动时间分析
│   └── grub.cfg               # GRUB 配置
│
├── logs/                      # 日志文件
│   ├── journalctl-errors.txt  # 最近 7 天错误
│   ├── journalctl-boot.txt    # 本次启动日志
│   ├── journalctl-sshd.txt    # SSH 服务日志
│   └── audit.log              # 审计日志（需要 root）
│
├── network/                   # 网络配置
│   ├── ip-addr.txt            # IP 地址
│   ├── ip-route.txt           # 路由表
│   ├── ss-all.txt             # 所有连接
│   ├── resolv.conf            # DNS 配置
│   ├── firewalld.txt          # 防火墙规则
│   └── nmcli-connection.txt   # NetworkManager 连接
│
├── storage/                   # 存储信息
│   ├── lsblk.txt              # 块设备
│   ├── df.txt                 # 磁盘使用
│   ├── df-inodes.txt          # Inode 使用
│   ├── mount.txt              # 挂载点
│   ├── fstab                  # 自动挂载配置
│   ├── pvs.txt, vgs.txt, lvs.txt  # LVM 信息
│   ├── smart-sda.txt          # 磁盘健康（需要 root）
│   └── iostat.txt             # I/O 性能
│
├── memory/                    # 内存与性能
│   ├── free.txt               # 内存状态
│   ├── meminfo.txt            # 详细内存信息
│   ├── vmstat.txt             # 虚拟内存统计
│   ├── slabtop.txt            # Slab 缓存（需要 root）
│   ├── top.txt                # 进程快照
│   ├── ps-mem.txt             # 内存消耗 Top 30
│   └── ps-cpu.txt             # CPU 消耗 Top 30
│
├── services/                  # 服务状态
│   ├── systemctl-list-units.txt  # 所有服务单元
│   ├── systemctl-failed.txt   # 失败的服务
│   ├── sshd-status.txt        # SSH 服务详情
│   └── ps-forest.txt          # 进程树
│
├── security/                  # 安全配置
│   ├── selinux-status.txt     # SELinux 状态
│   ├── selinux-denials.txt    # AVC 拒绝日志
│   ├── auditctl-rules.txt     # 审计规则
│   ├── last.txt               # 登录历史
│   ├── sshd_config            # SSH 配置
│   ├── passwd, group          # 用户和组
│   └── suid-files.txt         # SUID 文件列表
│
├── packages/                  # 软件包
│   ├── rpm-qa.txt             # 已安装 RPM 包（RHEL）
│   ├── dpkg-list.txt          # 已安装 DEB 包（Debian）
│   ├── dnf-repolist.txt       # 仓库列表
│   └── kernels.txt            # 已安装内核
│
├── custom/                    # 自定义/可选
│   ├── docker-info.txt        # Docker 信息
│   ├── docker-ps.txt          # 容器列表
│   └── podman-info.txt        # Podman 信息
│
└── analysis_report.md         # 🔥 智能分析报告（分析后生成）
```

---

## 🧪 测试验证

### 快速测试

```bash
# 1. 测试依赖检查
bash ~/.claude/skills/sosreport-analyzer/scripts/check_dependencies.sh

# 2. 测试数据收集（快速，约 10 秒）
bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh

# 3. 验证文件生成
ls -lh /tmp/sosreport-*/

# 4. 测试分析
bash ~/.claude/skills/sosreport-analyzer/scripts/analyze.sh /tmp/sosreport-*/

# 5. 查看报告
cat /tmp/sosreport-*/analysis_report.md
```

### 预期输出

收集完成后应该看到：
```
✅ 收集: cat /etc/os-release
✅ 收集: uname -a
✅ 收集: lscpu
⚠️  跳过: dmidecode (需要 root 权限)
...
数据收集完成！
📦 压缩包: /tmp/sosreport-hostname-TIMESTAMP.tar.gz
```

分析完成后应该看到：
```
═══════════════════════════════════════════════════════════
  系统诊断报告 - SOS Report Analyzer v1.0
═══════════════════════════════════════════════════════════

📌 系统信息
   主机名: ...
   操作系统: ...

⚠️  检测到 X 个问题
[SEVERITY] 问题描述
   → 建议: ...
```

---

## 💡 高级技巧

### 1. 远程收集

```bash
# SSH 到远程服务器执行
ssh user@remote-server 'bash -s' < ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh

# 下载结果
scp user@remote-server:/tmp/sosreport-*.tar.gz ./
```

### 2. 批量收集（多台服务器）

```bash
#!/bin/bash
SERVERS=(server1 server2 server3)

for server in "${SERVERS[@]}"; do
    echo "收集 $server ..."
    ssh "$server" 'sudo bash -s' < ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
    scp "$server:/tmp/sosreport-*.tar.gz" "./sosreport-${server}.tar.gz"
done
```

### 3. 定时清理

```bash
# 自动清理 7 天前的报告
find /tmp -name "sosreport-*" -type d -mtime +7 -exec rm -rf {} +
find /tmp -name "sosreport-*.tar.gz" -mtime +7 -delete
```

### 4. 与监控系统集成

```bash
# 触发条件：CPU 使用率 > 90%
if [ $(top -bn1 | grep "Cpu(s)" | awk '{print $2}' | cut -d'%' -f1) -gt 90 ]; then
    bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
    # 发送告警并附加报告
fi
```

---

## 🛠️ 故障排除

### 问题1: 脚本无执行权限

```bash
chmod +x ~/.claude/skills/sosreport-analyzer/scripts/*.sh
```

### 问题2: 缺少工具

```bash
# 先检查缺失项
bash ~/.claude/skills/sosreport-analyzer/scripts/check_dependencies.sh

# Debian/Ubuntu 安装
sudo apt install sysstat iotop smartmontools

# RHEL/CentOS 安装
sudo dnf install sysstat iotop smartmontools
```

### 问题3: 磁盘空间不足

```bash
# 使用其他目录
export TMPDIR=/home/user/tmp
mkdir -p $TMPDIR
bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

### 问题4: Claude 未触发 Skill

检查触发词是否正确：
- ✅ "sosreport"
- ✅ "系统诊断"
- ✅ "system diagnostic"
- ✅ "收集日志"
- ✅ "troubleshooting"

如果仍未触发，手动执行：
```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/quickstart.sh
```

---

## 📚 更多资源

- **详细文档**: `~/.claude/skills/sosreport-analyzer/README.md`
- **Skill 定义**: `~/.claude/skills/sosreport-analyzer/SKILL.md`
- **Red Hat sosreport**: https://access.redhat.com/solutions/3592

---

**Skill 版本**: v1.0.0
**创建时间**: 2026-01-05
**兼容平台**: Debian, Ubuntu, RHEL, CentOS, Fedora, Rocky Linux, AlmaLinux
