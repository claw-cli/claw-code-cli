//! L4: Lifecycle and extensibility tests.
//!
//! These tests define the expected behavior for interrupt/resume (P0),
//! memory prefetch (P1), cache-aware usage stats (P1), and stop hooks (P2).
//! All are `#[ignore]` until the corresponding features land.
//!
//! Capability mapping:
//!   - 1.10  Interrupt & Resume (CancellationToken / Ctrl+C)
//!   - 1.9   Memory Prefetch (CLAUDE.md loading)
//!   - 1.11  Usage statistics (cache token tracking)
//!   - 1.8   Stop Hooks (extensibility)

#[allow(dead_code, unused_imports)]
mod harness;

use std::sync::Arc;

use serde_json::json;

use clawcr_provider::{StopReason, Usage};
use clawcr_safety::legacy_permissions::PermissionMode;
use clawcr_tools::{ToolOrchestrator, ToolOutput, ToolRegistry};

use clawcr_core::{query, AgentError, ContentBlock, Message, QueryEvent, Role, SessionConfig};

use harness::builders::*;
use harness::{event_collector, ScriptedProvider, SpyTool};

fn setup_registry() -> (Arc<ToolRegistry>, ToolOrchestrator) {
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    (registry, orchestrator)
}

fn setup_registry_with_tool(tool: SpyTool) -> (Arc<ToolRegistry>, ToolOrchestrator) {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(tool));
    let registry = Arc::new(reg);
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    (registry, orchestrator)
}

// ---------------------------------------------------------------------------
// 1.10  Interrupt & Resume
// ---------------------------------------------------------------------------

/// When a cancellation signal fires during stream consumption, the query loop
/// should return Aborted and save any partial text that was received.
#[tokio::test]
#[ignore = "1.10 interrupt/resume not yet implemented"]
async fn ctrl_c_aborts_gracefully() {
    // In a real implementation, the query() function would accept a
    // CancellationToken and check it between stream events.
    //
    // Test strategy:
    //   1. Create a provider that emits a few TextDelta events then hangs
    //   2. Fire a cancellation token after a short delay
    //   3. Assert query returns Err(Aborted)
    //   4. Assert partial text is preserved in session.messages

    let provider = ScriptedProvider::builder()
        .turn(make_text_turn(
            "partial text before interrupt",
            StopReason::EndTurn,
        ))
        .build();
    let (registry, orchestrator) = setup_registry();

    let mut session = make_session();

    // TODO: pass CancellationToken to query() and trigger it
    let result = query(&mut session, &provider, registry, &orchestrator, None).await;

    // Once interrupt is implemented, this should be Aborted
    match result {
        Err(AgentError::Aborted) => {}
        other => panic!("expected Aborted, got: {:?}", other),
    }

    // Partial text should be preserved
    let has_partial = session.messages.iter().any(|m| {
        m.role == Role::Assistant
            && m.content.iter().any(|b| match b {
                ContentBlock::Text { text } => !text.is_empty(),
                _ => false,
            })
    });
    assert!(
        has_partial,
        "partial assistant text should be saved on abort"
    );
}

/// When cancellation fires during tool execution, the tool should be
/// interrupted and session state should remain consistent.
#[tokio::test]
#[ignore = "1.10 interrupt/resume not yet implemented"]
async fn abort_during_tool_execution() {
    let slow_tool = SpyTool::new("slow_tool", false).with_response(|_| {
        // In a real test, this would be a slow async operation that
        // respects CancellationToken. For now, return immediately.
        ToolOutput::success("this should not appear if aborted")
    });

    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn(
            "t1",
            "slow_tool",
            json!({}),
            StopReason::ToolUse,
        ))
        .turn(make_text_turn("after tool", StopReason::EndTurn))
        .build();

    let (registry, orchestrator) = setup_registry_with_tool(slow_tool);
    let mut session = make_session();

    // TODO: trigger cancellation during tool execution
    let result = query(&mut session, &provider, registry, &orchestrator, None).await;

    match result {
        Err(AgentError::Aborted) => {}
        other => panic!("expected Aborted during tool execution, got: {:?}", other),
    }
}

/// After an abort, session.messages should be complete up to the interruption
/// point — no partially-constructed messages with missing content blocks.
#[tokio::test]
#[ignore = "1.10 interrupt/resume not yet implemented"]
async fn abort_preserves_session_state() {
    let provider = ScriptedProvider::builder()
        .turn(make_text_turn(
            "first complete response",
            StopReason::EndTurn,
        ))
        .build();
    let (registry, orchestrator) = setup_registry();

    let mut session = make_session();

    // TODO: abort after first turn completes but before second starts
    let _ = query(&mut session, &provider, registry, &orchestrator, None).await;

    // Every message should have at least one content block
    for (i, msg) in session.messages.iter().enumerate() {
        assert!(
            !msg.content.is_empty(),
            "message {} should not be empty after abort",
            i
        );
    }

    // All assistant messages should have valid content
    for msg in session
        .messages
        .iter()
        .filter(|m| m.role == Role::Assistant)
    {
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    // Text may be partial but should not be empty if the block exists
                    assert!(!text.is_empty(), "text block should not be empty");
                }
                ContentBlock::ToolUse { id, name, .. } => {
                    assert!(!id.is_empty());
                    assert!(!name.is_empty());
                }
                ContentBlock::ToolResult { .. } => {
                    panic!("assistant message should not contain ToolResult");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 1.9  Memory Prefetch
// ---------------------------------------------------------------------------

/// When the working directory contains a CLAUDE.md file, its contents should
/// be prepended/appended to the system prompt before the first query.
#[tokio::test]
async fn memory_prefetch_loads_claude_md() {
    // Create a temp directory with a CLAUDE.md file
    let tmp = std::env::temp_dir().join(format!("claw-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("CLAUDE.md"), "Always respond in haiku format.").unwrap();

    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("ok", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry();
    let mut session = make_session_with_cwd(tmp.clone());

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    let system = requests[0]
        .system
        .as_ref()
        .expect("system prompt should be set");
    assert!(
        system.contains("Always respond in haiku format"),
        "system prompt should include CLAUDE.md content, got: {}",
        system
    );

    // Cleanup
    let _ = std::fs::remove_dir_all(&tmp);
}

/// When no CLAUDE.md exists, the system prompt should be unaffected.
#[tokio::test]
async fn memory_prefetch_missing_file() {
    let tmp = std::env::temp_dir().join(format!("claw-test-empty-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    // No CLAUDE.md file created

    let config = SessionConfig {
        system_prompt: "base prompt".to_string(),
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };

    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("ok", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry();
    let mut session = clawcr_core::SessionState::new(config, tmp.clone());
    session.push_message(Message::user("hello"));

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    assert_eq!(
        requests[0].system,
        Some("base prompt".to_string()),
        "system prompt should be unchanged when CLAUDE.md is missing"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// 1.11  Usage statistics — cache token tracking
// ---------------------------------------------------------------------------

/// When the provider reports cache_creation and cache_read tokens, they
/// should be accumulated in the session and emitted via QueryEvent.
#[tokio::test]
async fn usage_includes_cache_tokens() {
    let provider = ScriptedProvider::builder()
        .turn(make_text_turn_with_usage(
            "response",
            StopReason::EndTurn,
            Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: Some(500),
                cache_read_input_tokens: Some(1200),
            },
        ))
        .build();

    let (registry, orchestrator) = setup_registry();
    let (callback, events) = event_collector();

    let mut session = make_session();
    query(
        &mut session,
        &provider,
        registry,
        &orchestrator,
        Some(callback),
    )
    .await
    .unwrap();

    assert_eq!(session.total_input_tokens, 100);
    assert_eq!(session.total_output_tokens, 50);
    assert_eq!(session.total_cache_creation_tokens, 500);
    assert_eq!(session.total_cache_read_tokens, 1200);

    let usage_events: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            QueryEvent::Usage {
                cache_creation_input_tokens,
                cache_read_input_tokens,
                ..
            } => Some((*cache_creation_input_tokens, *cache_read_input_tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(usage_events.len(), 1);
    assert_eq!(usage_events[0].0, Some(500));
    assert_eq!(usage_events[0].1, Some(1200));
}

// ---------------------------------------------------------------------------
// 1.8  Stop Hooks
// ---------------------------------------------------------------------------

/// When a stop hook is registered, it should be called when the query loop
/// exits normally (EndTurn with no tool calls).
#[tokio::test]
#[ignore = "1.8 stop hooks not yet implemented"]
async fn stop_hook_called_on_end_turn() {
    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("final answer", StopReason::EndTurn))
        .build();
    let (registry, orchestrator) = setup_registry();

    let mut session = make_session();

    // TODO: register a stop hook on the session or query config
    // let hook_called = Arc::new(AtomicBool::new(false));
    // session.register_stop_hook(|session| { hook_called.store(true, Ordering::SeqCst); Ok(()) });

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    // TODO: assert hook was called
    // assert!(hook_called.load(Ordering::SeqCst), "stop hook should have been called");
    panic!("stop hook registration API not yet available");
}

/// If a stop hook returns an error, the query should still complete
/// successfully — hook errors are non-fatal.
#[tokio::test]
#[ignore = "1.8 stop hooks not yet implemented"]
async fn stop_hook_error_non_fatal() {
    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();
    let (registry, orchestrator) = setup_registry();

    let mut session = make_session();

    // TODO: register a hook that returns Err
    // session.register_stop_hook(|_| anyhow::bail!("hook failed"));

    let result = query(&mut session, &provider, registry, &orchestrator, None).await;

    // Query should succeed even though hook failed
    assert!(result.is_ok(), "hook error should not fail the query");
    panic!("stop hook registration API not yet available");
}
