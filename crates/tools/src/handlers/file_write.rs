use std::path::PathBuf;

use async_trait::async_trait;
use diffy::PatchFormatter;
use diffy::create_patch;
use serde_json::json;
use tracing::info;

use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::{FunctionToolOutput, ToolInvocation, ToolOutput};
use crate::tool_handler::ToolHandler;

pub struct WriteHandler;

#[async_trait]
impl ToolHandler for WriteHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::Write
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let path_str = invocation.input["filePath"].as_str().ok_or_else(|| {
            ToolExecutionError::ExecutionFailed {
                message: "missing 'filePath' field".into(),
            }
        })?;
        let content = invocation.input["content"].as_str().ok_or_else(|| {
            ToolExecutionError::ExecutionFailed {
                message: "missing 'content' field".into(),
            }
        })?;

        let path = resolve_path(&invocation.cwd, path_str);
        info!(path = %path.display(), bytes = content.len(), "writing file");
        let previous = tokio::fs::read_to_string(&path).await.ok();

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ToolExecutionError::ExecutionFailed {
                    message: format!("failed to create directories: {e}"),
                }
            })?;
        }

        tokio::fs::write(&path, content).await.map_err(|e| {
            ToolExecutionError::ExecutionFailed {
                message: format!("failed to write file: {e}"),
            }
        })?;

        let metadata = build_write_metadata(&path, previous.as_deref(), content);
        Ok(Box::new(FunctionToolOutput::success_with_metadata(
            format!("wrote {} bytes to {}", content.len(), path.display()),
            metadata,
        )))
    }
}

fn resolve_path(cwd: &std::path::Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() { p } else { cwd.join(p) }
}

fn build_write_metadata(path: &std::path::Path, previous: Option<&str>, content: &str) -> serde_json::Value {
    match previous {
        None => json!({
            "diff": format!(
                "diff --git a/{0} b/{0}\nnew file mode 100644\n--- /dev/null\n+++ b/{0}\n@@ -0,0 +1,{1} @@\n{2}",
                path.display(),
                content.lines().count(),
                content
                    .lines()
                    .map(|line| format!("+{line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            "files": [{
                "path": path.display().to_string(),
                "kind": "add",
                "additions": content.lines().count(),
                "deletions": 0
            }]
        }),
        Some(old) => {
            let patch = create_patch(old, content);
            let patch_text = PatchFormatter::new().fmt_patch(&patch).to_string();
            let additions = content.lines().count();
            let deletions = old.lines().count();
            json!({
                "diff": format!(
                    "diff --git a/{0} b/{0}\n{1}",
                    path.display(),
                    patch_text
                ),
                "files": [{
                    "path": path.display().to_string(),
                    "kind": "update",
                    "additions": additions,
                    "deletions": deletions
                }]
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn build_write_metadata_for_new_file_marks_add() {
        let metadata = build_write_metadata(std::path::Path::new("foo.txt"), None, "hello\nworld\n");
        assert_eq!(metadata["files"][0]["kind"], "add");
        assert_eq!(metadata["files"][0]["additions"], 2);
    }

    #[test]
    fn build_write_metadata_for_existing_file_marks_update() {
        let metadata = build_write_metadata(std::path::Path::new("foo.txt"), Some("old\n"), "new\n");
        assert_eq!(metadata["files"][0]["kind"], "update");
        assert!(metadata["diff"].as_str().unwrap_or_default().contains("diff --git a/foo.txt b/foo.txt"));
        assert!(metadata["diff"].as_str().unwrap_or_default().contains("@@ -1 +1 @@"));
    }
}
