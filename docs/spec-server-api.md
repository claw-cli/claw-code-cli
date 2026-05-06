# devo Detailed Specification: Server API

## Background and Goals

The overview defines a transport-neutral runtime API that supports:

- stdio
- WebSocket
- JSON-RPC 2.0 semantics with the `jsonrpc` field omitted on the wire
- session and turn lifecycle control
- event streaming for runtime observability

This specification defines the protocol state machine, methods, events, server-initiated requests, and error model.

## Scope

In scope:

- connection handshake
- session lifecycle methods
- turn lifecycle methods
- approval response flow
- event subscription and streaming
- transport framing rules

Out of scope:

- authentication for remote multi-tenant deployments
- UI rendering contracts

## Module Responsibilities and Boundaries

`devo-core` owns session and turn orchestration behind the API surface.

`devo-server` owns transport listeners, connection lifecycle, request routing, server-initiated request routing, and event fanout.

`devo-safety` supplies approval payloads routed through `approval/respond`.

The API layer must translate transport messages into typed runtime calls; it must not mutate session state directly.

## Client Topology

`devo-server` is the canonical runtime protocol surface, but the design must not require every UI client to share one singleton always-running server process.

Supported attachment modes:

1. embedded in-process server mode
2. spawned child-process stdio server mode
3. separately launched websocket server mode

Rules:

- desktop, CLI, and IDE clients must all use the same session, turn, item, and event protocol
- continuity across clients is achieved through shared persisted session storage, not through a mandatory shared daemon
- loaded in-memory session state is process-local
- a session created by one client may be resumed later by a different client talking to a different server instance
- if multiple clients happen to attach to the same live server process, they may share live subscriptions and loaded session state for that process only

Rationale:

- this matches the Codex split between a stable app-server protocol and multiple client bootstrap strategies
- Codex rich clients can talk to app-server over stdio or websocket, while CLI-oriented surfaces also use an in-process app-server client facade

## Transport Requirements

Supported transports:

- `stdio://` as newline-delimited JSON objects
- `ws://` as one JSON object per text frame

Rules:

- each message is a single JSON object
- clients and server must ignore unknown fields
- requests require `id`, `method`, and optional `params`
- notifications omit `id`

The wire format omits `"jsonrpc":"2.0"` but otherwise follows JSON-RPC request/response semantics.

## Connection State Machine

Per connection states:

1. `Connected`
2. `Initializing`
3. `Ready`
4. `Closed`

Handshake requirements:

1. client sends `initialize`
2. server replies success with server capabilities and runtime metadata
3. client sends `initialized`
4. only after step 3 may the client call session or turn methods

Any request before `initialized` must return protocol error `NotInitialized`.

## Core Protocol Types

```rust
pub struct InitializeParams {
    pub client_name: String,
    pub client_version: String,
    pub transport: ClientTransportKind,
    pub supports_streaming: bool,
    pub supports_binary_images: bool,
    pub opt_out_notification_methods: Vec<String>,
}
```

```rust
pub struct InitializeResult {
    pub server_name: String,
    pub server_version: String,
    pub platform_family: String,
    pub platform_os: String,
    pub server_home: PathBuf,
    pub capabilities: ServerCapabilities,
}
```

```rust
pub struct ServerCapabilities {
    pub session_resume: bool,
    pub session_fork: bool,
    pub turn_interrupt: bool,
    pub approval_requests: bool,
    pub event_streaming: bool,
}
```

## Session Methods

### `session/start`

Request:

```json
{
  "method": "session/start",
  "params": {
    "cwd": "C:/repo",
    "ephemeral": false,
    "title": null,
    "model": null
  }
}
```

Response fields:

- `sessionId`
- `createdAt`
- `cwd`
- `ephemeral`
- `resolvedModel`

### `session/resume`

Request fields:

- `sessionId`

Response fields:

- `session`
- `latestTurn`
- `loadedItemCount`

### `session/fork`

Request fields:

- `sessionId`
- `title`
- `cwd` optional override

Response fields:

- `session`
- `forkedFromSessionId`

## Skills Methods

### `skills/list`

Request fields:

- none

Response fields:

- `skills`

Rules:

- returns the currently discovered user and workspace skills
- skill records include `id`, `name`, `description`, `path`, `enabled`, and `source`

## Model Methods

### `model/catalog`

Request fields:

- none

Response fields:

- `models`

Rules:

- returns the merged model catalog (builtin + user `models.json` + project `models.json`)
- each entry includes `slug`, `display_name`, `channel`, `description`, `provider`, `context_window`, `thinking_capability`, `input_modalities`, `max_tokens`

### `model/saved`

Request fields:

- none

Response fields:

- `models`

Rules:

- returns only models that have been configured with credentials in `config.toml`
- each entry includes `slug`, `display_name`, `channel`, `description`, `provider_id`, `wire_api`, `context_window`
- cross-references the model catalog for display metadata

## Turn Methods

### `turn/start`

Request fields:

- `sessionId`
- `input`
- `model` optional override
- `sandbox` optional override
- `approvalPolicy` optional override
- `cwd` optional override

Response fields:

- `turnId`
- `status`
- `acceptedAt`

Rules:

- if another turn is active for the session, return `TurnAlreadyRunning`
- `cwd` override updates the session default for later turns only if the turn starts successfully

### `turn/interrupt`

Request fields:

- `sessionId`
- `turnId`
- `reason` optional

Response fields:

- `turnId`
- `status = interrupted` using the same wire/status vocabulary as `TurnStatus`

### `turn/steer`

`turn/steer` appends additional user input to an already active regular turn without starting a new turn.

Request fields:

- `sessionId`
- `expectedTurnId`
- `input`

Response fields:

- `turnId`

Rules:

- `expectedTurnId` is required
- the request succeeds only when the currently active turn exists and matches `expectedTurnId`
- review turns, manual compaction turns, and any other non-steerable turn kinds must reject `turn/steer`
- `turn/steer` must not accept model, sandbox, approval-policy, or cwd overrides
- a successful steer must not emit a second `turn/started`
- steering input belongs to the same logical turn for persistence and correlation purposes

## Approval Methods

### `approval/respond`

Request fields:

- `sessionId`
- `turnId`
- `approvalId`
- `decision`
- `scope`

Decision values:

- `approve`
- `deny`
- `cancel`

Scope values:

- `once`
- `turn`
- `session`
- `path_prefix`
- `host`
- `tool`

## Optional Event Subscription Method

Because the overview mentions an event subscription mechanism, the protocol must support:

### `events/subscribe`

Request fields:

- `sessionId` optional
- `eventTypes` optional filter

Response:

- `subscriptionId`

Assumption:

- stdio clients are auto-subscribed to their own connection-scoped session events.
- WebSocket clients may use explicit subscriptions for multiplexing.

## Event Model

The server emits three categories of outbound messages:

1. Notifications:
   - server-initiated JSON-RPC notifications with no `id`
   - used for lifecycle and streaming state
2. Server-initiated requests:
   - server messages with `id` that require client response
   - used for approvals, structured user input, and similar turn-interrupting interactions
3. Request-resolution notifications:
   - notifications confirming that a previously pending server request has been answered or cleared

The event stream is the notification subset plus any server-initiated requests that interrupt turn execution.

## Notification Opt-Out

Clients may suppress exact notification methods per connection by sending `optOutNotificationMethods` during `initialize`.

Rules:

- exact-match only, no wildcards or prefixes
- unknown method names are accepted and ignored
- opt-out applies to notifications only, not to server-initiated requests or normal responses
- opt-out state is connection-local

Example suppressions:

- `session/started`
- `item/agentMessage/delta`
- `turn/plan/updated`

## Notifications and Event Stream

Core notification families:

- `session/*`
- `turn/*`
- `item/*`
- `serverRequest/resolved`

Common event envelope:

```json
{
  "method": "item/completed",
  "params": {
    "sessionId": "...",
    "turnId": "...",
    "itemId": "...",
    "seq": 42,
    "payload": {}
  }
}
```

Event ordering guarantees:

- within a session connection, notifications are emitted in logical item sequence order
- `turn/completed` is emitted after the final `item/completed`
- approval requests are emitted after the approval-request item is persisted
- `serverRequest/resolved` is emitted after a pending server request is answered or cleared

## Session Events

Required session lifecycle notifications:

- `session/started`
- `session/status/changed`
- `session/archived`
- `session/unarchived`
- `session/closed`

`session/started` rules:

- emitted after `session/start` and `session/fork`
- carries the full session object including current status

`session/status/changed` rules:

- emitted when a loaded session changes runtime status after initial introduction
- payload includes `sessionId` and `status`

## Turn Events

Required turn notifications:

- `turn/started`
- `turn/completed`
- `turn/interrupted`
- `turn/failed`
- `turn/plan/updated`
- `turn/diff/updated`

`turn/started`:

- payload includes `turn`
- `turn.status` is `running`

`turn/completed`:

- payload includes final `turn`
- terminal status is `completed`, `interrupted`, or `failed`

Wire status contract:

- API payloads must reuse the canonical `TurnStatus` vocabulary from the conversation model
- the server must not introduce alternate spellings such as `inProgress` for the same state

Rules:

- the canonical final turn state is `turn/completed`
- `turn/failed` and `turn/interrupted` are low-latency hints, not replacements for terminal turn state

## Item Event Lifecycle

Every streamable item follows this lifecycle:

1. `item/started`
2. zero or more item-specific delta notifications
3. `item/completed`

Rules:

- `item/started` carries the initial full item envelope
- `item/completed` carries the authoritative final item state
- deltas must reference `itemId`
- deltas are applied in arrival order

## Item Taxonomy

The server must support typed items at least for:

- `userMessage`
- `agentMessage`
- `reasoning`
- `plan`
- `toolCall`
- `toolResult`
- `commandExecution`
- `fileChange`
- `mcpToolCall`
- `webSearch`
- `imageView`
- `contextCompaction`
- `approvalRequest`
- `approvalDecision`

The exact wire schema may evolve, but item kinds must remain explicit and tagged.

## Item-Specific Delta Events

Minimum delta notifications:

- `item/agentMessage/delta`
- `item/reasoning/summaryTextDelta`
- `item/reasoning/textDelta`
- `item/commandExecution/outputDelta`
- `item/fileChange/outputDelta`
- `item/plan/delta`

Semantics:

- `item/agentMessage/delta`:
  - streamed assistant text fragments
  - concatenate `delta` values by `itemId`
- `item/reasoning/summaryTextDelta`:
  - readable reasoning summary fragments
  - grouped by `summaryIndex`
- `item/reasoning/textDelta`:
  - raw reasoning text when supported
  - grouped by `contentIndex`
- `item/commandExecution/outputDelta`:
  - stdout and stderr fragments for a running command
- `item/fileChange/outputDelta`:
  - streamed patch or file edit tool output
- `item/plan/delta`:
  - streamed plan text

## Server-Initiated Requests

Some turn interactions require an explicit client response. These are JSON-RPC requests initiated by the server, not notifications.

Required request families:

- `item/commandExecution/requestApproval`
- `item/fileChange/requestApproval`
- `item/permissions/requestApproval`
- `item/tool/requestUserInput`
- `mcpServer/elicitation/request`

Rules:

- these requests must include stable request IDs
- when correlated with a turn, they must include `sessionId`, `turnId`, and target `itemId` where applicable
- the turn remains blocked until the request is answered or cleared

## Request Resolution Events

The server must emit:

- `serverRequest/resolved`

Payload:

- `sessionId`
- `requestId`
- `turnId` optional

This notification is emitted when:

- the client answered the pending server request
- the request was invalidated by turn completion
- the request was cleared by interruption or restart

This allows clients to remove stale approval or elicitation UI even when the request was not answered directly.

## Approval Event and Request Ordering

For command and file-change approvals, the required ordering is:

1. `item/started`
2. approval request from server
3. client response
4. `serverRequest/resolved`
5. `item/completed`

For permission-profile requests:

1. request from server
2. client response with granted subset
3. `serverRequest/resolved`
4. resumed turn execution

## Realtime and Auxiliary Event Families

The protocol may define additional namespaced event groups that are not persisted as turn items:

- `session/realtime/*`
- `command/exec/outputDelta`
- `fs/changed`
- `skills/changed`
- `app/list/updated`
- `mcpServer/startupStatus/updated`
- `windowsSandbox/setupCompleted`
- `account/*`

Rules:

- these are transport events, not conversation history items
- realtime audio and transcript events must be documented as non-persisted

## Error Model

Protocol error codes:

- `NotInitialized`
- `InvalidParams`
- `SessionNotFound`
- `TurnNotFound`
- `TurnAlreadyRunning`
- `ApprovalNotFound`
- `PolicyDenied`
- `ContextLimitExceeded`
- `InternalError`

Response shape:

```json
{
  "id": 1,
  "error": {
    "code": "SessionNotFound",
    "message": "session does not exist",
    "data": {}
  }
}
```

## State Transitions and Lifecycle

Per connection:

1. handshake
2. optional session creation or resume
3. repeated turn start and completion
4. interruption or closure

Per session:

- multiple connections may observe the same session
- only one active turn may exist at a time unless a future collaboration mode explicitly allows more

## Same-Turn Steering Design

Codex's "guide immediately without interrupting" behavior is implemented as same-turn steering with queued pending input. `devo` should follow that design.

### Goals

- let the user redirect an in-flight turn immediately
- avoid discarding already useful streamed work
- preserve one coherent turn record rather than synthesizing micro-turns
- prevent UI races from attaching late guidance to the wrong turn

### Runtime State

```rust
pub struct ActiveTurnSteeringState {
    pub turn_id: TurnId,
    pub turn_kind: TurnKind,
    pub pending_inputs: VecDeque<SteerInputRecord>,
}

pub struct SteerInputRecord {
    pub item_id: ItemId,
    pub received_at: DateTime<Utc>,
    pub input: Vec<InputItem>,
}
```

Rules:

- steering input is persisted immediately as user-input items linked to the active turn
- steering input is also queued in `pending_inputs` for later same-turn consumption
- the active turn consumes queued steering input only at safe internal checkpoints
- steering must never retroactively mutate already completed persisted items

### Safe Consumption Boundaries

The runtime may consume queued steering input only:

1. before the next model request starts
2. after a tool result is integrated and before the next planning or model step
3. after an approval request resolves and before execution resumes
4. after another natural subtask boundary where the runtime is about to re-prompt

The runtime must not inject steering input:

- into an already in-flight provider request
- into already-emitted message deltas retroactively
- into non-steerable turn kinds

### Why This Feels Immediate

The user-visible immediacy comes from three behaviors:

1. the client can send `turn/steer` without waiting for the current turn to finish
2. the server persists and acknowledges that guidance immediately
3. the active turn reuses the queued input at the next prompt boundary instead of forcing an interrupt and full restart

### Error Handling

`turn/steer` must fail with structured errors when:

- there is no active turn
- `expectedTurnId` does not match the current active turn
- the active turn kind is not steerable
- the submitted input is empty

Suggested error codes:

- `NoActiveTurn`
- `ExpectedTurnMismatch`
- `ActiveTurnNotSteerable`
- `EmptyInput`

### Persistence Rules

- steering input must be persisted as ordinary user-input items inside the active turn
- replay must preserve the fact that the user injected same-turn guidance
- no synthetic second turn is created for the steering input
- if the turn is interrupted before consuming some queued steering input, those user items still remain in history

### Event Rules

Minimum event behavior:

- optional `item/started` and `item/completed` for the appended user-input item
- no new `turn/started`
- later `turn/plan/updated` or item stream changes may naturally reflect the steering effect

Optional first-milestone event:

- `turn/steered`

### Concurrency Rules

- steering enqueues through the same per-session serialized runtime handle as other session mutations
- order of multiple steer requests must be preserved
- a steer request racing with turn completion must either bind to the still-active turn or fail cleanly; it must never attach to the next turn by mistake

## Concurrency and Async Model

- Each transport connection runs independently on `tokio`.
- Request handling is async and may overlap across sessions.
- Requests affecting the same session must serialize through the session runtime handle.
- Event delivery must preserve per-session ordering even when multiple sessions are active concurrently on the same server.

## Configuration Definitions

Required server config:

- `listen: Vec<ListenAddress>`
- `max_connections`
- `event_buffer_size`
- `idle_session_timeout`
- `persist_ephemeral_sessions: bool`

## Observability

Metrics:

- `api.connection.open.count`
- `api.initialize.duration_ms`
- `api.request.duration_ms`
- `api.notification.backpressure.count`
- `api.turn.active.count`
- `api.server_request.pending.count`

Logs:

- connection id
- transport kind
- session id
- turn id
- request method
- outcome
- notification method
- request correlation id for server-initiated requests

## Security and Edge Cases

- Reject requests before initialization.
- Validate all IDs as UUID strings.
- On connection drop while waiting for approval, keep the turn suspended only if another client can still answer.
- For stdio, a malformed JSON line terminates the connection with protocol error.
- Notification opt-out must never suppress mandatory server-initiated requests such as approval prompts.

## Testing Strategy and Acceptance Criteria

Required tests:

- handshake enforcement
- stdio framing
- WebSocket single-frame message handling
- session start, resume, and fork
- turn interrupt
- approval response routing
- event ordering
- request resolution events
- notification opt-out exact matching
- server-initiated request cleanup on interrupted turns

Acceptance criteria:

- A client can create or resume a session, start a turn, receive streamed items, answer approvals, and observe completion using either transport.
- Requests before `initialized` are rejected consistently.
- Clients can reliably reconcile partial UI state using `item/completed` and `serverRequest/resolved`.

## Dependencies With Other Modules

- Conversation provides session, turn, and item identifiers.
- Safety provides approval request and decision payloads.
- Language Model provides model metadata surfaced in session and turn responses.

## Open Questions and Assumptions

Assumptions:

- `devo-server` is the canonical implementation crate for this protocol surface, while keeping the protocol itself crate-agnostic.
- The server is single-tenant for the initial milestone.

Open questions:

- Whether `events/subscribe` should exist in the first milestone or whether implicit connection-scoped subscriptions are enough initially.
- Whether WebSocket clients should be allowed to drive multiple sessions concurrently over one connection.
