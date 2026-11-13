# aish 全量远程 C/S 功能实测

目标：用已编译的 `target/x86_64-unknown-linux-musl/release/aish` 对 `test-130 (root@10.10.17.130)` 做端到端实测，输出通过/失败矩阵。

## 隔离策略
- 本地状态测试用 `HOME=/tmp/aish-e2e-<ts>`，不污染真实 `~/.config/aish`
- 远端策略测试前 `ssh` 备份 `/root/.config/aish/security_policy.yaml`（或对应路径），测完还原
- 只操作自建的一次性会话，不动已有会话 `2de7952b`
- 全程不使用 `session attach`（全屏交互）

## 测试矩阵
| # | 模块 | 用例 |
|---|---|---|
| 1 | host | add（含 --group）、list、group: 选择器、remove、未知别名报错 |
| 2 | doctor | 正常路径 4 项检查；错误 remote-bin 失败路径 |
| 3 | policy | push（正常，版本自增）、push --diff、非法 YAML、targets 不匹配、show、status（单/多 host + --json）、漂移检测（stale/drifted/none） |
| 4 | audit | pull 到独立 --db（首次全量、二次去重 0）、cursor 文件生成与推进 |
| 5 | diag | run 五项检查、--json、list、status(--json)、retry、group 选择器、不可达主机超时分类 |
| 6 | session | 自建远端会话 → list、rename、replay --tail、kill，验证清理干净 |
| 7 | admin | keygen、show-key、show-identity、add-signer/list-signers、trust-operator/list-operators/untrust-operator（隔离 HOME） |

## 输出
- 逐项 PASS/FAIL 表 + 用时
- 记录缺陷（已发现：diag results 未落盘、policy status 强制 --host）

## 清理
- 删除一次性远端会话、还原远端策略、恢复真实本地状态验证 `policy status` 回到 `current`
