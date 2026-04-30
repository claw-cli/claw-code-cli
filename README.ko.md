![표지](./docs/assets/readme_cover.png)

<div align="center">

**매우 빠르고 안전하며 모델 공급자에 종속되지 않는 오픈소스 코딩 에이전트입니다.**

🚧초기 단계 프로젝트로 활발히 개발 중 — 아직 프로덕션 준비가 안 됐습니다.
⭐ 별표로 팔로우해 주세요

[![상태](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![언어](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![출처](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![라이선스](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="프로젝트 개요" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 목차

- [빠른 시작](#-빠른-시작)
- [설치](#-설치)
- [자주 묻는 질문](#-자주-묻는-질문)
- [기여하기](#-기여하기)
- [참고](#-참고)
- [라이선스](#-라이선스)

## 📦 설치

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 빠른 시작

소스에서 빌드하고 싶다면 아래 안내를 따르세요.

### 빌드

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Rust가 설치되어 있어야 합니다. 1.75+를 권장합니다(https://rustup.rs/).

## 자주 묻는 질문

### 이것은 Claude Code와 무엇이 다릅니까?

기능적으로는 Claude Code와 매우 비슷합니다. 핵심 차이는 다음과 같습니다.

- 100% 오픈소스
- 특정 공급자에 종속되지 않습니다. Devo은 Claude, OpenAI, z.ai, Qwen, Deepseek, 심지어 로컬 모델과도 함께 사용할 수 있습니다. 모델이 발전할수록 격차는 줄고 가격도 내려가므로, 공급자 독립성은 중요합니다.
- TUI 지원은 이미 구현되어 있습니다
- 클라이언트/서버 아키텍처로 구성되어 있습니다. 예를 들어 코어는 로컬 머신에서 실행하면서 모바일 앱 같은 원격 클라이언트로 제어할 수 있고, TUI는 가능한 여러 클라이언트 중 하나일 뿐입니다.

## 🤝 기여하기

기여를 환영합니다. 이 프로젝트는 아직 초기 설계 단계이며, 도울 수 있는 방법이 많습니다.

- **아키텍처 피드백** — crate 설계를 검토하고 개선안을 제안
- **RFC 토론** — issue로 새 아이디어 제안
- **문서화** — 문서 개선 또는 번역
- **구현** — 설계가 안정되면 구현 crate를 맡기

issue를 열거나 pull request를 보내 주세요.

## 📄 라이선스

이 프로젝트는 [MIT 라이선스](./LICENSE)를 따릅니다.

---

**이 프로젝트가 유용하다면 ⭐를 눌러 주세요**
