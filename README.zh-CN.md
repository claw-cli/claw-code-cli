![封面](./docs/assets/readme_cover.png)

<div align="center">

**一个开源编程代理，极其快速、安全且与模型提供商无关。**

🚧早期项目正在积极开发中 — 尚未准备好投入生产。
⭐ 点星关注我们

[![状态](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![语言](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![来源](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![许可](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![欢迎 PR](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="项目概览" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 目录

- [安装](#-安装)
- [快速开始](#-快速开始)
- [常见问题](#-常见问题)
- [参与贡献](#-参与贡献)
- [参考](#-参考)
- [许可证](#-许可证)

## 📦 安装

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 快速开始

如果你更想从源码构建，可以使用下面的说明。

### 构建

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> 确保已安装 Rust，推荐 1.75+（通过 https://rustup.rs/ 安装）。

## 常见问题

### 这和 Claude Code 有什么不同？

在能力上，它和 Claude Code 非常相似。主要区别如下：

- 100% 开源
- 不绑定任何提供商。Devo 可以配合 Claude、OpenAI、z.ai、Qwen、Deepseek，甚至本地模型使用。随着模型不断演进，差距会缩小，价格也会下降，因此保持 provider 无关性很重要。
- TUI 支持已实现
- 采用客户端/服务器架构。例如，核心可以在本机运行，同时由远程控制（比如从移动应用控制），而 TUI 只是众多客户端之一。

## 🤝 参与贡献

欢迎贡献！这个项目还处于早期设计阶段，有很多方式可以参与：

- **架构反馈** — 审查 crate 设计并提出改进建议
- **RFC 讨论** — 通过 issue 提出新想法
- **文档** — 帮助改进或翻译文档
- **实现** — 设计稳定后参与实现 crate

欢迎随时提 issue 或提交 pull request。

## 📄 许可

本项目采用 [MIT 许可证](./LICENSE)。

---

**如果这个项目对你有帮助，欢迎点个 ⭐**
