Sprache: Deutsch (German)

# AISH Roadmap

> **Vision**: AISH von einer aufdringlichen KI-Shell zu einem intelligenten Sidecar‑Assistenten transformieren, der den Nutzern vollständige Kontrolle über ihre Kommandozeile gibt.

---

## 🎯 Strategische Ziele (Q2 2026)

### Kernprinzipien
1. **Nutzerkontrolle zuerst**: KI liefert Vorschläge, blockiert jedoch niemals den Haupt‑Workflow
2. **Standardmäßig asynchron**: Alle KI‑Analysen laufen im Hintergrund
3. **Intelligente Eingriffe**: Fehlalarme durch intelligente Filterung um 95 % reduzieren
4. **Progressive Verbesserung**: Rückwärtskompatibilität bei der Weiterentwicklung der Architektur bewahren

### Erfolgskennzahlen
- ✅ Befehlsausführung kehrt in < 50 ms zum Prompt zurück
- ✅ Null blockierende KI‑Aufrufe im Hauptausführungspfad
- ✅ Fehlalarmrate < 5 %
- ✅ Nutzerzufriedenheit > 4,5/5

---

## 📅 Release‑Zeitleiste

```
Woche 1-2   │ v0.1.0 → v0.2.0  │ Asynchrone KI‑Sidecar‑Architektur
Woche 3-4   │ v0.2.0 → v0.3.0  │ Intelligente Analyse & Nutzerkontrollen
Woche 5-6   │ v0.3.0 → v0.4.0  │ Plan‑Modus & Erweiterte Tools
Woche 7-8   │ v0.4.0 → v0.5.0  │ Multi‑Agenten‑System
Woche 9-10  │ v0.5.0 → v0.6.0  │ Rich UI & Aufgabenmanagement
Woche 11-12 │ v0.6.0 → v0.7.0  │ Agent SDK & MCP‑Protokoll
```

---

## 🚀 Phase 1: Architektur‑Overhaul (Woche 1-4)

### v0.1.1 (Woche 1) - Kritische Fixes
**Typ**: Patch‑Release

**Fixes**
- 🐛 Skill‑Tool‑Invocation implementieren (`skill.py` TODO)
- 🐛 Tool‑Call‑Historie wird nicht in den Speicher aufgenommen (`shell.py:889`)
- 🐛 Auto‑Trigger von `handle_error_detect()` deaktivieren (temporäre Abhilfe)

**Auswirkung**: Reduziert akute Nutzerprobleme

---

### v0.2.0 (Woche 2) - Asynchrones KI‑Sidecar 🔥
**Typ**: Minor‑Release (Breaking Changes)

**Neue Architektur**
```
┌─────────────────────────────────────────────────────────┐
│                    Haupt‑Shell‑Prozess                  │
│  ┌──────────────┐                                       │
│  │   Eingabe    │ → Ausführen → Anzeige → Prompt        │
│  └──────────────┘      ↓                                │
│               Event einreihen (nicht blockierend)       │
└─────────────────────────┼───────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│                  KI‑Sidecar im Hintergrund              │
│  ┌──────────────┐   ┌──────────────┐   ┌────────────┐ │
│  │ Event‑Queue  │ → │ Smart‑Filter │ → │ LLM‑Worker │ │
│  └──────────────┘   └──────────────┘   └────────────┘ │
│                          ↓                               │
│                   ┌──────────────┐                      │
│                   │  Hinweis     │ → [AI:2]‑Indikator   │
│                   └──────────────┘                      │
└─────────────────────────────────────────────────────────┘
```

**Neue Module**
- `src/aish/sidecar/event_queue.py` - Nicht blockierende Ereignis‑Queue für Befehle
- `src/aish/sidecar/analyzer.py` - Logik für intelligentes Filtern
- `src/aish/sidecar/worker.py` - Hintergrund‑LLM‑Analyse‑Worker
- `src/aish/sidecar/notification.py` - Nicht‑intrusives Benachrichtigungssystem
- `src/aish/sidecar/storage.py` - Persistenz der Analyseergebnisse

**Kernfunktionen**
1. **Event‑Queue**: Befehlsabschluss‑Events werden eingereiht, ohne den Hauptfluss zu blockieren
2. **Smart Analyzer**: Filtert Fehlalarme (grep, diff, ssh, Ctrl‑C usw.)
3. **Background Worker**: Asynchrone LLM‑Analyse in separaten Tasks
4. **Benachrichtigungszentrum**: Leichter Statusindikator im Prompt (`[AI:2]`)

**Benutzerbefehle**
- `:ai` oder `:ai last` - Neueste Empfehlung anzeigen
- `:ai list` - Alle ausstehenden Empfehlungen auflisten
- `:ai show <n>` - Bestimmte Empfehlung anzeigen
- `:ai apply <n>` - Empfehlung anwenden (geht in den Sicherheits‑Freigabefluss)

**Breaking Changes**
- ❌ Auto‑Trigger von `handle_error_detect()` entfernt
- ❌ Auto‑Trigger von `handle_command_error()` entfernt
- ❌ `ShellState.CORRECT_PENDING` veraltet

**Migrationsleitfaden**
- Altes Verhalten: KI unterbricht automatisch bei Befehlsfehlern
- Neues Verhalten: KI analysiert im Hintergrund, Nutzer sehen Empfehlungen explizit
- Kompatibilität: `handle_command_error()`‑API bleibt für manuelle Aufrufe erhalten

**Konfiguration**
```yaml
# ~/.config/aish/config.yaml
sidecar:
  enabled: true
  max_queue_size: 100
  worker_threads: 1
```

---

### v0.2.1 (Woche 3) - Stabilität
**Typ**: Patch‑Release

**Fixes**
- 🐛 Speicherleck im Sidecar‑Worker beheben
- 🐛 Event‑Queue‑Performance optimieren
- 📊 Sidecar‑Analyse‑Metriken hinzufügen (Erfolgsrate, Latenz)

---

### v0.3.0 (Woche 4) - Intelligente Analyse
**Typ**: Minor‑Release

**Erweiterte Intelligenz**
- 🧠 **Kontextbewusstes Filtern**
  - Analyse der Befehlshistorie (aufeinanderfolgende Fehler → höhere Priorität)
  - Nutzerverhalten‑Tracking (sofortiger Retry → niedrigere Priorität)
  - Zeitbasierte Anpassung (späte Nacht → niedrigere Priorität)

**Konfigurierbare Strategien**
```yaml
sidecar:
  analysis_mode: smart  # smart | aggressive | minimal
  notification_style: indicator  # indicator | toast | silent

  # Regeln für Smart‑Modus
  smart_rules:
    ignore_commands: [grep, diff, test, ssh]
    ignore_exit_codes: [130]  # Ctrl-C
    benign_stderr_patterns:
      - "^Warning:"
      - "^Note:"
```

**Neue Befehle**
- `:ai clear` - Alle Empfehlungen löschen
- `:ai stats` - Analyse‑Statistiken anzeigen

**Verbesserungen**
- 95 % weniger Fehlalarme
- < 50 ms Befehlsausführungs‑Overhead
- Konfigurierbare Benachrichtigungsstile

---

## 🛠️ Phase 2: Kernfähigkeiten (Woche 5-8)

### v0.3.1 (Woche 5) - Verfeinerung
**Typ**: Patch‑Release

**Fixes**
- 🐛 Edge‑Cases im Smart‑Filtering beheben
- 🐛 Performance des Suggestion‑Speichers optimieren

---

### v0.4.0 (Woche 6) - Plan‑Modus & Tools
**Typ**: Minor‑Release

**Plan‑Modus** (inspiriert von Claude Code)
- 🎯 `PlanAgent`: Experte für Aufgabenzerlegung
- 📋 Nutzerfreigabe‑Workflow vor Ausführung
- 💾 Plan‑Persistenz in `.aish/plans/`

**Nutzung**
```bash
aish> ;deploy the application to production
[AI erstellt einen Plan]
┌─ Deployment‑Plan ─────────────────────────────────────┐
│ 1. Test‑Suite ausführen                               │
│ 2. Docker‑Image bauen                                 │
│ 3. In Registry pushen                                 │
│ 4. Kubernetes‑Deployment aktualisieren                │
│                                                      │
│ Genehmigen? [y/N]:                                    │
└───────────────────────────────────────────────────────┘
```

**Erweiterte Tool‑Suite**
- 🔍 `WebSearchTool`: DuckDuckGo‑Integration
- 🔧 `GitTool`: Wrapper für Git‑Operationen (status, diff, commit, push)
- 🧠 `CodeAnalysisTool`: AST‑basierte Codeanalyse (tree‑sitter)

**Neue Befehle**
- `:plan` - Plan‑Modus starten
- `:plan show` - Aktuellen Plan anzeigen
- `:plan approve` - Plan genehmigen und ausführen

---

### v0.4.1 (Woche 7) - Politur
**Typ**: Patch‑Release

**Fixes**
- 🐛 Edge‑Cases im Plan‑Modus beheben
- 🐛 WebSearch‑Ergebnis‑Parsing optimieren
- 📝 Plan‑Modus‑Dokumentation hinzufügen

---

### v0.5.0 (Woche 8) - Multi‑Agenten‑System
**Typ**: Minor‑Release

**Agenten‑Ökosystem**
- 🤖 `CodeReviewAgent`: Statische Analyse + Best Practices
- 🐛 `DebugAgent`: Log‑Analyse + Ursachenidentifikation
- 🔍 `ResearchAgent`: Web‑ und Dokumentationssuche
- 🎭 `AgentOrchestrator`: Parallel/sequentielle Agentenkoordination

**Agentenarchitektur**
```python
# Beispiel: Parallele Agentenausführung
aish> ;review this PR and check for security issues

[Agents parallel gestartet]
├─ CodeReviewAgent: Analysiert Codequalität...
├─ SecurityAgent: Scannt nach Schwachstellen...
└─ TestAgent: Prüft Testabdeckung...

[Ergebnisse aggregiert und präsentiert]
```

**Intelligentes Kontextmanagement**
- 📊 Prioritätsbasierte Nachrichtenbewertung
- 🗜️ Automatische Zusammenfassung langer Gespräche (mit kleinem Modell)
- 🧠 Sitzungsübergreifende Wissensbasis (optional per Vektorsuche)

**Konfiguration**
```yaml
agents:
  enabled: true
  max_parallel: 3
  context_window: 8000
  auto_summarize: true
```

---

## 🎨 Phase 3: Nutzererlebnis & Ökosystem (Woche 9-12)

### v0.5.1 (Woche 9) - Stabilität
**Typ**: Patch‑Release

**Fixes**
- 🐛 Race‑Conditions bei paralleler Agentenausführung beheben
- 🐛 Kontextkompressions‑Algorithmus optimieren
- 📊 Agenten‑Performance‑Metriken hinzufügen

---

### v0.6.0 (Woche 10) - Rich UI & Tasks
**Typ**: Minor‑Release

**Rich‑UI‑Verbesserungen**
- 🎨 Echtzeit‑Fortschrittsbalken für Agentenausführung
- 🌳 Visualisierung des Aufgabenbaums
- 🔍 Interaktive Bestätigungs‑Panels mit Diff‑Vorschau

**Aufgabenmanagement‑System** (inspiriert von Claude Code)
- 📋 Integriertes Aufgaben‑Tracking (`TaskCreate`, `TaskUpdate`, `TaskList`)
- 🔗 Aufgabenabhängigkeiten und Prioritäten
- 💾 Aufgaben‑Persistenz und Wiederherstellung

**Nutzung**
```bash
aish> :task list
┌─ Aktive Aufgaben ─────────────────────────────────────┐
│ [1] ⏳ Benutzer‑Authentifizierung implementieren      │
│     ├─ [2] ✅ Datenbankschema einrichten              │
│     ├─ [3] 🔄 Login‑Endpoint erstellen                │
│     └─ [4] ⏸️  JWT‑Token‑Validierung hinzufügen        │
└───────────────────────────────────────────────────────┘

aish> :task show 3
[Detaillierte Aufgabenansicht mit Fortschritt und Blockern]
```

**Neue Befehle**
- `:task create` - Neue Aufgabe erstellen
- `:task list` - Alle Aufgaben auflisten
- `:task show <id>` - Aufgabendetails anzeigen
- `:task complete <id>` - Aufgabe als erledigt markieren

---

### v0.6.1 (Woche 11) - Optimierung
**Typ**: Patch‑Release

**Fixes**
- 🐛 Speicherverbrauch des Task‑Managers beheben
- 🐛 Rendering‑Performance der Rich UI optimieren
- 📝 Aufgabenmanagement‑Dokumentation hinzufügen

---

### v0.7.0 (Woche 12) - Agent SDK & MCP
**Typ**: Minor‑Release

**Agent SDK**
- 🔌 Standardisierte Schnittstelle für Agentenentwicklung
- 🏗️ Agent‑Template‑Generator (`aish create-agent`)
- 🌐 Agent‑Marktplatz (Community‑Sharing)

**Agentenentwicklungs‑Beispiel**
```bash
# Neuen Agent erstellen
aish create-agent --name my-agent --type diagnostic

# Generierte Struktur
~/.config/aish/agents/my-agent/
├── agent.py          # Agent‑Implementierung
├── config.yaml       # Agent‑Konfiguration
├── README.md         # Dokumentation
└── tests/            # Unit‑Tests
```

**MCP‑Protokoll‑Support**
- 🔗 Kompatibel mit Claude Desktop MCP‑Servern
- 🔌 Integrierter MCP‑Client für externe Services
- 📡 Bidirektionale Kommunikation mit dem MCP‑Ökosystem

**Observability‑Dashboard** (optional)
- 📊 Web‑UI (FastAPI‑basiert)
- 📈 Echtzeit‑Session‑Monitoring
- 💰 Token‑Nutzungsstatistiken
- ⚡ Agenten‑Performance‑Analysen

**Konfiguration**
```yaml
mcp:
  enabled: true
  servers:
    - name: filesystem
      command: npx
      args: [-y, @modelcontextprotocol/server-filesystem, /tmp]
    - name: github
      command: npx
      args: [-y, @modelcontextprotocol/server-github]
      env:
        GITHUB_TOKEN: ${GITHUB_TOKEN}

observability:
  enabled: false  # Optionales Web‑Dashboard
  port: 8080
```

---

## 📊 Feature‑Vergleichsmatrix

| Feature | v0.1.0 (Aktuell) | v0.2.0 | v0.4.0 | v0.6.0 | v0.7.0 |
|---------|------------------|--------|--------|--------|--------|
| **Asynchrone KI‑Analyse** | ❌ | ✅ | ✅ | ✅ | ✅ |
| **Intelligentes Filtern** | ❌ | ⚠️ Basis | ✅ Fortgeschritten | ✅ | ✅ |
| **Plan‑Modus** | ❌ | ❌ | ✅ | ✅ | ✅ |
| **Multi‑Agent** | ⚠️ 1 Agent | ⚠️ 1 Agent | ⚠️ 1 Agent | ✅ 4+ Agenten | ✅ |
| **Aufgabenmanagement** | ❌ | ❌ | ❌ | ✅ | ✅ |
| **Agent SDK** | ❌ | ❌ | ❌ | ❌ | ✅ |
| **MCP‑Protokoll** | ❌ | ❌ | ❌ | ❌ | ✅ |
| **Rich UI** | ⚠️ Basis | ⚠️ Basis | ⚠️ Basis | ✅ | ✅ |

---

## 🎯 Wettbewerbspositionierung

### vs Claude Code

| Aspekt | AISH v0.7.0 | Claude Code |
|--------|-------------|-------------|
| **Open Source** | ✅ Apache 2.0 | ❌ Proprietär |
| **Lokale Modelle** | ✅ Volle Unterstützung | ❌ Eingeschränkt |
| **Multi‑Provider** | ✅ LiteLLM | ⚠️ Hauptsächlich Claude |
| **PTY‑Support** | ✅ Vollständig | ⚠️ Eingeschränkt |
| **Ops‑Fokus** | ✅ Systemdiagnose | ⚠️ Dev‑Fokus |
| **Datenschutz** | ✅ Local‑first | ⚠️ Cloud‑basiert |
| **Kosten** | ✅ Kostenlos | ❌ Abonnement |
| **Asynchrone KI** | ✅ Nicht blockierend | ⚠️ Blockierend |

### Einzigartige Wertversprechen

1. **Nicht‑intrusive KI**: Hintergrundanalyse blockiert nie den Nutzer‑Workflow
2. **Ops‑nativ**: Gebaut für Systemadministration und Troubleshooting
3. **Datenschutz‑first**: Lokale Modelle ohne Datenabfluss
4. **Community‑getrieben**: Open Source mit erweiterbarem Agenten‑Ökosystem
5. **Enterprise‑ready**: Sandbox, Audit‑Logs und feingranulare Berechtigungen

---

## 🚨 Risikomanagement

| Risiko | Auswirkung | Gegenmaßnahme | Status |
|------|--------|------------|--------|
| **Async‑Komplexität** | Hoch | Umfangreiche Tests + Fallback‑Modus | Woche 1-2 |
| **Smart‑Filter‑Genauigkeit** | Mittel | Konfigurierbare Regeln + Nutzerfeedback | Woche 3-4 |
| **Worker‑Ressourcennutzung** | Mittel | Queue‑Limits + Auto‑Throttling | Woche 2-3 |
| **Nutzerakzeptanz** | Niedrig | Progressive Migration + Doku | Laufend |
| **Agentenkoordination** | Mittel | Timeout‑Limits + Fehler‑Recovery | Woche 7-8 |

---

## 📈 Erfolgskennzahlen

### Technische KPIs
- ✅ Befehlsausführungs‑Latenz < 50 ms (P95)
- ✅ Fehlalarmrate der KI‑Analyse < 5 %
- ✅ Sidecar‑Worker‑Speicher < 50 MB
- ✅ Agenten‑Antwortzeit < 2 s (P95)
- ✅ Testabdeckung > 80 %

### Nutzer‑KPIs
- 📈 Wachstum der täglich aktiven Nutzer > 20 % MoM
- ⭐ Nutzerzufriedenheit > 4,5/5
- 🤝 Community‑Skills > 50
- 🏢 Enterprise‑Deployments > 10

### Ökosystem‑KPIs
- 🔌 Community‑Agenten > 20
- 📦 MCP‑Integrationen > 5
- 📝 Dokumentations‑Vollständigkeit > 90 %

---

## 🤝 Mitwirken

Wir freuen uns über Beiträge! Priorisierte Bereiche:

### Woche 1-4 (Phase 1)
- 🔧 Umsetzung der Sidecar‑Architektur
- 🧪 Entwicklung von Smart‑Filtering‑Regeln
- 📝 Dokumentation des Migrationsleitfadens

### Woche 5-8 (Phase 2)
- 🤖 Neue Agenten‑Implementierungen
- 🔍 Tool‑Integrationen (Web‑Suche, Code‑Analyse)
- 🧠 Optimierung des Kontextmanagements

### Woche 9-12 (Phase 3)
- 🎨 UI/UX‑Verbesserungen
- 🔌 Entwicklung des Agent SDK
- 📊 Observability‑Dashboard

Siehe [CONTRIBUTING.md](CONTRIBUTING.md) für detaillierte Richtlinien.

---

## 📚 Ressourcen

- **Dokumentation**: [docs.aishell.ai](https://docs.aishell.ai)
- **GitHub**: [github.com/AI-Shell-Team/aish](https://github.com/AI-Shell-Team/aish)
- **Discord**: [discord.gg/aish](https://discord.gg/aish)

---

## 📝 Änderungsprotokoll

### Kommend
- Siehe die einzelnen Release‑Abschnitte oben

### v0.1.0 (Aktuell)
- ✅ Vollständige PTY‑Unterstützung
- ✅ Multi‑Modell‑Support (LiteLLM)
- ✅ Grundlegende Sicherheitsrisikobewertung
- ✅ Skills‑Hot‑Reload‑System
- ✅ ReAct‑Diagnoseagent
- ✅ Sitzungspersistenz (SQLite)
- ✅ Output‑Offload‑Mechanismus
- ✅ i18n‑Support

---

**Zuletzt aktualisiert**: 2026-03-06
**Roadmap‑Version**: 2.0
**Status**: 🟢 Aktive Entwicklung
