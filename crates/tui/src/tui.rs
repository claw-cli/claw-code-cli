//! Terminal lifecycle and backend plumbing for the interactive TUI.
//!
//! If you come from web frontend work, this module is closest to the browser event loop and the
//! rendering substrate underneath a UI. It does not decide what the app means; it decides how the
//! app talks to the terminal, when input is observed, and when a redraw is allowed to happen.
//!
//! The responsibilities here are deliberately low level:
//!
//! - enter and restore terminal modes such as raw input, bracketed paste, focus reporting, and
//!   keyboard enhancement flags;
//! - initialize the terminal backend and panic hook so the app can recover cleanly even if the
//!   process exits unexpectedly;
//! - expose the `Tui` wrapper, which owns terminal state, redraw requests, alternate-screen
//!   handling, and temporary restoration for external interactive programs;
//! - host the `event_stream`, `frame_requester`, and `frame_rate_limiter` submodules, which work
//!   together like an input pipeline plus a render scheduler;
//! - keep terminal-specific concerns isolated from `host.rs`, `chatwidget.rs`, and the rest of
//!   the UI so higher-level code can reason in terms of events, frames, and state transitions
//!   instead of escape codes.
//!
//! The `event_stream` module is the input side of the system. It collects crossterm terminal
//! events, turns them into the smaller `TuiEvent` enum, and handles the awkward parts of terminal
//! ownership such as pausing and resuming stdin so can temporarily hand control to another
//! interactive program. In frontend terms, it is closer to a shared event source and input adapter
//! than to a widget.
//!
//! The `frame_requester` module is the redraw side. It gives widgets and background tasks a cheap
//! handle for saying "please render again," similar to scheduling a future animation frame or
//! dispatching a render request from another part of the UI. Requests are funneled through a small
//! scheduler so many rapid requests collapse into one draw instead of causing redundant work.
//!
//! The `frame_rate_limiter` module is the guardrail around that redraw pipeline. It prevents the
//! TUI from emitting draws faster than a human can perceive, which keeps animations responsive
//! without turning every tiny state change into unnecessary terminal work. Think of it as the
//! equivalent of capping an animation loop so repeated invalidations do not starve the rest of the
//! app.
//!
//! Put together, these pieces let the rest of the UI behave as if it has a normal event loop and a
//! normal render scheduler, even though the underlying environment is a terminal with global stdin,
//! alternate screen mode, and much more fragile input semantics than a browser.

use std::fmt;
use std::future::Future;
use std::io::IsTerminal;
use std::io::Result;
use std::io::Stdout;
use std::io::stdin;
use std::io::stdout;
use std::panic;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossterm::Command;
use crossterm::SynchronizedUpdate;
use crossterm::event::DisableBracketedPaste;
use crossterm::event::DisableFocusChange;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableBracketedPaste;
use crossterm::event::EnableFocusChange;
use crossterm::event::EnableMouseCapture;
use crossterm::event::KeyEvent;
use crossterm::event::KeyboardEnhancementFlags;
use crossterm::event::PopKeyboardEnhancementFlags;
use crossterm::event::PushKeyboardEnhancementFlags;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::supports_keyboard_enhancement;
use ratatui::backend::Backend;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::disable_raw_mode;
use ratatui::crossterm::terminal::enable_raw_mode;
use ratatui::layout::Size;
use tokio::sync::broadcast;
use tokio_stream::Stream;

use crate::chatwidget::ExitLayoutMode;
use crate::chatwidget::ExitLayoutSnapshot;
use crate::custom_terminal;
use crate::custom_terminal::Terminal as CustomTerminal;
use crate::history_cell::ScrollbackLine;
use crate::tui::event_stream::EventBroker;
use crate::tui::event_stream::TuiEventStream;
use crate::tui::frame_requester::FrameRequester;
#[cfg(unix)]
use crate::tui::job_control::SuspendContext;
use devo_utils::terminal_detection::Multiplexer;
use devo_utils::terminal_detection::TerminalName;

#[cfg(unix)]
mod job_control;

mod event_stream;
pub(crate) mod frame_rate_limiter;
pub(crate) mod frame_requester;

/// Target frame interval for UI redraw scheduling.
pub(crate) const TARGET_FRAME_INTERVAL: Duration =
    crate::tui::frame_rate_limiter::MIN_FRAME_INTERVAL;

/// A type alias for the terminal type used in this application
pub type Terminal = CustomTerminal<CrosstermBackend<Stdout>>;

fn keyboard_enhancement_supported() -> bool {
    if !supports_keyboard_enhancement().unwrap_or(false) {
        return false;
    }

    let info = devo_utils::terminal_detection::terminal_info();
    if matches!(
        info.multiplexer,
        Some(Multiplexer::Tmux { .. } | Multiplexer::Zellij {})
    ) {
        return false;
    }

    matches!(
        info.name,
        TerminalName::Kitty
            | TerminalName::WezTerm
            | TerminalName::Alacritty
            | TerminalName::Ghostty
    )
}

pub fn set_modes() -> Result<()> {
    execute!(stdout(), EnableBracketedPaste)?;

    enable_raw_mode()?;
    // Enable keyboard enhancement flags so modifiers for keys like Enter are disambiguated.
    // chat_composer.rs is using a keyboard event listener to enter for any modified keys
    // to create a new line that require this.
    // Some terminals (notably legacy Windows consoles) do not support
    // keyboard enhancement flags. Attempt to enable them, but continue
    // gracefully if unsupported.
    if keyboard_enhancement_supported() {
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        );
    }

    let _ = execute!(stdout(), EnableFocusChange);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableAlternateScroll;

impl Command for EnableAlternateScroll {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[?1007h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> Result<()> {
        Err(std::io::Error::other(
            "tried to execute EnableAlternateScroll using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableAlternateScroll;

impl Command for DisableAlternateScroll {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[?1007l")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> Result<()> {
        Err(std::io::Error::other(
            "tried to execute DisableAlternateScroll using WinAPI; use ANSI instead",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

fn restore_common(should_disable_raw_mode: bool) -> Result<()> {
    // Pop may fail on platforms that didn't support the push; ignore errors.
    if keyboard_enhancement_supported() {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    execute!(stdout(), DisableBracketedPaste)?;
    let _ = execute!(stdout(), DisableFocusChange);
    let _ = execute!(stdout(), DisableAlternateScroll);
    let _ = execute!(stdout(), LeaveAlternateScreen);
    if should_disable_raw_mode {
        disable_raw_mode()?;
    }
    let _ = execute!(stdout(), crossterm::cursor::Show);
    let _ = std::io::Write::flush(&mut stdout());
    Ok(())
}

/// Restore the terminal to its original state.
/// Inverse of `set_modes`.
pub fn restore() -> Result<()> {
    let should_disable_raw_mode = true;
    restore_common(should_disable_raw_mode)
}

/// Restore the terminal to its original state, but keep raw mode enabled.
pub fn restore_keep_raw() -> Result<()> {
    let should_disable_raw_mode = false;
    restore_common(should_disable_raw_mode)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreMode {
    #[allow(dead_code)]
    Full, // Fully restore the terminal (disables raw mode).
    KeepRaw, // Restore the terminal but keep raw mode enabled.
}

impl RestoreMode {
    fn restore(self) -> Result<()> {
        match self {
            RestoreMode::Full => restore(),
            RestoreMode::KeepRaw => restore_keep_raw(),
        }
    }
}

/// Flush the underlying stdin buffer to clear any input that may be buffered at the terminal level.
/// For example, clears any user input that occurred while the crossterm EventStream was dropped.
#[cfg(unix)]
fn flush_terminal_input_buffer() {
    // Safety: flushing the stdin queue is safe and does not move ownership.
    let result = unsafe { libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH) };
    if result != 0 {
        let err = std::io::Error::last_os_error();
        tracing::warn!("failed to tcflush stdin: {err}");
    }
}

/// Flush the underlying stdin buffer to clear any input that may be buffered at the terminal level.
/// For example, clears any user input that occurred while the crossterm EventStream was dropped.
#[cfg(windows)]
fn flush_terminal_input_buffer() {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::FlushConsoleInputBuffer;
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Console::STD_INPUT_HANDLE;

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle == 0 {
        let err = unsafe { GetLastError() };
        tracing::warn!("failed to get stdin handle for flush: error {err}");
        return;
    }

    let result = unsafe { FlushConsoleInputBuffer(handle) };
    if result == 0 {
        let err = unsafe { GetLastError() };
        tracing::warn!("failed to flush stdin buffer: error {err}");
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn flush_terminal_input_buffer() {}

/// Initialize the terminal (inline viewport; history stays in normal scrollback)
pub fn init() -> Result<Terminal> {
    if !stdin().is_terminal() {
        return Err(std::io::Error::other("stdin is not a terminal"));
    }
    if !stdout().is_terminal() {
        return Err(std::io::Error::other("stdout is not a terminal"));
    }
    set_modes()?;

    flush_terminal_input_buffer();

    set_panic_hook();

    let backend = CrosstermBackend::new(stdout());
    let tui = CustomTerminal::with_options(backend)?;
    Ok(tui)
}

fn set_panic_hook() {
    let hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = restore(); // ignore any errors as we are already failing
        hook(panic_info);
    }));
}

#[derive(Clone, Debug)]
pub enum TuiEvent {
    Key(KeyEvent),
    Paste(String),
    Mouse(crossterm::event::MouseEvent),
    Draw,
}

pub struct Tui {
    frame_requester: FrameRequester,
    draw_tx: broadcast::Sender<()>,
    event_broker: Arc<EventBroker>,
    pub(crate) terminal: Terminal,
    pending_history_lines: Vec<ScrollbackLine>,
    alt_saved_viewport: Option<ratatui::layout::Rect>,
    #[cfg(unix)]
    suspend_context: SuspendContext,
    // True when overlay alt-screen UI is active
    alt_screen_active: Arc<AtomicBool>,
    // True when terminal/tab is focused; updated internally from crossterm events
    terminal_focused: Arc<AtomicBool>,
    // True when the next draw should repaint the full viewport instead of diffing
    // against the previously rendered frame contents.
    needs_full_repaint: Arc<AtomicBool>,
    enhanced_keys_supported: bool,
    is_zellij: bool,
    // When false, enter_alt_screen() becomes a no-op (for Zellij scrollback support)
    alt_screen_enabled: bool,
    last_exit_layout_snapshot: Arc<Mutex<ExitLayoutSnapshot>>,
}

impl Tui {
    pub fn new(terminal: Terminal) -> Self {
        let (draw_tx, _) = broadcast::channel(1);
        let frame_requester = FrameRequester::new(draw_tx.clone());

        // Detect keyboard enhancement support before any EventStream is created so the
        // crossterm poller can acquire its lock without contention.
        let enhanced_keys_supported = supports_keyboard_enhancement().unwrap_or(false);
        // Cache this to avoid contention with the event reader.
        supports_color::on_cached(supports_color::Stream::Stdout);
        let _ = crate::terminal_palette::default_colors();
        let is_zellij = matches!(
            devo_utils::terminal_detection::terminal_info().multiplexer,
            Some(devo_utils::terminal_detection::Multiplexer::Zellij {})
        );

        Self {
            frame_requester,
            draw_tx,
            event_broker: Arc::new(EventBroker::new()),
            terminal,
            pending_history_lines: vec![],
            alt_saved_viewport: None,
            #[cfg(unix)]
            suspend_context: SuspendContext::new(),
            alt_screen_active: Arc::new(AtomicBool::new(false)),
            terminal_focused: Arc::new(AtomicBool::new(true)),
            needs_full_repaint: Arc::new(AtomicBool::new(false)),
            enhanced_keys_supported,
            is_zellij,
            alt_screen_enabled: true,
            last_exit_layout_snapshot: Arc::new(Mutex::new(ExitLayoutSnapshot::default())),
        }
    }

    /// Set whether alternate screen is enabled. When false, enter_alt_screen() becomes a no-op.
    pub fn set_alt_screen_enabled(&mut self, enabled: bool) {
        self.alt_screen_enabled = enabled;
    }

    pub fn frame_requester(&self) -> FrameRequester {
        self.frame_requester.clone()
    }

    pub fn enhanced_keys_supported(&self) -> bool {
        self.enhanced_keys_supported
    }

    pub fn is_alt_screen_active(&self) -> bool {
        self.alt_screen_active.load(Ordering::Relaxed)
    }

    pub fn is_terminal_focused(&self) -> bool {
        self.terminal_focused.load(Ordering::Relaxed)
    }

    pub(crate) fn set_exit_layout_snapshot_handle(
        &mut self,
        snapshot: Arc<Mutex<ExitLayoutSnapshot>>,
    ) {
        self.last_exit_layout_snapshot = snapshot;
    }

    // Drop crossterm EventStream to avoid stdin conflicts with other processes.
    pub fn pause_events(&mut self) {
        self.event_broker.pause_events();
    }

    // Resume crossterm EventStream to resume stdin polling.
    // Inverse of `pause_events`.
    pub fn resume_events(&mut self) {
        self.event_broker.resume_events();
    }

    /// Temporarily restore terminal state to run an external interactive program `f`.
    ///
    /// This pauses crossterm's stdin polling by dropping the underlying event stream, restores
    /// terminal modes (optionally keeping raw mode enabled), then re-applies devo TUI modes and
    /// flushes pending stdin input before resuming events.
    pub async fn with_restored<R, F, Fut>(&mut self, mode: RestoreMode, f: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = R>,
    {
        // Pause crossterm events to avoid stdin conflicts with external program `f`.
        self.pause_events();

        // Leave alt screen if active to avoid conflicts with external program `f`.
        let was_alt_screen = self.is_alt_screen_active();
        if was_alt_screen {
            let _ = self.leave_alt_screen();
        }

        if let Err(err) = mode.restore() {
            tracing::warn!("failed to restore terminal modes before external program: {err}");
        }

        let output = f().await;

        if let Err(err) = set_modes() {
            tracing::warn!("failed to re-enable terminal modes after external program: {err}");
        }
        // After the external program `f` finishes, reset terminal state and flush any buffered keypresses.
        flush_terminal_input_buffer();

        if was_alt_screen {
            let _ = self.enter_alt_screen();
        }

        self.resume_events();
        output
    }

    pub fn event_stream(&self) -> Pin<Box<dyn Stream<Item = TuiEvent> + Send + 'static>> {
        #[cfg(unix)]
        let stream = TuiEventStream::new(
            self.event_broker.clone(),
            self.draw_tx.subscribe(),
            self.terminal_focused.clone(),
            self.needs_full_repaint.clone(),
            self.suspend_context.clone(),
            self.alt_screen_active.clone(),
        );
        #[cfg(not(unix))]
        let stream = TuiEventStream::new(
            self.event_broker.clone(),
            self.draw_tx.subscribe(),
            self.terminal_focused.clone(),
            self.needs_full_repaint.clone(),
        );
        Box::pin(stream)
    }

    /// Enter alternate screen and expand the viewport to full terminal size, saving the current
    /// inline viewport for restoration when leaving.
    pub fn enter_alt_screen(&mut self) -> Result<()> {
        if !self.alt_screen_enabled {
            return Ok(());
        }
        let _ = execute!(self.terminal.backend_mut(), EnterAlternateScreen);
        // Enable "alternate scroll" so terminals may translate wheel to arrows
        let _ = execute!(self.terminal.backend_mut(), EnableAlternateScroll);
        let _ = execute!(self.terminal.backend_mut(), EnableMouseCapture);
        if let Ok(size) = self.terminal.size() {
            self.alt_saved_viewport = Some(self.terminal.viewport_area);
            self.terminal.set_viewport_area(ratatui::layout::Rect::new(
                0,
                0,
                size.width,
                size.height,
            ));
            let _ = self.terminal.clear();
            self.terminal.invalidate_viewport();
        }
        self.alt_screen_active.store(true, Ordering::Relaxed);
        self.needs_full_repaint.store(true, Ordering::Relaxed);
        self.frame_requester.schedule_frame();
        Ok(())
    }

    /// Leave alternate screen and restore the previously saved inline viewport, if any.
    pub fn leave_alt_screen(&mut self) -> Result<()> {
        if !self.alt_screen_enabled {
            return Ok(());
        }
        // Disable alternate scroll when leaving alt-screen
        let _ = execute!(self.terminal.backend_mut(), DisableMouseCapture);
        let _ = execute!(self.terminal.backend_mut(), DisableAlternateScroll);
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        if let Some(saved) = self.alt_saved_viewport.take() {
            self.terminal.set_viewport_area(saved);
            self.terminal.invalidate_viewport();
        }
        self.alt_screen_active.store(false, Ordering::Relaxed);
        self.needs_full_repaint.store(true, Ordering::Relaxed);
        self.frame_requester.schedule_frame();
        Ok(())
    }

    pub fn insert_history_lines(&mut self, lines: Vec<ScrollbackLine>) {
        self.pending_history_lines.extend(lines);
        self.frame_requester().schedule_frame();
    }

    pub fn clear_pending_history_lines(&mut self) {
        self.pending_history_lines.clear();
    }

    pub fn replace_inline_session_ui(&mut self) -> Result<()> {
        tracing::trace!(
            session_origin_top = self.terminal.session_origin_top(),
            viewport = ?self.terminal.viewport_area,
            visible_history_rows = self.terminal.visible_history_rows(),
            pending_history_lines = self.pending_history_lines.len(),
            "resetting inline session UI before switch"
        );
        Self::reset_inline_session_ui(&mut self.terminal, &mut self.pending_history_lines)?;
        tracing::trace!(
            session_origin_top = self.terminal.session_origin_top(),
            viewport = ?self.terminal.viewport_area,
            visible_history_rows = self.terminal.visible_history_rows(),
            pending_history_lines = self.pending_history_lines.len(),
            "inline session UI reset complete"
        );
        Ok(())
    }

    fn reset_inline_session_ui<B>(
        terminal: &mut CustomTerminal<B>,
        pending_history_lines: &mut Vec<ScrollbackLine>,
    ) -> Result<()>
    where
        B: Backend + std::io::Write,
    {
        pending_history_lines.clear();
        terminal.clear_visible_screen()?;
        Ok(())
    }

    pub fn flush_pending_history_lines_for_exit(&mut self) -> Result<()> {
        let _ = Self::flush_pending_history_lines(
            &mut self.terminal,
            &mut self.pending_history_lines,
            self.is_zellij,
        )?;
        Ok(())
    }

    pub fn shutdown_inline_precise(&mut self) -> Result<()> {
        if self.is_alt_screen_active() {
            self.leave_alt_screen()?;
        }
        self.flush_pending_history_lines_for_exit()?;

        let snapshot = self
            .last_exit_layout_snapshot
            .lock()
            .map(|guard| *guard)
            .unwrap_or_default();
        Self::apply_exit_layout_snapshot(&mut self.terminal, snapshot)
    }

    /// Resize the inline viewport to `height` rows, scrolling content above it if
    /// the viewport would extend past the bottom of the screen. Returns `true` when
    /// the caller must invalidate the diff buffer (Zellij mode), because the scroll
    /// was performed with raw newlines that ratatui cannot track.
    fn update_inline_viewport(
        terminal: &mut Terminal,
        height: u16,
        is_zellij: bool,
    ) -> Result<bool> {
        let size = terminal.size()?;
        let mut needs_full_repaint = false;

        let mut area = terminal.viewport_area;
        area.height = height.min(size.height);
        area.width = size.width;
        if area.bottom() > size.height {
            let scroll_by = area.bottom() - size.height;
            Self::append_expanded_viewport(terminal, size, scroll_by, is_zellij)?;
            needs_full_repaint = true;
            area.y = size.height - area.height;
        }
        if area != terminal.viewport_area {
            // TODO(nornagon): probably this could be collapsed with the clear + set_viewport_area above.
            terminal.clear()?;
            terminal.set_viewport_area(area);
        }

        Ok(needs_full_repaint)
    }

    /// Grow the live inline viewport by appending rows at the bottom of the terminal instead of
    /// scrolling only the region above the viewport.
    ///
    /// This matches the append-only behavior used by Codex's inline TUI: when the live area needs
    /// more height, we advance the terminal buffer downward so users who are currently viewing
    /// scrollback do not see previously rendered rows get rewritten in place.
    fn append_expanded_viewport(
        terminal: &mut Terminal,
        size: Size,
        scroll_by: u16,
        is_zellij: bool,
    ) -> Result<()> {
        if is_zellij {
            return Self::scroll_zellij_expanded_viewport(terminal, size, scroll_by);
        }

        terminal
            .backend_mut()
            .set_cursor_position(ratatui::layout::Position {
                x: 0,
                y: size.height.saturating_sub(1),
            })?;
        terminal.backend_mut().append_lines(scroll_by)
    }

    /// Push content above the viewport upward by `scroll_by` rows using raw
    /// newlines at the screen bottom. This is the Zellij-safe alternative to
    /// backend `append_lines`, which Zellij does not expose in a way ratatui can rely on.
    fn scroll_zellij_expanded_viewport(
        terminal: &mut Terminal,
        size: Size,
        scroll_by: u16,
    ) -> Result<()> {
        crossterm::queue!(
            terminal.backend_mut(),
            crossterm::cursor::MoveTo(0, size.height.saturating_sub(1))
        )?;
        for _ in 0..scroll_by {
            crossterm::queue!(terminal.backend_mut(), crossterm::style::Print("\n"))?;
        }
        Ok(())
    }

    /// Write any buffered history lines above the viewport and clear the buffer.
    /// Returns `true` when Zellij mode was used, signaling that the caller must
    /// invalidate the diff buffer for a full repaint.
    fn flush_pending_history_lines(
        terminal: &mut Terminal,
        pending_history_lines: &mut Vec<ScrollbackLine>,
        is_zellij: bool,
    ) -> Result<bool> {
        if pending_history_lines.is_empty() {
            return Ok(false);
        }

        crate::insert_history::insert_history_lines_with_mode(
            terminal,
            pending_history_lines.clone(),
            crate::insert_history::InsertHistoryMode::new(is_zellij),
        )?;
        pending_history_lines.clear();
        Ok(is_zellij)
    }

    fn apply_exit_layout_snapshot<B>(
        terminal: &mut CustomTerminal<B>,
        snapshot: ExitLayoutSnapshot,
    ) -> Result<()>
    where
        B: Backend + std::io::Write,
    {
        if snapshot.mode == ExitLayoutMode::InlineChat && !snapshot.bottom_pane_area.is_empty() {
            let current_viewport_top = terminal.viewport_area.top();
            let snapshot_viewport_top = snapshot.frame_area.top();
            let delta_y = i32::from(current_viewport_top) - i32::from(snapshot_viewport_top);
            // Offset the snapshot coordinates to account for viewport drift since
            // the last render (e.g. from flush_pending_history_lines_for_exit).
            let offset = ratatui::layout::Offset { x: 0, y: delta_y };
            let bottom_pane_area = snapshot.bottom_pane_area.offset(offset);
            // Also clear the history area so that session header and live text
            // do not remain on screen after exit.
            let history_area = snapshot.history_area.offset(offset);
            let clear_area = ratatui::layout::Rect {
                x: 0,
                y: history_area.top(),
                width: terminal.size()?.width,
                height: bottom_pane_area.bottom().saturating_sub(history_area.top()),
            };
            terminal.clear_screen_area(clear_area)?;
            terminal.set_cursor_below_rect(bottom_pane_area)?;
            return Ok(());
        }

        terminal.clear_inline_viewport()?;
        Ok(())
    }

    pub fn draw(
        &mut self,
        height: u16,
        draw_fn: impl FnOnce(&mut custom_terminal::Frame),
    ) -> Result<()> {
        // If we are resuming from ^Z, we need to prepare the resume action now so we can apply it
        // in the synchronized update.
        #[cfg(unix)]
        let mut prepared_resume = self
            .suspend_context
            .prepare_resume_action(&mut self.terminal, &mut self.alt_saved_viewport);

        // Precompute any viewport updates that need a cursor-position query before entering
        // the synchronized update, to avoid racing with the event reader.
        let mut pending_viewport_area = None;

        stdout().sync_update(|_| {
            #[cfg(unix)]
            if let Some(prepared) = prepared_resume.take() {
                prepared.apply(&mut self.terminal)?;
            }

            let terminal = &mut self.terminal;
            if let Some(new_area) = pending_viewport_area.take() {
                let previous_area = terminal.viewport_area;
                if previous_area != new_area {
                    terminal.clear_screen_area(previous_area)?;
                }
                terminal.set_viewport_area(new_area);
                terminal.clear()?;
                terminal.invalidate_viewport();
            }

            if self.needs_full_repaint.swap(false, Ordering::Relaxed) {
                terminal.invalidate_viewport();
            }

            let mut needs_full_repaint =
                Self::update_inline_viewport(terminal, height, self.is_zellij)?;
            needs_full_repaint |= Self::flush_pending_history_lines(
                terminal,
                &mut self.pending_history_lines,
                self.is_zellij,
            )?;

            if needs_full_repaint {
                terminal.invalidate_viewport();
            }

            // Update the y position for suspending so Ctrl-Z can place the cursor correctly.
            #[cfg(unix)]
            {
                let area = terminal.viewport_area;
                let inline_area_bottom = if self.alt_screen_active.load(Ordering::Relaxed) {
                    self.alt_saved_viewport
                        .map(|r| r.bottom().saturating_sub(1))
                        .unwrap_or_else(|| area.bottom().saturating_sub(1))
                } else {
                    area.bottom().saturating_sub(1)
                };
                self.suspend_context.set_cursor_y(inline_area_bottom);
            }

            terminal.draw(|frame| {
                draw_fn(frame);
            })
        })?
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.leave_alt_screen();
        let _ = restore();
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use ratatui::layout::Position;
    use ratatui::layout::Rect;
    use ratatui::text::Line;

    use super::Tui;
    use crate::chatwidget::ExitLayoutMode;
    use crate::chatwidget::ExitLayoutSnapshot;
    use crate::custom_terminal::Terminal as CustomTerminal;
    use crate::history_cell::ScrollbackLine;
    use crate::insert_history::insert_history_lines;
    use crate::test_backend::VT100Backend;

    #[test]
    fn reset_inline_session_ui_clears_pending_history_and_visible_transcript() {
        let width: u16 = 24;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 2, width, 2));

        insert_history_lines(&mut terminal, vec![Line::from("session 1").into()])
            .expect("insert history");
        let mut pending_history_lines = vec![ScrollbackLine::from(Line::from("queued line"))];

        Tui::reset_inline_session_ui(&mut terminal, &mut pending_history_lines)
            .expect("reset inline session ui");

        let rows_after: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();
        assert!(pending_history_lines.is_empty());
        assert_eq!(0, terminal.viewport_area.y);
        assert_eq!(0, terminal.visible_history_rows());
        assert_eq!(0, terminal.session_origin_top());
        assert!(
            rows_after.iter().all(|row| !row.contains("session 1")),
            "expected old session transcript to be cleared, rows: {rows_after:?}"
        );
    }

    #[test]
    fn shutdown_inline_precise_clears_viewport_and_repositions_cursor() {
        let width: u16 = 24;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 3, width, 3));

        // Write scrollback ABOVE viewport (y < 3)
        terminal
            .set_cursor_position(Position { x: 0, y: 2 })
            .expect("cursor position");
        std::io::Write::write_all(terminal.backend_mut(), b"history row").expect("write history");
        // Write live content WITHIN viewport (y >= 3)
        terminal
            .set_cursor_position(Position { x: 0, y: 3 })
            .expect("cursor position");
        std::io::Write::write_all(terminal.backend_mut(), b"top live").expect("write top");
        terminal
            .set_cursor_position(Position { x: 0, y: 4 })
            .expect("cursor position");
        std::io::Write::write_all(terminal.backend_mut(), b"bottom pane").expect("write bottom");

        Tui::apply_exit_layout_snapshot(
            &mut terminal,
            ExitLayoutSnapshot {
                mode: ExitLayoutMode::InlineChat,
                frame_area: Rect::new(0, 3, width, 3),
                history_area: Rect::new(0, 3, width, 1),
                bottom_pane_area: Rect::new(0, 4, width, 2),
            },
        )
        .expect("apply exit snapshot");

        let rows_after: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();
        let history_row = rows_after
            .iter()
            .position(|row| row.contains("history row"))
            .expect("history row remains visible");
        assert!(
            history_row < 3,
            "history should remain above viewport: {rows_after:?}"
        );
        // All viewport rows cleared
        assert_eq!("", rows_after[3].trim_end());
        assert_eq!("", rows_after[4].trim_end());
        assert_eq!("", rows_after[5].trim_end());
        assert_eq!(Position { x: 0, y: 6 }, terminal.last_known_cursor_pos);
    }

    #[test]
    fn shutdown_inline_precise_reanchors_bottom_pane_to_current_viewport() {
        let width: u16 = 24;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 5, width, 3));

        terminal
            .set_cursor_position(Position { x: 0, y: 5 })
            .expect("cursor position");
        std::io::Write::write_all(terminal.backend_mut(), b"live history").expect("write history");
        terminal
            .set_cursor_position(Position { x: 0, y: 6 })
            .expect("cursor position");
        std::io::Write::write_all(terminal.backend_mut(), b"current bottom").expect("write bottom");

        // Snapshot is stale: frame_area.y=3, but viewport is now at y=5
        // delta_y = 5 - 3 = 2
        // Area should be shifted down by 2 rows
        Tui::apply_exit_layout_snapshot(
            &mut terminal,
            ExitLayoutSnapshot {
                mode: ExitLayoutMode::InlineChat,
                frame_area: Rect::new(0, 3, width, 3),
                history_area: Rect::new(0, 3, width, 1),
                bottom_pane_area: Rect::new(0, 4, width, 2),
            },
        )
        .expect("apply exit snapshot");

        let rows_after: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();
        // Scrollback above viewport y=5 is preserved
        assert!(
            rows_after[4].is_empty() || !rows_after[4].contains("live"),
            "row above viewport untouched: {rows_after:?}"
        );
        // Viewport content shifted + cleared
        assert_eq!("", rows_after[6].trim_end());
        assert_eq!("", rows_after[7].trim_end());
        assert_eq!(Position { x: 0, y: 8 }, terminal.last_known_cursor_pos);
    }

    #[test]
    fn shutdown_inline_precise_falls_back_for_special_surfaces() {
        let width: u16 = 24;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 3, width, 2));

        terminal
            .set_cursor_position(Position { x: 0, y: 2 })
            .expect("cursor position");
        std::io::Write::write_all(terminal.backend_mut(), b"history row").expect("write history");
        terminal
            .set_cursor_position(Position { x: 0, y: 3 })
            .expect("cursor position");
        std::io::Write::write_all(terminal.backend_mut(), b"live row").expect("write live");

        Tui::apply_exit_layout_snapshot(
            &mut terminal,
            ExitLayoutSnapshot {
                mode: ExitLayoutMode::SpecialSurface,
                frame_area: Rect::new(0, 0, width, height),
                history_area: Rect::default(),
                bottom_pane_area: Rect::default(),
            },
        )
        .expect("apply exit snapshot");

        let rows_after: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();
        let history_row = rows_after
            .iter()
            .position(|row| row.contains("history row"))
            .expect("history row remains visible");
        assert!(
            history_row < 3,
            "history should remain above viewport: {rows_after:?}"
        );
        assert_eq!("", rows_after[3].trim_end());
    }

    #[test]
    fn shutdown_full_flow_preserves_scrollback_and_positions_cursor() {
        let width: u16 = 24;
        let height: u16 = 12;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 4, width, 4));

        // Write scrollback content above the viewport
        terminal
            .set_cursor_position(Position { x: 0, y: 2 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"scrollback row 1").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 3 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"scrollback row 2").expect("write");

        // Write live content within the viewport
        terminal
            .set_cursor_position(Position { x: 0, y: 4 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"live history row").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 5 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"live assistant row").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 6 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"bottom pane row 1").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 7 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"bottom pane row 2").expect("write");

        // Simulate pending history lines pushed before exit.
        // flush_pending_history_lines_for_exit() inserts these above the viewport,
        // shifting viewport.y downward.
        let mut pending_lines = vec![ScrollbackLine::from(Line::from("pending history line"))];
        insert_history_lines(&mut terminal, pending_lines.clone()).expect("insert");
        pending_lines.clear();
        // viewport.y is now 5 (shifted down by 1)

        // Apply the exit snapshot captured at the LAST render (viewport was at y=4).
        Tui::apply_exit_layout_snapshot(
            &mut terminal,
            ExitLayoutSnapshot {
                mode: ExitLayoutMode::InlineChat,
                frame_area: Rect::new(0, 4, width, 4),
                history_area: Rect::new(0, 4, width, 2),
                bottom_pane_area: Rect::new(0, 6, width, 2),
            },
        )
        .expect("apply");

        let rows: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();

        // Scrollback rows above viewport must be preserved
        let scrollback_idx = rows
            .iter()
            .position(|r| r.contains("scrollback row 1"))
            .expect("scrollback should be preserved");
        assert!(scrollback_idx < 4, "scrollback above viewport: {rows:?}");

        // The pending history line pushed at exit should also be in scrollback
        let pending_idx = rows
            .iter()
            .position(|r| r.contains("pending history line"))
            .expect("pending history line should be in scrollback");
        assert!(pending_idx < 6, "pending line in scrollback: {rows:?}");

        // All viewport rows (from history_area to bottom of bottom_pane) cleared
        assert_eq!("", rows[6].trim_end(), "viewport row cleared: {rows:?}");
        assert_eq!("", rows[7].trim_end(), "viewport row cleared: {rows:?}");
        assert_eq!("", rows[8].trim_end(), "viewport row cleared: {rows:?}");

        // Cursor placed right below the cleared bottom pane
        assert_eq!(
            Position { x: 0, y: 9 },
            terminal.last_known_cursor_pos,
            "cursor below bottom pane: {rows:?}"
        );
    }

    #[test]
    fn shutdown_clears_history_area_content() {
        let width: u16 = 24;
        let height: u16 = 8;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 2, width, 5));

        // Write content in all areas
        terminal
            .set_cursor_position(Position { x: 0, y: 2 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"session header row").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 3 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"live text row").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 4 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"composer row 1").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 5 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"composer row 2").expect("write");

        // Snapshot captures the full viewport at y=2
        Tui::apply_exit_layout_snapshot(
            &mut terminal,
            ExitLayoutSnapshot {
                mode: ExitLayoutMode::InlineChat,
                frame_area: Rect::new(0, 2, width, 5),
                history_area: Rect::new(0, 2, width, 2),
                bottom_pane_area: Rect::new(0, 4, width, 2),
            },
        )
        .expect("apply");

        let rows: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();

        // History area rows (y=2,3) should be cleared — session header should NOT remain
        let header_row = rows.iter().position(|r| r.contains("session header row"));
        assert!(
            header_row.is_none(),
            "session header should be cleared, but found at row {:?}: {rows:?}",
            header_row
        );

        // Live text row should also be cleared
        let live_row = rows.iter().position(|r| r.contains("live text row"));
        assert!(
            live_row.is_none(),
            "live text should be cleared, but found at row {:?}: {rows:?}",
            live_row
        );

        // Bottom pane cleared
        assert_eq!("", rows[4].trim_end());
        assert_eq!("", rows[5].trim_end());

        // Cursor below bottom pane
        assert_eq!(Position { x: 0, y: 6 }, terminal.last_known_cursor_pos);
    }

    #[test]
    fn shutdown_with_no_pending_lines_positions_cursor_correctly() {
        let width: u16 = 24;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 3, width, 4));

        // Write composer content
        terminal
            .set_cursor_position(Position { x: 0, y: 3 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"status line").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 4 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"composer").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 5 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"footer").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 6 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"spacer").expect("write");

        // Snapshot — no pending history lines, delta_y = 0
        Tui::apply_exit_layout_snapshot(
            &mut terminal,
            ExitLayoutSnapshot {
                mode: ExitLayoutMode::InlineChat,
                frame_area: Rect::new(0, 3, width, 4),
                history_area: Rect::new(0, 3, width, 1),
                bottom_pane_area: Rect::new(0, 4, width, 3),
            },
        )
        .expect("apply");

        let rows: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();

        // All viewport rows cleared
        assert_eq!("", rows[3].trim_end());
        assert_eq!("", rows[4].trim_end());
        assert_eq!("", rows[5].trim_end());
        assert_eq!("", rows[6].trim_end());

        // Cursor right below the cleared area
        assert_eq!(Position { x: 0, y: 7 }, terminal.last_known_cursor_pos);
    }

    #[test]
    fn shutdown_with_onboarding_view_clears_correct_area() {
        let width: u16 = 24;
        let height: u16 = 10;
        let backend = VT100Backend::new(width, height);
        let mut terminal = CustomTerminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 2, width, 7));

        // Write onboarding content (12-row onboarding view takes most of viewport)
        terminal
            .set_cursor_position(Position { x: 0, y: 2 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"  Welcome to Devo").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 3 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"  Choose a model").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 4 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"  > model-name").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 5 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"  model desc").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 6 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"  > other-model").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 7 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"  other desc").expect("write");
        terminal
            .set_cursor_position(Position { x: 0, y: 8 })
            .expect("cursor");
        std::io::Write::write_all(terminal.backend_mut(), b"  > Custom Model").expect("write");

        // Snapshot — large bottom_pane_area like onboarding view
        Tui::apply_exit_layout_snapshot(
            &mut terminal,
            ExitLayoutSnapshot {
                mode: ExitLayoutMode::InlineChat,
                frame_area: Rect::new(0, 2, width, 7),
                history_area: Rect::new(0, 2, width, 1),
                bottom_pane_area: Rect::new(0, 3, width, 6),
            },
        )
        .expect("apply");

        let rows: Vec<String> = terminal.backend().vt100().screen().rows(0, width).collect();

        // All onboarding content cleared
        for y in 2..9 {
            assert!(
                rows[y].trim().is_empty() || rows[y].trim().starts_with("│"),
                "row {y} should be cleared: {rows:?}"
            );
        }

        // Cursor below the cleared bottom pane
        assert_eq!(Position { x: 0, y: 9 }, terminal.last_known_cursor_pos);
    }
}
