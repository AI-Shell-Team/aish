#!/bin/bash
# Deepin SysAssist - 进程日志分析工具
echo "━━━ Deepin SysAssist - 进程日志分析 ━━━"
echo ""
echo "📅 分析时间范围: 最近 24 小时"
echo "📊 正在分析进程日志..."
echo ""

# OOM Killer 事件
echo "[分析] OOM Killer 事件"
# `grep -c` already prints "0" on no match; use `|| true` only to swallow its
# non-zero exit under `set -e` — never `|| echo 0`, which would append a second 0.
oom_count=$(journalctl --since "24 hours ago" --no-pager 2>/dev/null | grep -c "Out of memory" || true)
oom_count=${oom_count:-0}
if [ "$oom_count" -gt 0 ]; then
    echo "  发现 $oom_count 个 OOM 事件"
    journalctl --since "24 hours ago" --no-pager 2>/dev/null | grep "Out of memory" | tail -3
else
    echo "  ✓ 未发现 OOM 事件"
fi
echo ""

# 进程崩溃
echo "[分析] 进程崩溃事件"
segfault_count=$(journalctl --since "24 hours ago" --no-pager 2>/dev/null | grep -c "segfault" || true)
segfault_count=${segfault_count:-0}
if [ "$segfault_count" -gt 0 ]; then
    echo "  发现 $segfault_count 个 Segmentation Fault"
    journalctl --since "24 hours ago" --no-pager 2>/dev/null | grep "segfault" | tail -3
else
    echo "  ✓ 未发现崩溃事件"
fi
echo ""

# TOP CPU 进程
echo "[分析] CPU 占用 TOP 10"
ps aux --sort=-%cpu | head -11 | awk 'NR>1{printf "  %-20s %5s%%  PID:%s\n", $11, $3, $2}'
echo ""

# TOP 内存进程
echo "[分析] 内存占用 TOP 10"
ps aux --sort=-%mem | head -11 | awk 'NR>1{printf "  %-20s %5s%%  PID:%s\n", $11, $4, $2}'
echo ""

echo "━━━ 分析完成 ━━━"
