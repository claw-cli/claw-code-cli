<div align="center">

# 🦀 Claw RS

**A modular agent runtime extracted from Claude Code, rebuilt in Rust, and still in active development.**

[![Status](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Language](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Origin](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![License](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./docs/i18n/README.zh-CN.md) | [日本語](./docs/i18n/README.ja.md) | [한국어](./docs/i18n/README.ko.md) | [Español](./docs/i18n/README.es.md) | [Français](./docs/i18n/README.fr.md)

🚧Early-stage project under active development — not production-ready yet.

⭐ Star us to follow along — a usable version is coming within a month!

<img src="./docs/assets/overview.svg" alt="Project Overview" width="100%" />

</div>

---

## 📖 Table of Contents

- [What is This](#-what-is-this)
- [Quick Start](#-quick-start)
- [Why Rebuild in Rust](#-why-rebuild-in-rust)
- [Design Goals](#-design-goals)
- [Roadmap](#-roadmap)
- [Contributing](#-contributing)
- [References](#-references)
- [License](#-license)

## 💡 What is This

This project extracts the core runtime ideas from [Claude Code](https://docs.anthropic.com/en/docs/claude-code) and reorganizes them into a set of reusable Rust crates.

Think of it as an **agent runtime skeleton**:

| Layer | Role |
|-------|------|
| **Top** | A thin CLI that assembles all crates |
| **Middle** | Core runtime: message loop, tool orchestration, permissions, tasks, model abstraction |
| **Bottom** | Concrete implementations: built-in tools, MCP client, context management |

> If the boundaries are clean enough, this can serve not only Claude-style coding agents, but any agent system that needs a solid runtime foundation.

## 🚀 Quick Start

### Prerequisites

- **Rust** 1.75+ ([install](https://rustup.rs/))
- **Model backend** — one of the following:
  - [Ollama](https://ollama.com/) (recommended for local development)
  - Anthropic API key

### Build

```bash
git clone <repo-url> && cd rust-clw
cargo build
```

### Run with Ollama (local, no API key needed)

Make sure Ollama is running and has a model pulled:

```bash
# Pull a model (only needed once)
ollama pull qwen3.5:9b

# Single query
cargo run -- --provider ollama -m "qwen3.5:9b" -q "list files in the current directory"

# Interactive REPL
cargo run -- --provider ollama -m "qwen3.5:9b"
```

Any model with tool-calling support works. Larger models produce better tool-use results:

```bash
cargo run -- --provider ollama -m "qwen3.5:27b" -q "read Cargo.toml and summarize the workspace"
```

### Run with Anthropic API

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
cargo run -- -q "list files in the current directory"
```

### CLI Options

```text
Usage: clawcr [OPTIONS]

Options:
  -m, --model <MODEL>          Model name (default: auto per provider)
  -s, --system <SYSTEM>        System prompt
  -p, --permission <MODE>      Permission mode: auto, interactive, deny
  -q, --query <QUERY>          Single query (non-interactive), omit for REPL
      --provider <PROVIDER>    Provider: anthropic, ollama, openai, stub
      --ollama-url <URL>       Ollama server URL (default: http://localhost:11434)
      --max-turns <N>          Max turns per conversation (default: 100)
```

### Supported Providers

| Provider | Backend | How to activate |
|----------|---------|-----------------|
| `ollama` | Ollama (local) | `--provider ollama` or auto when no `ANTHROPIC_API_KEY` |
| `anthropic` | Anthropic API | Set `ANTHROPIC_API_KEY` env var |
| `openai` | Any OpenAI-compatible API | `--provider openai` + `OPENAI_BASE_URL` |
| `stub` | No real model (for testing) | `--provider stub` |

## 🤔 Why Rebuild in Rust

Claude Code has excellent engineering quality, but it's a **complete product**, not a reusable runtime library. UI, runtime, tool systems, and state management are deeply intertwined. Reading the source teaches a lot, but extracting parts for reuse is nontrivial.

This project aims to:

- **Decompose** tightly coupled logic into single-responsibility crates
- **Replace** runtime constraints with trait and enum boundaries
- **Transform** "only works inside this project" implementations into **reusable agent components**

## 🎯 Design Goals

1. **Runtime first, product later.** Prioritize solid foundations for Agent loop, Tool, Task, and Permission.
2. **Each crate should be self-explanatory.** Names reveal responsibility, interfaces reveal boundaries.
3. **Make replacement natural.** Tools, model providers, permission policies, and compaction strategies should all be swappable.
4. **Learn from Claude Code's experience** without replicating its UI or internal features.

## 🗺 Roadmap

<div align="center">
<img src="./docs/assets/roadmap.svg" alt="Roadmap" width="100%" />
</div>


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

<div align="center">

**If you find this project useful, please consider giving it a ⭐**

</div>
