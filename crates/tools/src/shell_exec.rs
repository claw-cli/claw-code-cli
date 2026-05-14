use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Instant;
use tokio::process::Command;
use tokio::time::{Duration, timeout};
use tracing::info;

use crate::FunctionToolOutput;
use crate::events::ToolProgressSender;

const MAX_METADATA_LENGTH: usize = 30_000;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_YIELD_TIME_MS: u64 = 1_000;
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 16_000;

pub(crate) struct ShellExecRequest {
    pub command: String,
    pub workdir: PathBuf,
    pub description: String,
    pub shell_override: Option<String>,
    pub tty: bool,
    pub login: bool,
    pub timeout_ms: u64,
    pub yield_time_ms: u64,
    pub max_output_tokens: usize,
}

struct PtyRunConfig {
    shell: ShellSpec,
    command_to_run: String,
    workdir: PathBuf,
    description: String,
    timeout_ms: u64,
    yield_time_ms: u64,
    max_output_tokens: usize,
}

pub(crate) fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

pub(crate) fn default_yield_time_ms() -> u64 {
    DEFAULT_YIELD_TIME_MS
}

pub(crate) fn default_max_output_tokens() -> usize {
    DEFAULT_MAX_OUTPUT_TOKENS
}

#[allow(dead_code)]
pub(crate) fn windows_destructive_filesystem_guidance() -> &'static str {
    r#"Windows safety rules:
- Do not compose destructive filesystem commands across shells. Do not enumerate paths in PowerShell and then pass them to `cmd /c`, batch builtins, or another shell for deletion or moving. Use one shell end-to-end, prefer native PowerShell cmdlets such as `Remove-Item` / `Move-Item` with `-LiteralPath`, and avoid string-built shell commands for file operations.
- Before any recursive delete or move on Windows, verify the resolved absolute target paths stay within the intended workspace or explicitly named target directory. Never issue a recursive delete or move against a computed path if the final target has not been checked."#
}

#[allow(dead_code)]
pub(crate) fn shell_command_description() -> String {
    if cfg!(windows) {
        format!(
            r#"Runs a Powershell command (Windows) and returns its output.

Examples of valid command strings:

- ls -a (show hidden): "Get-ChildItem -Force"
- recursive find by name: "Get-ChildItem -Recurse -Filter *.py"
- recursive grep: "Get-ChildItem -Path C:\myrepo -Recurse | Select-String -Pattern 'TODO' -CaseSensitive"
- ps aux | grep python: "Get-Process | Where-Object {{ $_.ProcessName -like '*python*' }}"
- setting an env var: "$env:FOO='bar'; echo $env:FOO"
- running an inline Python script: "@'\nprint('Hello, world!')\n'@ | python -"

{}"#,
            windows_destructive_filesystem_guidance()
        )
    } else {
        "Runs a shell command and returns its output.\n- Always set the `workdir` param when using the shell_command function. Do not use `cd` unless absolutely necessary.".to_string()
    }
}

pub(crate) async fn execute_shell_command(
    request: ShellExecRequest,
    progress: Option<ToolProgressSender>,
) -> anyhow::Result<FunctionToolOutput> {
    let ShellExecRequest {
        command,
        workdir,
        description,
        shell_override,
        tty,
        login,
        timeout_ms,
        yield_time_ms,
        max_output_tokens,
    } = request;

    if !workdir.exists() {
        return Ok(FunctionToolOutput::error(format!(
            "working directory does not exist: {}",
            workdir.display()
        )));
    }

    let shell = resolve_shell(shell_override.as_deref(), login);
    let command_to_run = if cfg!(windows) && shell.program.eq_ignore_ascii_case("powershell") {
        format!(
            concat!(
                "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); ",
                "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); ",
                "$OutputEncoding = [System.Text.UTF8Encoding]::new($false); ",
                "[System.Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); ",
                "{}"
            ),
            command
        )
    } else {
        command
    };

    if tty {
        return run_with_pty(
            PtyRunConfig {
                shell,
                command_to_run,
                workdir,
                description,
                timeout_ms,
                yield_time_ms,
                max_output_tokens,
            },
            progress,
        )
        .await;
    }

    info!(command = %command_to_run, shell = shell.program, "executing shell command");
    let command_preview = preview(&command_to_run);
    let mut child = Command::new(shell.program);
    child
        .args(shell.args)
        .arg(&command_to_run)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&workdir);

    if cfg!(windows) {
        child.env("PYTHONUTF8", "1");
    }

    let result = timeout(Duration::from_millis(timeout_ms), child.output()).await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            let result_text = merge_streams(&stdout, &stderr);
            if let Some(ref sender) = progress {
                let _ = sender.send(result_text.clone());
            }
            let result_text = truncate_output(&result_text, max_output_tokens);
            if output.status.success() {
                Ok(FunctionToolOutput::success_with_metadata(
                    result_text.clone(),
                    json!({
                        "output": preview(&result_text),
                        "command": command_preview,
                        "exit": output.status.code(),
                        "description": description,
                        "cwd": workdir,
                        "yield_time_ms": yield_time_ms,
                    }),
                ))
            } else {
                let code = output.status.code().unwrap_or(-1);
                Ok(FunctionToolOutput::error(format!(
                    "exit code {code}\n{result_text}"
                )))
            }
        }
        Ok(Err(error)) => Ok(FunctionToolOutput::error(format!(
            "failed to spawn process: {error}"
        ))),
        Err(_) => Ok(FunctionToolOutput::error(format!(
            "command timed out after {timeout_ms}ms"
        ))),
    }
}

struct ShellSpec {
    program: &'static str,
    args: &'static [&'static str],
}

fn resolve_shell(shell: Option<&str>, login: bool) -> ShellSpec {
    let shell = shell.unwrap_or("");
    let normalized = shell.to_ascii_lowercase();

    if normalized.contains("powershell") || normalized == "pwsh" || normalized == "powershell" {
        return ShellSpec {
            program: "powershell",
            args: &["-NoLogo", "-NoProfile", "-Command"],
        };
    }

    if normalized.ends_with("cmd") || normalized.ends_with("cmd.exe") || normalized == "cmd" {
        return ShellSpec {
            program: "cmd",
            args: &["/C"],
        };
    }

    if normalized.contains("zsh") {
        return ShellSpec {
            program: "zsh",
            args: if login { &["-lc"] } else { &["-c"] },
        };
    }

    if normalized.contains("bash") {
        return ShellSpec {
            program: "bash",
            args: if login { &["-lc"] } else { &["-c"] },
        };
    }

    if login {
        platform_shell(true)
    } else {
        platform_shell(false)
    }
}

#[cfg(test)]
pub(crate) fn platform_shell_program(login: bool) -> &'static str {
    platform_shell(login).program
}

pub(crate) fn preview(text: &str) -> String {
    if text.len() <= MAX_METADATA_LENGTH {
        return text.to_string();
    }
    format!("{}\n\n...", &text[..MAX_METADATA_LENGTH])
}

pub(crate) fn truncate_output(text: &str, max_output_tokens: usize) -> String {
    if max_output_tokens == 0 {
        return String::new();
    }
    let max_chars = max_output_tokens.saturating_mul(4);
    let mut out: String = text.chars().take(max_chars).collect();
    if out.len() < text.len() {
        out.push_str("\n\n... [truncated]");
    }
    out
}

pub(crate) fn merge_streams(stdout: &str, stderr: &str) -> String {
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("[stderr]\n");
        result.push_str(stderr);
    }
    if result.is_empty() {
        "(no output)".to_string()
    } else {
        result
    }
}

fn platform_shell(login: bool) -> ShellSpec {
    if cfg!(windows) {
        ShellSpec {
            program: "powershell",
            args: &["-NoProfile", "-Command"],
        }
    } else {
        ShellSpec {
            program: "bash",
            args: if login { &["-lc"] } else { &["-c"] },
        }
    }
}

async fn run_with_pty(
    config: PtyRunConfig,
    progress: Option<ToolProgressSender>,
) -> anyhow::Result<FunctionToolOutput> {
    let PtyRunConfig {
        shell,
        command_to_run,
        workdir,
        description,
        timeout_ms,
        yield_time_ms,
        max_output_tokens,
    } = config;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| anyhow::anyhow!("failed to open PTY: {error}"))?;

    let mut builder = CommandBuilder::new(shell.program);
    builder.args(shell.args);
    builder.arg(&command_to_run);
    builder.cwd(&workdir);
    if cfg!(windows) {
        builder.env("PYTHONUTF8", "1");
        builder.env("TERM", "xterm-256color");
        builder.env("COLORTERM", "truecolor");
    }

    let mut child = pair
        .slave
        .spawn_command(builder)
        .map_err(|error| anyhow::anyhow!("failed to spawn PTY command: {error}"))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| anyhow::anyhow!("failed to clone PTY reader: {error}"))?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match std::io::Read::read(&mut reader, &mut buffer) {
                Ok(0) => break,
                Ok(size) => {
                    if tx.send(buffer[..size].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let started = Instant::now();
    let sleep_ms = yield_time_ms.max(10);
    let timeout = Duration::from_millis(timeout_ms);
    let mut output = Vec::new();
    let mut exit_code = None;
    let mut timed_out = false;

    loop {
        while let Ok(chunk) = rx.try_recv() {
            output.extend_from_slice(&chunk);
            if let Some(ref sender) = progress {
                let text = String::from_utf8_lossy(&chunk).into_owned();
                let _ = sender.send(text);
            }
        }

        if let Some(status) = child
            .try_wait()
            .map_err(|error| anyhow::anyhow!("failed to poll PTY child: {error}"))?
        {
            exit_code = Some(status.exit_code() as i32);
            break;
        }

        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            let _ = child.wait();
            break;
        }

        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
    }

    while let Ok(chunk) = rx.try_recv() {
        output.extend_from_slice(&chunk);
    }

    let mut text = String::from_utf8_lossy(&output).into_owned();
    text = truncate_output(&text, max_output_tokens);

    if timed_out {
        return Ok(FunctionToolOutput::error(format!(
            "command timed out after {timeout_ms}ms\n{text}"
        )));
    }

    let is_error = exit_code.unwrap_or(1) != 0;
    let content = if is_error {
        format!("exit code {}\n{}", exit_code.unwrap_or(-1), text)
    } else {
        text.clone()
    };
    if is_error {
        return Ok(FunctionToolOutput::error(content));
    }

    Ok(FunctionToolOutput::success_with_metadata(
        content,
        json!({
            "output": preview(&text),
            "command": command_to_run,
            "exit": exit_code,
            "description": description,
            "cwd": workdir,
            "yield_time_ms": yield_time_ms,
            "tty": true,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolContent;
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn execute_shell_command_non_tty_sends_progress() {
        let cmd = "echo stream_test";
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let result = execute_shell_command(
            ShellExecRequest {
                command: cmd.to_string(),
                workdir: std::env::current_dir().unwrap_or_default(),
                description: "test".into(),
                shell_override: None,
                tty: false,
                login: false,
                timeout_ms: 5000,
                yield_time_ms: 100,
                max_output_tokens: 100,
            },
            Some(tx),
        )
        .await;

        assert!(result.is_ok(), "command should succeed: {:?}", result.err());
        // Progress channel should have received output
        if let Ok(chunk) = rx.try_recv() {
            assert!(!chunk.is_empty(), "progress chunk should not be empty");
        }
    }

    #[tokio::test]
    async fn execute_shell_command_progress_none_does_not_crash() {
        let cmd = "echo test";
        let result = execute_shell_command(
            ShellExecRequest {
                command: cmd.to_string(),
                workdir: std::env::current_dir().unwrap_or_default(),
                description: "test".into(),
                shell_override: None,
                tty: false,
                login: false,
                timeout_ms: 5000,
                yield_time_ms: 100,
                max_output_tokens: 100,
            },
            None,
        )
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn execute_shell_command_success_metadata_is_mixed() {
        let result = execute_shell_command(
            ShellExecRequest {
                command: "echo metadata_test".to_string(),
                workdir: std::env::current_dir().unwrap_or_default(),
                description: "metadata test".into(),
                shell_override: None,
                tty: false,
                login: false,
                timeout_ms: 5000,
                yield_time_ms: 100,
                max_output_tokens: 100,
            },
            None,
        )
        .await
        .expect("execute shell command");

        assert!(!result.is_error);
        match result.content {
            ToolContent::Mixed {
                text: Some(text),
                json: Some(metadata),
            } => {
                assert!(text.contains("metadata_test"));
                assert_eq!(metadata["description"], "metadata test");
            }
            content => panic!("expected mixed success output, got {content:?}"),
        }
    }

    #[tokio::test]
    async fn execute_shell_command_error_output_is_text_only() {
        let result = execute_shell_command(
            ShellExecRequest {
                command: "exit 7".to_string(),
                workdir: std::env::current_dir().unwrap_or_default(),
                description: "error test".into(),
                shell_override: None,
                tty: false,
                login: false,
                timeout_ms: 5000,
                yield_time_ms: 100,
                max_output_tokens: 100,
            },
            None,
        )
        .await
        .expect("execute shell command");

        assert!(result.is_error);
        assert!(matches!(result.content, ToolContent::Text(text) if text.contains("exit code 7")));
    }

    use super::{merge_streams, platform_shell_program, preview, resolve_shell, truncate_output};

    #[test]
    #[cfg(windows)]
    fn resolve_shell_prefers_powershell_alias() {
        let spec = resolve_shell(Some("pwsh"), true);
        assert_eq!(spec.program, "powershell");
        assert_eq!(spec.args, &["-NoLogo", "-NoProfile", "-Command"]);
    }

    #[test]
    #[cfg(windows)]
    fn resolve_shell_prefers_cmd_alias() {
        let spec = resolve_shell(Some("cmd.exe"), true);
        assert_eq!(spec.program, "cmd");
        assert_eq!(spec.args, &["/C"]);
    }

    #[test]
    fn resolve_shell_defaults_to_platform_shell_login() {
        let spec = resolve_shell(None, true);
        assert_eq!(spec.program, platform_shell_program(true));
    }

    #[test]
    fn preview_truncates_long_text() {
        let long = "a".repeat(30_001);
        let result = preview(&long);
        assert!(result.ends_with("\n\n..."));
    }

    #[test]
    fn truncate_output_handles_zero_tokens() {
        assert_eq!(truncate_output("text", 0), "");
    }

    #[test]
    fn truncate_output_limits_length() {
        let input = "a".repeat(200);
        let result = truncate_output(&input, 10);
        assert!(result.ends_with("\n\n... [truncated]"));
        assert!(result.len() < input.len());
    }

    #[test]
    fn merge_streams_combines_stdout_and_stderr() {
        let result = merge_streams("out", "err");
        assert!(result.contains("out"));
        assert!(result.contains("[stderr]"));
        assert!(result.contains("err"));
    }

    #[test]
    fn merge_streams_no_output() {
        assert_eq!(merge_streams("", ""), "(no output)");
    }
}
