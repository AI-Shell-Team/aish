---
name: ssl-cert-toolkit
version: 1.0.0
description: >
  Local SSL/TLS certificate toolkit using openssl: inspect certificates, check
  key/cert/CSR match, generate CSRs, convert PEM/PFX, and run simple chain checks.
  No cloud certificate APIs. Also matches Chinese requests such as SSL 证书、
  证书匹配、生成 CSR、转换证书格式、检查证书.
author: aish
allowed-tools:
  - bash
  - read_file
  - grep
  - glob
triggers:
  - ssl-cert-toolkit
  - SSL certificate
  - certificate match
  - check certificate
  - generate CSR
  - convert certificate
  - PEM PFX
  - openssl certificate
  - key cert match
  - SSL 证书
  - 证书匹配
  - 检查证书
  - 生成 CSR
  - 转换证书格式
platforms:
  - linux
distributions:
  - deepin
  - debian
  - ubuntu
  - uos
---

# SSL certificate toolkit

Use `openssl` on **this host** to inspect certificates, verify key/cert/CSR pairing, generate CSRs, convert common formats, and run simple validation.

**Out of scope**: cloud purchase/upload/deploy APIs, silently changing the system trust store, submitting CA requests for the user.

## Rules

1. **Clarify intent first**: inspect / match / generate CSR / convert / verify—ask if unclear.
2. **Confirm before writing files**: confirm output paths; do not overwrite existing files unless asked.
3. **Protect private keys**: never paste key material into chat; suggest mode `0600`; do not log key bodies.
4. **Dependency**: require `openssl` (`command -v openssl`). `keytool` only for JKS.
5. **Stop when enough**: after match/inspect succeeds, do not “optimize” unrelated files.
6. **Fail closed on match**: if OpenSSL cannot parse an input, report that—never treat empty digests as MATCH.

## Capabilities

| Intent | Example asks |
|--------|--------------|
| Inspect | validity dates, SAN, issuer |
| Match | are cert and key a pair; does CSR match |
| Generate CSR | create CSR to get a signed cert |
| Convert | PEM ↔ PFX/P12, split chain |
| Verify | chain OK, expired or not |

## Inspect

```bash
openssl version
openssl x509 -in <cert.pem> -noout -subject -issuer -dates -ext subjectAltName 2>/dev/null
openssl x509 -in <cert.pem> -noout -text | head -80
```

For PFX, ask for the password; prefer `-passin env:CERT_PASS` over putting secrets on the command line:

```bash
openssl pkcs12 -in <file.pfx> -nokeys -clcerts -passin env:CERT_PASS
```

## Match check (cert / key / CSR)

Compare public keys (preferred) or RSA moduli. Each openssl extract must succeed and produce non-empty output before comparing:

```bash
openssl x509 -in <cert.pem> -noout -pubkey | openssl md5
openssl pkey -in <key.pem> -pubout | openssl md5
openssl req -in <csr.pem> -noout -pubkey | openssl md5   # if CSR given
```

Parse failure or empty digest → not MATCH. Equal non-empty digests → **MATCH**.  
Optional: `openssl req -in <csr.pem> -noout -verify` (CSR signature; separate from key match).

## Generate CSR

Confirm CN/SAN, algorithm (default RSA 2048), and output directory.

```bash
openssl req -new -newkey rsa:2048 -nodes \
  -keyout <domain>.key -out <domain>.csr \
  -subj "/CN=<domain>"

openssl req -in <domain>.csr -noout -text | head -60
```

With SAN, write a temporary `san.cnf` (user confirms) then:

```bash
openssl req -new -newkey rsa:2048 -nodes \
  -keyout <domain>.key -out <domain>.csr \
  -config san.cnf -extensions v3_req
```

ECDSA:

```bash
openssl ecparam -genkey -name prime256v1 -out <domain>.key
openssl req -new -key <domain>.key -out <domain>.csr -subj "/CN=<domain>"
```

Remind: do not commit the private key; CSR can go to the CA.

## Format conversion

Confirm inputs and target format first.

```bash
openssl pkcs12 -export -out <out.pfx> -inkey <key.pem> -in <cert.pem> [-certfile chain.pem]
openssl pkcs12 -in <in.pfx> -clcerts -nokeys -out cert.pem
openssl pkcs12 -in <in.pfx> -nocerts -nodes -out key.pem
openssl x509 -in <file> -noout -subject 2>/dev/null || openssl req -in <file> -noout -subject 2>/dev/null
```

Handle JKS only if the user asks and `keytool` exists; otherwise prefer PEM/PFX.

## Simple verification

```bash
openssl x509 -in <cert.pem> -noout -checkend 0 && echo 'not expired' || echo 'expired or invalid'
openssl verify -CAfile <ca_or_chain.pem> <cert.pem>
```

Live site (read-only) when the user gives an HTTPS host:

```bash
echo | openssl s_client -connect <host>:443 -servername <host> 2>/dev/null | openssl x509 -noout -subject -dates
```

## Output format

```text
## Conclusion
What was done: inspect / match result / generated paths / conversion outputs.

## Evidence
- Relevant openssl output (redacted; no private key body)
- Digest comparison for match checks

## Recommendations
- Next steps (submit CSR, deploy where, `chmod 600`)
- Direction if expired or mismatched
```

Reply in the user's language.

## Related skills

| Case | Skill |
|------|-------|
| Domain does not resolve | `dns-diagnose` |
| Network fails before TLS | `network-path-diagnose` |

## Example

**User**: Are `server.crt` and `server.key` a pair?

1. Confirm readable paths  
2. Compare pubkey/modulus digests  
3. Conclude MATCH or name the mismatch  
