use async_trait::async_trait;

use crate::apply_patch::exec_apply_patch;
use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::{ToolInvocation, ToolOutput};
use crate::tool_handler::ToolHandler;

pub struct ApplyPatchHandler;

#[async_trait]
impl ToolHandler for ApplyPatchHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::ApplyPatch
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let output = exec_apply_patch(&invocation.cwd, &invocation.session_id, invocation.input)
            .await
            .map_err(|e| ToolExecutionError::ExecutionFailed {
                message: e.to_string(),
            })?;
        Ok(Box::new(output))
    }
}
