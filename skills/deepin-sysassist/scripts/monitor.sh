#!/bin/bash
#
# Deepin SysAssist - 系统监控工具
# 实时监控 CPU、内存、磁盘、网络、进程状态
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

# 全局变量
DAEMON_MODE=false
INTERVAL=5
LOG_FILE="/var/log/deepin-sysassist-monitor.log"
ALERT_LOG="/var/log/deepin-sysassist-alerts.log"

# 告警阈值
CPU_WARNING=80
CPU_CRITICAL=90
MEM_WARNING=80
MEM_CRITICAL=90
DISK_WARNING=85
DISK_CRITICAL=95
SWAP_WARNING=50

# 解析参数
while [[ $# -gt 0 ]]; do
    case $1 in
        --daemon) DAEMON_MODE=true ;;
        --interval) INTERVAL="$2"; shift ;;
        *) ;;
    esac
    shift
done

# 日志函数
log_info() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [INFO] $*" | tee -a "$LOG_FILE"
}

log_alert() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] [ALERT] $*" | tee -a "$ALERT_LOG"
}

# 清屏函数
clear_screen() {
    if [ "$DAEMON_MODE" = false ]; then
        clear
    fi
}

# CPU 监控
monitor_cpu() {
    local cpu_usage=$(top -bn1 | grep "Cpu(s)" | awk '{print $2}' | cut -d'%' -f1)
    local load_avg=$(uptime | awk -F'load average:' '{print $2}' | xargs)

    echo -e "${CYAN}━━━ CPU 状态 ━━━${NC}"
    echo "  使用率: ${cpu_usage}%"
    echo "  负载: $load_avg"

    # 告警检查
    local cpu_int=$(echo "$cpu_usage" | cut -d'.' -f1)
    if [ "$cpu_int" -ge "$CPU_CRITICAL" ]; then
        log_alert "CPU 使用率严重: ${cpu_usage}%"
        echo -e "  ${RED}[CRITICAL] CPU 使用率过高！${NC}"
    elif [ "$cpu_int" -ge "$CPU_WARNING" ]; then
        log_alert "CPU 使用率警告: ${cpu_usage}%"
        echo -e "  ${YELLOW}[WARNING] CPU 使用率偏高${NC}"
    else
        echo -e "  ${GREEN}[OK] CPU 正常${NC}"
    fi

    # TOP 5 CPU 进程
    echo ""
    echo "  TOP 5 CPU 进程:"
    ps aux --sort=-%cpu | head -6 | tail -5 | awk '{printf "    %-10s %5s%%  %s\n", $11, $3, $2}'
}

# 内存监控
monitor_memory() {
    local mem_total=$(free -m | awk 'NR==2{print $2}')
    local mem_used=$(free -m | awk 'NR==2{print $3}')
    local mem_free=$(free -m | awk 'NR==2{print $4}')
    local mem_percent=$((mem_used * 100 / mem_total))

    local swap_total=$(free -m | awk 'NR==3{print $2}')
    local swap_used=$(free -m | awk 'NR==3{print $3}')
    local swap_percent=0
    [ "$swap_total" -gt 0 ] && swap_percent=$((swap_used * 100 / swap_total))

    echo -e "${CYAN}━━━ 内存状态 ━━━${NC}"
    echo "  物理内存: ${mem_used}MB / ${mem_total}MB (${mem_percent}%)"
    echo "  Swap:     ${swap_used}MB / ${swap_total}MB (${swap_percent}%)"

    # 告警检查
    if [ "$mem_percent" -ge "$MEM_CRITICAL" ]; then
        log_alert "内存使用率严重: ${mem_percent}%"
        echo -e "  ${RED}[CRITICAL] 内存不足！${NC}"
    elif [ "$mem_percent" -ge "$MEM_WARNING" ]; then
        log_alert "内存使用率警告: ${mem_percent}%"
        echo -e "  ${YELLOW}[WARNING] 内存使用偏高${NC}"
    else
        echo -e "  ${GREEN}[OK] 内存正常${NC}"
    fi

    if [ "$swap_used" -gt 0 ] && [ "$swap_percent" -ge "$SWAP_WARNING" ]; then
        log_alert "Swap 使用率高: ${swap_percent}%"
        echo -e "  ${YELLOW}[WARNING] Swap 使用率高${NC}"
    fi

    # TOP 5 内存进程
    echo ""
    echo "  TOP 5 内存进程:"
    ps aux --sort=-%mem | head -6 | tail -5 | awk '{printf "    %-10s %5s%%  %s\n", $11, $4, $2}'
}

# 磁盘监控
monitor_disk() {
    echo -e "${CYAN}━━━ 磁盘状态 ━━━${NC}"

    while IFS= read -r line; do
        if [[ $line =~ ^/dev ]]; then
            local device=$(echo "$line" | awk '{print $1}')
            local size=$(echo "$line" | awk '{print $2}')
            local used=$(echo "$line" | awk '{print $3}')
            local avail=$(echo "$line" | awk '{print $4}')
            local percent=$(echo "$line" | awk '{print $5}' | tr -d '%')
            local mount=$(echo "$line" | awk '{print $6}')

            printf "  %-20s %5s / %-5s (%3s%%) %s\n" "$device" "$used" "$size" "$percent" "$mount"

            # 告警检查
            if [ "$percent" -ge "$DISK_CRITICAL" ]; then
                log_alert "磁盘空间严重不足: $mount ${percent}%"
                echo -e "    ${RED}[CRITICAL] 磁盘空间不足！${NC}"
            elif [ "$percent" -ge "$DISK_WARNING" ]; then
                log_alert "磁盘空间警告: $mount ${percent}%"
                echo -e "    ${YELLOW}[WARNING] 磁盘空间偏高${NC}"
            fi
        fi
    done < <(df -h | grep -E '^/dev')
}

# 网络监控
monitor_network() {
    echo -e "${CYAN}━━━ 网络状态 ━━━${NC}"

    # 网络接口状态
    local interfaces=$(ip -br link | grep -v "lo" | awk '{print $1, $2}')
    echo "  网络接口:"
    while IFS= read -r line; do
        local iface=$(echo "$line" | awk '{print $1}')
        local status=$(echo "$line" | awk '{print $2}')
        if [[ "$status" == "UP" ]]; then
            echo -e "    $iface: ${GREEN}UP${NC}"
        else
            echo -e "    $iface: ${YELLOW}DOWN${NC}"
        fi
    done <<< "$interfaces"

    # 连接统计
    local tcp_conns=$(ss -tan | grep ESTAB | wc -l)
    local listening=$(ss -tln | wc -l)

    echo ""
    echo "  TCP 连接: $tcp_conns 活动连接"
    echo "  监听端口: $listening"
}

# 进程监控
monitor_processes() {
    echo -e "${CYAN}━━━ 进程状态 ━━━${NC}"

    local total_proc=$(ps aux | wc -l)
    local zombie_proc=$(ps aux | awk '{print $8}' | grep -c Z || true)
    local running_proc=$(ps aux | awk '{print $8}' | grep -c R || true)

    echo "  总进程数: $total_proc"
    echo "  运行中:   $running_proc"

    if [ "$zombie_proc" -gt 0 ]; then
        echo -e "  僵尸进程: ${YELLOW}$zombie_proc${NC}"
        log_alert "检测到 $zombie_proc 个僵尸进程"
    else
        echo -e "  僵尸进程: ${GREEN}0${NC}"
    fi
}

# 系统信息
show_system_info() {
    local hostname=$(hostname)
    local uptime_info=$(uptime -p)
    local kernel=$(uname -r)

    echo -e "${MAGENTA}╔════════════════════════════════════════════════════════════╗${NC}"
    echo -e "${MAGENTA}║${NC}  ${BLUE}Deepin SysAssist - 系统监控${NC}                            ${MAGENTA}║${NC}"
    echo -e "${MAGENTA}╠════════════════════════════════════════════════════════════╣${NC}"
    echo -e "${MAGENTA}║${NC}  主机: $hostname"
    echo -e "${MAGENTA}║${NC}  运行时间: $uptime_info"
    echo -e "${MAGENTA}║${NC}  内核: $kernel"
    echo -e "${MAGENTA}║${NC}  刷新间隔: ${INTERVAL}秒"
    echo -e "${MAGENTA}╚════════════════════════════════════════════════════════════╝${NC}"
    echo ""
}

# 主监控循环
main_loop() {
    while true; do
        clear_screen
        show_system_info

        monitor_cpu
        echo ""

        monitor_memory
        echo ""

        monitor_disk
        echo ""

        monitor_network
        echo ""

        monitor_processes

        echo ""
        echo -e "${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
        echo -e "更新时间: $(date '+%Y-%m-%d %H:%M:%S')"
        echo -e "按 Ctrl+C 退出"

        sleep "$INTERVAL"
    done
}

# 主函数
main() {
    echo "启动 Deepin SysAssist 系统监控..."
    log_info "监控服务启动"

    if [ "$DAEMON_MODE" = true ]; then
        log_info "以守护进程模式运行"
        echo "以守护进程模式运行，日志: $LOG_FILE"
    fi

    # 捕获退出信号
    trap 'log_info "监控服务停止"; exit 0' INT TERM

    main_loop
}

main "$@"
