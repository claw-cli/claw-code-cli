# devo Detailed Specification: Interactive TUI v2

## Background and Goals

`devo` needs an interactive terminal experience that is:

- compatible with the server/session runtime already used by the rest of the system
- usable for day-to-day coding-agent workflows such as chat turns, model selection, onboarding, shell command review, and session navigation
- structured so rendering, input handling, and runtime orchestration can evolve independently

This document defines the required behavior and boundaries for the interactive terminal UI introduced by the current branch.

## Scope

In scope:

- interactive terminal session lifecycle
- terminal rendering and redraw behavior
- chat transcript presentation
- composer and popup behavior
- onboarding and model selection inside the TUI
- UI-local command and event contracts
- integration with the background worker and runtime server
- shell command summarization used by the UI

Out of scope:

- desktop-only or GUI-only experiences
- provider-specific API payload details
- full approval-modal design for features not currently present in devo
- plugin, marketplace, or external product surfaces that are not part of devo's runtime contract

## Design Goals

The interactive TUI must:

- feel like a first-class devo interface rather than a partial port
- keep terminal interaction responsive during streaming and long-running work
- preserve clear ownership boundaries between UI, worker, and protocol/runtime code
- expose only devo-supported actions to the user
- represent tool and shell activity in a human-readable form without depending on renderer-only heuristics

## Module Responsibilities and Boundaries

`devo-cli` owns:

- resolving initial interactive settings such as provider, model, onboarding mode, and saved models
- constructing the TUI launch configuration
- selecting interactive mode as the default user entrypoint

`devo-tui` owns:

- terminal lifecycle management
- frame scheduling and redraw orchestration
- transcript rendering
- composer, popups, and onboarding interaction
- devo-local UI command and event types
- mapping runtime events into user-visible history cells and status indicators

`devo-tui::worker` owns:

- bridging UI requests to the stdio server client and runtime
- session creation, switching, rename, rollback, and interruption requests
- provider validation and reconfiguration initiated from onboarding
- surfacing runtime events back to the TUI

`devo-protocol` owns:

- shared structured types needed across crates for shell-command summaries and other UI-visible normalized data

`devo-utils` owns:

- shell parsing and best-effort command summarization logic
- command safety helpers shared by more than one crate

Rules:

- the TUI must not depend directly on provider SDKs or provider wire formats
- the TUI must not own durable session truth
- the worker must not own terminal rendering concerns
- renderer-specific code must not be the sole owner of normalized command meaning

## Interactive Session Lifecycle

The interactive UI must support this lifecycle:

1. launch with resolved model/provider settings
2. optionally enter onboarding if configuration is missing or onboarding is forced
3. present a chat surface with transcript and composer
4. submit user turns to the runtime
5. render streaming and completed results
6. allow interruption, session changes, and shutdown

Requirements:

- the interactive mode must be launchable from the main `devo` CLI flow
- the initial session state must include the active working directory and active model
- onboarding must be available when provider configuration is incomplete
- the UI must be able to exit cleanly after shutting down its worker
- the UI should preserve lightweight session usage counters such as turn count and token totals when the runtime reports them

## Terminal Behavior

The TUI must operate correctly in a terminal environment.

Requirements:

- the UI must require interactive stdin and stdout terminals before initializing
- the UI must enable raw-mode style interaction needed for responsive keyboard handling
- the UI must support bracketed paste
- redraws must be explicitly scheduled rather than continuously repainting without state changes
- the UI must preserve usable terminal scrollback rather than treating the entire session as disposable alternate-screen content
- terminal restoration must occur on exit and on recoverable teardown paths

The terminal subsystem should:

- tolerate terminals that do not support every keyboard enhancement capability
- allow committed transcript history to move into normal scrollback while keeping the active interaction area live

## Transcript Requirements

The transcript is the primary user-visible conversation surface.

Requirements:

- the transcript must show session-start context such as cwd and active model
- the transcript must render user messages, assistant messages, reasoning content, status updates, and tool-related activity in distinct, readable forms
- streamed assistant output must be representable before the turn is complete
- committed history must remain visible after new activity begins
- status changes from the runtime should be reflected without requiring a full session restart

The transcript renderer should support:

- markdown-aware rendering for assistant content
- diff-style and tool-output-aware rendering where appropriate
- scrollback-friendly formatting for completed content

## Composer and Bottom-Pane Requirements

The interactive composer is the primary input surface.

Requirements:

- the composer must accept free-form text input
- the composer must support paste input
- the composer must submit user input through a normalized UI command path rather than invoking runtime calls directly
- the composer must support slash command discovery and execution
- the `/model` slash command opens a picker showing only configured models
  (those with credentials in `config.toml`), not the full model catalog
- the composer must support browsing input history
- the composer must expose status or helper text when onboarding or popup flows need to steer the user

Rules:

- composer state changes that affect visible UI must trigger frame requests
- popup behavior must be dismissible from the keyboard
- the bottom pane must remain focused on devo-supported workflows and must not expose orphaned UI surfaces from imported code that devo does not support

## Onboarding Requirements

The TUI must support provider onboarding for first-run or forced-onboarding flows.

Requirements:

- onboarding must allow the user to choose a channel (vendor group) first,
  then a model within that channel
- onboarding must present channels derived from the `channel` field in
  the model catalog
- onboarding must allow collection of optional base URL and API key values when required
- onboarding must validate provider settings before they replace the active runtime configuration
- successful onboarding must persist the resulting provider selection through the existing config path
- unsuccessful validation must leave the runtime in its previous usable state

## UI Command and Event Contract

The interactive TUI must define a devo-local command and event model.

Requirements for UI-to-host commands:

- there must be a typed command surface for user-turn submission
- there must be typed commands for interruption, model/thinking/context overrides, shell-command requests, session actions, review actions, and shutdown
- command variants must be specific enough that the worker can translate them into runtime/server requests without depending on widget internals

Requirements for internal app events:

- there must be a typed event surface for redraw, exit, submit, popup control, model selection, thinking selection, and status updates
- widget components should communicate through app events rather than directly mutating unrelated top-level state

Rules:

- the command/event surface must be devo-owned and must not import large foreign product enums wholesale
- app commands must describe user intent, not renderer actions
- app events must describe UI coordination, not transport protocol payloads

## Worker Integration Requirements

The background worker is the TUI's runtime adapter.

Requirements:

- the worker must support turn submission
- the worker must support active-turn interruption
- the worker must support model and thinking updates for future turns
- the worker must support session list retrieval and session switching
- the worker must support session rename and rollback requests
- the worker must support skill list retrieval if the UI requests it
- the worker must support provider validation and provider reconfiguration initiated by onboarding
- the worker must shut down gracefully, with bounded fallback behavior if graceful shutdown takes too long

Rules:

- worker communication with the UI must be event-driven
- worker failure must be surfaced to the user as UI-visible status or transcript output
- the worker must remain the owner of runtime/server communication details

## Shell Command Summary Requirements

The interactive UI needs a shared, structured summary of executed shell commands so it can present shell activity clearly.

Requirements:

- the system must provide a shared parsed-command type that is not TUI-local
- the parsed-command model must distinguish at least:
  - file reads
  - file listing
  - workspace search
  - unknown commands
- the parser must attempt to unwrap common shell wrappers such as `bash -lc` and PowerShell command wrappers
- the parser must extract useful metadata such as command text, query text, and target path when that can be done safely
- the parser must degrade to `Unknown` when intent cannot be determined confidently

Rules:

- command summarization is for UX and normalized display, not authorization
- the parser should be conservative around pipelines and mutating commands
- shared parser output must be reusable by crates other than the TUI

## Supported UX Surface

The first-class devo interactive UI must support:

- text chat turns
- transcript rendering with streaming updates
- onboarding for model/provider configuration
- model switching
- thinking selection
- slash-command initiated actions
- shell command display and summary
- session-level actions that devo currently supports

The devo interactive UI must not require support for:

- plugin marketplace flows
- external product approval overlays that devo has not implemented
- non-devo request-user-input surfaces imported from other products
- unrelated experimental or promotional popups

## Testing Requirements

Minimum required test coverage:

- unit tests for shell command parsing and normalization
- widget tests for composer, popup, and chat-widget state transitions
- rendering tests for markdown, diff, highlighting, and transcript presentation
- integration tests covering onboarding validation, turn submission, interruption, and session switching through the worker

Rules:

- tests should prefer asserting whole structured outputs instead of isolated fields where feasible
- platform-specific command or path behavior must be tested with platform-aware cases

## Acceptance Criteria

This specification is satisfied when:

- `devo` launches into a devo-owned interactive TUI flow backed by typed UI commands and events
- the TUI can onboard, submit turns, stream results, interrupt work, and shut down cleanly
- terminal behavior remains responsive and restores correctly on exit
- the worker cleanly bridges the UI to the runtime without leaking transport details into widgets
- shell activity can be summarized through shared parsed-command types instead of renderer-only string heuristics
- the user-visible interactive surface is limited to devo-supported behaviors rather than partially exposing foreign product features

## Open Questions and Follow-Up Work

Future specs may split out:

- transcript/history-cell rendering requirements
- onboarding persistence and provider reconfiguration details
- shell command safety versus shell command summarization
- detailed slash-command semantics

This document intentionally defines the contract for the current interactive TUI surface without requiring every future terminal feature to be designed now.
