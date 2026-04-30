![cover](./docs/assets/readme_cover.png)

<div align="center">

**An open-source coding agent that is blazing fast, secure, and model-provider agnostic.**

🚧Early-stage project under active development — not production-ready yet.
⭐ Star us to follow 

[![Status](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Language](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Origin](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![License](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="Project Overview" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 Table of Contents

- [Installation](#-installation)
- [Quick Start](#-quick-start)
- [FAQ](#-faq)
- [Contributing](#-contributing)
- [References](#-references)
- [License](#-license)

## 📦 Installation

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

> [!TIP]
> `devo` can check for newer GitHub releases on startup and print the matching
> upgrade command. You can disable or tune this with the `[updates]` section in
> `DEVO_HOME/config.toml` or `<workspace>/.devo/config.toml`.

## 🚀 Quick Start

If you prefer to build from source, use the instructions below.

### Build

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Make sure you have Rust installed, 1.75+ recommended (via https://rustup.rs/).

## FAQ

### How is this different from Claude Code?

It's very similar to Claude Code in terms of capability. Here are the key differences:

- 100% open source
- Not coupled to any provider. Devo can be used with Claude, OpenAI, z.ai, Qwen, Deepseek, or even local models. As models evolve, the gaps between them will close and pricing will drop, so being provider-agnostic is important.
- TUI support is already implemented.
- Built with a client/server architecture. For example, the core can run locally on your machine while being controlled remotely (e.g., from a mobile app), with the TUI acting as just one of many possible clients.


## 🤝 Contributing

Contributions are welcome! This project is in its early design phase, and there are many ways to help:

- **Architecture feedback** — Review the crate design and suggest improvements
- **RFC discussions** — Propose new ideas via issues
- **Documentation** — Help improve or translate documentation
- **Implementation** — Pick up crate implementation once designs stabilize

Please feel free to open an issue or submit a pull request.

## 📄 License

This project is licensed under the [MIT License](./LICENSE).

---

**If you find this project useful, please consider giving it a ⭐**
