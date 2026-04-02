<div align="center">

# 🦀 Claude Code Rust

**Claude Code의 핵심 런타임 아이디어를 추출하여 Rust로 재설계한 재사용 가능한 crate 세트.**

[![상태](https://img.shields.io/badge/상태-설계중-blue?style=flat-square)](https://github.com/)
[![언어](https://img.shields.io/badge/언어-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![출처](https://img.shields.io/badge/출처-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![라이선스](https://img.shields.io/badge/라이선스-MIT-green?style=flat-square)](../../LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](../../README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md)

<img src="../assets/overview.svg" alt="프로젝트 개요" width="100%" />

</div>

---

## 📖 목차

- [프로젝트 소개](#-프로젝트-소개)
- [왜 Rust로 재구축하는가](#-왜-rust로-재구축하는가)
- [설계 목표](#-설계-목표)
- [아키텍처](#-아키텍처)
- [모듈 상세](#-모듈-상세)
- [Rust vs TypeScript](#-rust-vs-typescript)
- [로드맵](#-로드맵)
- [디렉토리 구조](#-디렉토리-구조)
- [기여하기](#-기여하기)
- [참고 자료](#-참고-자료)
- [라이선스](#-라이선스)

## 💡 프로젝트 소개

이 프로젝트는 [Claude Code](https://docs.anthropic.com/en/docs/claude-code)의 에이전트 런타임 핵심 역량을 추출하여 재사용 가능한 Rust crate로 재구성합니다. TypeScript를 한 줄 한 줄 옮기는 포트가 아니라, 에이전트가 실제로 의존하는 역량을 클린룸 방식으로 재설계한 것입니다:

- **Message Loop** — 다턴 대화를 구동
- **Tool Execution** — 스키마 검증과 함께 tool 호출을 오케스트레이션
- **Permission Control** — 파일/셸/네트워크 접근 전 권한 부여
- **Long-running Tasks** — 생명주기 관리가 있는 백그라운드 실행
- **Context Compaction** — 토큰 예산 하에서 긴 세션을 안정적으로 유지
- **Model Providers** — 스트리밍 LLM 백엔드를 위한 통합 인터페이스
- **MCP Integration** — Model Context Protocol로 기능 확장

**에이전트 런타임 골격**으로 이해하면 됩니다:

| Layer | Role |
|-------|------|
| **Top** | 모든 crate를 조립하는 얇은 CLI |
| **Middle** | 핵심 런타임: message loop, tool 오케스트레이션, permissions, tasks, model 추상화 |
| **Bottom** | 구체적 구현: built-in tools, MCP client, context 관리 |

> 경계가 충분히 명확하면, Claude 스타일 코딩 에이전트뿐 아니라 견고한 런타임 기반이 필요한 모든 에이전트 시스템에 활용할 수 있습니다.

## 🤔 왜 Rust로 재구축하는가

Claude Code는 엔지니어링 품질이 뛰어나지만 **완성된 제품**이지 재사용 가능한 런타임 라이브러리는 아닙니다. UI, 런타임, tool 시스템, 상태 관리가 깊게 얽혀 있습니다. 소스를 읽는 것만으로도 배울 점이 많지만, 일부를 떼어 재사용하기는 쉽지 않습니다.

이 프로젝트의 목표는 다음과 같습니다:

- **분해** — 강하게 결합된 로직을 단일 책임 crate로 나눔
- **대체** — 런타임 제약을 trait·enum 경계로 표현
- **전환** — “이 프로젝트 안에서만 동작”하는 구현을 **재사용 가능한 에이전트 컴포넌트**로 바꿈

## 🎯 설계 목표

1. **Runtime first, product later.** Agent loop, Tool, Task, Permission에 대한 견고한 기반을 우선합니다.
2. **각 crate는 스스로 설명 가능해야 함.** 이름이 책임을, 인터페이스가 경계를 드러냅니다.
3. **교체가 자연스럽게.** Tool, model provider, permission 정책, compaction 전략은 모두 교체 가능해야 합니다.
4. **Claude Code의 경험에서 배우되** UI나 내부 기능을 그대로 복제하지 않습니다.

## 🏗 아키텍처

<div align="center">
<img src="../assets/architecture.svg" alt="아키텍처 개요" width="100%" />
</div>

### Crate Map

| Crate | Purpose | Derived From (Claude Code) |
|-------|---------|---------------------------|
| `agent-core` | Message model, state container, main loop, session | `query.ts`, `QueryEngine.ts`, `state/store.ts` |
| `agent-tools` | Tool trait, registry, execution orchestration | `Tool.ts`, `tools.ts`, tool service layer |
| `agent-tasks` | Long task lifecycle and notification mechanism | `Task.ts`, `tasks.ts` |
| `agent-permissions` | Tool call authorization and rule matching | `types/permissions.ts`, `utils/permissions/` |
| `agent-provider` | Unified model interface, streaming, retry | `services/api/` |
| `agent-compact` | Context trimming and token budget control | `services/compact/`, `query/tokenBudget.ts` |
| `agent-mcp` | MCP client, connection, discovery, reconnect | `services/mcp/` |
| `tools-builtin` | Built-in tool implementations | `tools/` |
| `claude-cli` | Executable entry point, assembles all crates | CLI layer |

## 🔍 모듈 상세

<details>
<summary><b>agent-core</b> — 기반</summary>

대화 턴이 어떻게 시작·진행·종료되는지 관리합니다. 통합 message model, 메인 루프, 세션 상태를 정의합니다. 전체 시스템의 기반입니다.
</details>

<details>
<summary><b>agent-tools</b> — Tool 정의 및 디스패치</summary>

“tool이 어떤 모습인지”, “tool이 어떻게 스케줄되는지”를 정의합니다. Rust 버전에서는 모든 context를 하나의 거대한 객체에 넣지 않고, tool이 실제로 필요한 부분만 받도록 합니다.
</details>

<details>
<summary><b>agent-tasks</b> — 백그라운드 task 런타임</summary>

tool 호출과 런타임 task를 분리하는 것은 긴 명령, 백그라운드 에이전트, 대화로 다시 피드백되는 완료 알림을 지원하는 데 필수적입니다.
</details>

<details>
<summary><b>agent-permissions</b> — 인가 레이어</summary>

에이전트가 무엇을 할 수 있는지, 언제 사용자에게 물어야 하는지, 언제 거절해야 하는지 제어합니다. 파일 읽기·쓰기·명령 실행이 있을 때마다 필요합니다.
</details>

<details>
<summary><b>agent-provider</b> — Model 추상화</summary>

model 백엔드 간 차이를 시스템으로부터 격리합니다. 스트리밍 출력, 재시도 로직, 오류 복구를 통합합니다.
</details>

<details>
<summary><b>agent-compact</b> — Context 관리</summary>

긴 세션의 안정성을 보장합니다. 단순 “요약”이 아니라, context에 따라 압축 수준과 예산 제어를 달리 적용해 무한 성장을 막습니다.
</details>

<details>
<summary><b>agent-mcp</b> — MCP 통합</summary>

외부 MCP 서비스에 연결해 원격 tool, resource, prompt를 통합된 capability 표면으로 가져옵니다.
</details>

<details>
<summary><b>tools-builtin</b> — Built-in tools</summary>

가장 자주 쓰는 tool을 구현하며, 파일 작업, 셸 명령, 검색, 편집 등 에이전트에게 필요한 기본 동작을 우선합니다.
</details>

## ⚖️ Rust vs TypeScript

| TypeScript (Claude Code) | Rust Approach |
|--------------------------|---------------|
| 광범위한 런타임 검사 | 검사를 타입 시스템으로 끌어올림 |
| Context 객체가 한없이 커지는 경향 | 더 작은 context / trait 경계 |
| 흩어진 callback과 이벤트 | 통합된 연속 이벤트 스트림 |
| 런타임 feature flag | 가능한 경우 컴파일 타임 feature gating |
| UI와 런타임이 강하게 결합 | 런타임을 독립 레이어로 |

> Rust가 “더 낫다”는 이야기가 아니라, **런타임 경계를 단단히 고정**하기에 Rust가 잘 맞는다는 뜻입니다. 장기적으로 진화하는 에이전트 시스템에는 그런 제약이 보통 가치 있습니다.

## 🗺 로드맵

<div align="center">
<img src="../assets/roadmap.svg" alt="로드맵" width="100%" />
</div>

### Phase 1: 먼저 실행하기

- `agent-core`, `agent-tools`, `agent-provider`, `agent-permissions` 구성
- 기본 `Bash`, `FileRead`, `FileWrite` tool 구현
- 최소한으로 동작하는 CLI 제공

> **목표:** 대화하고, tool을 호출하고, 명령을 실행하고, 파일을 읽고 쓸 수 있는 기본 버전.

### Phase 2: 세션 안정화

- 백그라운드 task와 알림을 위해 `agent-tasks` 추가
- 긴 세션과 큰 결과 처리를 위해 `agent-compact` 추가
- 편집, 검색, sub-agent 기능으로 `tools-builtin` 확장

> **목표:** 출력이 과도하게 커지거나 장기 실행 task 때문에 세션이 쉽게 깨지지 않고 더 오래 유지되도록 함.

### Phase 3: 경계 개방

- `agent-mcp` 통합
- plugin / skill 로딩 기능 추가
- 임베디드 시나리오를 위한 SDK / headless 사용 지원

> **목표:** CLI만이 아니라 다른 시스템에 통합할 수 있는 완전한 에이전트 런타임.

## 📁 디렉토리 구조

```text
rust-clw/
├── README.md                # English documentation
├── ARCHITECTURE.zh-CN.md    # Architecture analysis of Claude Code (TS)
└── docs/
    ├── README.zh-CN.md      # 简体中文文档
    ├── README.ja.md         # 日本語ドキュメント
    ├── README.ko.md         # 한국어 문서
    ├── README.es.md         # Documentación en español
    ├── README.fr.md         # Documentation en français
    └── assets/
        ├── overview.svg     # Project overview diagram
        ├── architecture.svg # Architecture diagram
        └── roadmap.svg      # Roadmap diagram
```

> crate가 추가되면 전체 Rust workspace 구조로 확장될 예정입니다.

## 🤝 기여하기

기여를 환영합니다! 이 프로젝트는 초기 설계 단계이며, 도울 수 있는 방법이 많습니다:

- **Architecture feedback** — crate 설계를 검토하고 개선안을 제안
- **RFC discussions** — issue로 새 아이디어 제안
- **Documentation** — 문서 개선 또는 번역
- **Implementation** — 설계가 안정되면 crate 구현에 참여

issue를 열거나 pull request를 보내 주셔도 됩니다.

## 📚 참고 자료

- [ARCHITECTURE.zh-CN.md](../../ARCHITECTURE.zh-CN.md) — Claude Code TypeScript 아키텍처에 대한 상세 분해
- [Claude Code Official Docs](https://docs.anthropic.com/en/docs/claude-code)
- [Model Context Protocol](https://modelcontextprotocol.io/)

## 📄 라이선스

이 프로젝트는 [MIT License](../../LICENSE)를 따릅니다.

---

<div align="center">

**이 프로젝트가 유용하다면 ⭐를 눌러주세요**

</div>
