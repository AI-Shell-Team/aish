# Changelog

All notable changes to this skill will be documented in this file.

## [2.0.0] - 2026-01-06

### 重大重构
- 从单文件提示词模式重构为标准 Claude Code skill 结构
- 添加完整的目录结构（config/, logs/, reports/, scripts/, templates/）

### 新增功能
- ✅ 独立可执行的诊断脚本 (`scripts/diagnose.sh`)
- ✅ 自动生成诊断报告到 `reports/` 目录
- ✅ 记录运行日志到 `logs/` 目录
- ✅ 配置文件支持 (`config/default.conf`)
- ✅ 快速启动脚本 (`scripts/quickstart.sh`)
- ✅ 依赖检查脚本 (`scripts/check_dependencies.sh`)
- ✅ 报告模板 (`templates/report_template.md`)

### 改进
- ✅ 修复进程解析 bug（处理包含特殊字符的命令行）
- ✅ 优化 CPU/内存/Swap/磁盘分析逻辑
- ✅ 改进错误处理和日志记录
- ✅ 支持命令行独立运行
- ✅ 可被其他 skill 调用复用

### 兼容性
- ✅ 完全兼容 Claude Code skills 规范
- ✅ 与 sosreport-analyzer、deepin-sysassist 等 skill 结构一致
- ✅ 支持 Deepin, Debian, Ubuntu, UOS 等发行版

## [1.0.0] - 2026-01-06 (初始版本)

### 功能
- 基础系统卡顿诊断功能
- 提示词驱动模式
- CPU/内存/Swap/磁盘分析
- 进程识别和分类

### 限制
- 仅有单个 SKILL.md 文件
- 不符合标准 skill 目录结构
- 无法被其他 skill 复用
