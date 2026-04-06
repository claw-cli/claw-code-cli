#[allow(dead_code, unused_imports)]
mod harness;

use std::sync::Arc;

use serde_json::json;

use clawcr_provider::{StopReason, Usage};
use clawcr_safety::legacy_permissions::PermissionMode;
use clawcr_tools::{ToolOrchestrator, ToolRegistry};

use clawcr_core::{query, ContentBlock, QueryEvent, Role, SessionConfig};

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

fn setup_registry_with_tools(tools: Vec<SpyTool>) -> (Arc<ToolRegistry>, ToolOrchestrator) {
    let mut reg = ToolRegistry::new();
    for tool in tools {
        reg.register(Arc::new(tool));
    }
    let registry = Arc::new(reg);
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    (registry, orchestrator)
}

// ---------------------------------------------------------------------------
// 1.1 Multi-turn conversation loop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_turn_text_only() {
    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("Hello back!", StopReason::EndTurn))
        .build();
    let (registry, orchestrator) = setup_registry();

    let mut session = make_session();
    let result = query(&mut session, &provider, registry, &orchestrator, None).await;

    assert!(result.is_ok());
    // user("hello") + assistant("Hello back!")
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, Role::User);
    assert_eq!(session.messages[1].role, Role::Assistant);
    assert_eq!(session.turn_count, 1);

    match &session.messages[1].content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "Hello back!"),
        other => panic!("expected Text, got {:?}", other),
    }
}

#[tokio::test]
async fn multi_turn_with_tools() {
    let spy = SpyTool::new("my_tool", false);

    let provider = ScriptedProvider::builder()
        // Turn 1: model calls my_tool
        .turn(make_tool_turn(
            "t1",
            "my_tool",
            json!({"x": 1}),
            StopReason::ToolUse,
        ))
        // Turn 2: model calls my_tool again
        .turn(make_tool_turn(
            "t2",
            "my_tool",
            json!({"x": 2}),
            StopReason::ToolUse,
        ))
        // Turn 3: model responds with text
        .turn(make_text_turn("all done", StopReason::EndTurn))
        .build();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session();

    let result = query(&mut session, &provider, registry, &orchestrator, None).await;
    assert!(result.is_ok());
    assert_eq!(session.turn_count, 3);

    // Messages: user, assistant(tool_use), user(tool_result),
    //           assistant(tool_use), user(tool_result),
    //           assistant(text)
    assert_eq!(session.messages.len(), 6);
    assert_eq!(session.messages[0].role, Role::User);
    assert_eq!(session.messages[1].role, Role::Assistant);
    assert_eq!(session.messages[2].role, Role::User);
    assert_eq!(session.messages[3].role, Role::Assistant);
    assert_eq!(session.messages[4].role, Role::User);
    assert_eq!(session.messages[5].role, Role::Assistant);
}

// ---------------------------------------------------------------------------
// 1.2 Streaming output
// ---------------------------------------------------------------------------

#[tokio::test]
async fn streaming_text_deltas() {
    let chunks = ["He", "llo", " ", "wo", "rld"];
    let provider = ScriptedProvider::builder()
        .turn(make_chunked_text_turn(&chunks, StopReason::EndTurn))
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

    let text_deltas: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            QueryEvent::TextDelta(t) => Some(t.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(text_deltas.len(), 5);
    let assembled: String = text_deltas.into_iter().collect();
    assert_eq!(assembled, "Hello world");
}

#[tokio::test]
async fn streaming_tool_json_assembly() {
    let full_input = json!({"command": "ls -la", "cwd": "/tmp"});
    // Split the serialized JSON into 3 chunks at safe boundaries
    let serialized = serde_json::to_string(&full_input).unwrap();
    let mid1 = serialized.len() / 3;
    let mid2 = mid1 * 2;
    let chunk1 = &serialized[..mid1];
    let chunk2 = &serialized[mid1..mid2];
    let chunk3 = &serialized[mid2..];
    let json_chunks = [chunk1, chunk2, chunk3];
    let spy = SpyTool::new("bash", false);

    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn_with_json_chunks(
            "t1",
            "bash",
            &json_chunks,
            full_input.clone(),
            StopReason::ToolUse,
        ))
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session();

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let assistant_msg = &session.messages[1];
    let tool_use = assistant_msg
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::ToolUse { input, .. } => Some(input),
            _ => None,
        })
        .expect("assistant message should contain ToolUse");

    assert_eq!(tool_use, &full_input);
}

// ---------------------------------------------------------------------------
// 1.1 Loop termination
// ---------------------------------------------------------------------------

#[tokio::test]
async fn max_turns_exceeded() {
    let spy = SpyTool::new("my_tool", false);

    // Model always returns tool_use — will exceed max_turns
    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
        ))
        .turn(make_tool_turn(
            "t2",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
        ))
        // Third turn would be needed but max_turns=2
        .build();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
    let mut session = make_session_with_config(SessionConfig {
        max_turns: 2,
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    });

    let result = query(&mut session, &provider, registry, &orchestrator, None).await;
    match result {
        Err(clawcr_core::AgentError::MaxTurnsExceeded(n)) => assert_eq!(n, 2),
        other => panic!("expected MaxTurnsExceeded(2), got {:?}", other),
    }
}

#[tokio::test]
async fn empty_tool_calls_terminates() {
    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("final answer", StopReason::EndTurn))
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

    let turn_complete = events.lock().unwrap().iter().any(|e| {
        matches!(
            e,
            QueryEvent::TurnComplete {
                stop_reason: StopReason::EndTurn
            }
        )
    });
    assert!(turn_complete, "should emit TurnComplete with EndTurn");
}

// ---------------------------------------------------------------------------
// 1.1 Multiple concurrent tool calls in one turn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_concurrent_tool_calls() {
    let read_tool = SpyTool::new("read_tool", true);
    let write_tool = SpyTool::new("write_tool", false);

    let provider = ScriptedProvider::builder()
        .turn(make_multi_tool_turn(
            &[
                ("t1", "read_tool", json!({"path": "/etc/hosts"})),
                ("t2", "write_tool", json!({"path": "/tmp/out"})),
            ],
            StopReason::ToolUse,
        ))
        .turn(make_text_turn("done", StopReason::EndTurn))
        .build();

    let (registry, orchestrator) = setup_registry_with_tools(vec![read_tool, write_tool]);
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

    // Both tool results should be emitted
    let tool_results: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, QueryEvent::ToolResult { .. }))
        .cloned()
        .collect();
    assert_eq!(tool_results.len(), 2);

    // The user message after tools should have 2 ToolResult blocks
    let tool_result_msg = &session.messages[2]; // user, assistant(2 tools), user(2 results)
    assert_eq!(tool_result_msg.role, Role::User);
    assert_eq!(tool_result_msg.content.len(), 2);
    assert!(tool_result_msg
        .content
        .iter()
        .all(|b| matches!(b, ContentBlock::ToolResult { .. })));
}

// ---------------------------------------------------------------------------
// 1.11 Usage token accumulation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usage_tokens_accumulated() {
    let spy = SpyTool::new("my_tool", false);

    let provider = ScriptedProvider::builder()
        .turn(make_tool_turn_with_usage(
            "t1",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
            Usage {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            },
        ))
        .turn(make_tool_turn_with_usage(
            "t2",
            "my_tool",
            json!({}),
            StopReason::ToolUse,
            Usage {
                input_tokens: 200,
                output_tokens: 80,
                ..Default::default()
            },
        ))
        .turn(make_text_turn_with_usage(
            "done",
            StopReason::EndTurn,
            Usage {
                input_tokens: 300,
                output_tokens: 120,
                ..Default::default()
            },
        ))
        .build();

    let (registry, orchestrator) = setup_registry_with_tool(spy);
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

    assert_eq!(session.total_input_tokens, 600);
    assert_eq!(session.total_output_tokens, 250);
    assert_eq!(session.turn_count, 3);

    let usage_events: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            QueryEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => Some((*input_tokens, *output_tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(usage_events.len(), 3);
    assert_eq!(usage_events[0], (100, 50));
    assert_eq!(usage_events[1], (200, 80));
    assert_eq!(usage_events[2], (300, 120));
}

// ---------------------------------------------------------------------------
// 1.1 System prompt forwarding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn system_prompt_forwarded() {
    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("ok", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry();
    let mut session = make_session_with_config(SessionConfig {
        system_prompt: "You are a helpful assistant.".to_string(),
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    });

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].system,
        Some("You are a helpful assistant.".to_string())
    );
}

// ---------------------------------------------------------------------------
// 1.1 Tool definitions included in request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_definitions_included() {
    let tool_a = SpyTool::new("tool_a", true);
    let tool_b = SpyTool::new("tool_b", false);

    let provider = ScriptedProvider::builder()
        .turn(make_text_turn("ok", StopReason::EndTurn))
        .build();
    let captured = provider.captured_requests.clone();

    let (registry, orchestrator) = setup_registry_with_tools(vec![tool_a, tool_b]);
    let mut session = make_session();

    query(&mut session, &provider, registry, &orchestrator, None)
        .await
        .unwrap();

    let requests = captured.lock().unwrap();
    let tools = requests[0].tools.as_ref().expect("tools should be present");
    assert_eq!(tools.len(), 2);

    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"tool_a"));
    assert!(names.contains(&"tool_b"));
}
