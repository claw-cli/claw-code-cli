# devo Detailed Specification: Interactive TUI

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
- on exit — regardless of trigger (`/exit`, `Ctrl+C`, SIGKILL, panic) — the shell prompt must appear **directly below the bottom composer status line**, with no extra blank lines and no overlap with the transcript history above

Exit position example (the `PS C:\...>` is the shell prompt):

```
  deepseek-v4-flash high  ↑0 ↓0  ░░░░░░░░░░ 0% (0) 
PS C:\Users\lenovo\Desktop\devo>
```

The TUI must ensure the cursor is placed at the row immediately following the last visible status line before restoring the terminal.

The terminal subsystem should:

- tolerate terminals that do not support every keyboard enhancement capability
- allow committed transcript history to move into normal scrollback while keeping the active interaction area live

### Screen Modes

The TUI supports two screen modes toggled by `Ctrl+T`:

**Inline mode (default)** — the TUI renders directly into the terminal scrollback. Mouse interaction is not available. Tool output cells are collapsed by default and can be expanded via keyboard (`Enter` on a selected cell). This mode preserves usable terminal scrollback after exit.

**Alternative screen mode** (`Ctrl+T` to enter) — the TUI switches to the terminal's alternate screen buffer. The transcript, composer, and all cells render identically to inline mode. Mouse events are captured so tool output cells can be clicked to expand or collapse. `Ctrl+T` toggles back to inline mode.

On exit, if the TUI is in alternative screen mode, it must switch back to inline mode first, then restore the shell prompt position.

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
- typing `/` in the composer opens a command list popup above the input line;
  the currently highlighted option is indicated by the theme's accent color:

  ```
  ┃ /

    /theme     switch the UI theme
    /model     choose the active model
    /compact   compact the current session context
    /resume    resume a saved chat
    /new       start a new chat
    /status    show current session configuration and token usage
    /onboard   configure model provider connection
  ```

  `/thinking` is not included — thinking effort is configured through the `/model` picker instead.

- the `/model` slash command opens a popup picker above the input line with two steps:

  1. **Model picker** — list of configured models with vendor name and a `current` marker
     on the active model; no header title:

     ```
       deepseek-v4-flash
         DeepSeek
       deepseek-v4-pro  current
         DeepSeek
       qwen3-coder-next
         Qwen
     ```

     The currently highlighted row uses the theme's accent color.

  2. **Thinking effort picker** — shown only if the selected model supports thinking;
     lists effort levels with descriptions and a `current` marker:

     ```
       Off
         Disable thinking for this turn
       High  current
         More deliberate for harder tasks
       Max
         Most deliberate, highest effort
     ```

     The highlighted row uses the theme's accent color. After selecting, the picker confirms and closes.
- the `/theme` slash command opens a popup picker above the input line showing available themes with a `current` marker:

  ```
    devo (default)
    dark
    light  current
    aurora
  ```

  The highlighted row uses the theme's accent color. Selecting a theme applies it immediately to all themed elements (borders, separators, accents, composer `┃`, cell `▌`). The selection persists across sessions.
- the `/resume` slash command enters an alternative full-screen session picker:

  ```
  Devo Sessions
  Resume Session
  Use Up/Down to select a session, Enter to resume.
  Esc to go back.

    Title                                 Session ID                            Updated
    ------------------------------------  ------------------------------------  -------------------
    Hello, investigate the project , wi…  019ddc5f-7fa1-7622-b342-9439ea181a7c  2026-04-30 03:15:47 UTC
    Hello, explain the project in chine…  019ddc39-f13f-7072-8fe7-3f7ed7344b6a  2026-04-30 02:31:18 UTC
    Investigate the project, then answe…  019dd8b9-c85a-79b0-b97b-bcfb1dc40dc5  2026-04-29 10:12:36 UTC
  ```
- the composer must support browsing input history
- the `/status` slash command renders a header-box-style info panel in the transcript showing current session configuration:

  ```
  ╭──────────────────────────────────────────────────────╮
  │ Session Status                                       │
  │                                                      │
  │ model:       deepseek-v4-flash                       │
  │ thinking:    high                                    │
  │ cwd:         ~\Desktop\devo                          │
  │ turns:       3                                       │
  │ tokens:      ↑1,234 ↓45,678  ░░░░░░░░░░ 12% (58K)    │
  ╰──────────────────────────────────────────────────────╯
  ```

  The panel reuses the header-box border style (`╭──╮` / `╰──╯`). Contents update to reflect current live state.
- the composer must expose status or helper text when onboarding or popup flows need to steer the user

Rules:

- composer state changes that affect visible UI must trigger frame requests
- popup behavior must be dismissible from the keyboard
- during active processing (generating, compacting), configuration-changing slash commands (`/model`, `/onboard`, etc.) must be disabled; if the user invokes them, a single-line message is inserted into the transcript: `Cannot change model while generating`
- the bottom pane must remain focused on devo-supported workflows and must not expose orphaned UI surfaces from imported code that devo does not support

## Keybindings

| Key | Context | Action |
|-----|---------|--------|
| `Enter` | composer | submit turn |
| `Esc` | generating / compacting | interrupt active processing |
| `Esc` | picker / popup / onboarding | go back or cancel |
| `Up` / `Down` | composer | browse input history |
| `Up` / `Down` | picker list | navigate options |
| `Enter` | picker list | confirm selection |
| `Enter` | tool cell (inline mode, selected) | expand / collapse tool output |
| `Alt+Up` / `Alt+Down` | any | enter selection mode; move between user cells |
| `Enter` | selection mode (on a user cell) | open action menu (Rollback / Fork / Cancel) |
| `Esc` | selection mode | exit selection mode, return to composer |
| Mouse click | tool cell (alt-screen mode) | expand / collapse tool output |
| `Ctrl+T` | any | toggle inline / alternative screen mode |
| `Ctrl+C` | any | exit TUI |
| `/exit` | composer | exit TUI |
| `/` | composer | open slash command list |
| `Type to search` | onboarding model list | filter list by text |

All pickers and popups are dismissible via `Esc`. The `/` slash list closes on `Esc` or on selecting a command.

## History Interaction

The user can browse, select, and act on past turns in the transcript.

### Selection Mode

`Alt+Up` / `Alt+Down` enters selection mode and moves the selection cursor between **user message cells** in the transcript. When in selection mode:

- the selected user cell is visually highlighted — the `┃` prefix and the text use the theme's **accent color**
- the status line updates to indicate the active selection:

  ```
  Selected turn 3 · Enter to act  Esc to cancel
  ```

- `Esc` exits selection mode and returns focus to the composer without action
- `Enter` on a selected cell opens an action menu popup above the composer

### Action Menu

The action menu appears as a popup above the composer:

```
  ┃ Rollback
  ┃ Fork
  ┃ Cancel
```

The highlighted option uses the theme's accent color. `Up`/`Down` to navigate, `Enter` to confirm, `Esc` to dismiss.

### Rollback

Truncates the current session to the selected turn:

- all turns after the selected one are discarded
- the selected user message text is loaded into the composer for editing and re-submission
- the transcript shows only content up to and including the selected turn

### Fork

Creates a new session from the selected turn:

- a new session is created containing all transcript content from the beginning up to the selected turn
- the selected user message text is loaded into the composer
- the TUI switches to the new session immediately
- the original session remains intact and accessible via `/resume`

Rules:

- selection mode is unavailable during active processing (generating, compacting)
- Rollback and Fork are disabled on the **most recent** user turn (there is nothing to roll back from)

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

## Visual Style

Below is the complete annotated layout of the TUI at launch (no prior interaction), with each visual region labeled.

```
                                                                              cell / region
═══════════════════════════════════════════════════════════════════════════════════════════
PS C:\Users\lenovo\Desktop\devo> .\target\debug\devo                                     │ shell prompt (pre-launch)
                                                                                         │
╭──────────────────────────────────────────────────────╮                                │
│ >_  Devo (v0.1.3)                                    │  HEADER BOX                    │
│                                                      │  - version from Cargo.toml     │
│ model:     <model-name> <effort>   /model to change  │  - model + effort (live)       │
│ directory: ~\Desktop\devo                            │  - cwd (live)                  │
╰──────────────────────────────────────────────────────╯  - rendered only once on       │
                                                           initial launch                │
                                                                                         │
  Tip: Random tip text here.                                     TIP AREA                │
                                                                  - random from array     │
                                                                                         │
                                                                                         │
                                                                  3 blank lines           │
                                                                                         │
┃ Ask Devo                                                     COMPOSER                 │
                                                                  - ┃ in accent color     │
  <model-name> <effort>  ↑0 ↓0  ░░░░░░░░░░ 0% (0)               STATUS LINE             │
                                                                                         │
PS C:\Users\lenovo\Desktop\devo>                                                        │ shell prompt (post-exit)
```

### Header Box

- version must be in sync with the crate version in `Cargo.toml`
- model name and thinking-reasoning-effort label must reflect the current active configuration
- cwd must be shown and kept in sync with runtime state
- rendered **only once** on initial TUI launch; switching or resuming a session must not re-render it

### Tip Area

- tips are stored in a configurable array
- on each start, one tip is picked at random
- prefixed with `  Tip:`

### Composer Region

- separated from content above by **three blank lines**
- one blank line above the input line, one below
- `┃` at the left edge of the input line in the theme's accent color
- status line below shows: model name, effort, token usage (`↑` sent / `↓` received), context-window bar with percentage and approximate count

### Transcript Cells (populated during a session)

Each cell (user message, thinking, tool-ran, assistant reply) has:

- a left vertical line (`▌`) in a color distinct from the composer's `┃`
- adjacent cells separated by one blank line
- identical rendering whether produced live or loaded from history

#### User Message Cells

```
┃ Hello, explain the project in Chinese.
```

- `┃` uses the same composer accent color to visually tie user input to the composer
- text is rendered in the default foreground color
- no left `▌` line — user cells use `┃` instead to distinguish them from system/assistant cells

#### Thinking and Tool Cells

```
▌ Thinking: The user wants me to explain the project in Chinese.
```

- `▌` is the left vertical line
- `Thinking:` is italic with a distinct color
- the rest of the text is gray

Tool-ran cells follow the same convention. Tool output is rendered collapsed by default. In inline mode, use keyboard (`Enter` on a selected cell) to expand or collapse. In alternative screen mode, the cell is clickable to expand or collapse.

The tool cell distinguishes success and failure:

- **Success** — normal color:

  ```
  ▌ Ran bash cloc --version … +4 lines (exit 0)
  ```

- **Failure** — the entire `▌ Ran <command>` line renders in the theme's **error color**:

  ```
  ▌ Ran bash cloc --version 2>nul && cloc crates/ --by-file --quiet --hide-rate
    working directory does not exist:
  ```

  A non-zero exit code or a runtime error triggers error coloring. The output content below the line is unaffected.

#### Working Indicator

During active processing (streaming, tool execution, thinking), a live working indicator must appear in the status line area:

```
  ⠴ Working (3s • esc to interrupt)
```

Requirements:

- the leftmost character is a frame-based spinner animation (e.g. `⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏`)
- the duration in seconds is live-updating
- the hint `esc to interrupt` informs the user they can press Escape to cancel

### Turn Footer

The bottom of every completed turn (spanning thinking, tool calls, and assistant reply) includes a footer. Note that thinking content is model-dependent and may not appear in every turn.

```
  ▣ <model-name> · 15s
```

Requirements:

- model name matches the model that generated the turn
- if the turn completed normally, duration uses the largest appropriate unit: `s`, `h`, `d`, or `w`
- if the turn was interrupted, the footer shows `interrupted` in place of the duration:

```
  ▣ <model-name> · interrupted
```

### Exit Position

Regardless of exit method (`/exit`, `Ctrl+C`, kill, panic), the shell prompt must appear directly below the status line with no extra blank lines and no overlap with transcript history:

```
  <model-name> <effort>  ↑0 ↓0  ░░░░░░░░░░ 0% (0)
PS C:\Users\lenovo\Desktop\devo>
```

The cursor must be placed at the row immediately following the last visible status line before terminal restoration.

### General Appearance

All borders, separators, and accents respect a configurable theme so the color scheme can be changed without altering layout logic. Each theme must define at minimum: an **accent color** (composer `┃`, highlighted list options), a **cell-line color** (transcript `▌`), and an **error color** (failed tool command lines). Themes are defined in a named set (built-in and user-defined in config). The active theme persists across sessions and is switched via `/theme`.

### Onboarding Screen

The onboarding screen uses an alternative full-screen layout with four vertical sections.

#### Section 1 — Title

```
  Welcome to Devo
  Choose a model to get started.
```

#### Section 2 — Search

```
  ▌ Search models...
```

A search input with the themed vertical line.

#### Section 3 — Selectable List

```
  minimax-m2.7
  glm-5.1
  deepseek-v4-flash
  deepseek-v4-pro
```

This section is scrollable with Up/Down navigation. The onboarding flow uses this same list layout sequentially:

1. **Channel picker** — list of vendor groups (from the `channel` field in the model catalog)
2. **Model picker** — models within the selected channel
3. **Thinking effort picker** — shown only if the selected model supports thinking; lists available effort levels (e.g., `low`, `high`)
4. **Provider SDK picker** — available provider SDKs for the chosen model
5. **Base URL** — text input field
6. **API key** — text input field

No "Custom Model" entry appears in the model list. Users who need a custom model must add it manually via `model.json`.

#### Section 4 — Bottom Hints

```
  ↑↓ Navigate  Enter Select  Type to search  Esc Cancel
  To add a custom model, refer to model.json
```

The second line is an additional hint directing users to `model.json` for custom model registration. `Esc` exits the onboarding process.

## Open Questions and Follow-Up Work

Future specs may split out:

- transcript/history-cell rendering requirements
- onboarding persistence and provider reconfiguration details
- shell command safety versus shell command summarization
- detailed slash-command semantics

This document intentionally defines the contract for the current interactive TUI surface without requiring every future terminal feature to be designed now.
