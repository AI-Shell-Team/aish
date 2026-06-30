# 系统卡顿诊断报告模板

═══════════════════════════════════════════════════════════
  系统卡顿诊断报告
  生成时间: {{TIMESTAMP}}
  主机名: {{HOSTNAME}}
═══════════════════════════════════════════════════════════

## 📋 诊断结论

{{CONCLUSION}}

## 📊 系统资源概览

- Load Average: {{LOAD_1}}, {{LOAD_5}}, {{LOAD_15}} (CPU 核心数: {{CPU_CORES}})
- CPU 使用: {{CPU_USER}}% user, {{CPU_SYS}}% system, {{CPU_IDLE}}% idle, {{CPU_IOWAIT}}% iowait
- 内存: {{MEM_TOTAL}} 总 / {{MEM_USED}} 已用 / {{MEM_AVAILABLE}} 可用
- Swap: {{SWAP_TOTAL}} 总 / {{SWAP_USED}} 已用 / {{SWAP_FREE}} 可用{{SWAP_WARNING}}
{{DISK_INFO}}

## 🔍 内存占用 TOP 进程

{{TOP_PROCESSES}}

## 💡 优化建议

{{RECOMMENDATIONS}}

═══════════════════════════════════════════════════════════
报告文件: {{REPORT_FILE}}
日志文件: {{LOG_FILE}}
═══════════════════════════════════════════════════════════
