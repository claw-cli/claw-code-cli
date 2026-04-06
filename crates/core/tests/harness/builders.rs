use std::path::PathBuf;

use anyhow::Result;

use clawcr_provider::{ModelResponse, ResponseContent, StopReason, StreamEvent, Usage};
use clawcr_safety::legacy_permissions::PermissionMode;

use clawcr_core::{SessionConfig, SessionState};

/// Build a `SessionState` with sensible defaults for testing.
/// The user message "hello" is pre-loaded.
pub fn make_session() -> SessionState {
    let config = SessionConfig {
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };
    let mut session = SessionState::new(config, std::env::temp_dir());
    session.push_message(clawcr_core::Message::user("hello"));
    session
}

/// Build a `SessionState` with a custom config.
pub fn make_session_with_config(config: SessionConfig) -> SessionState {
    let mut session = SessionState::new(config, std::env::temp_dir());
    session.push_message(clawcr_core::Message::user("hello"));
    session
}

/// Build a `SessionState` pointing to a specific working directory.
pub fn make_session_with_cwd(cwd: PathBuf) -> SessionState {
    let config = SessionConfig {
        permission_mode: PermissionMode::AutoApprove,
        ..Default::default()
    };
    let mut session = SessionState::new(config, cwd);
    session.push_message(clawcr_core::Message::user("hello"));
    session
}

/// Generate a stream event sequence for a pure-text response turn.
pub fn make_text_turn(text: &str, stop_reason: StopReason) -> Vec<Result<StreamEvent>> {
    make_text_turn_with_usage(text, stop_reason, Usage::default())
}

/// Generate a text turn with specific usage stats.
pub fn make_text_turn_with_usage(
    text: &str,
    stop_reason: StopReason,
    usage: Usage,
) -> Vec<Result<StreamEvent>> {
    vec![
        Ok(StreamEvent::TextDelta {
            index: 0,
            text: text.to_string(),
        }),
        Ok(StreamEvent::MessageDone {
            response: ModelResponse {
                id: format!("resp-text-{}", uuid_short()),
                content: vec![ResponseContent::Text(text.to_string())],
                stop_reason: Some(stop_reason),
                usage,
            },
        }),
    ]
}

/// Generate a stream event sequence for a single tool-use turn.
pub fn make_tool_turn(
    tool_id: &str,
    tool_name: &str,
    input: serde_json::Value,
    stop_reason: StopReason,
) -> Vec<Result<StreamEvent>> {
    make_tool_turn_with_usage(tool_id, tool_name, input, stop_reason, Usage::default())
}

/// Generate a tool turn with specific usage stats.
pub fn make_tool_turn_with_usage(
    tool_id: &str,
    tool_name: &str,
    input: serde_json::Value,
    stop_reason: StopReason,
    usage: Usage,
) -> Vec<Result<StreamEvent>> {
    let input_json = serde_json::to_string(&input).unwrap();
    vec![
        Ok(StreamEvent::ContentBlockStart {
            index: 0,
            content: ResponseContent::ToolUse {
                id: tool_id.to_string(),
                name: tool_name.to_string(),
                input: input.clone(),
            },
        }),
        Ok(StreamEvent::InputJsonDelta {
            index: 0,
            partial_json: input_json,
        }),
        Ok(StreamEvent::MessageDone {
            response: ModelResponse {
                id: format!("resp-tool-{}", uuid_short()),
                content: vec![ResponseContent::ToolUse {
                    id: tool_id.to_string(),
                    name: tool_name.to_string(),
                    input,
                }],
                stop_reason: Some(stop_reason),
                usage,
            },
        }),
    ]
}

/// Generate a stream event sequence with multiple tool uses in one turn.
pub fn make_multi_tool_turn(
    tools: &[(&str, &str, serde_json::Value)], // (id, name, input)
    stop_reason: StopReason,
) -> Vec<Result<StreamEvent>> {
    let mut events = Vec::new();
    let mut response_content = Vec::new();

    for (i, (id, name, input)) in tools.iter().enumerate() {
        events.push(Ok(StreamEvent::ContentBlockStart {
            index: i,
            content: ResponseContent::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: input.clone(),
            },
        }));
        let input_json = serde_json::to_string(input).unwrap();
        events.push(Ok(StreamEvent::InputJsonDelta {
            index: i,
            partial_json: input_json,
        }));
        response_content.push(ResponseContent::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input: input.clone(),
        });
    }

    events.push(Ok(StreamEvent::MessageDone {
        response: ModelResponse {
            id: format!("resp-multi-{}", uuid_short()),
            content: response_content,
            stop_reason: Some(stop_reason),
            usage: Usage::default(),
        },
    }));

    events
}

/// Generate a text turn split into multiple TextDelta chunks.
pub fn make_chunked_text_turn(
    chunks: &[&str],
    stop_reason: StopReason,
) -> Vec<Result<StreamEvent>> {
    let full_text: String = chunks.iter().copied().collect();
    let mut events: Vec<Result<StreamEvent>> = chunks
        .iter()
        .map(|chunk| {
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: chunk.to_string(),
            })
        })
        .collect();

    events.push(Ok(StreamEvent::MessageDone {
        response: ModelResponse {
            id: format!("resp-chunked-{}", uuid_short()),
            content: vec![ResponseContent::Text(full_text)],
            stop_reason: Some(stop_reason),
            usage: Usage::default(),
        },
    }));
    events
}

/// Generate a tool turn where the JSON input arrives in multiple deltas.
pub fn make_tool_turn_with_json_chunks(
    tool_id: &str,
    tool_name: &str,
    json_chunks: &[&str],
    full_input: serde_json::Value,
    stop_reason: StopReason,
) -> Vec<Result<StreamEvent>> {
    let mut events = vec![Ok(StreamEvent::ContentBlockStart {
        index: 0,
        content: ResponseContent::ToolUse {
            id: tool_id.to_string(),
            name: tool_name.to_string(),
            input: full_input.clone(),
        },
    })];

    for chunk in json_chunks {
        events.push(Ok(StreamEvent::InputJsonDelta {
            index: 0,
            partial_json: chunk.to_string(),
        }));
    }

    events.push(Ok(StreamEvent::MessageDone {
        response: ModelResponse {
            id: format!("resp-json-chunks-{}", uuid_short()),
            content: vec![ResponseContent::ToolUse {
                id: tool_id.to_string(),
                name: tool_name.to_string(),
                input: full_input,
            }],
            stop_reason: Some(stop_reason),
            usage: Usage::default(),
        },
    }));

    events
}

fn uuid_short() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}
