#!/bin/bash
#
# diagnose_system_lag - 快速启动脚本
# 一键执行完整的诊断流程并展示结果
#

set -euo pipefail

SKILL_DIR="$HOME/.claude/skills/diagnose_system_lag"
SCRIPT_DIR="${SKILL_DIR}/scripts"

# 颜色定义
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo "═══════════════════════════════════════════════════════════"
echo "  系统卡顿诊断工具 - 快速启动"
echo "═══════════════════════════════════════════════════════════"
echo ""

# 执行诊断
echo -e "${BLUE}正在执行系统诊断...${NC}"
echo ""
bash "${SCRIPT_DIR}/diagnose.sh"
echo ""

# 完成
echo ""
echo "═══════════════════════════════════════════════════════════"
echo -e "${GREEN}✅ 诊断完成！${NC}"
echo ""
echo "📁 报告位置:"
LATEST_REPORT=$(ls -t "${SKILL_DIR}/reports"/diagnose-*.txt 2>/dev/null | head -1)
if [ -n "$LATEST_REPORT" ]; then
    echo "   $LATEST_REPORT"
    echo ""
    echo "📖 查看报告:"
    echo "   cat $LATEST_REPORT"
else
    echo "   未找到报告文件"
fi
echo ""
echo "🧹 清理旧数据:"
echo "   find $SKILL_DIR/reports/ -name 'diagnose-*.txt' -mtime +30 -delete"
echo "   find $SKILL_DIR/logs/ -name 'diagnose-*.log' -mtime +7 -delete"
echo "═══════════════════════════════════════════════════════════"
