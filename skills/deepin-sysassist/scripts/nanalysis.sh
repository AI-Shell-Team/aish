#!/bin/bash
# Deepin SysAssist - 网络日志分析工具
echo "━━━ Deepin SysAssist - 网络日志分析 ━━━"
echo ""
echo "📅 分析时间: $(date '+%Y-%m-%d %H:%M:%S')"
echo "📊 正在分析网络日志..."
echo ""

# 当前连接统计
echo "[分析] 网络连接统计"
tcp_estab=$(ss -tan | grep ESTAB | wc -l)
tcp_tw=$(ss -tan | grep TIME_WAIT | wc -l)
tcp_listen=$(ss -tln | wc -l)
echo "  ESTABLISHED: $tcp_estab"
echo "  TIME_WAIT:   $tcp_tw"
echo "  LISTENING:   $tcp_listen"
echo ""

# TOP 连接 IP
echo "[分析] TOP 10 连接源 IP"
ss -tan | awk 'NR>1{print $5}' | cut -d: -f1 | sort | uniq -c | sort -rn | head -10 | \
  awk '{printf "  %3d 次  %s\n", $1, $2}'
echo ""

# TOP 监听端口
echo "[分析] 监听端口"
ss -tln | awk 'NR>1{print $4}' | cut -d: -f2 | sort -u | head -10 | \
  awk '{printf "  端口 %s\n", $1}'
echo ""

# SSH 登录尝试（如果有日志）
echo "[分析] SSH 登录尝试"
if [ -f /var/log/auth.log ]; then
    # `wc -l` always emits a number, so no fallback needed.
    failed_ssh=$(grep "Failed password" /var/log/auth.log 2>/dev/null | wc -l)
    echo "  失败登录尝试: $failed_ssh 次"
elif command -v journalctl &>/dev/null; then
    # `grep -c` already prints "0" on no match; use `|| true` only to swallow its
    # non-zero exit under `set -e` — never `|| echo 0`, which would append a second 0.
    failed_ssh=$(journalctl -u sshd --since "24 hours ago" --no-pager 2>/dev/null | grep -c "Failed password" || true)
    failed_ssh=${failed_ssh:-0}
    echo "  失败登录尝试（24h）: $failed_ssh 次"
else
    echo "  无法获取 SSH 日志"
fi
echo ""

echo "━━━ 分析完成 ━━━"
