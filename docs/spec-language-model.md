# Language Model

## Background and Goals

The overview defines model configuration as a complete description of:

- Capabilities
- Constraints
- Behavior

This specification defines the model catalog, prompt assembly inputs, provider-facing request model, and fallback behavior required to implement that contract.

## Scope

In scope:

- Model catalog schema.
- Config loading and validation.
- Prompt assembly inputs.
- Context window, compaction, verbosity, reasoning, and truncation controls.
- Rust interfaces inside `devo-core` between model selection, prompt building, and provider adapters.

Out of scope:

- Provider transport details.
- Billing, account tiers, or UI model picker rendering.

## Reference Rationale

The overview defines the model contract and budget knobs but does not prescribe a tokenizer implementation. For local budgeting, the required baseline is a deterministic byte-heuristic estimator because it:

- is deterministic and cheap enough to run before every turn
- works without provider-specific tokenizer dependencies
- can be reconciled against actual post-response usage when providers report authoritative counts
- handles structured items, encoded reasoning, and inline images in a way that is stable across providers

## Module Responsibilities and Boundaries

`devo-core::model_catalog` and `devo-core::model_preset` own:

- Model catalog loading from embedded JSON and filesystem overrides.
- Capability lookup and model resolution.
- Conversion from raw `ModelPreset` to runtime `Model`.

`devo-core` owns:

- Selecting a model for a turn.
- Passing prompt and tool definitions to the provider.
- Enforcing config-derived constraints during prompt construction.

## Model Catalog File Format

The built-in model catalog is embedded at compile time from `crates/core/models.json`. At runtime, optional override files are merged on top:

Required path:

```text
~/.devo/models.json
```

Optional project override:

```text
<workspace>/.devo/models.json
```

Merge order by `slug`: built-in defaults < user file < project file. On first
run, the built-in catalog is automatically copied to `~/.devo/models.json`.

```rust
pub struct Model {
    pub slug: String,
    pub display_name: String,
    pub provider: ProviderWireApi,
    pub channel: Option<String>,           // vendor/brand grouping (e.g. "DeepSeek")
    ...
}
```

The `channel` field groups models by vendor for UI display (onboarding,
model picker).

## Core Data Structures

```rust
pub struct Model {
    pub slug: String,
    pub display_name: String,
    pub provider: ProviderWireApi,
    pub description: Option<String>,
    pub thinking_capability: ThinkingCapability,
    pub default_reasoning_effort: Option<ReasoningEffort>,
    pub thinking_implementation: Option<ThinkingImplementation>,
    pub base_instructions: String,
    pub context_window: u32,
    pub effective_context_window_percent: Option<u8>,
    pub truncation_policy: TruncationPolicyConfig,
    pub input_modalities: Vec<InputModality>,
    pub supports_image_detail_original: bool,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<f64>,
    pub max_tokens: Option<u32>,
}
```

```rust
pub struct ModelPreset {
    pub slug: String,
    pub display_name: String,
    pub provider: ProviderWireApi,
    pub description: Option<String>,
    pub thinking_capability: ThinkingCapability,
    pub supported_reasoning_levels: Vec<ReasoningEffort>,
    pub default_reasoning_effort: Option<ReasoningEffort>,
    pub thinking_implementation: Option<ThinkingImplementation>,
    pub base_instructions: String,
    pub context_window: u32,
    pub effective_context_window_percent: Option<u8>,
    pub truncation_policy: TruncationPolicyConfig,
    pub input_modalities: Vec<InputModality>,
    pub supports_image_detail_original: bool,
    pub api_configured: bool,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<f64>,
    pub max_tokens: Option<u32>,
    pub priority: i32,
}
```

`ModelPreset` is deserialized from the JSON catalog and converted into the runtime `Model` type via `From<ModelPreset>`. The runtime works exclusively with `Model`.

Relevant enums:

- `ThinkingCapability = Unsupported | Toggle | ToggleWithLevels(Vec<ReasoningEffort>) | Levels(Vec<ReasoningEffort>)`
- `ReasoningEffort = Low | Medium | High | XHigh`
- `InputModality = Text | Image`
- `ProviderWireApi = OpenAIChatCompletions | OpenAIResponses | AnthropicMessages`

## Interface Definitions

```rust
pub trait ModelCatalog {
    fn list_visible(&self) -> Vec<&Model>;
    fn get(&self, slug: &str) -> Option<&Model>;
    fn resolve_for_turn(&self, requested: Option<&str>) -> Result<&Model, ModelError>;
}
```

```rust
pub struct PresetModelCatalog {
    models: Vec<Model>,
}
```

`PresetModelCatalog` is the concrete implementation. It loads the built-in model catalog from embedded JSON (`crates/core/models.json`) at compile time as a fallback. At runtime, user-level and project-level `models.json` files may override or extend the catalog.

## Prompt Assembly

Prompt assembly inputs, in order:

1. Base instructions from `Model.base_instructions`
2. Safety constraint messages
3. Context summary items
4. Full recent conversation items
5. Current user input
6. Tool definitions

Requirements:

- Structured prompt sections must be deterministic.
- The same input history and model config must produce the same prompt envelope.
- Tool descriptions are included only when the model supports the requested tools.

## Context Window and Budget Computation

Effective input budget:

```text
effective_input_budget =
  floor(context_window * effective_context_window_percent / 100)
```

Auto-compact threshold:

- If `auto_compact_token_limit` is set, use it.
- Otherwise compute `floor(effective_input_budget * 0.90)`.

Token estimation baseline:

- Preflight token budgeting must use the shared context-management estimator defined in [spec-context-management.md](./spec-context-management.md).
- That estimator must operate on the normalized prompt-visible view, not raw stored history.
- It must estimate ordinary items from serialized model-visible bytes, apply special handling for encoded reasoning content, and replace inline image payload bytes with fixed or patch-derived image costs before converting bytes to tokens.
- If provider usage is returned after a successful response, future turn budgeting must treat that usage as the authoritative baseline and only re-estimate newly appended local prompt material.
- Model selection, auto-compaction, and output-headroom reservation must all use this same estimator contract so budgeting decisions are internally consistent.

Max output tokens:

- Turn settings may override default output cap.
- The provider layer must reject values that exceed provider hard limits.

## Truncation Policy

The model config owns item-level truncation behavior.

Required config shape:

```rust
pub struct TruncationPolicyConfig {
    pub default_max_chars: usize,
    pub tool_output_max_chars: usize,
    pub user_input_max_chars: usize,
    pub binary_placeholder: String,
    pub preserve_json_shape: bool,
}
```

Rules:

- Truncation happens before prompt serialization.
- Tool outputs use shape-preserving truncation when the content is JSON.
- Binary payloads are replaced with placeholder text and metadata.

## State Transitions and Lifecycle

Model resolution for a turn:

1. Load catalog via `PresetModelCatalog::load()`.
2. Resolve requested slug or fall back to the highest-priority visible model.
3. Validate turn-level overrides.
4. Derive effective context budget and tool capability set.
5. Pass resolved `Model` to prompt builder and provider.

Fallback behavior:

1. Provider error indicates unsupported model or routing failure.
2. Resolve fallback candidate by same provider family or configured upgrade target.
3. Record fallback metadata in turn metadata.
4. Rebuild request if capabilities differ.

## Error Handling Strategy

`ModelError` variants:

- `ModelNotFound`
- `NoVisibleModels`

## Concurrency and Async Model

- Catalog loading may be cached behind `ArcSwap` or `RwLock`.
- Provider invocation remains async and streaming.
- Fallback resolution must occur inside the same turn without racing a second active provider call.

## Persistence, Caching, and IO

- Model catalog is loaded lazily at startup and file-watched only if hot reload is added later.
- Resolved model slug and fallback metadata are persisted in `TurnRecord`.
- Byte-estimate caches, including image-cost caches, may be in-memory only.

## Observability

Required fields in logs and metrics:

- `model_slug`
- `provider_name`
- `context_window`
- `effective_input_budget`
- `auto_compact_token_limit`
- `fallback_used`

Metrics:

- `provider.request.duration_ms`
- `provider.stream.first_token_ms`
- `provider.error.count`
- `provider.fallback.count`

## Security and Edge Cases

- Hidden models must not be exposed through public API listing unless explicitly allowed.
- Models that do not support images must receive image-stripped prompt material.
- A project-level catalog override must not silently remove required built-in models without a warning.

## Testing Strategy and Acceptance Criteria

Required tests:

- Catalog merge behavior.
- Context budget derivation.
- Fallback selection.
- Capability mismatch rejection.
- Prompt assembly ordering.

Acceptance criteria:

- A turn can resolve its model config without UI-specific state.
- Prompt assembly and token budgeting are deterministic and use the shared byte-heuristic estimator contract described in the context-management spec.
- Unsupported tool or modality combinations fail before a provider request is sent.

## Dependencies With Other Modules

- Conversation provides prompt-view items.
- Context Management provides summary and truncation inputs.
- Safety injects constraint instructions and redaction behavior.
- API exposes the model list and per-turn overrides.

## Open Questions and Assumptions

Assumptions:

- Model catalogs remain JSON for compatibility.

Open questions:
.
- Whether `upgrade` should be only advisory metadata or an automatic fallback hint.
