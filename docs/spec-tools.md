# devo Detailed Specification: Tools

## Background and Goals

`devo` is a tool-using coding agent, so tools are not an implementation detail. They are a first-class runtime subsystem that sits between model output, safety policy, execution backends, and conversation persistence.

Primary goals:

- define a typed tool contract rather than free-form command dispatch
- separate tool declaration, planning-time exposure, approval checks, and execution
- make every tool invocation replayable and observable
- support built-in and runtime-native tools owned by `devo-tools`

## Scope

In scope:

- built-in tool interfaces and execution contracts
- tool registration and exposure to models
- shell-command and file-search tool design
- tool execution lifecycle, result shaping, and error handling
- safety, sandbox, and approval integration

Out of scope:

- MCP server lifecycle, discovery, and transport details
- skill discovery and skill injection behavior
- provider-specific tool-call wire formats
- UI rendering of tool events

Related specifications:

- MCP-specific external capability integration is defined in [spec-mcp.md](./spec-mcp.md)
- skill discovery and structured skill references are defined in [spec-skills.md](./spec-skills.md)

## Module Responsibilities and Boundaries

`devo-tools` owns:

- built-in tool trait definitions
- tool registry and lookup
- tool schema exposure to the language model layer
- normalized tool input parsing and validation
- tool execution orchestration entry points
- normalized tool result and tool error shaping

`devo-safety` owns:

- approval checks before execution
- sandbox policy derivation
- path/network permission enforcement
- secret-aware redaction of tool-visible and model-visible payloads

`devo-core::conversation` owns:

- persisted `ToolCall`, `ToolResult`, and `ToolProgress` items
- correlation between tool invocation and session history

`devo-core::runtime` owns:

- turn-level orchestration
- tool-call scheduling
- cancellation and interruption propagation

`devo-mcp` owns:

- MCP server discovery and MCP tool/resource bridging before those capabilities are handed to `devo-tools`

## Core Data Structures

```rust
pub struct ToolName(pub SmolStr);

pub struct ToolCallId(pub Uuid);
```

```rust
pub struct ToolDefinition {
    pub name: ToolName,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_mode: ToolOutputMode,
    pub capability_tags: Vec<ToolCapabilityTag>,
    pub needs_approval: ApprovalHint,
}
```

```rust
pub enum ToolOutputMode {
    StructuredJson,
    Text,
    Mixed,
}

pub enum ToolCapabilityTag {
    ReadFiles,
    WriteFiles,
    ExecuteProcess,
    NetworkAccess,
    SearchWorkspace,
    ReadImages,
}

pub enum ApprovalHint {
    Never,
    Maybe,
    Always,
}
```

```rust
pub struct ToolInvocation {
    pub tool_call_id: ToolCallId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_name: ToolName,
    pub input: serde_json::Value,
    pub requested_at: DateTime<Utc>,
}
```

```rust
pub struct ToolExecutionContext {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub cwd: PathBuf,
    pub policy_snapshot: PolicySnapshot,
    pub app_config: Arc<AppConfig>,
    pub cancellation: CancellationToken,
}
```

```rust
pub enum ToolExecutionOutcome {
    Completed(ToolResultPayload),
    Failed(ToolFailure),
    Denied(ToolDenied),
    Interrupted,
}
```

```rust
pub struct ToolResultPayload {
    pub content: ToolContent,
    /// Optional UI-only text. Model replay and persisted canonical output must
    /// continue using `content`; clients may use this for cleaner display.
    pub display_content: Option<String>,
    pub metadata: ToolResultMetadata,
}

pub enum ToolContent {
    Text(String),
    Json(serde_json::Value),
    Mixed {
        text: Option<String>,
        json: Option<serde_json::Value>,
    },
}
```

Current implementation note: tool handlers return `Box<dyn ToolOutput>` from
`invocation.rs`; `FunctionToolOutput` is the standard concrete implementation,
and legacy text-plus-metadata results should be represented as `ToolContent::Mixed`.
`display_content` is best-effort display text, not a replacement for canonical
tool content. The `read` tool uses it to show file bodies without `<content>` and
directory listings without `<entries>`, while keeping the full tagged output for
the model, history replay, and provider context.

## Tool Trait Contract

```rust
#[async_trait]
pub trait ToolHandler: Send + Sync {
    fn tool_kind(&self) -> ToolHandlerKind;

    async fn handle(
        &self,
        invocation: ToolInvocation,
        progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError>;
}
```

```rust
pub trait ToolRegistry: Send + Sync {
    fn get(&self, name: &ToolName) -> Option<Arc<dyn Tool>>;
    fn list(&self) -> Vec<ToolDefinition>;
}
```

Rules:

- tools must declare a stable schema before being exposed to the model
- schema validation runs before approval resolution and before backend execution
- tools must return normalized structured outcomes, not raw backend process objects
- tools must not write directly to session history; they report through runtime orchestration
- MCP-discovered tools may be adapted into this contract, but MCP transport and discovery are not specified here

## Tool Execution Lifecycle

1. runtime receives a model tool-call request
2. runtime resolves the tool by name from `ToolRegistry`
3. runtime validates the input against the declared schema
4. runtime persists a `ToolCall` item
5. safety resolves approval and sandbox requirements
6. if approval is needed, runtime emits an `ApprovalRequest` item and pauses the turn
7. after approval, runtime invokes `Tool::execute`
8. tool may emit zero or more progress updates through `ToolProgressReporter`
9. runtime normalizes the terminal outcome into a `ToolResult` item
10. runtime resumes model execution or ends the turn

Rules:

- every accepted tool call must have exactly one terminal result item: completed, denied, failed, or interrupted
- progress items are optional and may be omitted for very fast tools
- approval denial must still produce a terminal tool-result-shaped item so replay remains complete

## Built-in Tool Set

The current built-in tools in `devo-tools` include:

- `bash` / `shell_command` — shell command execution
- `apply_patch` — file editing via patch application
- `read` — file reading
- `write` / `file_write` — file writing
- `glob` — filename pattern matching
- `grep` — content search with regex
- `webfetch` — HTTP content fetching
- `websearch` — web search
- `task` — sub-agent task dispatch
- `question` — interactive user questions
- `skill` — skill loading
- `plan` — plan mode support
- `lsp` — language server protocol integration

## Shell Command Tool

### Responsibilities

The shell command tool runs a subprocess within a controlled execution environment and returns normalized stdout, stderr, exit status, timing, and truncation metadata.

### Input Contract

```rust
pub struct ShellCommandInput {
    pub command: String,
    pub workdir: Option<PathBuf>,
    pub timeout_ms: Option<u64>,
    pub environment: Option<BTreeMap<String, String>>,
    pub escalation: Option<ShellEscalationRequest>,
}
```

```rust
pub struct ShellEscalationRequest {
    pub justification: String,
    pub sandbox_permissions: SandboxPermissionMode,
    pub prefix_rule: Option<Vec<String>>,
}
```

Rules:

- `command` is required and is interpreted by the configured shell adapter
- `workdir` defaults to the current session workspace root
- `timeout_ms` must be clamped to an app-configured maximum
- environment overrides are additive and must be filtered by the safety layer
- escalation request fields are metadata for approval and execution policy, not free authorization
- command-prefix approvals are only offered for simple commands that can be parsed into a stable executable/subcommand prefix
- commands containing shell control operators, redirection, command substitution, environment-variable substitution, wildcard patterns, or other ambiguous shell features must not be cached by command prefix; they may still be approved for a narrower once/turn/session/tool scope

### Execution Contract

Execution steps:

1. normalize the working directory
2. classify the command for sandbox and approval purposes
3. merge the requested execution policy with the active policy snapshot
4. build the platform-specific sandbox runner
5. spawn the process
6. stream stdout and stderr chunks to the reporter when configured
7. enforce timeout and cancellation
8. collect exit status and truncate terminal payloads if needed
9. return a normalized `ToolResultPayload`

### Output Contract

```rust
pub struct ShellCommandResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub timed_out: bool,
    pub truncated_stdout: bool,
    pub truncated_stderr: bool,
}
```

Rules:

- stdout and stderr must be captured separately
- large output must be truncated deterministically with explicit truncation flags
- exit-code absence means the process was interrupted before a normal exit
- terminal payloads written to model-visible history must pass through redaction before persistence or model reuse

## File Search Tool

### Responsibilities

The file search tool provides fast workspace search over file names and file contents. It should expose a structured capability similar in spirit to Codex file search, while allowing the backend implementation to prefer `rg` when available.

### Input Contract

```rust
pub struct FileSearchInput {
    pub query: String,
    pub mode: FileSearchMode,
    pub roots: Option<Vec<PathBuf>>,
    pub glob: Option<Vec<String>>,
    pub case_sensitive: bool,
    pub max_results: Option<u32>,
}
```

```rust
pub enum FileSearchMode {
    Content,
    FileName,
    Auto,
}
```

Rules:

- search roots default to the workspace root
- all roots must be normalized and checked against safety policy before execution
- backend should prefer `rg` or equivalent indexed fast search when available
- `max_results` must be clamped to a configured maximum

### Output Contract

```rust
pub struct FileSearchResult {
    pub mode: FileSearchMode,
    pub matches: Vec<FileSearchMatch>,
    pub truncated: bool,
}

pub struct FileSearchMatch {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub preview: String,
}
```

Rules:

- results must be normalized to workspace-relative or canonical paths according to API context, but persistence should keep one stable representation
- previews must be line-oriented and size-bounded
- backend-specific error text must not leak directly into model-facing output

## Configuration Definitions

Tool-related config belongs partly in model config and partly in app config.

App-level tool config:

```rust
pub struct ToolRuntimeConfig {
    pub enabled_tools: Vec<String>,
    pub shell: ShellToolConfig,
    pub file_search: FileSearchToolConfig,
    pub max_parallel_read_tools: u16,
}
```

```rust
pub struct ShellToolConfig {
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub stream_output: bool,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
}

pub struct FileSearchToolConfig {
    pub prefer_rg: bool,
    pub max_results: u32,
    pub max_preview_bytes: usize,
}
```

Rules:

- a tool must be disabled centrally even if a model claims to support it
- model config controls exposure compatibility, but app config controls operational enablement

## Error Handling Strategy

`ToolExecuteError` variants should include:

- `UnknownTool`
- `InvalidInput`
- `ApprovalRequired`
- `PermissionDenied`
- `SandboxUnavailable`
- `ExecutionFailed`
- `Timeout`
- `Interrupted`
- `Internal`

Behavior:

- invalid input is a tool-call failure, not a runtime panic
- unknown tools must be surfaced back into the turn as structured errors
- backend process errors must be normalized into stable error codes plus human-readable messages
- tool failure must not corrupt session persistence; the tool result is still appended

## Concurrency and Async Model

- tools execute under `tokio`
- read-only tools such as file search may run concurrently when the model supports parallel tool calls and safety policy allows it
- mutating tools must serialize against the session writer and any relevant workspace lock
- one tool execution must have exactly one cancellation token derived from the owning turn

## Persistence and IO Behavior

- every tool call and terminal outcome is persisted as conversation items
- large raw tool output may be truncated in item payloads, but truncation metadata must be preserved
- tools may use temporary files internally, but these are implementation details and not part of canonical history

## Observability

Required logs and metrics:

- `tools.call.started`
- `tools.call.completed`
- `tools.call.failed`
- `tools.call.denied`
- `tools.shell.duration_ms`
- `tools.file_search.duration_ms`
- `tools.output.truncated.count`

Tracing spans:

- `tool.resolve`
- `tool.validate`
- `tool.approval`
- `tool.execute`

## Security and Edge Cases

- shell tool execution must never bypass the safety subsystem
- file search must not escape approved roots through symlink traversal without an explicit policy decision
- environment variable forwarding must be allowlisted or explicitly filtered
- tool output containing secrets must be redacted before model reuse
- cancellation must terminate child processes or process groups according to platform capability

## Testing Strategy and Acceptance Criteria

Required tests:

- tool registry lookup and disabled-tool filtering
- shell command input validation and timeout behavior
- shell command truncation and redaction behavior
- file search result normalization and max-result clamping
- approval-required shell execution path
- interrupted tool execution produces one terminal result item

Acceptance criteria:

- runtime can expose a typed tool catalog to models without backend-specific leaking
- shell command and file search both execute through the same lifecycle: validate, approve, run, persist, emit events
- tool calls are replayable from persisted session history

## Dependencies With Other Modules

- Conversation persists tool-call, progress, and result items
- Safety governs approval, sandboxing, path access, and redaction
- Language Model consumes tool definitions when building tool-enabled prompts
- Server API surfaces tool events and approval requests to clients

## Open Questions and Assumptions

Assumptions:

- `devo-tools` remains the crate for tool contracts and built-in tool implementations
- shell command and file search are required in the first milestone

Open questions:

- whether file search should eventually support indexed semantic search in addition to literal search
