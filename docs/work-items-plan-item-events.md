# Plan Item Events

## Goal

Implement the spec-defined plan item and plan update events for successful `update_plan` tool executions so the server emits first-class `Plan` items instead of treating plan updates as only generic tool results.

## Why

`docs/spec-conversation.md` defines `TurnItem::Plan(PlanItem)` as a first-class persisted item kind.

`docs/spec-server-api.md` requires:

- `turn/plan/updated` as a required turn notification
- `item/plan/delta` as a minimum item delta notification
- explicit `plan` item support in the item taxonomy

The current runtime already defines the protocol types for these events and item kinds, but a successful `update_plan` tool call is only surfaced as `ToolCall` + `ToolResult`. That leaves the plan portion of the spec partially implemented.

## Changes

- Detect successful `update_plan` tool results in the server turn event loop.
- Continue emitting the existing `ToolCall` item for the tool invocation.
- Emit a first-class `Plan` item for the successful plan output.
- Emit an `item/plan/delta` notification for the plan text before the plan item completes.
- Emit `turn/plan/updated` after the plan item is completed.
- Keep non-plan tool results and errored `update_plan` executions on the existing `ToolResult` path.

## Files Affected

- `crates/server/src/runtime.rs`
- `crates/server/src/runtime/plan.rs`
- `crates/server/tests/plan_integration.rs`

## Task List

1. Update the turn event handling in `crates/server/src/runtime.rs`.
2. Track tool-use ids to tool names while streaming query events so the runtime can recognize which `ToolResult` came from `update_plan`.
3. For a successful `update_plan` result:
4. Start a `Plan` item with an empty initial text payload.
5. Emit one `item/plan/delta` notification containing the full plan text.
6. Persist the successful update as one item record containing replay-facing `ToolResult` and visible `TurnItem::Plan(TextItem { text: ... })`, then complete the live item as `plan`.
7. Emit `turn/plan/updated` for the owning turn after the plan item completes.
8. Preserve the existing `ToolResult` behavior for every other tool result, including errored `update_plan` results.
9. Add an integration test that drives a turn through an `update_plan` tool call and asserts:
10. the runtime emits `item/started` / `item/plan/delta` / `item/completed` for a `plan` item
11. the runtime emits `turn/plan/updated`
12. the persisted or resumed turn history includes a `Plan` item instead of relying only on a generic tool result
13. Add a regression assertion that a non-plan tool result still emits the existing `ToolResult` item path.

## Verification

- `cargo test --package clawcr-server --test plan_integration`
- `cargo test --package clawcr-server --test protocol_contract`
