use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use pretty_assertions::assert_eq;
use serde_json::json;

use clawcr_core::{ContentBlock, Message, Model, SessionConfig, SessionState, TurnConfig, query};
use clawcr_provider::{
    ModelRequest, ModelResponse, ResponseContent, StopReason, StreamEvent, Usage,
};
use clawcr_tools::{Tool, ToolOrchestrator, ToolOutput, ToolRegistry};

struct UpdatePlanToolUseProvider {
    requests: AtomicUsize,
}

#[async_trait]
impl clawcr_provider::ModelProviderSDK for UpdatePlanToolUseProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
        unreachable!("tests stream responses only")
    }

    async fn completion_stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let request_number = self.requests.fetch_add(1, Ordering::SeqCst);

        let events = if request_number == 0 {
            vec![
                Ok(StreamEvent::ContentBlockStart {
                    index: 0,
                    content: ResponseContent::ToolUse {
                        id: "tool-1".into(),
                        name: "update_plan".into(),
                        input: json!({ "plan": [] }),
                    },
                }),
                Ok(StreamEvent::InputJsonDelta {
                    index: 0,
                    partial_json: r#"{"plan":[]}"#.into(),
                }),
                Ok(StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp-1".into(),
                        content: vec![ResponseContent::ToolUse {
                            id: "tool-1".into(),
                            name: "update_plan".into(),
                            input: json!({ "plan": [] }),
                        }],
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Usage::default(),
                        metadata: Default::default(),
                    },
                }),
            ]
        } else {
            vec![Ok(StreamEvent::MessageDone {
                response: ModelResponse {
                    id: "resp-2".into(),
                    content: vec![ResponseContent::Text("done".into())],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Usage::default(),
                    metadata: Default::default(),
                },
            })]
        };

        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn name(&self) -> &str {
        "update-plan-tool-use-provider"
    }
}

struct OversizedUpdatePlanErrorTool;

#[async_trait]
impl Tool for OversizedUpdatePlanErrorTool {
    fn name(&self) -> &str {
        "update_plan"
    }

    fn description(&self) -> &str {
        "A test-only update_plan tool that emits an oversized error."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "plan": { "type": "array" }
            },
            "required": ["plan"]
        })
    }

    async fn execute(
        &self,
        _ctx: &clawcr_tools::ToolContext,
        _input: serde_json::Value,
    ) -> Result<ToolOutput> {
        Ok(ToolOutput::error(
            "oversized update_plan error ".repeat(512),
        ))
    }
}

#[tokio::test]
async fn errored_update_plan_results_still_micro_compact() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(OversizedUpdatePlanErrorTool));
    let registry = Arc::new(registry);
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let provider = UpdatePlanToolUseProvider {
        requests: AtomicUsize::new(0),
    };
    let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
    session.push_message(Message::user("update the plan"));

    query(
        &mut session,
        &TurnConfig {
            model: Model::default(),
            thinking_selection: None,
        },
        &provider,
        registry,
        &orchestrator,
        None,
    )
    .await
    .expect("query should complete");

    let tool_result_message = session
        .messages
        .iter()
        .find(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        })
        .expect("tool_result message should be appended");
    let tool_result_blocks = tool_result_message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                content, is_error, ..
            } => Some((content, is_error)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(tool_result_blocks.len(), 1);
    let (content, is_error) = tool_result_blocks[0];
    assert_eq!(*is_error, true);
    assert!(
        content.contains("...[truncated]"),
        "errored update_plan results should still be micro-compacted"
    );
    assert!(
        content.len() < "oversized update_plan error ".repeat(512).len(),
        "errored update_plan content should shrink after compaction"
    );
}
