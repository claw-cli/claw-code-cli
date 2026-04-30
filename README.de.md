![Titelbild](./docs/assets/readme_cover.png)

<div align="center">

**Ein Open-Source-Coding-Agent, der unglaublich schnell, sicher und modellanbieterunabhängig ist.**

🚧Frühes Projekt in aktiver Entwicklung — noch nicht produktionsreif.
⭐ Sternen Sie uns, um zu folgen

[![Status](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Sprache](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Herkunft](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Lizenz](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="Projektübersicht" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 Inhaltsverzeichnis

- [Schnellstart](#-schnellstart)
- [Installation](#-installation)
- [Häufig gestellte Fragen](#-häufig-gestellte-fragen)
- [Mitwirken](#-mitwirken)
- [Referenzen](#-referenzen)
- [Lizenz](#-lizenz)

## 📦 Installation

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 Schnellstart

Falls Sie es vorziehen, aus dem Quellcode zu bauen, verwenden Sie die folgenden Anweisungen.

### Bauen

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Stellen Sie sicher, dass Rust installiert ist, Version 1.75+ wird empfohlen (über https://rustup.rs/).

## Häufig gestellte Fragen

### Wie unterscheidet sich dies von Claude Code?

Es ist Claude Code in Bezug auf Fähigkeiten sehr ähnlich. Hier sind die wichtigsten Unterschiede:

- 100% open source
- Nicht an einen bestimmten Anbieter gekoppelt. Devo kann mit Claude, OpenAI, z.ai, Qwen, Deepseek oder sogar lokalen Modellen verwendet werden. Da sich Modelle weiterentwickeln, werden die Lücken zwischen ihnen schrumpfen und die Preise sinken, daher ist Anbieterunabhängigkeit wichtig.
- TUI-Unterstützung ist bereits implementiert.
- Auf Client/Server-Architektur aufgebaut. Beispielsweise kann der Kern lokal auf Ihrem Computer laufen, während er ferngesteuert wird (z.B. von einer mobilen App), wobei die TUI nur einer von vielen möglichen Clients ist.


## 🤝 Mitwirken

Beiträge sind willkommen! Dieses Projekt befindet sich in einer frühen Designphase, und es gibt viele Möglichkeiten zu helfen:

- **Architektur-Feedback** — Überprüfen Sie das Crate-Design und schlagen Sie Verbesserungen vor
- **RFC-Diskussionen** — Schlagen Sie neue Ideen über Issues vor
- **Dokumentation** — Helfen Sie bei der Verbesserung oder Übersetzung der Dokumentation
- **Implementierung** — Übernehmen Sie Crate-Implementierung, sobald die Designs stabilisiert sind

Sie können gerne ein Issue öffnen oder einen Pull Request einreichen.

## 📄 Lizenz

Dieses Projekt ist unter der [MIT-Lizenz](./LICENSE) lizenziert.

---

**Wenn Sie dieses Projekt nützlich finden, erwägen Sie bitte, einen ⭐ zu vergeben**
