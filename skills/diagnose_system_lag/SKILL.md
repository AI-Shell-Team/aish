---
name: diagnose_system_lag
version: 3.0.0
description: >
  Local Linux lag and performance diagnosis. Run read-only probes based on the
  user's symptoms to find CPU, memory, disk, or local network bottlenecks, then
  give safe advice. Never auto-remediate. Also matches Chinese requests such as
  系统卡顿、机器好慢、内存不足、突然重启、OOM、磁盘很慢.
author: aish
context: subagent
agent: troubleshoot
allowed-tools:
  - bash
  - read_file
  - grep
  - glob
triggers:
  - diagnose_system_lag
  - system lag diagnosis
  - performance check
  - system is slow
  - high load
  - out of memory
  - OOM
  - disk is slow
  - unexpected reboot
  - 系统卡顿诊断
  - 看看系统卡不卡
  - 系统很卡
  - 机器好慢
  - 内存不足
  - 磁盘很慢
  - 突然重启
  - 异常关机
platforms:
  - linux
distributions:
  - deepin
  - debian
  - ubuntu
  - uos
---

# System lag diagnosis

Short-path, read-only diagnosis for lag, slowness, resource pressure, or a recent unexpected reboot on **this host**.

**Out of scope**: full sos collection, long-running health baselines, cloud VPC troubleshooting, auto config changes or killing processes. Use `sosreport-analyzer`, `deepin-sysassist`, or other skills for those.

## Rules

1. **Read-only**: do not change config, kill processes, drop caches, tune sysctl, or unload modules.
2. **Symptom-first**: decide what to probe from the user's report; do not run a fixed full command list every time.
3. **Stop when enough**: stop once the symptom is explained and key objects (process / mount / device) are named.
4. **Follow-ups fill gaps only**: the next probe answers “what is still missing”, not “scan every focus again”.
5. **Advice needs confirmation**: prefer action targets; give concrete kill/restart commands only if asked, and remind the user to save work.
6. **Isolated sessions**: do not run report-writing scripts under this skill (for example `scripts/diagnose.sh`).
7. **Honest probes**: if a tool is missing or a probe fails, state that in Evidence—do not treat it as “no problem found”.

## Workflow

```text
Understand symptom → quick check (only if vague) → focused probes → answer
                         ↑ stop when evidence is enough ↑
```

### 1. Understand the symptom

| User report | Focus | First step |
|-------------|-------|------------|
| Slow / high load / no specifics | Unknown | Quick check |
| Low memory, Swap, OOM, process killed | Memory | Memory probes |
| Copy/open large files stalls, disk noise, frozen I/O | Disk | Disk probes |
| High CPU, fans spinning, one process pegged | CPU | CPU probes |
| Slow/unreachable network (local host side) | Network | Local network probes |
| Sudden reboot, black screen then recovery, unclean shutdown | Incident | Reboot/crash probes |

Named apps (browser, WPS, Java, a service) are **suspects** under the matching focus—not a separate workflow.

### 2. Quick check (vague symptoms only)

```bash
nproc
uptime
free -h
df -h
vmstat 1 5
ps -eo pid,ppid,comm,%cpu,%mem,state --sort=-%cpu | head -15
ps -eo pid,ppid,comm,%cpu,%mem,state --sort=-%mem | head -15
```

Then decide:

| Observation | Next |
|-------------|------|
| Very low available memory or clearly rising Swap | Memory probes |
| Sustained high `wa` in `vmstat`, or many `D` state processes | Disk probes |
| Load clearly above CPU count and idle is very low | CPU probes |
| High load with high idle and high `wa` | Prefer disk, not CPU |
| Root or home ≥ 90% | Call out disk-full risk; inspect large dirs only if needed |
| All looks normal | Conclude no clear bottleneck; ask when/how it reproduces |

### 3. Focused probes

Add only the set that fills the current gap. If a tool is missing, skip it and note the fallback in the answer.

#### Memory

```bash
free -h
swapon --show
ps -eo pid,user,comm,rss,%mem --sort=-rss | head -20
journalctl -k -b --no-pager | grep -iE 'out of memory|oom|killed process' | tail -30
```

If still missing “who was killed / cache vs anon”:

```bash
grep -E 'MemTotal|MemAvailable|Buffers|Cached|AnonPages|Shmem|SwapTotal|SwapFree' /proc/meminfo
df -h /dev/shm /run /tmp
```

#### Disk

```bash
vmstat 1 5
iostat -xz 1 5
df -h
ps -eo pid,state,comm,wchan:32 | awk '$2=="D" || $2=="d"' | head -20
```

Skip `iostat` if missing and note that in Evidence. If still missing “who is writing” and the tool exists: `pidstat -d 1 5` or `iotop -boPqqq -n 3`.

#### CPU

```bash
nproc; uptime
vmstat 1 5
ps -eo pid,user,comm,state,%cpu,%mem --sort=-%cpu | head -20
```

If still missing “what it waits on”, for a suspect PID: `cat /proc/<pid>/wchan`.

#### Local network (only when the user mentions network)

```bash
ip -s link
ss -s
ss -tnp | head -30
```

Inspect local sockets and error/retransmit counters first. For full path quality to a remote target, prefer `network-path-diagnose`.

#### Incident (sudden reboot / crash recovery)

Start with `last -x`. Prefer specific signatures (OOM killed process, kernel panic, I/O error)—not generic `error`/`fail`/`hung` alone.

```bash
last -x | head -20
journalctl -b -1 -p warning..alert --no-pager | tail -80
journalctl -k -b -1 --no-pager | grep -iE 'Out of memory|Killed process|Kernel panic|Oops:|I/O error' | tail -40
```

Correlated OOM/panic/I/O → conclude from that. Otherwise inconclusive; suggest re-running after the next incident or using `sosreport-analyzer`.

### 4. Threshold hints (secondary to evidence)

- **CPU**: load stays > nproc × 1.5 and idle < 20% → CPU pressure
- **I/O**: sustained `wa` > 5%, or very high device `%util` → I/O pressure
- **Memory**: very low MemAvailable, or Swap used ≥ 50% / ≥ 2GB → memory pressure
- **Disk space**: root or home ≥ 90% → full-disk risk

Thresholds help; **conclusions must follow this run’s samples and logs**.

## Output format

```text
## Conclusion
1–2 sentences: whether something is wrong, main cause, blast radius.

## Evidence
- Only metrics/processes/logs that support the conclusion
- Label timing: current sample / current boot logs / previous boot logs
- Note unavailable or failed probes when relevant

## Recommendations
- Short-term action targets (close user apps, free space, etc.)
- Concrete commands only if asked; warn before kill
- Never recommend stopping: Xorg, wayland, dde-*, systemd, dbus, NetworkManager, fcitx5/ibus
```

If the system looks healthy, still use this skeleton; say no action needed instead of inventing problems. Reply in the user's language.

## Desktop app hints

| Kind | Keywords |
|------|----------|
| Browser | chrome, chromium, firefox |
| WPS | wps, wpp, et |
| WeCom | WXWork, WeMail |
| Editor | code, cursor, typora |
| Proxy | clash |
| Terminal | warp-terminal |

## Examples

**User**: Desktop freezes while copying a large file

1. Focus → disk (skip a blind full check)
2. Disk probes → high `wa`, `D` state processes, busy disk
3. Enough → stop
4. Conclude I/O saturation; suggest off-peak copy / check free space and disk health

**User**: Screen went black briefly; did it crash?

1. Focus → incident
2. Previous-boot journals + `last -x`
3. OOM → memory pressure + suspect process; no clue → say evidence is insufficient and offer next steps
