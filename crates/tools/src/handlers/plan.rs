use async_trait::async_trait;
use serde_json::json;

use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::{FunctionToolOutput, ToolInvocation, ToolOutput};
use crate::tool_handler::ToolHandler;

pub struct PlanHandler;

#[async_trait]
impl ToolHandler for PlanHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::Plan
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let explanation = invocation
            .input
            .get("explanation")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let plan = invocation
            .input
            .get("plan")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolExecutionError::ExecutionFailed {
                message: "missing 'plan' field".into(),
            })?;

        let in_progress_count = plan
            .iter()
            .filter(|item| item.get("status").and_then(|v| v.as_str()) == Some("in_progress"))
            .count();
        if in_progress_count > 1 {
            return Ok(Box::new(FunctionToolOutput::error(
                "At most one step can be in_progress at a time.",
            )));
        }

        let plan_text = serde_json::to_string_pretty(plan).map_err(|e| {
            ToolExecutionError::ExecutionFailed {
                message: e.to_string(),
            }
        })?;
        let content = if explanation.trim().is_empty() {
            plan_text
        } else {
            format!("{explanation}\n\n{plan_text}")
        };

        Ok(Box::new(FunctionToolOutput {
            content: crate::invocation::ToolContent::Mixed {
                text: Some(content),
                json: Some(json!({
                    "explanation": explanation,
                    "plan": plan,
                })),
            },
            is_error: false,
            display_content: None,
        }))
    }
}
