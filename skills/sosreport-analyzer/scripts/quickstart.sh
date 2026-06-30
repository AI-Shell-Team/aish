#!/bin/bash
#
# SOS Report Analyzer - 快速启动脚本
# 一键执行完整的数据收集和分析流程
#

set -euo pipefail

SKILL_DIR="$HOME/.claude/skills/sosreport-analyzer"
SCRIPT_DIR="${SKILL_DIR}/scripts"

# 颜色定义
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo "═══════════════════════════════════════════════════════════"
echo "  SOS Report Analyzer - 快速启动"
echo "═══════════════════════════════════════════════════════════"
echo ""

# 检查是否为 root
if [ "$EUID" -ne 0 ]; then
    echo -e "${YELLOW}提示: 当前以普通用户运行，部分功能受限${NC}"
    echo "      建议使用 sudo 运行以获取完整诊断信息"
    echo ""
    read -p "是否继续？(y/N): " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "已取消"
        exit 0
    fi
    echo ""
fi

# 步骤1: 检查依赖
echo -e "${BLUE}步骤 1/3: 检查系统依赖${NC}"
echo ""
bash "${SCRIPT_DIR}/check_dependencies.sh"
echo ""

read -p "按 Enter 键继续数据收集..."
echo ""

# 步骤2: 收集数据
echo -e "${BLUE}步骤 2/3: 收集系统数据${NC}"
echo ""
bash "${SCRIPT_DIR}/collect_all.sh"
echo ""

# 获取最新的报告目录
LATEST_REPORT=$(ls -dt /tmp/sosreport-2* 2>/dev/null | head -1)

if [ -z "$LATEST_REPORT" ]; then
    echo "错误: 未找到收集的数据"
    exit 1
fi

read -p "按 Enter 键继续分析..."
echo ""

# 步骤3: 分析数据
echo -e "${BLUE}步骤 3/3: 分析系统数据${NC}"
echo ""
bash "${SCRIPT_DIR}/analyze.sh" "$LATEST_REPORT"
echo ""

# 完成
echo ""
echo "═══════════════════════════════════════════════════════════"
echo -e "${GREEN}✅ 诊断完成！${NC}"
echo ""
echo "📁 数据位置:"
echo "   原始数据: $LATEST_REPORT"
echo "   压缩包: $(ls -t /tmp/sosreport-*.tar.gz 2>/dev/null | head -1)"
echo "   分析报告: ${LATEST_REPORT}/analysis_report.md"
echo ""
echo "📖 查看报告:"
echo "   cat ${LATEST_REPORT}/analysis_report.md"
echo ""
echo "🧹 清理数据:"
echo "   rm -rf $LATEST_REPORT"
echo "   rm -f /tmp/sosreport-*.tar.gz"
echo "═══════════════════════════════════════════════════════════"
