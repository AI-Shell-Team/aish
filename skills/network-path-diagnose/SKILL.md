---
name: network-path-diagnose
version: 1.0.0
description: >
  Diagnose network path quality from this host to a target using ping/mtr/curl.
  Investigate packet loss, high latency, jitter, and reachability failures; interpret
  hop results and suggest follow-up tests. Do not change network config. Also matches
  Chinese requests such as 网络丢包、延迟高、链路质量、ping 不通、网络抖动.
author: aish
context: subagent
agent: troubleshoot
allowed-tools:
  - bash
  - read_file
  - grep
  - glob
triggers:
  - network-path-diagnose
  - path quality
  - packet loss
  - high latency
  - mtr
  - ping failed
  - network jitter
  - 网络路径诊断
  - 链路质量
  - 网络丢包
  - 延迟高
  - ping 不通
  - 网络抖动
platforms:
  - linux
distributions:
  - deepin
  - debian
  - ubuntu
  - uos
---

# Network path diagnosis

Measure reachability and path quality (**packet loss, latency, jitter**) from **this host** to a user-specified target.

**Out of scope**: cloud VPC/security-group debugging, whole-host lag checks (`diagnose_system_lag`), DNS-only failures (`dns-diagnose`), changing routes or firewall rules.

## Rules

1. **Need a target first**: ask for IP/host/URL if missing; do not invent defaults.
2. **Read-only probes**: ping/mtr/curl/ss are fine; do not change NICs, routes, or firewall.
3. **Stop when enough**: stop once the symptom is explained; do not add unrelated probes.
4. **Missing tools**: if `mtr` is absent, fall back to `ping` + `traceroute`/`tracepath` and note that in Evidence.
5. **Uncontrollable peers**: for public DNS and similar hosts you cannot log into, run forward tests only and state that reverse MTR is impossible.

## Workflow

```text
Confirm target & symptom → quick reachability → path quality (mtr) → TCP/port follow-up if needed → answer
```

### 1. Confirm inputs

| Input | Notes |
|-------|-------|
| Target | IP, hostname, `host:port`, or URL |
| Symptom | unreachable / slow / intermittent loss / bad at certain times |
| Port/protocol | record TCP port if the app uses one (e.g. 443/22) |

### 2. Quick reachability

```bash
getent ahosts <host> | head -5
ping -c 5 -W 2 <host>
curl -sS -o /dev/null -w '%{http_code} time=%{time_total}\n' --connect-timeout 5 <URL>
# or
nc -zv -w 3 <host> <port> 2>&1
```

- Resolve fails → use or hand off to `dns-diagnose`; continue only with a user-provided IP.
- ICMP all lost but TCP port works → ICMP likely filtered; switch to TCP path tests; do not call the path “dead”.

### 3. Path quality (prefer mtr)

```bash
command -v mtr
mtr -rwzc 50 <host>
# without mtr:
traceroute -n <host> 2>/dev/null || tracepath -n <host>
ping -c 20 -W 2 <host>
```

**How to read results**

| Pattern | Likely meaning |
|---------|----------------|
| High loss on middle hops, last hop and destination OK | Middle boxes may rate-limit ICMP; **trust the last hop more** |
| Sustained loss or high latency on the last hop | Path or peer/egress problem |
| Latency jumps after a hop | Bottleneck often after that hop |
| Occasional jitter, no sustained loss | Congestion or local Wi-Fi/interference; retest later |

### 4. Follow-up (only if needed)

```bash
mtr -rwzc 30 --tcp -P <port> <host>
```

If the host itself looks suspicious:

```bash
ip -br link; ip route | head -20
ss -s
```

Bidirectional: if the user can run commands on the peer, ask for an mtr from the peer back to this host’s reachable address; otherwise note forward-only.

## Output format

```text
## Conclusion
1–2 sentences: reachable or not; loss / latency / DNS / port.

## Evidence
- Target and resolution
- ping / mtr (or fallback) numbers; mark forward-only when applicable
- Missing-tool fallbacks

## Recommendations
- Next checks (local Wi-Fi/cable, another network, peer side, ICMP filtered)
- Do not change config automatically
```

Reply in the user's language.

## Related skills

| Case | Skill |
|------|-------|
| Name resolution failed | `dns-diagnose` |
| Host lag / CPU / memory | `diagnose_system_lag` |
| NFS/CIFS mount failed | `nfs-cifs-mount-diagnose` |

## Example

**User**: Intermittent timeouts to `x.x.x.x` on port 443

1. `ping` + `nc -zv x.x.x.x 443`
2. `mtr -rwzc 50`; if mid-hop loss but last hop fine → TCP mtr `-P 443`
3. Conclude from last-hop and TCP results; suggest peer/egress follow-up
