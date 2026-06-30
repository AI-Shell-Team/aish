#!/bin/bash
#
# Deepin SysAssist - 基线配置管理工具
# 备份、对比和管理系统配置基线
#

set -euo pipefail

# 颜色定义
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

# 全局变量
BASELINE_DIR="$HOME/.deepin-sysassist/baselines"
mkdir -p "$BASELINE_DIR"

ACTION="${1:---help}"
BASELINE_NAME="${2:-default}"

# 配置文件列表
CONFIG_FILES=(
    "/etc/ssh/sshd_config"
    "/etc/sysctl.conf"
    "/etc/fstab"
    "/etc/hosts"
    "/etc/resolv.conf"
    "/etc/security/limits.conf"
    "/etc/default/grub"
)

# 创建基线
create_baseline() {
    local name="$1"
    local timestamp=$(date +%Y%m%d-%H%M%S)
    local baseline_path="$BASELINE_DIR/${name}-${timestamp}"

    mkdir -p "$baseline_path"

    echo -e "${CYAN}━━━ 创建配置基线: $name ━━━${NC}"
    echo "保存路径: $baseline_path"
    echo ""

    # 备份配置文件
    local count=0
    for config in "${CONFIG_FILES[@]}"; do
        if [ -f "$config" ]; then
            echo -e "  ${GREEN}[✓]${NC} 备份 $config"
            cp "$config" "$baseline_path/"
            ((count++))
        else
            echo -e "  ${YELLOW}[!]${NC} 跳过 $config (文件不存在)"
        fi
    done

    # 保存元数据
    cat > "$baseline_path/metadata.txt" <<EOF
基线名称: $name
创建时间: $(date '+%Y-%m-%d %H:%M:%S')
主机名: $(hostname)
内核版本: $(uname -r)
操作系统: $(cat /etc/os-release | grep "PRETTY_NAME=" | cut -d'"' -f2)
备份文件数: $count
EOF

    # 保存系统快照
    systemctl list-units --type=service --state=running > "$baseline_path/running_services.txt"
    ps aux > "$baseline_path/processes.txt"
    df -h > "$baseline_path/disk_usage.txt"

    echo ""
    echo -e "${GREEN}✓ 基线配置创建成功${NC}"
    echo "  备份文件数: $count"
    echo "  保存位置: $baseline_path"
}

# 列出所有基线
list_baselines() {
    echo -e "${CYAN}━━━ 已保存的配置基线 ━━━${NC}"
    echo ""

    if [ ! -d "$BASELINE_DIR" ] || [ -z "$(ls -A "$BASELINE_DIR" 2>/dev/null)" ]; then
        echo "  没有找到任何基线配置"
        echo "  使用 --create 创建第一个基线"
        return
    fi

    local count=0
    for baseline in "$BASELINE_DIR"/*; do
        if [ -d "$baseline" ]; then
            ((count++))
            local name=$(basename "$baseline")
            local time=$(stat -c %y "$baseline" | cut -d'.' -f1)
            local size=$(du -sh "$baseline" | cut -f1)

            echo -e "  ${BLUE}[$count]${NC} $name"
            echo "      时间: $time"
            echo "      大小: $size"

            if [ -f "$baseline/metadata.txt" ]; then
                local files=$(grep "备份文件数:" "$baseline/metadata.txt" | cut -d: -f2 | xargs)
                echo "      文件: $files 个配置文件"
            fi
            echo ""
        fi
    done

    if [ "$count" -eq 0 ]; then
        echo "  没有找到任何基线配置"
    else
        echo "  共 $count 个基线配置"
    fi
}

# 对比基线
compare_baseline() {
    echo -e "${CYAN}━━━ 配置对比分析 ━━━${NC}"
    echo ""

    # 获取最新基线
    local latest=$(ls -t "$BASELINE_DIR" 2>/dev/null | head -1)

    if [ -z "$latest" ]; then
        echo -e "${YELLOW}错误: 未找到基线配置${NC}"
        echo "请先使用 --create 创建基线"
        return 1
    fi

    local baseline_path="$BASELINE_DIR/$latest"
    echo "对比基线: $latest"

    if [ -f "$baseline_path/metadata.txt" ]; then
        echo "创建时间: $(grep "创建时间:" "$baseline_path/metadata.txt" | cut -d: -f2- | xargs)"
    fi
    echo ""

    local changed=0
    local unchanged=0

    # 对比每个配置文件
    for config in "${CONFIG_FILES[@]}"; do
        local filename=$(basename "$config")
        local baseline_file="$baseline_path/$filename"

        if [ ! -f "$baseline_file" ]; then
            continue
        fi

        if [ ! -f "$config" ]; then
            echo -e "${YELLOW}[!]${NC} $config"
            echo "    当前文件不存在（基线中存在）"
            ((changed++))
            echo ""
            continue
        fi

        # 对比文件
        if diff -q "$baseline_file" "$config" >/dev/null 2>&1; then
            echo -e "${GREEN}[✓]${NC} $config"
            echo "    无变化"
            ((unchanged++))
        else
            echo -e "${YELLOW}[⚠]${NC} $config"
            echo "    检测到变更:"
            echo ""
            diff -u "$baseline_file" "$config" | head -20 | sed 's/^/    /'
            ((changed++))
        fi
        echo ""
    done

    # 总结
    echo -e "${BLUE}━━━ 对比总结 ━━━${NC}"
    echo "  未变更: $unchanged 个文件"
    echo "  已变更: $changed 个文件"

    if [ "$changed" -gt 0 ]; then
        echo ""
        echo -e "${YELLOW}建议:${NC}"
        echo "  1. 检查变更是否符合预期"
        echo "  2. 如需恢复，可使用 --restore 命令"
        echo "  3. 如变更正常，可创建新基线 --create"
    fi
}

# 恢复基线（示例功能）
restore_baseline() {
    echo -e "${YELLOW}[警告] 恢复功能需要 root 权限${NC}"
    echo "此功能将从基线恢复配置文件，请谨慎操作"
    echo ""
    echo "实际恢复操作需要手动执行，防止误操作"

    local latest=$(ls -t "$BASELINE_DIR" 2>/dev/null | head -1)
    if [ -z "$latest" ]; then
        echo "错误: 未找到基线配置"
        return 1
    fi

    echo "最新基线: $latest"
    echo "基线位置: $BASELINE_DIR/$latest"
    echo ""
    echo "恢复命令示例:"
    echo "  sudo cp $BASELINE_DIR/$latest/sshd_config /etc/ssh/sshd_config"
    echo "  sudo systemctl restart sshd"
}

# 主函数
main() {
    case "$ACTION" in
        --create)
            create_baseline "$BASELINE_NAME"
            ;;

        --list)
            list_baselines
            ;;

        --compare)
            compare_baseline
            ;;

        --restore)
            restore_baseline
            ;;

        --help|*)
            echo -e "${BLUE}Deepin SysAssist - 基线配置管理${NC}"
            echo ""
            echo "用法: baselineconfig [选项] [名称]"
            echo ""
            echo "选项:"
            echo "  --create [名称]   创建新的配置基线"
            echo "  --list            列出所有基线"
            echo "  --compare         对比当前配置与最新基线"
            echo "  --restore         显示恢复命令（需要手动执行）"
            echo "  --help            显示此帮助信息"
            echo ""
            echo "示例:"
            echo "  baselineconfig --create production-v1"
            echo "  baselineconfig --list"
            echo "  baselineconfig --compare"
            ;;
    esac
}

main "$@"
