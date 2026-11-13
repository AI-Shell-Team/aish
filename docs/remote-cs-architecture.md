# AISH 远程 C/S 架构说明

> 面向使用者和维护者的通俗讲解。读完你就能明白：这套东西是什么、怎么用、
> 为什么这样设计、出问题时去哪查。

---

## 1. 一句话概括

**你在一台机器上用 aish 开的终端会话，可以从另一台机器"接过来"继续用；
策略（安全规则）可以从一台管理机推送到多台机器；跨机器的巡检可以一条
命令跑完并汇总结果。**

这三件事分别对应 issue #586（会话接管）、#587（策略与审计）、#588（批量
诊断），共用同一套通信机制。

---

## 2. 先搞懂三个角色

### 2.1 Daemon（守护进程）—— "会话的房东"

你在服务器上运行 `aish` 时，如果配置里 `pty_daemon_enabled: true`，
aish 会悄悄在后台启动一个 **daemon 进程**。它做的事：

- 持有一个**伪终端（PTY）**，你的 bash 就跑在里面
- 监听一个**本机 Unix socket**（如 `/run/user/0/aish/pty-xxx.sock`）
- 把你的会话元数据写成文件：`~/.local/share/aish/pty-sessions/<uuid>.json`

关键特性：**终端关了，daemon 和 bash 还活着**。这就是"断网/换终端后会话
还在"的原因——daemon 忽略挂断信号（SIGHUP），bash 跑在它的 PTY 里，
跟你的终端窗口没有生命周期绑定。

### 2.2 Client（客户端）—— "接会话的人"

任何想连上这个会话的程序，都是一个 client。本机场景下就是 aish 自己
（你重开终端时它自动 attach）；远程场景下就是另一台机器上的 aish。

Client 和 daemon 之间交流用的是一套**自定义帧协议**（见第 4 节）。

### 2.3 Agent 入口 —— "远端机器上的服务员"

远程机器上没有一个常驻的"服务器进程"。当本地机器想操作远端时，它通过
**ssh 运行一条一次性命令**：

```
ssh user@远端 aish --remote-sessions    # 列出会话，输出 JSON，立刻退出
ssh user@远端 aish --remote-attach <id> # 常驻中继，会话结束才退出
```

这些 `--remote-*` 参数就是 "agent 入口"。它们按需启动、用完即走，
所以远端**不需要预装任何服务、不需要开端口、不需要配防火墙**——
只要远端装了 aish、ssh 能通，一切就能工作。

---

## 3. 全景数据流

```
你的笔记本                          远端服务器 10.10.17.130
┌─────────────────────┐            ┌──────────────────────────────┐
│ aish remote ...     │            │                              │
│   │                 │            │  ┌────────────────────────┐  │
│   │ ssh 子进程       │            │  │ PTY daemon（常驻）      │  │
│   ▼                 │            │  │  ┌──────────────────┐  │  │
│ 帧协议编解码 ◄══════╪══ ssh ═════╪══╪══▶ Unix socket   │  │  │
│                     │            │  │  └───────┬──────────┘  │  │
│                     │            │  │          ▼             │  │
│                     │            │  │   PTY ◀─▶ bash         │  │
│                     │            │  └────────────────────────┘  │
└─────────────────────┘            └──────────────────────────────┘
```

一次 `aish remote session attach` 的完整过程：

1. 本地 aish 起 ssh 子进程：`ssh host aish --remote-attach <session-id>`
2. 远端那个 aish 连接本机 Unix socket，变成"字节搬运工"
3. 本地 aish 发送 `ATTACH` 帧 → daemon 回 `ATTACH_ACK` + 历史输出回放
4. 此后双方进入"实况模式"：你敲键盘 → `INPUT` 帧 → bash 执行 →
   输出 `PTY_OUTPUT` 帧 → 你的屏幕

---

## 4. 帧协议（daemon_protocol.rs）

所有通信都是同一种格式的帧：

```
┌──────────┬────────────┬─────────────────────┐
│ type 1B  │ len 2B(LE) │ payload ≤ 64KB      │
└──────────┴────────────┴─────────────────────┘
```

两类方向、15 种帧：

**client → daemon（你发出的操作）**

| 帧 | 作用 |
|---|---|
| `ATTACH` (0x10) | 我要接入这个会话 |
| `INPUT` (0x11) | 这是我敲的键盘字节 |
| `RESIZE` (0x12) | 我调了窗口大小 |
| `DETACH` (0x13) | 我走了（会话保留） |
| `KILL_SESSION` (0x14) | 杀掉整个会话 |
| `CLAIM_INPUT` (0x15) | 我要夺取输入权 |

**daemon → client（你看到的东西）**

| 帧 | 作用 |
|---|---|
| `PTY_OUTPUT` (0x01) | bash 的实时输出 |
| `SCROLLBACK` (0x03) | 接入时的历史回放（256KB 环形缓冲） |
| `ATTACH_ACK` (0x05) | 接入成功 |
| `EXIT_NOTICE` (0x08) | 会话已退出 |
| `ROLE_NOTIFY` (0x09) | 你的角色变了（控制器/观察者） |
| `TAKEOVER_REQUEST` (0x0A) | 有人想接管，需要你确认 |
| `DAEMON_ERROR` (0x06) | 出错了 |

版本兼容：`ATTACH`/`ATTACH_ACK` 帧都带 `protocol_version`，新客户端连旧
daemon（或反过来）会自动降级，不会崩。

---

## 5. 角色仲裁：谁有权打字（#586 核心）

一个会话同一时刻只有一个 **Controller**（能打字的人），其他人是
**Observer**（只能看）。规则由 daemon 里的 `RoleArbiter` 状态机裁决：

```
新客户端 attach
    │
    ├─ 没有Controller ──▶ 你就是 Controller（保持单机手感）
    │
    └─ 已有Controller ──▶ 你是 Observer（输入帧被 daemon 直接丢弃）
                              │
                              └─ 你发 CLAIM_INPUT → daemon 转发给
                                 Controller → TAKEOVER_REQUEST
                                 ├─ Controller 同意 → 角色互换
                                 └─ 10秒没回应 → 自动拒绝

Controller 断线 ──▶ 角色标记 stale（占位不阻塞）
                    下一个 CLAIM_INPUT 直接成功
```

**为什么 Observer 的输入是"丢弃"而不是"不发送"**：信任判断必须在
daemon 一侧完成——客户端说自己只读不算数。这是结构性保证，不是君子
协定。

**Controller 是谁、状态如何，写进了会话文件**（`controller`、
`controller_stale` 字段），所以 `remote session list` 能直接回答
"现在谁在控制"，不需要连上去问。

每次角色变化还会追加一条记录到 `<session>.role-audit.log`——谁在
什么时候拿到了/交出了控制权，有账可查。

---

## 6. 策略分发与审计（#587）

### 6.1 信封（envelope）

策略不是一个裸 YAML 文件，而是一个**信封**：

```json
{
  "version": 21,              ← 严格递增，防回滚
  "issuer": "ops-admin",      ← 谁发的
  "targets": ["prod-*"],      ← 目标主机匹配模式，防错发
  "policy": "<YAML 内容>",     ← 策略本体
  "signature": "<ed25519>"    ← 管理员签名，防篡改
}
```

### 6.2 推送与校验链

```
管理机                                  目标主机
aish remote policy push
  ├─ 本地先验：YAML 合法？
  ├─ 自动造信封（version 自动 = 本地/远端计数器最大值 + 1）
  ├─ ed25519 签名（有 admin keypair 时）
  ├─ --diff：先展示与远端当前策略的差异，确认后发送
  ▼ ssh stdin（信封从标准输入进入，避免 ssh 参数拼接问题）
                                        ┌─ 校验顺序 ─────────────┐
                                        │ 1. targets 匹配本机？   │
                                        │ 2. version > 已安装？   │
                                        │ 3. 签名可信？           │
                                        │ 4. YAML 能解析？        │
                                        │ 5. 不是误发的信封本体？  │
                                        └───────────┬────────────┘
                                                    ▼
                                     全过 → 原子替换策略文件 + 记版本
                                     任一失败 → 拒绝并说明原因
```

**关键设计：目标端自治**。策略落地为普通文件，之后即使管理机消失，
目标机的策略依然生效、本机 aish 依然正常。不依赖在线。

### 6.3 记住的批准不能"越权存活"

aish 会记住你对某条命令说过 "yes"（批准记忆）。问题：如果新策略禁止了
这条命令，旧的批准还在，等于绕过新策略。

解法：每次工具调用前，比对当前策略文件的 **mtime 指纹**；发现变了就
清空全部批准记忆。代价是每次调用一次 stat（纳秒级），换来"旧批准永远
不越过新策略"。

### 6.4 密钥管理

```bash
# 管理机：生成密钥对（一次性）
aish remote admin keygen
#   → 公钥打印出来，分发到各目标机

# 目标机：登记可信签名者
aish remote admin add-signer <公钥hex>
#   → 写入 ~/.config/aish/policy-signers.json

# 之后：目标机只接受持有对应私钥者签名的信封
```

未配置签名者的主机处于"无签名模式"（向前兼容旧流程）；配置了的主机
强制要求有效签名。

### 6.5 审计汇总

```bash
aish remote audit pull --host prod-1
```

增量拉取：本地为每台主机存一个**游标**（上次同步时间），每次只拉新增
事件，按 `(host, 时间, 类型, 命令)` 幂等去重——重复合并不重复计数。
拉取结果说明明确（"没有新事件"还是"远端没数据"），不让你猜。

---

## 7. 批量诊断（#588）

```bash
aish remote diag run --host prod-1,prod-2,prod-3 --checks svc,disk,load
```

- **检查项是白名单**：`svc/ports/logs/disk/load` 五种，每种对应一条
  固定命令。白名单外的名字直接拒绝——**不存在"执行任意命令"的代码
  路径**，写入/重启/提权被结构性阻止
- **并发 fan-out**：最多 4 台同时查，每台 20 秒超时
- **五种结果状态**：`success` / `unreachable` / `denied` / `timeout` /
  `evidence-insufficient`，汇总时失败节点明确标注"不进入结论"
- **任务持久化**：每次 run 生成任务 ID（如 `dgn-7f3a2c`），落盘到
  `~/.local/share/aish/diag-tasks/`，之后可以：

```bash
aish remote diag list            # 历史任务
aish remote diag status dgn-7f3a2c   # 每台主机的状态
aish remote diag retry dgn-7f3a2c    # 只重跑失败的节点
```

---

## 8. 日常使用速查

```bash
# ── 一次性配置 ─────────────────────────────
aish remote host add prod-1 root@10.10.17.130   # 起个别名
aish remote doctor --host prod-1                 # 自检远端环境

# ── 会话接管 ───────────────────────────────
aish remote session list --host prod-1
aish remote session attach --host prod-1 web-1   # 按名字/前缀/UUID
aish remote session rename --host prod-1 web-1 --to web-2
aish remote session kill   --host prod-1 web-2

# ── 策略 ──────────────────────────────────
aish remote policy push --host prod-1 --policy p.yaml --targets 'prod-*' --diff
aish remote policy show --host prod-1
aish remote audit pull --host prod-1

# ── 诊断 ──────────────────────────────────
aish remote diag run --host prod-1,prod-2 --checks svc,disk,load
aish remote diag status dgn-7f3a2c
aish remote diag retry dgn-7f3a2c
```

`--host` 接受别名（`prod-1`）或完整 ssh 目标（`root@10.10.17.130`），
二选一，全命令统一。

---

## 9. 出问题去哪查

| 症状 | 检查点 |
|---|---|
| `no usable aish binary found` | 远端没装 aish 或不在默认路径 → `AISH_REMOTE_BIN=/path` 或 `remote host add --remote-bin` |
| `unknown remote host alias` | 别名没配 → `remote host list` 查看 / `host add` 添加 |
| attach 报需要交互终端 | 换到真实终端运行；或用 `session list` 只查看 |
| 会话显示 `unreachable` | daemon 挂了或主机不可达；状态未知，不代表会话已死 |
| 策略 REJECTED: version... | 版本必须严格递增；`policy show` 查看远端当前版本 |
| 策略 REJECTED: signature... | 目标机没登记你的公钥，或信封被改过 |
| 诊断显示 timeout | 远端慢或网络抖动；`diag retry` 只重跑失败节点 |

---

## 10. 设计取舍备忘（为什么不做某些事）

| 决策 | 理由 |
|---|---|
| 不做中央服务器/常驻 agent | 三个 issue 都是"可选扩展、首期少量主机、复用 SSH"。ssh 按需连接与 shpool 同构，零部署成本。未来规模上去后可平滑升级为 agent 主动拉取模式（信封格式不变） |
| Observer 输入在 daemon 侧丢弃 | 信任判断必须在服务端；客户端自证只读不可信 |
| 策略落地为文件而非中央内存 | 目标端自治：管理机消失不影响目标机运行 |
| 用 mtime 指纹做批准失效 | 零跨进程通信、纳秒级开销；策略文件只在安装时变化 |
| 帧协议带版本号 | 保证新旧版本 aish 混合部署时不崩 |

---

## 10.5 操作者身份验证（identity，免密之外的另一层）

ssh 免密只解决"能不能连上"，不回答"你是谁"。aish 增加了独立的
**操作者身份层**（与 ssh 凭据解耦）：

### 10.5.1 机制

- 每个 aish 实例自动拥有一个身份密钥对
  `~/.config/aish/identity-key.json`（首次运行静默生成，0600）
- 目标机维护受信操作者公钥表 `~/.config/aish/trusted-operators.json`
- 客户端每次远程调用先发送一行签名挑战：`{action, pubkey, challenge, signature}`，
  签名覆盖 `action|nonce`（绑定操作类型，防重放到其他操作）
- 目标端 agent 入口先验证身份再执行；验证失败输出
  `{"auth_failed": true, "reason": ...}` 并退出

### 10.5.2 开关语义（单机无感）

| 状态 | 行为 |
|---|---|
| `trusted-operators.json` 不存在或为空 | **开放模式**：不读身份行，行为与旧版完全一致 |
| 至少配置了一个受信公钥 | **强制模式**：所有 `--remote-*` 入口要求有效身份 |

配置命令：

```bash
aish remote admin show-identity          # 打印本机操作者公钥
aish remote admin trust-operator <hex>   # 在目标机登记受信操作者（开启强制模式）
aish remote admin list-operators
aish remote admin untrust-operator <hex> # 移除；最后一个被移除时回到开放模式
```

`remote admin` 子命令本身不受身份门管控（`show-identity` 是登记自己的引导路径）。

### 10.5.3 ssh 密码认证（方案 2，sshpass 集成）

ssh 免密**不是**必须的。当目标机只能用密码登录时，aish 可以自带凭据：

```bash
# 方式一：host add 时写入（存 remote-hosts.json，文件 0600）
aish remote host add prod-1 root@10.0.0.1 --password 'uos123!!'

# 方式二：环境变量（不落盘，适合 CI/脚本）
export AISH_REMOTE_PASSWORD='uos123!!'
aish remote session list --host root@10.0.0.1
```

实现：客户端检测到密码时，把 ssh 调用包进 `sshpass -e`（密码经
`SSHPASS` 环境变量传给子进程，不出现在命令行/ps 输出里）；ssh 内部仍
保持 `BatchMode=yes`——密码错误立即失败而不是挂起。环境变量优先于
条目里存的密码。

安全权衡：密码落在 `remote-hosts.json` 明文仅受 0600 保护；生产环境
推荐方式二（环境变量）或回到密钥。aish 的操作者身份层（10.5）独立于
ssh 凭据，无论 ssh 用密码还是密钥都生效。

### 10.5.4 RBAC：策略里的 remote_access 段

身份验证回答"你是谁"，`remote_access` 回答"你能做什么"（写进已安装策略，
随信封签名分发）：

```yaml
remote_access:
  actions:
    session.attach:
      operators: ["3614d369"]     # 公钥 hex 前缀匹配
    policy.install:
      operators: ["d7e3cb33"]
```

规则：无 `remote_access` 段 = 允许全部已认证操作者；action 列出但
`operators` 为空 = 显式拒绝所有；`operators` 前缀匹配操作者公钥。

### 10.5.5 实测记录（130 ⇄ 243）

- 双方互配 trust 后，强制模式下 session list / policy push / diag / replay
  全部通过（挑战-签名自动完成，用户无感）
- 243 移除 130 的信任后，130 的请求被拒绝：
  `{"auth_failed":true,"reason":"operator d7e3… is not trusted on this host"}`
- RBAC：130（d7e3…）推策略到 130 被允许，243 的操作者（5fbe…）推同一策略
  被 `remote_access` 拒绝：
  `operator 5fbe… is not authorized for 'policy.install' by this host's remote_access policy`

---

## 10.6 会话录制与回放

daemon 位于 PTY 与所有客户端之间，天然是录制点。每个会话的字节流按
asciinema v2 兼容格式落盘到 `~/.local/share/aish/pty-sessions/<id>.cast`
（一行一个 JSON：时间戳 + 输出内容；非 UTF-8 字节以 `raw_b64` 无损保留）。
录制始终开启、自动滚动保留最近 20 个会话。

```bash
aish remote session replay --host prod-1 web-1           # 完整回放
aish remote session replay --host prod-1 web-1 --tail 50 # 只看最后 50 行
```

---

## 10.7 主机分组与策略漂移检测

### 分组

```bash
aish remote host add prod-1 root@10.0.0.1 --group prod --group web
aish remote policy push --host group:prod --policy p.yaml   # 组内逐台推送
```

### 漂移检测

```bash
aish remote policy status --host group:prod
HOST                     STATE      EXPECTED
root@10.0.0.1            current    v26
root@10.0.0.2            drifted    v24    ← 远端比本地记录新（别处改过）
root@10.0.0.3            stale      v26    ← 漏推
```

状态：`current`（与本地记录一致）/ `drifted`（远端更新——在别处被改）/
`stale`（远端落后——漏推）/ `none` / `unreachable`。

---

## 11. 代码地图

| 文件 | 行数 | 职责 |
|---|---|---|
| `crates/aish-pty/src/daemon.rs` | ~2000 | 事件循环、RoleArbiter、会话元数据 |
| `crates/aish-pty/src/daemon_protocol.rs` | ~730 | 15 种帧的定义与编解码 |
| `crates/aish-pty/src/backend.rs` | ~590 | 客户端抽象（本机 socket / ssh 两种后端） |
| `crates/aish-cli/src/remote_cmd.rs` | ~1500 | 全部子命令实现 + agent 入口 |
| `crates/aish-cli/src/attach_remote.rs` | ~200 | 远程 attach 的本地终端循环 |
| `crates/aish-cli/src/diagnose.rs` | ~350 | 诊断 fan-out 与聚合 |
| `crates/aish-cli/src/diag_tasks.rs` | ~150 | 诊断任务持久化 |
| `crates/aish-cli/src/policy_signing.rs` | ~200 | ed25519 签名/验签/密钥管理 |
| `crates/aish-cli/src/remote_hosts.rs` | ~200 | 主机别名/分组存储与解析 |
| `crates/aish-cli/src/remote_identity.rs` | ~300 | 操作者身份：密钥、挑战-签名、信任表 |
| `crates/aish-cli/src/remote_access.rs` | ~110 | remote_access RBAC 校验 |
| `crates/aish-pty/src/session_recording.rs` | ~190 | asciinema 格式录制与回放解码 |
