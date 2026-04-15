# `clawcr-provider`

Shared provider layer for model invocation.

This crate defines:

- a common request/response IR used by the rest of the project
- traits for provider SDK implementations
- concrete adapters for OpenAI-family and Anthropic-family transports
- provider capability metadata used by higher layers to shape requests

## What lives in this crate

The crate is organized around three shared building blocks:

- `request.rs`
  Defines the normalized request types:
  - `ModelRequest`
  - `RequestMessage`
  - `RequestContent`
  - `SamplingControls`
  - `ToolDefinition`
  - `RequestRole`

- `response.rs`
  Defines the normalized response and streaming types:
  - `ModelResponse`
  - `ResponseContent`
  - `Usage`
  - `ResponseMetadata`
  - `ResponseExtra`
  - `StreamEvent`
  - `StopReason`

- `provider.rs`
  Defines the provider abstraction:
  - `ModelProviderSDK`
  - `ProviderAdapter`
  - `ProviderFamily`
  - `ProviderCapabilities`

In short, higher layers construct a `ModelRequest`, send it through a
`ModelProviderSDK`, and receive either a complete `ModelResponse` or a stream
of `StreamEvent` values.

## Provider families

### OpenAI

The `openai` module contains OpenAI-family adapters and helpers.

Implemented transports:

- `openai::chat_completions::OpenAIProvider`
  OpenAI Chat Completions style transport.
  Reference:
  <https://developers.openai.com/api/reference/chat-completions/overview>

- `openai::responses::OpenAIResponsesProvider`
  OpenAI Responses API transport.
  Reference:
  <https://developers.openai.com/api/reference/resources/responses>

Supporting modules:

- `openai::capabilities`
  Resolves transport/model-specific request capability differences.

- `openai::reasoning_effort`
  Shared reasoning-effort enum for OpenAI-family transports.

- `openai::role`
  OpenAI wire-role definitions.

The OpenAI adapters parse the full provider payload into typed local structs,
map the shared fields into the crate IR, and preserve richer provider-specific
fields in `ResponseMetadata`.

### Anthropic

The `anthropic` module contains Anthropic-family adapters and helpers.

Implemented transport:

- `anthropic::messages::AnthropicProvider`
  Anthropic Messages API transport.
  Reference:
  <https://platform.claude.com/docs/en/api/messages>

Supporting modules:

- `anthropic::role`
  Anthropic wire-role definitions.

Like the OpenAI adapters, the Anthropic adapter builds typed request payloads,
parses typed response payloads, maps supported fields into the shared IR, and
preserves richer provider-specific data in response metadata.

## Normalized IR

The crate does not try to expose every vendor field directly in the shared API.
Instead it normalizes the pieces the rest of the system depends on:

- text output
- tool-use output
- stop reason
- token usage
- streaming deltas
- selected provider-specific metadata

When a provider returns additional fields that do not fit the shared IR cleanly,
they can be preserved through `ResponseExtra::ProviderSpecific`.

This keeps the common interface small while still allowing adapters to retain
important vendor-specific information.

## Capabilities

`ProviderCapabilities` describes differences between providers and transports,
for example:

- supported roles
- support for `temperature`
- support for `top_p`
- support for `top_k`
- support for reasoning-effort controls
- support for tool calls
- support for surfaced reasoning content

Higher layers can use these capability flags to shape requests before they are
serialized by a provider adapter.

## What does not belong here

This crate is not responsible for:

- model catalog loading
- session management
- orchestration
- persistence
- UI-facing event shaping
- provider selection policy

Those concerns belong in higher-level crates such as `core`, `server`, and
`protocol`.

## Public exports

The crate root re-exports the shared request/response/provider types:

- `clawcr_provider::ModelRequest`
- `clawcr_provider::ModelResponse`
- `clawcr_provider::StreamEvent`
- `clawcr_provider::ModelProviderSDK`
- `clawcr_provider::ProviderAdapter`
- `clawcr_provider::ProviderCapabilities`
- `clawcr_provider::ProviderFamily`

It also exposes the provider family modules:

- `clawcr_provider::openai`
- `clawcr_provider::anthropic`

## Current scope

Today this crate ships:

- one Anthropic transport
- two OpenAI-family transports
- a shared IR for requests, responses, and streaming
- typed request/response parsing inside each adapter

If a new provider family or transport is added, it should follow the same
pattern:

1. map from `ModelRequest` into the provider wire format
2. map provider responses into `ModelResponse` and `StreamEvent`
3. expose provider-specific details through metadata when needed
4. report capabilities through `ProviderAdapter`
