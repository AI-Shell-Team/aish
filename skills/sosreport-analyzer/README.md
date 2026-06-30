# SOS Report Analyzer Skill

模拟 Red Hat `sosreport` 功能的 Claude Code 技能，用于Linux系统诊断和故障排查。

## 📋 功能特性

- ✅ **跨发行版支持**: Debian、Ubuntu、RHEL、CentOS、Fedora、Rocky Linux、AlmaLinux
- ✅ **全面数据收集**: 系统、内核、日志、网络、存储、内存、服务、安全、软件包
- ✅ **智能分析**: 自动检测性能问题、服务故障、安全风险
- ✅ **多格式输出**: 终端摘要、Markdown报告、JSON数据
- ✅ **隐私保护**: 自动脱敏敏感信息

## 🚀 快速开始

### 1. 安装 Skill

Skill 已安装到：`~/.claude/skills/sosreport-analyzer/`

### 2. 检查依赖

```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/check_dependencies.sh
```

### 3. 收集系统数据

```bash
# 普通用户（部分功能受限）
bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh

# Root 用户（推荐，获取完整信息）
sudo bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

### 4. 分析报告

```bash
# 自动分析并生成报告
bash ~/.claude/skills/sosreport-analyzer/scripts/analyze.sh /tmp/sosreport-YYYYMMDD-HHMMSS/
```

## 🎯 使用方式

### 方式一：Claude Code 交互（推荐）

在 Claude Code 中直接输入：

```
帮我做个系统诊断
```

或

```
运行 sosreport 收集系统信息并分析
```

或

```
系统健康检查
```

Claude 会自动触发此 skill，执行数据收集和分析。

### 方式二：手动执行

```bash
# 1. 收集数据
cd ~/.claude/skills/sosreport-analyzer/scripts
bash collect_all.sh

# 2. 分析数据
bash analyze.sh /tmp/sosreport-<timestamp>/

# 3. 查看报告
cat /tmp/sosreport-<timestamp>/analysis_report.md
```

## 📊 输出示例

### 终端摘要
```
═══════════════════════════════════════════════════════════
  系统诊断报告 - SOS Report Analyzer v1.0
═══════════════════════════════════════════════════════════

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
   → 建议: 检查 /dev/sda 性能

[MEDIUM] 存在 2 个失败的服务
   → 建议: journalctl -u <service>

✅ 健康检查
   [✓] 内存使用正常 (62%)
   [✓] 网络接口无丢包
```

### 生成的文件
```
/tmp/sosreport-20260105-143022/
├── system/           # 系统信息
│   ├── os-release.txt
│   ├── lscpu.txt
│   └── ...
├── kernel/           # 内核信息
├── logs/             # 日志文件
├── network/          # 网络配置
├── storage/          # 存储信息
├── memory/           # 内存和性能
├── services/         # 服务状态
├── security/         # 安全配置
├── packages/         # 软件包
├── custom/           # 自定义（容器等）
├── metadata.json     # 元数据
├── collection.log    # 收集日志
└── analysis_report.md # 分析报告

压缩包: /tmp/sosreport-hostname-20260105-143022.tar.gz
```

## 🛠️ 高级用法

### 定制化收集

编辑 `scripts/collect_all.sh`，注释掉不需要的模块：

```bash
# collect_container_info  # 跳过容器信息收集
```

### 添加自定义收集器

在 `scripts/custom/` 目录创建脚本：

```bash
#!/bin/bash
# scripts/custom/collect_mysql.sh

if command -v mysql &>/dev/null; then
    mysql -e "SHOW STATUS" > "${REPORT_DIR}/custom/mysql_status.txt"
fi
```

然后在 `collect_all.sh` 中调用：

```bash
# 在 main() 函数中添加
bash scripts/custom/collect_mysql.sh
```

### 定时巡检

创建 cron 任务（需要 sudo）：

```bash
# 每周一早上 8 点执行
0 8 * * 1 /bin/bash /home/user/.claude/skills/sosreport-analyzer/scripts/collect_all.sh
```

## 🔍 智能分析规则

当前支持的自动检测：

### 性能问题
- ✅ 磁盘空间不足 (>95%, >85%, >75%)
- ✅ Inode 耗尽 (>90%)
- ✅ 内存压力 (<10% 可用)
- ✅ CPU iowait 过高 (>30%, >15%)

### 服务异常
- ✅ 失败的 systemd 服务
- ✅ 僵尸进程累积 (>10, >0)

### 网络问题
- ✅ 网络接口丢包/错误

### 安全风险
- ✅ SELinux 禁用或 Permissive
- ✅ SELinux AVC 拒绝事件
- ✅ SSH root 登录启用
- ✅ SSH 密码认证启用

## 📦 依赖工具

### 核心工具（必需）
- systemctl, journalctl (systemd)
- ip, ss (iproute2)
- df, lsblk (util-linux)
- ps, top (procps)

### 增强工具（推荐）
- iostat, mpstat (sysstat)
- iotop
- lscpu, lsmem (util-linux)
- dmidecode (需要 root)
- smartctl (smartmontools)

### 安装命令

**Debian/Ubuntu:**
```bash
sudo apt update
sudo apt install sysstat iotop smartmontools dmidecode pciutils usbutils
```

**RHEL/CentOS/Fedora:**
```bash
sudo dnf install sysstat iotop smartmontools dmidecode pciutils usbutils
```

## 🔒 隐私与安全

### 数据脱敏
- IP 地址模糊化（保留网段）
- MAC 地址模糊化（保留厂商前缀）
- 密码/密钥文件内容不收集

### 数据存储
- 默认保存到 `/tmp/`
- 建议 7 天后删除
- **不会上传到任何外部服务器**

### 权限要求
- 普通用户：可收集大部分信息
- Root 用户：获取完整硬件、审计日志、I/O 详情

## 🆘 故障排除

### Q: 脚本执行权限错误
```bash
chmod +x ~/.claude/skills/sosreport-analyzer/scripts/*.sh
```

### Q: 缺少依赖工具
```bash
bash ~/.claude/skills/sosreport-analyzer/scripts/check_dependencies.sh
```

### Q: 磁盘空间不足
```bash
# 使用其他目录
REPORT_DIR="/home/user/sosreport-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$REPORT_DIR"
# 修改脚本中的 REPORT_DIR 变量
```

### Q: 某些模块被跳过
- 检查日志: `cat /tmp/sosreport-*/collection.log`
- 安装缺失工具或以 root 运行

## 📖 参考资源

- **Red Hat sosreport**: https://access.redhat.com/solutions/3592
- **Linux Performance**: http://www.brendangregg.com/linuxperf.html
- **Systemd Documentation**: https://www.freedesktop.org/software/systemd/man/

## 🔄 版本历史

- **v1.0.0** (2026-01-05): 初始版本
  - 支持 10 大模块数据收集
  - 智能分析引擎
  - 多格式报告输出

## 📄 许可证

MIT License

## 🤝 贡献

欢迎提交 Issue 和 Pull Request！

---

**Made with ❤️ by Claude AI Assistant**
