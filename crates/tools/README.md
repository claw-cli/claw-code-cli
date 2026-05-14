# crate: devo-tools

## protocol

This section summarizes the tool-calling protocol shapes used by the currently targeted model providers: OpenAI Chat Completions, OpenAI Responses API, Anthropic Messages API.

### OpenAI chat completion

In OpenAI Chat Completions, tool results are sent back to the model as role = "tool" messages. Tool calls are emitted by the model in assistant deltas under choices[].delta.tool_calls[]. Arguments for function tools are emitted as strings and are not guaranteed to be valid JSON.

The tool definition at request (client -> LLM) is

```typescript
type ChatCompletionFunctionTool = {
  /** Tool type. Currently only "function" is supported. */
  type: "function"

  function: {
    /** The name of the function to be called. Must be a-z, A-Z, 0-9, or contain underscores and dashes, with a maximum length of 64. */
    name: string

    /** A description of what the function does, used by the model to choose when and how to call the function. */
    description?: string

    /** The parameters the functions accepts, described as a JSON Schema object. See the guide for examples, and the JSON Schema reference for documentation about the format. Omitting parameters defines a function with an empty parameter list. */
    parameters?: JSONSchema

    /** Whether to enable strict schema adherence when generating the function call. If set to true, the model will follow the exact schema defined in the parameters field. Only a subset of JSON Schema is supported when strict is true. */
    strict?: boolean
  }
}

type ChatCompletionCustomTool = {
  /** Tool type discriminator. */
  type: "custom"

  custom: {
    /**
     * Unique tool name used in tool calls.
     */
    name: string

    /**
     * Optional description used by the model for tool selection.
     */
    description?: string

    /**
     * Input format specification for the tool.
     * Defaults to unconstrained text if omitted.
     */
    format?: TextFormat | GrammarFormat
  }
}

type TextFormat = {
  /**
   * Format discriminator.
   * Indicates unconstrained free-form text input.
   */
  type: "text"
}

type GrammarFormat = {
  /**
   * Format discriminator.
   * Indicates grammar-constrained input.
   */
  type: "grammar"

  grammar: {
    /**
     * Grammar definition string.
     */
    definition: string

    /**
     * Grammar syntax type.
     * - "lark": full grammar
     * - "regex": regular expression
     */
    syntax: "lark" | "regex"
  }
}

/**
 * Tool definitions for OpenAI Chat Completions.
 *
 * Includes:
 * - Function tools (JSON-schema based)
 * - Custom tools (text / grammar-based input)
 */
type ChatCompletionTools = Array<
  ChatCompletionFunctionTool | ChatCompletionCustomTool
>
```

The tool_call request (client -> LLM) is

```typescript
/**
 * Message list used in a Chat Completions request.
 *
 * After a tool is executed, the client appends a tool message to `messages`
 * so the model can continue generation with the tool result.
 */
type ChatCompletionMessageParam =
  | ChatCompletionAssistantMessageParam
  | ChatCompletionToolMessageParam

/**
 * Assistant message type.
 *
 * Defined elsewhere in the Chat Completions protocol.
 */
type ChatCompletionAssistantMessageParam = unknown

/**
 * Tool message returned by the client after executing a tool call.
 */
type ChatCompletionToolMessageParam = {
  /** Message role. Always "tool". */
  role: "tool"

  /**
   * Identifier of the tool call being answered.
   * Must match the provider-issued tool call id from the model output.
   */
  tool_call_id: string

  /**
   * Tool result content.
   * May be plain text or an array of text parts.
   * For tool messages, only text parts are supported.
   */
  content: string | ChatCompletionContentPartText[]
}

/**
 * Text content part used in tool result messages.
 */
type ChatCompletionContentPartText = {
  /** Content part type. Always "text". */
  type: "text"

  /** Text content of the tool result. */
  text: string
}

/**
 * Full request shape relevant to tool-result round-trips.
 */
type ChatCompletionToolCallRequest = {
  /**
   * Conversation messages sent to the model.
   * Includes prior assistant messages and tool result messages.
   */
  messages: ChatCompletionMessageParam[]
}
```

for openai chat completion, the tool_call reponse (LLM -> client) is 

```typescript
/**
 * Tool result message sent back to OpenAI Chat Completions.
 *
 * This message is appended to `messages` after the client executes a tool call.
 */
type ChatCompletionToolMessageParam = {
  /** The role of the message author. Always "tool". */
  role: "tool"

  /**
   * Provider-issued identifier of the tool call being answered.
   * This must match the `id` of the corresponding tool call emitted by the model.
   */
  tool_call_id: string

  /**
   * Tool result content.
   * May be plain text or an array of text parts.
   * For tool messages, only text parts are currently supported.
   */
  content: string | ChatCompletionContentPartText[]
}

/**
 * Text content part used in tool result messages.
 */
type ChatCompletionContentPartText = {
  /** Content part type. Always "text". */
  type: "text"

  /** Text content of the tool result. */
  text: string
}

/**
 * Streaming tool call delta emitted by OpenAI Chat Completions for function tools.
 *
 * Path:
 * `choices[].delta.tool_calls[]`
 */
type ChatCompletionFunctionToolCallDelta = {
  /**
   * Index of the tool call in the streamed `tool_calls` array.
   * Used to assemble partial tool calls across multiple deltas.
   */
  index: number

  /**
   * Provider-issued identifier of the tool call.
   * May be absent in early streaming chunks.
   */
  id?: string

  /** Tool type discriminator. */
  type?: "function"

  function?: {
    /**
     * Name of the function being invoked.
     * May be absent in early streaming chunks.
     */
    name?: string

    /**
     * Function arguments encoded as a JSON string.
     * May be partial, malformed, or incomplete during streaming.
     * Must be validated by the client before execution.
     */
    arguments?: string
  }
}

/**
 * Tool call emitted by OpenAI Chat Completions for custom tools.
 */
type ChatCompletionCustomToolCall = {
  /** Provider-issued identifier of the tool call. */
  id: string

  /** Tool type discriminator. */
  type: "custom"

  custom: {
    /** Name of the custom tool being invoked. */
    name: string

    /**
     * Raw input generated for the custom tool.
     * Not guaranteed to be valid JSON.
     */
    input: string
  }
}

/**
 * Tool call item emitted by OpenAI Chat Completions.
 *
 * Function tools are typically streamed through `choices[].delta.tool_calls[]`.
 * Custom tools are represented as complete tool call objects.
 */
type ChatCompletionToolCall =
  | ChatCompletionFunctionToolCallDelta
  | ChatCompletionCustomToolCall

/**
 * Relevant message payload used when returning tool results to the model.
 */
type ChatCompletionMessages = Array<
  ChatCompletionAssistantMessageParam | ChatCompletionToolMessageParam
>

/**
 * Placeholder for assistant messages already defined elsewhere in the protocol.
 */
type ChatCompletionAssistantMessageParam = unknown
```

### OpenAI responses

The responses API add support for MCP tools, Built-in Tools. here we just utilize "Function calls" (custom tools).
For mcp tools, built-in tools, they are all 'built-in' tools, here for local coding agent is unnecerssary.

For function calls. here is the parameter request (client -> LLM)

- tools
 - Function : Type Defines a function in your own code the model can choose to call. Learn more about function calling.
  - name: string  The name of the function to call.
  - parameters: map[unknown]   A JSON schema object describing the parameters of the function.
  - type: "function" The type of the function tool. Always "function"
  - defer_loading: optional boolean Whether this function is deferred and loaded via tool search.
  - description: optional string A description of the function. Used by the model to determine whether or not to call the function.

For function calls , here is the response (LLM -> client)

- output
 - FunctionCall  A tool call to run a function. See the function calling guide for more information.
  - arguments  string     A JSON string of the arguments to pass to the function.
  - call_id   string     The unique ID of the function tool call generated by the model
  - name    string       The name of the function to run
  - type    "function_call"    The type of the function tool. Always "function_call"
  - id     optional string       The unique ID of the function tool call
  - namespace   optional string The namspace of the function to run
  - status     optional "in_progress" or "completed" or "incomplete"   The status of the item. One of in_progress, completed, or incomplete. Populated when items are returned via API.
 - CustomToolCall      A call to a custom tool created by the model.
  - call_id: string    An identifier used to map this custom tool call to a tool call output.
  - input: string      The input for the custom tool call generated by the model.
  - name: string       The name of the custom tool being called.
  - type: "custom_tool_call"      The type of the custom tool call. Always custom_tool_call.
  - id: optional string           The unique ID of the custom tool call in the OpenAI platform.
  - namespace: optional string    The namespace of the custom tool being called.

### Anthropic messages

/v1/messages , Send a structured list of input messages with text and/or image content, and the model will generate the next message in the conversation. The Messages API can be used for either single queries or stateless multi-turn conversations.

Here is the request . (client -> LLM)

- tools: optional array of ToolUnion    Definitions of tools that the model may use.

If you include tools in your API request, the model may return tool_use content blocks that represent the model's use of those tools. You can then run those tools using the tool input generated by the model and then optionally return results back to the model using tool_result content blocks.

There are two types of tools: client tools and server tools. The behavior described below applies to client tools. For server tools, see their individual documentation as each has its own behavior (e.g., the web search tool).

Each tool definition includes:

name: Name of the tool.
description: Optional, but strongly-recommended description of the tool.
input_schema: JSON schema for the tool input shape that the model will produce in tool_use output content blocks.
For example, if you defined tools as:
```json
[
  {
    "name": "get_stock_price",
    "description": "Get the current stock price for a given ticker symbol.",
    "input_schema": {
      "type": "object",
      "properties": {
        "ticker": {
          "type": "string",
          "description": "The stock ticker symbol, e.g. AAPL for Apple Inc."
        }
      },
      "required": ["ticker"]
    }
  }
]
```

And then asked the model "What's the S&P 500 at today?", the model might produce tool_use content blocks in the response like this:

```
[
  {
    "type": "tool_use",
    "id": "toolu_01D7FLrfh4GYq7yT1ULFeyMV",
    "name": "get_stock_price",
    "input": { "ticker": "^GSPC" }
  }
]
```

You might then run your get_stock_price tool with {"ticker": "^GSPC"} as an input, and return the following back to the model in a subsequent user message:

```
[
  {
    "type": "tool_result",
    "tool_use_id": "toolu_01D7FLrfh4GYq7yT1ULFeyMV",
    "content": "259.75 USD"
  }
]
```

Here is the tool request
```
Tool = object {}
input_schema: object { type, properties, required }
JSON schema for this tool's input.
  This defines the shape of the input that your tool accepts and that the model will produce.

  type: "object"
  properties: optional map[unknown]
  required: optional array of string

name: string
Name of the tool.   This is how the tool will be called by the model and in tool_use blocks.
maxLength 128 minLength 1

allowed_callers: optional array of "direct" or "code_execution_20250825" or "code_execution_20260120"
cache_control: optional CacheControlEphemeral { type, ttl }
Create a cache control breakpoint at this content block.

defer_loading: optional boolean
If true, tool will not be included in initial system prompt. Only loaded when returned via tool_reference from tool search.

description: optional string
Description of what this tool does.

eager_input_streaming: optional boolean
Enable eager input streaming for this tool. When true, tool input parameters will be streamed incrementally as they are generated, and types will be inferred on-the-fly rather than buffering the full JSON output. When false, streaming is disabled for this tool even if the fine-grained-tool-streaming beta is active. When null (default), uses the default behavior based on beta headers.

input_examples: optional array of map[unknown]
strict: optional boolean
When true, guarantees schema validation on tool names and inputs

type: optional "custom"

```


Here is the response

```
ToolUseBlock = object {}
  id: string
  caller: DirectCaller or ServerToolCaller or ServerToolCaller20260120
  Tool invocation directly from the model.

    Accepts one of the following:
    DirectCaller = object {}
    Tool invocation directly from the model.
      type: "direct"
  input: map[unknown]
  name: string
  type: "tool_use"

```


## Tool Design

Although these providers expose similar capabilities (tool definition, invocation, and result passing), their wire protocols differ significantly in:

Tool schema representation (parameters vs input_schema)
Tool call encoding (stringified JSON vs structured objects)
Correlation identifiers (tool_call_id, call_id, tool_use_id)
Streaming behavior (partial deltas vs complete blocks)

This design introduces a three-layer architecture to isolate these differences while maintaining a consistent runtime model.

Goals

Provide a provider-agnostic tool runtime model
Preserve lossless mapping from provider-specific formats
Support streaming tool call assembly
Avoid premature normalization that loses semantic fidelity
Enable extensibility for future providers
Keep the runtime model minimal until concurrency or orchestration requirements justify more abstraction

Here is architecture
```
+-----------------------------+
|   Provider Layer            |
| (OpenAI / Anthropic APIs)   |
+-----------------------------+
            ↓
+-----------------------------+
|   Protocol Adapter Layer    |
| (encode/decode mapping)     |
+-----------------------------+
            ↓
+-----------------------------+
|   Core Runtime Layer        |
| (ToolSpec / Call / Result)  |
+-----------------------------+
            ↓
+-----------------------------+
|   Tool Execution Layer      |
| (Registry + Handlers)       |
+-----------------------------+
```

Notes

- This section describes the intended improved tool architecture, not a strict mirror of the current implementation.
- Tool execution output is standardized on `invocation::ToolOutput`, implemented by `FunctionToolOutput`; structured metadata is carried through `ToolContent::Mixed`.
- The improved design may intentionally differ from `crates/tools/src/spec.rs` where the current shapes are too limited or too implementation-driven.
- Streaming assembly stays in the adapter layer. Adapters accumulate provider deltas and only emit fully formed tool invocations into the runtime.
- The runtime should not model partial tool calls.

Tool Origin

To explicitly model where a tool comes from.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolOrigin {
    Local,
    Mcp {
        server_id: String,
        tool_name: String,
    },
}
```

Note

- Keep `ToolOrigin::Mcp`, but treat MCP integration as future work for now.
- Avoid expanding runtime execution logic around MCP until the local-tool runtime has been standardized on the improved design.

Tool Specification (ToolSpec)

Unifies tool definitions across providers.

```rust
struct ToolDefinitionSpec {
    name: ToolName,
    description: String,
    input_schema: serde_json::Value,
    output_mode: ToolOutputMode,
    capability_tags: Vec<ToolCapabilityTag>,
}
```

Notes

- `input_schema` unifies:
- OpenAI: parameters
- Anthropic: input_schema
- Keep provider-specific wire details in adapters unless they are required by the runtime.
- This README intentionally uses the improved design vocabulary even where current code still differs.

Tool Call (ToolCall)

Represents a single invocation from the model.
```rust
struct ToolInvocation {
    tool_call_id: ToolCallId,
    session_id: String,
    turn_id: String,
    tool_name: ToolName,
    input: serde_json::Value,
    requested_at: DateTime<Utc>,
}
```

Notes

- Do not introduce a separate `correlation_id` yet.
- Use the provider-emitted call identifier as the primary reference until concurrency or orchestration requirements justify an internal ID model.
- Runtime should only receive fully formed invocations from adapters.

Represents execution output.

```rust
enum ToolExecutionOutcome {
    Completed(ToolResultPayload),
    Failed(ToolFailure),
    Denied(ToolDenied),
    Interrupted,
}
```

Supports:

```rust
enum ToolContent {
    Text(String),
    Json(Value),
    Mixed {
        text: Option<String>,
        json: Option<Value>,
    },
}
```

This design should align with:

- the future normalized runtime result model
- provider result encoding handled by the adapter layer

Tool Execution Layer
7.1 ToolHandler Trait
```rust
#[async_trait]
pub trait RuntimeTool: Send + Sync {
    fn definition(&self) -> ToolDefinitionSpec;
    async fn validate(&self, input: &serde_json::Value) -> Result<(), ToolInputError>;
    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: ToolExecutionContext,
        reporter: Arc<dyn ToolProgressReporter>,
    ) -> Result<ToolExecutionOutcome, ToolExecuteError>;
}
```

7.2 ToolRegistry
```rust
struct RuntimeToolRegistry {
    tools: RwLock<HashMap<ToolName, Arc<dyn RuntimeTool>>>,
}
```

Responsibilities:

Store tool implementations
Provide specs to adapter
Execute tool calls
7.3 Execution Flow
LLM Response
    ↓
Adapter.decode_tool_calls()
    ↓
ToolInvocation
    ↓
Registry.invoke()
    ↓
ToolExecutionOutcome
    ↓
Adapter.encode_tool_result()
    ↓
LLM Input


Error Handling Strategy

Errors are handled at three levels:

8.1 Input Errors
Invalid JSON
Missing required fields

→ ToolInputError

8.2 Runtime Errors
Tool not found
Execution failure

→ ToolExecuteError

8.3 Protocol Errors
Malformed tool calls
Unknown tool types

Handled in adapter layer, not runtime
