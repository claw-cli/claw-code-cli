![封面](./docs/assets/readme_cover.png)

<div align="center">

**一個開源程式碼代理，極其快速、安全且與模型提供商無關。**

🚧早期專案正在積極開發中 — 尚未準備好投入生產。
⭐ 點星關注我們

[![狀態](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![語言](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![來源](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![授權](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![歡迎 PR](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="專案概覽" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 目錄

- [快速開始](#-快速開始)
- [安裝](#-安裝)
- [常見問題](#-常見問題)
- [參與貢獻](#-參與貢獻)
- [參考](#-參考)
- [授權](#-授權)

## 📦 安裝

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 快速開始

如果你更想從原始碼建置，可以使用下面的說明。

### 建置

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> 確保已安裝 Rust，推薦 1.75+（透過 https://rustup.rs/ 安裝）。

## 常見問題

### 這和 Claude Code 有什麼不同？

在能力上，它和 Claude Code 非常相似。主要差異如下：

- 100% 開源
- 不綁定任何供應商。Devo 可以搭配 Claude、OpenAI、z.ai、Qwen、Deepseek，甚至本地模型使用。隨著模型持續演進，差距會縮小，價格也會下降，因此保持 provider 無關非常重要。
- TUI 支援已實現
- 採用客戶端/伺服器架構。例如，核心可以在你的本機執行，同時由遠端控制（比如透過手機 App 操作），而 TUI 只是眾多客戶端之一。

## 🤝 參與貢獻

歡迎貢獻！這個專案仍處於早期設計階段，有很多方式可以參與：

- **架構回饋** — 審查 crate 設計並提出改善建議
- **RFC 討論** — 透過 issue 提出新想法
- **文件** — 協助改善或翻譯文件
- **實作** — 等設計穩定後，協助推進 crate 實作

歡迎直接開 issue 或提交 pull request。

## 📄 授權

本專案採用 [MIT 授權](./LICENSE)。

---

**如果你覺得這個專案有幫助，歡迎給它一個 ⭐**
