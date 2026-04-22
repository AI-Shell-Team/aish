<div align="center">

[English](README.md) | Deutsch | [简体中文](README_CN.md)

Sprache: Deutsch (German)

---

# AISH

Gib der Shell Denkfähigkeit. Entwickle den Betrieb weiter.

[![Official Website](https://img.shields.io/badge/Website-aishell.ai-blue.svg)](https://www.aishell.ai)
[![GitHub](https://img.shields.io/badge/GitHub-AI--Shell--Team/aish-black.svg)](https://github.com/AI-Shell-Team/aish/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Platform](https://img.shields.io/badge/platform-linux-lightgrey.svg)](#)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

![](./docs/images/demo_show.gif)

**Eine echte KI-Shell: Vollständiges PTY + konfigurierbare Sicherheit & Risikokontrolle**

</div>

---

## Inhaltsverzeichnis

- [Warum AISH](#warum-aish)
- [Schnellstart](#schnellstart)
- [Installation](#installation)
- [Deinstallation](#deinstallation)
- [Konfiguration](#konfiguration)
- [Verwendung](#verwendung)
- [Sicherheit & Risikokontrolle](#sicherheit--risikokontrolle)
- [Skills (Plugins)](#skills-plugins)
- [Daten & Datenschutz](#daten--datenschutz)
- [Dokumentation](#dokumentation)
- [Community & Support](#community--support)
- [Entwicklung & Tests](#entwicklung--tests)
- [Mitwirken](#mitwirken)
- [Lizenz](#lizenz)

---

## Warum AISH

- **Echte interaktive Shell**: Vollständige PTY-Unterstützung, läuft mit interaktiven Programmen wie `vim` / `ssh` / `top`
- **KI-native Integration**: Aufgaben in natürlicher Sprache beschreiben, Befehle generieren, erklären und ausführen
- **Sicher & kontrollierbar**: KI-Befehle mit Risikostufen und Bestätigungsfluss; optionaler Sandbox-Probelauf zur Änderungsbewertung
- **Erweiterbar**: Skills-Plugin-System mit Hot-Loading und Prioritätsüberschreibung
- **Geringe Migrationskosten**: Kompatibel mit üblichen Befehlen und Workflows, standardmäßig alles im Terminal

---

## Funktionsvergleich

| Funktion | AISH | Claude Code |
|---------|------|-------------|
| 🎯 **Kernausrichtung** | Ops/System‑Troubleshooting‑CLI | Entwicklungs‑Coding‑Assistent |
| 🤖 **Multi‑Modell‑Support** | ✅ Vollständig offen | ⚠️ Hauptsächlich Claude |
| 🔧 **Sub‑Agenten‑System** | ✅ ReAct‑Diagnoseagent | ✅ Mehrere Agententypen |
| 🧩 **Skills‑Support** | ✅ Hot‑Loading | ✅ |
| 🖥️ **Native Terminalintegration** | ✅ Vollständige PTY‑Unterstützung | ⚠️ Eingeschränkte Unterstützung |
| 🛡️ **Sicherheits‑Risikobewertung** | ✅ Sicherheitsbestätigung | ✅ Sicherheitsbestätigung |
| 🌐 **Unterstützung lokaler Modelle** | ✅ Vollständig unterstützt | ✅ Vollständig unterstützt |
| 📁 **Werkzeuge für Dateiverarbeitung** | ✅ Minimale Kernunterstützung | ✅ Vollständige Unterstützung |
| 💰 **Komplett kostenlos** | ✅ Open Source | ❌ Kostenpflichtiger Dienst |
| 📊 **Beobachtbarkeit** | ✅ Langfuse optional | ⚠️ Integriert |
| 🌍 **Mehrsprachige Ausgabe** | ✅ Automatische Erkennung | ✅ |

---

## Schnellstart

### 1) Installieren und Starten

#### Option 1: Einzeilige Installation (empfohlen)

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

#### Option 2: Manuelle Bundle-Installation

Lade das passende `aish-<version>-linux-<arch>.tar.gz`-Bundle aus dem offiziellen Release-Verzeichnis herunter und führe dann aus:

```bash
tar -xzf aish-<version>-linux-<arch>.tar.gz
cd aish-<version>-linux-<arch>
sudo ./install.sh
```

Dann starten:

```bash
aish
```

Hinweis: `aish` ohne Unterbefehle entspricht `aish run`.

### 2) Wie eine normale Shell verwenden

```bash
aish> ls -la
aish> cd /etc
aish> vim hosts
```

### 3) Lass die KI die Arbeit erledigen (mit ;) starten)

Beginnend mit `;` oder `；` gelangst du in den KI‑Modus:

```bash
aish> ;finde Dateien größer als 100M im aktuellen Verzeichnis und sortiere nach Größe
aish> ;erkläre diesen Befehl: tar -czf a.tgz ./dir
```

---

## Installation

### Linux-Release-Bundle

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

Der Installer ermittelt die neueste stabile Version, lädt das passende Bundle für deine Architektur herunter und installiert `aish`, `aish-sandbox` und `aish-uninstall` in `/usr/local/bin`.

### Aus dem Quellcode ausführen (Entwicklung/Test)

```bash
uv sync
uv run aish
# oder
python -m aish
```

---

## Deinstallation

Deinstallieren (Konfigurationsdateien behalten):

```bash
sudo aish-uninstall
```

Vollständige Deinstallation (entfernt auch systemweite Sicherheitsrichtlinien):

```bash
sudo aish-uninstall --purge-config
```

Optional: Benutzerkonfiguration bereinigen (löscht Modell/API-Keys usw.):

```bash
rm -rf ~/.config/aish
```

---

## Konfiguration

### Speicherort der Konfigurationsdatei

- Standard: `~/.config/aish/config.yaml` (oder `$XDG_CONFIG_HOME/aish/config.yaml`, wenn `XDG_CONFIG_HOME` gesetzt ist)

### Priorität (hoch nach niedrig)

1. Kommandozeilenargumente
2. Umgebungsvariablen
3. Konfigurationsdatei

### Minimales Konfigurationsbeispiel

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
api_key: your_api_key
```

Alternativ über Umgebungsvariablen (besser für Secrets):

```bash
export AISH_MODEL="openai/deepseek-chat"
export AISH_API_BASE="https://openrouter.ai/api/v1"
export AISH_API_KEY="your_api_key"

```

> Tipp: LiteLLM unterstützt ebenfalls das Lesen anbieterspezifischer Umgebungsvariablen (z. B. `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`).

Interaktive Konfiguration (optional):

```bash
aish setup
```

Tool-Calling-Kompatibilitätsprüfung (bestätigt, dass das gewählte Modell/der Kanal Tool Calling unterstützt):

```bash
aish check-tool-support --model openai/deepseek-chat --api-base https://openrouter.ai/api/v1 --api-key your_api_key
```

Langfuse (optionale Observability):

1) In der Konfiguration aktivieren:

```yaml
enable_langfuse: true
```

2) Umgebungsvariablen setzen:

```bash
export LANGFUSE_PUBLIC_KEY="..."
export LANGFUSE_SECRET_KEY="..."
export LANGFUSE_HOST="https://cloud.langfuse.com"
```

`aish check-langfuse` führt Prüfungen aus, wenn `check_langfuse.py` im Projektstamm vorhanden ist.

---

## Verwendung

### Häufige Eingabetypen

| Typ | Beispiel | Beschreibung |
|:----:|---------|-------------|
| Shell‑Befehle | `ls -la`, `cd /path`, `git status` | Normale Befehle direkt ausführen |
| KI‑Anfragen | `;wie prüfe ich Port‑Belegung`, `;finde Dateien größer als 100M` | Mit `;`/`；`‑Präfix in den KI‑Modus |
| Integrierte Befehle | `help`, `clear`, `exit`, `quit` | Integrierte Shell‑Steuerbefehle |
| Modellwechsel | `/model gpt-4` | Modell anzeigen oder wechseln |

### Shell‑Kompatibilität (PTY)

```bash
aish> ssh user@host
aish> top
aish> vim /etc/hosts
```

---

## Sicherheit & Risikokontrolle

AI Shell führt Sicherheitsbewertungen nur für **KI-generierte und zur Ausführung vorbereitete** Befehle durch.

### Risikostufen

- **LOW**: Standardmäßig erlaubt
- **MEDIUM**: Bestätigung vor Ausführung
- **HIGH**: Standardmäßig blockiert

### Pfad der Sicherheitsrichtliniendatei

Richtliniendateien werden in folgender Reihenfolge aufgelöst:
1. `/etc/aish/security_policy.yaml` (systemweit)
2. `~/.config/aish/security_policy.yaml` (benutzerspezifisch; automatisch generierte Vorlage, falls nicht vorhanden)

### Sandbox-Probelauf (optional, empfohlen für Produktion)

Die Standardrichtlinie hat den Sandbox-Probelauf **deaktiviert**. Zum Aktivieren:

1) In der Sicherheitsrichtlinie setzen:

```yaml
global:
  enable_sandbox: true
```

2) Privilegierten Sandbox-Service starten (systemd):

```bash
sudo systemctl enable --now aish-sandbox.socket
```

Standard-Socket: `/run/aish/sandbox.sock`.
Wenn die Sandbox nicht verfügbar ist, greift je nach Richtlinie `sandbox_off_action` (BLOCK/CONFIRM/ALLOW).

---

## Skills (Plugins)

Skills erweitern das Domänenwissen und die Workflows der KI und unterstützen Hot‑Loading sowie Prioritätsüberschreibung.

Standard-Scan-Verzeichnisse und Priorität:
- `~/.config/aish/skills/` (oder `$AISH_CONFIG_DIR/skills`)
- `~/.claude/skills/`

Paketierte Versionen versuchen beim ersten Start, systemweite Skills in das Benutzerverzeichnis zu kopieren (z. B. `/usr/share/aish/skills`).

Weitere Details: `docs/skills-guide.md`

---

## Daten & Datenschutz

Dieses Projekt speichert folgende Daten lokal (für Troubleshooting und Nachvollziehbarkeit):

- **Logs**: Standard `~/.config/aish/logs/aish.log`
- **Sitzungen/Verlauf**: Standard `~/.local/share/aish/sessions.db` (SQLite)
- **Große Ausgaben‑Offload**: Standard `~/.local/share/aish/offload/`

Empfehlungen:
- Keine echten API-Keys ins Repository committen; bevorzugt Umgebungsvariablen oder Secret-Management.
- In Produktionsumgebungen können Sicherheitsrichtlinien die für die KI zugänglichen Verzeichnisse einschränken.

---

## Dokumentation

- Konfigurationsleitfaden: `CONFIGURATION.md`
- Schnellstart: `QUICKSTART.md`
- Skills‑Nutzung: `docs/skills-guide.md`
- Mechanismus zur Befehlskorrektur: `docs/command-interaction-correction.md`

---

## Community & Support

| Link | Beschreibung |
|------|-------------|
| [Official Website](https://www.aishell.ai) | Projektstartseite und weitere Informationen |
| [GitHub Repository](https://github.com/AI-Shell-Team/aish/) | Quellcode und Issue‑Tracking |
| [GitHub Issues](https://github.com/AI-Shell-Team/aish/issues) | Bug‑Reports |
| [GitHub Discussions](https://github.com/AI-Shell-Team/aish/discussions) | Community‑Diskussionen |
| [Discord](https://discord.com/invite/Pw2mjZt3) | Community beitreten |

---

## Entwicklung & Tests

```bash
uv sync
uv run aish
uv run pytest
```

---

## Mitwirken

Siehe [CONTRIBUTING.md](CONTRIBUTING.md) für Richtlinien.
---

## Lizenz

`LICENSE` (Apache 2.0)
