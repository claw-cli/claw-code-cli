<div align="center">

# 🦀 Claude Code Rust

**Claude Codeのコアランタイムのアイデアを抽出し、Rustで再設計した再利用可能なcrateセット。**

[![ステータス](https://img.shields.io/badge/ステータス-設計中-blue?style=flat-square)](https://github.com/)
[![言語](https://img.shields.io/badge/言語-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![由来](https://img.shields.io/badge/由来-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![ライセンス](https://img.shields.io/badge/ライセンス-MIT-green?style=flat-square)](../../LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](../../README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md)

<img src="../assets/overview.svg" alt="プロジェクト概要" width="100%" />

</div>

---

## 📖 目次

- [プロジェクトについて](#-プロジェクトについて)
- [なぜRustで再構築するのか](#-なぜrustで再構築するのか)
- [設計目標](#-設計目標)
- [アーキテクチャ](#-アーキテクチャ)
- [モジュール詳細](#-モジュール詳細)
- [Rust vs TypeScript](#-rust-vs-typescript)
- [ロードマップ](#-ロードマップ)
- [ディレクトリ構成](#-ディレクトリ構成)
- [コントリビュート](#-コントリビュート)
- [参考資料](#-参考資料)
- [ライセンス](#-ライセンス)

## 💡 プロジェクトについて

このプロジェクトは [Claude Code](https://docs.anthropic.com/en/docs/claude-code) のTypeScriptを一行ずつ翻訳するものではありません。エージェントが本当に依存するコア機能を整理し、再利用可能なRust crateセットとして再設計します：

- **メッセージループ** — マルチターン会話の駆動
- **ツール実行** — スキーマ検証付きのツール呼び出しオーケストレーション
- **権限制御** — ファイル/Shell/ネットワークアクセス前の認可
- **長期実行タスク** — ライフサイクル管理付きのバックグラウンド実行
- **コンテキスト圧縮** — トークン予算内での長いセッションの安定維持
- **モデルプロバイダー** — ストリーミングLLMバックエンドの統一インターフェース
- **MCP統合** — Model Context Protocolによる機能拡張

これは **エージェントランタイムのスケルトン** として考えることができます：

| レイヤー | 役割 |
|---------|------|
| **上層** | 全crateを組み立てる薄いCLI |
| **中間** | コアランタイム：メッセージループ、ツールオーケストレーション、権限、タスク、モデル抽象化 |
| **下層** | 具体実装：組み込みツール、MCPクライアント、コンテキスト管理 |

> 境界が十分にクリーンであれば、Claude風のコーディングエージェントだけでなく、あらゆるエージェントシステムの基盤として機能できます。

## 🤔 なぜRustで再構築するのか

Claude Codeは優れたエンジニアリング品質を持っていますが、本質的には**完全な製品**であり、再利用可能なランタイムライブラリではありません。UI、ランタイム、ツールシステム、状態管理が密結合しています。

このプロジェクトの目的：

- **分解** — 密結合なロジックを単一責任のcrateに分割
- **抽象化** — ランタイム制約をtraitとenum境界に置き換え
- **再利用** — 「このプロジェクト内でのみ動作」する実装を「他のエージェントから再利用可能」なコンポーネントに変換

## 🎯 設計目標

1. **ランタイム優先、プロダクト後。** Agent loop、Tool、Task、Permissionの基盤を優先的に固める。
2. **各crateは自己説明的であるべき。** 名前が責任を、インターフェースが境界を明らかにする。
3. **置換を自然にする。** ツール、モデルプロバイダー、権限ポリシー、圧縮戦略はすべて交換可能であるべき。
4. **Claude Codeの経験を活かしつつ、** UIや内部機能をそのまま複製しない。

## 🏗 アーキテクチャ

<div align="center">
<img src="../assets/architecture.svg" alt="アーキテクチャ概要" width="100%" />
</div>

### Crate マップ

| Crate | 目的 | Claude Codeの対応レイヤー |
|-------|------|--------------------------|
| `agent-core` | メッセージモデル、状態コンテナ、メインループ、セッション | `query.ts`、`QueryEngine.ts`、`state/store.ts` |
| `agent-tools` | ツールtrait、レジストリ、実行オーケストレーション | `Tool.ts`、`tools.ts`、ツールサービス層 |
| `agent-tasks` | 長期タスクのライフサイクルと通知メカニズム | `Task.ts`、`tasks.ts` |
| `agent-permissions` | ツール呼び出し認可とルールマッチング | `types/permissions.ts`、`utils/permissions/` |
| `agent-provider` | 統一モデルインターフェース、ストリーミング、リトライ | `services/api/` |
| `agent-compact` | コンテキストトリミングとトークン予算制御 | `services/compact/`、`query/tokenBudget.ts` |
| `agent-mcp` | MCPクライアント、接続、ディスカバリ、再接続 | `services/mcp/` |
| `tools-builtin` | 組み込みツール実装 | `tools/` |
| `claude-cli` | 実行可能エントリーポイント、全crateの組み立て | CLI層 |

## 🔍 モジュール詳細

<details>
<summary><b>agent-core</b> — システムの基盤</summary>

会話ターンの開始、継続、停止を管理。統一メッセージモデル、メインループ、セッション状態を定義。システム全体の基盤です。
</details>

<details>
<summary><b>agent-tools</b> — ツール定義とディスパッチ</summary>

「ツールがどのように見えるか」と「ツールがどのようにスケジュールされるか」を定義。Rust版は全コンテキストを一つの巨大なオブジェクトに詰め込むことを避けます。
</details>

<details>
<summary><b>agent-tasks</b> — バックグラウンドタスクランタイム</summary>

ツール呼び出しとランタイムタスクを分離することで、長いコマンド、バックグラウンドエージェント、完了通知のフィードバックをサポートします。
</details>

<details>
<summary><b>agent-permissions</b> — 認可レイヤー</summary>

エージェントが何ができるか、いつユーザーに確認すべきか、いつ拒否すべきかを制御。ファイルの読み書きやコマンド実行を行うエージェントには不可欠です。
</details>

<details>
<summary><b>agent-provider</b> — モデル抽象化</summary>

異なるモデルバックエンド間の差異を吸収。ストリーミング出力、リトライロジック、エラー回復を統一します。
</details>

<details>
<summary><b>agent-compact</b> — コンテキスト管理</summary>

単なる「要約」ではなく、コンテキストに応じて異なる圧縮レベルと予算制御を適用し、無制限な成長を防ぎます。
</details>

<details>
<summary><b>agent-mcp</b> — MCP統合</summary>

外部MCPサービスに接続し、リモートツール、リソース、プロンプトを統一された能力面に組み込みます。
</details>

<details>
<summary><b>tools-builtin</b> — 組み込みツール</summary>

最も一般的に使用されるツールを実装。ファイル操作、シェルコマンド、検索、編集を優先します。
</details>

## ⚖️ Rust vs TypeScript

| TypeScript（Claude Code） | Rust アプローチ |
|---------------------------|----------------|
| 広範なランタイムチェック | チェックを型システムに移行 |
| 肥大化しがちなコンテキストオブジェクト | より小さなcontext / trait境界 |
| 分散したcallbackとevent | 統一された連続イベントストリーム |
| ランタイムフィーチャーフラグ | 可能な限りコンパイル時のフィーチャーゲーティング |
| UIとランタイムの密結合 | ランタイムを独立レイヤーとして分離 |

> Rustが「優れている」というわけではなく、**ランタイム境界を固定する**のに適しているということです。長期進化するエージェントシステムにとって、このような制約は通常価値があります。

## 🗺 ロードマップ

<div align="center">
<img src="../assets/roadmap.svg" alt="ロードマップ" width="100%" />
</div>

### Phase 1：まず動かす

- `agent-core`、`agent-tools`、`agent-provider`、`agent-permissions`を構築
- 基本的な `Bash`、`FileRead`、`FileWrite` ツールを実装
- 最小限の実行可能なCLIを提供

> **目標：** 対話、ツール呼び出し、コマンド実行、ファイル読み書きができる基本バージョン。

### Phase 2：セッションを安定させる

- `agent-tasks`を追加、バックグラウンドタスクと通知をサポート
- `agent-compact`を追加、長いセッションと大きな結果の処理
- `tools-builtin`を拡張、編集・検索・サブエージェント機能を追加

> **目標：** 出力の肥大化や長時間タスクによる不安定さのない持続的なセッション。

### Phase 3：境界を開く

- `agent-mcp`を統合
- プラグイン/スキルローディング機能を追加
- 組み込みシナリオ向けのSDK / headless使用をサポート

> **目標：** CLIだけでなく、他のシステムに統合可能な完全なエージェントランタイム。

## 📁 ディレクトリ構成

```text
rust-clw/
├── README.md                # English documentation
├── ARCHITECTURE.zh-CN.md    # Claude Code (TS) アーキテクチャ分析
└── docs/
    ├── README.zh-CN.md      # 简体中文文档
    ├── README.ja.md         # 日本語ドキュメント
    ├── README.ko.md         # 한국어 문서
    ├── README.es.md         # Documentación en español
    ├── README.fr.md         # Documentation en français
    └── assets/
        ├── overview.svg     # プロジェクト概要図
        ├── architecture.svg # アーキテクチャ図
        └── roadmap.svg      # ロードマップ図
```

> 各crateが実装されたら、完全なRust workspaceに展開されます。

## 🤝 コントリビュート

コントリビュートを歓迎します！プロジェクトは初期設計段階にあり、参加方法はたくさんあります：

- **アーキテクチャフィードバック** — crate設計のレビューと改善提案
- **RFCディスカッション** — issueを通じた新しいアイデアの提案
- **ドキュメント改善** — ドキュメントの改善や翻訳
- **コード実装** — 設計が安定した後のcrate実装への参加

お気軽にissueを開くか、pull requestを送信してください。

## 📚 参考資料

- [ARCHITECTURE.zh-CN.md](../../ARCHITECTURE.zh-CN.md) — Claude Code TypeScriptアーキテクチャの詳細分析
- [Claude Code 公式ドキュメント](https://docs.anthropic.com/en/docs/claude-code)
- [Model Context Protocol](https://modelcontextprotocol.io/)

## 📄 ライセンス

このプロジェクトは [MIT ライセンス](../../LICENSE) の下で公開されています。

---

<div align="center">

**このプロジェクトが役に立ったら、⭐をお願いします**

</div>
