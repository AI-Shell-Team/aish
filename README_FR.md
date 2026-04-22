<div align="center">

[English](README.md) | Français | [简体中文](README_CN.md)

Langue : français (French)

---

# AISH

Donnez au shell la capacité de penser. Faites évoluer les opérations.

[![Official Website](https://img.shields.io/badge/Website-aishell.ai-blue.svg)](https://www.aishell.ai)
[![GitHub](https://img.shields.io/badge/GitHub-AI--Shell--Team/aish-black.svg)](https://github.com/AI-Shell-Team/aish/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Platform](https://img.shields.io/badge/platform-linux-lightgrey.svg)](#)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

![](./docs/images/demo_show.gif)

**Un véritable shell IA : PTY complet + sécurité et contrôle des risques configurables**

</div>

---

## Table des matières

- [Pourquoi choisir AISH](#pourquoi-choisir-aish)
- [Démarrage rapide](#démarrage-rapide)
- [Installation](#installation)
- [Désinstallation](#désinstallation)
- [Configuration](#configuration)
- [Utilisation](#utilisation)
- [Sécurité et contrôle des risques](#sécurité-et-contrôle-des-risques)
- [Skills (Plugins)](#skills-plugins)
- [Données et confidentialité](#données-et-confidentialité)
- [Documentation](#documentation)
- [Communauté et support](#communauté-et-support)
- [Développement et tests](#développement-et-tests)
- [Contribuer](#contribuer)
- [Licence](#licence)

---

## Pourquoi choisir AISH

- **Véritable shell interactif** : prise en charge PTY complète, exécute des programmes interactifs comme `vim` / `ssh` / `top`
- **Intégration IA native** : décrire les tâches en langage naturel, générer, expliquer et exécuter des commandes
- **Sûr et contrôlable** : les commandes IA ont un niveau de risque et un flux de confirmation ; bac à sable optionnel avant exécution pour évaluer les changements
- **Extensible** : système de plugins Skills avec chargement à chaud et priorité
- **Faible coût de migration** : compatible avec les commandes et workflows habituels, tout reste dans le terminal par défaut

---

## Comparaison des fonctionnalités

| Fonctionnalité | AISH | Claude Code |
|---------|------|-------------|
| 🎯 **Positionnement principal** | CLI d'exploitation / dépannage système | Assistant de développement |
| 🤖 **Prise en charge multi‑modèles** | ✅ Entièrement ouvert | ⚠️ Principalement Claude |
| 🔧 **Système de sous‑agents** | ✅ Agent de diagnostic ReAct | ✅ Plusieurs types d’agents |
| 🧩 **Prise en charge des Skills** | ✅ Chargement à chaud | ✅ |
| 🖥️ **Intégration native au terminal** | ✅ Prise en charge PTY complète | ⚠️ Prise en charge limitée |
| 🛡️ **Évaluation des risques de sécurité** | ✅ Confirmation de sécurité | ✅ Confirmation de sécurité |
| 🌐 **Prise en charge des modèles locaux** | ✅ Entièrement pris en charge | ✅ Entièrement pris en charge |
| 📁 **Outils d’opérations sur fichiers** | ✅ Prise en charge minimale essentielle | ✅ Prise en charge complète |
| 💰 **Entièrement gratuit** | ✅ Open source | ❌ Service payant |
| 📊 **Observabilité** | ✅ Langfuse optionnel | ⚠️ Intégré |
| 🌍 **Sortie multilingue** | ✅ Détection automatique | ✅ |

---

## Démarrage rapide

### 1) Installer et lancer

#### Option 1 : Installation en une ligne (recommandé)

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

#### Option 2 : Installation manuelle du bundle

Téléchargez le bundle correspondant `aish-<version>-linux-<arch>.tar.gz` depuis le répertoire officiel des releases, puis exécutez :

```bash
tar -xzf aish-<version>-linux-<arch>.tar.gz
cd aish-<version>-linux-<arch>
sudo ./install.sh
```

Puis lancez :

```bash
aish
```

Remarque : `aish` sans sous‑commandes équivaut à `aish run`.

### 2) Utiliser comme un shell classique

```bash
aish> ls -la
aish> cd /etc
aish> vim hosts
```

### 3) Laisser l’IA faire le travail (commencer par ;)

Commencer par `;` ou `；` active le mode IA :

```bash
aish> ;trouve les fichiers plus grands que 100M dans le répertoire courant et trie par taille
aish> ;explique cette commande : tar -czf a.tgz ./dir
```

---

## Installation

### Bundle de release Linux

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

L’installeur détermine la dernière version stable, télécharge le bundle correspondant à votre architecture et installe `aish`, `aish-sandbox` et `aish-uninstall` dans `/usr/local/bin`.

### Exécuter depuis les sources (développement/essai)

```bash
uv sync
uv run aish
# ou
python -m aish
```

---

## Désinstallation

Désinstaller (conserver les fichiers de configuration) :

```bash
sudo aish-uninstall
```

Désinstallation complète (supprime aussi les politiques de sécurité système) :

```bash
sudo aish-uninstall --purge-config
```

Optionnel : nettoyer la configuration utilisateur (efface les clés modèle/API, etc.) :

```bash
rm -rf ~/.config/aish
```

---

## Configuration

### Emplacement du fichier de configuration

- Par défaut : `~/.config/aish/config.yaml` (ou `$XDG_CONFIG_HOME/aish/config.yaml` si `XDG_CONFIG_HOME` est défini)

### Priorité (du plus haut au plus bas)

1. Arguments de ligne de commande
2. Variables d’environnement
3. Fichier de configuration

### Exemple de configuration minimale

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
api_key: your_api_key
```

Ou via les variables d’environnement (plus adapté aux secrets) :

```bash
export AISH_MODEL="openai/deepseek-chat"
export AISH_API_BASE="https://openrouter.ai/api/v1"
export AISH_API_KEY="your_api_key"

```

> Astuce : LiteLLM prend aussi en charge la lecture des variables d’environnement spécifiques aux fournisseurs (p. ex. `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`).

Configuration interactive (optionnelle) :

```bash
aish setup
```

Vérification de compatibilité avec les tool calls (confirme que le modèle/le canal sélectionné prend en charge les tool calls) :

```bash
aish check-tool-support --model openai/deepseek-chat --api-base https://openrouter.ai/api/v1 --api-key your_api_key
```

Langfuse (observabilité optionnelle) :

1) Activer dans la configuration :

```yaml
enable_langfuse: true
```

2) Définir les variables d’environnement :

```bash
export LANGFUSE_PUBLIC_KEY="..."
export LANGFUSE_SECRET_KEY="..."
export LANGFUSE_HOST="https://cloud.langfuse.com"
```

`aish check-langfuse` exécute des vérifications lorsque `check_langfuse.py` existe à la racine du projet.

---

## Utilisation

### Types d’entrée courants

| Type | Exemple | Description |
|:----:|---------|-------------|
| Commandes shell | `ls -la`, `cd /path`, `git status` | Exécuter directement les commandes classiques |
| Requêtes IA | `;comment vérifier l’utilisation des ports`, `;trouve les fichiers plus grands que 100M` | Entrer en mode IA avec le préfixe `;`/`；` |
| Commandes intégrées | `help`, `clear`, `exit`, `quit` | Commandes de contrôle intégrées du shell |
| Changement de modèle | `/model gpt-4` | Voir ou changer de modèle |

### Compatibilité shell (PTY)

```bash
aish> ssh user@host
aish> top
aish> vim /etc/hosts
```

---

## Sécurité et contrôle des risques

AI Shell effectue l’évaluation de sécurité uniquement sur les commandes **générées par l’IA et prêtes à être exécutées**.

### Niveaux de risque

- **LOW** : Autorisé par défaut
- **MEDIUM** : Confirmation avant exécution
- **HIGH** : Bloqué par défaut

### Chemin des fichiers de politique de sécurité

Les fichiers de politique sont résolus dans l’ordre suivant :
1. `/etc/aish/security_policy.yaml` (niveau système)
2. `~/.config/aish/security_policy.yaml` (niveau utilisateur ; modèle généré automatiquement si absent)

### Exécution préalable en bac à sable (optionnelle, recommandée en production)

La politique par défaut a l’exécution préalable en bac à sable **désactivée**. Pour l’activer :

1) Définir dans la politique de sécurité :

```yaml
global:
  enable_sandbox: true
```

2) Démarrer le service de bac à sable privilégié (systemd) :

```bash
sudo systemctl enable --now aish-sandbox.socket
```

Socket par défaut : `/run/aish/sandbox.sock`.
Lorsque le bac à sable est indisponible, le comportement suit `sandbox_off_action` (BLOCK/CONFIRM/ALLOW) dans la politique.

---

## Skills (Plugins)

Les Skills étendent les connaissances et workflows de l’IA, avec chargement à chaud et priorité configurable.

Répertoires analysés par défaut et priorité :
- `~/.config/aish/skills/` (ou `$AISH_CONFIG_DIR/skills`)
- `~/.claude/skills/`

Les versions packagées tentent de copier les skills système dans le répertoire utilisateur au premier lancement (p. ex. `/usr/share/aish/skills`).

Pour plus de détails : `docs/skills-guide.md`

---

## Données et confidentialité

Ce projet stocke localement les données suivantes (pour le dépannage et la traçabilité) :

- **Logs** : par défaut `~/.config/aish/logs/aish.log`
- **Sessions/Historique** : par défaut `~/.local/share/aish/sessions.db` (SQLite)
- **Déport des grandes sorties** : par défaut `~/.local/share/aish/offload/`

Recommandations :
- Ne commitez pas de vraies clés API dans le dépôt ; préférez les variables d’environnement ou un gestionnaire de secrets.
- En production, combinez les politiques de sécurité pour limiter la portée des répertoires accessibles à l’IA.

---

## Documentation

- Guide de configuration : `CONFIGURATION.md`
- Démarrage rapide : `QUICKSTART.md`
- Utilisation des Skills : `docs/skills-guide.md`
- Mécanisme de correction de commandes : `docs/command-interaction-correction.md`

---

## Communauté et support

| Lien | Description |
|------|-------------|
| [Official Website](https://www.aishell.ai) | Page du projet et plus d’informations |
| [GitHub Repository](https://github.com/AI-Shell-Team/aish/) | Code source et suivi des issues |
| [GitHub Issues](https://github.com/AI-Shell-Team/aish/issues) | Rapports de bugs |
| [GitHub Discussions](https://github.com/AI-Shell-Team/aish/discussions) | Discussions de la communauté |
| [Discord](https://discord.com/invite/Pw2mjZt3) | Rejoindre la communauté |

---

## Développement et tests

```bash
uv sync
uv run aish
uv run pytest
```

---

## Contribuer

Voir [CONTRIBUTING.md](CONTRIBUTING.md) pour les directives.
---

## Licence

`LICENSE` (Apache 2.0)
