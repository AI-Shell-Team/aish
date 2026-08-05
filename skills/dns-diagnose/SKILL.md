---
name: dns-diagnose
version: 1.0.0
description: >
  Local DNS resolution troubleshooting with dig/resolvectl/whois. Investigate
  NXDOMAIN, wrong answers, records not taking effect, and resolver failures.
  Read-only; do not change DNS config. Also matches Chinese requests such as
  DNS 解析失败、域名解析不了、NXDOMAIN、解析不到.
author: aish
context: subagent
agent: troubleshoot
allowed-tools:
  - bash
  - read_file
  - grep
  - glob
triggers:
  - dns-diagnose
  - DNS diagnosis
  - DNS resolution failed
  - NXDOMAIN
  - cannot resolve domain
  - DNS not working
  - domain resolve
  - DNS 诊断
  - DNS 解析失败
  - 域名解析不了
  - 解析不到
platforms:
  - linux
distributions:
  - deepin
  - debian
  - ubuntu
  - uos
---

# DNS resolution diagnosis

Troubleshoot domain resolution failures, wrong answers, or “I changed the record but it still fails” on **this host**.

**Out of scope**: editing `/etc/resolv.conf` or NetworkManager by default, buying domains / changing authoritative DNS for the user, HTTP 5xx, or TLS certificate issues (`ssl-cert-toolkit`). Path quality after resolve succeeds belongs to `network-path-diagnose`.

## Rules

1. **Need a domain first**: ask if missing; optional expected type (A/AAAA/CNAME/MX/TXT).
2. **Read-only**: inspect resolution and config; do not change DNS servers or hosts unless the user explicitly asks (default is advice only).
3. **Stop when enough**: once you can separate “local resolver” vs “authoritative/record” issues, answer.
4. **Missing tools**: prefer `dig`; else `host` / `nslookup` / `getent`.
5. Ask before sending **internal/sensitive** names to public DNS, `+trace`, or `whois`.

## Workflow

```text
Confirm domain & symptom → local resolver → local dig → optional external checks → answer
```

### 1. Confirm inputs

| Input | Notes |
|-------|-------|
| Domain | e.g. `example.com` or `www.example.com` |
| Symptom | total failure / wrong answer / recent change not visible / only on some networks |
| Record type | default A/AAAA; include MX if mail-related |

### 2. Local resolver

```bash
cat /etc/resolv.conf 2>/dev/null
resolvectl status 2>/dev/null || systemd-resolve --status 2>/dev/null || true
getent ahosts <domain> | head -10
grep '^hosts:' /etc/nsswitch.conf 2>/dev/null
grep -E "^\s*[0-9a-fA-F:.]+\s+.*\b<domain>\b" /etc/hosts 2>/dev/null || true
```

| Observation | Lean toward |
|-------------|-------------|
| `getent` works, browser fails | App/proxy/DoH; not only system DNS |
| Bad `/etc/hosts` entry | Local hijack |
| No nameserver / broken resolved | Local resolver config |

### 3. Local dig (default)

```bash
command -v dig
dig +noall +answer +stats <domain> A
dig +noall +answer <domain> AAAA
```

Without dig: `host <domain>` or `nslookup <domain>`.

| Status | Meaning |
|--------|---------|
| NXDOMAIN | Name/label missing (or not delegated) |
| NOERROR with empty answer | No records of that type |
| SERVFAIL / timeout | Upstream / network / firewall |

Stop here if this explains the symptom.

### 4. Optional external checks

Use when still needed (after consent for sensitive/internal names):

```bash
dig @1.1.1.1 +noall +answer <domain> A
dig @8.8.8.8 +noall +answer <domain> A
dig +trace <domain> A | tail -40
dig NS <apex> +short
dig @<auth-ns> <domain> A +noall +answer
command -v whois && whois <apex> | head -60
```

| Status | Meaning |
|--------|---------|
| Different answers across resolvers | Cache / propagation / split-horizon (check TTL) |
| System resolver fails, public DNS works | Local upstream DNS problem |

No cloud DNS vendor APIs.

## Output format

```text
## Conclusion
1–2 sentences: local resolver / missing record / cache / upstream failure.

## Evidence
- Local nameserver / hosts / getent
- dig (or fallback) status and answers; system vs public DNS
- TTL / authority check if done

## Recommendations
- Action targets (change DNS, fix hosts, wait for TTL, update zone at registrar)
- Do not change config by default
```

Reply in the user's language.

## Related skills

| Case | Skill |
|------|-------|
| Resolves but path is slow/lossy | `network-path-diagnose` |
| HTTPS certificate errors | `ssl-cert-toolkit` |
| Whole-host lag | `diagnose_system_lag` |

## Example

**User**: Browser cannot find `xxx.com`

1. `getent` + `dig xxx.com A`
2. NXDOMAIN → record/domain issue; system SERVFAIL but 8.8.8.8 OK → local upstream DNS
3. Recommend the matching fix
