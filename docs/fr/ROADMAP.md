Langue : français (French)

# Feuille de route AISH

> **Vision** : Transformer AISH d’un shell IA intrusif en un assistant sidecar intelligent qui donne aux utilisateurs un contrôle absolu de leur ligne de commande.

---

## 🎯 Objectifs stratégiques (T2 2026)

### Principes clés
1. **Contrôle utilisateur d’abord** : l’IA propose des suggestions, ne bloque jamais le flux principal
2. **Asynchrone par défaut** : toutes les analyses IA se déroulent en arrière‑plan
3. **Intervention intelligente** : réduire les faux positifs de 95 % via un filtrage intelligent
4. **Amélioration progressive** : maintenir la rétro‑compatibilité tout en faisant évoluer l’architecture

### Indicateurs de succès
- ✅ L’exécution des commandes revient au prompt en < 50 ms
- ✅ Zéro appel IA bloquant dans le chemin d’exécution principal
- ✅ Taux de faux positifs < 5 %
- ✅ Satisfaction utilisateur > 4,5/5

---

## 📅 Calendrier des releases

```
Semaine 1-2   │ v0.1.0 → v0.2.0  │ Architecture sidecar IA asynchrone
Semaine 3-4   │ v0.2.0 → v0.3.0  │ Analyse intelligente & contrôles utilisateur
Semaine 5-6   │ v0.3.0 → v0.4.0  │ Mode Plan & outils améliorés
Semaine 7-8   │ v0.4.0 → v0.5.0  │ Système multi‑agents
Semaine 9-10  │ v0.5.0 → v0.6.0  │ Rich UI & gestion des tâches
Semaine 11-12 │ v0.6.0 → v0.7.0  │ SDK d’agents & protocole MCP
```

---

## 🚀 Phase 1 : Refonte d’architecture (Semaines 1-4)

### v0.1.1 (Semaine 1) - Correctifs critiques
**Type** : Release correctif

**Correctifs**
- 🐛 Implémenter l’invocation de l’outil skill (`skill.py` TODO)
- 🐛 Corriger l’historique des tool calls non ajouté à la mémoire (`shell.py:889`)
- 🐛 Désactiver l’auto‑déclenchement de `handle_error_detect()` (atténuation temporaire)

**Impact** : Réduit les douleurs immédiates des utilisateurs

---

### v0.2.0 (Semaine 2) - Sidecar IA asynchrone 🔥
**Type** : Release mineure (Breaking Changes)

**Nouvelle architecture**
```
┌─────────────────────────────────────────────────────────┐
│                  Processus principal du shell           │
│  ┌──────────────┐                                       │
│  │   Saisie     │ → Exécuter → Afficher → Prompt        │
│  └──────────────┘      ↓                                │
│                Mise en file (non bloquante)             │
└─────────────────────────┼───────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│                 Sidecar IA en arrière‑plan              │
│  ┌──────────────┐   ┌──────────────┐   ┌────────────┐ │
│  │ File d’évén. │ → │ Filtre smart │ → │ LLM Worker │ │
│  └──────────────┘   └──────────────┘   └────────────┘ │
│                          ↓                               │
│                   ┌──────────────┐                      │
│                   │ Notification │ → Indicateur [AI:2] │
│                   └──────────────┘                      │
└─────────────────────────────────────────────────────────┘
```

**Nouveaux modules**
- `src/aish/sidecar/event_queue.py` - File d’événements de commandes non bloquante
- `src/aish/sidecar/analyzer.py` - Logique de filtrage intelligent
- `src/aish/sidecar/worker.py` - Worker d’analyse LLM en arrière‑plan
- `src/aish/sidecar/notification.py` - Système de notification non intrusif
- `src/aish/sidecar/storage.py` - Persistance des résultats d’analyse

**Fonctionnalités clés**
1. **File d’événements** : les événements de fin de commande sont mis en file sans bloquer le flux principal
2. **Analyseur intelligent** : filtre les faux positifs (grep, diff, ssh, Ctrl‑C, etc.)
3. **Worker en arrière‑plan** : analyse LLM asynchrone dans une tâche séparée
4. **Centre de notifications** : indicateur d’état léger dans le prompt (`[AI:2]`)

**Commandes utilisateur**
- `:ai` ou `:ai last` - Voir la dernière suggestion
- `:ai list` - Lister toutes les suggestions en attente
- `:ai show <n>` - Voir une suggestion spécifique
- `:ai apply <n>` - Appliquer la suggestion (entre dans le flux d’approbation sécurité)

**Breaking Changes**
- ❌ Suppression de l’auto‑déclenchement de `handle_error_detect()`
- ❌ Suppression de l’auto‑déclenchement de `handle_command_error()`
- ❌ `ShellState.CORRECT_PENDING` déprécié

**Guide de migration**
- Ancien comportement : l’IA interrompait automatiquement en cas d’échec de commande
- Nouveau comportement : l’IA analyse en arrière‑plan, l’utilisateur consulte explicitement les suggestions
- Compatibilité : l’API `handle_command_error()` est conservée pour l’invocation manuelle

**Configuration**
```yaml
# ~/.config/aish/config.yaml
sidecar:
  enabled: true
  max_queue_size: 100
  worker_threads: 1
```

---

### v0.2.1 (Semaine 3) - Stabilité
**Type** : Release correctif

**Correctifs**
- 🐛 Corriger une fuite mémoire du worker sidecar
- 🐛 Optimiser les performances de la file d’événements
- 📊 Ajouter des métriques d’analyse du sidecar (taux de succès, latence)

---

### v0.3.0 (Semaine 4) - Analyse intelligente
**Type** : Release mineure

**Intelligence améliorée**
- 🧠 **Filtrage contextuel**
  - Analyse de l’historique des commandes (échecs consécutifs → priorité plus élevée)
  - Suivi du comportement utilisateur (re‑tentative immédiate → priorité plus faible)
  - Ajustement temporel (tard dans la nuit → priorité plus faible)

**Stratégies configurables**
```yaml
sidecar:
  analysis_mode: smart  # smart | aggressive | minimal
  notification_style: indicator  # indicator | toast | silent

  # Règles du mode smart
  smart_rules:
    ignore_commands: [grep, diff, test, ssh]
    ignore_exit_codes: [130]  # Ctrl-C
    benign_stderr_patterns:
      - "^Warning:"
      - "^Note:"
```

**Nouvelles commandes**
- `:ai clear` - Effacer toutes les suggestions
- `:ai stats` - Afficher les statistiques d’analyse

**Améliorations**
- 95 % de faux positifs en moins
- < 50 ms de surcharge d’exécution des commandes
- Styles de notification configurables

---

## 🛠️ Phase 2 : Capacités cœur (Semaines 5-8)

### v0.3.1 (Semaine 5) - Affinage
**Type** : Release correctif

**Correctifs**
- 🐛 Corriger les cas limites du filtrage smart
- 🐛 Optimiser les performances de stockage des suggestions

---

### v0.4.0 (Semaine 6) - Mode Plan & Outils
**Type** : Release mineure

**Mode Plan** (inspiré de Claude Code)
- 🎯 `PlanAgent` : expert en décomposition de tâches
- 📋 Workflow d’approbation utilisateur avant exécution
- 💾 Persistance du plan dans `.aish/plans/`

**Utilisation**
```bash
aish> ;deploy the application to production
[L’IA crée un plan]
┌─ Plan de déploiement ─────────────────────────────────┐
│ 1. Exécuter la suite de tests                          │
│ 2. Construire l’image Docker                           │
│ 3. Pousser vers le registre                             │
│ 4. Mettre à jour le déploiement Kubernetes             │
│                                                      │
│ Approuver ? [y/N]:                                     │
└───────────────────────────────────────────────────────┘
```

**Suite d’outils améliorée**
- 🔍 `WebSearchTool` : intégration DuckDuckGo
- 🔧 `GitTool` : wrapper d’opérations Git (status, diff, commit, push)
- 🧠 `CodeAnalysisTool` : analyse de code basée sur AST (tree‑sitter)

**Nouvelles commandes**
- `:plan` - Entrer en mode plan
- `:plan show` - Voir le plan actuel
- `:plan approve` - Approuver et exécuter le plan

---

### v0.4.1 (Semaine 7) - Finitions
**Type** : Release correctif

**Correctifs**
- 🐛 Corriger les cas limites du mode plan
- 🐛 Optimiser l’analyse des résultats WebSearch
- 📝 Ajouter la documentation du mode plan

---

### v0.5.0 (Semaine 8) - Système multi‑agents
**Type** : Release mineure

**Écosystème d’agents**
- 🤖 `CodeReviewAgent` : analyse statique + bonnes pratiques
- 🐛 `DebugAgent` : analyse de logs + identification des causes racines
- 🔍 `ResearchAgent` : recherche web + documentation
- 🎭 `AgentOrchestrator` : coordination parallèle/séquentielle des agents

**Architecture des agents**
```python
# Exemple : exécution parallèle d’agents
aish> ;review this PR and check for security issues

[Agents lancés en parallèle]
├─ CodeReviewAgent : Analyse de la qualité du code...
├─ SecurityAgent : Scan des vulnérabilités...
└─ TestAgent : Vérifie la couverture de tests...

[Résultats agrégés et présentés]
```

**Gestion intelligente du contexte**
- 📊 Classement des messages basé sur la priorité
- 🗜️ Auto‑résumé des longues conversations (avec un petit modèle)
- 🧠 Base de connaissances inter‑sessions (recherche vectorielle optionnelle)

**Configuration**
```yaml
agents:
  enabled: true
  max_parallel: 3
  context_window: 8000
  auto_summarize: true
```

---

## 🎨 Phase 3 : Expérience utilisateur & écosystème (Semaines 9-12)

### v0.5.1 (Semaine 9) - Stabilité
**Type** : Release correctif

**Correctifs**
- 🐛 Corriger les conditions de course en exécution parallèle d’agents
- 🐛 Optimiser l’algorithme de compression de contexte
- 📊 Ajouter des métriques de performance des agents

---

### v0.6.0 (Semaine 10) - Rich UI & Tâches
**Type** : Release mineure

**Améliorations Rich UI**
- 🎨 Barres de progression en temps réel pour l’exécution des agents
- 🌳 Visualisation de l’arbre des tâches
- 🔍 Panneaux de confirmation interactifs avec aperçu des diffs

**Système de gestion des tâches** (inspiré de Claude Code)
- 📋 Suivi des tâches intégré (`TaskCreate`, `TaskUpdate`, `TaskList`)
- 🔗 Dépendances et priorités des tâches
- 💾 Persistance et récupération des tâches

**Utilisation**
```bash
aish> :task list
┌─ Tâches actives ───────────────────────────────────────┐
│ [1] ⏳ Implémenter l’authentification utilisateur       │
│     ├─ [2] ✅ Mettre en place le schéma BD              │
│     ├─ [3] 🔄 Créer l’endpoint de connexion            │
│     └─ [4] ⏸️  Ajouter la validation du jeton JWT       │
└───────────────────────────────────────────────────────┘

aish> :task show 3
[Vue détaillée de la tâche avec progression et blocages]
```

**Nouvelles commandes**
- `:task create` - Créer une nouvelle tâche
- `:task list` - Lister toutes les tâches
- `:task show <id>` - Voir les détails d’une tâche
- `:task complete <id>` - Marquer une tâche comme terminée

---

### v0.6.1 (Semaine 11) - Optimisation
**Type** : Release correctif

**Correctifs**
- 🐛 Corriger l’usage mémoire du gestionnaire de tâches
- 🐛 Optimiser les performances de rendu Rich UI
- 📝 Ajouter la documentation du gestionnaire de tâches

---

### v0.7.0 (Semaine 12) - SDK d’agents & MCP
**Type** : Release mineure

**SDK d’agents**
- 🔌 Interface standardisée de développement d’agents
- 🏗️ Générateur de templates d’agents (`aish create-agent`)
- 🌐 Marketplace d’agents (partage communautaire)

**Exemple de développement d’agents**
```bash
# Créer un nouvel agent
aish create-agent --name my-agent --type diagnostic

# Structure générée
~/.config/aish/agents/my-agent/
├── agent.py          # Implémentation de l’agent
├── config.yaml       # Configuration de l’agent
├── README.md         # Documentation
└── tests/            # Tests unitaires
```

**Support du protocole MCP**
- 🔗 Compatible avec les serveurs MCP de Claude Desktop
- 🔌 Client MCP intégré pour services externes
- 📡 Communication bidirectionnelle avec l’écosystème MCP

**Tableau de bord d’observabilité** (optionnel)
- 📊 Web UI (basé sur FastAPI)
- 📈 Suivi des sessions en temps réel
- 💰 Statistiques d’utilisation des tokens
- ⚡ Analyses de performance des agents

**Configuration**
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
  enabled: false  # Tableau de bord web optionnel
  port: 8080
```

---

## 📊 Matrice de comparaison des fonctionnalités

| Fonctionnalité | v0.1.0 (Actuel) | v0.2.0 | v0.4.0 | v0.6.0 | v0.7.0 |
|---------|------------------|--------|--------|--------|--------|
| **Analyse IA asynchrone** | ❌ | ✅ | ✅ | ✅ | ✅ |
| **Filtrage intelligent** | ❌ | ⚠️ Basique | ✅ Avancé | ✅ | ✅ |
| **Mode Plan** | ❌ | ❌ | ✅ | ✅ | ✅ |
| **Multi‑agents** | ⚠️ 1 agent | ⚠️ 1 agent | ⚠️ 1 agent | ✅ 4+ agents | ✅ |
| **Gestion des tâches** | ❌ | ❌ | ❌ | ✅ | ✅ |
| **SDK d’agents** | ❌ | ❌ | ❌ | ❌ | ✅ |
| **Protocole MCP** | ❌ | ❌ | ❌ | ❌ | ✅ |
| **Rich UI** | ⚠️ Basique | ⚠️ Basique | ⚠️ Basique | ✅ | ✅ |

---

## 🎯 Positionnement concurrentiel

### vs Claude Code

| Aspect | AISH v0.7.0 | Claude Code |
|--------|-------------|-------------|
| **Open Source** | ✅ Apache 2.0 | ❌ Propriétaire |
| **Modèles locaux** | ✅ Support complet | ❌ Limité |
| **Multi‑fournisseur** | ✅ LiteLLM | ⚠️ Principalement Claude |
| **Support PTY** | ✅ Complet | ⚠️ Limité |
| **Focus Ops** | ✅ Diagnostic système | ⚠️ Focus dev |
| **Confidentialité** | ✅ Local‑first | ⚠️ Cloud‑based |
| **Coût** | ✅ Gratuit | ❌ Abonnement |
| **IA asynchrone** | ✅ Non bloquante | ⚠️ Bloquante |

### Propositions de valeur uniques

1. **IA non intrusive** : l’analyse en arrière‑plan ne bloque jamais le workflow utilisateur
2. **Ops‑native** : conçu pour l’administration système et le dépannage
3. **Confidentialité d’abord** : support des modèles locaux sans sortie de données
4. **Piloté par la communauté** : open source avec écosystème d’agents extensible
5. **Prêt pour l’entreprise** : sandbox, logs d’audit et permissions fines

---

## 🚨 Gestion des risques

| Risque | Impact | Atténuation | Statut |
|------|--------|------------|--------|
| **Complexité async** | Élevé | Tests extensifs + mode fallback | Semaines 1-2 |
| **Précision du smart filter** | Moyen | Règles configurables + feedback utilisateur | Semaines 3-4 |
| **Usage ressources worker** | Moyen | Limites de queue + auto‑throttling | Semaines 2-3 |
| **Adoption utilisateur** | Faible | Migration progressive + docs | En cours |
| **Coordination des agents** | Moyen | Limites de timeout + récupération d’erreurs | Semaines 7-8 |

---

## 📈 Indicateurs de succès

### KPI techniques
- ✅ Latence d’exécution des commandes < 50 ms (P95)
- ✅ Taux de faux positifs d’analyse IA < 5 %
- ✅ Mémoire du worker sidecar < 50 MB
- ✅ Temps de réponse des agents < 2 s (P95)
- ✅ Couverture de tests > 80 %

### KPI utilisateurs
- 📈 Croissance des utilisateurs actifs quotidiens > 20 % MoM
- ⭐ Satisfaction utilisateur > 4,5/5
- 🤝 Skills communautaires > 50
- 🏢 Déploiements entreprise > 10

### KPI écosystème
- 🔌 Agents communautaires > 20
- 📦 Intégrations MCP > 5
- 📝 Complétude de la documentation > 90 %

---

## 🤝 Contribuer

Nous accueillons les contributions ! Domaines prioritaires :

### Semaines 1-4 (Phase 1)
- 🔧 Implémentation de l’architecture sidecar
- 🧪 Développement des règles de filtrage smart
- 📝 Documentation du guide de migration

### Semaines 5-8 (Phase 2)
- 🤖 Nouvelles implémentations d’agents
- 🔍 Intégrations d’outils (recherche web, analyse de code)
- 🧠 Optimisation de la gestion de contexte

### Semaines 9-12 (Phase 3)
- 🎨 Améliorations UI/UX
- 🔌 Développement du SDK d’agents
- 📊 Tableau de bord d’observabilité

Voir [CONTRIBUTING.md](CONTRIBUTING.md) pour les directives détaillées.

---

## 📚 Ressources

- **Documentation** : [docs.aishell.ai](https://docs.aishell.ai)
- **GitHub** : [github.com/AI-Shell-Team/aish](https://github.com/AI-Shell-Team/aish)
- **Discord** : [discord.gg/aish](https://discord.gg/aish)

---

## 📝 Changelog

### À venir
- Voir les sections de release ci‑dessus

### v0.1.0 (Actuel)
- ✅ Support PTY complet
- ✅ Support multi‑modèles (LiteLLM)
- ✅ Évaluation de base des risques de sécurité
- ✅ Système de hot‑reload des Skills
- ✅ Agent de diagnostic ReAct
- ✅ Persistance des sessions (SQLite)
- ✅ Mécanisme d’offload des sorties
- ✅ Support i18n

---

**Dernière mise à jour** : 2026-03-06
**Version de la roadmap** : 2.0
**Statut** : 🟢 Développement actif
