#!/bin/bash
#
# SOS Report Analyzer - 智能分析引擎
# 自动分析收集的数据并生成诊断报告
#

set -euo pipefail

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
NC='\033[0m'

REPORT_DIR="$1"
ISSUES=()
WARNINGS=()
INFO_ITEMS=()

# 问题严重性级别
CRITICAL=0
HIGH=0
MEDIUM=0
LOW=0

# 添加问题
add_issue() {
    local severity="$1"
    local category="$2"
    local message="$3"
    local recommendation="$4"

    ISSUES+=("${severity}|${category}|${message}|${recommendation}")

    case "$severity" in
        CRITICAL) ((CRITICAL++)) ;;
        HIGH) ((HIGH++)) ;;
        MEDIUM) ((MEDIUM++)) ;;
        LOW) ((LOW++)) ;;
    esac
}

# 分析磁盘空间
analyze_disk_space() {
    local df_file="${REPORT_DIR}/storage/df.txt"
    [ ! -f "$df_file" ] && return

    while read -r line; do
        if [[ $line =~ ^/dev ]]; then
            local usage=$(echo "$line" | awk '{print $5}' | tr -d '%')
            local mount=$(echo "$line" | awk '{print $6}')
            local avail=$(echo "$line" | awk '{print $4}')

            if [ "$usage" -ge 95 ]; then
                add_issue "CRITICAL" "storage" \
                    "磁盘空间严重不足: $mount 使用率 ${usage}% (剩余 ${avail})" \
                    "1. 清理日志: journalctl --vacuum-time=7d\n2. 删除旧内核包\n3. 扩展文件系统或添加新磁盘"
            elif [ "$usage" -ge 85 ]; then
                add_issue "HIGH" "storage" \
                    "磁盘空间不足: $mount 使用率 ${usage}% (剩余 ${avail})" \
                    "计划清理或扩容，避免空间耗尽"
            elif [ "$usage" -ge 75 ]; then
                add_issue "MEDIUM" "storage" \
                    "磁盘空间偏高: $mount 使用率 ${usage}%" \
                    "监控增长趋势，提前规划扩容"
            fi
        fi
    done < "$df_file"
}

# 分析 inode 使用率
analyze_inodes() {
    local df_inode="${REPORT_DIR}/storage/df-inodes.txt"
    [ ! -f "$df_inode" ] && return

    while read -r line; do
        if [[ $line =~ ^/dev ]]; then
            local usage=$(echo "$line" | awk '{print $5}' | tr -d '%')
            local mount=$(echo "$line" | awk '{print $6}')

            if [ "$usage" -ge 90 ]; then
                add_issue "HIGH" "storage" \
                    "Inode 使用率过高: $mount 达到 ${usage}%" \
                    "查找小文件目录: find $mount -xdev -type f | cut -d/ -f2 | sort | uniq -c | sort -rn"
            fi
        fi
    done < "$df_inode"
}

# 分析内存使用
analyze_memory() {
    local meminfo="${REPORT_DIR}/memory/meminfo.txt"
    [ ! -f "$meminfo" ] && return

    local total=$(grep "MemTotal:" "$meminfo" | awk '{print $2}')
    local available=$(grep "MemAvailable:" "$meminfo" | awk '{print $2}')
    local swap_total=$(grep "SwapTotal:" "$meminfo" | awk '{print $2}')
    local swap_free=$(grep "SwapFree:" "$meminfo" | awk '{print $2}')

    if [ "$total" -gt 0 ]; then
        local avail_percent=$((available * 100 / total))

        if [ "$avail_percent" -lt 10 ]; then
            local swap_used=$((swap_total - swap_free))
            if [ "$swap_used" -gt $((swap_total / 2)) ]; then
                add_issue "CRITICAL" "memory" \
                    "内存严重不足，可用内存仅 ${avail_percent}%，且 swap 使用过半" \
                    "1. 识别内存泄漏进程: ps aux --sort=-%mem | head -20\n2. 重启高内存进程\n3. 考虑增加物理内存"
            else
                add_issue "HIGH" "memory" \
                    "内存使用率高，可用内存仅 ${avail_percent}%" \
                    "监控内存趋势，检查是否有内存泄漏"
            fi
        fi
    fi
}

# 分析 CPU iowait
analyze_cpu_iowait() {
    local vmstat="${REPORT_DIR}/memory/vmstat.txt"
    [ ! -f "$vmstat" ] && return

    # 跳过前两行标题，计算平均 iowait
    local avg_iowait=$(tail -n +4 "$vmstat" | awk '{sum+=$16; count++} END {if(count>0) print int(sum/count); else print 0}')

    if [ "$avg_iowait" -ge 30 ]; then
        add_issue "HIGH" "performance" \
            "CPU iowait 过高 (${avg_iowait}%)，存在磁盘 I/O 瓶颈" \
            "1. 检查 iostat 输出定位慢速磁盘\n2. 使用 iotop 识别 I/O 密集型进程\n3. 考虑使用 SSD 或优化文件系统"
    elif [ "$avg_iowait" -ge 15 ]; then
        add_issue "MEDIUM" "performance" \
            "CPU iowait 偏高 (${avg_iowait}%)" \
            "监控磁盘 I/O 性能"
    fi
}

# 分析失败的服务
analyze_failed_services() {
    local failed="${REPORT_DIR}/services/systemctl-failed.txt"
    [ ! -f "$failed" ] && return

    local failed_count=$(grep -c "●" "$failed" 2>/dev/null || echo 0)

    if [ "$failed_count" -gt 0 ]; then
        local failed_list=$(grep "●" "$failed" | awk '{print $2}' | tr '\n' ', ' | sed 's/,$//')
        add_issue "MEDIUM" "services" \
            "检测到 ${failed_count} 个失败的服务: $failed_list" \
            "使用 journalctl -u <service> 查看失败原因并修复"
    fi
}

# 分析僵尸进程
analyze_zombie_processes() {
    local ps_forest="${REPORT_DIR}/services/ps-forest.txt"
    [ ! -f "$ps_forest" ] && return

    local zombie_count=$(grep -c "<defunct>" "$ps_forest" 2>/dev/null || echo 0)

    if [ "$zombie_count" -gt 10 ]; then
        add_issue "MEDIUM" "processes" \
            "检测到 ${zombie_count} 个僵尸进程" \
            "识别父进程: ps -eo pid,ppid,stat,cmd | grep Z\n考虑重启父进程"
    elif [ "$zombie_count" -gt 0 ]; then
        add_issue "LOW" "processes" \
            "检测到 ${zombie_count} 个僵尸进程" \
            "通常无害，但如果持续增长需要调查"
    fi
}

# 分析 SELinux 拒绝
analyze_selinux() {
    local sestatus="${REPORT_DIR}/security/selinux-status.txt"
    [ ! -f "$sestatus" ] && return

    local status=$(cat "$sestatus" 2>/dev/null)

    if [[ "$status" == "Disabled" ]]; then
        add_issue "HIGH" "security" \
            "SELinux 已禁用，系统安全性降低" \
            "编辑 /etc/selinux/config 设置 SELINUX=enforcing (需要重启)"
    elif [[ "$status" == "Permissive" ]]; then
        add_issue "MEDIUM" "security" \
            "SELinux 处于 Permissive 模式，仅记录但不强制执行策略" \
            "分析日志后切换到 Enforcing: setenforce 1"
    fi

    # 检查 AVC 拒绝
    local denials="${REPORT_DIR}/security/selinux-denials.txt"
    if [ -f "$denials" ]; then
        local denial_count=$(grep -c "type=AVC" "$denials" 2>/dev/null || echo 0)
        if [ "$denial_count" -gt 0 ]; then
            add_issue "MEDIUM" "security" \
                "发现 ${denial_count} 个 SELinux AVC 拒绝事件" \
                "分析日志: ausearch -m AVC -ts recent\n生成策略: audit2allow -a"
        fi
    fi
}

# 分析网络丢包
analyze_network_drops() {
    local ip_stats="${REPORT_DIR}/network/ip-link-stats.txt"
    [ ! -f "$ip_stats" ] && return

    while read -r line; do
        if [[ $line =~ RX:.*errors ]]; then
            local rx_errors=$(echo "$line" | grep -oP 'errors\s+\K\d+')
            local rx_dropped=$(echo "$line" | grep -oP 'dropped\s+\K\d+')

            read -r line  # 读取下一行 TX
            local tx_errors=$(echo "$line" | grep -oP 'errors\s+\K\d+')
            local tx_dropped=$(echo "$line" | grep -oP 'dropped\s+\K\d+')

            local total_errors=$((rx_errors + tx_errors))
            local total_dropped=$((rx_dropped + tx_dropped))

            if [ "$total_errors" -gt 1000 ] || [ "$total_dropped" -gt 1000 ]; then
                add_issue "MEDIUM" "network" \
                    "网络接口存在丢包/错误: RX errors=$rx_errors dropped=$rx_dropped, TX errors=$tx_errors dropped=$tx_dropped" \
                    "1. 检查网卡驱动\n2. 增加 ring buffer: ethtool -G eth0 rx 4096\n3. 检查网线和交换机"
            fi
        fi
    done < "$ip_stats"
}

# 分析 SSH 配置安全性
analyze_ssh_security() {
    local sshd_config="${REPORT_DIR}/security/sshd_config"
    [ ! -f "$sshd_config" ] && return

    if grep -q "^PermitRootLogin yes" "$sshd_config"; then
        add_issue "MEDIUM" "security" \
            "SSH 允许 root 直接登录" \
            "修改 /etc/ssh/sshd_config: PermitRootLogin no\n重启: systemctl restart sshd"
    fi

    if grep -q "^PasswordAuthentication yes" "$sshd_config"; then
        add_issue "LOW" "security" \
            "SSH 允许密码认证，建议仅使用密钥认证" \
            "设置 PasswordAuthentication no (确保已配置密钥)"
    fi
}

# 生成终端摘要报告
generate_console_summary() {
    echo ""
    echo "═══════════════════════════════════════════════════════════"
    echo -e "  ${CYAN}系统诊断报告 - SOS Report Analyzer v1.0${NC}"
    echo "═══════════════════════════════════════════════════════════"
    echo ""

    # 读取元数据
    if [ -f "${REPORT_DIR}/metadata.json" ]; then
        local hostname=$(grep '"hostname"' "${REPORT_DIR}/metadata.json" | cut -d'"' -f4)
        local timestamp=$(grep '"timestamp"' "${REPORT_DIR}/metadata.json" | cut -d'"' -f4)
        echo -e "${BLUE}📌 系统信息${NC}"
        echo "   主机名: $hostname"
        echo "   时间: $timestamp"
    fi

    # 读取 OS 信息
    if [ -f "${REPORT_DIR}/system/os-release.txt" ]; then
        local os_name=$(grep "^PRETTY_NAME=" "${REPORT_DIR}/system/os-release.txt" | cut -d'"' -f2)
        echo "   操作系统: $os_name"
    fi

    # 读取内核信息
    if [ -f "${REPORT_DIR}/system/uname.txt" ]; then
        local kernel=$(awk '{print $3}' "${REPORT_DIR}/system/uname.txt")
        echo "   内核: $kernel"
    fi

    # 读取运行时间
    if [ -f "${REPORT_DIR}/system/uptime.txt" ]; then
        local uptime=$(cat "${REPORT_DIR}/system/uptime.txt")
        echo "   运行时间: $uptime"
    fi

    echo ""

    # 问题统计
    local total_issues=${#ISSUES[@]}
    if [ "$total_issues" -gt 0 ]; then
        echo -e "${RED}⚠️  检测到 ${total_issues} 个问题${NC}"
        echo ""

        # 按严重性显示问题
        for severity in CRITICAL HIGH MEDIUM LOW; do
            local severity_count=0
            local color="$NC"

            case "$severity" in
                CRITICAL) severity_count=$CRITICAL; color="$RED" ;;
                HIGH) severity_count=$HIGH; color="$YELLOW" ;;
                MEDIUM) severity_count=$MEDIUM; color="$BLUE" ;;
                LOW) severity_count=$LOW; color="$CYAN" ;;
            esac

            if [ "$severity_count" -gt 0 ]; then
                for issue in "${ISSUES[@]}"; do
                    IFS='|' read -r sev cat msg rec <<< "$issue"
                    if [ "$sev" == "$severity" ]; then
                        echo -e "${color}[$sev] $msg${NC}"
                        echo -e "   ${MAGENTA}→ 建议:${NC}"
                        echo "$rec" | while IFS= read -r line; do
                            echo "     $line"
                        done
                        echo ""
                    fi
                done
            fi
        done
    else
        echo -e "${GREEN}✅ 未发现严重问题${NC}"
        echo ""
    fi

    # 健康检查摘要
    echo -e "${GREEN}✅ 健康检查${NC}"

    # 简单的检查
    [ -f "${REPORT_DIR}/storage/df.txt" ] && echo "   [✓] 存储检查已完成"
    [ -f "${REPORT_DIR}/memory/meminfo.txt" ] && echo "   [✓] 内存检查已完成"
    [ -f "${REPORT_DIR}/network/ip-addr.txt" ] && echo "   [✓] 网络检查已完成"
    [ -f "${REPORT_DIR}/services/systemctl-list-units.txt" ] && echo "   [✓] 服务检查已完成"

    echo ""
    echo "═══════════════════════════════════════════════════════════"
}

# 生成 Markdown 报告
generate_markdown_report() {
    local md_file="${REPORT_DIR}/analysis_report.md"

    cat > "$md_file" <<EOF
# 系统诊断分析报告

**生成时间:** $(date '+%Y-%m-%d %H:%M:%S')
**报告版本:** sosreport-analyzer v1.0

---

## 📊 执行摘要

本次诊断发现 **${#ISSUES[@]} 个问题**：
- 🔴 严重 (CRITICAL): $CRITICAL
- 🟡 高 (HIGH): $HIGH
- 🔵 中 (MEDIUM): $MEDIUM
- 🟢 低 (LOW): $LOW

---

## 🖥️ 系统信息

EOF

    # 添加系统信息
    if [ -f "${REPORT_DIR}/system/os-release.txt" ]; then
        echo "### 操作系统" >> "$md_file"
        echo '```' >> "$md_file"
        cat "${REPORT_DIR}/system/os-release.txt" >> "$md_file"
        echo '```' >> "$md_file"
        echo "" >> "$md_file"
    fi

    # 添加硬件信息
    if [ -f "${REPORT_DIR}/system/lscpu.txt" ]; then
        echo "### CPU 信息" >> "$md_file"
        echo '```' >> "$md_file"
        head -20 "${REPORT_DIR}/system/lscpu.txt" >> "$md_file"
        echo '```' >> "$md_file"
        echo "" >> "$md_file"
    fi

    # 添加问题详情
    if [ "${#ISSUES[@]}" -gt 0 ]; then
        cat >> "$md_file" <<EOF
---

## ⚠️ 发现的问题

EOF
        for issue in "${ISSUES[@]}"; do
            IFS='|' read -r sev cat msg rec <<< "$issue"

            local emoji=""
            case "$sev" in
                CRITICAL) emoji="🔴" ;;
                HIGH) emoji="🟡" ;;
                MEDIUM) emoji="🔵" ;;
                LOW) emoji="🟢" ;;
            esac

            cat >> "$md_file" <<EOF
### $emoji [$sev] $msg

**类别:** $cat

**建议措施:**
$rec

---

EOF
        done
    fi

    echo "Markdown 报告已生成: $md_file"
}

# 主函数
main() {
    if [ -z "$REPORT_DIR" ] || [ ! -d "$REPORT_DIR" ]; then
        echo "用法: $0 <report_directory>"
        exit 1
    fi

    echo "正在分析系统数据: $REPORT_DIR"
    echo ""

    # 执行所有分析
    analyze_disk_space
    analyze_inodes
    analyze_memory
    analyze_cpu_iowait
    analyze_failed_services
    analyze_zombie_processes
    analyze_selinux
    analyze_network_drops
    analyze_ssh_security

    # 生成报告
    generate_console_summary
    generate_markdown_report

    echo "分析完成！"
}

main "$@"
