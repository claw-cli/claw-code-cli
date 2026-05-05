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

## ⚙️ Configuration

Devo reads configuration from a TOML file, merged with higher-priority sources
overriding lower-priority ones:

1. Built-in defaults (compiled into the binary)
2. `DEVO_HOME/config.toml` — user-level config (defaults to `~/.devo/config.toml`)
3. `<workspace>/.devo/config.toml` — project-level config
4. CLI flags — command-line overrides

Both config files are optional. A minimal config file only needs a provider
section so devo knows which model to use. Run `devo onboard` for an interactive
setup that writes this for you.

### Minimal Config Example

```toml
# ~/.devo/config.toml
model = "deepseek-v4-flash"
model_provider = "api.deepseek.com"
model_thinking_selection = "high"

[model_providers."api.deepseek.com"]
name = "api.deepseek.com"
api_key = "sk-0b7b2422983141e5973d1fc3eccf0822"
base_url = "https://api.deepseek.com"
wire_api = "openai_chat_completions"

[[model_providers."api.deepseek.com".models]]
model = "deepseek-v4-pro"

[[model_providers."api.deepseek.com".models]]
model = "deepseek-v4-flash"
```

### Full Config Reference

```toml
# ── Model Provider (required) ───────────────────────────────────
model_provider = "openai"          # active provider id
model = "gpt-4o"                   # active model slug
model_thinking_selection = "high"   # optional: thinking/reasoning effort
model_auto_compact_token_limit = 970000   # optional
model_context_window = 128000      # optional
disable_response_storage = false   # optional
preferred_auth_method = "apikey"   # optional: "apikey"

# ── Provider Profiles ───────────────────────────────────────────
[model_providers.openai]
name = "OpenAI"
base_url = "https://api.openai.com/v1"
wire_api = "openai_chat_completions"   # openai_chat_completions | openai_responses | anthropic_messages
api_key = "sk-..."
default_model = "gpt-4o"

[[model_providers.openai.models]]
model = "gpt-4o"

[[model_providers.openai.models]]
model = "gpt-4o-mini"

# ── App Settings (optional) ─────────────────────────────────────
enable_auxiliary_model = false     # use a second model for safety/summaries
summary_model = "UseTurnModel"     # "UseTurnModel" or "UseAxiliaryModel"
safety_policy_model = "UseAxiliaryModel"
project_root_markers = [".git"]

[context]
preserve_recent_turns = 3          # keep last N turns un-compacted
auto_compact_percent = 90          # trigger compaction at N% of context window
manual_compaction_enabled = true

[server]
listen = []                        # e.g. ["stdio://", "ws://127.0.0.1:3000"]
max_connections = 32
event_buffer_size = 1024
idle_session_timeout_secs = 1800
persist_ephemeral_sessions = false

[logging]
level = "info"                     # trace, debug, info, warn, error
json = false                       # emit JSON-formatted logs
redact_secrets_in_logs = true

[logging.file]
directory = "logs"                 # relative to DEVO_HOME
filename_prefix = "devo"
rotation = "Daily"                 # Never | Minutely | Hourly | Daily
max_files = 14

[skills]
enabled = true
user_roots = ["skills"]            # dirs to scan for user skills
workspace_roots = ["skills"]       # dirs to scan for workspace skills
watch_for_changes = true

[updates]
enabled = true
check_on_startup = true
check_interval_hours = 24
```

### Model Catalog (`~/.devo/models.json`)

A separate JSON file defines available models and their capabilities. On first
run, the built-in catalog is automatically copied to `~/.devo/models.json` so
you can customize it. Models are organized by `channel` (brand/vendor).

```json
[
  {
    "slug": "deepseek-v4-pro",
    "display_name": "deepseek-v4-pro",
    "channel": "DeepSeek",
    "provider_family": "openai",
    "description": "DeepSeek v4 pro model",
    "context_window": 1000000,
    "max_tokens": 384000,
    "thinking_capability": "toggle",
    "supported_reasoning_levels": ["high", "max"],
    "base_instructions": "You are Devo, a coding agent based on DeepSeek...",
    "input_modalities": ["text"],
    "priority": 10
  }
]
```

Merge order: builtin defaults < `~/.devo/models.json` < `<workspace>/.devo/models.json`,
merged by model `slug`. You can override existing entries (e.g. change prompts or
context window) or add custom models.

The `/model` slash command in the TUI shows only models you have configured
with credentials in `config.toml`, not the full catalog.

### Environment Variables

| Variable      | Purpose                                       |
|---------------|-----------------------------------------------|
| `DEVO_HOME`   | Override the config directory (default: `~/.devo`) |

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
