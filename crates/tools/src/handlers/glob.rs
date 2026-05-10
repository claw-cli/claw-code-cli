use async_trait::async_trait;
use tracing::debug;

use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::{FunctionToolOutput, ToolInvocation, ToolOutput};
use crate::tool_handler::ToolHandler;

pub struct GlobHandler;

#[async_trait]
impl ToolHandler for GlobHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::Glob
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let pattern = invocation.input["pattern"].as_str().ok_or_else(|| {
            ToolExecutionError::ExecutionFailed {
                message: "missing 'pattern' field".into(),
            }
        })?;

        let base = match invocation.input["path"].as_str() {
            Some(p) => {
                let pb = std::path::PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else {
                    invocation.cwd.join(pb)
                }
            }
            None => invocation.cwd.clone(),
        };

        debug!(pattern, base = %base.display(), "glob search");

        let full_pattern = base.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let mut entries: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();

        match glob::glob(&pattern_str) {
            Ok(paths) => {
                for entry in paths.flatten() {
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    entries.push((entry, mtime));
                }
            }
            Err(e) => {
                return Ok(Box::new(FunctionToolOutput::error(format!(
                    "invalid glob pattern: {e}"
                ))));
            }
        }

        entries.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));

        if entries.is_empty() {
            return Ok(Box::new(FunctionToolOutput::success("(no matches)")));
        }

        let lines: Vec<String> = entries
            .iter()
            .map(|(p, _)| p.to_string_lossy().to_string())
            .collect();

        Ok(Box::new(FunctionToolOutput::success(lines.join("\n"))))
    }
}
