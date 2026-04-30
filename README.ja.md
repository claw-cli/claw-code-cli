![カバー](./docs/assets/readme_cover.png)

<div align="center">

**超高速で安全、モデルプロバイダーに依存しないオープンソースのコーディングエージェント。**

🚧早期段階のプロジェクトで活発に開発中 — まだ本番環境の準備はできていません。
⭐ スターをつけてフォローしてください

[![ステータス](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![言語](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![由来](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![ライセンス](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="プロジェクト概要" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 目次

- [クイックスタート](#-クイックスタート)
- [インストール](#-インストール)
- [よくある質問](#-よくある質問)
- [コントリビュート](#-コントリビュート)
- [参考](#-参考)
- [ライセンス](#-ライセンス)

## 📦 インストール

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 クイックスタート

ソースからビルドしたい場合は、以下の手順を使用してください。

### ビルド

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Rust がインストールされていることを確認してください。1.75+ を推奨します（https://rustup.rs/ から）。

## よくある質問

### これは Claude Code と何が違いますか？

機能面では Claude Code と非常に似ています。主な違いは次のとおりです。

- 100% オープンソース
- 特定のプロバイダーに依存しません。Devo は Claude、OpenAI、z.ai、Qwen、Deepseek、あるいはローカルモデルでも利用できます。モデルが進化するにつれて差は縮まり、価格も下がっていくため、プロバイダー非依存であることは重要です。
- TUI サポートはすでに実装済みです
- クライアント/サーバー型アーキテクチャで構築されています。たとえば、コアはローカルマシンで動作しつつ、モバイルアプリなどからリモート制御でき、TUI は複数あるクライアントの1つにすぎません。

## 🤝 コントリビュート

コントリビュートを歓迎します。このプロジェクトはまだ設計初期段階で、協力できる方法がたくさんあります。

- **アーキテクチャのフィードバック** — crate 設計をレビューして改善案を提案する
- **RFC ディスカッション** — issue を通じて新しいアイデアを提案する
- **ドキュメント** — ドキュメントの改善や翻訳を手伝う
- **実装** — 設計が安定したら実装 crate を担当する

issue を開くか pull request を送ってください。

## 📄 ライセンス

このプロジェクトは [MIT ライセンス](./LICENSE) のもとで公開されています。

---

**このプロジェクトが役に立ったら、⭐ をお願いします**
