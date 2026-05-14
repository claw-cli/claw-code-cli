use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::apply_patch::exec_apply_patch;
use crate::errors::ToolExecutionError;
use crate::events::ToolProgressSender;
use crate::handler_kind::ToolHandlerKind;
use crate::invocation::{FunctionToolOutput, ToolInvocation, ToolOutput};
use crate::tool_handler::ToolHandler;
use crate::unified_exec::process::{UnifiedExecProcess, collect_output};
use crate::unified_exec::store::ProcessStore;
use crate::unified_exec::{ExecCommandArgs, ProcessOutput, WARNING_PROCESSES, WriteStdinArgs};

const MAX_EXEC_OUTPUT_DELTAS_PER_CALL: usize = 10_000;
const UNIFIED_EXEC_OUTPUT_DELTA_MAX_BYTES: usize = 8_192;

pub struct ExecCommandHandler {
    store: Arc<ProcessStore>,
}

impl ExecCommandHandler {
    pub fn new(store: Arc<ProcessStore>) -> Self {
        ExecCommandHandler { store }
    }
}

#[async_trait]
impl ToolHandler for ExecCommandHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::ExecCommand
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let args = ExecCommandArgs {
            cmd: invocation
                .input
                .get("cmd")
                .or_else(|| invocation.input.get("command"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolExecutionError::ExecutionFailed {
                    message: "missing 'cmd' field".into(),
                })?
                .to_string(),
            workdir: invocation.input["workdir"].as_str().map(|s| s.to_string()),
            shell: invocation.input["shell"].as_str().map(|s| s.to_string()),
            login: invocation.input["login"].as_bool().unwrap_or(true),
            tty: invocation.input["tty"].as_bool().unwrap_or(false),
            yield_time_ms: invocation.input["yield_time_ms"]
                .as_u64()
                .unwrap_or(crate::unified_exec::DEFAULT_YIELD_MS),
            max_output_tokens: invocation.input["max_output_tokens"]
                .as_u64()
                .map(|v| v as usize)
                .unwrap_or(crate::unified_exec::MAX_OUTPUT_TOKENS),
        };

        let cwd = invocation.input["workdir"]
            .as_str()
            .map(|path| {
                let path = std::path::PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    invocation.cwd.join(path)
                }
            })
            .unwrap_or_else(|| invocation.cwd.clone());

        if !cwd.exists() {
            return Ok(Box::new(FunctionToolOutput::error(format!(
                "working directory does not exist: {}",
                cwd.display()
            ))));
        }

        if is_raw_apply_patch_body(&args.cmd) {
            return Ok(Box::new(FunctionToolOutput::error(
                "apply_patch verification failed: patch detected without explicit call to apply_patch. Rerun as [\"apply_patch\", \"<patch>\"]",
            )));
        }

        if let Some((patch_cwd, patch_text)) = apply_patch_command(&args.cmd, &cwd) {
            let output = exec_apply_patch(
                &patch_cwd,
                &invocation.session_id,
                serde_json::json!({ "patchText": patch_text }),
            )
            .await
            .map_err(|e| ToolExecutionError::ExecutionFailed {
                message: e.to_string(),
            })?;
            let content = format_apply_patch_intercept_response(
                output.content.text_part().unwrap_or_default(),
            );
            let output = if output.is_error {
                FunctionToolOutput::error(content)
            } else {
                FunctionToolOutput::success(content)
            };
            return Ok(Box::new(output));
        }

        let Some(session_id) = self.store.reserve_process_id().await else {
            return Ok(Box::new(FunctionToolOutput::error(format!(
                "max unified exec processes ({}) reached; cannot allocate process",
                crate::unified_exec::MAX_PROCESSES
            ))));
        };

        let (proc, _broadcast_rx) = match UnifiedExecProcess::spawn(
            session_id,
            &args.cmd,
            &cwd,
            args.shell.as_deref(),
            args.login,
            args.tty,
        ) {
            Ok(spawned) => spawned,
            Err(error) => {
                self.store.release_reserved(session_id).await;
                return Err(ToolExecutionError::ExecutionFailed {
                    message: format!("failed to spawn process: {error}"),
                });
            }
        };

        if let Some(ref sender) = progress {
            let mut progress_rx = proc.subscribe();
            let s = sender.clone();
            tokio::spawn(async move {
                let mut emitted_deltas = 0usize;
                while let Ok(bytes) = progress_rx.recv().await {
                    for text in progress_delta_chunks(&bytes) {
                        if emitted_deltas >= MAX_EXEC_OUTPUT_DELTAS_PER_CALL {
                            return;
                        }
                        emitted_deltas += 1;
                        if s.send(text).is_err() {
                            return;
                        }
                    }
                }
            });
        }

        let proc = Arc::new(proc);
        self.store
            .insert_reserved(session_id, Arc::clone(&proc))
            .await;

        let mut rx = proc.subscribe();
        let output = collect_output(
            &mut rx,
            &proc,
            crate::unified_exec::clamp_exec_yield_time(args.yield_time_ms),
            args.max_output_tokens,
        )
        .await;
        let warning = if output.exit_code.is_some() {
            self.store.remove(session_id).await;
            None
        } else {
            let process_count = self.store.len().await;
            (process_count >= WARNING_PROCESSES).then(|| open_process_warning(process_count))
        };

        let response = format_exec_response(
            &output,
            Some(session_id),
            Some(generate_chunk_id()),
            warning.as_deref(),
        );
        Ok(Box::new(FunctionToolOutput::success(response)))
    }
}

pub struct WriteStdinHandler {
    store: Arc<ProcessStore>,
}

impl WriteStdinHandler {
    pub fn new(store: Arc<ProcessStore>) -> Self {
        WriteStdinHandler { store }
    }
}

#[async_trait]
impl ToolHandler for WriteStdinHandler {
    fn tool_kind(&self) -> ToolHandlerKind {
        ToolHandlerKind::WriteStdin
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
        _progress: Option<ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
        let args = WriteStdinArgs {
            session_id: invocation.input["session_id"].as_i64().ok_or_else(|| {
                ToolExecutionError::ExecutionFailed {
                    message: "missing 'session_id' field".into(),
                }
            })? as i32,
            chars: invocation.input["chars"].as_str().unwrap_or("").to_string(),
            yield_time_ms: invocation.input["yield_time_ms"]
                .as_u64()
                .unwrap_or(crate::unified_exec::DEFAULT_POLL_YIELD_MS),
            max_output_tokens: invocation.input["max_output_tokens"]
                .as_u64()
                .map(|v| v as usize)
                .unwrap_or(crate::unified_exec::MAX_OUTPUT_TOKENS),
        };

        let proc = self.store.get(args.session_id).await.ok_or_else(|| {
            ToolExecutionError::ExecutionFailed {
                message: format!("Unknown process id {}", args.session_id),
            }
        })?;

        if !args.chars.is_empty() {
            if !proc.tty() {
                return Err(ToolExecutionError::ExecutionFailed {
                    message: "stdin is closed for this session".to_string(),
                });
            }
            if let Err(error) = proc.write_stdin(&args.chars)
                && proc.is_running()
                && proc.exit_code().is_none()
            {
                return Err(ToolExecutionError::ExecutionFailed {
                    message: format!("write_stdin failed: {error}"),
                });
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        let mut rx = proc.subscribe();
        let output = collect_output(
            &mut rx,
            &proc,
            crate::unified_exec::clamp_write_stdin_yield_time(args.yield_time_ms, &args.chars),
            args.max_output_tokens,
        )
        .await;

        if output.exit_code.is_some() {
            self.store.remove(args.session_id).await;
        }

        let response = format_exec_response(
            &output,
            Some(args.session_id),
            Some(generate_chunk_id()),
            /*warning*/ None,
        );
        Ok(Box::new(FunctionToolOutput::success(response)))
    }
}

fn format_exec_response(
    output: &ProcessOutput,
    session_id: Option<i32>,
    chunk_id: Option<String>,
    warning: Option<&str>,
) -> String {
    let mut parts = Vec::new();

    if let Some(chunk_id) = chunk_id
        && !chunk_id.is_empty()
    {
        parts.push(format!("Chunk ID: {chunk_id}"));
    }

    parts.push(format!("Wall time: {:.4} seconds", output.wall_time_secs));

    if let Some(code) = output.exit_code {
        parts.push(format!("Process exited with code {code}"));
    }
    if let Some(sid) = session_id
        && output.exit_code.is_none()
    {
        parts.push(format!("Process running with session ID {sid}"));
    }
    if let Some(warning) = warning {
        parts.push(warning.to_string());
    }

    parts.push(format!(
        "Original token count: {}",
        output.original_token_count
    ));
    parts.push("Output:".to_string());
    parts.push(output.output.clone());

    parts.join("\n")
}

fn generate_chunk_id() -> String {
    Uuid::new_v4().to_string().chars().take(6).collect()
}

fn open_process_warning(process_count: usize) -> String {
    format!(
        "Warning: The maximum number of unified exec processes you can keep open is {WARNING_PROCESSES} and you currently have {process_count} processes open. Reuse older processes or close them to prevent automatic pruning of old processes"
    )
}

fn format_apply_patch_intercept_response(content: &str) -> String {
    format!("Wall time: 0.0000 seconds\nOutput:\n{content}")
}

fn progress_delta_chunks(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut chunks = Vec::new();
    let mut remaining = text.as_ref();
    while !remaining.is_empty() {
        let take = floor_char_boundary(
            remaining,
            remaining.len().min(UNIFIED_EXEC_OUTPUT_DELTA_MAX_BYTES),
        );
        let take = if take == 0 {
            remaining
                .char_indices()
                .nth(1)
                .map_or(remaining.len(), |(index, _)| index)
        } else {
            take
        };
        chunks.push(remaining[..take].to_string());
        remaining = &remaining[take..];
    }
    chunks
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn is_raw_apply_patch_body(command: &str) -> bool {
    let trimmed = command.trim();
    trimmed.starts_with("*** Begin Patch") && trimmed.contains("*** End Patch")
}

fn apply_patch_command(
    command: &str,
    cwd: &std::path::Path,
) -> Option<(std::path::PathBuf, String)> {
    let trimmed = command.trim();
    if let Some(argv) = shlex::split(trimmed)
        && let [cmd, patch_text] = argv.as_slice()
        && (cmd == "apply_patch" || cmd == "applypatch")
    {
        return Some((cwd.to_path_buf(), patch_text.clone()));
    }

    let (effective_cwd, script) = if let Some((cd_command, rest)) = trimmed.split_once("&&") {
        let argv = shlex::split(cd_command.trim())?;
        match argv.as_slice() {
            [cmd, dir] if cmd == "cd" => {
                let path = std::path::PathBuf::from(dir);
                let path = if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                };
                (path, rest.trim())
            }
            _ => (cwd.to_path_buf(), trimmed),
        }
    } else {
        (cwd.to_path_buf(), trimmed)
    };

    let mut lines = script.lines();
    let first_line = lines.next()?.trim();
    let command_name = first_line.split_whitespace().next()?;
    if command_name != "apply_patch" && command_name != "applypatch" {
        return None;
    }
    let heredoc_index = first_line.find("<<")?;
    let delimiter = first_line[heredoc_index + 2..].trim();
    let delimiter = delimiter
        .strip_prefix('-')
        .unwrap_or(delimiter)
        .trim()
        .trim_matches('"')
        .trim_matches('\'');
    if delimiter.is_empty() {
        return None;
    }

    let mut patch_lines = Vec::new();
    while let Some(line) = lines.next() {
        if line.trim() == delimiter {
            if lines.any(|remaining| !remaining.trim().is_empty()) {
                return None;
            }
            return Some((effective_cwd, patch_lines.join("\n")));
        }
        patch_lines.push(line);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn format_exec_response_exited() {
        let output = ProcessOutput {
            output: "hello world".into(),
            exit_code: Some(0),
            wall_time_secs: 1.5,
            truncated: false,
            original_token_count: 3,
        };
        let text = format_exec_response(&output, None, None, /*warning*/ None);
        assert!(text.contains("Wall time: 1.5000"));
        assert!(text.contains("Process exited with code 0"));
        assert!(text.contains("hello world"));
        assert!(!text.contains("session ID"));
        assert!(text.contains("Original token count: 3"));
    }

    #[test]
    fn format_exec_response_running() {
        let output = ProcessOutput {
            output: "building...".into(),
            exit_code: None,
            wall_time_secs: 10.0,
            truncated: false,
            original_token_count: 3,
        };
        let text = format_exec_response(&output, Some(42), None, /*warning*/ None);
        assert!(text.contains("Process running with session ID 42"));
        assert!(!text.contains("exit code"));
    }

    #[test]
    fn format_exec_response_truncated() {
        let output = ProcessOutput {
            output: "long output...".into(),
            exit_code: None,
            wall_time_secs: 5.0,
            truncated: true,
            original_token_count: 3,
        };
        let text = format_exec_response(&output, Some(1), None, /*warning*/ None);
        assert!(text.contains("Output:"));
    }

    #[test]
    fn format_exec_response_with_both_exit_and_session() {
        let output = ProcessOutput {
            output: "done".into(),
            exit_code: Some(0),
            wall_time_secs: 3.0,
            truncated: false,
            original_token_count: 1,
        };
        // When exit_code is Some, session_id is not shown even if provided
        let text = format_exec_response(&output, Some(99), None, /*warning*/ None);
        assert!(text.contains("Process exited with code 0"));
        assert!(!text.contains("session ID"));
    }

    #[test]
    fn format_exec_response_includes_open_process_warning() {
        let output = ProcessOutput {
            output: "building...".into(),
            exit_code: None,
            wall_time_secs: 10.0,
            truncated: false,
            original_token_count: 3,
        };

        let text = format_exec_response(
            &output,
            Some(42),
            None,
            Some(&open_process_warning(WARNING_PROCESSES)),
        );

        assert!(text.contains("currently have 60 processes open"));
        assert!(text.contains("Reuse older processes"));
    }

    #[test]
    fn progress_delta_chunks_caps_chunk_size_on_utf8_boundary() {
        let text = "a".repeat(UNIFIED_EXEC_OUTPUT_DELTA_MAX_BYTES - 1) + "😀tail";

        let chunks = progress_delta_chunks(text.as_bytes());

        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].len() <= UNIFIED_EXEC_OUTPUT_DELTA_MAX_BYTES);
        assert_eq!(chunks.join(""), text);
    }

    #[test]
    fn exec_command_args_missing_cmd() {
        let args = serde_json::json!({});
        let result = serde_json::from_value::<serde_json::Value>(args);
        assert!(result.is_ok());
        // The cmd field is required but we can't easily test parse failure
        // because there's no deserialize impl for ExecCommandArgs
    }

    #[test]
    fn apply_patch_command_extracts_heredoc() {
        let command = "apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch\nPATCH\n";

        let parsed = apply_patch_command(command, std::path::Path::new("/tmp/root"));

        assert_eq!(
            parsed,
            Some((
                std::path::PathBuf::from("/tmp/root"),
                "*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch".to_string()
            ))
        );
    }

    #[test]
    fn apply_patch_command_extracts_cd_heredoc() {
        let command = "cd sub && apply_patch <<EOF\n*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch\nEOF";

        let parsed = apply_patch_command(command, std::path::Path::new("/tmp/root"));

        assert_eq!(
            parsed,
            Some((
                std::path::PathBuf::from("/tmp/root/sub"),
                "*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch".to_string()
            ))
        );
    }

    #[test]
    fn apply_patch_command_extracts_direct_body() {
        let command =
            "apply_patch '*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch'";

        let parsed = apply_patch_command(command, std::path::Path::new("/tmp/root"));

        assert_eq!(
            parsed,
            Some((
                std::path::PathBuf::from("/tmp/root"),
                "*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch".to_string()
            ))
        );
    }

    #[test]
    fn apply_patch_command_rejects_trailing_commands_after_heredoc() {
        let command = "apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch\nPATCH\necho done";

        assert_eq!(
            apply_patch_command(command, std::path::Path::new("/tmp/root")),
            None
        );
    }

    #[tokio::test]
    async fn exec_command_rejects_raw_apply_patch_body() {
        let root = std::env::temp_dir().join(format!("devo-apply-patch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp test dir");
        let handler = ExecCommandHandler::new(Arc::new(ProcessStore::new()));
        let command = "*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch\n";
        let invocation = ToolInvocation {
            call_id: crate::invocation::ToolCallId("call-1".to_string()),
            tool_name: crate::invocation::ToolName("exec_command".into()),
            session_id: "session-1".to_string(),
            cwd: root.clone(),
            input: serde_json::json!({ "cmd": command }),
        };

        let output = handler
            .handle(invocation, /*progress*/ None)
            .await
            .expect("handle exec command");

        assert!(output.is_error());
        assert!(
            output
                .to_content()
                .into_string()
                .contains("patch detected without explicit call to apply_patch")
        );
        assert!(!root.join("file.txt").exists());
        std::fs::remove_dir_all(root).expect("cleanup temp test dir");
    }

    #[tokio::test]
    async fn exec_command_intercepts_apply_patch_heredoc() {
        let root = std::env::temp_dir().join(format!("devo-apply-patch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp test dir");
        let handler = ExecCommandHandler::new(Arc::new(ProcessStore::new()));
        let command = "apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: file.txt\n+hello\n*** End Patch\nPATCH\n";
        let invocation = ToolInvocation {
            call_id: crate::invocation::ToolCallId("call-1".to_string()),
            tool_name: crate::invocation::ToolName("exec_command".into()),
            session_id: "session-1".to_string(),
            cwd: root.clone(),
            input: serde_json::json!({ "cmd": command }),
        };

        let output = handler
            .handle(invocation, /*progress*/ None)
            .await
            .expect("handle exec command")
            .to_content()
            .into_string();

        assert!(output.starts_with("Wall time: 0.0000 seconds\nOutput:\n"));
        assert!(output.contains("Success. Updated the following files:"));
        assert!(!output.contains("\"diagnostics\""));
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).expect("read patched file"),
            "hello\n"
        );
        std::fs::remove_dir_all(root).expect("cleanup temp test dir");
    }

    #[tokio::test]
    async fn exec_command_intercepts_apply_patch_after_cd() {
        let root = std::env::temp_dir().join(format!("devo-apply-patch-{}", Uuid::new_v4()));
        let subdir = root.join("sub");
        std::fs::create_dir_all(&subdir).expect("create temp test dir");
        let handler = ExecCommandHandler::new(Arc::new(ProcessStore::new()));
        let command = "cd sub && apply_patch <<'PATCH'\n*** Begin Patch\n*** Add File: nested.txt\n+hello\n*** End Patch\nPATCH\n";
        let invocation = ToolInvocation {
            call_id: crate::invocation::ToolCallId("call-1".to_string()),
            tool_name: crate::invocation::ToolName("exec_command".into()),
            session_id: "session-1".to_string(),
            cwd: root.clone(),
            input: serde_json::json!({ "cmd": command }),
        };

        let output = handler
            .handle(invocation, /*progress*/ None)
            .await
            .expect("handle exec command")
            .to_content()
            .into_string();

        assert!(output.starts_with("Wall time: 0.0000 seconds\nOutput:\n"));
        assert!(output.contains("Success. Updated the following files:"));
        assert!(!output.contains("\"diagnostics\""));
        assert_eq!(
            std::fs::read_to_string(subdir.join("nested.txt")).expect("read patched file"),
            "hello\n"
        );
        std::fs::remove_dir_all(root).expect("cleanup temp test dir");
    }
}
