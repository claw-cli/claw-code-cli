# Detail Specification: Response Item

This specification defines:

- The response IR (ResponseItem) – a unified representation of LLM conversation items.
- A history management system that holds a sequence of ResponseItems together with token usage metadata, environment context, and provides utilities for mutation, normalisation, and prompt preparation.
- A compaction method that summarises history via a separate LLM call when the token budget is exceeded.

Implementations must reuse existing code where possible.

The response IR should be defined as `ResponseItem` enum, currently it has Reason / Message / ToolCall / ToolCallOutput.

- Reason is model reasonning output, some model has, some did not.
- Message is user sent Message and model reply message, including Text and Image.
- ToolCall is model tool call request.
- ToolCallOutput is the tool call response.

The implementation should reuse the current codebase. such as current code already defined Image message, text message, should implement reuse as possible.

## History Management

based on ResponseItem, there is a history management for context.

Should have a normalize module, here is requirements.
1. `ToolCall` and `ToolCallOutput` should be paired. That is, for a `ResponseItem` container, if remove an item, if remove `ToolCall`, then should remove the corresponding `ToolCallOutput`. If remove `ToolCallOutput`, then should remove the corresponding `TooCall` item.
2. For a model, there is a modality array, contains Text / Image / Video modality, the normalize module should provider a method / function to remove the `ResponseItem` acording to the modality array.

For the history management, should support the function, to remove the tail UserMessage, the goal is to remove the user message from tail (user drawback serveral last messages.) .

The history management should have a item array, `Vec<ResponseItem>`, should pertain the token info, describing the current total token info. The token info stores info returned by LLM provider, since the current project is LLM provider agnostic, then here is my design for token info.

- input token: the input token count, returns by llm provider, supported by OpenAI chat completions, OpenAI responses, Anthropic messages API.
- cached input token: the cached input token count, returns by llm provider, supported by OpenAI chat completions, OpenAI responses, Anthropic messages API.
- output token: the output token count, returns by llm provider, supported by OpenAI chat completions, OpenAI responses, Anthropic messages API.

the history management should have a reference to context item, utilize for diff when context changed, containing os, shell, timezone, model, thinking effort, persona, date, etc.

the context item should have diff method to produce 'diff prompt' for context item.

the history management should have a `for prompt` method to prepare prompt for a LLM call.

## Compaction

For the context management, there should be a compaction module, it is a seprate LLM call.

The LLM call compact the current history, it it exceeds the context window, then it remove the oldest history item and retry.

The compaction result should have a prefix prompt to indicate this is the compaction of history.

And what's more, we should have a limit token buget for pertain user rencent message after compaction.

For the compaction, I think we'd better filterd Reason, only keep Message (Text / Image / Video (only when model support image/viode)), ToolCall, ToolCallOutput.
