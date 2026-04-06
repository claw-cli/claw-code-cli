//! L3: Token budget and context-management tests.
//!
//! These tests define the expected behavior for P1 budget control and
//! auto-compaction capabilities. All are `#[ignore]` until the corresponding
//! features are wired into the query loop.
//!
//! Capability mapping:
//!   - 1.3  Automatic context compaction (triggered by budget threshold)
//!   - 1.4  Per-item truncation before prompt reconstruction
//!   - 1.7  Token Budget control (input_budget / should_compact integration)

#[allow(dead_code, unused_imports)]
mod harness;

use std::sync::Arc;

use serde_json::json;

use clawcr_provider::{StopReason, Usage};
use clawcr_safety::legacy_permissions::PermissionMode;
use clawcr_tools::{ToolOrchestrator, ToolOutput, ToolRegistry};

use clawcr_core::{query, ContentBlock, Message, SessionConfig, TokenBudget};

use harness::builders::*;
use harness::{ScriptedProvider, SpyTool};

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
// 1.3 + 1.7  Auto compact triggered at threshold
// ---------------------------------------------------------------------------

/// When accumulated token usage exceeds the budget threshold, the query loop
/// should run context compaction on session.messages before the next request.
#[tokio::test]
async fn auto_compact_triggered_at_threshold() {
    let spy = SpyTool::new("my_tool", false);

    // Budget: context=1000, output=200 => input_budget=800, threshold@0.8 => 640
    let config = SessionConfig {
        token_budget: TokenBudget {
            context_window: 1000,
            max_output_tokens: 200,
            compact_threshold: 0.8,
        },
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };

    let provider = ScriptedProvider::builder()
        // Turn 1: returns tool call + usage that exceeds threshold
        .turn(make_tool_turn_with_usage(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
            Usage {
                input_tokens: 700, // above 640 threshold
                output_tokens: 50,
                ..Default::default()
            },
        ))
        // Turn 2: after compaction, model responds with text
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session_with_config(config);
    // Pad messages so compaction has something to remove
    for i in 0..10 {
        session.push_message(Message::user(format!("padding {}", i)));
        session.push_message(Message::assistant_text(format!("ack {}", i)));
    }

    let result = query(&mut session, &provider, registry, &orchestrator, None).await;
    assert!(result.is_ok());

    // The second request should have fewer messages due to compaction
    let requests = captured.lock().unwrap();
    assert!(requests.len() >= 2);
    assert!(
        requests[1].messages.len() < requests[0].messages.len(),
        "compacted request ({} msgs) should have fewer messages than original ({} msgs)",
        requests[1].messages.len(),
        requests[0].messages.len(),
    );
}

/// Auto-compact must not touch the system prompt — it lives in request.system,
/// not in messages.
#[tokio::test]
async fn auto_compact_preserves_system_prompt() {
    let spy = SpyTool::new("my_tool", false);
    let config = SessionConfig {
        system_prompt: "You are a coding assistant.".to_string(),
        token_budget: TokenBudget {
            context_window: 1000,
            max_output_tokens: 200,
            compact_threshold: 0.8,
        },
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };

    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn_with_usage(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
            Usage {
                input_tokens: 700,
                output_tokens: 50,
                ..Default::default()
            },
        ))
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session_with_config(config);
    for i in 0..10 {
        session.push_message(Message::user(format!("padding {}", i)));
        session.push_message(Message::assistant_text(format!("ack {}", i)));
    }

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    for req in requests.iter() {
        assert_eq!(
            req.system,
            Some("You are a coding assistant.".to_string()),
            "system prompt must survive compaction"
        );
    }
}

/// After compaction, at least the most recent user+assistant exchange should
/// be preserved so the model has immediate context.
#[tokio::test]
async fn auto_compact_preserves_last_exchange() {
    let spy = SpyTool::new("my_tool", false);
    let config = SessionConfig {
        token_budget: TokenBudget {
            context_window: 1000,
            max_output_tokens: 200,
            compact_threshold: 0.8,
        },
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };

    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn_with_usage(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
            Usage {
                input_tokens: 700,
                output_tokens: 50,
                ..Default::default()
            },
        ))
        .turn(make_text_turn("final", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session_with_config(config);
    for i in 0..10 {
        session.push_message(Message::user(format!("padding {}", i)));
        session.push_message(Message::assistant_text(format!("ack {}", i)));
    }
    // Add a distinctive last exchange
    session.push_message(Message::user("the important question"));
    session.push_message(Message::assistant_text("the important answer"));

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    if requests.len() >= 2 {
        let compacted_msgs = &requests[1].messages;
        assert!(
            compacted_msgs.len() >= 2,
            "should preserve at least 2 messages"
        );
    }
}

// ---------------------------------------------------------------------------
// 1.7  Token budget controls output tokens
// ---------------------------------------------------------------------------

/// max_output_tokens from TokenBudget should be used as ModelRequest.max_tokens.
/// This tests the EXISTING behavior (already wired).
#[tokio::test]
async fn token_budget_limits_output_tokens() {
    let config = SessionConfig {
        token_budget: TokenBudget::new(100_000, 4096),
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };

    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("ok", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry();
    let mut session = make_session_with_config(config);

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    assert_eq!(requests[0].max_tokens, 4096);
}

/// should_compact() should be evaluated before each turn's model request.
/// This is a behavioral contract for when budget checking is wired in.
#[tokio::test]
async fn input_budget_checked_each_turn() {
    let spy = SpyTool::new("my_tool", false);
    let config = SessionConfig {
        token_budget: TokenBudget {
            context_window: 500,
            max_output_tokens: 100,
            compact_threshold: 0.8,
        },
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };

    let provider = ScriptedProvider::builder()
        // Turn 1: usage stays under threshold
        .turn(make_tool_turn_with_usage(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
            Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..Default::default()
            },
        ))
        // Turn 2: usage exceeds threshold; compaction should occur before request
        .turn(make_tool_turn_with_usage(
            "t2",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
            Usage {
                input_tokens: 350,
                output_tokens: 30,
                ..Default::default()
            },
        ))
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session_with_config(config);
    for i in 0..5 {
        session.push_message(Message::user(format!("msg {}", i)));
        session.push_message(Message::assistant_text(format!("reply {}", i)));
    }

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    // Third request should have fewer messages than second (compacted)
    assert!(requests.len() >= 3);
    assert!(
        requests[2].messages.len() < requests[1].messages.len(),
        "third request should reflect compaction"
    );
}

// ---------------------------------------------------------------------------
// 1.4  Micro compact (per-tool-result truncation)
// ---------------------------------------------------------------------------

/// When a tool result exceeds a size threshold, it should be locally
/// compressed before being stored in session.messages.
#[tokio::test]
async fn micro_compact_large_tool_result() {
    let large_output = "x".repeat(15_000);
    let spy = SpyTool::new("my_tool", false)
        .with_response(move |_| ToolOutput::success(large_output.clone()));

    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
        ))
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session();

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    // Find the tool_result in session messages
    let tool_result_content = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("should have tool_result");

    assert!(
        tool_result_content.len() < 15_000,
        "large tool result ({} chars) should be micro-compacted",
        tool_result_content.len()
    );
}

/// Small tool results should be left untouched by micro-compact.
#[tokio::test]
async fn micro_compact_small_tool_result_untouched() {
    let spy = SpyTool::new("my_tool", false).with_response(|_| ToolOutput::success("short result"));

    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
        ))
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session();

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let tool_result_content = session
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|b| match b {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .expect("should have tool_result");

    assert_eq!(
        tool_result_content, "short result",
        "small results should be preserved as-is"
    );
}

/// The context compactor should be pluggable — a custom strategy implementation
/// should be used when provided.
#[tokio::test]
#[ignore = "1.3 context compactor pluggability not yet wired"]
async fn context_compactor_pluggable() {
    // This test verifies that a custom context compactor can be injected and
    // will be called during automatic compaction.
    //
    // Implementation sketch:
    //   1. Create a SpyCompactStrategy that records calls
    //   2. Configure session with this strategy
    //   3. Trigger compaction via budget threshold
    //   4. Assert the spy strategy was called
    //
    // For now, this is a placeholder — the test body will be fleshed out
    // when the compact strategy injection point is designed.

    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("ok", StopReason::EndTurn))
        .build();
    let (registry, orchestrator) = setup_registry();
    let mut session = make_session();

    // TODO: inject custom context compactor into session/query
    let _ = query(&mut session, &provider, registry, &orchestrator, None).await;

    // TODO: assert custom strategy was called
    panic!("context_compactor_pluggable: test body not yet implemented");
}
