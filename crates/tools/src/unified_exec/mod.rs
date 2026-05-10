pub mod buffer;
pub mod process;
pub mod store;
#[cfg(windows)]
pub mod windows_pty;

pub const MAX_PROCESSES: usize = 64;
pub const WARNING_PROCESSES: usize = 60;
pub const DEFAULT_YIELD_MS: u64 = 10_000;
pub const DEFAULT_POLL_YIELD_MS: u64 = 250;
pub const MIN_YIELD_TIME_MS: u64 = 250;
pub const MIN_EMPTY_YIELD_TIME_MS: u64 = 5_000;
pub const MAX_YIELD_TIME_MS: u64 = 30_000;
pub const MAX_WRITE_STDIN_YIELD_MS: u64 = 300_000;
pub const MAX_OUTPUT_TOKENS: usize = 10_000;

pub fn clamp_exec_yield_time(yield_time_ms: u64) -> u64 {
    yield_time_ms.clamp(MIN_YIELD_TIME_MS, MAX_YIELD_TIME_MS)
}

pub fn clamp_write_stdin_yield_time(yield_time_ms: u64, chars: &str) -> u64 {
    let time_ms = yield_time_ms.max(MIN_YIELD_TIME_MS);
    if chars.is_empty() {
        time_ms.clamp(MIN_EMPTY_YIELD_TIME_MS, MAX_WRITE_STDIN_YIELD_MS)
    } else {
        time_ms.min(MAX_YIELD_TIME_MS)
    }
}

pub struct ExecCommandArgs {
    pub cmd: String,
    pub workdir: Option<String>,
    pub shell: Option<String>,
    pub login: bool,
    pub tty: bool,
    pub yield_time_ms: u64,
    pub max_output_tokens: usize,
}

pub struct WriteStdinArgs {
    pub session_id: i32,
    pub chars: String,
    pub yield_time_ms: u64,
    pub max_output_tokens: usize,
}

pub struct ProcessOutput {
    pub output: String,
    pub exit_code: Option<i32>,
    pub wall_time_secs: f64,
    pub truncated: bool,
    pub original_token_count: usize,
}
