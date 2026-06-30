#!/bin/bash
#
# SOS Report Analyzer - 主数据收集脚本
# 版本: 1.0.0
# 兼容: Debian, Ubuntu, RHEL, CentOS, Fedora, Rocky, Alma
#

set -euo pipefail

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 全局变量
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
REPORT_DIR="/tmp/sosreport-${TIMESTAMP}"
HOSTNAME=$(hostname -s)
CURRENT_USER=$(whoami)
IS_ROOT=false
[[ $EUID -eq 0 ]] && IS_ROOT=true

# 创建输出目录
mkdir -p "${REPORT_DIR}"/{system,kernel,logs,network,storage,memory,services,security,packages,custom}

# 日志函数
log() {
    echo -e "${BLUE}[INFO]${NC} $*" | tee -a "${REPORT_DIR}/collection.log"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $*" | tee -a "${REPORT_DIR}/collection.log"
}

error() {
    echo -e "${RED}[ERROR]${NC} $*" | tee -a "${REPORT_DIR}/collection.log"
}

success() {
    echo -e "${GREEN}[✓]${NC} $*" | tee -a "${REPORT_DIR}/collection.log"
}

# 检查命令是否存在
command_exists() {
    command -v "$1" &>/dev/null
}

# 安全执行命令（失败不中断）
safe_exec() {
    local output_file="$1"
    shift
    local cmd="$*"

    if eval "$cmd" > "$output_file" 2>&1; then
        success "收集: $cmd"
    else
        warn "跳过: $cmd (命令失败或需要更高权限)"
    fi
}

# 检测发行版
detect_distro() {
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        echo "$ID"
    elif [ -f /etc/redhat-release ]; then
        echo "rhel"
    elif [ -f /etc/debian_version ]; then
        echo "debian"
    else
        echo "unknown"
    fi
}

DISTRO=$(detect_distro)

# ============================================
# 1. 系统基础信息收集
# ============================================
collect_system_info() {
    log "收集系统基础信息..."

    # 发行版信息
    safe_exec "${REPORT_DIR}/system/os-release.txt" cat /etc/os-release
    safe_exec "${REPORT_DIR}/system/uname.txt" uname -a
    safe_exec "${REPORT_DIR}/system/hostname.txt" hostname -f
    safe_exec "${REPORT_DIR}/system/uptime.txt" uptime
    safe_exec "${REPORT_DIR}/system/date.txt" date

    # CPU 信息
    if command_exists lscpu; then
        safe_exec "${REPORT_DIR}/system/lscpu.txt" lscpu
        safe_exec "${REPORT_DIR}/system/lscpu-extended.txt" lscpu -e
    fi
    safe_exec "${REPORT_DIR}/system/cpuinfo.txt" cat /proc/cpuinfo

    # 内存信息
    if command_exists lsmem; then
        safe_exec "${REPORT_DIR}/system/lsmem.txt" lsmem
    fi

    # 硬件信息（需要 root）
    if $IS_ROOT; then
        if command_exists dmidecode; then
            safe_exec "${REPORT_DIR}/system/dmidecode.txt" dmidecode
        fi
        if command_exists lspci; then
            safe_exec "${REPORT_DIR}/system/lspci.txt" lspci -vvv
        fi
        if command_exists lsusb; then
            safe_exec "${REPORT_DIR}/system/lsusb.txt" lsusb -v
        fi
    else
        if command_exists lspci; then
            safe_exec "${REPORT_DIR}/system/lspci.txt" lspci
        fi
        if command_exists lsusb; then
            safe_exec "${REPORT_DIR}/system/lsusb.txt" lsusb
        fi
    fi

    # 虚拟化检测
    if command_exists systemd-detect-virt; then
        safe_exec "${REPORT_DIR}/system/virtualization.txt" systemd-detect-virt
    fi
    if command_exists virt-what && $IS_ROOT; then
        safe_exec "${REPORT_DIR}/system/virt-what.txt" virt-what
    fi
}

# ============================================
# 2. 内核与启动信息收集
# ============================================
collect_kernel_info() {
    log "收集内核与启动信息..."

    safe_exec "${REPORT_DIR}/kernel/version.txt" cat /proc/version
    safe_exec "${REPORT_DIR}/kernel/cmdline.txt" cat /proc/cmdline
    safe_exec "${REPORT_DIR}/kernel/modules.txt" lsmod
    safe_exec "${REPORT_DIR}/kernel/dmesg.txt" dmesg

    # Systemd 启动分析
    if command_exists systemd-analyze; then
        safe_exec "${REPORT_DIR}/kernel/systemd-analyze.txt" systemd-analyze
        safe_exec "${REPORT_DIR}/kernel/systemd-analyze-blame.txt" systemd-analyze blame
        safe_exec "${REPORT_DIR}/kernel/systemd-analyze-critical-chain.txt" systemd-analyze critical-chain
    fi

    # GRUB 配置
    if [ -f /boot/grub2/grub.cfg ]; then
        safe_exec "${REPORT_DIR}/kernel/grub.cfg" cat /boot/grub2/grub.cfg
    elif [ -f /boot/grub/grub.cfg ]; then
        safe_exec "${REPORT_DIR}/kernel/grub.cfg" cat /boot/grub/grub.cfg
    fi

    if [ -f /etc/default/grub ]; then
        safe_exec "${REPORT_DIR}/kernel/grub-default.txt" cat /etc/default/grub
    fi
}

# ============================================
# 3. 日志收集
# ============================================
collect_logs() {
    log "收集系统日志..."

    # Systemd Journal
    if command_exists journalctl; then
        safe_exec "${REPORT_DIR}/logs/journalctl-errors.txt" journalctl --no-pager -p err --since "7 days ago"
        safe_exec "${REPORT_DIR}/logs/journalctl-boot.txt" journalctl --no-pager -b
        safe_exec "${REPORT_DIR}/logs/journalctl-disk-usage.txt" journalctl --disk-usage

        # 关键服务日志
        for service in sshd NetworkManager systemd-networkd firewalld docker containerd nginx apache2 mysql postgresql; do
            if systemctl list-units --type=service --all | grep -q "$service"; then
                safe_exec "${REPORT_DIR}/logs/journalctl-${service}.txt" journalctl -u "$service" --no-pager --since "24 hours ago"
            fi
        done
    fi

    # 传统日志文件（如果存在）
    for logfile in /var/log/messages /var/log/syslog /var/log/auth.log /var/log/secure; do
        if [ -f "$logfile" ]; then
            safe_exec "${REPORT_DIR}/logs/$(basename $logfile)" tail -n 1000 "$logfile"
        fi
    done

    # 审计日志（需要 root）
    if $IS_ROOT && [ -f /var/log/audit/audit.log ]; then
        safe_exec "${REPORT_DIR}/logs/audit.log" tail -n 1000 /var/log/audit/audit.log
    fi
}

# ============================================
# 4. 网络配置收集
# ============================================
collect_network_info() {
    log "收集网络配置..."

    # 网络接口
    if command_exists ip; then
        safe_exec "${REPORT_DIR}/network/ip-addr.txt" ip addr show
        safe_exec "${REPORT_DIR}/network/ip-link.txt" ip link show
        safe_exec "${REPORT_DIR}/network/ip-route.txt" ip route show
        safe_exec "${REPORT_DIR}/network/ip-link-stats.txt" ip -s link
    fi

    # 网络连接
    if command_exists ss; then
        safe_exec "${REPORT_DIR}/network/ss-listening.txt" ss -tulpn
        safe_exec "${REPORT_DIR}/network/ss-all.txt" ss -tunap
    elif command_exists netstat; then
        safe_exec "${REPORT_DIR}/network/netstat-listening.txt" netstat -tulpn
        safe_exec "${REPORT_DIR}/network/netstat-all.txt" netstat -tunap
    fi

    # ARP 表
    safe_exec "${REPORT_DIR}/network/arp.txt" arp -n

    # DNS 配置
    safe_exec "${REPORT_DIR}/network/resolv.conf" cat /etc/resolv.conf
    safe_exec "${REPORT_DIR}/network/hosts" cat /etc/hosts
    safe_exec "${REPORT_DIR}/network/nsswitch.conf" cat /etc/nsswitch.conf

    # 防火墙
    if command_exists iptables && $IS_ROOT; then
        safe_exec "${REPORT_DIR}/network/iptables.txt" iptables -L -n -v
        safe_exec "${REPORT_DIR}/network/iptables-nat.txt" iptables -t nat -L -n -v
    fi

    if command_exists nft && $IS_ROOT; then
        safe_exec "${REPORT_DIR}/network/nftables.txt" nft list ruleset
    fi

    if command_exists firewall-cmd; then
        safe_exec "${REPORT_DIR}/network/firewalld.txt" firewall-cmd --list-all
        safe_exec "${REPORT_DIR}/network/firewalld-zones.txt" firewall-cmd --list-all-zones
    fi

    if command_exists ufw; then
        safe_exec "${REPORT_DIR}/network/ufw.txt" ufw status verbose
    fi

    # NetworkManager
    if command_exists nmcli; then
        safe_exec "${REPORT_DIR}/network/nmcli-connection.txt" nmcli connection show
        safe_exec "${REPORT_DIR}/network/nmcli-device.txt" nmcli device show
    fi
}

# ============================================
# 5. 存储信息收集
# ============================================
collect_storage_info() {
    log "收集存储配置..."

    # 块设备
    if command_exists lsblk; then
        safe_exec "${REPORT_DIR}/storage/lsblk.txt" lsblk -f
        safe_exec "${REPORT_DIR}/storage/lsblk-all.txt" lsblk -a
    fi

    # 分区表
    if $IS_ROOT; then
        if command_exists fdisk; then
            safe_exec "${REPORT_DIR}/storage/fdisk.txt" fdisk -l
        fi
        if command_exists parted; then
            safe_exec "${REPORT_DIR}/storage/parted.txt" parted -l
        fi
    fi

    safe_exec "${REPORT_DIR}/storage/blkid.txt" blkid

    # 文件系统
    safe_exec "${REPORT_DIR}/storage/df.txt" df -h
    safe_exec "${REPORT_DIR}/storage/df-inodes.txt" df -i
    safe_exec "${REPORT_DIR}/storage/mount.txt" mount
    safe_exec "${REPORT_DIR}/storage/fstab" cat /etc/fstab

    # LVM
    if command_exists pvs; then
        safe_exec "${REPORT_DIR}/storage/pvs.txt" pvs
        safe_exec "${REPORT_DIR}/storage/vgs.txt" vgs
        safe_exec "${REPORT_DIR}/storage/lvs.txt" lvs

        if $IS_ROOT; then
            safe_exec "${REPORT_DIR}/storage/pvdisplay.txt" pvdisplay
            safe_exec "${REPORT_DIR}/storage/vgdisplay.txt" vgdisplay
            safe_exec "${REPORT_DIR}/storage/lvdisplay.txt" lvdisplay
        fi
    fi

    # RAID
    if [ -f /proc/mdstat ]; then
        safe_exec "${REPORT_DIR}/storage/mdstat.txt" cat /proc/mdstat
    fi

    if command_exists mdadm && $IS_ROOT; then
        # 扫描所有 RAID 设备
        for md in /dev/md*; do
            [ -b "$md" ] && safe_exec "${REPORT_DIR}/storage/mdadm-$(basename $md).txt" mdadm --detail "$md"
        done
    fi

    # 磁盘健康（需要 smartmontools）
    if command_exists smartctl && $IS_ROOT; then
        for disk in /dev/sd? /dev/nvme?n?; do
            [ -b "$disk" ] && safe_exec "${REPORT_DIR}/storage/smart-$(basename $disk).txt" smartctl -a "$disk"
        done
    fi

    # I/O 统计
    if command_exists iostat; then
        safe_exec "${REPORT_DIR}/storage/iostat.txt" iostat -x 1 5
    fi
}

# ============================================
# 6. 内存与性能收集
# ============================================
collect_memory_performance() {
    log "收集内存与性能数据..."

    # 内存状态
    safe_exec "${REPORT_DIR}/memory/free.txt" free -h
    safe_exec "${REPORT_DIR}/memory/meminfo.txt" cat /proc/meminfo
    safe_exec "${REPORT_DIR}/memory/vmstat.txt" vmstat 1 5

    # Slab 缓存
    if command_exists slabtop && $IS_ROOT; then
        safe_exec "${REPORT_DIR}/memory/slabtop.txt" slabtop -o --once
    fi
    safe_exec "${REPORT_DIR}/memory/slabinfo.txt" cat /proc/slabinfo

    # CPU 性能
    safe_exec "${REPORT_DIR}/memory/top.txt" top -b -n 3
    safe_exec "${REPORT_DIR}/memory/uptime.txt" uptime

    if command_exists mpstat; then
        safe_exec "${REPORT_DIR}/memory/mpstat.txt" mpstat -P ALL 1 3
    fi

    # 进程信息
    # Pass the pipeline as a single quoted string so safe_exec's eval sees the `|`
    # instead of the outer shell applying it to safe_exec's own stdout.
    safe_exec "${REPORT_DIR}/memory/ps-mem.txt" "ps aux --sort=-%mem | head -30"
    safe_exec "${REPORT_DIR}/memory/ps-cpu.txt" "ps aux --sort=-%cpu | head -30"
    safe_exec "${REPORT_DIR}/memory/pstree.txt" pstree -p

    # Swap
    safe_exec "${REPORT_DIR}/memory/swapon.txt" swapon -s
    safe_exec "${REPORT_DIR}/memory/swaps.txt" cat /proc/swaps
}

# ============================================
# 7. 服务状态收集
# ============================================
collect_services() {
    log "收集服务状态..."

    if command_exists systemctl; then
        safe_exec "${REPORT_DIR}/services/systemctl-list-units.txt" systemctl list-units --type=service --all
        safe_exec "${REPORT_DIR}/services/systemctl-failed.txt" systemctl --failed
        safe_exec "${REPORT_DIR}/services/systemctl-timers.txt" systemctl list-timers

        # 关键服务详细状态
        for service in sshd NetworkManager firewalld docker nginx apache2 mysql postgresql; do
            if systemctl list-units --type=service --all | grep -q "$service"; then
                safe_exec "${REPORT_DIR}/services/${service}-status.txt" systemctl status "$service"
            fi
        done
    fi

    # 进程树
    safe_exec "${REPORT_DIR}/services/ps-forest.txt" ps auxf
}

# ============================================
# 8. 安全配置收集
# ============================================
collect_security() {
    log "收集安全配置..."

    # SELinux (RHEL/CentOS)
    if command_exists getenforce; then
        safe_exec "${REPORT_DIR}/security/selinux-status.txt" getenforce
        safe_exec "${REPORT_DIR}/security/sestatus.txt" sestatus

        if $IS_ROOT && command_exists ausearch; then
            safe_exec "${REPORT_DIR}/security/selinux-denials.txt" ausearch -m AVC -ts recent
        fi
    fi

    # AppArmor (Debian/Ubuntu)
    if command_exists aa-status; then
        safe_exec "${REPORT_DIR}/security/apparmor-status.txt" aa-status
    fi

    # 审计系统
    if command_exists auditctl && $IS_ROOT; then
        safe_exec "${REPORT_DIR}/security/auditctl-rules.txt" auditctl -l
    fi

    if command_exists aureport && $IS_ROOT; then
        safe_exec "${REPORT_DIR}/security/aureport.txt" aureport --summary
    fi

    # 登录记录
    safe_exec "${REPORT_DIR}/security/last.txt" last -20
    safe_exec "${REPORT_DIR}/security/lastb.txt" lastb -20
    safe_exec "${REPORT_DIR}/security/who.txt" who
    safe_exec "${REPORT_DIR}/security/w.txt" w

    # SSH 配置（敏感信息需脱敏）
    if [ -f /etc/ssh/sshd_config ]; then
        safe_exec "${REPORT_DIR}/security/sshd_config" "grep -v '^#' /etc/ssh/sshd_config | grep -v '^$'"
    fi

    # 用户与组
    safe_exec "${REPORT_DIR}/security/passwd" cat /etc/passwd
    safe_exec "${REPORT_DIR}/security/group" cat /etc/group

    # SUID/SGID 文件
    if $IS_ROOT; then
        safe_exec "${REPORT_DIR}/security/suid-files.txt" find / -perm -4000 -type f 2>/dev/null
        safe_exec "${REPORT_DIR}/security/sgid-files.txt" find / -perm -2000 -type f 2>/dev/null
    fi
}

# ============================================
# 9. 软件包管理收集
# ============================================
collect_packages() {
    log "收集软件包信息..."

    case "$DISTRO" in
        rhel|centos|fedora|rocky|alma)
            if command_exists rpm; then
                safe_exec "${REPORT_DIR}/packages/rpm-qa.txt" rpm -qa
            fi
            if command_exists dnf; then
                safe_exec "${REPORT_DIR}/packages/dnf-repolist.txt" dnf repolist
                safe_exec "${REPORT_DIR}/packages/dnf-history.txt" dnf history
                safe_exec "${REPORT_DIR}/packages/dnf-check.txt" dnf check
            elif command_exists yum; then
                safe_exec "${REPORT_DIR}/packages/yum-repolist.txt" yum repolist
                safe_exec "${REPORT_DIR}/packages/yum-history.txt" yum history
            fi
            ;;
        debian|ubuntu)
            if command_exists dpkg; then
                safe_exec "${REPORT_DIR}/packages/dpkg-list.txt" dpkg -l
            fi
            if command_exists apt; then
                safe_exec "${REPORT_DIR}/packages/apt-list.txt" apt list --installed
            fi
            ;;
    esac

    # 内核包
    case "$DISTRO" in
        rhel|centos|fedora|rocky|alma)
            safe_exec "${REPORT_DIR}/packages/kernels.txt" "rpm -qa | grep kernel"
            ;;
        debian|ubuntu)
            safe_exec "${REPORT_DIR}/packages/kernels.txt" "dpkg -l | grep linux-image"
            ;;
    esac
}

# ============================================
# 10. 容器信息收集（可选）
# ============================================
collect_container_info() {
    log "收集容器信息（如果存在）..."

    # Docker
    if command_exists docker; then
        safe_exec "${REPORT_DIR}/custom/docker-info.txt" docker info
        safe_exec "${REPORT_DIR}/custom/docker-ps.txt" docker ps -a
        safe_exec "${REPORT_DIR}/custom/docker-images.txt" docker images
        safe_exec "${REPORT_DIR}/custom/docker-networks.txt" docker network ls
        safe_exec "${REPORT_DIR}/custom/docker-volumes.txt" docker volume ls
    fi

    # Podman
    if command_exists podman; then
        safe_exec "${REPORT_DIR}/custom/podman-info.txt" podman info
        safe_exec "${REPORT_DIR}/custom/podman-ps.txt" podman ps -a
        safe_exec "${REPORT_DIR}/custom/podman-images.txt" podman images
    fi
}

# ============================================
# 主执行流程
# ============================================
main() {
    echo "═══════════════════════════════════════════════════════════"
    echo "  SOS Report Analyzer - 系统数据收集工具"
    echo "  版本: 1.0.0"
    echo "═══════════════════════════════════════════════════════════"
    echo ""
    log "主机名: $HOSTNAME"
    log "用户: $CURRENT_USER"
    log "Root 权限: $IS_ROOT"
    log "发行版: $DISTRO"
    log "输出目录: $REPORT_DIR"
    echo ""

    # 执行所有收集模块
    collect_system_info
    collect_kernel_info
    collect_logs
    collect_network_info
    collect_storage_info
    collect_memory_performance
    collect_services
    collect_security
    collect_packages
    collect_container_info

    # 生成元数据
    cat > "${REPORT_DIR}/metadata.json" <<EOF
{
  "version": "1.0.0",
  "timestamp": "$(date -Iseconds)",
  "hostname": "$HOSTNAME",
  "user": "$CURRENT_USER",
  "is_root": $IS_ROOT,
  "distribution": "$DISTRO",
  "kernel": "$(uname -r)",
  "collection_duration": "$(date +%s)"
}
EOF

    # 打包报告
    TARBALL="/tmp/sosreport-${HOSTNAME}-${TIMESTAMP}.tar.gz"
    log "正在打包报告..."
    tar czf "$TARBALL" -C /tmp "sosreport-${TIMESTAMP}"

    echo ""
    echo "═══════════════════════════════════════════════════════════"
    success "数据收集完成！"
    echo ""
    echo "  📁 原始数据: $REPORT_DIR"
    echo "  📦 压缩包: $TARBALL"
    echo "  📊 大小: $(du -h "$TARBALL" | cut -f1)"
    echo ""
    echo "下一步："
    echo "  1. 使用 Claude Code 分析数据: 分析 $TARBALL"
    echo "  2. 解压查看: tar xzf $TARBALL"
    echo "  3. 清理: rm -rf $REPORT_DIR $TARBALL"
    echo "═══════════════════════════════════════════════════════════"
}

# 执行
main "$@"
