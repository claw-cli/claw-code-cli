# devo Detailed Specification: Conversation

## Background and Goals

The design overview defines a three-level conversation hierarchy:

- Session
- Turn
- Item

This specification turns that hierarchy into the canonical runtime and persistence model.

Goals:

- Define the recoverable source-of-truth data model.
- Support persistence, replay, resume, and fork.
- Make prompt construction a derived operation rather than the primary state representation.

## Scope

In scope:

- Session, turn, and item identifiers.
- Persistent storage layout and journal schema.
- Session lifecycle and turn lifecycle.
- Rust structs, repository interfaces, and invariants.

Out of scope:

- Provider-specific request shaping.
- UI rendering and transport framing.

## Design Constraints

The conversation model must preserve:

- Individual item boundaries.
- Turn-level status.
- Approval records.
- Tool progress metadata.
- Replayable persistence.

## Module Responsibilities and Boundaries

`devo-core::conversation` owns:

- Identifiers.
- Session and turn metadata.
- Item schema and serialization.
- rollout JSONL append and load.
- Resume and fork reconstruction.
- secondary index updates for listing and lookup.
- session-title state transitions and title update persistence.

`devo-core::runtime` owns:

- Transitioning a turn through states.
- Emitting items during execution.
- Translating provider and tool events into items.

`devo-core::context` may read items but must not mutate raw persisted history.

## Data Structures

### Identifiers

```rust
pub struct SessionId(Uuid);
pub struct TurnId(Uuid);
pub struct ItemId(Uuid);
```

Requirements:

- `SessionId`, `TurnId`, and `ItemId` use UUID v7.
- Newtypes implement `Debug, Clone, Copy, PartialEq, Eq, TS, Hash` only when cheap and safe.
- IDs are generated only by core runtime factories, not by UI adapters.
- Should have `TryFrom<&str>`, `TryFrom<String>`, `Deserialize<'de>`, `Serialize`, `JsonSchema`, so many implementation, should extract 
  reusable internal Id structure.

### Session Metadata

```rust
pub enum SessionTitleState {
    Unset,
    Provisional,
    Final(SessionTitleFinalSource),
}

pub enum SessionTitleFinalSource {
    ModelGenerated,
    UserRename,
    ExplicitCreate,
}

pub struct SessionRecord {
    /// The session identifier.
    pub id: SessionId,
    /// The absolute rollout path on disk.
    pub rollout_path: PathBuf,
    /// The creation timestamp.
    pub created_at: DateTime<Utc>,
    /// The last update timestamp.
    pub updated_at: DateTime<Utc>,
    /// The session source (stringified enum).
    pub source: String,
    /// Optional random unique nickname assigned to an AgentControl-spawned sub-agent.
    pub agent_nickname: Option<String>,
    /// Optional role (agent_role) assigned to an AgentControl-spawned sub-agent.
    pub agent_role: Option<String>,
    /// Optional canonical agent path assigned to an AgentControl-spawned sub-agent.
    pub agent_path: Option<String>,
    /// The model provider identifier.
    pub model_provider: String,
    /// The latest observed model for the session.
    pub model: Option<String>,
    /// The latest observed reasoning effort for the session.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// The working directory for the session.
    pub cwd: PathBuf,
    /// Version of the CLI that created the session.
    pub cli_version: String,
    /// The current best-effort session title, if any.
    pub title: Option<String>,
    /// The current title lifecycle state.
    pub title_state: SessionTitleState,
    /// The parent session when this session was forked, if any.
    pub parent_session_id: Option<SessionId>,
    /// The sandbox policy (stringified enum).
    pub sandbox_policy: String,
    /// The approval mode (stringified enum).
    pub approval_mode: String,
    /// The last observed token usage.
    pub tokens_used: i64,
    /// First user message observed for this session, if any.
    pub first_user_message: Option<String>,
    /// The archive timestamp, if the session is archived.
    pub archived_at: Option<DateTime<Utc>>,
    /// The git commit SHA, if known.
    pub git_sha: Option<String>,
    /// The git branch name, if known.
    pub git_branch: Option<String>,
    /// The git origin URL, if known.
    pub git_origin_url: Option<String>,
}
```

### Turn Metadata

```rust
pub enum TurnStatus {
    Pending,
    Running,
    WaitingApproval,
    Interrupted,
    Completed,
    Failed,
}

pub struct TurnRecord {
    pub id: TurnId,
    pub session_id: SessionId,
    pub sequence: u32,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: TurnStatus,
    pub model_slug: String,
    pub input_token_estimate: Option<u32>,
    pub usage: Option<TurnUsage>,
}
```

### Item Model

```rust
pub enum TurnItem {
    UserMessage(UserMessageItem),
    SteerInput(SteerInputItem),
    HookPrompt(HookPromptItem),
    AgentMessage(AgentMessageItem),
    Plan(PlanItem),
    Reasoning(ReasoningItem),
    ToolCall(ToolCallItem),
    ToolProgress(ToolProgressItem),
    ToolResult(ToolResultItem),
    ApprovalRequest(ApprovalRequestItem),
    ApprovalDecision(ApprovalDecisionItem),
    WebSearch(WebSearchItem),
    ImageGeneration(ImageGenerationItem),
    ContextCompaction(ContextCompactionItem),
}

pub struct ItemRecord {
    pub id: ItemId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub attempt_placement: Option<i64>,
    pub turn_status: Option<TurnStatus>,
    pub sibling_turn_ids: Vec<TurnId>,
    pub input_items: Vec<TurnItem>,
    pub output_items: Vec<TurnItem>,
    pub worklog: Option<Worklog>,
    pub error: Option<TurnError>,
    pub schema_version: u32,
}
```

### Rollout Line

Primary persisted history must be written as a single append-only rollout stream.

```rust
pub enum RolloutLine {
    SessionMeta(SessionMetaLine),
    Turn(TurnLine),
    Item(ItemLine),
    SessionTitleUpdated(SessionTitleUpdatedLine),
    CompactionSnapshot(CompactionSnapshotLine),
}
```

```rust
pub struct SessionMetaLine {
    pub timestamp: DateTime<Utc>,
    pub session: SessionRecord,
    pub git: Option<GitSessionInfo>,
}
```

```rust
pub struct TurnLine {
    pub timestamp: DateTime<Utc>,
    pub turn: TurnRecord,
}
```

```rust
pub struct ItemLine {
    pub timestamp: DateTime<Utc>,
    pub item: ItemRecord,
}
```

```rust
pub struct SessionTitleUpdatedLine {
    pub timestamp: DateTime<Utc>,
    pub session_id: SessionId,
    pub title: String,
    pub title_state: SessionTitleState,
    pub previous_title: Option<String>,
}
```

## Persistence Layout

The overview requires JSONL partitioned by date and session ID. The primary persistence model should follow the Codex rollout pattern: one append-only rollout file per session, plus derived metadata indexes for listing and repair.

The conversation subsystem must also maintain a required SQLite `state` database for session metadata, listing, search acceleration, and metadata repair support. The state database is not the canonical history source, but it is the canonical metadata index.

Required filesystem layout:

```text
<data_root>/sessions/
  2026/
    04/
      05/
        rollout-2026-04-05T12-30-45-<session_id>.jsonl
<data_root>/session_index.jsonl
<data_root>/state/
  devo.sqlite
```

Rules:

- the rollout `.jsonl` file is the canonical recoverable source of truth
- the first persisted line for a created session must be `SessionMetaLine`
- subsequent lines append turn metadata changes, item records, and compaction snapshot records in chronological order
- title changes append `SessionTitleUpdatedLine` records in the same chronological stream
- date partition is derived from session creation timestamp
- filename must embed both creation timestamp and session id
- forked sessions create their own rollout file and record `parent_session_id`
- `session_index.jsonl` is optional and may remain as an append-only supplemental index
- `devo.sqlite` is the required structured metadata store for session listing, filtering, search acceleration, and metadata repair workflows
- the rollout `.jsonl` file remains the canonical recoverable history source; `devo.sqlite` is derived metadata

### Primary Rollout File Rules

- writes are append-only
- every line is standalone JSON
- partial trailing lines may be ignored or rejected on resume, but earlier valid lines remain authoritative
- persistence may flush after every append for durability

### State Database Contract

The `state` database is required for efficient cross-session metadata queries.

Minimum tables:

```sql
CREATE TABLE sessions (
  session_id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  cwd TEXT NOT NULL,
  title TEXT,
  title_state_json TEXT NOT NULL,
  source_kind TEXT NOT NULL,
  ephemeral INTEGER NOT NULL,
  parent_session_id TEXT,
  archived INTEGER NOT NULL DEFAULT 0,
  latest_turn_id TEXT,
  latest_turn_status TEXT,
  latest_model_slug TEXT,
  first_user_message_preview TEXT,
  rollout_path TEXT NOT NULL,
  schema_version INTEGER NOT NULL
);
```

Rules:

- `sessions` stores derived metadata only, never the full canonical turn or item history
- `title_state_json` stores the normalized `SessionTitleState` projection used for listing and fast metadata reads
- `rollout_path` points to the canonical rollout file
- `devo.sqlite` schema migrations must be versioned independently from rollout schema versioning
- if `devo.sqlite` is missing or corrupt, it must be rebuildable by rescanning rollout files
- session resume must still trust rollout data over any conflicting state-database metadata

## Lifecycle and State Transitions

Session lifecycle:

1. `Created`
2. `Active`
3. `Archived`

Session title lifecycle:

1. `Unset` when the session is created without an explicit title
2. `Provisional` after the first successful assistant reply if deterministic derivation succeeds
3. `Final(ModelGenerated)` after an asynchronous title-generation upgrade succeeds
4. `Final(UserRename)` whenever the user or API explicitly sets a title
5. `Final(ExplicitCreate)` when the session starts with a caller-supplied title

Title transition rules:

- explicit title creation or rename always wins and must never be auto-overwritten
- a provisional title may be replaced by one model-generated final title
- once `Final(UserRename)` is set, queued automatic title jobs must be canceled or ignored
- automatic title generation must not run before the first assistant reply completes successfully
- session title and first-message preview are separate concepts and must remain separate in storage and API surfaces

Turn lifecycle:

1. `Pending` when accepted by the runtime
2. `Running` after provider execution begins
3. `WaitingApproval` whenever user approval blocks execution
4. `Completed`, `Interrupted`, or `Failed` as terminal states

Transition rules:

- A turn cannot move from a terminal state to a non-terminal state.
- An approval decision resumes the same turn; it must not create a new turn.
- A fork copies all completed turns and items from the source session and starts with no active turn.

## Key Execution Flows

### New Session

1. Generate `SessionId`.
2. Create rollout path from timestamp plus session id.
3. Append `SessionMetaLine`.
4. Update `devo.sqlite` and any optional supplemental indexes.
5. Emit session-created event.
6. Accept first `turn/start`.

### Session Title Generation

This design combines Claude Code's placeholder-first behavior with Codex's explicit metadata update discipline.

1. Create the session with `title = None` and `title_state = Unset` unless the client supplied an explicit title at create time, in which case persist that title with `title_state = Final(ExplicitCreate)`.
2. Persist the first user item as normal and execute the first turn.
3. When the first assistant reply reaches `Completed`, check whether the session already has an explicit title.
4. If not, attempt deterministic provisional derivation from the first user message.
5. If derivation succeeds, append `SessionTitleUpdatedLine`, update `devo.sqlite` and any optional supplemental indexes, and emit a title-updated event.
6. If config enables asynchronous finalization, queue a background title-generation job using the first completed exchange as input context.
7. When the background job returns a valid title, re-check the current title state.
8. If the title is still `Unset` or `Provisional`, append a second `SessionTitleUpdatedLine` with `Final(ModelGenerated)`.
9. If the user renamed the session while the background job was running, discard the generated result without writing it.

### Provisional Title Derivation

The provisional title path must be deterministic, cheap, and independent of any model call.

Rules:

- source text is the first persisted user-message item of the session
- ignore leading whitespace, markdown quote markers, and obvious shell prompt noise
- collapse internal whitespace to single spaces
- strip fenced code blocks and large pasted code spans before deriving a title candidate
- prefer the first title-worthy clause or sentence, not the full body
- output must be sentence case
- target length is 20 to 60 visible characters
- hard maximum is 80 visible characters
- if the first message is too short, code-only, or otherwise not title-worthy, the session may remain `Unset` until async generation or explicit rename

### Model-Generated Title Contract

The final automatic title path may use a model, but it produces metadata rather than conversational output.

Rules:

- generation input is a prompt view containing the first user message and the first successful assistant reply
- generation runs asynchronously relative to the visible first-turn completion
- output must be a short sentence-case title, not a filename, slug, or markdown heading
- target length is 3 to 8 words
- hard maximum is 80 visible characters
- trailing punctuation should be omitted unless required by a proper noun
- provider failure, timeout, or invalid output must not fail the turn or session; the current title remains unchanged
- only one automatic finalization attempt is required for the first milestone

### Turn Start

1. Generate `TurnId`.
2. Append `TurnLine` with `Pending`.
3. Append initial `UserMessage` item or input item batch as `ItemLine`s.
4. Append updated `TurnLine` with `Running`.
5. Invoke prompt builder.

### Resume Session

1. Read rollout `.jsonl`.
2. Parse the first `SessionMetaLine` as the canonical session header.
3. Replay subsequent `TurnLine`, `ItemLine`, `SessionTitleUpdatedLine`, and snapshot lines in file order.
4. Rebuild in-memory indices.
5. Validate item sequence and tool pair invariants.
6. Reconstruct the latest session title state from the most recent valid title-update line.
7. Reconstruct prompt view lazily on next turn.

### Fork Session

1. Load source session.
2. Materialize a new `SessionRecord` with `parent_session_id = Some(source.id)`.
3. Create a new rollout file for the forked session.
4. Append a new `SessionMetaLine` for the forked session.
5. Replay copied raw history into the new rollout stream or persist an explicit fork baseline record.
6. Append a `SystemNotice` item describing fork origin.

## Invariants

- Item `seq` is strictly increasing within a session.
- Turn `sequence` is strictly increasing within a session.
- `ItemRecord.turn_id` must reference an existing turn.
- `ItemRecord.session_id` must reference the enclosing session.
- Every `ToolResult` must reference a prior `ToolCall`.
- Every `ApprovalDecision` must reference a prior `ApprovalRequest`.
- Compaction summaries cannot replace raw history; they only affect prompt materialization.
- the effective session title is the latest valid `SessionTitleUpdatedLine` if one exists; otherwise it falls back to `SessionMetaLine.session.title`
- session title and session preview must never be conflated in persistence or API responses

## Configuration Definitions

Conversation-related config fields:

- `data_root: PathBuf`
- `ephemeral_sessions: bool`
- `session_title: SessionTitleConfig`
- `rollout_flush_mode: enum { immediate, buffered }`
- `max_items_per_turn: u32`
- `enable_session_index: bool`
- `enable_state_db: bool`

## Error Handling Strategy

`SessionRepoError` variants:

- `NotFound`
- `AlreadyExists`
- `CorruptRollout`
- `SchemaMismatch`
- `Io`
- `InvariantViolation`

Behavior:

- Corrupt rollout files fail session resume with a hard error once the canonical header or invariant-critical lines are unreadable.
- A failed item append aborts the current turn.
- Buffered writes are allowed only if the process still flushes before sending a terminal turn event.
- State-database write failure must not invalidate canonical rollout persistence, but it must surface as a warning and schedule repair.
- Optional supplemental index write failure must not invalidate canonical rollout persistence, but it must surface as a warning and schedule repair.
- Failed automatic title writes must not invalidate the session or turn; they surface as metadata warnings and may be retried only while no explicit title exists

## Concurrency and Async Model

- One session writer task owns append order.
- Read operations may run concurrently with prompt construction if they use immutable loaded state.
- Resume and fork operations lock the target session but not unrelated sessions.
- state-database reconciliation may run asynchronously after canonical rollout append succeeds.
- optional supplemental index reconciliation may run asynchronously after canonical rollout append succeeds.
- automatic title generation may run on a background task, but title persistence must still be serialized through the session writer

## Observability

Required logs and metrics:

- `conversation.session.created`
- `conversation.session.title.updated`
- `conversation.session.title.generation_failed`
- `conversation.turn.started`
- `conversation.turn.completed`
- `conversation.item.appended`
- `conversation.resume.duration_ms`
- `conversation.fork.duration_ms`
- `conversation.index.repair.count`
- `conversation.state.repair.count`
- `conversation.state.write.failure.count`

## Security and Edge Cases

- User-supplied text may contain secrets; items persisted to disk must store redacted copies when configured.
- Partial rollout writes must be detected during resume by rejecting malformed trailing lines or stopping replay at the final incomplete line.
- Ephemeral sessions must never create on-disk directories.
- automatic title derivation and generation must use the same redacted prompt view that is safe for persistence and telemetry
- titles must not be derived from hidden system prompts, raw reasoning content, or tool-only output that the user never saw

## Testing Strategy and Acceptance Criteria

Required tests:

- UUID v7 ordering and serialization.
- Append and resume round-trip.
- Fork preserves parent history without reusing IDs.
- Tool call/result pair validation.
- Corrupt trailing JSONL line handling.
- state-database rebuild from rollout files
- optional supplemental index rebuild from rollout file
- provisional title derivation from a natural-language first user message
- explicit rename blocks asynchronous automatic overwrite
- resume reconstructs latest title state from title-update lines
- listing preview remains distinct from canonical session title
- state-database listing reflects title updates, archive status, and latest-turn metadata

Acceptance criteria:

- A session with approvals, tool calls, and compaction can be replayed from a single rollout JSONL file without consulting any UI adapter state.
- Resume and fork preserve ordering and cross-reference integrity.
- after the first successful exchange, a session can expose a stable title without blocking turn completion on a model call

## Dependencies With Other Modules

- Language Model consumes prompt-view projections from conversation state.
- Safety adds approval and redaction item types.
- Context Management consumes items to build summaries.
- API exposes session and turn lifecycle operations.

## Open Questions and Assumptions

Assumptions:

- rollout JSONL is the only canonical recoverable history artifact
- `devo.sqlite` is the required metadata index, but it remains derivable from rollout files
- the first milestone requires at most one automatic model-generated title upgrade per session

Open questions:

- Whether reasoning raw content should be persisted as encrypted blobs or omitted entirely.
