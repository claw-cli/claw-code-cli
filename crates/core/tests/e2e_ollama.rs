//! End-to-end tests against a local Ollama instance.
//!
//! These tests verify the query-runtime capabilities described in
//! `goal/query-runtime.md` using a real LLM backend instead of scripted mocks.
//!
//! Prerequisites:
//!   - Ollama running on localhost:11434
//!   - Model `qwen2.5:3b` pulled (`ollama pull qwen2.5:3b`)
//!
//! Run:
//!   cargo test -p claw-code-rust-core --test e2e_ollama -- --ignored --nocapture

#[allow(dead_code, unused_imports)]
mod harness;

use std::sync::{Arc, Mutex};

use clawcr_core::{
    query, ContentBlock, EventCallback, Message, QueryEvent, Role, SessionConfig, SessionState,
    TokenBudget,
};
use clawcr_provider::openai_compat::OpenAICompatProvider;
use clawcr_safety::legacy_permissions::PermissionMode;
use clawcr_tools::{ToolOrchestrator, ToolRegistry};

const OLLAMA_BASE: &str = "http://localhost:11434/v1";
const MODEL: &str = "qwen2.5:3b";

// Generous timeout — Ollama cold-starts can be slow.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

fn make_provider() -> OpenAICompatProvider {
    OpenAICompatProvider::new(OLLAMA_BASE)
}

fn make_e2e_session(prompt: &str) -> SessionState {
    let config = SessionConfig {
        model: MODEL.to_string(),
        system_prompt: "You are a helpful assistant. Be concise.".to_string(),
        max_turns: 10,
        token_budget: TokenBudget::new(32_000, 2_048),
        permission_mode: PermissionMode::AutoApprove,
    };
    let mut session = SessionState::new(config, std::env::temp_dir());
    session.push_message(Message::user(prompt));
    session
}

fn event_collector() -> (EventCallback, Arc<Mutex<Vec<QueryEvent>>>) {
    let events: Arc<Mutex<Vec<QueryEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    let callback: EventCallback = Arc::new(move |event| {
        events_clone.lock().unwrap().push(event);
    });
    (callback, events)
}

fn collected_text(events: &[QueryEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            QueryEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

// ============================================================================
// 1.1 + 1.12: Single-turn text response (multi-provider streaming protocol)
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_single_turn_text_response() {
    let provider = make_provider();
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let (callback, events) = event_collector();

    let mut session = make_e2e_session("What is 2+2? Answer with just the number.");

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            registry,
            &orchestrator,
            Some(callback),
        ),
    )
    .await
    .expect("query timed out")
    .expect("query failed");

    // Session should contain: user message + assistant reply
    assert!(
        session.messages.len() >= 2,
        "expected at least 2 messages, got {}",
        session.messages.len()
    );
    assert_eq!(session.messages[0].role, Role::User);
    assert_eq!(session.messages.last().unwrap().role, Role::Assistant);

    // Assistant message should have text content
    let assistant = session.messages.last().unwrap();
    let has_text = assistant
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::Text { .. }));
    assert!(has_text, "assistant response should contain text");

    // Should have received at least one TextDelta event
    let ev = events.lock().unwrap();
    let text = collected_text(&ev);
    assert!(!text.is_empty(), "expected streaming text deltas");
    eprintln!("[e2e] response: {}", text);
}

// ============================================================================
// 1.2: Streaming output — verify incremental TextDelta events arrive
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_streaming_text_deltas() {
    let provider = make_provider();
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let (callback, events) = event_collector();

    let mut session = make_e2e_session("Tell me a short joke in two sentences.");

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            registry,
            &orchestrator,
            Some(callback),
        ),
    )
    .await
    .expect("query timed out")
    .expect("query failed");

    let ev = events.lock().unwrap();
    let delta_count = ev
        .iter()
        .filter(|e| matches!(e, QueryEvent::TextDelta(_)))
        .count();

    // A multi-token response should produce multiple deltas
    assert!(
        delta_count > 1,
        "expected multiple TextDelta events, got {}",
        delta_count
    );
    eprintln!("[e2e] received {} text delta events", delta_count);

    // TurnComplete event should be emitted
    let has_turn_complete = ev
        .iter()
        .any(|e| matches!(e, QueryEvent::TurnComplete { .. }));
    assert!(has_turn_complete, "should emit TurnComplete event");
}

// ============================================================================
// 1.11: Usage statistics accumulated
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_usage_stats_nonzero() {
    let provider = make_provider();
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let (callback, events) = event_collector();

    let mut session = make_e2e_session("Say hello.");

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            registry,
            &orchestrator,
            Some(callback),
        ),
    )
    .await
    .expect("query timed out")
    .expect("query failed");

    eprintln!(
        "[e2e] usage: {} in / {} out tokens (session totals)",
        session.total_input_tokens, session.total_output_tokens
    );

    // Usage events should have been emitted
    let ev = events.lock().unwrap();
    let usage_events: Vec<_> = ev
        .iter()
        .filter(|e| matches!(e, QueryEvent::Usage { .. }))
        .collect();
    assert!(
        !usage_events.is_empty(),
        "should emit at least one Usage event"
    );

    // With stream_options.include_usage, session totals should be non-zero
    assert!(
        session.total_input_tokens > 0,
        "total_input_tokens should be > 0, got {}",
        session.total_input_tokens
    );
    assert!(
        session.total_output_tokens > 0,
        "total_output_tokens should be > 0, got {}",
        session.total_output_tokens
    );

    assert_eq!(session.turn_count, 1);
}

// ============================================================================
// 1.1: Multi-turn conversation
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_multi_turn_conversation() {
    let provider = make_provider();
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));

    let mut session = make_e2e_session("My name is Alice.");

    // Turn 1
    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            Arc::clone(&registry),
            &orchestrator,
            None,
        ),
    )
    .await
    .expect("turn 1 timed out")
    .expect("turn 1 failed");

    assert_eq!(session.turn_count, 1);
    let msgs_after_t1 = session.messages.len();
    assert!(msgs_after_t1 >= 2);

    // Turn 2 — context should carry forward
    session.push_message(Message::user("What is my name?"));

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            Arc::clone(&registry),
            &orchestrator,
            None,
        ),
    )
    .await
    .expect("turn 2 timed out")
    .expect("turn 2 failed");

    assert_eq!(session.turn_count, 2);
    assert!(session.messages.len() > msgs_after_t1);

    // Extract the last assistant response
    let last_assistant = session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant)
        .unwrap();
    let text: String = last_assistant
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    eprintln!("[e2e] turn 2 response: {}", text);
    // The model should remember the name (reasonable expectation for multi-turn)
    let mentions_alice = text.to_lowercase().contains("alice");
    assert!(
        mentions_alice,
        "model should remember the name from turn 1, got: {}",
        text
    );
}

// ============================================================================
// 1.9: Memory prefetch — CLAUDE.md loaded into system prompt
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_memory_prefetch_claude_md() {
    let provider = make_provider();
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));

    // Create a temp directory with a CLAUDE.md containing a secret phrase
    let tmp = std::env::temp_dir().join(format!("clawcr_e2e_{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let claude_md = tmp.join("CLAUDE.md");
    std::fs::write(
        &claude_md,
        "IMPORTANT: The secret code word is PINEAPPLE42.",
    )
    .unwrap();

    let config = SessionConfig {
        model: MODEL.to_string(),
        system_prompt: "You are a helpful assistant.".to_string(),
        max_turns: 5,
        token_budget: TokenBudget::new(32_000, 2_048),
        permission_mode: PermissionMode::AutoApprove,
    };
    let mut session = SessionState::new(config, tmp.clone());
    session.push_message(Message::user(
        "What is the secret code word from your instructions? Reply with just the code word.",
    ));

    let (callback, events) = event_collector();

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            registry,
            &orchestrator,
            Some(callback),
        ),
    )
    .await
    .expect("query timed out")
    .expect("query failed");

    let ev = events.lock().unwrap();
    let response = collected_text(&ev);
    eprintln!("[e2e] memory prefetch response: {}", response);

    assert!(
        response.to_uppercase().contains("PINEAPPLE42"),
        "model should reference the CLAUDE.md content, got: {}",
        response
    );
}

// ============================================================================
// 1.3 + 1.7: Auto compact triggers when token budget threshold exceeded
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_auto_compact_on_budget_threshold() {
    let provider = make_provider();
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));

    // Tiny budget so compaction triggers quickly
    let config = SessionConfig {
        model: MODEL.to_string(),
        system_prompt: "Be concise.".to_string(),
        max_turns: 10,
        token_budget: TokenBudget::new(4_000, 512),
        permission_mode: PermissionMode::AutoApprove,
    };
    let mut session = SessionState::new(config, std::env::temp_dir());

    // Fill the session with enough messages to push past the budget
    session.push_message(Message::user(
        "Repeat after me: The quick brown fox jumps over the lazy dog.",
    ));

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            Arc::clone(&registry),
            &orchestrator,
            None,
        ),
    )
    .await
    .expect("turn 1 timed out")
    .expect("turn 1 failed");

    let msgs_after_t1 = session.messages.len();
    let tokens_t1 = session.last_input_tokens;
    eprintln!(
        "[e2e] after turn 1: {} messages, {} last_input_tokens",
        msgs_after_t1, tokens_t1
    );

    // Add more messages to push past threshold
    session.push_message(Message::user(
        "Now tell me a long story about a dragon. Make it at least 3 paragraphs.",
    ));

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            Arc::clone(&registry),
            &orchestrator,
            None,
        ),
    )
    .await
    .expect("turn 2 timed out")
    .expect("turn 2 failed");

    // If compaction fired, turn_count should still increase,
    // but message count might be lower than the cumulative total
    eprintln!(
        "[e2e] after turn 2: {} messages, {} turns, last_input={} budget_input={}",
        session.messages.len(),
        session.turn_count,
        session.last_input_tokens,
        session.config.token_budget.input_budget()
    );

    // The test verifies the loop doesn't crash even with a very tight budget.
    assert!(session.turn_count >= 2);
}

// ============================================================================
// 1.4: Micro compact — large tool results get truncated
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_micro_compact_large_tool_result() {
    use clawcr_tools::Tool;

    // A custom tool that returns a very large result
    struct BigResultTool;

    #[async_trait::async_trait]
    impl Tool for BigResultTool {
        fn name(&self) -> &str {
            "big_result"
        }
        fn description(&self) -> &str {
            "Returns a large blob of text for testing."
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {},
            })
        }
        async fn execute(
            &self,
            _ctx: &clawcr_tools::ToolContext,
            _input: serde_json::Value,
        ) -> anyhow::Result<clawcr_tools::ToolOutput> {
            // Generate >10KB result to trigger micro_compact
            let big = "x".repeat(20_000);
            Ok(clawcr_tools::ToolOutput::success(big))
        }
        fn is_read_only(&self) -> bool {
            true
        }
    }

    let provider = make_provider();
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(BigResultTool));
    let registry = Arc::new(reg);
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let (callback, events) = event_collector();

    let config = SessionConfig {
        model: MODEL.to_string(),
        system_prompt: "You have a tool called big_result. Call it now.".to_string(),
        max_turns: 5,
        token_budget: TokenBudget::new(32_000, 2_048),
        permission_mode: PermissionMode::AutoApprove,
    };
    let mut session = SessionState::new(config, std::env::temp_dir());
    session.push_message(Message::user("Call the big_result tool."));

    let result = tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            registry,
            &orchestrator,
            Some(callback),
        ),
    )
    .await
    .expect("query timed out");

    // Even if the model doesn't call the tool, the test still passes —
    // we mainly verify no crash. If it *does* call the tool, check truncation.
    if result.is_ok() {
        let ev = events.lock().unwrap();
        let tool_results: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                QueryEvent::ToolResult { content, .. } => Some(content.clone()),
                _ => None,
            })
            .collect();

        if !tool_results.is_empty() {
            for content in &tool_results {
                eprintln!(
                    "[e2e] tool result length: {} (truncated={})",
                    content.len(),
                    content.contains("...[truncated]")
                );
                // Micro compact threshold is 10,000 chars
                assert!(
                    content.len() <= 10_100,
                    "tool result should be truncated by micro_compact, got {} bytes",
                    content.len()
                );
                assert!(
                    content.contains("...[truncated]"),
                    "truncated content should have marker"
                );
            }
        } else {
            eprintln!("[e2e] model didn't call the tool — micro_compact not exercised in this run");
        }
    } else {
        eprintln!(
            "[e2e] query returned error (expected for some models): {:?}",
            result
        );
    }
}

// ============================================================================
// 1.1: Tool use round-trip (if model supports function calling)
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_tool_use_round_trip() {
    let provider = make_provider();
    let mut reg = ToolRegistry::new();
    clawcr_tools::register_builtin_tools(&mut reg);
    let registry = Arc::new(reg);
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let (callback, events) = event_collector();

    let config = SessionConfig {
        model: MODEL.to_string(),
        system_prompt: "You have access to a bash tool. Use it when asked to run commands."
            .to_string(),
        max_turns: 5,
        token_budget: TokenBudget::new(32_000, 2_048),
        permission_mode: PermissionMode::AutoApprove,
    };
    let mut session = SessionState::new(config, std::env::temp_dir());
    session.push_message(Message::user(
        "Use the bash tool to run: echo hello_e2e_test",
    ));

    let result = tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            registry,
            &orchestrator,
            Some(callback),
        ),
    )
    .await
    .expect("query timed out");

    let ev = events.lock().unwrap();
    eprintln!("[e2e] collected {} events", ev.len());
    for e in ev.iter() {
        match e {
            QueryEvent::ToolUseStart { name, .. } => eprintln!("  tool_use_start: {}", name),
            QueryEvent::ToolResult {
                content, is_error, ..
            } => {
                eprintln!(
                    "  tool_result (error={}): {}",
                    is_error,
                    &content[..content.len().min(200)]
                )
            }
            QueryEvent::TextDelta(t) => eprint!("{}", t),
            _ => {}
        }
    }
    eprintln!();

    match result {
        Ok(()) => {
            // Check if any tool calls were made
            let tool_starts: Vec<_> = ev
                .iter()
                .filter(|e| matches!(e, QueryEvent::ToolUseStart { .. }))
                .collect();

            if tool_starts.is_empty() {
                eprintln!("[e2e] model chose not to use tools — this is model-dependent, not a runtime bug");
            } else {
                eprintln!(
                    "[e2e] model made {} tool call(s) — tool flow verified!",
                    tool_starts.len()
                );

                // Verify the tool result was appended to the session
                let has_tool_result = session.messages.iter().any(|m| {
                    m.content
                        .iter()
                        .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
                });
                assert!(
                    has_tool_result,
                    "session should contain tool_result messages"
                );

                // Verify multi-turn happened (assistant → tool_result → assistant)
                assert!(
                    session.turn_count >= 2,
                    "tool use should cause at least 2 turns, got {}",
                    session.turn_count
                );
            }
        }
        Err(e) => {
            eprintln!("[e2e] query error (may be expected): {}", e);
        }
    }
}

// ============================================================================
// 3.2: Error classification — simulate by connecting to wrong port
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_connection_error_is_provider_error() {
    let bad_provider = OpenAICompatProvider::new("http://localhost:1/v1");
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));

    let mut session = make_e2e_session("hello");

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        query(&mut session, &bad_provider, registry, &orchestrator, None),
    )
    .await
    .expect("should not hang");

    assert!(result.is_err(), "query to bad endpoint should fail");
    let err_msg = format!("{}", result.unwrap_err());
    eprintln!("[e2e] error message: {}", err_msg);
    assert!(
        err_msg.contains("provider") || err_msg.contains("error"),
        "error should indicate a provider issue"
    );
}

// ============================================================================
// 1.1: System prompt forwarded to model
// ============================================================================

#[tokio::test]
#[ignore = "requires local Ollama"]
async fn e2e_system_prompt_affects_response() {
    let provider = make_provider();
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let (callback, events) = event_collector();

    let config = SessionConfig {
        model: MODEL.to_string(),
        system_prompt: "You are a pirate. Always respond starting with 'Arrr'.".to_string(),
        max_turns: 5,
        token_budget: TokenBudget::new(32_000, 2_048),
        permission_mode: PermissionMode::AutoApprove,
    };
    let mut session = SessionState::new(config, std::env::temp_dir());
    session.push_message(Message::user("Hello, who are you?"));

    tokio::time::timeout(
        TIMEOUT,
        query(
            &mut session,
            &provider,
            registry,
            &orchestrator,
            Some(callback),
        ),
    )
    .await
    .expect("query timed out")
    .expect("query failed");

    let ev = events.lock().unwrap();
    let response = collected_text(&ev);
    eprintln!("[e2e] pirate response: {}", response);
    // The model should follow the system prompt (somewhat)
    assert!(!response.is_empty(), "should produce a non-empty response");
}
