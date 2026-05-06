# Architecture Overview

## Background and Goals

`design-overview.md` defines the product at a high level: Rust coding agent built around sessions, turns, items, tools, permissions, compaction, and a API. This document expands that overview into an implementation-ready architecture contract.

Primary goals:

- The target binary should named `devo`.
- Keep the architecture Rust-native, modular, and testable.
- Provide stable crate and module boundaries that allow incremental delivery.

This overview spec is contract for the subordinate specs:

- [Conversation](./spec-conversation.md)
- [Language Model](./spec-language-model.md)
- [Safety](./spec-safety.md)
- [Context Management](./spec-context-management.md)
- [Tools](./spec-tools.md)
- [MCP](./spec-mcp.md)
- [Skills](./spec-skills.md)
- [Server API](./spec-server-api.md)
- [App Config](./spec-app-config.md)

## Scope

- Crate responsibilities and ownership boundaries.
- Shared runtime vocabulary and invariants.
- Cross-cutting requirements for async execution, persistence, observability, and testing.
- The target architecture for the project.

Out of scope:

- Provider-specific HTTP payload minutiae.
- UI rendering details for CLI, desktop, or IDE clients.
- MCP protocol internals beyond the boundaries needed by the agent runtime.

## Architectural Principles

1. Conversation state is the source of truth. Views such as prompts, summaries, and streamed UI events are derived artifacts.
2. Tool execution is explicit and itemized. Every tool request and result must become structured history.
3. Safety is enforced outside the model. The model is informed of constraints, but enforcement is deterministic.
4. Session state and turn state are separate. Session state persists across turns; turn state is disposable and cancellable.
5. Context compaction changes prompt materialization, never the recoverable raw history.
6. Transport and UI are adapters. Core runtime logic must not depend on CLI-specific behavior.
7. User guidance during an active turn should be modeled as same-turn steering with queued pending input, not as implicit interruption and restart.

## Target Crate Responsibilities

| Crate | Responsibility |
| --- | --- |
| `devo-core` | Session, turn, item model, main loop, model integration, context management, persistence / rollout, event emission, coding workflow orchestration, config loading, and runtime state |
| `devo-tools` | Tool traits, registry, orchestration, execution metadata, built-in tools (shell command, file search, read, write, patch) |
| `devo-safety` | Policy evaluation, rules, approval scopes, secret redaction contracts, and sandbox integration |
| `devo-mcp` | MCP server lifecycle, capability discovery, and bridging |
| `devo-protocol` | Shared structured types across crates (shell summaries, provider wire API enums, token usage, etc.) |
| `devo-server` | Transport-neutral runtime server, JSON-RPC 2.0, lifecycle, subscriptions, and connection management |
| `devo-client` | Client library for connecting to devo-server over stdio or WebSocket |
| `devo-tui` | Interactive terminal UI (ratatui + crossterm) |
| `devo-cli` | Local bootstrap, config loading, command dispatch, and terminal UX entrypoint |
| `devo-utils` | Cross-cutting low-level helpers (home dir, config paths, path normalization, etc.) |
| `devo-arg0` | OS-specific arg0/binary path resolution |
| `devo-tasks` | Background task state and long-running execution |
| `devo-file-search` | Separate file-search binary for search functionality |

## Shared Vocabulary

| Term | Definition |
| --- | --- |
| Session | Persistent conversation identified by UUID v7 and containing turn history |
| Turn | One execution cycle beginning with user input and ending in terminal assistant output, interruption, or failure.  |
| Item | Smallest persisted execution record, including user input, assistant output, tool use, tool result, tool progress, reasoning summary, approval request, approval decision, and same-turn steering input. |
| Prompt View | Model-facing materialization of session history after truncation, compaction, and modality filtering |
| Policy Snapshot | Resolved safety state used for a turn, including sandbox, network, and approval caches |
| Summary Snapshot | Recoverable compaction artifact that replaces historical prompt material but does not delete raw item history |

## Cross-Cutting Data Contracts

Every persisted domain object must carry:

- Stable identifier.
- `session_id`.
- `turn_id` when applicable.
- RFC 3339 timestamp in UTC.
- Schema version.

Every streamed event must carry:

- Event name.
- Correlation identifiers: `session_id`, `turn_id`, and `item_id` when applicable.
- Monotonic sequence number per session connection.

Every error exposed outside a crate must be one of:

- Validation error.
- Policy denial.
- Approval required.
- Provider error.
- Sandbox error.
- Persistence error.
- Internal invariant violation.

## Async and Concurrency Model

- Use `tokio` as the sole async runtime.
- Session operations are serialized per session through a `SessionHandle` actor or `tokio::Mutex<SessionState>`.
- Read-only tool calls may execute concurrently within a turn.
- Mutating tool calls are serialized in invocation order.
- Approval waits, model streaming, and MCP elicitations must be cancellable.
- Persistence appends must be ordered and awaited before emitting terminal item completion events.

## Client and Server Topology

The architecture must support multiple UI clients, but it must not depend on one mandatory singleton server process.

Supported topology modes:

- embedded server runtime inside a local client process
- spawned child-process server connected over stdio
- separately launched shared server connected over websocket

Rules:

- all modes use the same `devo-server` protocol and the same persisted session format
- persisted sessions are the cross-client continuity mechanism
- in-memory loaded-session state is local to one running server process
- a client may attach to a different server process later and resume the same persisted session
- process-local optimizations such as live subscriptions, loaded-session caches, and active-turn handles are ephemeral runtime state rather than durable shared truth

## Persistence and IO Baseline

- Raw session history is stored as JSONL under a date-partitioned directory tree.
- Session metadata and resumable indexes are stored separately from raw item journals.
- Configuration is read from user-level JSON or TOML config, but runtime journals are JSONL only.
- Secrets are never written to model-visible history; redacted values may be written only if the original cannot be reconstructed from logs.

## Observability Baseline

- Structured logs use `tracing`.
- Every turn emits start, completion, interruption, and failure events.
- Metrics must include token usage, compaction count, approval prompts, tool latency, model latency, and persistence write latency.
- Long-running operations should create tracing spans: `session.start`, `turn.start`, `model.stream`, `tool.execute`, `approval.wait`, `compact.run`.

## Security Baseline

- Redaction occurs before tool output prompt sent to language model.
- Sandbox policy is resolved before command execution.
- Approval only within the approved scope, Now there are following scope type.
  ```
  /// User has approved this command and the agent should execute it.
  Approved,
  /// User has approved this command and wants to apply the proposed execpolicy
  /// amendment so future matching commands are permitted.
  ApprovedExecpolicyAmendment {
      proposed_execpolicy_amendment: ExecPolicyAmendment,
  },
  /// User has approved this request and wants future prompts in the same
  /// session-scoped approval cache to be automatically approved for the
  /// remainder of the session.
  ApprovedForSession,
  /// User chose to persist a network policy rule (allow/deny) for future
  /// requests to the same host.
  NetworkPolicyAmendment {
      network_policy_amendment: NetworkPolicyAmendment,
  },
  /// User has denied this command and the agent should not execute it, but
  /// it should continue the session and try something else.
  #[default]
  Denied,
  /// User has denied this command and the agent should not do anything until
  /// the user's next command.
  Abort,
  ```
- Persisted journals must avoid storing raw secrets in text fields, command stderr, or tool output payloads.

## Testing Strategy

Minimum required test layers:

- Unit tests for IDs, item schemas, prompt construction, policy evaluation, and compaction selectors.
- Contract tests for provider normalization and API serialization.
- Integration tests for session resume, fork, approval escalation, compaction recovery, and tool pairing invariants.
- Golden JSON fixtures for streamed event sequences and persisted journal lines.
- Integration tests for round revoke in session, should be back file state accroding to git ghost branch commit history.

## Acceptance Criteria

- All runtime behavior described in `design-overview.md` maps to a concrete crate and module owner.
- A session can be persisted, resumed, compacted, and replayed without losing raw history.
- A session can be back to a point in the history.
- Tool execution, approvals, and compaction all produce structured items and events.
- The API layer can drive the runtime without accessing crate-internal mutable state directly.
- common helper logic that is reused by multiple crates has a clear home in `devo-utils` instead of accumulating as duplicated local helpers

## Dependencies With Other Specifications

- Conversation defines the persisted model used everywhere else.
- Language Model defines prompt building inputs and model catalog data.
- Safety defines policy, redaction, and approval contracts.
- Context Management defines prompt view derivation and summary lifecycle.
- Tools defines the built-in tool contract and execution lifecycle.
- MCP defines external capability discovery and invocation boundaries.
- Skills defines reusable instruction discovery and turn injection behavior.
- Server API defines the external orchestration surface.
- App Config defines cross-cutting runtime defaults and config merge rules.

## Open Questions and Assumptions

Assumptions:

- JSONL persistence belongs in `devo-core`, not a new crate, unless persistence complexity grows enough to justify extraction.
- `devo-server` is the canonical home for the runtime API surface.
