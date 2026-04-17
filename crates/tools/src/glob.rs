use crate::{Tool, ToolContext, ToolOutput};
use async_trait::async_trait;
use serde_json::json;
use tracing::debug;

const DESCRIPTION: &str = include_str!("glob.txt");

/// Find files matching a glob pattern, sorted by modification time.
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g. \"**/*.rs\")"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: cwd)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'pattern' field"))?;

        let base = match input["path"].as_str() {
            Some(p) => {
                let pb = std::path::PathBuf::from(p);
                if pb.is_absolute() {
                    pb
                } else {
                    ctx.cwd.join(pb)
                }
            }
            None => ctx.cwd.clone(),
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
            Err(e) => return Ok(ToolOutput::error(format!("invalid glob pattern: {}", e))),
        }

        // Sort newest first
        entries.sort_by_key(|b| std::cmp::Reverse(b.1));

        if entries.is_empty() {
            return Ok(ToolOutput::success("(no matches)"));
        }

        let lines: Vec<String> = entries
            .iter()
            .map(|(p, _)| p.to_string_lossy().to_string())
            .collect();

        Ok(ToolOutput::success(lines.join("\n")))
    }

    fn is_read_only(&self) -> bool {
        true
    }
}
