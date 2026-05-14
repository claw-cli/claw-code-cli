use std::path::PathBuf;

use async_trait::async_trait;

use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::{FunctionToolOutput, ToolInvocation, ToolOutput};
use crate::read::{is_binary_file, missing_file_message, read_directory, read_file};
use crate::tool_handler::ToolHandler;

pub struct ReadHandler;

#[async_trait]
impl ToolHandler for ReadHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::Read
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let mut filepath = invocation.input["filePath"]
            .as_str()
            .ok_or_else(|| ToolExecutionError::ExecutionFailed {
                message: "missing 'filePath' field".into(),
            })?
            .to_string();
        let offset = invocation.input["offset"].as_u64().map(|v| v as usize);
        let limit = invocation.input["limit"].as_u64().map(|v| v as usize);

        if let Some(offset) = offset
            && offset < 1
        {
            return Ok(Box::new(FunctionToolOutput::error(
                "offset must be greater than or equal to 1",
            )));
        }

        if !PathBuf::from(&filepath).is_absolute() {
            filepath = invocation.cwd.join(&filepath).to_string_lossy().to_string();
        }

        let path = PathBuf::from(&filepath);
        if !path.exists() {
            return Ok(Box::new(FunctionToolOutput::error(missing_file_message(
                &filepath,
            ))));
        }

        if path.is_dir() {
            let output = read_directory(&path, limit.unwrap_or(usize::MAX), offset.unwrap_or(1));
            let output = output.map_err(|e| ToolExecutionError::ExecutionFailed {
                message: format!("{}", e),
            })?;
            return Ok(Box::new(output));
        }

        let is_bin = is_binary_file(&path);
        let is_bin = is_bin.map_err(|e| ToolExecutionError::ExecutionFailed {
            message: format!("{}", e),
        })?;
        if is_bin {
            return Ok(Box::new(FunctionToolOutput::error(format!(
                "Cannot read binary file: {}",
                path.display()
            ))));
        }

        let output = read_file(&path, limit.unwrap_or(usize::MAX), offset.unwrap_or(1));
        let output = output.map_err(|e| ToolExecutionError::ExecutionFailed {
            message: format!("{}", e),
        })?;
        Ok(Box::new(output))
    }
}
