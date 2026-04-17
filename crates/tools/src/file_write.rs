use std::path::PathBuf;

use crate::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use clawcr_safety::legacy_permissions::{PermissionDecision, PermissionRequest, ResourceKind};
use serde_json::json;
use tracing::info;

const DESCRIPTION: &str = include_str!("write.txt");

/// Write content to a file, creating directories as needed.
pub struct FileWriteTool;

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "filePath": {
                    "type": "string",
                    "description": "The absolute or relative file path to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["filePath", "content"]
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let path_str = input["filePath"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'filePath' field"))?;
        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'content' field"))?;

        let path = resolve_path(&ctx.cwd, path_str);

        let perm_request = PermissionRequest {
            tool_name: "file_write".into(),
            resource: ResourceKind::FileWrite,
            description: format!("write file: {}", path.display()),
            target: Some(path.to_string_lossy().to_string()),
        };

        match ctx.permissions.check(&perm_request).await {
            PermissionDecision::Allow => {}
            PermissionDecision::Deny { reason } => {
                return Ok(ToolOutput::error(format!("permission denied: {}", reason)));
            }
            PermissionDecision::Ask { message } => {
                return Ok(ToolOutput::error(format!(
                    "permission required — run with --permission interactive to approve: {}",
                    message
                )));
            }
        }

        info!(path = %path.display(), bytes = content.len(), "writing file");

        if let Some(parent) = path.parent()
            && let Err(e) = tokio::fs::create_dir_all(parent).await
        {
            return Ok(ToolOutput::error(format!(
                "failed to create directories: {}",
                e
            )));
        }

        match tokio::fs::write(&path, content).await {
            Ok(_) => Ok(ToolOutput::success(format!(
                "wrote {} bytes to {}",
                content.len(),
                path.display()
            ))),
            Err(e) => Ok(ToolOutput::error(format!("failed to write file: {}", e))),
        }
    }

    fn is_read_only(&self) -> bool {
        false
    }
}

fn resolve_path(cwd: &std::path::Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() { p } else { cwd.join(p) }
}
