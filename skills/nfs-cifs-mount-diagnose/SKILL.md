---
name: nfs-cifs-mount-diagnose
version: 1.0.0
description: >
  Diagnose NFS/CIFS(SMB) mount failures on this host. Match common error
  patterns and run read-only client/network checks; suggest fixes without
  auto-running mount or editing fstab. Also matches Chinese requests such as
  NFS 挂载、CIFS 挂载、挂载失败、access denied by server while mounting.
author: aish
context: subagent
agent: troubleshoot
allowed-tools:
  - bash
  - read_file
  - grep
  - glob
triggers:
  - nfs-cifs-mount-diagnose
  - NFS mount
  - CIFS mount
  - SMB mount
  - mount.nfs
  - mount failed
  - access denied by server while mounting
  - mount error
  - NFS 挂载
  - CIFS 挂载
  - SMB 挂载
  - 挂载失败
platforms:
  - linux
distributions:
  - deepin
  - debian
  - ubuntu
  - uos
---

# NFS / CIFS mount diagnosis

Troubleshoot NFS or CIFS/SMB mount failures, boot-time automount failures, and permission denials on **this host**.

**Out of scope**: automatically running `mount`/`umount`, editing fstab, or calling cloud NAS APIs. Suggest only; write actions need explicit user confirmation.

## Rules

1. **Collect first**: protocol (NFS/CIFS), server, export/share path, full error text, mount command used.
2. **Read-only first**: modules, network, current mounts, fstab—do not mount on your own.
3. **Match error patterns early**: treat them as **candidates**, then verify.
4. **Stop when enough**: stop once the likely root cause is clear.
5. **No secrets in chat**: do not paste passwords or `password=` / `credentials=` options.

## Workflow

```text
Collect info → error pattern match → client environment → connectivity → recommendations
```

### 1. Collect info

| Item | Example |
|------|---------|
| Protocol | NFS v3/v4 or CIFS/SMB |
| Server | `files.example.com` or IP |
| Remote path | NFS `:/export/data`; CIFS `//server/share` |
| Full error | whole `mount.nfs: ...` / `mount error(13): ...` line |
| Context | manual / fstab at boot / inside container |

### 2. Error patterns (prefer these)

#### NFS

| Error / symptom | Common cause | Direction |
|-----------------|--------------|-----------|
| `access denied by server while mounting` (esp. with a **subdirectory** path) | Export/ACL/IP, wrong path, or missing subdir | Do not assume “subdir missing” from the message alone; check exports (`showmount -e`), then the path |
| `mount.nfs: No such device` | `nfs`/`sunrpc` not loaded, or bad sunrpc option spelling | Check `lsmod`, `/etc/modprobe.d/*sunrpc*`; use `tcp_slot_table_entries` not `tcp_slot_entries` |
| `Operation not permitted` on NFSv4 while v3 works | NFSv4 client id / hostname conflict, etc. | Try v3 to confirm; check hostname / nfs4 unique id; check server exports |
| `Connection timed out` / `No route to host` | Network, firewall, NFS/RPC ports | `ping` / 2049 / 111 before tuning mount options |
| `Protocol not supported` | Missing `nfs-common` or version mismatch | Install client package; align `vers=` |
| Works manually, fails at boot | `remote-fs` / fstab options / network not ready | `_netdev`, `x-systemd.automount`, `remote-fs.target` |
| `mount: can't find ... in /etc/fstab` | Wrong mount command shape | Missing source or target in the command |

#### CIFS / SMB

| Error / symptom | Common cause | Direction |
|-----------------|--------------|-----------|
| `mount error(13): Permission denied` | Credentials / share ACL | Check user, domain, credentials file mode `0600` |
| `mount error(112): Host is down` / cannot connect | Host, firewall, SMB port 445 | `nc -zv <server> 445` |
| `mount error(2): No such file or directory` | Wrong share/path | Verify `//server/share` |
| `mount error(95): Operation not supported` | Dialect/version mismatch | Try `vers=3.0` / `2.1` / `1.0` (legacy) |
| Missing `mount.cifs` | `cifs-utils` not installed | Debian/Ubuntu: `cifs-utils` |

### 3. Client environment (read-only)

```bash
findmnt -t nfs,nfs4,cifs 2>/dev/null
grep -E 'nfs|cifs|smb' /etc/fstab 2>/dev/null
command -v mount.nfs; dpkg -l nfs-common 2>/dev/null | tail -1
lsmod | grep -E 'nfs|sunrpc'
ls /etc/modprobe.d/*nfs* /etc/modprobe.d/*sunrpc* 2>/dev/null
command -v mount.cifs; dpkg -l cifs-utils 2>/dev/null | tail -1
lsmod | grep cifs
```

### 4. Connectivity (when server is known)

```bash
getent ahosts <server> | head -5
ping -c 3 -W 2 <server>
nc -zv -w 3 <server> 2049
nc -zv -w 3 <server> 111
rpcinfo -p <server> 2>/dev/null | head -20
showmount -e <server> 2>/dev/null | head -20
nc -zv -w 3 <server> 445
```

Port/RPC issues → fix network/firewall before mount options.  
Resolve fails → consider `dns-diagnose`.

### 5. Example command shapes (suggestions only)

```bash
sudo mount -t nfs -o vers=4,proto=tcp <server>:/export /mnt/point
sudo mount -t nfs -o vers=3,proto=tcp <server>:/export /mnt/point
sudo mount -t cifs //server/share /mnt/point -o username=<user>,uid=$(id -u),gid=$(id -g),vers=3.0
```

Debian/Ubuntu client packages: `nfs-common`, `cifs-utils`.

## Output format

```text
## Conclusion
1–2 sentences: protocol + most likely cause.

## Evidence
- User error text and matched pattern
- Client packages/modules, fstab, port checks

## Recommendations
- Prioritized fix steps (user confirms before running)
- Never echo passwords into the chat or logs; remind about credential file permissions
```

Reply in the user's language.

## Related skills

| Case | Skill |
|------|-------|
| DNS failure before mount | `dns-diagnose` |
| Loss/latency to file server | `network-path-diagnose` |
| System lag after a successful mount | `diagnose_system_lag` |

## Example

**User**: `mount.nfs: access denied by server while mounting 10.0.0.5:/data/backup`

1. Candidates → export ACL / wrong path / possibly missing subdir  
2. `showmount -e` / RPC checks; try export root only if appropriate  
3. Root denied → server/client ACL; root works but `/backup` fails → fix subdir  

