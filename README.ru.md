![обложка](./docs/assets/readme_cover.png)

<div align="center">

**Открытый агент для программирования, который работает очень быстро, безопасен и не зависит от конкретного поставщика моделей.**

🚧Проект на ранней стадии активной разработки — пока не готов к production.
⭐ Поставьте звезду, чтобы следить за проектом

[![Статус](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Язык](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Источник](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Лицензия](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="Обзор проекта" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 Содержание

- [Быстрый старт](#-быстрый-старт)
- [Установка](#-установка)
- [Часто задаваемые вопросы](#-часто-задаваемые-вопросы)
- [Участие в разработке](#-участие-в-разработке)
- [Ссылки](#-ссылки)
- [Лицензия](#-лицензия)

## 📦 Установка

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 Быстрый старт

Если вы предпочитаете собрать из исходников, используйте инструкции ниже.

### Сборка

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Убедитесь, что Rust установлен; рекомендуется версия 1.75+ (через https://rustup.rs/).

## Часто задаваемые вопросы

### Чем это отличается от Claude Code?

По возможностям проект очень похож на Claude Code. Основные отличия:

- 100% open source
- Не привязан к одному провайдеру. Devo можно использовать с Claude, OpenAI, z.ai, Qwen, Deepseek или даже с локальными моделями. По мере развития моделей разрыв между ними будет сокращаться, а стоимость снижаться, поэтому независимость от провайдера важна.
- TUI уже реализован
- Построен на клиент-серверной архитектуре. Например, ядро может работать локально на вашем компьютере и при этом управляться удалённо, например из мобильного приложения, а TUI будет лишь одним из возможных клиентов

## 🤝 Участие в разработке

Мы приветствуем вклад в проект. Он находится на ранней стадии проектирования, и помочь можно разными способами:

- **Обратная связь по архитектуре** — изучите дизайн крейтов и предложите улучшения
- **Обсуждение RFC** — предлагайте новые идеи через issues
- **Документация** — помогайте улучшать или переводить документацию
- **Реализация** — подключайтесь к реализации крейтов, когда дизайн стабилизируется

Не стесняйтесь открывать issue или отправлять pull request.

## 📄 Лицензия

Проект распространяется по [лицензии MIT](./LICENSE).

---

**Если проект оказался полезным, поставьте ему ⭐**
