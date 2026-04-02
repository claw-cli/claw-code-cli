<div align="center">

# 🦀 Claude Code Rust

**Un runtime modular para agentes extraído de Claude Code, reconstruido en Rust.**

[![Estado](https://img.shields.io/badge/estado-diseño-blue?style=flat-square)](https://github.com/)
[![Lenguaje](https://img.shields.io/badge/lenguaje-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Origen](https://img.shields.io/badge/origen-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Licencia](https://img.shields.io/badge/licencia-MIT-green?style=flat-square)](../../LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](../../README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md)

<img src="../assets/overview.svg" alt="Visión general del proyecto" width="100%" />

</div>

---

## 📖 Índice

- [Qué es este proyecto](#-qué-es-este-proyecto)
- [Por qué reconstruir en Rust](#-por-qué-reconstruir-en-rust)
- [Objetivos de diseño](#-objetivos-de-diseño)
- [Arquitectura](#-arquitectura)
- [Detalle de módulos](#-detalle-de-módulos)
- [Rust vs TypeScript](#-rust-vs-typescript)
- [Hoja de ruta](#-hoja-de-ruta)
- [Estructura del proyecto](#-estructura-del-proyecto)
- [Contribuir](#-contribuir)
- [Referencias](#-referencias)
- [Licencia](#-licencia)

## 💡 Qué es este proyecto

Este proyecto extrae las ideas centrales del runtime de agentes de [Claude Code](https://docs.anthropic.com/en/docs/claude-code) y las reorganiza en un conjunto de crates de Rust reutilizables. No es una traducción línea por línea de TypeScript: es un rediseño *clean-room* de las capacidades de las que un agente depende de verdad:

- **Message Loop** — impulsar conversaciones multironda
- **Tool Execution** — orquestar llamadas a herramientas con validación de esquemas
- **Permission Control** — autorización antes de acceder a archivos, shell o red
- **Long-running Tasks** — ejecución en segundo plano con gestión del ciclo de vida
- **Context Compaction** — mantener sesiones largas estables bajo presupuestos de tokens
- **Model Providers** — interfaz unificada para backends LLM con streaming
- **MCP Integration** — ampliar capacidades mediante Model Context Protocol

Piénsalo como un **esqueleto de runtime para agentes**:

| Capa | Rol |
|------|-----|
| **Superior** | Un CLI delgado que ensambla todos los crates |
| **Intermedia** | Runtime central: bucle de mensajes, orquestación de herramientas, permisos, tareas, abstracción del modelo |
| **Inferior** | Implementaciones concretas: herramientas integradas, cliente MCP, gestión del contexto |

> Si los límites son lo bastante claros, esto puede servir no solo a agentes de código al estilo Claude, sino a cualquier sistema de agentes que necesite una base de runtime sólida.

## 🤔 Por qué reconstruir en Rust

Claude Code tiene una ingeniería excelente, pero es un **producto completo**, no una biblioteca de runtime reutilizable. La interfaz, el runtime, los sistemas de herramientas y la gestión del estado están profundamente entrelazados. Leer el código fuente enseña mucho, pero extraer partes para reutilizarlas no es trivial.

Este proyecto pretende:

- **Descomponer** la lógica fuertemente acoplada en crates de responsabilidad única
- **Sustituir** las restricciones del runtime por límites basados en traits y enums
- **Transformar** implementaciones que «solo funcionan dentro de este proyecto» en **componentes de agente reutilizables**

## 🎯 Objetivos de diseño

1. **Runtime primero, producto después.** Priorizar cimientos sólidos para Agent loop, Tool, Task y Permission.
2. **Cada crate debe ser autoexplicativo.** Los nombres revelan la responsabilidad; las interfaces revelan los límites.
3. **Que el reemplazo sea natural.** Herramientas, proveedores de modelo, políticas de permisos y estrategias de compactación deben poder intercambiarse.
4. **Aprender de la experiencia de Claude Code** sin replicar su interfaz ni sus funciones internas.

## 🏗 Arquitectura

<div align="center">
<img src="../assets/architecture.svg" alt="Visión general de la arquitectura" width="100%" />
</div>

### Mapa de crates

| Crate | Propósito | Origen en Claude Code |
|-------|-----------|------------------------|
| `agent-core` | Modelo de mensajes, contenedor de estado, bucle principal, sesión | `query.ts`, `QueryEngine.ts`, `state/store.ts` |
| `agent-tools` | Trait de herramientas, registro, orquestación de ejecución | `Tool.ts`, `tools.ts`, capa de servicio de herramientas |
| `agent-tasks` | Ciclo de vida de tareas largas y mecanismo de notificación | `Task.ts`, `tasks.ts` |
| `agent-permissions` | Autorización de llamadas a herramientas y coincidencia de reglas | `types/permissions.ts`, `utils/permissions/` |
| `agent-provider` | Interfaz unificada del modelo, streaming, reintentos | `services/api/` |
| `agent-compact` | Recorte de contexto y control del presupuesto de tokens | `services/compact/`, `query/tokenBudget.ts` |
| `agent-mcp` | Cliente MCP, conexión, descubrimiento, reconexión | `services/mcp/` |
| `tools-builtin` | Implementaciones de herramientas integradas | `tools/` |
| `claude-cli` | Punto de entrada ejecutable, ensambla todos los crates | Capa CLI |

## 🔍 Detalle de módulos

<details>
<summary><b>agent-core</b> — La base</summary>

Gestiona cómo comienza, continúa y termina un turno de conversación. Define el modelo de mensajes unificado, el bucle principal y el estado de sesión. Es el cimiento de todo el sistema.
</details>

<details>
<summary><b>agent-tools</b> — Definición y despacho de herramientas</summary>

Define «cómo es una herramienta» y «cómo se programan las herramientas». La versión en Rust evita meter todo el contexto en un único objeto gigante: las herramientas solo reciben lo que realmente necesitan.
</details>

<details>
<summary><b>agent-tasks</b> — Runtime de tareas en segundo plano</summary>

Separar las llamadas a herramientas de las tareas del runtime es clave para soportar comandos largos, agentes en segundo plano y notificaciones de finalización que vuelven a la conversación.
</details>

<details>
<summary><b>agent-permissions</b> — Capa de autorización</summary>

Controla qué puede hacer el agente, cuándo debe preguntar al usuario y cuándo rechazar por completo. Es esencial cuando los agentes leen, escriben o ejecutan comandos.
</details>

<details>
<summary><b>agent-provider</b> — Abstracción del modelo</summary>

Protege al sistema de las diferencias entre backends de modelo. Unifica la salida en streaming, la lógica de reintentos y la recuperación ante errores.
</details>

<details>
<summary><b>agent-compact</b> — Gestión del contexto</summary>

Garantiza la estabilidad de sesiones largas. No es solo «resumir»: aplica distintos niveles de compresión y controles de presupuesto según el contexto para evitar un crecimiento sin límite.
</details>

<details>
<summary><b>agent-mcp</b> — Integración MCP</summary>

Se conecta a servicios MCP externos e incorpora herramientas, recursos y prompts remotos a la superficie unificada de capacidades.
</details>

<details>
<summary><b>tools-builtin</b> — Herramientas integradas</summary>

Implementa las herramientas más habituales, priorizando operaciones sobre archivos, comandos shell, búsqueda y edición: las operaciones básicas que necesita cualquier agente.
</details>

## ⚖️ Rust vs TypeScript

| TypeScript (Claude Code) | Enfoque en Rust |
|--------------------------|-----------------|
| Comprobaciones extensivas en runtime | Llevar comprobaciones al sistema de tipos |
| Los objetos de contexto tienden a crecer sin límite | Contexto más pequeño / límites por traits |
| Callbacks y eventos dispersos | Flujos de eventos unificados y continuos |
| Feature flags en runtime | Activación por features en tiempo de compilación cuando sea posible |
| Interfaz y runtime fuertemente acoplados | Runtime como capa independiente |

> No se trata de que Rust sea «mejor»: se trata de que encaja bien para **fijar límites claros en el runtime**. Para un sistema de agentes que evoluciona a largo plazo, ese tipo de restricciones suele ser valioso.

## 🗺 Hoja de ruta

<div align="center">
<img src="../assets/roadmap.svg" alt="Hoja de ruta" width="100%" />
</div>

### Fase 1: Ponerlo en marcha

- Configurar `agent-core`, `agent-tools`, `agent-provider`, `agent-permissions`
- Implementar herramientas básicas `Bash`, `FileRead`, `FileWrite`
- Entregar un CLI mínimo ejecutable

> **Objetivo:** Una versión básica que pueda chatear, llamar herramientas, ejecutar comandos y leer/escribir archivos.

### Fase 2: Estabilizar las sesiones

- Añadir `agent-tasks` para tareas en segundo plano y notificaciones
- Añadir `agent-compact` para sesiones largas y resultados voluminosos
- Ampliar `tools-builtin` con edición, búsqueda y capacidades de subagente

> **Objetivo:** Sesiones que duren más sin volverse frágiles por salidas demasiado grandes o tareas de larga duración.

### Fase 3: Abrir las fronteras

- Integrar `agent-mcp`
- Añadir carga de plugins / skills
- Soportar uso mediante SDK / sin interfaz para escenarios embebidos

> **Objetivo:** No solo un CLI, sino un runtime de agente completo integrable en otros sistemas.

## 📁 Estructura del proyecto

```text
rust-clw/
├── README.md                # Documentación en inglés
├── ARCHITECTURE.zh-CN.md    # Análisis de arquitectura de Claude Code (TS)
└── docs/
    ├── README.zh-CN.md      # 简体中文文档
    ├── README.ja.md         # 日本語ドキュメント
    ├── README.ko.md         # 한국어 문서
    ├── README.es.md         # Documentación en español
    ├── README.fr.md         # Documentation en français
    └── assets/
        ├── overview.svg     # Diagrama de visión general del proyecto
        ├── architecture.svg # Diagrama de arquitectura
        └── roadmap.svg      # Diagrama de la hoja de ruta
```

> Cuando los crates estén en el repositorio, esto crecerá hasta un workspace de Rust completo.

## 🤝 Contribuir

¡Las contribuciones son bienvenidas! Este proyecto está en fase inicial de diseño y hay muchas formas de ayudar:

- **Retroalimentación de arquitectura** — Revisar el diseño de los crates y proponer mejoras
- **Discusiones tipo RFC** — Plantear ideas nuevas mediante issues
- **Documentación** — Mejorar o traducir la documentación
- **Implementación** — Implementar crates cuando los diseños se estabilicen

No dudes en abrir un issue o enviar un pull request.

## 📚 Referencias

- [ARCHITECTURE.zh-CN.md](../../ARCHITECTURE.zh-CN.md) — Desglose detallado de la arquitectura TypeScript de Claude Code
- [Documentación oficial de Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- [Model Context Protocol](https://modelcontextprotocol.io/)

## 📄 Licencia

Este proyecto está bajo la [licencia MIT](../../LICENSE).

---

<div align="center">

**Si este proyecto te resulta útil, considera darle una ⭐**

</div>
