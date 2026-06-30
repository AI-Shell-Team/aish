#!/bin/bash
#
# diagnose_system_lag - 依赖检查脚本
# 检查系统是否具备运行诊断所需的工具
#

set -euo pipefail

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo "═══════════════════════════════════════════════════════════"
echo "  系统卡顿诊断工具 - 依赖检查"
echo "═══════════════════════════════════════════════════════════"
echo ""

# 检查命令是否存在
check_command() {
    local cmd="$1"
    local required="$2"
    local desc="$3"

    if command -v "$cmd" &>/dev/null; then
        echo -e "${GREEN}[✓]${NC} $cmd - $desc"
        return 0
    else
        if [ "$required" = "required" ]; then
            echo -e "${RED}[✗]${NC} $cmd - $desc (必需)"
            return 1
        else
            echo -e "${YELLOW}[!]${NC} $cmd - $desc (可选，缺少将跳过部分功能)"
            return 0
        fi
    fi
}

MISSING_REQUIRED=0

echo "核心工具（必需）："
check_command "uptime" "required" "系统负载信息" || ((MISSING_REQUIRED++))
check_command "nproc" "required" "CPU 核心数" || ((MISSING_REQUIRED++))
check_command "top" "required" "进程和 CPU 信息" || ((MISSING_REQUIRED++))
check_command "free" "required" "内存和 Swap 信息" || ((MISSING_REQUIRED++))
check_command "df" "required" "磁盘使用情况" || ((MISSING_REQUIRED++))
check_command "ps" "required" "进程列表" || ((MISSING_REQUIRED++))
check_command "bash" "required" "Bash shell" || ((MISSING_REQUIRED++))
check_command "awk" "required" "文本处理" || ((MISSING_REQUIRED++))
check_command "grep" "required" "文本搜索" || ((MISSING_REQUIRED++))
check_command "sed" "required" "文本处理" || ((MISSING_REQUIRED++))

echo ""
echo "增强工具（推荐）："
check_command "bc" "optional" "浮点数计算"
check_command "iostat" "optional" "I/O 统计（sysstat 包）"
check_command "iotop" "optional" "I/O 进程监控"
check_command "htop" "optional" "增强版 top"
check_command "lsof" "optional" "文件和网络连接"

echo ""
echo "═══════════════════════════════════════════════════════════"

if [ $MISSING_REQUIRED -eq 0 ]; then
    echo -e "${GREEN}✅ 所有必需工具已安装，可以正常运行诊断${NC}"
    echo ""
    echo "运行诊断："
    echo "  bash ~/.claude/skills/diagnose_system_lag/scripts/diagnose.sh"
    exit 0
else
    echo -e "${RED}✗ 缺少 $MISSING_REQUIRED 个必需工具${NC}"
    echo ""
    echo "安装缺失的工具："
    echo ""

    # 检测发行版并给出安装建议
    if [ -f /etc/os-release ]; then
        . /etc/os-release
        case "$ID" in
            debian|ubuntu|deepin|uos)
                echo "Debian/Ubuntu/Deepin/UOS:"
                echo "  sudo apt update"
                echo "  sudo apt install coreutils procps util-linux bc sysstat iotop"
                ;;
            rhel|centos|fedora|rocky|alma)
                echo "RHEL/CentOS/Fedora:"
                echo "  sudo dnf install coreutils procps-ng util-linux bc sysstat iotop"
                ;;
            *)
                echo "未知发行版，请手动安装缺失的工具"
                ;;
        esac
    fi

    exit 1
fi
