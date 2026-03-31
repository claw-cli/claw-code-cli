<div align="center">

# 🦀 Claude Code Rust

**从 Claude Code 中提取核心运行时思路，用 Rust 重新组织成可复用的 crate。**

[![状态](https://img.shields.io/badge/状态-设计中-blue?style=flat-square)](https://github.com/)
[![语言](https://img.shields.io/badge/语言-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![来源](https://img.shields.io/badge/来源-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![许可](https://img.shields.io/badge/许可-MIT-green?style=flat-square)](../LICENSE)
[![欢迎 PR](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](../README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md)

<img src="./assets/overview.svg" alt="项目总览" width="100%" />

</div>

---

## 📖 目录

- [这个项目是什么](#-这个项目是什么)
- [为什么要用 Rust 重做](#-为什么要用-rust-重做)
- [设计目标](#-设计目标)
- [架构总览](#-架构总览)
- [模块详解](#-模块详解)
- [Rust vs TypeScript](#-rust-vs-typescript)
- [实现路线](#-实现路线)
- [目录结构](#-目录结构)
- [参与贡献](#-参与贡献)
- [参考资料](#-参考资料)
- [许可证](#-许可证)

## 💡 这个项目是什么

这个项目不是把 Claude Code 的 TypeScript 逐行翻译成 Rust，而是把 agent 真正依赖的核心能力整理清楚，重新设计成一组可复用的 Rust crate：

- **消息循环** — 驱动多轮对话
- **工具执行** — 带 schema 校验的工具调度
- **权限控制** — 文件/Shell/网络访问前的授权
- **长任务管理** — 后台执行与生命周期管理
- **上下文压缩** — 在 token 预算内保持长会话稳定
- **模型接入** — 统一的流式 LLM 后端接口
- **MCP 扩展** — 通过 Model Context Protocol 扩展能力

可以把它理解成一个 **agent 运行时骨架**：

| 层级 | 职责 |
|------|------|
| **上层** | 一个很薄的 CLI，负责组装各个 crate |
| **中间** | 核心运行时：消息循环、工具编排、权限、任务、模型抽象 |
| **底层** | 具体实现：内置工具、MCP 客户端、上下文治理 |

> 如果边界划分得足够清晰，它不只能服务 Claude 风格的 coding agent，也可以作为其他 agent 系统的基础设施。

## 🤔 为什么要用 Rust 重做

Claude Code 的工程质量很高，但它本质上是一个**完整产品**，而不是一个容易复用的 runtime library。UI、运行时、工具系统和状态管理交织在一起。读源码能学到很多，但想把其中一部分单独拿出来复用并不轻松。

这个项目想做的是：

- **拆解** — 把大块耦合逻辑拆成职责单一的 crate
- **抽象** — 把依赖运行时约束的部分改成 trait 和 enum 边界
- **复用** — 把"只能在这个项目里工作"的实现，变成"可以被别的 agent 复用"的组件

## 🎯 设计目标

1. **先抽象运行时，再补齐产品层。** 优先把 Agent loop、Tool、Task、Permission 做扎实。
2. **每个 crate 都要能单独理解。** 看名字能猜到职责，读接口能知道边界。
3. **让替换变得自然。** 工具、模型提供方、权限策略、压缩策略都应该能按需替换。
4. **保留 Claude Code 的经验，但不照着 UI 和内部 feature 复刻。**

## 🏗 架构总览

<div align="center">
<img src="./assets/architecture.svg" alt="架构总览" width="100%" />
</div>

### Crate 对照表

| Crate | 作用 | 来自 Claude Code 的哪一层 |
|-------|------|---------------------------|
| `agent-core` | 消息模型、状态容器、主循环、会话封装 | `query.ts`、`QueryEngine.ts`、`state/store.ts` |
| `agent-tools` | 工具 trait、注册表、执行编排 | `Tool.ts`、`tools.ts`、工具服务层 |
| `agent-tasks` | 长任务生命周期和通知机制 | `Task.ts`、`tasks.ts` |
| `agent-permissions` | 工具调用授权与规则匹配 | `types/permissions.ts`、`utils/permissions/` |
| `agent-provider` | 模型统一接口、流式处理、重试 | `services/api/` |
| `agent-compact` | 上下文裁剪与 token 预算控制 | `services/compact/`、`query/tokenBudget.ts` |
| `agent-mcp` | MCP 客户端、连接、发现与重连 | `services/mcp/` |
| `tools-builtin` | 内置工具实现 | `tools/` |
| `claude-cli` | 可执行入口，负责组装全部 crate | CLI 层 |

## 🔍 模块详解

<details>
<summary><b>agent-core</b> — 系统底座</summary>

负责一轮对话怎么开始、怎么继续、什么时候停止。定义统一的消息模型、主循环和会话状态，是整个系统的底座。
</details>

<details>
<summary><b>agent-tools</b> — 工具定义与调度</summary>

负责"工具长什么样"和"工具如何被调度"。Rust 版会避免把所有上下文都塞进一个巨大的对象里，而是按职责拆开，让工具只拿到它真正需要的部分。
</details>

<details>
<summary><b>agent-tasks</b> — 后台任务运行时</summary>

只有把 tool call 和 runtime task 分开，才容易支持长命令、后台 agent 和任务完成后的通知回灌。
</details>

<details>
<summary><b>agent-permissions</b> — 授权层</summary>

负责控制 agent 能做什么、什么时候必须问用户、什么时候直接拒绝。只要 agent 会读文件、写文件或执行命令，这一层就绕不开。
</details>

<details>
<summary><b>agent-provider</b> — 模型抽象</summary>

屏蔽不同模型后端的差异，统一流式输出、重试和错误恢复逻辑。
</details>

<details>
<summary><b>agent-compact</b> — 上下文治理</summary>

不是简单做"摘要"，而是根据场景做不同层次的压缩和预算控制，避免上下文无限增长。
</details>

<details>
<summary><b>agent-mcp</b> — MCP 集成</summary>

接入外部 MCP 服务，把远程 tool、resource、prompt 纳入统一的能力面。
</details>

<details>
<summary><b>tools-builtin</b> — 内置工具</summary>

实现最常用的内置工具，优先级放在文件操作、命令执行、搜索和编辑这些 agent 的基本操作上。
</details>

## ⚖️ Rust vs TypeScript

| TypeScript 版常见做法 | Rust 版对应思路 |
|----------------------|----------------|
| 大量运行时判断 | 尽量前移到类型系统里 |
| 容易膨胀的上下文对象 | 拆成更小的 context / trait 边界 |
| 分散的 callback 和 event | 尽量统一成更连续的事件流 |
| 运行时 feature 开关 | 能在编译期裁剪的尽量编译期处理 |
| UI 和运行时耦合较深 | 优先把 runtime 独立出来 |

> 这不是说 Rust 一定更"高级"，而是它更适合把运行时边界钉死。对于一个长期演进的 agent 系统来说，这样的约束通常是有价值的。

## 🗺 实现路线

<div align="center">
<img src="./assets/roadmap.svg" alt="实现路线" width="100%" />
</div>

### Phase 1：先跑起来

- 建立 `agent-core`、`agent-tools`、`agent-provider`、`agent-permissions`
- 先实现最基础的 `Bash`、`FileRead`、`FileWrite`
- 提供一个最小可运行的 CLI

> **目标：** 先得到一个能对话、能调工具、能执行命令和读写文件的基础版本。

### Phase 2：把会话做稳

- 加入 `agent-tasks`，支持后台任务与通知
- 加入 `agent-compact`，解决长会话和大结果处理
- 扩展 `tools-builtin`，补齐编辑、搜索和子 agent 能力

> **目标：** 让 session 可以持续更久，不因为输出过大或任务过长而变得脆弱。

### Phase 3：把边界打开

- 接入 `agent-mcp`
- 补更完整的插件/技能加载能力
- 支持更适合嵌入式场景的 SDK / headless 用法

> **目标：** 让它不只是一个 CLI，而是一套可以集成进别的系统里的 agent runtime。

## 📁 目录结构

```text
rust-clw/
├── README.md                # English documentation
├── ARCHITECTURE.zh-CN.md    # Claude Code (TS) 架构拆解笔记
└── docs/
    ├── README.zh-CN.md      # 简体中文文档
    ├── README.ja.md         # 日本語ドキュメント
    ├── README.ko.md         # 한국어 문서
    ├── README.es.md         # Documentación en español
    ├── README.fr.md         # Documentation en français
    └── assets/
        ├── overview.svg     # 项目总览图
        ├── architecture.svg # 架构图
        └── roadmap.svg      # 路线图
```

> 等各个 crate 真正落地之后，这里会继续展开成一个 Rust workspace。

## 🤝 参与贡献

欢迎贡献！项目目前处于早期设计阶段，有很多方式可以参与：

- **架构反馈** — 审阅 crate 设计并提出改进建议
- **RFC 讨论** — 通过 issue 提出新想法
- **文档完善** — 帮助改进或翻译文档
- **代码实现** — 在设计稳定后参与 crate 的实现

欢迎随时提 issue 或提交 pull request。

## 📚 参考资料

- [ARCHITECTURE.zh-CN.md](../ARCHITECTURE.zh-CN.md) — 对 TypeScript 版 Claude Code 的拆解笔记
- [Claude Code 官方文档](https://docs.anthropic.com/en/docs/claude-code)
- [Model Context Protocol](https://modelcontextprotocol.io/)

## 📄 许可证

本项目基于 [MIT 许可证](../LICENSE) 开源。

---

<div align="center">

**如果觉得这个项目有价值，欢迎点个 ⭐**

</div>
