#!/bin/bash
#
# SOS Report Analyzer - 依赖检查工具
# 检查系统中可用的诊断工具并提供安装建议
#

set -euo pipefail

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

echo "═══════════════════════════════════════════════════════════"
echo "  SOS Report Analyzer - 依赖检查"
echo "═══════════════════════════════════════════════════════════"
echo ""

# 检测发行版
if [ -f /etc/os-release ]; then
    . /etc/os-release
    DISTRO="$ID"
else
    DISTRO="unknown"
fi

echo "发行版: $DISTRO"
echo ""

# 核心工具（必需）
echo "核心工具检查（必需）:"
CORE_TOOLS=(
    "systemctl:systemd:systemd"
    "journalctl:systemd:systemd"
    "ip:iproute2:iproute2"
    "ss:iproute2:iproute2"
    "df:coreutils:coreutils"
    "lsblk:util-linux:util-linux"
    "ps:procps:procps-ng"
    "top:procps:procps-ng"
)

CORE_MISSING=()

for tool in "${CORE_TOOLS[@]}"; do
    IFS=':' read -r cmd pkg_deb pkg_rhel <<< "$tool"
    if command -v "$cmd" &>/dev/null; then
        echo -e "  ${GREEN}[✓]${NC} $cmd"
    else
        echo -e "  ${RED}[✗]${NC} $cmd (缺失)"
        CORE_MISSING+=("$cmd:$pkg_deb:$pkg_rhel")
    fi
done

echo ""

# 增强工具（推荐）
echo "增强工具检查（推荐）:"
ENHANCED_TOOLS=(
    "iostat:sysstat:sysstat"
    "mpstat:sysstat:sysstat"
    "iotop:iotop:iotop"
    "lscpu:util-linux:util-linux"
    "lsmem:util-linux:util-linux"
    "dmidecode:dmidecode:dmidecode"
    "smartctl:smartmontools:smartmontools"
    "lspci:pciutils:pciutils"
    "lsusb:usbutils:usbutils"
    "ethtool:ethtool:ethtool"
)

ENHANCED_MISSING=()

for tool in "${ENHANCED_TOOLS[@]}"; do
    IFS=':' read -r cmd pkg_deb pkg_rhel <<< "$tool"
    if command -v "$cmd" &>/dev/null; then
        echo -e "  ${GREEN}[✓]${NC} $cmd"
    else
        echo -e "  ${YELLOW}[!]${NC} $cmd (推荐安装)"
        ENHANCED_MISSING+=("$cmd:$pkg_deb:$pkg_rhel")
    fi
done

echo ""

# 可选工具（高级功能）
echo "可选工具检查（高级功能）:"
OPTIONAL_TOOLS=(
    "docker:docker.io:docker-ce"
    "podman:podman:podman"
    "nmcli:network-manager:NetworkManager"
    "firewall-cmd:firewalld:firewalld"
    "auditctl:auditd:audit"
    "aureport:auditd:audit"
    "getenforce:libselinux-utils:libselinux-utils"
    "aa-status:apparmor-utils:N/A"
)

for tool in "${OPTIONAL_TOOLS[@]}"; do
    IFS=':' read -r cmd pkg_deb pkg_rhel <<< "$tool"
    if command -v "$cmd" &>/dev/null; then
        echo -e "  ${GREEN}[✓]${NC} $cmd"
    else
        echo -e "  ${YELLOW}[!]${NC} $cmd (可选)"
    fi
done

echo ""
echo "═══════════════════════════════════════════════════════════"

# 提供安装建议
if [ ${#CORE_MISSING[@]} -gt 0 ]; then
    echo ""
    echo -e "${RED}警告: 缺少核心工具，部分功能将无法使用${NC}"
    echo ""
fi

if [ ${#ENHANCED_MISSING[@]} -gt 0 ] || [ ${#CORE_MISSING[@]} -gt 0 ]; then
    echo ""
    echo "安装建议:"
    echo ""

    # 提取包名
    PKGS_DEB=()
    PKGS_RHEL=()

    for item in "${CORE_MISSING[@]}" "${ENHANCED_MISSING[@]}"; do
        IFS=':' read -r cmd pkg_deb pkg_rhel <<< "$item"
        [[ ! " ${PKGS_DEB[@]} " =~ " ${pkg_deb} " ]] && PKGS_DEB+=("$pkg_deb")
        [[ ! " ${PKGS_RHEL[@]} " =~ " ${pkg_rhel} " ]] && PKGS_RHEL+=("$pkg_rhel")
    done

    case "$DISTRO" in
        debian|ubuntu)
            echo "Debian/Ubuntu 系统:"
            echo "  sudo apt update"
            echo "  sudo apt install ${PKGS_DEB[*]}"
            ;;
        rhel|centos|fedora|rocky|alma)
            echo "RHEL/CentOS/Fedora 系统:"
            echo "  sudo dnf install ${PKGS_RHEL[*]}"
            ;;
        *)
            echo "Debian/Ubuntu 系统:"
            echo "  sudo apt install ${PKGS_DEB[*]}"
            echo ""
            echo "RHEL/CentOS/Fedora 系统:"
            echo "  sudo dnf install ${PKGS_RHEL[*]}"
            ;;
    esac
    echo ""
fi

# 权限检查
echo "═══════════════════════════════════════════════════════════"
echo "权限检查:"
if [ "$EUID" -eq 0 ]; then
    echo -e "  ${GREEN}[✓]${NC} 当前以 root 用户运行，可收集完整信息"
else
    echo -e "  ${YELLOW}[!]${NC} 当前以普通用户运行"
    echo "      部分功能受限（如 dmidecode, SMART 状态, 审计日志）"
    echo "      建议: 使用 sudo 运行以获取完整诊断信息"
fi

echo ""
echo "准备就绪！使用以下命令开始收集:"
echo "  bash ~/.claude/skills/sosreport-analyzer/scripts/collect_all.sh"
echo "═══════════════════════════════════════════════════════════"
