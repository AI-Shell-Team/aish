# AISH 远程 C/S 自测教程

> 从零开始，在一台控制机上验证 aish 远程 C/S 架构的全部功能。
> 场景假设：1 台控制机（本机）+ 2 台被管机（示例 `10.10.17.130`、
> `10.10.17.243`，root 密码 `uos123!!`）。二选一：ssh 密码或密钥，教程
> 以密码模式为主线（最贴近"零预配置"）。

---

## 0. 前置条件

| 检查项 | 命令 | 通过标准 |
|---|---|---|
| 构建产物 | `ls target/x86_64-unknown-linux-musl/release/aish` | 文件存在 |
| sshpass | `sshpass -V` | 1.x 可用（仅密码模式需要） |
| 被管机可达 | `ping -c1 <被管机>` | 有回包 |
| 被管机架构 | `ssh <被管机> uname -m` | x86_64（aarch64 需另建，见附录 B） |

---

## 1. 构建与分发

```bash
# 1.1 构建（静态二进制，被管机无需任何依赖）
cd /path/to/aish-cs
cargo build --release --target x86_64-unknown-linux-musl -p aish-cli
md5sum target/x86_64-unknown-linux-musl/release/aish   # 记下 md5

# 1.2 分发（密码模式；首次用 sshpass，之后 aish 自己会带密码）
sshpass -p 'uos123!!' scp -o StrictHostKeyChecking=no \
    target/x86_64-unknown-linux-musl/release/aish \
    root@10.10.17.130:/usr/local/bin/aish

# 1.3 校验 md5 必须一致
sshpass -p 'uos123!!' ssh -o StrictHostKeyChecking=no \
    root@10.10.17.130 md5sum /usr/local/bin/aish
```

> 注意：目标机上若旧版 aish 正在运行，scp 会报 `Text file busy`。
> 先 `sshpass -p 'uos123!!' ssh <host> 'pkill -x aish; sleep 2'` 再传。

---

## 2. 启动被管机的会话 daemon

在**每台被管机**上启动一个 aish 会话（daemon 会自动后台化）：

```bash
# 通过 ssh 远程启动（用 script 包一层拿 PTY）
sshpass -p 'uos123!!' ssh root@10.10.17.130 \
    'nohup aish > /tmp/aish-main.log 2>&1 < /dev/null & sleep 3'

# 验证 daemon 在跑（进程数 >= 2：daemon + shell）
sshpass -p 'uos123!!' ssh root@10.10.17.130 'pgrep -x aish | wc -l'
# 会话文件已生成
sshpass -p 'uos123!!' ssh root@10.10.17.130 \
    'ls /root/.local/share/aish/pty-sessions/*.json'
```

---

## 3. 控制机注册主机与自检

```bash
export AISH_REMOTE_PASSWORD='uos123!!'   # 推荐方式：不落盘
# 或者写进别名（存 remote-hosts.json，0600）：
aish remote host add uos-130 root@10.10.17.130 --password 'uos123!!'

# 自检 4 项：二进制 / 版本 / 会话发现 / 审计存储
aish remote doctor --host root@10.10.17.130
```

预期输出（全部 ✓）：

```
Checking remote C/S setup on root@10.10.17.130...
  ✓ remote aish binary: /usr/local/bin/aish
  ✓ remote version: aish 1.0.0-rc1
  ✓ session discovery works (1 sessions)
  ✓ audit store readable
All checks passed.
```

对 243 重复。然后开始功能测试。

---

## 4. 功能测试清单

### 4.1 会话发现与状态

```bash
aish remote session list --host root@10.10.17.130
```

预期：SESSION / STATE / CONTROLLER / CWD / NAME 表头 + 会话行
（`attached` = daemon 活着且 shell 活着；刚启动时 controller 可能为
`(lost)`——上次控制者已断开，属正常）。

```bash
# JSON 格式（供脚本用）
aish remote session list --host root@10.10.17.130 --json
```

### 4.2 会话命名

```bash
aish remote session rename --host root@10.10.17.130 --to smoke <会话ID前缀>
aish remote session list --host root@10.10.17.130   # NAME 列出现 smoke
```

### 4.3 会话录制与回放

录制自动开启（daemon 在 PTY 读点写 `.cast`）。在被管机上随便敲几条
命令后：

```bash
aish remote session replay --host root@10.10.17.130 smoke --tail 10
# 去掉 --tail 为完整回放；输出应是被管机终端的真实历史
```

### 4.4 批量诊断

```bash
aish remote diag run --host root@10.10.17.130,root@10.10.17.243 \
    --checks svc,disk,load
```

预期：每台机器的检查输出 + `Successful: 2/2 hosts` + 任务 ID
（如 `dgn-xxxxxx`）。

```bash
aish remote diag list                 # 历史任务
aish remote diag status dgn-xxxxxx    # 每台机器单独状态
aish remote diag retry dgn-xxxxxx     # 只重跑失败主机（全成功时提示无需重试）
```

**失败隔离验证**（可选）：把一台关机或改错 IP 再跑一次，预期
`Successful: 1/2`，失败的明确标注 `unreachable (NOT included in
conclusions)`。

### 4.5 策略推送（含签名）

```bash
# 4.5.1 控制机生成管理员密钥对（一次性）
aish remote admin keygen
# 记下输出的公钥 hex

# 4.5.2 每台被管机登记签名者（登录被管机执行）
aish remote admin add-signer <上面的公钥hex>

# 4.5.3 推送（先看 diff，再确认）
aish remote policy push --host root@10.10.17.130 \
    --policy ~/.config/aish/security_policy.yaml --diff
# 出现 unified diff 后输入 y

# 4.5.4 验证版本与元数据
aish remote policy show --host root@10.10.17.130
#   policy v1  issuer: <你的用户名>  installed: <时间>  rules: N
```

**签名链负向验证**（可选）：在被管机 add-signer **之前**推一次策略，
预期 `REJECTED: unsigned envelope rejected`。

### 4.6 策略漂移检测

```bash
aish remote policy status --host root@10.10.17.130,root@10.10.17.243
```

预期：刚推完的机器显示 `current`。状态语义：

| 状态 | 含义 |
|---|---|
| current | 远端版本与本地记录一致 |
| drifted | 远端比本地记录**新**（别人在别处改过） |
| stale | 远端比本地记录**旧**（漏推） |
| none | 远端没装过远程策略 |
| unreachable | 连不上（不代表会话死了） |

### 4.7 主机分组与组推送

```bash
aish remote host remove uos-130; aish remote host add uos-130 \
    root@10.10.17.130 --group prod --password 'uos123!!'
aish remote host remove uos-243; aish remote host add uos-243 \
    root@10.10.17.243 --group prod --password 'uos123!!'
aish remote host list        # GROUPS 列出现 prod

echo y | aish remote policy push --host group:prod \
    --policy ~/.config/aish/security_policy.yaml
# 预期: 每台各自的版本号 + "group 'prod' push: 2 ok, 0 failed"
```

### 4.8 审计拉取

```bash
aish remote audit pull --host root@10.10.17.130
# 首次拉取有内容或 "no new events"；游标文件记录增量位置
```

---

## 5. 身份验证（安全层）测试

> 这一层独立于 ssh 凭据：即使 ssh 用密码，操作者身份照常工作。

### 5.1 开关语义

```bash
# 查看本机操作者身份（自动生成）
aish remote admin show-identity        # 输出 64 位 hex

# 被管机默认无 trusted-operators => 开放模式（单机无感）
aish remote session list --host root@10.10.17.130   # 正常工作
```

### 5.2 开启强制模式

在**每台被管机**上登记控制机的操作者公钥：

```bash
ssh root@10.10.17.130 'aish remote admin trust-operator <控制机公钥hex>'
# 输出: operator trusted (identity enforcement is now ACTIVE on this host)
```

之后控制机的所有远程操作自动带签名挑战，用户无感。

### 5.3 强制模式全链路

```bash
aish remote session list --host root@10.10.17.130     # 应照常工作
aish remote diag run --host root@10.10.17.130 --checks load
aish remote session replay --host root@10.10.17.130 smoke --tail 5
```

### 5.4 负向测试（必须全部被拒）

```bash
# 5.4.1 未受信操作者
ssh root@10.10.17.130 'aish remote admin untrust-operator <控制机公钥hex>'
aish remote session list --host root@10.10.17.130 --json
# 预期: {"auth_failed":true,"reason":"operator … is not trusted …"}

# 5.4.2 伪造签名（在被管机手动喂一条假身份）
ssh root@10.10.17.130 \
  'echo "{\"action\":\"sessions.list\",\"pubkey\":\"<hex>\",\"challenge\":\"x\",\"signature\":\"00\"}" | aish --remote-sessions'
# 预期: {"auth_failed":true,"reason":"challenge signature verification failed"}

# 5.4.3 签名绑定错误的 action
ssh root@10.10.17.130 \
  'echo "{\"action\":\"policy.install\",\"pubkey\":\"<hex>\",\"challenge\":\"x\",\"signature\":\"00\"}" | aish --remote-sessions'
# 预期: identity bound to action 'policy.install', this entry requires 'sessions.list'

# 测完恢复信任
ssh root@10.10.17.130 'aish remote admin trust-operator <控制机公钥hex>'
```

### 5.5 RBAC（操作级授权）

```bash
# 推送一份带 remote_access 的策略（限制谁能 attach / 谁能推策略）
cat > /tmp/rbac.yaml <<'EOF'
global:
  default_risk_level: MEDIUM
audit:
  enabled: true
remote_access:
  actions:
    session.attach:
      operators: ["<被管机自己的身份公钥前8位>"]
    policy.install:
      operators: ["<控制机公钥前8位>"]
EOF
echo y | aish remote policy push --host root@10.10.17.130 --policy /tmp/rbac.yaml

# 在被管机上用"另一台机器的身份"尝试推策略 => 应被拒
# (需要那台机器的操作者公钥已在信任表但不在 policy.install 白名单)
# 预期: policy rejected: operator … is not authorized for 'policy.install'
```

---

## 6. 单机无感回归（重要）

```bash
# 被管机上的本地 aish 功能不受任何 remote 改动影响：
ssh root@10.10.17.130 'aish remote diag list'          # 本地任务列表
ssh root@10.10.17.130 'aish remote admin list-operators'
ssh root@10.10.17.130 'ls ~/.local/share/aish/pty-sessions/'
# 交互式正常使用 aish（;AI 模式、普通命令）应与改动前完全一致
```

---

## 7. 常见问题

| 现象 | 原因 | 处理 |
|---|---|---|
| `Text file busy` | 被管机 aish 进程占着二进制 | `pkill -x aish` 后重传 |
| `no usable aish binary found` | 被管机没装/不在候选路径 | `host add --remote-bin /path` 或 `AISH_REMOTE_BIN` |
| `malformed remote reply` | ssh 认证失败（密码错/免密没配） | 检查 `AISH_REMOTE_PASSWORD` 或配置密钥 |
| scp 到 `/usr/local/bin` 失败 | 目标目录只读或 busy | 传到 `/root/aish-cs-test` 后 `host add --remote-bin` |
| 会话显示 `unreachable` | daemon 挂了或机器断网 | 状态未知≠会话死了；重连后自动恢复 |
| `remote host add` 报 alias 已存在 | 别名占用 | `remote host remove <alias>` 后重加 |
| diag 全部 unreachable | 控制机到被管机网络不通 | `ping`/`ssh` 基础连通性 |
| 强制模式后控制机自己连不上 | 没登记操作者公钥 | 被管机 `trust-operator <控制机 show-identity 输出>` |
| 拿不到自己公钥（死锁） | 不应发生：`admin` 子命令不走身份门 | 升级到 ≥ e672138 的版本 |

---

## 8. 一键自测脚本（快速冒烟）

```bash
#!/bin/bash
# smoke-test.sh <user@host> <password> —— 被管机需已部署 aish 并启动会话
set -e
HOST=$1; PW=$2
export AISH_REMOTE_PASSWORD="$PW"
echo "== doctor:";        aish remote doctor --host "$HOST" | tail -1
echo "== session list:";  aish remote session list --host "$HOST" | tail -1
echo "== diag run:";      aish remote diag run --host "$HOST" --checks load | tail -2
echo "== policy show:";   aish remote policy show --host "$HOST" | head -1
echo "== PASS"
```

用法：`bash smoke-test.sh root@10.10.17.130 'uos123!!'`

---

## 附录 A：aarch64（ARM）被管机

1. 控制机安装交叉编译器：`sudo apt-get install -y gcc-aarch64-linux-gnu`
2. 构建：`cargo build --release --target aarch64-unknown-linux-musl -p aish-cli`
   （若 ring 报错找不到编译器，设
   `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc`）
3. 分发与测试同 x86_64 流程

## 附录 B：从源码跑测试套件

```bash
cargo test --workspace          # 58 个套件，全绿为基线
cargo clippy --workspace --all-targets   # 0 warning 为基线
```
