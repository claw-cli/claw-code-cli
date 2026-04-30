![capa](./docs/assets/readme_cover.png)

<div align="center">

**Um agente de programação open source que é extremamente rápido, seguro e independente do provedor de modelos.**

🚧Projeto em fase inicial em desenvolvimento ativo — ainda não está pronto para produção.
⭐ Dê uma estrela para nos seguir

[![Status](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Linguagem](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Origem](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Licença](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="Visão geral do projeto" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 Índice

- [Início Rápido](#-início-rápido)
- [Instalação](#-instalação)
- [Perguntas Frequentes](#-perguntas-frequentes)
- [Contribuindo](#-contribuindo)
- [Referências](#-referências)
- [Licença](#-licença)

## 📦 Instalação

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 Início Rápido

Se preferir compilar a partir do código-fonte, use as instruções abaixo.

### Compilar

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Certifique-se de que o Rust está instalado. Recomenda-se a versão 1.75+ (via https://rustup.rs/).

## Perguntas Frequentes

### Em que isto é diferente do Claude Code?

É muito semelhante ao Claude Code em termos de capacidade. Aqui estão as principais diferenças:

- 100% open source
- Não está acoplado a nenhum provedor. Devo pode ser usado com Claude, OpenAI, z.ai, Qwen, Deepseek ou até modelos locais. À medida que os modelos evoluem, as lacunas entre eles se fecham e os preços caem, então ser independente de provedor é importante.
- Suporte TUI já está implementado.
- Construído com uma arquitetura cliente/servidor. Por exemplo, o núcleo pode rodar localmente na sua máquina enquanto é controlado remotamente (por exemplo, por um app móvel), com o TUI sendo apenas um dos muitos clientes possíveis.

## 🤝 Contribuindo

Contribuições são bem-vindas! Este projeto está na sua fase inicial de design, e há muitas formas de ajudar:

- **Feedback de arquitetura** — Revise o design dos crates e sugira melhorias
- **Discussões RFC** — Proponha novas ideias por meio de issues
- **Documentação** — Ajude a melhorar ou traduzir a documentação
- **Implementação** — Assuma a implementação dos crates quando os designs estiverem mais estáveis

Sinta-se à vontade para abrir um issue ou enviar um pull request.

## 📄 Licença

Este projeto está licenciado sob a [Licença MIT](./LICENSE).

---

**Se você achar este projeto útil, considere dar uma ⭐**
