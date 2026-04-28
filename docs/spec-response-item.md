# Detail Specification: Response Item

This documentation describes the content type of model response IR, such as Reason, Message, ToolCall, ToolCallOutput, for context management.

The response IR should be defined as `ResponseItem` enum, currently it has Reason / Message / ToolCall / ToolCallOutput.

- Reason is model reasonning output, some model has, some did not.
- Message is user sent Message and model reply message, including Text and Image.
- ToolCall is model tool call request.
- ToolCallOutput is the tool call response.


The implementation should reuse the current codebase. such as current code already defined Image message, text message, should implement reuse as possible.

## Context Management

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

the history

## Compaction

For the context management, there should be a compaction module, it is a seprate LLM call.