<div align="center">

[English](README.md) | 日本語 | [简体中文](README_CN.md)

言語：日本語 (Japanese)

---

# AISH

シェルに思考力を。運用を進化させる。

[![Official Website](https://img.shields.io/badge/Website-aishell.ai-blue.svg)](https://www.aishell.ai)
[![GitHub](https://img.shields.io/badge/GitHub-AI--Shell--Team/aish-black.svg)](https://github.com/AI-Shell-Team/aish/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Platform](https://img.shields.io/badge/platform-linux-lightgrey.svg)](#)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

![](./docs/images/demo_show.gif)

**本物のAIシェル：完全なPTY + 設定可能なセキュリティとリスク制御**

</div>

---

## 目次

- [AISHを選ぶ理由](#aishを選ぶ理由)
- [クイックスタート](#クイックスタート)
- [インストール](#インストール)
- [アンインストール](#アンインストール)
- [設定](#設定)
- [使い方](#使い方)
- [セキュリティとリスク制御](#セキュリティとリスク制御)
- [Skills (Plugins)](#skills-plugins)
- [データとプライバシー](#データとプライバシー)
- [ドキュメント](#ドキュメント)
- [コミュニティとサポート](#コミュニティとサポート)
- [開発とテスト](#開発とテスト)
- [コントリビュート](#コントリビュート)
- [ライセンス](#ライセンス)

---

## AISHを選ぶ理由

- **真の対話型シェル**：完全なPTY対応で、`vim` / `ssh` / `top` などの対話型プログラムを実行可能
- **AIネイティブ統合**：自然言語でタスクを記述し、コマンドを生成・説明・実行
- **安全で制御可能**：AIコマンドはリスク評価と確認フロー付き。変更評価のためのサンドボックス事前実行も任意で利用可能
- **拡張可能**：ホットロードと優先度上書きに対応したSkillsプラグインシステム
- **移行コストが低い**：通常のコマンドやワークフローに互換、デフォルトですべてターミナル内

---

## 機能比較

| 機能 | AISH | Claude Code |
|---------|------|-------------|
| 🎯 **コアの位置づけ** | 運用/システム障害対応CLI | 開発向けコーディングアシスタント |
| 🤖 **マルチモデル対応** | ✅ 完全にオープン | ⚠️ 主にClaude |
| 🔧 **サブエージェントシステム** | ✅ ReAct診断エージェント | ✅ 複数のエージェントタイプ |
| 🧩 **Skills対応** | ✅ ホットロード | ✅ |
| 🖥️ **ネイティブターミナル統合** | ✅ 完全なPTY対応 | ⚠️ 限定的な対応 |
| 🛡️ **セキュリティリスク評価** | ✅ セキュリティ確認 | ✅ セキュリティ確認 |
| 🌐 **ローカルモデル対応** | ✅ 完全対応 | ✅ 完全対応 |
| 📁 **ファイル操作ツール** | ✅ 必要最小限のサポート | ✅ フルサポート |
| 💰 **完全無料** | ✅ オープンソース | ❌ 有料サービス |
| 📊 **可観測性** | ✅ Langfuseは任意 | ⚠️ 内蔵 |
| 🌍 **多言語出力** | ✅ 自動検出 | ✅ |

---

## クイックスタート

### 1) インストールして起動

#### オプション1：ワンラインインストール（推奨）

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

#### オプション2：手動でバンドルをインストール

公式リリースディレクトリから対応する `aish-<version>-linux-<arch>.tar.gz` バンドルをダウンロードし、次を実行します：

```bash
tar -xzf aish-<version>-linux-<arch>.tar.gz
cd aish-<version>-linux-<arch>
sudo ./install.sh
```

起動：

```bash
aish
```

注：`aish` をサブコマンドなしで実行すると `aish run` と同等です。

### 2) 通常のシェルとして使う

```bash
aish> ls -la
aish> cd /etc
aish> vim hosts
```

### 3) AIに任せる（先頭に ; を付ける）

先頭が `;` または `；` の入力はAIモードになります：

```bash
aish> ;現在のディレクトリで100Mを超えるファイルを探してサイズ順に並べて
aish> ;このコマンドを説明して: tar -czf a.tgz ./dir
```

---

## インストール

### Linuxリリースバンドル

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

インストーラは最新の安定版を解決し、アーキテクチャに対応するバンドルをダウンロードして `aish`、`aish-sandbox`、`aish-uninstall` を `/usr/local/bin` にインストールします。

### ソースから実行（開発/試用）

```bash
uv sync
uv run aish
# または
python -m aish
```

---

## アンインストール

アンインストール（設定ファイルは保持）：

```bash
sudo aish-uninstall
```

完全アンインストール（システムレベルのセキュリティポリシーも削除）：

```bash
sudo aish-uninstall --purge-config
```

任意：ユーザー設定のクリーンアップ（モデル/APIキーなどを削除）：

```bash
rm -rf ~/.config/aish
```

---

## 設定

### 設定ファイルの場所

- デフォルト：`~/.config/aish/config.yaml`（`XDG_CONFIG_HOME` が設定されている場合は `$XDG_CONFIG_HOME/aish/config.yaml`）

### 優先順位（高い順）

1. コマンドライン引数
2. 環境変数
3. 設定ファイル

### 最小構成例

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
api_key: your_api_key
```

環境変数でも設定可能（シークレット向き）：

```bash
export AISH_MODEL="openai/deepseek-chat"
export AISH_API_BASE="https://openrouter.ai/api/v1"
export AISH_API_KEY="your_api_key"

```

> ヒント：LiteLLM はベンダー固有の環境変数（例：`OPENAI_API_KEY`、`ANTHROPIC_API_KEY`）の読み取りにも対応しています。

対話型の設定（任意）：

```bash
aish setup
```

ツール呼び出し互換性チェック（選択したモデル/チャネルが tool calling をサポートしているか確認）：

```bash
aish check-tool-support --model openai/deepseek-chat --api-base https://openrouter.ai/api/v1 --api-key your_api_key
```

Langfuse（任意の可観測性）：

1) 設定で有効化：

```yaml
enable_langfuse: true
```

2) 環境変数を設定：

```bash
export LANGFUSE_PUBLIC_KEY="..."
export LANGFUSE_SECRET_KEY="..."
export LANGFUSE_HOST="https://cloud.langfuse.com"
```

`aish check-langfuse` はプロジェクトルートに `check_langfuse.py` が存在する場合にチェックを実行します。

---

## 使い方

### よく使う入力タイプ

| 種類 | 例 | 説明 |
|:----:|---------|-------------|
| シェルコマンド | `ls -la`, `cd /path`, `git status` | 通常のコマンドを直接実行 |
| AIリクエスト | `;ポート使用状況の確認方法`, `;100M超のファイルを探して` | `;`/`；` プレフィックスでAIモードへ |
| 内蔵コマンド | `help`, `clear`, `exit`, `quit` | シェルの内蔵制御コマンド |
| モデル切替 | `/model gpt-4` | モデルの表示/切替 |

### シェル互換性（PTY）

```bash
aish> ssh user@host
aish> top
aish> vim /etc/hosts
```

---

## セキュリティとリスク制御

AI Shell は **AIが生成し実行準備が整った** コマンドのみセキュリティ評価を行います。

### リスクレベル

- **LOW**：デフォルトで許可
- **MEDIUM**：実行前に確認
- **HIGH**：デフォルトでブロック

### セキュリティポリシーファイルのパス

ポリシーファイルは次の順で解決されます：
1. `/etc/aish/security_policy.yaml`（システムレベル）
2. `~/.config/aish/security_policy.yaml`（ユーザーレベル；存在しない場合はテンプレートを自動生成）

### サンドボックス事前実行（任意、プロダクション推奨）

デフォルトポリシーではサンドボックス事前実行が **無効** です。有効化するには：

1) セキュリティポリシーで設定：

```yaml
global:
  enable_sandbox: true
```

2) 特権サンドボックスサービスを起動（systemd）：

```bash
sudo systemctl enable --now aish-sandbox.socket
```

デフォルトソケット：`/run/aish/sandbox.sock`。
サンドボックスが利用できない場合は、ポリシーの `sandbox_off_action`（BLOCK/CONFIRM/ALLOW）に従ってフォールバックします。

---

## Skills (Plugins)

Skills はAIのドメイン知識とワークフローを拡張し、ホットロードと優先度上書きをサポートします。

デフォルトのスキャンディレクトリと優先順位：
- `~/.config/aish/skills/`（または `$AISH_CONFIG_DIR/skills`）
- `~/.claude/skills/`

パッケージ版は初回起動時にシステムレベルのSkillsをユーザーディレクトリへコピーしようとします（例：`/usr/share/aish/skills`）。

詳細：`docs/skills-guide.md`

---

## データとプライバシー

このプロジェクトは次のデータをローカルに保存します（トラブルシューティングと追跡可能性のため）：

- **ログ**：デフォルト `~/.config/aish/logs/aish.log`
- **セッション/履歴**：デフォルト `~/.local/share/aish/sessions.db`（SQLite）
- **大容量出力のオフロード**：デフォルト `~/.local/share/aish/offload/`

推奨事項：
- 実際のAPIキーをリポジトリにコミットしないでください。環境変数やシークレット管理を推奨します。
- 本番環境では、セキュリティポリシーを組み合わせてAIがアクセス可能なディレクトリ範囲を制限できます。

---

## ドキュメント

- 設定ガイド：`CONFIGURATION.md`
- クイックスタート：`QUICKSTART.md`
- Skills の使い方：`docs/skills-guide.md`
- コマンド修正メカニズム：`docs/command-interaction-correction.md`

---

## コミュニティとサポート

| Link | 説明 |
|------|-------------|
| [Official Website](https://www.aishell.ai) | プロジェクトのホームページと詳細情報 |
| [GitHub Repository](https://github.com/AI-Shell-Team/aish/) | ソースコードとIssueトラッキング |
| [GitHub Issues](https://github.com/AI-Shell-Team/aish/issues) | バグ報告 |
| [GitHub Discussions](https://github.com/AI-Shell-Team/aish/discussions) | コミュニティの議論 |
| [Discord](https://discord.com/invite/Pw2mjZt3) | コミュニティに参加 |

---

## 開発とテスト

```bash
uv sync
uv run aish
uv run pytest
```

---

## コントリビュート

ガイドラインは [CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。
---

## ライセンス

`LICENSE`（Apache 2.0）
