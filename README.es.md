![carátula](./docs/assets/readme_cover.png)

<div align="center">

**Un agente de programación de código abierto que es increíblemente rápido, seguro e independiente del proveedor de modelos.**

🚧Proyecto en etapa temprana bajo desarrollo activo — aún no está listo para producción.
⭐ Dános una estrella para seguirnos

[![Estado](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Idioma](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Origen](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Licencia](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="Vista general del proyecto" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 Tabla de Contenidos

- [Inicio Rápido](#-inicio-rápido)
- [Instalación](#-instalación)
- [Preguntas Frecuentes](#-preguntas-frecuentes)
- [Contribuir](#-contribuir)
- [Referencias](#-referencias)
- [Licencia](#-licencia)

## 📦 Instalación

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 Inicio Rápido

Si prefieres compilar desde el código fuente, usa las instrucciones a continuación.

### Construir

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Asegúrate de tener Rust instalado, se recomienda 1.75+ (a través de https://rustup.rs/).

## Preguntas Frecuentes

### ¿En qué se diferencia esto de Claude Code?

Es muy similar a Claude Code en términos de capacidad. Aquí están las diferencias clave:

- 100% open source
- No está acoplado a ningún proveedor. Devo puede ser usado con Claude, OpenAI, z.ai, Qwen, Deepseek, o incluso modelos locales. A medida que los modelos evolucionan, las brechas entre ellos se cerrarán y los precios bajarán, por lo que ser independiente del proveedor es importante.
- El soporte TUI ya está implementado.
- Construido con una arquitectura cliente/servidor. Por ejemplo, el núcleo puede ejecutarse localmente en tu máquina mientras es controlado remotamente (por ejemplo, desde una aplicación móvil), con el TUI actuando como solo uno de los muchos clientes posibles.


## 🤝 Contribuir

¡Las contribuciones son bienvenidas! Este proyecto está en su fase de diseño inicial, y hay muchas formas de ayudar:

- **Comentarios sobre arquitectura** — Revisa el diseño de los crates y sugiere mejoras
- **Discusiones RFC** — Propón nuevas ideas a través de issues
- **Documentación** — Ayuda a mejorar o traducir la documentación
- **Implementación** — Toma la implementación de crates una vez que los diseños se estabilicen

Siéntete libre de abrir un issue o enviar un pull request.

## 📄 Licencia

Este proyecto está licenciado bajo la [Licencia MIT](./LICENSE).

---

**Si encuentras útil este proyecto, por favor considera darle un ⭐**
