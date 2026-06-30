#!/bin/bash
#
# diagnose_system_lag - 系统卡顿诊断工具
# 检测 CPU/内存/Swap/磁盘瓶颈，分析原因并给出安全的进程关闭建议
#
# 版本: 2.0.0
# 兼容: Debian, Deepin, Ubuntu, UOS
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
SKILL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${SKILL_DIR}/reports"
LOG_DIR="${SKILL_DIR}/logs"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
REPORT_FILE="${REPORT_DIR}/diagnose-${TIMESTAMP}.txt"
LOG_FILE="${LOG_DIR}/diagnose-${TIMESTAMP}.log"

# 创建必要的目录
mkdir -p "${REPORT_DIR}" "${LOG_DIR}"

# 日志函数
log() {
    echo -e "${BLUE}[INFO]${NC} $*" | tee -a "${LOG_FILE}"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $*" | tee -a "${LOG_FILE}"
}

error() {
    echo -e "${RED}[ERROR]${NC} $*" | tee -a "${LOG_FILE}"
}

success() {
    echo -e "${GREEN}[✓]${NC} $*" | tee -a "${LOG_FILE}"
}

# ============================================
# 数据收集模块
# ============================================

collect_system_metrics() {
    log "收集系统指标..."

    # 系统运行时间和负载
    UPTIME_OUTPUT=$(uptime)

    # CPU 核心数
    CPU_CORES=$(nproc)

    # CPU 和内存概览
    TOP_OUTPUT=$(top -b -n1 | head -n 20)

    # 内存和 Swap 详情
    FREE_OUTPUT=$(free -h)

    # 磁盘使用情况
    DF_OUTPUT=$(df -h)

    # 按内存占用排序的进程
    PS_MEM_OUTPUT=$(ps aux --sort=-%mem | head -n 26)

    # 按 CPU 占用排序的进程
    PS_CPU_OUTPUT=$(ps aux --sort=-%cpu | head -n 16)

    success "数据收集完成"
}

# ============================================
# 分析模块
# ============================================

analyze_cpu() {
    log "分析 CPU 性能..."

    # 提取 load average
    LOAD_1=$(echo "$UPTIME_OUTPUT" | awk '{print $(NF-2)}' | tr -d ',')
    LOAD_5=$(echo "$UPTIME_OUTPUT" | awk '{print $(NF-1)}' | tr -d ',')
    LOAD_15=$(echo "$UPTIME_OUTPUT" | awk '{print $NF}')

    # 提取 CPU 使用率（兼容中英文）
    CPU_USER=$(echo "$TOP_OUTPUT" | grep -i '%cpu' | awk '{for(i=1;i<=NF;i++){if($i~/^[0-9.]+$/&&$(i+1)~/用|us/){print $i; exit}}}')
    CPU_SYS=$(echo "$TOP_OUTPUT" | grep -i '%cpu' | awk '{for(i=1;i<=NF;i++){if($i~/^[0-9.]+$/&&$(i+1)~/系|sy/){print $i; exit}}}')
    CPU_IDLE=$(echo "$TOP_OUTPUT" | grep -i '%cpu' | awk '{for(i=1;i<=NF;i++){if($i~/^[0-9.]+$/&&$(i+1)~/闲|id/){print $i; exit}}}')
    CPU_IOWAIT=$(echo "$TOP_OUTPUT" | grep -i '%cpu' | awk '{for(i=1;i<=NF;i++){if($i~/^[0-9.]+$/&&$(i+1)~/等|wa/){print $i; exit}}}')

    # 判定 CPU 状态
    CPU_THRESHOLD=$(echo "$CPU_CORES * 1.5" | bc)
    CPU_BOTTLENECK=false
    IO_BOTTLENECK=false

    if (( $(echo "$LOAD_1 > $CPU_THRESHOLD" | bc -l) )) && (( $(echo "$CPU_IDLE < 20" | bc -l) )); then
        CPU_BOTTLENECK=true
    fi

    if (( $(echo "$LOAD_1 > $CPU_THRESHOLD" | bc -l) )) && (( $(echo "$CPU_IDLE > 60" | bc -l) )); then
        IO_BOTTLENECK=true
    fi

    if (( $(echo "${CPU_IOWAIT:-0} > 5" | bc -l) )); then
        IO_BOTTLENECK=true
    fi
}

analyze_memory() {
    log "分析内存和 Swap..."

    # 提取内存信息（兼容中英文）
    MEM_TOTAL=$(echo "$FREE_OUTPUT" | grep -i '内存\|mem:' | awk '{print $2}')
    MEM_USED=$(echo "$FREE_OUTPUT" | grep -i '内存\|mem:' | awk '{print $3}')
    MEM_AVAILABLE=$(echo "$FREE_OUTPUT" | grep -i '内存\|mem:' | awk '{print $7}')

    SWAP_TOTAL=$(echo "$FREE_OUTPUT" | grep -i '交换\|swap:' | awk '{print $2}')
    SWAP_USED=$(echo "$FREE_OUTPUT" | grep -i '交换\|swap:' | awk '{print $3}')
    SWAP_FREE=$(echo "$FREE_OUTPUT" | grep -i '交换\|swap:' | awk '{print $4}')

    # 判定内存状态
    MEMORY_CRITICAL=false
    SWAP_CRITICAL=false

    # 检查 Swap 使用情况
    if [[ "$SWAP_TOTAL" != "0" && "$SWAP_TOTAL" != "0B" ]]; then
        # 提取数字和单位
        SWAP_USED_NUM=$(echo "$SWAP_USED" | sed 's/[^0-9.]//g')
        SWAP_USED_UNIT=$(echo "$SWAP_USED" | sed 's/[0-9.]//g')

        if [[ "$SWAP_USED_UNIT" == "Gi" || "$SWAP_USED_UNIT" == "G" ]]; then
            if (( $(echo "$SWAP_USED_NUM >= 2" | bc -l) )); then
                SWAP_CRITICAL=true
            fi
        fi
    fi
}

analyze_disk() {
    log "分析磁盘使用..."

    DISK_CRITICAL=false
    DISK_WARNING_LIST=""

    # 检查关键分区
    while IFS= read -r line; do
        USAGE=$(echo "$line" | awk '{print $5}' | tr -d '%')
        MOUNT=$(echo "$line" | awk '{print $6}')

        if [[ "$MOUNT" == "/" || "$MOUNT" == "/home" ]] && (( USAGE >= 90 )); then
            DISK_CRITICAL=true
            DISK_WARNING_LIST="${DISK_WARNING_LIST}  - $MOUNT: ${USAGE}% ⚠️\n"
        elif (( USAGE >= 95 )); then
            DISK_WARNING_LIST="${DISK_WARNING_LIST}  - $MOUNT: ${USAGE}% ⚠️\n"
        fi
    done < <(echo "$DF_OUTPUT" | tail -n +2 | grep -E '^/dev')
}

analyze_processes() {
    log "分析进程列表..."

    # 识别大户进程
    declare -A PROCESS_GROUPS

    while IFS= read -r line; do
        # 跳过表头
        [[ "$line" =~ ^USER ]] && continue

        # 使用更安全的方法提取字段
        read -r USER PID CPU MEM VSZ RSS TTY STAT START TIME CMD_REST <<< "$line"

        # 跳过无效行
        [[ -z "$PID" || ! "$PID" =~ ^[0-9]+$ ]] && continue

        # 进程分类
        PROC_TYPE="其他"
        IS_SYSTEM=false

        if [[ "$CMD_REST" =~ gemini ]]; then
            PROC_TYPE="Gemini 客户端"
        elif [[ "$CMD_REST" =~ wps|wpp|et ]]; then
            PROC_TYPE="WPS 办公套件"
        elif [[ "$CMD_REST" =~ WXWork|WeMail ]]; then
            PROC_TYPE="企业微信"
        elif [[ "$CMD_REST" =~ warp-terminal ]]; then
            PROC_TYPE="Warp Terminal"
        elif [[ "$CMD_REST" =~ typora ]]; then
            PROC_TYPE="Typora"
        elif [[ "$CMD_REST" =~ chrome|chromium ]]; then
            PROC_TYPE="Chrome 浏览器"
        elif [[ "$CMD_REST" =~ firefox ]]; then
            PROC_TYPE="Firefox 浏览器"
        elif [[ "$CMD_REST" =~ code|cursor ]]; then
            PROC_TYPE="VSCode/Cursor"
        elif [[ "$CMD_REST" =~ clash ]]; then
            PROC_TYPE="Clash 代理"
        elif [[ "$CMD_REST" =~ Xorg|kwin|dde-|systemd|dbus|fcitx|pulseaudio|pipewire|sshd|NetworkManager ]]; then
            IS_SYSTEM=true
            PROC_TYPE="系统服务"
        fi

        # 存储进程信息（暂时不用，简化版本直接用原始输出）
        # if [[ "$IS_SYSTEM" == "false" ]]; then
        #     PROCESS_GROUPS["$PROC_TYPE"]+="PID:$PID MEM:$MEM% RSS:$RSS CMD:$CMD_REST|"
        # fi
    done < <(echo "$PS_MEM_OUTPUT")
}

# ============================================
# 报告生成模块
# ============================================

generate_report() {
    log "生成诊断报告..."

    {
        echo "═══════════════════════════════════════════════════════════"
        echo "  系统卡顿诊断报告"
        echo "  生成时间: $(date '+%Y-%m-%d %H:%M:%S')"
        echo "═══════════════════════════════════════════════════════════"
        echo ""

        # 1. 诊断结论
        echo "## 📋 诊断结论"
        echo ""

        if [[ "$CPU_BOTTLENECK" == "true" ]]; then
            echo "**现在机器卡顿的主要原因是：CPU 负载过高**（负载 $LOAD_1 超过核心数 $CPU_CORES 的 1.5 倍），CPU 空闲率仅 ${CPU_IDLE}%。"
        elif [[ "$IO_BOTTLENECK" == "true" ]]; then
            echo "**现在机器卡顿的主要原因是：磁盘 I/O 瓶颈**（iowait ${CPU_IOWAIT}%，负载高但 CPU 空闲）。"
        elif [[ "$SWAP_CRITICAL" == "true" ]]; then
            echo "**现在机器卡顿的主要原因是：Swap 大量使用**（已用 $SWAP_USED），导致频繁换页，性能严重下降。"
        elif [[ "$DISK_CRITICAL" == "true" ]]; then
            echo "**磁盘空间严重不足**，可能影响系统性能。"
        else
            echo "**您的系统目前运行流畅，没有明显卡顿现象。**"
        fi

        echo ""

        # 2. 系统资源概览
        echo "## 📊 系统资源概览"
        echo ""
        echo "- Load Average: $LOAD_1, $LOAD_5, $LOAD_15 (CPU 核心数: $CPU_CORES)"
        echo "- CPU 使用: ${CPU_USER}% user, ${CPU_SYS}% system, ${CPU_IDLE}% idle, ${CPU_IOWAIT}% iowait"
        echo "- 内存: $MEM_TOTAL 总 / $MEM_USED 已用 / $MEM_AVAILABLE 可用"

        if [[ "$SWAP_CRITICAL" == "true" ]]; then
            echo "- Swap: $SWAP_TOTAL 总 / $SWAP_USED 已用 / $SWAP_FREE 可用 ⚠️"
        else
            echo "- Swap: $SWAP_TOTAL 总 / $SWAP_USED 已用 / $SWAP_FREE 可用"
        fi

        if [[ -n "$DISK_WARNING_LIST" ]]; then
            echo "- 磁盘:"
            echo -e "$DISK_WARNING_LIST"
        fi

        echo ""

        # 3. 进程列表
        echo "## 🔍 内存占用 TOP 进程"
        echo ""
        echo "$PS_MEM_OUTPUT" | head -n 11
        echo ""

        # 4. 优化建议
        echo "## 💡 优化建议"
        echo ""

        if [[ "$SWAP_CRITICAL" == "true" || "$MEMORY_CRITICAL" == "true" ]]; then
            echo "### ✅ 立即操作（释放内存）"
            echo ""
            echo "建议关闭以下大户进程以释放内存："
            echo ""

            # 这里可以根据 PROCESS_GROUPS 生成具体的 kill 命令
            echo "# 请先保存工作内容，然后手动执行以下命令"
            echo "# kill <PID>  # 优雅停止"
            echo "# kill -9 <PID>  # 强制停止（如果优雅停止无效）"
            echo ""
        fi

        if [[ "$DISK_CRITICAL" == "true" ]]; then
            echo "### 🧹 清理磁盘空间"
            echo ""
            echo "sudo apt clean                        # 清理包缓存"
            echo "sudo journalctl --vacuum-time=7d      # 清理旧日志"
            echo "du -sh ~/.cache /tmp                  # 检查缓存"
            echo ""
        fi

        echo "═══════════════════════════════════════════════════════════"

    } | tee "$REPORT_FILE"

    success "报告已保存到: $REPORT_FILE"
}

# ============================================
# 主函数
# ============================================

main() {
    echo ""
    echo -e "${CYAN}═══════════════════════════════════════════════════════════${NC}"
    echo -e "${CYAN}  系统卡顿诊断工具 v2.0${NC}"
    echo -e "${CYAN}═══════════════════════════════════════════════════════════${NC}"
    echo ""

    # 收集数据
    collect_system_metrics

    # 分析数据
    analyze_cpu
    analyze_memory
    analyze_disk
    analyze_processes

    # 生成报告
    generate_report

    echo ""
    success "诊断完成！"
    echo ""
    echo -e "${BLUE}📁 报告位置:${NC} $REPORT_FILE"
    echo -e "${BLUE}📋 日志位置:${NC} $LOG_FILE"
    echo ""
}

# 执行主函数
main "$@"
