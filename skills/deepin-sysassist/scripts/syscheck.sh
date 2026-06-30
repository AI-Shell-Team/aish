#!/bin/bash
#
# Deepin SysAssist - 系统体检工具
# 全面检查系统硬件、软件、配置、安全状态
#

set -uo pipefail

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
MAGENTA='\033[0;35m'
NC='\033[0m'

# 全局变量
REPORT_DIR="/tmp/syscheck-$(date +%Y%m%d-%H%M%S)"
QUICK_MODE=false
CHECK_HARDWARE=true
CHECK_SOFTWARE=true
CHECK_SECURITY=true
CHECK_PERFORMANCE=true

# 统计变量
TOTAL_CHECKS=0
PASSED_CHECKS=0
WARNING_CHECKS=0
FAILED_CHECKS=0

mkdir -p "$REPORT_DIR"

# 解析参数
while [[ $# -gt 0 ]]; do
    case $1 in
        --quick) QUICK_MODE=true; CHECK_PERFORMANCE=false ;;
        --hardware) CHECK_SOFTWARE=false; CHECK_SECURITY=false; CHECK_PERFORMANCE=false ;;
        --security) CHECK_HARDWARE=false; CHECK_SOFTWARE=false; CHECK_PERFORMANCE=false ;;
        *) ;;
    esac
    shift 2>/dev/null || true
done

# 检查结果记录
check_result() {
    local category="$1"
    local item="$2"
    local status="$3"  # PASS, WARN, FAIL
    local message="$4"

    ((TOTAL_CHECKS++))

    case "$status" in
        PASS)
            ((PASSED_CHECKS++))
            echo -e "  ${GREEN}[✓]${NC} $item"
            ;;
        WARN)
            ((WARNING_CHECKS++))
            echo -e "  ${YELLOW}[⚠]${NC} $item"
            echo -e "      → $message"
            ;;
        FAIL)
            ((FAILED_CHECKS++))
            echo -e "  ${RED}[✗]${NC} $item"
            echo -e "      → $message"
            ;;
    esac

    echo "$category|$item|$status|$message" >> "$REPORT_DIR/check_results.csv"
}

# 硬件检查
check_hardware() {
    if [ "$CHECK_HARDWARE" = false ]; then return; fi

    echo -e "${CYAN}━━━ 硬件检查 ━━━${NC}"

    # CPU 检查
    local cpu_count=$(nproc)
    check_result "硬件" "CPU 核心数: $cpu_count" "PASS" ""

    # 内存检查
    local mem_total=$(free -g | awk 'NR==2{print $2}')
    if [ "$mem_total" -lt 2 ]; then
        check_result "硬件" "内存容量: ${mem_total}GB" "WARN" "内存容量较小，建议至少 4GB"
    else
        check_result "硬件" "内存容量: ${mem_total}GB" "PASS" ""
    fi

    # 磁盘检查
    local root_usage=$(df -h / | awk 'NR==2{print $5}' | tr -d '%')
    if [ "$root_usage" -ge 95 ]; then
        check_result "硬件" "根分区使用率: ${root_usage}%" "FAIL" "磁盘空间严重不足"
    elif [ "$root_usage" -ge 85 ]; then
        check_result "硬件" "根分区使用率: ${root_usage}%" "WARN" "磁盘空间偏高，建议清理"
    else
        check_result "硬件" "根分区使用率: ${root_usage}%" "PASS" ""
    fi

    # SMART 状态（需要 root）
    if command -v smartctl &>/dev/null && [ "$EUID" -eq 0 ]; then
        for disk in /dev/sd? /dev/nvme?n?; do
            if [ -b "$disk" ]; then
                if smartctl -H "$disk" | grep -q "PASSED"; then
                    check_result "硬件" "磁盘健康 $(basename $disk)" "PASS" ""
                else
                    check_result "硬件" "磁盘健康 $(basename $disk)" "WARN" "SMART 状态异常"
                fi
            fi
        done
    fi

    echo ""
}

# 软件检查
check_software() {
    if [ "$CHECK_SOFTWARE" = false ]; then return; fi

    echo -e "${CYAN}━━━ 软件检查 ━━━${NC}"

    # 系统版本
    if [ -f /etc/os-release ]; then
        local os_name=$(grep "^PRETTY_NAME=" /etc/os-release | cut -d'"' -f2)
        check_result "软件" "操作系统: $os_name" "PASS" ""
    fi

    # 关键服务检查
    local services=("sshd" "ssh" "NetworkManager" "systemd-networkd")
    for service in "${services[@]}"; do
        if systemctl list-units --type=service --all | grep -q "$service"; then
            if systemctl is-active --quiet "$service"; then
                check_result "软件" "服务 $service" "PASS" ""
            else
                check_result "软件" "服务 $service" "WARN" "服务未运行"
            fi
        fi
    done

    # 失败的服务
    local failed_count=$(systemctl --failed --no-legend | wc -l)
    if [ "$failed_count" -gt 0 ]; then
        local failed_list=$(systemctl --failed --no-legend | awk '{print $1}' | tr '\n' ',' | sed 's/,$//')
        check_result "软件" "失败的服务: $failed_count 个" "WARN" "$failed_list"
    else
        check_result "软件" "失败的服务检查" "PASS" ""
    fi

    # 僵尸进程
    local zombie_count=$(ps aux | awk '{print $8}' | grep -c Z || true)
    if [ "$zombie_count" -gt 10 ]; then
        check_result "软件" "僵尸进程: $zombie_count 个" "WARN" "僵尸进程过多"
    elif [ "$zombie_count" -gt 0 ]; then
        check_result "软件" "僵尸进程: $zombie_count 个" "PASS" "少量僵尸进程正常"
    else
        check_result "软件" "僵尸进程检查" "PASS" ""
    fi

    echo ""
}

# 安全检查
check_security() {
    if [ "$CHECK_SECURITY" = false ]; then return; fi

    echo -e "${CYAN}━━━ 安全检查 ━━━${NC}"

    # SSH 配置检查
    if [ -f /etc/ssh/sshd_config ]; then
        if grep -q "^PermitRootLogin yes" /etc/ssh/sshd_config; then
            check_result "安全" "SSH Root 登录" "WARN" "允许 root 直接登录存在风险"
        else
            check_result "安全" "SSH Root 登录" "PASS" ""
        fi

        if grep -q "^PasswordAuthentication yes" /etc/ssh/sshd_config; then
            check_result "安全" "SSH 密码认证" "WARN" "建议使用密钥认证"
        else
            check_result "安全" "SSH 密码认证" "PASS" ""
        fi
    fi

    # SELinux/AppArmor 检查
    if command -v getenforce &>/dev/null; then
        local selinux_status=$(getenforce 2>/dev/null || echo "Unknown")
        if [ "$selinux_status" = "Enforcing" ]; then
            check_result "安全" "SELinux 状态" "PASS" ""
        elif [ "$selinux_status" = "Permissive" ]; then
            check_result "安全" "SELinux 状态" "WARN" "处于 Permissive 模式"
        elif [ "$selinux_status" = "Disabled" ]; then
            check_result "安全" "SELinux 状态" "WARN" "SELinux 已禁用"
        fi
    elif command -v aa-status &>/dev/null; then
        if aa-status --enabled 2>/dev/null; then
            check_result "安全" "AppArmor 状态" "PASS" ""
        else
            check_result "安全" "AppArmor 状态" "WARN" "AppArmor 未启用"
        fi
    fi

    # 防火墙检查
    local firewall_active=false
    if command -v ufw &>/dev/null; then
        if ufw status | grep -q "Status: active"; then
            check_result "安全" "防火墙状态 (ufw)" "PASS" ""
            firewall_active=true
        fi
    fi
    if command -v firewall-cmd &>/dev/null; then
        if firewall-cmd --state 2>/dev/null | grep -q "running"; then
            check_result "安全" "防火墙状态 (firewalld)" "PASS" ""
            firewall_active=true
        fi
    fi
    if [ "$firewall_active" = false ]; then
        check_result "安全" "防火墙状态" "WARN" "防火墙未启用"
    fi

    # SUID 文件检查（需要 root）
    if [ "$EUID" -eq 0 ]; then
        local suid_count=$(find / -perm -4000 -type f 2>/dev/null | wc -l)
        check_result "安全" "SUID 文件数量: $suid_count" "PASS" ""
    fi

    echo ""
}

# 性能检查
check_performance() {
    if [ "$CHECK_PERFORMANCE" = false ]; then return; fi

    echo -e "${CYAN}━━━ 性能检查 ━━━${NC}"

    # CPU 负载
    local load_1min=$(uptime | awk -F'load average:' '{print $2}' | awk -F',' '{print $1}' | xargs)
    local cpu_count=$(nproc)
    local load_percent=$(echo "scale=0; $load_1min * 100 / $cpu_count" | bc 2>/dev/null || echo 0)

    if [ "$load_percent" -ge 90 ]; then
        check_result "性能" "系统负载: $load_1min" "WARN" "负载过高（${load_percent}%）"
    else
        check_result "性能" "系统负载: $load_1min" "PASS" "负载正常（${load_percent}%）"
    fi

    # 内存使用率
    local mem_percent=$(free | awk 'NR==2{printf "%.0f", $3*100/$2}')
    if [ "$mem_percent" -ge 90 ]; then
        check_result "性能" "内存使用率: ${mem_percent}%" "WARN" "内存使用率高"
    else
        check_result "性能" "内存使用率: ${mem_percent}%" "PASS" ""
    fi

    # Swap 使用
    local swap_used=$(free -m | awk 'NR==3{print $3}')
    local swap_total=$(free -m | awk 'NR==3{print $2}')
    if [ "$swap_total" -gt 0 ] && [ "$swap_used" -gt $((swap_total / 2)) ]; then
        check_result "性能" "Swap 使用: ${swap_used}MB / ${swap_total}MB" "WARN" "Swap 使用率高"
    else
        check_result "性能" "Swap 使用" "PASS" ""
    fi

    echo ""
}

# 生成摘要报告
generate_summary() {
    local health_score=$((PASSED_CHECKS * 100 / TOTAL_CHECKS))

    echo ""
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${CYAN}📊 体检结果摘要${NC}"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    echo "  总检查项: $TOTAL_CHECKS"
    echo -e "  ${GREEN}通过: $PASSED_CHECKS (✓)${NC}"
    echo -e "  ${YELLOW}警告: $WARNING_CHECKS (⚠)${NC}"
    echo -e "  ${RED}失败: $FAILED_CHECKS (✗)${NC}"
    echo ""

    if [ "$health_score" -ge 90 ]; then
        echo -e "  健康度评分: ${GREEN}$health_score/100${NC} (优秀)"
    elif [ "$health_score" -ge 75 ]; then
        echo -e "  健康度评分: ${YELLOW}$health_score/100${NC} (良好)"
    elif [ "$health_score" -ge 60 ]; then
        echo -e "  健康度评分: ${YELLOW}$health_score/100${NC} (一般)"
    else
        echo -e "  健康度评分: ${RED}$health_score/100${NC} (需改进)"
    fi

    echo ""
    echo "  详细报告: $REPORT_DIR/check_results.csv"
    echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

# 主函数
main() {
    echo -e "${MAGENTA}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${MAGENTA}║${NC}  ${BLUE}Deepin SysAssist - 系统体检${NC}                            ${MAGENTA}║${NC}"
    echo -e "${MAGENTA}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
    echo "开始时间: $(date '+%Y-%m-%d %H:%M:%S')"
    echo ""

    # CSV 头部
    echo "类别|检查项|状态|说明" > "$REPORT_DIR/check_results.csv"

    # 执行检查
    check_hardware
    check_software
    check_security
    check_performance

    # 生成摘要
    generate_summary

    echo ""
    echo "体检完成！"
}

main "$@"
