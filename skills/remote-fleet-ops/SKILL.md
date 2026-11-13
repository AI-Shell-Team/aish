---
name: remote-fleet-ops
version: 1.0.0
description: >
  Operate aish's remote C/S features: take over shell sessions on remote
  hosts, push/inspect signed security policies, pull audit events, run
  whitelisted fleet diagnostics, and check remote setup health. Also matches
  Chinese requests such as 远程接管会话、查看远程会话、推送策略、策略漂移、
  批量诊断、巡检、远程运维.
author: aish
context: subagent
agent: troubleshoot
allowed-tools:
  - bash
  - read_file
  - grep
  - glob
triggers:
  - remote-fleet-ops
  - remote session
  - take over session
  - fleet diagnostics
  - push policy
  - policy drift
  - audit pull
  - remote ops
  - 远程会话
  - 接管会话
  - 批量诊断
  - 巡检
  - 推送策略
  - 策略漂移
  - 远程运维
platforms:
  - linux
distributions:
  - deepin
  - debian
  - ubuntu
  - uos
---

# Remote fleet operations (aish remote C/S)

Drive aish's remote C/S plane: session takeover (#586), signed policy
distribution with drift detection (#587), and whitelisted fleet diagnostics
(#588). The control machine runs `aish remote ...`; targets need only an
aish binary and ssh reachability (password via `AISH_REMOTE_PASSWORD` /
`--password`, or keys).

**Out of scope**: interactive attach (needs a real TTY — for a human, not the
agent), changing policy content on your own initiative, killing sessions
without an explicit user request.

## Rules

1. **Password handling**: never echo, log, or store the password in command
   output. Prefer `export AISH_REMOTE_PASSWORD=...` (not persisted) over
   `--password` (lands in remote-hosts.json, 0600).
2. **Policy pushes need explicit user consent**: always show what will change
   (`policy show` + the local YAML diff) and wait for confirmation. Never
   push a policy the user did not approve.
3. **Session kill is destructive**: `session kill` terminates the remote
   shell and its child processes. Ask first unless the user said so.
4. **Honest results**: diag states are success / unreachable / denied /
   timeout / evidence-insufficient. Unreachable hosts are NOT part of any
   conclusion — say so.
5. **Stop when enough**: once the user's question is answered, do not run
   every subcommand "while you are here".

## Command map

```bash
# one-time setup
aish remote host add <alias> <user@host> [--password '...'] [--group prod] [--remote-bin /path]
aish remote doctor --host <alias|user@host>     # 4 checks: bin/version/sessions/audit

# sessions
aish remote session list --host <h> [--json]    # STATE + CONTROLLER + NAME
aish remote session rename --host <h> --to <name> <id>
aish remote session kill   --host <h> <id>      # destructive — confirm first
aish remote session replay --host <h> <id> [--tail N]   # recorded terminal output

# policy (signed envelope; version auto-increments per host)
aish remote policy push  --host <h|group:tag> --policy <yaml> [--diff] [--yes]
aish remote policy show  --host <h>           # installed version/issuer/time
aish remote policy status --host <h1,h2|group:tag>    # drift: current/drifted/stale/none/unreachable

# audit & diagnostics
aish remote audit pull --host <h>
aish remote diag run    --host <h1,h2> --checks svc,disk,load [--json]
aish remote diag list / status <task> / retry <task>

# identity & signers (see Security layer below)
aish remote admin keygen | show-key | show-identity
aish remote admin add-signer <hex> | list-signers          # policy signatures
aish remote admin trust-operator <hex> | list-operators | untrust-operator <hex>
```

`--host` accepts an alias, a literal `user@host`, or `group:<tag>`.

## Workflow

```text
clarify target host(s) → doctor (first contact) → task (list/diag/policy)
        → report with per-host states → follow-up only on gaps
```

### 1. First contact with a host

Run `doctor` before anything else. Its 4 checks (binary, version, session
discovery, audit store) separate "aish broken" from "feature broken". If
doctor fails, report its fix hint verbatim — every failure mode names one.

### 2. Sessions

`session list` answers "what is running and who controls it". The
CONTROLLER column shows the controlling uid (`uid=0`), `(lost)` when the
controller disconnected, or `—` when never attached. Observers cannot type;
a takeover requires the current controller's approval (10 s timeout denies).

`session replay <id> --tail N` reads the session's recording
(asciinema-compatible `.cast`) — use it to answer "what happened in that
session" without attaching.

### 3. Diagnostics

Whitelisted checks only: `svc`, `ports`, `logs`, `disk`, `load`. The agent
side rejects anything else — there is no arbitrary-command path. Fan-out is
bounded (4 concurrent, 20 s/host); failures are isolated and listed after
`=== Coverage ===` as NOT included in conclusions. Tasks persist
(`dgn-xxxxxx`); `retry` re-runs only failed hosts.

### 4. Policy distribution

The envelope is `version|targets|policy` signed with the admin ed25519 key.
Targets enforce: hostname glob match, strictly increasing version, valid
signature (when signers are configured), parseable YAML. Version never
regresses — to fix a bad policy, push a NEW version, never an old file.

Drift states: `current` / `drifted` (remote newer — changed elsewhere) /
`stale` (remote older — missed update) / `none` / `unreachable`. Converge
stale hosts with a push; investigate drifted hosts before overwriting.

### 5. Security layer (two independent gates)

| Layer | File | Answers |
|-------|------|---------|
| ssh credentials | password or keys | "network reachable" |
| operator identity | `trusted-operators.json` (ed25519 challenge `action\|nonce`) | "who is acting" |
| RBAC | policy `remote_access.actions.<action>.operators` (key-prefix) | "what may this operator do" |

Identity enforcement activates when the target has ≥1 trusted operator; an
empty/absent file is open mode (single-machine default). `remote admin`
subcommands are local management ops and bypass the gate (bootstrap path).
Rejected calls return `{"auth_failed":true,"reason":...}` — surface the
reason verbatim; it names the exact fix (trust-operator / authorization).

## Report skeleton

```text
## Conclusion
1–2 sentences: what the user asked, per-host outcome.

## Evidence
- Per-host table (state / version / failures), task IDs, installed versions
- Unreachable hosts explicitly excluded from conclusions

## Recommendations
- Concrete next commands (converge drift, retry failed hosts, fix auth)
- Anything destructive flagged as needing user confirmation
```

Reply in the user's language.

## Examples

**User**: 看看 130 和 243 磁盘怎么样

1. `diag run --host root@10.10.17.130,root@10.10.17.243 --checks disk,load`
2. `Successful: 2/2` → summarize df/load per host
3. One unreachable → run it anyway, mark that host excluded, offer `diag retry`

**User**: 把新策略推到 prod 组

1. `policy show --host group:prod` members + current versions
2. Show the local YAML diff → **wait for explicit approval**
3. `echo y | policy push --host group:prod --policy <yaml>`
4. `policy status --host group:prod` → all `current`

**User**: 那台机器上的会话现在是谁在用？

1. `session list --host <h>`
2. CONTROLLER column answers; `(lost)` means the controller left
3. Optional: `session replay --tail 20` to see what they ran
