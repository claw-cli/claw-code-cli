# devo Design
This document defines the specification for the devo coding agent. The documentation's goal is to guide implementation.

## 1. Overview
Detailed specification: [Architecture Overview](./spec-overview-architecture.md)

The target crate split is:

- `devo-core`: conversation, model integration, context management, coding workflow orchestration, long-running task execution, and shared runtime state
- `devo-tools`: built-in tool contracts and tool execution adapters
- `devo-safety`: sandboxing, redaction, approvals, and safety policy
- `devo-mcp`: MCP server lifecycle, discovery, and capability bridging
- `devo-protocol`: shared structured types across crates (shell summaries, provider wire API enums, etc.)
- `devo-server`: transport-neutral runtime API server
- `devo-client`: client library for connecting to devo-server
- `devo-tui`: interactive terminal UI (ratatui/crossterm)
- `devo-cli`: CLI binary entrypoint, config loading, command dispatch
- `devo-utils`: shared low-level utilities (home dir, config paths, etc.)
- `devo-arg0`: OS-specific arg0/binary path resolution
- `devo-tasks`: background task state and long-running execution
- `devo-file-search`: separate file-search binary for search functionality

All crates must implement observability. At minimum, each crate must emit structured logs, useful tracing spans for long-running operations, and crate-appropriate metrics. Logging and observability are not optional add-ons; they are part of the baseline implementation contract.

### 1.1 Conversation
The agent is organized into three hierarchical levels:

- Session: A conversation between a user and the devo agent. Each session contains multiple turns. Each session is identified by a UUID v7, which provides time-ordered uniqueness and enables efficient sorting and lookup.
- Turn: One turn of the conversation, typically starting with a user message and finishing with an agent message. Each turn contains multiple items.
- Item: Represents user inputs and agent outputs as part of the turn, persisted and used as the context for future conversations. Example items include user message, agent reasoning, agent message, shell command, file edit, etc.

The agent uses a JSONL (JSON Lines) format to record and persist execution data. Each item is appended as a new line. JSONL logs are partitioned into multiple folder files based on date (e.g., YYYY-MM-DD) and session ID.

Detailed specification: [Conversation](./spec-conversation.md)

### 1.2 Language Model
The model configuration is a complete specification of capabilities (tools, modalities, reasoning), constraints (context window, truncation, compaction) and behavior (base instructions, verbosity). The model config should be read from config json file, there is a array, each item is a model config follow below field design.

Model Identification
- `slug`: Unique identifier of the model, such as `deepseek-chat`
- `display_name`: Human-readable name for UI
- `description`: Optional description of the model

Reasoning Configuration, These fields control how deeply the model reasons during execution.
- `default_reasoning_level`: Default reasoning effort (e.g., low / medium / high)
- `supported_reasoning_levels`: Supported reasoning levels

Prompt
- `base_instructions`: System prompt that defines model behavior
- `model_messages`: Optional structured template for assembling instructions, such as developer roles.

Tooling
- `shell_type`: Defines how shell commands are executed.
- `apply_patch_tool_type`: Determines how file edits are applied
- `web_search_tool_type`: Defines how web search is executed
- `supports_parallel_tool_calls`: Whether multiple tool calls can run concurrently
- `supports_search_tool`: Whether search functionality is available
- `experimental_supported_tools`: List of additional experimental tools

Output Control
- `support_verbosity`: Whether verbosity control is supported
- `default_verbosity`: Default verbosity level

Reasoning Summarization
- `supports_reasoning_summaries`: Whether reasoning can be summarized
- `default_reasoning_summary`: Default summarization behavior

Context Management
- `context_window`: Maximum token capacity of the model
- `effective_context_window_percent`: Percentage of the context window usable for input.
  The remaining portion is reserved for, system prompts and tool overhead
- `auto_compact_token_limit`: Token threshold that triggers automatic context compaction. If not specified, it is derived from the context window (typically ~90%).

Truncation Policy
- `truncation_policy`: Defines how large payloads (e.g., tool outputs) are truncated This is applied at the item level to control token growth.

Modalities
- `input_modalities`: Supported input types (e.g., text, image)
- `supports_image_detail_original`: Whether high-detail image input is supported

Availability
- `visibility`: Whether the model is exposed to users
- `supported_in_api`: Whether the model is usable via API
- `priority`: Ordering used for model selection
- `availability_nux`: Optional onboarding or availability hints
- `upgrade`: Optional upgrade recommendation

Internal Metadata
- `used_fallback_model_metadata`: Internal flag indicating fallback resolution (not user-facing)

Detailed specification: [Language Model](./spec-language-model.md)

## 1.3 Safety
Safety refers to the set of mechanisms that ensure the agent operates within user-defined boundaries, prevents unintended or harmful actions, and protects sensitive data from exposure to language models. Including: preventing leakage of secrets through pattern match redaction; restricting tools execution, such as read, write, network through sandbox; ensuring user approval over permission; guiding the model through constraints in context.

1. Secret Protection: Sensitive information such as API keys, access tokens and credentials must never be exposed to the language model. Those data sent to the model must be sanitized beforehand. Secrets may be used by local tools during execution. Secrets must not appear in model input (prompt/context).
Use pattern-based or rule-based filtering (e.g., regex, key detection), Apply deterministic redaction before sending data to the model. Do not rely on language model for redaction. such as `r"sk-[A-Za-z0-9]{20,}"`, `r"\bAKIA[0-9A-Z]{16}\b"`, `r"(?i)\bBearer\s+[A-Za-z0-9._\-]{16,}\b"`, `r#"(?i)\b(api[_-]?key|token|secret|password)\b(\s*[:=]\s*)(["']?)[^\s"']{8,}"#`.

2. Access Control: The agent must enforce control, including shell commands and tool operations such as file access and network access. This is implemented through sandboxing, where the runtime environment constrains what operations are permitted at the system level. For file access, permissions are defined at the path level (e.g., read-only directories, writable directories), and for network access, restrictions can be applied based on destination (e.g., allowed hostnames or full denial). Implementations rely on OS-level sandboxing mechanisms such as Linux `seccomp` / `bubblewrap`, macOS `seatbelt`, and Windows restricted tokens, which limit system calls like `read`, `write` and `open`.In addition to enforcement, the system defines an Access Control Policy layer that determines how decisions are made: Unrestricted: No control is applied; all operations are allowed (not recommended). Static Policy: Predefined rules determine allowed and denied operations (e.g., read-only workspace, no network). Model-Guided Policy: A language model evaluates each requested operation and decides whether it should be executed directly or escalated for user approval. For model-guided control, a function is introduced that takes a proposed operation (e.g., command, file path, network request) and produces a decision: `decide(operation) -> { allow | deny | request_approval }`. If the decision is `request_approval`, the system pauses execution and forwards the request to the user.

3. User Approval: For operations that exceed current access control policy, explicit user approval is required. Approval must be explicit (no implicit escalation), explainable (including reason and risk), and fine-grained. Granularity can include per tool (e.g., allow shell but deny network), per command (single execution), per turn (current interaction), per session (persisted permission), or per resource (specific file path or hostname). Each approval request should clearly state what will be executed and why it is necessary. At the same time, system constraints must be translated into natural language and injected into the model context so the model can reason under these constraints. This includes sandbox boundaries, allowed and denied capabilities, current permission state, and past approval decisions. For example: "You are not allowed to access files outside the workspace.", "Network access is restricted unless explicitly approved.", or "The user denied access to `/etc` in the previous step." This ensures that model reasoning is guided by constraints proactively, rather than only being checked after execution.

Detailed specification: [Safety](./spec-safety.md)

**Execution Flow with Safety**
The agent follows a controlled loop:

1. User input arrives
2. System constructs context:
   - conversation history
   - sandbox constraints
   - permission state
   - available tools
3. Model generates response (may include tool call)
4. If tool call is requested:
   - check policy against current permissions
   - if additional permission required -> trigger approval
5. User decision:
   - Reject:
     - record rejection
     - inject rejection reason into context
     - model re-plans
   - Approve:
     - update permission policy (scoped: once / turn / session)
     - proceed with execution
6. Tool execution:
   - runs inside sandbox
   - has access to secrets if needed
7. Output processing:
   - apply redaction (non-LLM, rule-based)
   - remove sensitive data
8. Construct new context
9. Model continues reasoning

## 1.4 Context Management
Context Management refers to the set of mechanisms that allow the agent to operate continuously over long-running, multi-turn tasks despite the finite context window of the language model.

Objectives:
1. Stay within context window limits
2. Preserve important historical information
3. Prioritize recent and relevant context
4. Enable long-running task continuity
5. Maintain recoverability of full history

The agent must estimate the current context token usage of:
- system prompts
- tool descriptions
- conversation history

Since exact tokenization depends on model-specific tokenizers, estimation is typically performed locally using approximations. This estimation is used to determine when compaction is needed.

Compaction is triggered when the estimated context size exceeds a threshold. `trigger_compaction if total_tokens >= 90% of context_window`
This threshold may be:
- explicitly configured
- derived from model metadata

Compaction reduces context size by replacing historical content with a summary.
1. Select a portion of historical items (older turns)
2. Construct a summarization prompt
3. Invoke the model to generate a summary
4. Replace selected history with the summary

To maintain conversational quality: recent users message be preserve, only older history is eligible for summarization. `Keep last K turns intact, Summarize older turns`

Compaction alters the **context view**, not the **true history**. To preserve recoverability, snapshot is stored (e.g., git commit on a ghost branch). Context is a derived, compressed representation. This enables rollback to previous states, deterministic replay, user-controlled recovery.

If the selected content for summarization exceeds model limits: remove oldest items first, ensure structural consistency. Critical rule when removing: `Tool calls must remain paired (input + output)`. Invalid states such as tool input without output or partial execution traces must be avoided.

To prevent individual items from dominating context: large tool outputs are truncated, large user input are truncated. maximum length per item is enforced. Truncation is applied before items enter the context.

**Context Construction Pipeline**

For each model invocation:
1. Collect inputs:
   - base instructions
   - tool descriptions
   - safety constraints
   - conversation history (possibly compacted)
   - current user input
2. Estimate total token size
3. If near threshold:
   - trigger compaction
   - rebuild context
4. Apply truncation rules
5. Construct final prompt
6. Invoke model

**Compaction Execution Model**

Compaction itself is a separate model call: `History -> Summarization Prompt -> Model -> Summary -> Replace History Segment`.
This creates a dual-flow system:
- Main flow: task execution
- Side flow: context compression

Detailed specification: [Context Management](./spec-context-management.md)

## 1.5 Tools
Tools are a first-class subsystem. They are not just exposed model functions; they are typed runtime capabilities with validation, safety checks, execution backends, persistence records, and streaming events.

The first milestone should include at least:

- shell command execution (`bash` / `shell_command`)
- file search (`glob` for filenames, `grep` for content)
- file reading (`read`)
- file writing (`write` / `file_write`)
- patch or file editing support (`apply_patch`)
- web content fetching (`webfetch`)
- web search (`websearch`)
- sub-agent task dispatch (`task`)
- interactive user questions (`question`)

Tool execution must follow a stable lifecycle:

1. model requests a tool call
2. runtime validates input
3. safety checks approval and sandbox policy
4. runtime executes the tool
5. tool progress and terminal result are persisted as items
6. model continues with the structured result

Detailed specification: [Tools](./spec-tools.md)

## 1.6 MCP
MCP support allows `devo` to connect to external capability providers that expose tools, resources, and resource templates through the Model Context Protocol.

The MCP subsystem must support:

- configured server startup and refresh
- MCP tool discovery and exposure
- MCP resource and resource-template discovery
- auth and startup status reporting
- elicitation and approval integration when MCP servers need client interaction

Detailed specification: [MCP](./spec-mcp.md)

## 1.7 Skills
Skills are reusable instruction bundles that can be discovered from disk, listed to clients, and injected into turns as structured context.

The skills subsystem must support:

- skill discovery from user and workspace roots
- stable metadata listing
- change detection and cache invalidation
- explicit structured skill references in turn input

Detailed specification: [Skills](./spec-skills.md)

## 1.8 Server API
The agent runtime exposes a server API designed for integration with various user interfaces such as CLI tools, desktop applications, and IDE extensions. The API supports two transports: stdio (for local, process-based communication) and WebSocket (for networked or remote clients), follows a JSON-RPC 2.0 protocol (with the `"jsonrpc":"2.0"` header omitted on the wire) for structured request-response interactions. In addition to standard method calls for driving agent behavior (e.g., submitting user input, controlling execution), the API provides an event subscription mechanism, allowing clients to receive real-time updates such as streaming model outputs, tool execution progress, approval requests, and state changes.

- stdio (`--listen stdio://`, default): newline-delimited JSON (JSONL)
- websocket (`--listen ws://IP:PORT`): one JSON-RPC message per websocket text frame

For each connection, here is lifecycle.
For ensuring capability negotiation and a well-defined protocol state before execution begins.
- Initialize (per connection): After establishing a transport (stdio or WebSocket), the client must send an `initialize` request with client metadata, followed by an `initialized` notification. Any request before this handshake is rejected.
For providing a stable container for long-lived state, recovery, and branching.
- Start or resume a Session: A Session represents a persistent conversation. Call `session/start` to create a new session, `session/resume` to continue an existing one, or `session/fork` to branch from prior history. Sessions may be ephemeral (in-memory only).
For isoloating each iteraction into a controllable unit of execution.
- Start a Turn: A Turn represents one execution cycle within a Session. Call `turn/start` with the `sessionId` and user input. Optional overrides (model, sandbox, approvals, etc.) can be provided per turn.
For enabling real-time streaming and fine-grained observability of agent behavior.
- Stream Items: During a Turn, the system emits a stream of Item events (e.g., `item/started`, `item/delta`, `item/completed`). Items represent atomic units such as messages, tool calls, tool results, and reasoning steps.
- Complete the Turn: When execution finishes (or is interrupted via `turn/interrupt`), the system emits `turn/completed` with final state and token usage.

Detailed specification: [Server API](./spec-server-api.md)

## 1.9 App Config
The agent requires an application-level configuration layer separate from the model catalog. App config defines cross-cutting runtime defaults such as default model selection, summary-model selection, safety defaults, server defaults, and logging behavior. User-level config and project-level config are merged into one normalized runtime config before session execution begins.

Summary generation must use an explicit configuration setting rather than an implicit architecture choice. The runtime must support both:

- using the current turn model for compaction
- using a separately configured model slug for compaction

Detailed specification: [App Config](./spec-app-config.md)

## 1.10 Observability
Observability is a cross-cutting requirement for the whole system.

Every crate must contribute:

- structured logs
- tracing spans around important async or long-running work
- metrics for success, failure, latency, and high-value counters

Observability must cover at least:

- session and turn lifecycle
- model calls and retries
- tool execution and truncation
- approval requests and decisions
- sandbox selection and failures
- context compaction and snapshotting
- API request handling and event delivery
- config loading and validation

Rules:

- logs must be structured and machine-parseable
- secrets must never be written to logs, traces, or metrics labels
- correlation identifiers such as `session_id`, `turn_id`, `item_id`, and request ids should be attached whenever applicable
- failures must produce actionable diagnostics rather than generic error text

Detailed specification: [Architecture Overview](./spec-overview-architecture.md)

## 2. Detailed Specifications
The detailed specifications below are the implementation-facing source of truth for the design:

- [Architecture Overview](./spec-overview-architecture.md)
- [Conversation](./spec-conversation.md)
- [Language Model](./spec-language-model.md)
- [Safety](./spec-safety.md)
- [Context Management](./spec-context-management.md)
- [Tools](./spec-tools.md)
- [MCP](./spec-mcp.md)
- [Skills](./spec-skills.md)
- [Server API](./spec-server-api.md)
- [App Config](./spec-app-config.md)
- [Response Item IR](./spec-response-item.md)
- [Interactive TUI](./spec-interactive-tui-v2.md)
- [Observability via Architecture Overview](./spec-overview-architecture.md)
