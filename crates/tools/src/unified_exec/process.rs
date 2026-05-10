use std::io::Write;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use portable_pty::{CommandBuilder, PtySize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::broadcast;
use tokio::time::{Duration, sleep};

use super::ProcessOutput;
use super::buffer::HeadTailBuffer;

const PTY_READ_BUF: usize = 4096;
const PTY_ROWS: u16 = 24;
const PTY_COLS: u16 = 120;
const PTY_TRAILING_OUTPUT_GRACE_MS: u64 = 150;
const POWERSHELL_UTF8_OUTPUT_PREFIX: &str =
    "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\n";
const UNIFIED_EXEC_ENV: [(&str, &str); 10] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CODEX_CI", "1"),
];

#[derive(Debug, PartialEq, Eq)]
struct ShellSpec {
    program: String,
    args: Vec<String>,
}

fn resolve_shell(shell_override: Option<&str>, login: bool) -> ShellSpec {
    let default_shell = if cfg!(windows) {
        "powershell".to_string()
    } else {
        std::env::var("SHELL")
            .ok()
            .filter(|shell| !shell.is_empty())
            .unwrap_or_else(|| "bash".to_string())
    };
    resolve_shell_with_default(shell_override, login, &default_shell)
}

fn resolve_shell_with_default(
    shell_override: Option<&str>,
    login: bool,
    default_shell: &str,
) -> ShellSpec {
    if let Some(shell) = shell_override {
        return ShellSpec {
            program: shell.to_string(),
            args: shell_args(shell, login),
        };
    }

    ShellSpec {
        program: default_shell.to_string(),
        args: shell_args(default_shell, login),
    }
}

fn shell_args(shell: &str, login: bool) -> Vec<String> {
    let shell_name = shell_name(shell);

    if is_powershell_name(&shell_name) {
        let mut args = Vec::new();
        if !login {
            args.push("-NoProfile".to_string());
        }
        args.push("-Command".to_string());
        return args;
    }

    if shell_name == "cmd" {
        return vec!["/c".to_string()];
    }

    vec![if login { "-lc" } else { "-c" }.to_string()]
}

fn shell_name(shell: &str) -> String {
    Path::new(shell)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(shell)
        .to_ascii_lowercase()
}

fn is_powershell_name(name: &str) -> bool {
    name == "powershell" || name == "pwsh"
}

fn command_for_shell(cmd: &str, shell_spec: &ShellSpec) -> String {
    if !is_powershell_name(&shell_name(&shell_spec.program)) {
        return cmd.to_string();
    }
    let trimmed = cmd.trim_start();
    if trimmed.starts_with(POWERSHELL_UTF8_OUTPUT_PREFIX) {
        cmd.to_string()
    } else {
        format!("{POWERSHELL_UTF8_OUTPUT_PREFIX}{cmd}")
    }
}

/// Max time (in seconds) a process can live without any write_stdin interaction.
const IDLE_TIMEOUT_SECS: u64 = 1800;

fn unified_exec_pty_system() -> Box<dyn portable_pty::PtySystem + Send> {
    #[cfg(windows)]
    {
        Box::new(super::windows_pty::ConPtySystem)
    }

    #[cfg(not(windows))]
    {
        portable_pty::native_pty_system()
    }
}

struct PtyKeepAlive {
    _master: Box<dyn portable_pty::MasterPty + Send>,
    #[cfg(windows)]
    _slave: Box<dyn portable_pty::SlavePty + Send>,
}

pub struct UnifiedExecProcess {
    exit_code: Arc<std::sync::atomic::AtomicI32>,
    terminated_flag: Arc<AtomicBool>,
    stdin_writer: Arc<Mutex<Option<Box<dyn Write + Send>>>>,
    output_tx: broadcast::Sender<Vec<u8>>,
    output_buffer: Arc<AsyncMutex<HeadTailBuffer>>,
    last_stdin_interaction: Arc<Mutex<Instant>>,
    process_id: i32,
    tty: bool,
    _pty_keep_alive: Mutex<Option<PtyKeepAlive>>,
}

impl UnifiedExecProcess {
    pub fn spawn(
        process_id: i32,
        cmd: &str,
        cwd: &Path,
        shell: Option<&str>,
        login: bool,
        tty: bool,
    ) -> Result<(Self, broadcast::Receiver<Vec<u8>>), String> {
        if tty {
            Self::spawn_pty(process_id, cmd, cwd, shell, login)
        } else {
            Self::spawn_piped(process_id, cmd, cwd, shell, login)
        }
    }

    fn spawn_pty(
        process_id: i32,
        cmd: &str,
        cwd: &Path,
        shell: Option<&str>,
        login: bool,
    ) -> Result<(Self, broadcast::Receiver<Vec<u8>>), String> {
        let (output_tx, _output_rx) = broadcast::channel(256);
        let output_buffer = Arc::new(AsyncMutex::new(HeadTailBuffer::new()));
        let terminated_flag = Arc::new(AtomicBool::new(false));
        let terminated_flag_clone = Arc::clone(&terminated_flag);
        let last_stdin_interaction = Arc::new(Mutex::new(Instant::now()));
        let last_stdin_interaction_clone = Arc::clone(&last_stdin_interaction);

        let pty_system = unified_exec_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: PTY_ROWS,
                cols: PTY_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("failed to open PTY: {e}"))?;

        let shell_spec = resolve_shell(shell, login);
        let mut builder = CommandBuilder::new(&shell_spec.program);
        builder.args(&shell_spec.args);
        builder.arg(command_for_shell(cmd, &shell_spec));
        builder.cwd(cwd);
        for (key, value) in UNIFIED_EXEC_ENV {
            builder.env(key, value);
        }
        if cfg!(windows) {
            builder.env("PYTHONUTF8", "1");
        }

        let mut child = pair
            .slave
            .spawn_command(builder)
            .map_err(|e| format!("failed to spawn PTY command: {e}"))?;

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("failed to clone PTY reader: {e}"))?;

        let writer: Box<dyn Write + Send> = pair
            .master
            .take_writer()
            .map_err(|e| format!("failed to take PTY writer: {e}"))?;
        let keep_alive = PtyKeepAlive {
            _master: pair.master,
            #[cfg(windows)]
            _slave: pair.slave,
        };

        let (tokio_tx, mut tokio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

        // Reader thread: blocking PTY read -> tokio::mpsc, with panic protection
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut buf = [0u8; PTY_READ_BUF];
                loop {
                    match std::io::Read::read(&mut reader, &mut buf) {
                        Ok(0) => break,
                        Ok(size) => {
                            if tokio_tx.send(buf[..size].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }));
            if result.is_err() {
                // Reader thread panicked - log and continue (process will be detected as exited)
            }
        });

        let exit_code = Arc::new(std::sync::atomic::AtomicI32::new(-1));
        let exit_code_clone = Arc::clone(&exit_code);
        let output_tx_clone = output_tx.clone();
        let output_buffer_clone = Arc::clone(&output_buffer);

        let idle_timeout = Duration::from_secs(IDLE_TIMEOUT_SECS);
        // Background task: forward tokio::mpsc -> broadcast, handle shutdown/exit/idle timeout
        tokio::spawn(async move {
            let (wait_tx, mut wait_rx) = tokio::sync::oneshot::channel();
            let mut child_killer = child.clone_killer();
            let _wait_thread = std::thread::spawn(move || {
                let code = child.wait().ok().map(|status| status.exit_code() as i32);
                let _ = wait_tx.send(code);
            });

            loop {
                tokio::select! {
                    _ = async {
                        while !terminated_flag_clone.load(Ordering::SeqCst) {
                            let idle_for = last_stdin_interaction_clone
                                .lock()
                                .map(|last| last.elapsed())
                                .unwrap_or(idle_timeout);
                            if idle_for >= idle_timeout {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    } => {
                        break;
                    }
                    Some(bytes) = tokio_rx.recv() => {
                        output_buffer_clone.lock().await.push(&bytes);
                        let _ = output_tx_clone.send(bytes);
                    }
                    result = &mut wait_rx => {
                        let code = result.ok().flatten().unwrap_or(-1);
                        sleep(Duration::from_millis(PTY_TRAILING_OUTPUT_GRACE_MS)).await;
                        while let Ok(bytes) = tokio_rx.try_recv() {
                            output_buffer_clone.lock().await.push(&bytes);
                            let _ = output_tx_clone.send(bytes);
                        }
                        exit_code_clone.store(code, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                    else => break,
                }
            }

            if exit_code_clone.load(std::sync::atomic::Ordering::SeqCst) < 0 {
                let _ = child_killer.kill();
            }
            // Mark as no longer running (both normal exit and forced kill)
            terminated_flag_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let proc_output_rx = output_tx.subscribe();

        Ok((
            UnifiedExecProcess {
                exit_code,
                terminated_flag,
                stdin_writer: Arc::new(Mutex::new(Some(writer))),
                output_tx,
                output_buffer,
                last_stdin_interaction,
                process_id,
                tty: true,
                _pty_keep_alive: Mutex::new(Some(keep_alive)),
            },
            proc_output_rx,
        ))
    }

    fn spawn_piped(
        process_id: i32,
        cmd: &str,
        cwd: &Path,
        shell: Option<&str>,
        login: bool,
    ) -> Result<(Self, broadcast::Receiver<Vec<u8>>), String> {
        let (output_tx, _output_rx) = broadcast::channel(256);
        let output_buffer = Arc::new(AsyncMutex::new(HeadTailBuffer::new()));
        let terminated_flag = Arc::new(AtomicBool::new(false));
        let exit_code = Arc::new(std::sync::atomic::AtomicI32::new(-1));

        let shell_spec = resolve_shell(shell, login);
        let mut command = tokio::process::Command::new(&shell_spec.program);
        command.args(&shell_spec.args);
        command.arg(command_for_shell(cmd, &shell_spec));
        command.current_dir(cwd);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        for (key, value) in UNIFIED_EXEC_ENV {
            command.env(key, value);
        }
        if cfg!(windows) {
            command.env("PYTHONUTF8", "1");
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("failed to spawn command: {e}"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        if let Some(stdout) = stdout {
            spawn_pipe_reader(stdout, output_tx.clone(), Arc::clone(&output_buffer));
        }
        if let Some(stderr) = stderr {
            spawn_pipe_reader(stderr, output_tx.clone(), Arc::clone(&output_buffer));
        }

        let terminated_flag_clone = Arc::clone(&terminated_flag);
        let exit_code_clone = Arc::clone(&exit_code);
        tokio::spawn(async move {
            let code = tokio::select! {
                status = child.wait() => status.ok().and_then(|status| status.code()),
                _ = async {
                    while !terminated_flag_clone.load(Ordering::SeqCst) {
                        sleep(Duration::from_millis(100)).await;
                    }
                } => {
                    let _ = child.kill().await;
                    child.wait().await.ok().and_then(|status| status.code())
                }
            };

            exit_code_clone.store(code.unwrap_or(-1), Ordering::SeqCst);
            terminated_flag_clone.store(true, Ordering::SeqCst);
        });

        let proc_output_rx = output_tx.subscribe();

        Ok((
            UnifiedExecProcess {
                exit_code,
                terminated_flag,
                stdin_writer: Arc::new(Mutex::new(None)),
                output_tx,
                output_buffer,
                last_stdin_interaction: Arc::new(Mutex::new(Instant::now())),
                process_id,
                tty: false,
                _pty_keep_alive: Mutex::new(None),
            },
            proc_output_rx,
        ))
    }

    pub fn write_stdin(&self, chars: &str) -> Result<(), String> {
        let bytes = stdin_bytes_for_pty(chars);
        let mut guard = self
            .stdin_writer
            .lock()
            .map_err(|e| format!("lock error: {e}"))?;
        if let Some(writer) = guard.as_mut() {
            writer
                .write_all(&bytes)
                .map_err(|e| format!("failed to write to stdin: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("failed to flush stdin: {e}"))?;
            *self
                .last_stdin_interaction
                .lock()
                .map_err(|e| format!("lock error: {e}"))? = Instant::now();
            Ok(())
        } else {
            Err("stdin is closed for this session".to_string())
        }
    }

    pub fn terminate(&self) {
        self.terminated_flag.store(true, Ordering::SeqCst);
    }

    pub fn exit_code(&self) -> Option<i32> {
        let code = self.exit_code.load(std::sync::atomic::Ordering::SeqCst);
        if code >= 0 { Some(code) } else { None }
    }

    pub fn is_running(&self) -> bool {
        !self
            .terminated_flag
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn process_id(&self) -> i32 {
        self.process_id
    }

    pub fn tty(&self) -> bool {
        self.tty
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }
}

fn stdin_bytes_for_pty(chars: &str) -> Vec<u8> {
    #[cfg(windows)]
    {
        let mut bytes = Vec::with_capacity(chars.len());
        let mut previous_was_cr = false;
        for byte in chars.bytes() {
            if byte == b'\n' {
                if !previous_was_cr {
                    bytes.push(b'\r');
                }
                bytes.push(b'\n');
                previous_was_cr = false;
                continue;
            }
            bytes.push(byte);
            previous_was_cr = byte == b'\r';
        }
        bytes
    }

    #[cfg(not(windows))]
    {
        chars.as_bytes().to_vec()
    }
}

impl Drop for UnifiedExecProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

pub async fn collect_output(
    output_rx: &mut broadcast::Receiver<Vec<u8>>,
    process: &UnifiedExecProcess,
    yield_time_ms: u64,
    max_output_tokens: usize,
) -> ProcessOutput {
    let started = Instant::now();
    let mut collected = Vec::new();
    let deadline = Duration::from_millis(yield_time_ms);

    loop {
        {
            let mut pending = process.output_buffer.lock().await;
            collected.extend_from_slice(&pending.drain_collect_bytes());
        }

        loop {
            match output_rx.try_recv() {
                Ok(_bytes) => {}
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => {
                    let _ = output_rx.try_recv();
                    break;
                }
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            }
        }

        let done = !process.is_running() || (process.exit_code().is_some() && output_rx.is_empty());

        if done {
            sleep(Duration::from_millis(50)).await;
            {
                let mut pending = process.output_buffer.lock().await;
                collected.extend_from_slice(&pending.drain_collect_bytes());
            }
            while let Ok(bytes) = output_rx.try_recv() {
                let _ = bytes;
            }
            break;
        }

        if started.elapsed() >= deadline {
            break;
        }

        sleep(Duration::from_millis(10)).await;
    }

    let original_token_count = approximate_token_count(collected.len());
    let raw_output = String::from_utf8_lossy(&collected).to_string();
    let (output, truncated) = formatted_truncate_tokens(&raw_output, max_output_tokens);

    ProcessOutput {
        output,
        exit_code: process.exit_code(),
        wall_time_secs: started.elapsed().as_secs_f64(),
        truncated,
        original_token_count,
    }
}

fn approximate_token_count(byte_len: usize) -> usize {
    if byte_len == 0 {
        0
    } else {
        byte_len.div_ceil(4)
    }
}

fn formatted_truncate_tokens(content: &str, max_output_tokens: usize) -> (String, bool) {
    let max_bytes = max_output_tokens.saturating_mul(4);
    if content.len() <= max_bytes {
        return (content.to_string(), false);
    }

    let total_lines = content.lines().count();
    let truncated = truncate_middle_with_token_marker(content, max_bytes);
    (
        format!("Total output lines: {total_lines}\n\n{truncated}"),
        true,
    )
}

fn truncate_middle_with_token_marker(content: &str, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return format!(
            "…{} tokens truncated…",
            approximate_token_count(content.len())
        );
    }

    let head_budget = max_bytes / 2;
    let tail_budget = max_bytes.saturating_sub(head_budget);
    let head_end = floor_char_boundary(content, head_budget);
    let tail_start = ceil_char_boundary(content, content.len().saturating_sub(tail_budget));
    let omitted_bytes = tail_start.saturating_sub(head_end);
    format!(
        "{}…{} tokens truncated…{}",
        &content[..head_end],
        approximate_token_count(omitted_bytes),
        &content[tail_start..]
    )
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn spawn_pipe_reader<R>(
    mut stream: R,
    output_tx: broadcast::Sender<Vec<u8>>,
    output_buffer: Arc<AsyncMutex<HeadTailBuffer>>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0u8; PTY_READ_BUF];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(size) => {
                    let bytes = buf[..size].to_vec();
                    output_buffer.lock().await.push(&bytes);
                    let _ = output_tx.send(bytes);
                }
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::Path;

    #[tokio::test]
    async fn process_spawn_and_exit() {
        let cmd = "echo hello";
        let (proc, mut rx) = UnifiedExecProcess::spawn(
            1,
            cmd,
            Path::new("."),
            /*shell*/ None,
            /*login*/ false,
            /*tty*/ false,
        )
        .expect("spawn should succeed");

        // Wait for process to finish
        let mut waited = 0u64;
        while proc.is_running() && waited < 3000 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            waited += 100;
        }

        let _output = collect_output(&mut rx, &proc, 1000, 1000).await;
        // Process should have exited (echo is a short command)
        // On all platforms, echo finishes quickly
        if !proc.is_running() {
            assert!(
                proc.exit_code().is_some(),
                "process should have an exit code"
            );
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn process_non_tty_captures_output_without_early_subscription() {
        let (proc, mut rx) = UnifiedExecProcess::spawn(
            4,
            "printf buffered-output",
            Path::new("."),
            /*shell*/ None,
            /*login*/ false,
            /*tty*/ false,
        )
        .expect("spawn should succeed");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let output = collect_output(&mut rx, &proc, 250, 1000).await;

        assert_eq!(output.output, "buffered-output");
        assert_eq!(output.exit_code, Some(0));
    }

    #[tokio::test]
    async fn process_non_tty_rejects_stdin_write() {
        let (proc, _rx) = UnifiedExecProcess::spawn(
            5,
            "echo test",
            Path::new("."),
            /*shell*/ None,
            /*login*/ false,
            /*tty*/ false,
        )
        .expect("spawn should succeed");

        assert_eq!(
            proc.write_stdin("input\n"),
            Err("stdin is closed for this session".to_string())
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn process_non_tty_applies_codex_unified_exec_env() {
        let (proc, mut rx) = UnifiedExecProcess::spawn(
            6,
            "printf '%s|%s|%s' \"$NO_COLOR\" \"$TERM\" \"$PAGER\"",
            Path::new("."),
            /*shell*/ None,
            /*login*/ false,
            /*tty*/ false,
        )
        .expect("spawn should succeed");

        let output = collect_output(&mut rx, &proc, 1000, 1000).await;

        assert_eq!(output.output, "1|dumb|cat");
        assert_eq!(output.exit_code, Some(0));
    }

    #[test]
    fn formatted_truncate_tokens_keeps_head_tail_and_line_count() {
        let content = "alpha beta gamma delta epsilon\nzeta eta theta iota kappa";

        let (output, truncated) = formatted_truncate_tokens(content, 5);

        assert!(truncated);
        assert!(output.starts_with("Total output lines: 2\n\nalpha"));
        assert!(output.contains("tokens truncated"));
        assert!(output.ends_with("iota kappa"));
    }

    #[test]
    fn formatted_truncate_tokens_preserves_utf8_boundaries() {
        let content = "😀😀😀😀😀😀😀😀😀😀";

        let (output, truncated) = formatted_truncate_tokens(content, 2);

        assert!(truncated);
        assert!(output.contains("tokens truncated"));
    }

    #[test]
    fn resolve_shell_uses_user_shell_default_and_codex_style_args() {
        assert_eq!(
            resolve_shell_with_default(
                /*shell_override*/ None, /*login*/ true, "/bin/zsh"
            ),
            ShellSpec {
                program: "/bin/zsh".to_string(),
                args: vec!["-lc".to_string()],
            }
        );
        assert_eq!(
            resolve_shell_with_default(
                /*shell_override*/ None, /*login*/ false, "/bin/zsh"
            ),
            ShellSpec {
                program: "/bin/zsh".to_string(),
                args: vec!["-c".to_string()],
            }
        );
    }

    #[test]
    fn resolve_shell_uses_powershell_profile_only_for_login() {
        assert_eq!(
            resolve_shell_with_default(
                /*shell_override*/ Some("pwsh"),
                /*login*/ true,
                "/bin/zsh"
            ),
            ShellSpec {
                program: "pwsh".to_string(),
                args: vec!["-Command".to_string()],
            }
        );
        assert_eq!(
            resolve_shell_with_default(
                /*shell_override*/ Some("pwsh"),
                /*login*/ false,
                "/bin/zsh",
            ),
            ShellSpec {
                program: "pwsh".to_string(),
                args: vec!["-NoProfile".to_string(), "-Command".to_string()],
            }
        );
    }

    #[test]
    fn command_for_shell_prefixes_powershell_utf8_output() {
        let shell_spec = ShellSpec {
            program: "pwsh".to_string(),
            args: vec!["-Command".to_string()],
        };

        assert_eq!(
            command_for_shell("Write-Output hi", &shell_spec),
            format!("{POWERSHELL_UTF8_OUTPUT_PREFIX}Write-Output hi")
        );
        assert_eq!(
            command_for_shell(
                &format!("{POWERSHELL_UTF8_OUTPUT_PREFIX}Write-Output hi"),
                &shell_spec
            ),
            format!("{POWERSHELL_UTF8_OUTPUT_PREFIX}Write-Output hi")
        );
    }

    #[test]
    fn command_for_shell_leaves_posix_shell_unchanged() {
        let shell_spec = ShellSpec {
            program: "/bin/zsh".to_string(),
            args: vec!["-lc".to_string()],
        };

        assert_eq!(command_for_shell("echo hi", &shell_spec), "echo hi");
    }

    #[cfg(windows)]
    #[test]
    fn stdin_bytes_for_windows_pty_uses_carriage_return() {
        assert_eq!(stdin_bytes_for_pty("Alice\n"), b"Alice\r\n");
        assert_eq!(stdin_bytes_for_pty("Alice\r\n"), b"Alice\r\n");
    }

    #[tokio::test]
    async fn process_terminate_works() {
        // Only run on platforms where we have reliable PTY support
        if cfg!(target_os = "linux") {
            let (proc, _rx) = UnifiedExecProcess::spawn(
                2,
                "sleep 60",
                Path::new("."),
                /*shell*/ None,
                /*login*/ false,
                /*tty*/ true,
            )
            .expect("spawn should succeed");
            assert!(proc.is_running());

            proc.terminate();
            // Poll up to 5s for termination (CI can be slow).
            let mut waited = 0u64;
            while proc.is_running() && waited < 5000 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                waited += 100;
            }

            assert!(!proc.is_running(), "process should have been terminated");
        }
    }

    #[tokio::test]
    async fn process_write_stdin_before_exit() {
        // Only run on Unix where cat + PTY stdin works reliably
        if cfg!(target_os = "linux") {
            let (proc, _rx) = UnifiedExecProcess::spawn(
                3,
                "cat",
                Path::new("."),
                /*shell*/ None,
                /*login*/ false,
                /*tty*/ true,
            )
            .expect("spawn should succeed");

            tokio::time::sleep(Duration::from_millis(300)).await;

            *proc
                .last_stdin_interaction
                .lock()
                .expect("last stdin interaction lock should not be poisoned") =
                Instant::now() - Duration::from_secs(60);
            let result = proc.write_stdin("test data\n");
            assert!(result.is_ok(), "write_stdin failed: {:?}", result);
            let idle_for = proc
                .last_stdin_interaction
                .lock()
                .expect("last stdin interaction lock should not be poisoned")
                .elapsed();
            assert!(idle_for < Duration::from_secs(1));
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn process_windows_pty_echo_exits() {
        let (proc, mut rx) = UnifiedExecProcess::spawn(
            4,
            "Write-Output unified-pty-ok",
            Path::new("."),
            /*shell*/ Some("powershell"),
            /*login*/ false,
            /*tty*/ true,
        )
        .expect("spawn should succeed");

        let output = collect_output(&mut rx, &proc, 5_000, 1_000).await;

        assert_eq!(output.exit_code, Some(0));
        assert!(output.output.contains("unified-pty-ok"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn process_windows_pty_read_host_accepts_stdin() {
        let (proc, mut rx) = UnifiedExecProcess::spawn(
            5,
            "Write-Host \"Enter name:\"; $name = Read-Host; Write-Host \"Hello, $name\"",
            Path::new("."),
            /*shell*/ Some("powershell"),
            /*login*/ false,
            /*tty*/ true,
        )
        .expect("spawn should succeed");

        let initial = collect_output(&mut rx, &proc, 2_000, 1_000).await;
        assert!(initial.output.contains("Enter name:"));

        proc.write_stdin("Alice\n")
            .expect("stdin write should work");
        let output = collect_output(&mut rx, &proc, 5_000, 1_000).await;

        assert_eq!(output.exit_code, Some(0));
        assert!(
            output.output.contains("Hello, Alice"),
            "missing greeting in output: {:?}",
            output.output
        );
    }
}
