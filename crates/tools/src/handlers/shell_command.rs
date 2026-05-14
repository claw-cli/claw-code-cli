use std::path::PathBuf;

use async_trait::async_trait;

use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::{ToolInvocation, ToolOutput};
use crate::shell_exec::{
    ShellExecRequest, default_max_output_tokens, default_timeout_ms, default_yield_time_ms,
    execute_shell_command,
};
use crate::tool_handler::ToolHandler;

pub struct ShellCommandHandler;

#[async_trait]
impl ToolHandler for ShellCommandHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::ShellCommand
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let command = invocation
            .input
            .get("command")
            .or_else(|| invocation.input.get("cmd"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolExecutionError::ExecutionFailed {
                message: "missing 'command' field".into(),
            })?;

        let workdir = invocation
            .input
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| invocation.cwd.clone());

        let timeout_ms = invocation.input["timeout_ms"]
            .as_u64()
            .unwrap_or(default_timeout_ms());

        let login = invocation.input["login"].as_bool().unwrap_or(true);

        let output = execute_shell_command(
            ShellExecRequest {
                command: command.to_string(),
                workdir,
                description: "shell command".into(),
                shell_override: None,
                tty: false,
                login,
                timeout_ms,
                yield_time_ms: default_yield_time_ms(),
                max_output_tokens: default_max_output_tokens(),
            },
            progress,
        )
        .await
        .map_err(|e| ToolExecutionError::ExecutionFailed {
            message: e.to_string(),
        })?;

        Ok(Box::new(output))
    }
}
