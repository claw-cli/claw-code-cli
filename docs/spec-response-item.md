# devo Detailed Specification: Response Item

## Background and Goals

This specification defines the response IR (`ResponseItem`) — a unified
representation of LLM conversation items used in prompt construction and
history management.

## ResponseItem Variants

The response IR is defined as a `ResponseItem` enum with these variants:

- `Reason` — model reasoning output (thinking/reasoning content)
- `Message` — user messages and model replies, including Text and Image content
- `ToolCall` — model tool-call request
- `ToolCallOutput` — tool-call result

## History Management

History management holds a sequence of `ResponseItem`s with token usage metadata.

### Normalization Rules

1. `ToolCall` and `ToolCallOutput` items must remain paired. Removing a
   `ToolCall` must also remove its corresponding `ToolCallOutput`, and vice versa.
2. Items with unsupported modalities (e.g., images for a text-only model) should
   be filtered according to the model's `input_modalities` configuration.

### Token Info

The token info tracks counts reported by the LLM provider, abstracted across
provider APIs:

- `input_tokens` — input token count (supported by OpenAI chat completions,
  OpenAI responses, Anthropic messages APIs)
- `cached_input_tokens` — cached input token count
- `output_tokens` — output token count

### Context Items

History management maintains a reference to context items (OS, shell, timezone,
model, thinking effort, persona, date, etc.) for detecting context changes and
generating diff prompts.

## Compaction

Compaction is a separate LLM call that summarizes history when the context
window threshold is exceeded:

1. Estimate current token usage.
2. If over the auto-compact threshold, select oldest eligible history items.
3. Build a summarization prompt.
4. Invoke the model to generate a summary.
5. Replace compacted history with the summary, preserving recent turns.

The compaction result includes a prefix prompt indicating the content is a
historical summary. The system preserves a configurable number of recent turns
(`preserve_recent_turns`) after compaction.

During compaction, `Reason` items may be filtered out, keeping only `Message`,
`ToolCall`, and `ToolCallOutput` items to reduce token usage.
