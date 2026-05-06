# devo Detailed Specification: Context Management

## Background and Goals

The overview defines context management as the system that keeps the agent operating over long-running conversations despite finite model context windows. This specification defines estimation, truncation, compaction, summary storage, and prompt reconstruction.

## Scope

In scope:

- Token estimation.
- Per-item truncation.
- Compaction triggers and selection.
- Summary generation and replacement.
- Recoverability and snapshot storage.

Out of scope:

- Provider-specific tokenizer implementations beyond the estimation hook.

## Reference Rationale

The overview directly specifies threshold-based compaction and recoverable history. The more detailed split between raw history, summary items, and prompt segments is informed by:

- Claude Code's compaction and micro-compaction model, which treats prompt management as a derived view.
- history manager, which preserves structural invariants such as paired tool calls and outputs during history normalization.
- Codex's token-estimation design, which is simple enough to run before every turn: normalize history into a model-visible view, estimate text and structured items from serialized bytes, treat encoded reasoning specially, discount inline image payloads, and reconcile later with authoritative provider usage.

## Design Goals

- Stay inside model limits with deterministic headroom.
- Preserve recent and operationally relevant context.
- Keep raw history recoverable.
- Avoid structurally invalid prompt views such as orphaned tool results.

## Module Responsibilities and Boundaries

`devo-core::context` owns:

- token estimation
- selection of items eligible for summarization
- summary request construction
- summary result schema
- history snapshot metadata

`devo-core::runtime` owns:

- invoking compaction during the turn lifecycle
- storing compaction items and snapshot references
- rebuilding prompt views from summary plus recent raw items

## Token Estimation

The overview permits local approximations. The required default estimator is a deterministic byte-heuristic estimator. It is not tokenizer-accurate; it exists to make compaction and prompt fitting decisions before provider calls.

Required estimator behavior:

- estimate from model-visible bytes
- account separately for system prompt, tool schema, history, and current input
- expose conservative totals
- reconcile local estimates with authoritative provider usage after successful responses

Interface:

```rust
pub trait TokenEstimator {
    fn estimate_prompt(
        &self,
        model: &ModelConfig,
        prompt: &PromptAssemblyInput,
    ) -> ContextWindowEstimate;
}
```

Rules:

- The default implementation must be a byte-heuristic estimator, not a tokenizer-specific implementation.
- The estimator must run against the normalized prompt view, not against raw journal storage.
- Base instructions and other non-history prompt prefixes must be estimated separately, then added to item-derived totals.
- Estimates must overestimate rather than underestimate when uncertain.
- If provider returns accurate usage after a call, that usage becomes the baseline for the next estimate, and only prompt material added after the last successful provider response is re-estimated locally.

### Normalized Prompt View

Before any token estimate is computed, the context manager must build the same prompt-visible item sequence that prompt assembly will later send to the model.

Normalization requirements:

- exclude internal-only items that are never sent to the model
- exclude ghost or snapshot-only bookkeeping entries
- preserve pair invariants between tool calls and tool outputs
- remove orphaned outputs that no longer have a corresponding call
- insert or preserve corresponding outputs when the prompt contract requires paired structures
- strip unsupported image content when the selected model does not accept image input

The estimate is invalid if it is computed from a history view that differs from the later serialized provider request.

### Default Estimation Algorithm

The estimator must implement the following algorithm:

1. Build normalized prompt segments in provider send order.
2. Estimate base instructions and fixed prompt prefixes independently.
3. Estimate each model-visible history item.
4. Estimate current user input and tool schema sections.
5. Convert byte counts to token counts with a coarse fixed ratio.
6. Sum all segments with saturating arithmetic.

Required byte-to-token conversion:

- use a fixed heuristic equivalent to `ceil(bytes / 4)`
- clamp negative or invalid byte totals to zero
- saturate on overflow rather than wrapping

### Item Estimation Rules

For ordinary text and structured items:

- serialize the exact model-visible item representation
- use the serialized byte length as the raw cost basis
- convert bytes to tokens using the shared fixed ratio

For reasoning and compaction items stored in encoded form:

- do not charge the full encoded payload length directly
- estimate the decoded model-visible payload instead
- apply a fixed framing discount so encryption or transport envelope overhead is not mistaken for prompt-visible reasoning text

For image inputs:

- do not use raw inline base64 payload size as the token estimate
- preserve JSON wrapper bytes and data-URL metadata bytes already present in the serialized item
- subtract only the base64 payload bytes from the raw serialized size
- replace the payload with an estimated model-visible image cost

Image-cost rules:

- default or resized image inputs use a fixed replacement byte estimate
- `original` detail inputs use a patch-based estimate derived from decoded width and height
- the patch-based estimate uses 32px patches and converts patch count back into approximate bytes before token conversion
- image-dimension decoding failures fall back to the fixed replacement estimate
- image estimate caching may be keyed by content hash and remain in-memory only

### Baseline Reconciliation

After a successful provider response, the context manager must persist the provider-reported usage as the authoritative baseline for the prompt state that the provider actually saw.

For subsequent local estimates:

- start from the last authoritative provider total when available
- identify prompt-visible items appended after the last successful provider-produced item
- estimate only those newly added local items
- if past reasoning tokens were already included by the provider, do not re-estimate them
- if authoritative usage is unavailable, fall back to full local recomputation

This baseline model prevents repeated double-counting of old history while preserving a cheap local estimate for newly appended tool outputs, user messages, or safety context.

## Truncation Policy

Truncation occurs before prompt construction.

Rules:

- large tool outputs are truncated first
- large user inputs are truncated only when necessary and must preserve the leading task statement
- text truncation must preserve UTF-8 validity
- structured JSON outputs must remain syntactically valid when `preserve_json_shape` is enabled

Required invariant:

- truncation may reduce content, but it may not reorder or detach paired tool-call structures

## Compaction Trigger

Compaction must trigger when:

```text
estimated_total_tokens >= auto_compact_token_limit
```

Default threshold:

- 90 percent of the effective input budget, unless explicitly overridden by the model config

Compaction must also be callable manually.

## Eligibility Rules for Summarization

Protected content:

- current user input
- last `K` complete turns, where `K` defaults to 3
- any unresolved approval or tool interaction

Eligible content:

- older completed turns
- prior summaries when re-compaction is necessary

Hard invariants:

- tool calls and their outputs remain paired
- approval requests and decisions remain paired
- do not summarize partial turns

Summary model selection rules:

- summary generation must not implicitly switch between "active model" and "cheap summarizer" policies
- the summary model is selected from app-level configuration
- if `SummaryModelSelection::UseTurnModel` is configured, compaction uses the turn's resolved model
- if `SummaryModelSelection::UseConfiguredModel { model_slug }` is configured, compaction resolves that model through the normal model catalog
- summary-model resolution must fail before the summarization call starts if the configured model is missing or lacks required text capability

## Compaction Execution Flow

1. Compute prompt estimate.
2. Select eligible history prefix.
3. If selected content still exceeds summarizer limits, drop oldest eligible items while preserving pair invariants.
4. Build summarization prompt with explicit instructions:
   - preserve user goals
   - preserve decisions and file paths
   - preserve unresolved constraints
   - omit verbose tool output details
5. Resolve summary model from app configuration.
6. Invoke summarizer model call.
7. Persist summary as a new `ContextSummary` item.
8. Persist `CompactionSnapshot`.
9. Rebuild prompt view as:
   - summary item
   - uncompact recent turns
   - current input

## Summary Format

Summary payload schema:

```rust
pub struct ContextSummaryPayload {
    pub summary_text: String,
    pub covered_turn_sequences: Vec<u32>,
    pub preserved_facts: Vec<String>,
    pub open_loops: Vec<String>,
    pub generated_by_model: String,
}
```

The summary must be model-visible and human-readable. If encrypted storage is added later, store both encrypted and display-safe variants.

## Recoverability and Snapshots

The overview mentions snapshot storage, for example ghost-branch commits. The required contract is:

- raw history remains unchanged and fully recoverable
- compaction stores enough metadata to reconstruct which raw items were replaced in prompt view
- a session resume can rebuild the same compacted prompt view deterministically

Snapshot backend policy:

- JSON snapshot metadata is the canonical required backend
- git-backed ghost snapshots are an optional backend that may be enabled when the session workspace is inside a trusted git repository
- compaction logic must not depend on git availability for correctness
- if git snapshot creation fails, compaction must fall back to JSON-only snapshot persistence unless app config requires strict git snapshots

### JSON Snapshot Backend

The required first-line snapshot backend is a JSON record stored beside compaction metadata.

Required JSON snapshot contents:

- session and turn identifiers
- replaced item range
- summary item id
- prompt-segment ordering metadata required for deterministic rebuild
- model slug used for summary generation
- summary-model selection mode at the time of compaction
- optional workspace root and repo root hints for later recovery tooling

Rules:

- JSON snapshot persistence must succeed before the compaction result is considered durable
- JSON snapshots must be sufficient to rebuild compacted prompt view without accessing git
- JSON snapshots must not attempt to encode raw large file contents or working-tree blobs

### Optional Git Ghost Snapshot Backend

When enabled, the runtime may also capture a git-backed workspace snapshot modeled after Codex's ghost-commit flow.

Required design:

- create an unreferenced detached commit object rather than creating or moving a visible branch
- use a temporary git index so snapshot creation never mutates the user's real index state
- seed the temporary index from `HEAD` when a parent commit exists so unchanged tracked files remain represented
- stage tracked changes and selected untracked files into the temporary index
- write a tree from the temporary index
- create the detached commit with `git commit-tree`
- record the commit id and parent commit id in `CompactionSnapshot`

Tracked and untracked preservation rules:

- tracked working-tree state may be restored from the ghost commit later
- preexisting untracked or ignored files must be recorded separately and preserved during restore
- newly created untracked files introduced after the snapshot may be removed during restore only if they were not already present at snapshot time
- default-ignored large dependency/build directories may be excluded from the snapshot backend to avoid unbounded snapshot growth
- large untracked files may be excluded based on configured size thresholds and reported in snapshot warnings
- force-included ignored files may be explicitly added when a caller needs them preserved in the snapshot

Restore rules:

- restore must reset the working tree to the ghost commit content without overwriting the user's staged index state
- restore scope may be limited to the active repository subdirectory when the session workspace is below repo root
- ignored or untracked files that existed before the snapshot must remain preserved after restore
- ignored files created after the snapshot should remain untouched unless explicit cleanup mode is enabled later

### Ghost Snapshot Activation Rules

The runtime may create a git ghost snapshot only when all of the following are true:

- app config enables git-backed snapshots
- the workspace resolves inside a trusted git repository
- snapshot capture is operating on a local filesystem path
- the repository state can be inspected without path-escape violations

The runtime must use JSON-only snapshots when:

- the workspace is not in a git repository
- the repository is not trusted
- git is unavailable or git plumbing commands fail
- snapshot scope spans multiple unrelated repositories

### Snapshot Backend Responsibilities

`devo-core::context` owns:

- deciding whether a snapshot is required for a compaction event
- creating canonical JSON snapshot metadata
- recording which backend succeeded

`devo-core::code` or a future `devo-git` integration layer owns:

- trusted-repository detection
- detached commit capture and restore plumbing
- ignored/untracked preservation policies for git-backed snapshots

### Failure Handling

`SnapshotPersistFailed` must carry backend-specific context:

- `JsonSnapshotWriteFailed`
- `GitSnapshotUnavailable`
- `GitSnapshotCaptureFailed`
- `GitSnapshotRestoreFailed`

Rules:

- JSON snapshot failure is fatal for compaction durability
- git snapshot failure is non-fatal when JSON snapshot persistence succeeded and app config does not require git snapshots
- snapshot warnings about excluded large files or directories must be emitted to logs and optional UI events, but do not fail compaction

## Context Construction Pipeline

For every provider invocation:

1. gather base instructions
2. gather tool descriptions
3. gather safety constraints
4. build compacted or full history view
5. append current input
6. estimate tokens
7. if threshold exceeded, run compaction and rebuild
8. apply final truncation pass
9. serialize provider request

## Error Handling Strategy

`CompactionError` variants:

- `EstimateUnavailable`
- `SummaryProviderFailed`
- `InvariantViolation`
- `SnapshotPersistFailed`
- `CompactionNotPossible`

Behavior:

- if compaction fails proactively, continue only if current request is still under hard limit
- if provider reports context too long, run one reactive compaction attempt before failing the turn

## Concurrency and Async Model

- Only one compaction may run per session at a time.
- Compaction is a side flow but must complete before the blocked provider request is retried.
- Summary generation is an async provider call that must be cancellable.

## Persistence, Caching, and IO

- Persist summary items in the same session journal.
- Persist `CompactionSnapshot` in `summaries/` or `snapshots/`.
- In-memory estimate caches are allowed but must not be required for correctness.
- Git ghost snapshots, when enabled, must use detached commit objects and must not create visible working branches by default.

## Observability

Metrics:

- `context.estimate.tokens`
- `context.compact.count`
- `context.compact.duration_ms`
- `context.compact.tokens_saved`
- `context.truncation.applied.count`
- `context.snapshot.json.count`
- `context.snapshot.git.count`
- `context.snapshot.git.warning.count`

Logs must include:

- model slug
- estimated token totals
- compacted turn range
- tokens saved
- snapshot backend used
- ghost commit id when git snapshotting succeeds

## Security and Edge Cases

- Summaries must not reintroduce secrets that redaction removed from raw prompt material.
- Images or binary tool outputs must be summarized as text placeholders, not embedded raw.
- If the oldest eligible content cannot be compacted without breaking invariants, the turn must fail with a context limit error instead of emitting malformed history.

## Testing Strategy and Acceptance Criteria

Required tests:

- threshold trigger calculation
- last-K-turn preservation
- tool pair preservation
- reactive compaction after provider context error
- deterministic prompt reconstruction after resume
- JSON-only snapshot reconstruction
- git ghost snapshot capture without index mutation
- restore preserving preexisting ignored/untracked files
- fallback from git snapshot failure to JSON-only snapshot

Acceptance criteria:

- Long sessions remain operable without deleting raw history.
- Prompt views never contain orphaned tool or approval structures.
- A resumed session reconstructs the same compacted prompt view from persisted state.

## Dependencies With Other Modules

- Conversation provides the raw item journal.
- Language Model provides context window limits and truncation defaults.
- Safety contributes constraint segments that count toward budget.
- App Config provides summary-model selection and compaction defaults.

## Open Questions and Assumptions

Assumptions:

- The first implementation uses a single summarizer model call rather than multi-stage compaction.
- JSON snapshot metadata is always the canonical recovery record, even when git snapshots are enabled.

Open questions:

- Whether git-backed snapshot capture should stay in `devo-core::code` or move into a dedicated git integration module if the feature grows.
