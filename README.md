![cover](./docs/assets/readme_cover.png)

<div align="center">

**An open-source coding agent that is blazing fast, secure, and model-provider agnostic.**

🚧Early-stage project under active development — not production-ready yet.
⭐ Star us to follow 

[![Stars](https://img.shields.io/github/stars/7df-lab/devo?style=flat-square)](https://github.com/7df-lab/devo/stargazers)
[![Language](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/7df-lab/devo/pulls)
[![CI](https://img.shields.io/github/actions/workflow/status/7df-lab/devo/ci.yml?branch=main&style=flat-square)](https://github.com/7df-lab/devo/actions)
[![Release](https://img.shields.io/github/v/release/7df-lab/devo?style=flat-square)](https://github.com/7df-lab/devo/releases)


[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<table>
  <tr>
    <td width="50%" align="center">
      <strong>Explain Codebase</strong>
      <br />
      <sub>Ask Devo to quickly understand a repository.</sub>
      <br /><br />
      <img width="100%" alt="Image" src="https://github.com/user-attachments/assets/b2867013-c6e6-4a26-9956-5a8a6133b96c" />
    </td>
    <td width="50%" align="center">
      <strong>中文支持</strong>
      <br />
      <sub>用中文回复问题</sub>
      <br /><br />
      <img width="100%" alt="Image" src="https://github.com/user-attachments/assets/bd7db80b-f31c-4da5-bf35-bf919385edf3" />
    </td>
  </tr>
  <tr>
      <td width="50%" align="center">
      <strong>Deepseek V4 (Cost-Effective)</strong>
      <br />
      <sub>Fully Support, input/output token, cached rate, context window usage.</sub>
      <br /><br />
      <img width="100%" alt="Image" src="https://github.com/user-attachments/assets/35639cae-9fd7-48ff-9b0f-d43b1aff86d8" />
    </td>
    <td width="50%" align="center">
      <strong>Safety First</strong>
      <br />
      <sub>Permission control for tool call.</sub>
      <br /><br />
      <img width="100%" alt="Image" src="https://github.com/user-attachments/assets/d70e30af-3194-4dba-b1bf-c0ba869b801f" />
    </td>
  </tr>
</table>

</div>

---

## 📖 Table of Contents

- [Installation](#-installation)
- [Quick Start](#-quick-start)
- [Configuration](#%EF%B8%8F-configuration)
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
> `DEVO_HOME/config.toml` (defaults to `~/.devo/config.toml` on macOS/linux, `C:\Users\yourname\.devo\config.toml` on Windows.) or `<workspace>/.devo/config.toml`.

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
2. `DEVO_HOME/config.toml` — user-level config (defaults to `~/.devo/config.toml` on macOS/linux, `C:\Users\yourname\.devo\config.toml` on Windows.)
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
api_key = "sk-..."
base_url = "https://api.deepseek.com"
wire_api = "openai_chat_completions"

[[model_providers."api.deepseek.com".models]]
model = "deepseek-v4-pro"

[[model_providers."api.deepseek.com".models]]
model = "deepseek-v4-flash"
```

<details>
<summary>Full Config Reference (click to expand)</summary>

```toml
# ── Model Provider (required) ───────────────────────────────────
model_provider = "api.deepseek.com"          # active provider id
model = "deepseek-v4-flash"                   # active model slug
model_thinking_selection = "high"   # optional: thinking/reasoning effort
model_auto_compact_token_limit = 970000   # optional
model_context_window = 128000      # optional
disable_response_storage = false   # optional
preferred_auth_method = "apikey"   # optional: "apikey"

# ── Provider Profiles ───────────────────────────────────────────
[model_providers."api.deepseek.com"]
name = "api.deepseek.com"
base_url = "https://api.deepseek.com"
wire_api = "openai_chat_completions"   # openai_chat_completions | openai_responses | anthropic_messages
api_key = "sk-..."
default_model = "deepseek-v4-flash"  # optional

[[model_providers.openai.models]]
model = "deepseek-v4-pro"

[[model_providers.openai.models]]
model = "deepseek-v4-flash"

# ── App Settings (optional) ─────────────────────────────────────
enable_auxiliary_model = false     # optional, use a second model for safety/summaries
summary_model = "UseTurnModel"     # optional, "UseTurnModel" or "UseAxiliaryModel"
safety_policy_model = "UseAxiliaryModel" # optional
project_root_markers = [".git"] # optional

[context]
preserve_recent_turns = 3          # optional, keep last N turns un-compacted
auto_compact_percent = 97          # optional, trigger compaction at N% of context window
manual_compaction_enabled = true   # optional

[server]
listen = []                        # optional, e.g. ["stdio://", "ws://127.0.0.1:3000"]
max_connections = 32               # optional
event_buffer_size = 1024           # optional
idle_session_timeout_secs = 1800   # optional
persist_ephemeral_sessions = false # optional

[logging]
level = "info"                     # optional, trace, debug, info, warn, error
json = false                        # optional, emit JSON-formatted logs
redact_secrets_in_logs = true      # optional

[logging.file]
directory = "logs"                 # optional, relative to DEVO_HOME
filename_prefix = "devo"           # optional
rotation = "Daily"                 # optional, Never | Minutely | Hourly | Daily
max_files = 14                     # optional

[skills]
enabled = true                      # optional
user_roots = ["skills"]             # optional, dirs to scan for user skills
workspace_roots = ["skills"]        # optional, dirs to scan for workspace skills
watch_for_changes = true            # optional

[updates]
enabled = true                       # optional
check_on_startup = true              # optional
check_interval_hours = 24            # optional
### Model Catalog (`~/.devo/models.json`)
```
</details>

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


## Star us

<a href="https://www.star-history.com/?repos=7df-lab%2Fdevo&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=7df-lab/devo&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=7df-lab/devo&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=7df-lab/devo&type=date&legend=top-left" />
 </picture>
</a>

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
