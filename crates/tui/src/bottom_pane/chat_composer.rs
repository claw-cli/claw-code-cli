//! The chat composer is the bottom-pane text input state machine.
//!
//! It is responsible for:
//!
//! - Editing the input buffer (a [`TextArea`]), including placeholder "elements" for attachments.
//! - Routing keys to the active popup (slash commands, file search, skill/apps mentions).
//! - Promoting typed slash commands into atomic elements when the command name is completed.
//! - Handling submit vs newline on Enter.
//! - Turning raw key streams into explicit paste operations on platforms where terminals
//!   don't provide reliable bracketed paste (notably Windows).
//!
//! # Key Event Routing
//!
//! Most key handling goes through [`ChatComposer::handle_key_event`], which dispatches to a
//! popup-specific handler if a popup is visible and otherwise to
//! [`ChatComposer::handle_key_event_without_popup`]. After every handled key, we call
//! [`ChatComposer::sync_popups`] so UI state follows the latest buffer/cursor.
//!
//! # History Navigation (↑/↓)
//!
//! The Up/Down history path is managed by [`ChatComposerHistory`]. It merges:
//!
//! - Persistent cross-session history (text-only; no element ranges or attachments).
//! - Local in-session history (full text + text elements + local/remote image attachments).
//!
//! When recalling a local entry, the composer rehydrates text elements and both attachment kinds
//! (local image paths + remote image URLs).
//! When recalling a persistent entry, only the text is restored.
//! Recalled entries move the cursor to end-of-line so repeated Up/Down presses keep shell-like
//! history traversal semantics instead of dropping to column 0.
//!
//! # Submission and Prompt Expansion
//!
//! `Enter` submits immediately. `Tab` requests queuing while a task is running; if no task is
//! running, `Tab` submits just like Enter so input is never dropped.
//! `Tab` does not submit when entering a `!` shell command.
//!
//! On submit/queue paths, the composer:
//!
//! - Expands pending paste placeholders so element ranges align with the final text.
//! - Trims whitespace and rebases text elements accordingly.
//! - Prunes local attached images so only placeholders that survive expansion are sent.
//! - Preserves remote image URLs as separate attachments even when text is empty.
//!
//! When these paths clear the visible textarea after a successful submit or slash-command
//! dispatch, they intentionally preserve the textarea kill buffer. That lets users `Ctrl+K` part
//! of a draft, perform a composer action such as changing reasoning level, and then `Ctrl+Y` the
//! killed text back into the now-empty draft.
//!
//! The numeric auto-submit path used by the slash popup performs the same pending-paste expansion
//! and attachment pruning, and clears pending paste state on success.
//! Slash commands with arguments (like `/plan` and `/review`) reuse the same preparation path so
//! pasted content and text elements are preserved when extracting args.
//!
//! # Remote Image Rows (Up/Down/Delete)
//!
//! Remote image URLs are rendered as non-editable `[Image #N]` rows above the textarea (inside the
//! same composer block). These rows represent image attachments rehydrated from app-server/backtrack
//! history; TUI users can remove them, but cannot type into that row region.
//!
//! Keyboard behavior:
//!
//! - `Up` at textarea cursor `0` enters remote-row selection at the last remote image.
//! - `Up`/`Down` move selection between remote rows.
//! - `Down` on the last row clears selection and returns control to the textarea.
//! - `Delete`/`Backspace` remove the selected remote image row.
//!
//! Placeholder numbering is unified across remote and local images:
//!
//! - Remote rows occupy `[Image #1]..[Image #M]`.
//! - Local placeholders are offset after that range (`[Image #M+1]..`).
//! - Deleting a remote row relabels local placeholders to keep numbering contiguous.
//!
//! # Non-bracketed Paste Bursts
//!
//! On some terminals (especially on Windows), pastes arrive as a rapid sequence of
//! `KeyCode::Char` and `KeyCode::Enter` key events instead of a single paste event.
//!
//! To avoid misinterpreting these bursts as real typing (and to prevent transient UI effects like
//! shortcut overlays toggling on a pasted `?`), we feed "plain" character events into
//! [`PasteBurst`](super::paste_burst::PasteBurst), which buffers bursts and later flushes them
//! through [`ChatComposer::handle_paste`].
//!
//! The burst detector intentionally treats ASCII and non-ASCII differently:
//!
//! - ASCII: we briefly hold the first fast char (flicker suppression) until we know whether the
//!   stream is paste-like.
//! - non-ASCII: we do not hold the first char (IME input would feel dropped), but we still allow
//!   burst detection for actual paste streams.
//!
//! The burst detector can also be disabled (`disable_paste_burst`), which bypasses the state
//! machine and treats the key stream as normal typing. When toggling from enabled → disabled, the
//! composer flushes/clears any in-flight burst state so it cannot leak into subsequent input.
//!
//! For the detailed burst state machine, see `bottom_pane/paste_burst.rs`.
//! For a narrative overview of the combined state machine, see `docs/tui-chat-composer.md`.
//!
//! # PasteBurst Integration Points
//!
//! The burst detector is consulted in a few specific places:
//!
//! - [`ChatComposer::handle_input_basic`]: flushes any due burst first, then intercepts plain char
//!   input to either buffer it or insert normally.
//! - [`ChatComposer::handle_non_ascii_char`]: handles the non-ASCII/IME path without holding the
//!   first char, while still allowing paste detection via retro-capture.
//! - [`ChatComposer::flush_paste_burst_if_due`]/[`ChatComposer::handle_paste_burst_flush`]: called
//!   from UI ticks to turn a pending burst into either an explicit paste (`handle_paste`) or a
//!   normal typed character.
//!
//! # Input Disabled Mode
//!
//! The composer can be temporarily read-only (`input_enabled = false`). In that mode it ignores
//! edits and renders a placeholder prompt instead of the editable textarea. This is part of the
//! overall state machine, since it affects which transitions are even possible from a given UI
//! state.
//!
use crate::bottom_pane::footer::mode_indicator_line;
use crate::key_hint;
use crate::key_hint::KeyBinding;
use crate::key_hint::has_ctrl_or_alt;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::ui_consts::FOOTER_INDENT_COLS;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use devo_core::INPUT_TEXT_LIMIT;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::StatefulWidgetRef;
use ratatui::widgets::WidgetRef;

use super::chat_composer_history::ChatComposerHistory;
use super::chat_composer_history::HistoryEntry;
use super::command_popup::CommandItem;
use super::command_popup::CommandPopup;
use super::command_popup::CommandPopupFlags;
use super::file_search_popup::FileSearchPopup;
use super::footer::CollaborationModeIndicator;
use super::footer::FooterMode;
use super::footer::FooterProps;
use super::footer::SummaryLeft;
use super::footer::can_show_left_with_context;
use super::footer::context_window_line;
use super::footer::esc_hint_mode;
use super::footer::footer_height;
use super::footer::footer_hint_items_width;
use super::footer::footer_line_width;
use super::footer::inset_footer_hint_area;
use super::footer::max_left_width_for_right;
use super::footer::passive_footer_status_line;
use super::footer::render_context_right;
use super::footer::render_footer_from_props;
use super::footer::render_footer_hint_items;
use super::footer::render_footer_line;
use super::footer::reset_mode_after_activity;
use super::footer::single_line_footer_layout;
use super::footer::toggle_shortcut_mode;
use super::footer::uses_passive_footer_status_layout;
use super::paste_burst::CharDecision;
use super::paste_burst::PasteBurst;
use super::skill_popup::MentionItem;
use super::skill_popup::SkillPopup;
use super::slash_commands;
use super::slash_commands::BuiltinCommandFlags;
use crate::bottom_pane::paste_burst::FlushResult;
use crate::bottom_pane::prompt_args::parse_slash_name;
use crate::render::Insets;
use crate::render::RectExt;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
use crate::style::user_message_style;
use devo_protocol::user_input::TextElement;
use devo_protocol::user_input::Utf8ByteSpan as ByteRange;

use crate::app_event::AppEvent;
use crate::app_event::ConnectorsSnapshot;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::PluginCapabilitySummary;
use crate::bottom_pane::SkillMetadata;
use crate::bottom_pane::textarea::TextArea;
use crate::bottom_pane::textarea::TextAreaState;
use crate::clipboard_paste::normalize_pasted_path;
use crate::clipboard_paste::pasted_image_format;
use crate::tui::frame_requester::FrameRequester;
use crate::ui_consts::LIVE_PREFIX_COLS;
use devo_file_search::FileMatch;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

/// If the pasted content exceeds this number of characters, replace it with a
/// placeholder in the UI.
const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;
const ACTIVE_LARGE_PASTE_PLACEHOLDER: &str = "[Pasting...]";

fn user_input_too_large_message(actual_chars: usize) -> String {
    format!(
        "Message exceeds the maximum length of {INPUT_TEXT_LIMIT} characters ({actual_chars} provided)."
    )
}

/// Result returned when the user interacts with the text area.
#[derive(Debug, PartialEq)]
pub enum InputResult {
    Submitted {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Queued {
        text: String,
        text_elements: Vec<TextElement>,
    },
    Command(SlashCommand),
    CommandWithArgs(SlashCommand, String, Vec<TextElement>),
    None,
}

#[derive(Clone, Debug, PartialEq)]
struct AttachedImage {
    placeholder: String,
    path: PathBuf,
}

#[derive(Clone, Debug)]
struct ActiveLargePaste {
    placeholder: String,
    element_id: u64,
    last_input_at: Instant,
}

/// Feature flags for reusing the chat composer in other bottom-pane surfaces.
///
/// The default keeps today's behavior intact. Other call sites can opt out of
/// specific behaviors by constructing a config with those flags set to `false`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ChatComposerConfig {
    /// Whether command/file/skill popups are allowed to appear.
    pub(crate) popups_enabled: bool,
    /// Whether `/...` input is parsed and dispatched as slash commands.
    pub(crate) slash_commands_enabled: bool,
    /// Whether pasting a file path can attach local images.
    pub(crate) image_paste_enabled: bool,
    /// Whether `@...` file-search popups are available.
    pub(crate) file_search_enabled: bool,
}

impl Default for ChatComposerConfig {
    fn default() -> Self {
        Self {
            popups_enabled: true,
            slash_commands_enabled: true,
            image_paste_enabled: true,
            file_search_enabled: true,
        }
    }
}

impl ChatComposerConfig {
    #[allow(dead_code)]
    /// A minimal preset for plain-text inputs embedded in other surfaces.
    ///
    /// This disables popups, slash commands, and image-path attachment behavior
    /// so the composer behaves like a simple notes field.
    pub(crate) const fn plain_text() -> Self {
        Self {
            popups_enabled: false,
            slash_commands_enabled: false,
            image_paste_enabled: false,
            file_search_enabled: false,
        }
    }
}

pub(crate) struct ChatComposer {
    textarea: TextArea,
    textarea_state: RefCell<TextAreaState>,
    active_popup: ActivePopup,
    app_event_tx: AppEventSender,
    history: ChatComposerHistory,
    quit_shortcut_expires_at: Option<Instant>,
    quit_shortcut_key: KeyBinding,
    esc_backtrack_hint: bool,
    use_shift_enter_hint: bool,
    dismissed_file_popup_token: Option<String>,
    current_file_query: Option<String>,
    pending_pastes: Vec<(String, String)>,
    active_large_paste: Option<ActiveLargePaste>,
    large_paste_counters: HashMap<usize, usize>,
    has_focus: bool,
    frame_requester: Option<FrameRequester>,
    /// Invariant: attached images are labeled in vec order as
    /// `[Image #M+1]..[Image #N]`, where `M` is the number of remote images.
    attached_images: Vec<AttachedImage>,
    placeholder_text: String,
    is_task_running: bool,
    /// When false, the composer is temporarily read-only (e.g. during sandbox setup).
    input_enabled: bool,
    input_disabled_placeholder: Option<String>,
    /// Non-bracketed paste burst tracker (see `bottom_pane/paste_burst.rs`).
    paste_burst: PasteBurst,
    // When true, disables paste-burst logic and inserts characters immediately.
    disable_paste_burst: bool,
    footer_mode: FooterMode,
    footer_hint_override: Option<Vec<(String, String)>>,
    remote_image_urls: Vec<String>,
    /// Tracks keyboard selection for the remote-image rows so Up/Down + Delete/Backspace
    /// can highlight and remove remote attachments from the composer UI.
    selected_remote_image_index: Option<usize>,
    footer_flash: Option<FooterFlash>,
    context_window_percent: Option<i64>,
    // Monotonically increasing identifier for textarea elements we insert.
    #[cfg(not(target_os = "linux"))]
    next_element_id: u64,
    context_window_used_tokens: Option<i64>,
    skills: Option<Vec<SkillMetadata>>,
    plugins: Option<Vec<PluginCapabilitySummary>>,
    connectors_snapshot: Option<ConnectorsSnapshot>,
    dismissed_mention_popup_token: Option<String>,
    mention_bindings: HashMap<u64, ComposerMentionBinding>,
    recent_submission_mention_bindings: Vec<MentionBinding>,
    collaboration_modes_enabled: bool,
    config: ChatComposerConfig,
    collaboration_mode_indicator: Option<CollaborationModeIndicator>,
    connectors_enabled: bool,
    plugins_command_enabled: bool,
    fast_command_enabled: bool,
    personality_command_enabled: bool,
    realtime_conversation_enabled: bool,
    audio_device_selection_enabled: bool,
    windows_degraded_sandbox_active: bool,
    is_zellij: bool,
    status_line_value: Option<Line<'static>>,
    status_line_enabled: bool,
    // Agent label injected into the footer's contextual row when multi-agent mode is active.
    active_agent_label: Option<String>,
    accent_color: Color,
}

#[derive(Clone, Debug)]
struct FooterFlash {
    line: Line<'static>,
    expires_at: Instant,
}

#[derive(Clone, Debug)]
struct ComposerMentionBinding {
    mention: String,
    path: String,
}

/// Popup state – at most one can be visible at any time.
enum ActivePopup {
    None,
    Command(CommandPopup),
    File(FileSearchPopup),
    Skill(SkillPopup),
}

const FOOTER_SPACING_HEIGHT: u16 = 0;

impl ChatComposer {
    fn builtin_command_flags(&self) -> BuiltinCommandFlags {
        BuiltinCommandFlags {
            collaboration_modes_enabled: self.collaboration_modes_enabled,
            connectors_enabled: self.connectors_enabled,
            plugins_command_enabled: self.plugins_command_enabled,
            fast_command_enabled: self.fast_command_enabled,
            personality_command_enabled: self.personality_command_enabled,
            realtime_conversation_enabled: self.realtime_conversation_enabled,
            audio_device_selection_enabled: self.audio_device_selection_enabled,
            allow_elevate_sandbox: self.windows_degraded_sandbox_active,
        }
    }

    #[allow(dead_code)]
    pub fn new(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
    ) -> Self {
        Self::new_with_config(
            has_input_focus,
            app_event_tx,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            ChatComposerConfig::default(),
        )
    }

    /// Construct a composer with explicit feature gating.
    ///
    /// This enables reuse in contexts like request-user-input where we want
    /// the same visuals and editing behavior without slash commands or popups.
    pub(crate) fn new_with_config(
        has_input_focus: bool,
        app_event_tx: AppEventSender,
        enhanced_keys_supported: bool,
        placeholder_text: String,
        disable_paste_burst: bool,
        config: ChatComposerConfig,
    ) -> Self {
        let use_shift_enter_hint = enhanced_keys_supported;

        let mut this = Self {
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
            active_popup: ActivePopup::None,
            app_event_tx,
            history: ChatComposerHistory::new(),
            quit_shortcut_expires_at: None,
            quit_shortcut_key: key_hint::ctrl(KeyCode::Char('c')),
            esc_backtrack_hint: false,
            use_shift_enter_hint,
            dismissed_file_popup_token: None,
            current_file_query: None,
            pending_pastes: Vec::new(),
            active_large_paste: None,
            large_paste_counters: HashMap::new(),
            has_focus: has_input_focus,
            frame_requester: None,
            attached_images: Vec::new(),
            placeholder_text,
            is_task_running: false,
            input_enabled: true,
            input_disabled_placeholder: None,
            paste_burst: PasteBurst::default(),
            disable_paste_burst: false,
            footer_mode: FooterMode::ComposerEmpty,
            footer_hint_override: None,
            remote_image_urls: Vec::new(),
            selected_remote_image_index: None,
            footer_flash: None,
            context_window_percent: None,
            #[cfg(not(target_os = "linux"))]
            next_element_id: 0,
            context_window_used_tokens: None,
            skills: None,
            plugins: None,
            connectors_snapshot: None,
            dismissed_mention_popup_token: None,
            mention_bindings: HashMap::new(),
            recent_submission_mention_bindings: Vec::new(),
            collaboration_modes_enabled: false,
            config,
            collaboration_mode_indicator: None,
            connectors_enabled: false,
            plugins_command_enabled: false,
            fast_command_enabled: false,
            personality_command_enabled: false,
            realtime_conversation_enabled: false,
            audio_device_selection_enabled: false,
            windows_degraded_sandbox_active: false,
            is_zellij: matches!(
                devo_utils::terminal_detection::terminal_info().multiplexer,
                Some(devo_utils::terminal_detection::Multiplexer::Zellij {})
            ),
            status_line_value: None,
            status_line_enabled: false,
            active_agent_label: None,
            accent_color: Color::Cyan,
        };
        // Apply configuration via the setter to keep side-effects centralized.
        this.set_disable_paste_burst(disable_paste_burst);
        this
    }

    pub(crate) fn set_accent_color(&mut self, color: Color) {
        self.accent_color = color;
    }

    #[cfg(not(target_os = "linux"))]
    fn next_id(&mut self) -> String {
        let id = self.next_element_id;
        self.next_element_id = self.next_element_id.wrapping_add(1);
        id.to_string()
    }

    pub(crate) fn set_frame_requester(&mut self, frame_requester: FrameRequester) {
        self.frame_requester = Some(frame_requester);
    }

    pub fn set_skill_mentions(&mut self, skills: Option<Vec<SkillMetadata>>) {
        self.skills = skills;
        self.sync_popups();
    }

    #[allow(dead_code)]
    /// Toggle composer-side image paste handling.
    ///
    /// This only affects whether image-like paste content is converted into attachments; the
    /// `ChatWidget` layer still performs capability checks before images are submitted.
    pub fn set_image_paste_enabled(&mut self, enabled: bool) {
        self.config.image_paste_enabled = enabled;
    }

    #[allow(dead_code)]
    pub(crate) fn take_mention_bindings(&mut self) -> Vec<MentionBinding> {
        let elements = self.current_mention_elements();
        let mut ordered = Vec::new();
        for (id, mention) in elements {
            if let Some(binding) = self.mention_bindings.remove(&id)
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    mention: binding.mention,
                    path: binding.path,
                });
            }
        }
        self.mention_bindings.clear();
        ordered
    }

    #[allow(dead_code)]
    pub fn set_collaboration_modes_enabled(&mut self, enabled: bool) {
        self.collaboration_modes_enabled = enabled;
    }

    #[allow(dead_code)]
    pub fn set_connectors_enabled(&mut self, enabled: bool) {
        self.connectors_enabled = enabled;
    }

    #[allow(dead_code)]
    pub(crate) fn set_fast_command_enabled(&mut self, enabled: bool) {
        self.fast_command_enabled = enabled;
    }

    #[allow(dead_code)]
    pub fn set_collaboration_mode_indicator(
        &mut self,
        indicator: Option<CollaborationModeIndicator>,
    ) {
        self.collaboration_mode_indicator = indicator;
    }

    #[allow(dead_code)]
    pub fn set_personality_command_enabled(&mut self, enabled: bool) {
        self.personality_command_enabled = enabled;
    }

    #[allow(dead_code)]
    pub fn set_realtime_conversation_enabled(&mut self, enabled: bool) {
        self.realtime_conversation_enabled = enabled;
    }

    #[allow(dead_code)]
    pub fn set_audio_device_selection_enabled(&mut self, enabled: bool) {
        self.audio_device_selection_enabled = enabled;
    }

    #[allow(dead_code)]
    /// Compatibility shim for tests that still toggle the removed steer mode flag.
    #[cfg(test)]
    pub fn set_steer_enabled(&mut self, _enabled: bool) {}

    /// Centralized feature gating keeps config checks out of call sites.
    fn popups_enabled(&self) -> bool {
        self.config.popups_enabled
    }

    fn slash_commands_enabled(&self) -> bool {
        self.config.slash_commands_enabled
    }

    fn image_paste_enabled(&self) -> bool {
        self.config.image_paste_enabled
    }

    fn file_search_enabled(&self) -> bool {
        self.config.file_search_enabled
    }

    #[allow(dead_code)]
    #[cfg(target_os = "windows")]
    pub fn set_windows_degraded_sandbox_active(&mut self, enabled: bool) {
        self.windows_degraded_sandbox_active = enabled;
    }

    fn layout_areas(&self, area: Rect) -> [Rect; 4] {
        let footer_props = self.footer_props();
        let footer_hint_height = self
            .custom_footer_height()
            .unwrap_or_else(|| footer_height(&footer_props));
        let footer_spacing = Self::footer_spacing(footer_hint_height);
        let footer_total_height = footer_hint_height + footer_spacing;
        let popup_constraint = match &self.active_popup {
            ActivePopup::Command(popup) => {
                Constraint::Max(popup.calculate_required_height(area.width))
            }
            ActivePopup::File(popup) => Constraint::Max(popup.calculate_required_height()),
            ActivePopup::Skill(popup) => {
                Constraint::Max(popup.calculate_required_height(area.width))
            }
            ActivePopup::None => Constraint::Max(footer_total_height),
        };
        let [composer_rect, popup_rect] =
            Layout::vertical([Constraint::Min(3), popup_constraint]).areas(area);
        let mut textarea_rect = composer_rect.inset(Insets::tlbr(
            /*top*/ 1,
            LIVE_PREFIX_COLS,
            /*bottom*/ 1,
            /*right*/ 1,
        ));
        let remote_images_height = self
            .remote_images_lines(textarea_rect.width)
            .len()
            .try_into()
            .unwrap_or(u16::MAX)
            .min(textarea_rect.height.saturating_sub(1));
        let remote_images_separator = u16::from(remote_images_height > 0);
        let consumed = remote_images_height.saturating_add(remote_images_separator);
        let remote_images_rect = Rect {
            x: textarea_rect.x,
            y: textarea_rect.y,
            width: textarea_rect.width,
            height: remote_images_height,
        };
        textarea_rect.y = textarea_rect.y.saturating_add(consumed);
        textarea_rect.height = textarea_rect.height.saturating_sub(consumed);
        [composer_rect, remote_images_rect, textarea_rect, popup_rect]
    }

    fn footer_spacing(footer_hint_height: u16) -> u16 {
        if footer_hint_height == 0 {
            0
        } else {
            FOOTER_SPACING_HEIGHT
        }
    }

    #[allow(dead_code)]
    pub fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if !self.input_enabled {
            return None;
        }

        let [_, _, textarea_rect, _] = self.layout_areas(area);
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }
    /// Returns true if the composer currently contains no user-entered input.
    pub(crate) fn is_empty(&self) -> bool {
        self.textarea.is_empty()
            && self.attached_images.is_empty()
            && self.remote_image_urls.is_empty()
    }

    #[allow(dead_code)]
    /// Record the history metadata advertised by `SessionConfiguredEvent` so
    /// that the composer can navigate cross-session history.
    pub(crate) fn set_history_metadata(&mut self, log_id: u64, entry_count: usize) {
        self.history.set_metadata(log_id, entry_count);
    }

    #[allow(dead_code)]
    /// Integrate an asynchronous response to an on-demand history lookup.
    ///
    /// If the entry is present and the offset still matches the active history cursor, the
    /// composer rehydrates the entry immediately. This path intentionally routes through
    /// [`Self::apply_history_entry`] so cursor placement remains aligned with keyboard history
    /// recall semantics.
    pub(crate) fn on_history_entry_response(
        &mut self,
        log_id: u64,
        offset: usize,
        entry: Option<String>,
    ) -> bool {
        let Some(entry) = self.history.on_entry_response(log_id, offset, entry) else {
            return false;
        };
        // Persistent ↑/↓ history is text-only (backwards-compatible and avoids persisting
        // attachments), but local in-session ↑/↓ history can rehydrate elements and image paths.
        self.apply_history_entry(entry);
        true
    }

    /// Integrate pasted text into the composer.
    ///
    /// Acts as the only place where paste text is integrated, both for:
    ///
    /// - Real/explicit paste events surfaced by the terminal, and
    /// - Non-bracketed "paste bursts" that [`PasteBurst`](super::paste_burst::PasteBurst) buffers
    ///   and later flushes here.
    ///
    /// Behavior:
    ///
    /// - If the paste is larger than `LARGE_PASTE_CHAR_THRESHOLD` chars, inserts a placeholder
    ///   element (expanded on submit) and stores the full text in `pending_pastes`.
    /// - Otherwise, if the paste looks like an image path, attaches the image and inserts a
    ///   trailing space so the user can keep typing naturally.
    /// - Otherwise, inserts the pasted text directly into the textarea.
    ///
    /// In all cases, clears any paste-burst Enter suppression state so a real paste cannot affect
    /// the next user Enter key, then syncs popup state.
    pub fn handle_paste(&mut self, pasted: String) -> bool {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let char_count = pasted.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            self.insert_large_paste_placeholder(pasted);
        } else if char_count > 1
            && self.image_paste_enabled()
            && self.handle_paste_image_path(pasted.clone())
        {
            self.textarea.insert_str(" ");
        } else {
            self.insert_str(&pasted);
        }
        self.paste_burst.clear_after_explicit_paste();
        self.sync_popups();
        true
    }

    pub fn handle_paste_image_path(&mut self, pasted: String) -> bool {
        let Some(path_buf) = normalize_pasted_path(&pasted) else {
            return false;
        };

        // normalize_pasted_path already handles Windows → WSL path conversion,
        // so we can directly try to read the image dimensions.
        match image::image_dimensions(&path_buf) {
            Ok((width, height)) => {
                tracing::info!("OK: {pasted}");
                tracing::debug!("image dimensions={}x{}", width, height);
                let format = pasted_image_format(&path_buf);
                tracing::debug!("attached image format={}", format.label());
                self.attach_image(path_buf);
                true
            }
            Err(err) => {
                tracing::trace!("ERR: {err}");
                false
            }
        }
    }

    pub(crate) fn attach_image(&mut self, path: PathBuf) {
        let placeholder =
            local_image_label_text(self.remote_image_urls.len() + self.attached_images.len() + 1);
        self.textarea.insert_element(&placeholder);
        self.attached_images
            .push(AttachedImage { placeholder, path });
        self.sync_popups();
    }

    /// Enable or disable paste-burst handling.
    ///
    /// `disable_paste_burst` is an escape hatch for terminals/platforms where the burst heuristic
    /// is unwanted or has already been handled elsewhere.
    ///
    /// When transitioning from enabled → disabled, we "defuse" any in-flight burst state so it
    /// cannot affect subsequent normal typing:
    ///
    /// - First, flush any held/buffered text immediately via
    ///   [`PasteBurst::flush_before_modified_input`], and feed it through `handle_paste(String)`.
    ///   This preserves user input and routes it through the same integration path as explicit
    ///   pastes (large-paste placeholders, image-path detection, and popup sync).
    /// - Then clear the burst timing and Enter-suppression window via
    ///   [`PasteBurst::clear_after_explicit_paste`].
    ///
    /// We intentionally do not use `clear_window_after_non_char()` here: it clears timing state
    /// without emitting any buffered text, which can leave a non-empty buffer unable to flush
    /// later (because `flush_if_due()` relies on `last_plain_char_time` to time out).
    pub(crate) fn set_disable_paste_burst(&mut self, disabled: bool) {
        let was_disabled = self.disable_paste_burst;
        self.disable_paste_burst = disabled;
        if disabled && !was_disabled {
            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            self.paste_burst.clear_after_explicit_paste();
        }
    }

    #[allow(dead_code)]
    /// Replace the composer content with text from an external editor.
    /// Clears pending paste placeholders and keeps only attachments whose
    /// placeholder labels still appear in the new text. Image placeholders
    /// are renumbered to `[Image #M+1]..[Image #N]` (where `M` is the number of
    /// remote images). Cursor is placed at the end after rebuilding elements.
    pub(crate) fn apply_external_edit(&mut self, text: String) {
        self.pending_pastes.clear();

        // Count placeholder occurrences in the new text.
        let mut placeholder_counts: HashMap<String, usize> = HashMap::new();
        for placeholder in self.attached_images.iter().map(|img| &img.placeholder) {
            if placeholder_counts.contains_key(placeholder) {
                continue;
            }
            let count = text.match_indices(placeholder).count();
            if count > 0 {
                placeholder_counts.insert(placeholder.clone(), count);
            }
        }

        // Keep attachments only while we have matching occurrences left.
        let mut kept_images = Vec::new();
        for img in self.attached_images.drain(..) {
            if let Some(count) = placeholder_counts.get_mut(&img.placeholder)
                && *count > 0
            {
                *count -= 1;
                kept_images.push(img);
            }
        }
        self.attached_images = kept_images;

        // Rebuild textarea so placeholders become elements again.
        self.textarea.set_text_clearing_elements("");
        let mut remaining: HashMap<&str, usize> = HashMap::new();
        for img in &self.attached_images {
            *remaining.entry(img.placeholder.as_str()).or_insert(0) += 1;
        }

        let mut occurrences: Vec<(usize, &str)> = Vec::new();
        for placeholder in remaining.keys() {
            for (pos, _) in text.match_indices(placeholder) {
                occurrences.push((pos, *placeholder));
            }
        }
        occurrences.sort_unstable_by_key(|(pos, _)| *pos);

        let mut idx = 0usize;
        for (pos, ph) in occurrences {
            let Some(count) = remaining.get_mut(ph) else {
                continue;
            };
            if *count == 0 {
                continue;
            }
            if pos > idx {
                self.textarea.insert_str(&text[idx..pos]);
            }
            self.textarea.insert_element(ph);
            *count -= 1;
            idx = pos + ph.len();
        }
        if idx < text.len() {
            self.textarea.insert_str(&text[idx..]);
        }

        // Keep local image placeholders normalized in attachment order after the
        // remote-image prefix.
        self.relabel_attached_images_and_update_placeholders();
        self.textarea.set_cursor(self.textarea.text().len());
        self.sync_popups();
    }

    #[allow(dead_code)]
    pub(crate) fn current_text_with_pending(&self) -> String {
        let mut text = self.textarea.text().to_string();
        for (placeholder, actual) in &self.pending_pastes {
            if text.contains(placeholder) {
                text = text.replace(placeholder, actual);
            }
        }
        text
    }

    #[allow(dead_code)]
    pub(crate) fn pending_pastes(&self) -> Vec<(String, String)> {
        self.pending_pastes.clone()
    }

    pub(crate) fn set_pending_pastes(&mut self, pending_pastes: Vec<(String, String)>) {
        let text = self.textarea.text().to_string();
        self.pending_pastes = pending_pastes
            .into_iter()
            .filter(|(placeholder, _)| text.contains(placeholder))
            .collect();
    }

    #[allow(dead_code)]
    /// Override the footer hint items displayed beneath the composer. Passing
    /// `None` restores the default shortcut footer.
    pub(crate) fn set_footer_hint_override(&mut self, items: Option<Vec<(String, String)>>) {
        self.footer_hint_override = items;
    }

    pub(crate) fn set_remote_image_urls(&mut self, urls: Vec<String>) {
        self.remote_image_urls = urls;
        self.selected_remote_image_index = None;
        self.relabel_attached_images_and_update_placeholders();
        self.sync_popups();
    }

    #[allow(dead_code)]
    pub(crate) fn remote_image_urls(&self) -> Vec<String> {
        self.remote_image_urls.clone()
    }

    #[allow(dead_code)]
    pub(crate) fn take_remote_image_urls(&mut self) -> Vec<String> {
        let urls = std::mem::take(&mut self.remote_image_urls);
        self.selected_remote_image_index = None;
        self.relabel_attached_images_and_update_placeholders();
        self.sync_popups();
        urls
    }

    #[allow(dead_code)]
    #[cfg(test)]
    pub(crate) fn show_footer_flash(&mut self, line: Line<'static>, duration: Duration) {
        let expires_at = Instant::now()
            .checked_add(duration)
            .unwrap_or_else(Instant::now);
        self.footer_flash = Some(FooterFlash { line, expires_at });
    }

    pub(crate) fn footer_flash_visible(&self) -> bool {
        self.footer_flash
            .as_ref()
            .is_some_and(|flash| Instant::now() < flash.expires_at)
    }

    /// Replace the entire composer content with `text` and reset cursor.
    ///
    /// This is the "fresh draft" path: it clears pending paste payloads and
    /// mention link targets. Callers restoring a previously submitted draft
    /// that must keep `$name -> path` resolution should use
    /// [`Self::set_text_content_with_mention_bindings`] instead.
    pub(crate) fn set_text_content(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
    ) {
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            Vec::new(),
        );
    }

    /// Replace the entire composer content while restoring mention link targets.
    ///
    /// Mention popup insertion stores both visible text (for example `$file`)
    /// and hidden mention bindings used to resolve the canonical target during
    /// submission. Use this method when restoring an interrupted or blocked
    /// draft; if callers restore only text and images, mentions can appear
    /// intact to users while resolving to the wrong target or dropping on
    /// retry.
    ///
    /// This helper intentionally places the cursor at the start of the restored text. Callers
    /// that need end-of-line restore behavior (for example shell-style history recall) should call
    /// [`Self::move_cursor_to_end`] after this method.
    pub(crate) fn set_text_content_with_mention_bindings(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        local_image_paths: Vec<PathBuf>,
        mention_bindings: Vec<MentionBinding>,
    ) {
        // Clear any existing content, placeholders, and attachments first.
        self.textarea.set_text_clearing_elements("");
        self.pending_pastes.clear();
        self.attached_images.clear();
        self.mention_bindings.clear();

        self.textarea.set_text_with_elements(&text, &text_elements);

        for (idx, path) in local_image_paths.into_iter().enumerate() {
            let placeholder = local_image_label_text(self.remote_image_urls.len() + idx + 1);
            self.attached_images
                .push(AttachedImage { placeholder, path });
        }

        self.bind_mentions_from_snapshot(mention_bindings);
        self.relabel_attached_images_and_update_placeholders();
        self.selected_remote_image_index = None;
        self.textarea.set_cursor(/*pos*/ 0);
        self.sync_popups();
    }

    /// Update the placeholder text without changing input enablement.
    pub(crate) fn set_placeholder_text(&mut self, placeholder: String) {
        self.placeholder_text = placeholder;
    }

    /// Move the cursor to the end of the current text buffer.
    pub(crate) fn move_cursor_to_end(&mut self) {
        self.textarea.set_cursor(self.textarea.text().len());
        self.sync_popups();
    }

    #[allow(dead_code)]
    pub(crate) fn clear_for_ctrl_c(&mut self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let previous = self.current_text();
        let text_elements = self.textarea.text_elements();
        let local_image_paths = self
            .attached_images
            .iter()
            .map(|img| img.path.clone())
            .collect();
        let pending_pastes = std::mem::take(&mut self.pending_pastes);
        let remote_image_urls = self.remote_image_urls.clone();
        let mention_bindings = self.snapshot_mention_bindings();
        self.set_text_content(String::new(), Vec::new(), Vec::new());
        self.remote_image_urls.clear();
        self.selected_remote_image_index = None;
        self.history.reset_navigation();
        self.history.record_local_submission(HistoryEntry {
            text: previous.clone(),
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        });
        Some(previous)
    }

    /// Get the current composer text.
    pub(crate) fn current_text(&self) -> String {
        self.textarea.text().to_string()
    }

    /// Rehydrate a history entry into the composer with shell-like cursor placement.
    ///
    /// This path restores text, elements, images, mention bindings, and pending paste payloads,
    /// then moves the cursor to end-of-line. If a caller reused
    /// [`Self::set_text_content_with_mention_bindings`] directly for history recall and forgot the
    /// final cursor move, repeated Up/Down would stop navigating history because cursor-gating
    /// treats interior positions as normal editing mode.
    fn apply_history_entry(&mut self, entry: HistoryEntry) {
        let HistoryEntry {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        } = entry;
        self.set_remote_image_urls(remote_image_urls);
        self.set_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.set_pending_pastes(pending_pastes);
        self.move_cursor_to_end();
    }

    #[allow(dead_code)]
    pub(crate) fn text_elements(&self) -> Vec<TextElement> {
        self.textarea.text_elements()
    }

    #[allow(dead_code)]
    pub(crate) fn local_images(&self) -> Vec<LocalImageAttachment> {
        self.attached_images
            .iter()
            .map(|img| LocalImageAttachment {
                placeholder: img.placeholder.clone(),
                path: img.path.clone(),
            })
            .collect()
    }

    #[allow(dead_code)]
    pub(crate) fn mention_bindings(&self) -> Vec<MentionBinding> {
        self.snapshot_mention_bindings()
    }

    pub(crate) fn take_recent_submission_mention_bindings(&mut self) -> Vec<MentionBinding> {
        std::mem::take(&mut self.recent_submission_mention_bindings)
    }

    fn prune_attached_images_for_submission(&mut self, text: &str, text_elements: &[TextElement]) {
        if self.attached_images.is_empty() {
            return;
        }
        let image_placeholders: HashSet<&str> = text_elements
            .iter()
            .filter_map(|elem| elem.placeholder(text))
            .collect();
        self.attached_images
            .retain(|img| image_placeholders.contains(img.placeholder.as_str()));
    }

    #[allow(dead_code)]
    #[cfg(test)]
    pub fn take_recent_submission_images(&mut self) -> Vec<PathBuf> {
        let images = std::mem::take(&mut self.attached_images);
        images.into_iter().map(|img| img.path).collect()
    }

    pub fn take_recent_submission_images_with_placeholders(&mut self) -> Vec<LocalImageAttachment> {
        let images = std::mem::take(&mut self.attached_images);
        images
            .into_iter()
            .map(|img| LocalImageAttachment {
                placeholder: img.placeholder,
                path: img.path,
            })
            .collect()
    }

    /// Flushes any due paste-burst state.
    ///
    /// Call this from a UI tick to turn paste-burst transient state into explicit textarea edits:
    ///
    /// - If a burst times out, flush it via `handle_paste(String)`.
    /// - If only the first ASCII char was held (flicker suppression) and no burst followed, emit it
    ///   as normal typed input.
    ///
    /// This also allows a single "held" ASCII char to render even when it turns out not to be part
    /// of a paste burst.
    pub(crate) fn flush_paste_burst_if_due(&mut self) -> bool {
        self.handle_paste_burst_flush(Instant::now())
    }

    /// Returns whether the composer is currently in any paste-burst related transient state.
    ///
    /// This includes actively buffering, having a non-empty burst buffer, or holding the first
    /// ASCII char for flicker suppression.
    pub(crate) fn is_in_paste_burst(&self) -> bool {
        self.paste_burst.is_active() || self.active_large_paste.is_some()
    }

    /// Returns a delay that reliably exceeds the paste-burst timing threshold.
    ///
    /// Use this in tests to avoid boundary flakiness around the `PasteBurst` timeout.
    pub(crate) fn recommended_paste_flush_delay() -> Duration {
        PasteBurst::recommended_flush_delay()
    }

    #[allow(dead_code)]
    /// Integrate results from an asynchronous file search.
    pub(crate) fn on_file_search_result(&mut self, query: String, matches: Vec<FileMatch>) {
        // Only apply if user is still editing a token starting with `query`.
        let current_opt = Self::current_at_token(&self.textarea);
        let Some(current_token) = current_opt else {
            return;
        };

        if !current_token.starts_with(&query) {
            return;
        }

        if let ActivePopup::File(popup) = &mut self.active_popup {
            popup.set_matches(&query, matches);
        }
    }

    #[allow(dead_code)]
    /// Show the transient "press again to quit" hint for `key`.
    ///
    /// The owner (`BottomPane`/`ChatWidget`) is responsible for scheduling a
    /// redraw after [`super::QUIT_SHORTCUT_TIMEOUT`] so the hint can disappear
    /// even when the UI is otherwise idle.
    pub fn show_quit_shortcut_hint(&mut self, key: KeyBinding, has_focus: bool) {
        self.quit_shortcut_expires_at = Instant::now()
            .checked_add(super::QUIT_SHORTCUT_TIMEOUT)
            .or_else(|| Some(Instant::now()));
        self.quit_shortcut_key = key;
        self.footer_mode = FooterMode::QuitShortcutReminder;
        self.set_has_focus(has_focus);
    }

    #[allow(dead_code)]
    /// Clear the "press again to quit" hint immediately.
    pub fn clear_quit_shortcut_hint(&mut self, has_focus: bool) {
        self.quit_shortcut_expires_at = None;
        self.footer_mode = reset_mode_after_activity(self.footer_mode);
        self.set_has_focus(has_focus);
    }

    /// Whether the quit shortcut hint should currently be shown.
    ///
    /// This is time-based rather than event-based: it may become false without
    /// any additional user input, so the UI schedules a redraw when the hint
    /// expires.
    pub(crate) fn quit_shortcut_hint_visible(&self) -> bool {
        self.quit_shortcut_expires_at
            .is_some_and(|expires_at| Instant::now() < expires_at)
    }

    fn next_large_paste_placeholder(&mut self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let next_suffix = self.large_paste_counters.entry(char_count).or_insert(0);
        *next_suffix += 1;
        if *next_suffix == 1 {
            base
        } else {
            format!("{base} #{next_suffix}")
        }
    }

    fn insert_large_paste_placeholder(&mut self, pasted: String) -> String {
        let placeholder = self.next_large_paste_placeholder(pasted.chars().count());
        self.textarea.insert_element(&placeholder);
        self.pending_pastes.push((placeholder.clone(), pasted));
        placeholder
    }

    fn insert_active_large_paste_placeholder(&mut self, pasted: String, now: Instant) {
        let placeholder = ACTIVE_LARGE_PASTE_PLACEHOLDER.to_string();
        let element_id = self.textarea.insert_element(&placeholder);
        self.pending_pastes.push((placeholder.clone(), pasted));
        self.active_large_paste = Some(ActiveLargePaste {
            placeholder,
            element_id,
            last_input_at: now,
        });
    }

    fn append_to_active_large_paste(&mut self, ch: char, now: Instant) -> bool {
        let Some(active) = &self.active_large_paste else {
            return false;
        };
        if now.duration_since(active.last_input_at) >= PasteBurst::recommended_flush_delay() {
            self.active_large_paste = None;
            return false;
        }
        let placeholder = active.placeholder.clone();
        let Some((_, pasted)) = self
            .pending_pastes
            .iter_mut()
            .find(|(candidate, _)| candidate == &placeholder)
        else {
            self.active_large_paste = None;
            return false;
        };
        pasted.push(ch);
        if let Some(active) = &mut self.active_large_paste {
            active.last_input_at = now;
        }
        true
    }

    fn finalize_active_large_paste_if_idle(&mut self, now: Instant) -> bool {
        if !self.active_large_paste.as_ref().is_some_and(|active| {
            now.duration_since(active.last_input_at) >= PasteBurst::recommended_flush_delay()
        }) {
            return false;
        }

        let Some(active) = self.active_large_paste.take() else {
            return false;
        };
        let Some(index) = self
            .pending_pastes
            .iter()
            .position(|(placeholder, _)| placeholder == &active.placeholder)
        else {
            return false;
        };
        let final_placeholder =
            self.next_large_paste_placeholder(self.pending_pastes[index].1.chars().count());
        if self
            .textarea
            .update_element_by_id(active.element_id, &final_placeholder)
        {
            self.pending_pastes[index].0 = final_placeholder;
            self.paste_burst.clear_after_explicit_paste();
            self.sync_popups();
            true
        } else {
            self.pending_pastes.remove(index);
            self.paste_burst.clear_after_explicit_paste();
            false
        }
    }

    fn promote_paste_burst_to_large_placeholder_if_needed(&mut self, now: Instant) -> bool {
        if self.paste_burst.buffered_char_count() <= LARGE_PASTE_CHAR_THRESHOLD {
            return false;
        }
        let Some(pasted) = self.paste_burst.flush_before_modified_input() else {
            return false;
        };
        self.insert_active_large_paste_placeholder(pasted, now);
        self.sync_popups();
        true
    }

    pub(crate) fn insert_str(&mut self, text: &str) {
        self.textarea.insert_str(text);
        self.sync_popups();
    }

    /// Handle a key event coming from the main UI.
    pub fn handle_key_event(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if !self.input_enabled {
            return (InputResult::None, false);
        }

        if matches!(key_event.kind, KeyEventKind::Release) {
            return (InputResult::None, false);
        }

        let result = match &mut self.active_popup {
            ActivePopup::Command(_) => self.handle_key_event_with_slash_popup(key_event),
            ActivePopup::File(_) => self.handle_key_event_with_file_popup(key_event),
            ActivePopup::Skill(_) => self.handle_key_event_with_skill_popup(key_event),
            ActivePopup::None => self.handle_key_event_without_popup(key_event),
        };
        // Update (or hide/show) popup after processing the key.
        self.sync_popups();
        result
    }

    /// Return true if either the slash-command popup or the file-search popup is active.
    pub(crate) fn popup_active(&self) -> bool {
        !matches!(self.active_popup, ActivePopup::None)
    }

    /// Handle key event when the slash-command popup is visible.
    fn handle_key_event_with_slash_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        if key_event.code == KeyCode::Esc {
            let next_mode = esc_hint_mode(self.footer_mode, self.is_task_running);
            if next_mode != self.footer_mode {
                self.footer_mode = next_mode;
                return (InputResult::None, true);
            }
        } else {
            self.footer_mode = reset_mode_after_activity(self.footer_mode);
        }
        let ActivePopup::Command(popup) = &mut self.active_popup else {
            unreachable!();
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                // Dismiss the slash popup; keep the current input untouched.
                self.active_popup = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                // Ensure popup filtering/selection reflects the latest composer text
                // before applying completion.
                let first_line = self.textarea.text().lines().next().unwrap_or("");
                popup.on_composer_text_change(first_line.to_string());
                if let Some(sel) = popup.selected_item() {
                    let CommandItem::Builtin(cmd) = sel;
                    let starts_with_cmd = first_line
                        .trim_start()
                        .starts_with(&format!("/{}", cmd.command()));
                    if !starts_with_cmd {
                        self.textarea
                            .set_text_clearing_elements(&format!("/{} ", cmd.command()));
                    }
                    if !self.textarea.text().is_empty() {
                        self.textarea.set_cursor(self.textarea.text().len());
                    }
                }
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(sel) = popup.selected_item() {
                    let CommandItem::Builtin(cmd) = sel;
                    // If the command supports inline args and there's already
                    // text after the command name, don't clear the textarea —
                    // let the default submission handle it so the arguments
                    // are preserved.
                    let text = self.textarea.text();
                    if !cmd.supports_inline_args() || text.split_whitespace().count() < 2 {
                        self.textarea.set_text_clearing_elements("");
                        return (InputResult::Command(cmd), true);
                    }
                }
                // Fallback to default newline handling if no command selected
                // or the command has inline args that should be submitted.
                self.handle_key_event_without_popup(key_event)
            }
            input => self.handle_input_basic(input),
        }
    }

    #[inline]
    fn clamp_to_char_boundary(text: &str, pos: usize) -> usize {
        let mut p = pos.min(text.len());
        if p < text.len() && !text.is_char_boundary(p) {
            p = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= p)
                .last()
                .unwrap_or(0);
        }
        p
    }

    /// Handle non-ASCII character input (often IME) while still supporting paste-burst detection.
    ///
    /// This handler exists because non-ASCII input often comes from IMEs, where characters can
    /// legitimately arrive in short bursts that should **not** be treated as paste.
    ///
    /// The key differences from the ASCII path:
    ///
    /// - We never hold the first character (`PasteBurst::on_plain_char_no_hold`), because holding a
    ///   non-ASCII char can feel like dropped input.
    /// - If a burst is detected, we may need to retroactively remove already-inserted text before
    ///   the cursor and move it into the paste buffer (see `PasteBurst::decide_begin_buffer`).
    ///
    /// Because this path mixes "insert immediately" with "maybe retro-grab later", it must clamp
    /// the cursor to a UTF-8 char boundary before slicing `textarea.text()`.
    #[inline]
    fn handle_non_ascii_char(&mut self, input: KeyEvent, now: Instant) -> (InputResult, bool) {
        if self.disable_paste_burst {
            // When burst detection is disabled, treat IME/non-ASCII input as normal typing.
            // In particular, do not retro-capture or buffer already-inserted prefix text.
            self.textarea.input(input);
            let text_after = self.textarea.text();
            self.pending_pastes
                .retain(|(placeholder, _)| text_after.contains(placeholder));
            return (InputResult::None, true);
        }
        if let KeyEvent {
            code: KeyCode::Char(ch),
            ..
        } = input
        {
            if self.paste_burst.try_append_char_if_active(ch, now) {
                self.promote_paste_burst_to_large_placeholder_if_needed(now);
                return (InputResult::None, true);
            }
            if self.append_to_active_large_paste(ch, now) {
                return (InputResult::None, true);
            }
            // Non-ASCII input often comes from IMEs and can arrive in quick bursts.
            // We do not want to hold the first char (flicker suppression) on this path, but we
            // still want to detect paste-like bursts. Before applying any non-ASCII input, flush
            // any existing burst buffer (including a pending first char from the ASCII path) so
            // we don't carry that transient state forward.
            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
            if let Some(decision) = self.paste_burst.on_plain_char_no_hold(now) {
                match decision {
                    CharDecision::BufferAppend => {
                        self.paste_burst.append_char_to_buffer(ch, now);
                        self.promote_paste_burst_to_large_placeholder_if_needed(now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        // For non-ASCII we inserted prior chars immediately, so if this turns out
                        // to be paste-like we need to retroactively grab & remove the already-
                        // inserted prefix from the textarea before buffering the burst.
                        let cur = self.textarea.cursor();
                        let txt = self.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) =
                            self.paste_burst
                                .decide_begin_buffer(now, before, retro_chars as usize)
                        {
                            if !grab.grabbed.is_empty() {
                                self.textarea.replace_range(grab.start_byte..safe_cur, "");
                            }
                            // seed the paste burst buffer with everything (grabbed + new)
                            self.paste_burst.append_char_to_buffer(ch, now);
                            self.promote_paste_burst_to_large_placeholder_if_needed(now);
                            return (InputResult::None, true);
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    _ => unreachable!("on_plain_char_no_hold returned unexpected variant"),
                }
            }
        }
        if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }
        self.textarea.input(input);

        let text_after = self.textarea.text();
        self.pending_pastes
            .retain(|(placeholder, _)| text_after.contains(placeholder));
        (InputResult::None, true)
    }

    /// Handle key events when file search popup is visible.
    fn handle_key_event_with_file_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        if key_event.code == KeyCode::Esc {
            let next_mode = esc_hint_mode(self.footer_mode, self.is_task_running);
            if next_mode != self.footer_mode {
                self.footer_mode = next_mode;
                return (InputResult::None, true);
            }
        } else {
            self.footer_mode = reset_mode_after_activity(self.footer_mode);
        }
        let ActivePopup::File(popup) = &mut self.active_popup else {
            unreachable!();
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                // Hide popup without modifying text, remember token to avoid immediate reopen.
                if let Some(tok) = Self::current_at_token(&self.textarea) {
                    self.dismissed_file_popup_token = Some(tok);
                }
                self.active_popup = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                let Some(sel) = popup.selected_match() else {
                    self.active_popup = ActivePopup::None;
                    return if key_event.code == KeyCode::Enter {
                        self.handle_key_event_without_popup(key_event)
                    } else {
                        (InputResult::None, true)
                    };
                };

                let sel_path = sel.to_string_lossy().to_string();
                // If selected path looks like an image (png/jpeg), attach as image instead of inserting text.
                let is_image = Self::is_image_path(&sel_path);
                if is_image {
                    // Determine dimensions; if that fails fall back to normal path insertion.
                    let path_buf = PathBuf::from(&sel_path);
                    match image::image_dimensions(&path_buf) {
                        Ok((width, height)) => {
                            tracing::debug!("selected image dimensions={}x{}", width, height);
                            // Remove the current @token (mirror logic from insert_selected_path without inserting text)
                            // using the flat text and byte-offset cursor API.
                            let cursor_offset = self.textarea.cursor();
                            let text = self.textarea.text();
                            // Clamp to a valid char boundary to avoid panics when slicing.
                            let safe_cursor = Self::clamp_to_char_boundary(text, cursor_offset);
                            let before_cursor = &text[..safe_cursor];
                            let after_cursor = &text[safe_cursor..];

                            // Determine token boundaries in the full text.
                            let start_idx = before_cursor
                                .char_indices()
                                .rfind(|(_, c)| c.is_whitespace())
                                .map(|(idx, c)| idx + c.len_utf8())
                                .unwrap_or(0);
                            let end_rel_idx = after_cursor
                                .char_indices()
                                .find(|(_, c)| c.is_whitespace())
                                .map(|(idx, _)| idx)
                                .unwrap_or(after_cursor.len());
                            let end_idx = safe_cursor + end_rel_idx;

                            self.textarea.replace_range(start_idx..end_idx, "");
                            self.textarea.set_cursor(start_idx);

                            self.attach_image(path_buf);
                            // Add a trailing space to keep typing fluid.
                            self.textarea.insert_str(" ");
                        }
                        Err(err) => {
                            tracing::trace!("image dimensions lookup failed: {err}");
                            // Fallback to plain path insertion if metadata read fails.
                            self.insert_selected_path(&sel_path);
                        }
                    }
                } else {
                    // Non-image: inserting file path.
                    self.insert_selected_path(&sel_path);
                }
                self.active_popup = ActivePopup::None;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        }
    }

    fn handle_key_event_with_skill_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        self.footer_mode = reset_mode_after_activity(self.footer_mode);

        let ActivePopup::Skill(popup) = &mut self.active_popup else {
            unreachable!();
        };

        let mut selected_mention: Option<(String, Option<String>)> = None;
        let mut close_popup = false;

        let result = match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some(tok) = self.current_mention_token() {
                    self.dismissed_mention_popup_token = Some(tok);
                }
                self.active_popup = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if let Some(mention) = popup.selected_mention() {
                    selected_mention = Some((mention.insert_text.clone(), mention.path.clone()));
                }
                close_popup = true;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        };

        if close_popup {
            if let Some((insert_text, path)) = selected_mention {
                self.insert_selected_mention(&insert_text, path.as_deref());
            }
            self.active_popup = ActivePopup::None;
        }

        result
    }

    fn is_image_path(path: &str) -> bool {
        let lower = path.to_ascii_lowercase();
        lower.ends_with(".png")
            || lower.ends_with(".jpg")
            || lower.ends_with(".jpeg")
            || lower.ends_with(".gif")
            || lower.ends_with(".webp")
    }

    fn trim_text_elements(
        original: &str,
        trimmed: &str,
        elements: Vec<TextElement>,
    ) -> Vec<TextElement> {
        if trimmed.is_empty() || elements.is_empty() {
            return Vec::new();
        }
        let trimmed_start = original.len().saturating_sub(original.trim_start().len());
        let trimmed_end = trimmed_start.saturating_add(trimmed.len());

        elements
            .into_iter()
            .filter_map(|elem| {
                let start = elem.byte_range.start;
                let end = elem.byte_range.end;
                if end <= trimmed_start || start >= trimmed_end {
                    return None;
                }
                let new_start = start.saturating_sub(trimmed_start);
                let new_end = end.saturating_sub(trimmed_start).min(trimmed.len());
                if new_start >= new_end {
                    return None;
                }
                let placeholder = trimmed.get(new_start..new_end).map(str::to_string);
                Some(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ))
            })
            .collect()
    }

    /// Expand large-paste placeholders using element ranges and rebuild other element spans.
    pub(crate) fn expand_pending_pastes(
        text: &str,
        mut elements: Vec<TextElement>,
        pending_pastes: &[(String, String)],
    ) -> (String, Vec<TextElement>) {
        if pending_pastes.is_empty() || elements.is_empty() {
            return (text.to_string(), elements);
        }

        // Stage 1: index pending paste payloads by placeholder for deterministic replacements.
        let mut pending_by_placeholder: HashMap<&str, VecDeque<&str>> = HashMap::new();
        for (placeholder, actual) in pending_pastes {
            pending_by_placeholder
                .entry(placeholder.as_str())
                .or_default()
                .push_back(actual.as_str());
        }

        // Stage 2: walk elements in order and rebuild text/spans in a single pass.
        elements.sort_by_key(|elem| elem.byte_range.start);

        let mut rebuilt = String::with_capacity(text.len());
        let mut rebuilt_elements = Vec::with_capacity(elements.len());
        let mut cursor = 0usize;

        for elem in elements {
            let start = elem.byte_range.start.min(text.len());
            let end = elem.byte_range.end.min(text.len());
            if start > end {
                continue;
            }
            if start > cursor {
                rebuilt.push_str(&text[cursor..start]);
            }
            let elem_text = &text[start..end];
            let placeholder = elem.placeholder(text).map(str::to_string);
            let replacement = placeholder
                .as_deref()
                .and_then(|ph| pending_by_placeholder.get_mut(ph))
                .and_then(VecDeque::pop_front);
            if let Some(actual) = replacement {
                // Stage 3: inline actual paste payloads and drop their placeholder elements.
                rebuilt.push_str(actual);
            } else {
                // Stage 4: keep non-paste elements, updating their byte ranges for the new text.
                let new_start = rebuilt.len();
                rebuilt.push_str(elem_text);
                let new_end = rebuilt.len();
                let placeholder = placeholder.or_else(|| Some(elem_text.to_string()));
                rebuilt_elements.push(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ));
            }
            cursor = end;
        }

        // Stage 5: append any trailing text that followed the last element.
        if cursor < text.len() {
            rebuilt.push_str(&text[cursor..]);
        }

        (rebuilt, rebuilt_elements)
    }

    #[allow(dead_code)]
    pub fn skills(&self) -> Option<&Vec<SkillMetadata>> {
        self.skills.as_ref()
    }

    #[allow(dead_code)]
    pub fn plugins(&self) -> Option<&Vec<PluginCapabilitySummary>> {
        self.plugins.as_ref()
    }

    fn mentions_enabled(&self) -> bool {
        let skills_ready = self
            .skills
            .as_ref()
            .is_some_and(|skills| !skills.is_empty());
        let plugins_ready = self
            .plugins
            .as_ref()
            .is_some_and(|plugins| !plugins.is_empty());
        let connectors_ready = self.connectors_enabled
            && self
                .connectors_snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.connectors.is_empty());
        skills_ready || plugins_ready || connectors_ready
    }

    /// Extract a token prefixed with `prefix` under the cursor, if any.
    ///
    /// The returned string **does not** include the prefix.
    ///
    /// Behavior:
    /// - The cursor may be anywhere *inside* the token (including on the
    ///   leading prefix). It does **not** need to be at the end of the line.
    /// - A token is delimited by ASCII whitespace (space, tab, newline).
    /// - If the cursor is on `prefix` inside an existing token (for example the
    ///   second `@` in `@scope/pkg@latest`), keep treating the surrounding
    ///   whitespace-delimited token as the active token rather than starting a
    ///   new token at that nested prefix.
    /// - If the token under the cursor starts with `prefix`, that token is
    ///   returned without the leading prefix. When `allow_empty` is true, a
    ///   lone prefix character yields `Some(String::new())` to surface hints.
    fn current_prefixed_token(
        textarea: &TextArea,
        prefix: char,
        allow_empty: bool,
    ) -> Option<String> {
        let cursor_offset = textarea.cursor();
        let text = textarea.text();

        // Adjust the provided byte offset to the nearest valid char boundary at or before it.
        let mut safe_cursor = cursor_offset.min(text.len());
        // If we're not on a char boundary, move back to the start of the current char.
        if safe_cursor < text.len() && !text.is_char_boundary(safe_cursor) {
            // Find the last valid boundary <= cursor_offset.
            safe_cursor = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= cursor_offset)
                .last()
                .unwrap_or(0);
        }

        // Split the line around the (now safe) cursor position.
        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        // Detect whether we're on whitespace at the cursor boundary.
        let at_whitespace = if safe_cursor < text.len() {
            text[safe_cursor..]
                .chars()
                .next()
                .map(char::is_whitespace)
                .unwrap_or(false)
        } else {
            false
        };

        // Left candidate: token containing the cursor position.
        let start_left = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);
        let end_left_rel = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_left = safe_cursor + end_left_rel;
        let token_left = if start_left < end_left {
            Some(&text[start_left..end_left])
        } else {
            None
        };

        // Right candidate: token immediately after any whitespace from the cursor.
        let ws_len_right: usize = after_cursor
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum();
        let start_right = safe_cursor + ws_len_right;
        let end_right_rel = text[start_right..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(text.len() - start_right);
        let end_right = start_right + end_right_rel;
        let token_right = if start_right < end_right {
            Some(&text[start_right..end_right])
        } else {
            None
        };

        let prefix_str = prefix.to_string();
        let left_match = token_left.filter(|t| t.starts_with(prefix));
        let right_match = token_right.filter(|t| t.starts_with(prefix));

        let left_prefixed = left_match.map(|t| t[prefix.len_utf8()..].to_string());
        let right_prefixed = right_match.map(|t| t[prefix.len_utf8()..].to_string());

        if at_whitespace {
            if right_prefixed.is_some() {
                return right_prefixed;
            }
            if token_left.is_some_and(|t| t == prefix_str) {
                return allow_empty.then(String::new);
            }
            return left_prefixed;
        }
        if after_cursor.starts_with(prefix) {
            let prefix_starts_token = before_cursor
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
            return if prefix_starts_token {
                right_prefixed.or(left_prefixed)
            } else {
                left_prefixed
            };
        }
        left_prefixed.or(right_prefixed)
    }

    /// Extract the `@token` that the cursor is currently positioned on, if any.
    ///
    /// The returned string **does not** include the leading `@`.
    fn current_at_token(textarea: &TextArea) -> Option<String> {
        Self::current_prefixed_token(textarea, '@', /*allow_empty*/ false)
    }

    fn current_mention_token(&self) -> Option<String> {
        if !self.mentions_enabled() {
            return None;
        }
        Self::current_prefixed_token(&self.textarea, '$', /*allow_empty*/ true)
    }

    /// Replace the active `@token` (the one under the cursor) with `path`.
    ///
    /// The algorithm mirrors `current_at_token` so replacement works no matter
    /// where the cursor is within the token and regardless of how many
    /// `@tokens` exist in the line.
    fn insert_selected_path(&mut self, path: &str) {
        let cursor_offset = self.textarea.cursor();
        let text = self.textarea.text();
        // Clamp to a valid char boundary to avoid panics when slicing.
        let safe_cursor = Self::clamp_to_char_boundary(text, cursor_offset);

        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        // Determine token boundaries.
        let start_idx = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);

        let end_rel_idx = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_idx = safe_cursor + end_rel_idx;

        // If the path contains whitespace, wrap it in double quotes so the
        // local prompt arg parser treats it as a single argument. Avoid adding
        // quotes when the path already contains one to keep behavior simple.
        let needs_quotes = path.chars().any(char::is_whitespace);
        let inserted = if needs_quotes && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path.to_string()
        };

        // Replace just the active `@token` so unrelated text elements, such as
        // large-paste placeholders, remain atomic and can still expand on submit.
        self.textarea
            .replace_range(start_idx..end_idx, &format!("{inserted} "));
        let new_cursor = start_idx.saturating_add(inserted.len()).saturating_add(1);
        self.textarea.set_cursor(new_cursor);
    }

    fn insert_selected_mention(&mut self, insert_text: &str, path: Option<&str>) {
        let cursor_offset = self.textarea.cursor();
        let text = self.textarea.text();
        let safe_cursor = Self::clamp_to_char_boundary(text, cursor_offset);

        let before_cursor = &text[..safe_cursor];
        let after_cursor = &text[safe_cursor..];

        let start_idx = before_cursor
            .char_indices()
            .rfind(|(_, c)| c.is_whitespace())
            .map(|(idx, c)| idx + c.len_utf8())
            .unwrap_or(0);

        let end_rel_idx = after_cursor
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map(|(idx, _)| idx)
            .unwrap_or(after_cursor.len());
        let end_idx = safe_cursor + end_rel_idx;

        // Remove the active token and insert the selected mention as an atomic element.
        self.textarea.replace_range(start_idx..end_idx, "");
        self.textarea.set_cursor(start_idx);
        let id = self.textarea.insert_element(insert_text);

        if let (Some(path), Some(mention)) =
            (path, Self::mention_name_from_insert_text(insert_text))
        {
            self.mention_bindings.insert(
                id,
                ComposerMentionBinding {
                    mention,
                    path: path.to_string(),
                },
            );
        }

        self.textarea.insert_str(" ");
        let new_cursor = start_idx
            .saturating_add(insert_text.len())
            .saturating_add(1);
        self.textarea.set_cursor(new_cursor);
    }

    fn mention_name_from_insert_text(insert_text: &str) -> Option<String> {
        let name = insert_text.strip_prefix('$')?;
        if name.is_empty() {
            return None;
        }
        if name
            .as_bytes()
            .iter()
            .all(|byte| is_mention_name_char(*byte))
        {
            Some(name.to_string())
        } else {
            None
        }
    }

    fn current_mention_elements(&self) -> Vec<(u64, String)> {
        self.textarea
            .text_element_snapshots()
            .into_iter()
            .filter_map(|snapshot| {
                Self::mention_name_from_insert_text(snapshot.text.as_str())
                    .map(|mention| (snapshot.id, mention))
            })
            .collect()
    }

    fn snapshot_mention_bindings(&self) -> Vec<MentionBinding> {
        let mut ordered = Vec::new();
        for (id, mention) in self.current_mention_elements() {
            if let Some(binding) = self.mention_bindings.get(&id)
                && binding.mention == mention
            {
                ordered.push(MentionBinding {
                    mention: binding.mention.clone(),
                    path: binding.path.clone(),
                });
            }
        }
        ordered
    }

    fn bind_mentions_from_snapshot(&mut self, mention_bindings: Vec<MentionBinding>) {
        self.mention_bindings.clear();
        if mention_bindings.is_empty() {
            return;
        }

        let text = self.textarea.text().to_string();
        let mut scan_from = 0usize;
        for binding in mention_bindings {
            let token = format!("${}", binding.mention);
            let Some(range) =
                find_next_mention_token_range(text.as_str(), token.as_str(), scan_from)
            else {
                continue;
            };

            let id = if let Some(id) = self.textarea.add_element_range(range.clone()) {
                Some(id)
            } else {
                self.textarea.element_id_for_exact_range(range.clone())
            };

            if let Some(id) = id {
                self.mention_bindings.insert(
                    id,
                    ComposerMentionBinding {
                        mention: binding.mention,
                        path: binding.path,
                    },
                );
                scan_from = range.end;
            }
        }
    }

    /// Prepare text for submission/queuing. Returns None if submission should be suppressed.
    /// On success, clears pending paste payloads because placeholders have been expanded.
    ///
    /// When `record_history` is true, the final submission is stored for ↑/↓ recall.
    fn prepare_submission_text(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        let mut text = self.textarea.text().to_string();
        let original_input = text.clone();
        let original_text_elements = self.textarea.text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self
            .attached_images
            .iter()
            .map(|img| img.path.clone())
            .collect::<Vec<_>>();
        let original_pending_pastes = self.pending_pastes.clone();
        let mut text_elements = original_text_elements.clone();
        let input_starts_with_space = original_input.starts_with(' ');
        self.recent_submission_mention_bindings.clear();
        self.textarea.set_text_clearing_elements("");

        if !self.pending_pastes.is_empty() {
            // Expand placeholders so element byte ranges stay aligned.
            let (expanded, expanded_elements) =
                Self::expand_pending_pastes(&text, text_elements, &self.pending_pastes);
            text = expanded;
            text_elements = expanded_elements;
        }

        let expanded_input = text.clone();

        // If there is neither text nor attachments, suppress submission entirely.
        text = text.trim().to_string();
        text_elements = Self::trim_text_elements(&expanded_input, &text, text_elements);

        if self.slash_commands_enabled()
            && let Some((name, _rest, _rest_offset)) = parse_slash_name(&text)
        {
            let treat_as_plain_text = input_starts_with_space || name.contains('/');
            if !treat_as_plain_text {
                let is_builtin =
                    slash_commands::find_builtin_command(name, self.builtin_command_flags())
                        .is_some();
                if !is_builtin {
                    let message = format!(
                        r#"Unrecognized command '/{name}'. Type "/" for a list of supported commands."#
                    );
                    self.app_event_tx
                        .send(AppEvent::StatusMessageChanged { message });
                    self.set_text_content_with_mention_bindings(
                        original_input.clone(),
                        original_text_elements,
                        original_local_image_paths,
                        original_mention_bindings,
                    );
                    self.pending_pastes.clone_from(&original_pending_pastes);
                    self.textarea.set_cursor(original_input.len());
                    return None;
                }
            }
        }

        let actual_chars = text.chars().count();
        if actual_chars > INPUT_TEXT_LIMIT {
            let message = user_input_too_large_message(actual_chars);
            self.app_event_tx
                .send(AppEvent::StatusMessageChanged { message });
            self.set_text_content_with_mention_bindings(
                original_input.clone(),
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.pending_pastes.clone_from(&original_pending_pastes);
            self.textarea.set_cursor(original_input.len());
            return None;
        }
        self.prune_attached_images_for_submission(&text, &text_elements);
        if text.is_empty() && self.attached_images.is_empty() && self.remote_image_urls.is_empty() {
            return None;
        }
        self.recent_submission_mention_bindings = original_mention_bindings.clone();
        if record_history
            && (!text.is_empty()
                || !self.attached_images.is_empty()
                || !self.remote_image_urls.is_empty())
        {
            let local_image_paths = self
                .attached_images
                .iter()
                .map(|img| img.path.clone())
                .collect();
            self.history.record_local_submission(HistoryEntry {
                text: text.clone(),
                text_elements: text_elements.clone(),
                local_image_paths,
                remote_image_urls: self.remote_image_urls.clone(),
                mention_bindings: original_mention_bindings,
                pending_pastes: Vec::new(),
            });
        }
        self.pending_pastes.clear();
        Some((text, text_elements))
    }

    /// Common logic for handling message submission/queuing.
    /// Returns the appropriate InputResult based on `should_queue`.
    fn handle_submission(&mut self, should_queue: bool) -> (InputResult, bool) {
        self.handle_submission_with_time(should_queue, Instant::now())
    }

    fn handle_submission_with_time(
        &mut self,
        should_queue: bool,
        now: Instant,
    ) -> (InputResult, bool) {
        // If the first line is a bare built-in slash command (no args),
        // dispatch it even when the slash popup isn't visible. This preserves
        // the workflow: type a prefix ("/di"), press Tab to complete to
        // "/diff ", then press Enter/Ctrl+Shift+Q to run it. Tab moves the cursor beyond
        // the '/name' token and our caret-based heuristic hides the popup,
        // but Enter/Ctrl+Shift+Q should still dispatch the command rather than submit
        // literal text.
        if let Some(result) = self.try_dispatch_bare_slash_command() {
            return (result, true);
        }

        // If we're in a paste-like burst capture, treat Enter/Ctrl+Shift+Q as part of the burst
        // and accumulate it rather than submitting or inserting immediately.
        // Do not treat as paste inside a slash-command context.
        let in_slash_context = self.slash_commands_enabled()
            && (matches!(self.active_popup, ActivePopup::Command(_))
                || self
                    .textarea
                    .text()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .starts_with('/'));
        if !self.disable_paste_burst
            && self.paste_burst.is_active()
            && !in_slash_context
            && self.paste_burst.append_newline_if_active(now)
        {
            self.promote_paste_burst_to_large_placeholder_if_needed(now);
            return (InputResult::None, true);
        }
        if !in_slash_context && self.append_to_active_large_paste('\n', now) {
            return (InputResult::None, true);
        }

        // During a paste-like burst, treat Enter/Ctrl+Shift+Q as a newline instead of submit.
        if !in_slash_context
            && !self.disable_paste_burst
            && self
                .paste_burst
                .newline_should_insert_instead_of_submit(now)
        {
            self.textarea.insert_str("\n");
            self.paste_burst.extend_window(now);
            return (InputResult::None, true);
        }

        if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
            self.handle_paste(pasted);
        }

        let original_input = self.textarea.text().to_string();
        let original_text_elements = self.textarea.text_elements();
        let original_mention_bindings = self.snapshot_mention_bindings();
        let original_local_image_paths = self
            .attached_images
            .iter()
            .map(|img| img.path.clone())
            .collect::<Vec<_>>();
        let original_pending_pastes = self.pending_pastes.clone();
        if let Some(result) = self.try_dispatch_slash_command_with_args() {
            return (result, true);
        }

        if let Some((text, text_elements)) =
            self.prepare_submission_text(/*record_history*/ true)
        {
            if should_queue {
                (
                    InputResult::Queued {
                        text,
                        text_elements,
                    },
                    true,
                )
            } else {
                // Do not clear attached_images here; ChatWidget drains them via take_recent_submission_images().
                (
                    InputResult::Submitted {
                        text,
                        text_elements,
                    },
                    true,
                )
            }
        } else {
            // Restore text if submission was suppressed.
            self.set_text_content_with_mention_bindings(
                original_input,
                original_text_elements,
                original_local_image_paths,
                original_mention_bindings,
            );
            self.pending_pastes = original_pending_pastes;
            (InputResult::None, true)
        }
    }

    /// Check if the first line is a bare slash command (no args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    fn try_dispatch_bare_slash_command(&mut self) -> Option<InputResult> {
        if !self.slash_commands_enabled() {
            return None;
        }
        let first_line = self.textarea.text().lines().next().unwrap_or("");
        if let Some((name, rest, _rest_offset)) = parse_slash_name(first_line)
            && rest.is_empty()
            && let Some(cmd) =
                slash_commands::find_builtin_command(name, self.builtin_command_flags())
        {
            if self.reject_slash_command_if_unavailable(cmd) {
                return Some(InputResult::None);
            }
            self.textarea.set_text_clearing_elements("");
            Some(InputResult::Command(cmd))
        } else {
            None
        }
    }

    /// Check if the input is a slash command with args (e.g., /review args) and dispatch it.
    /// Returns Some(InputResult) if a command was dispatched, None otherwise.
    fn try_dispatch_slash_command_with_args(&mut self) -> Option<InputResult> {
        if !self.slash_commands_enabled() {
            return None;
        }
        let text = self.textarea.text().to_string();
        if text.starts_with(' ') {
            return None;
        }

        let (name, rest, rest_offset) = parse_slash_name(&text)?;
        if rest.is_empty() || name.contains('/') {
            return None;
        }

        let cmd = slash_commands::find_builtin_command(name, self.builtin_command_flags())?;

        if !cmd.supports_inline_args() {
            return None;
        }
        if self.reject_slash_command_if_unavailable(cmd) {
            return Some(InputResult::None);
        }

        let mut args_elements =
            Self::slash_command_args_elements(rest, rest_offset, &self.textarea.text_elements());
        let trimmed_rest = rest.trim();
        args_elements = Self::trim_text_elements(rest, trimmed_rest, args_elements);
        Some(InputResult::CommandWithArgs(
            cmd,
            trimmed_rest.to_string(),
            args_elements,
        ))
    }

    /// Expand pending placeholders and extract normalized inline-command args.
    ///
    /// Inline-arg commands are initially dispatched using the raw draft so command rejection does
    /// not consume user input. Once a command is accepted, this helper performs the usual
    /// submission preparation (paste expansion, element trimming) and rebases element ranges from
    /// full-text offsets to command-arg offsets.
    #[allow(dead_code)]
    pub(crate) fn prepare_inline_args_submission(
        &mut self,
        record_history: bool,
    ) -> Option<(String, Vec<TextElement>)> {
        let (prepared_text, prepared_elements) = self.prepare_submission_text(record_history)?;
        let (_, prepared_rest, prepared_rest_offset) = parse_slash_name(&prepared_text)?;
        let mut args_elements = Self::slash_command_args_elements(
            prepared_rest,
            prepared_rest_offset,
            &prepared_elements,
        );
        let trimmed_rest = prepared_rest.trim();
        args_elements = Self::trim_text_elements(prepared_rest, trimmed_rest, args_elements);
        Some((trimmed_rest.to_string(), args_elements))
    }

    fn reject_slash_command_if_unavailable(&self, cmd: SlashCommand) -> bool {
        if !self.is_task_running || cmd.available_during_task() {
            return false;
        }
        let message = format!(
            "'/{}' is disabled while a task is in progress.",
            cmd.command()
        );
        self.app_event_tx
            .send(AppEvent::StatusMessageChanged { message });
        true
    }

    /// Translate full-text element ranges into command-argument ranges.
    ///
    /// `rest_offset` is the byte offset where `rest` begins in the full text.
    fn slash_command_args_elements(
        rest: &str,
        rest_offset: usize,
        text_elements: &[TextElement],
    ) -> Vec<TextElement> {
        if rest.is_empty() || text_elements.is_empty() {
            return Vec::new();
        }
        text_elements
            .iter()
            .filter_map(|elem| {
                if elem.byte_range.end <= rest_offset {
                    return None;
                }
                let start = elem.byte_range.start.saturating_sub(rest_offset);
                let mut end = elem.byte_range.end.saturating_sub(rest_offset);
                if start >= rest.len() {
                    return None;
                }
                end = end.min(rest.len());
                (start < end).then_some(elem.map_range(|_| ByteRange { start, end }))
            })
            .collect()
    }

    fn remote_images_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.remote_image_urls
            .iter()
            .enumerate()
            .map(|(idx, _)| {
                let label = local_image_label_text(idx + 1);
                if self.selected_remote_image_index == Some(idx) {
                    label.cyan().reversed().into()
                } else {
                    label.cyan().into()
                }
            })
            .collect()
    }

    fn clear_remote_image_selection(&mut self) {
        self.selected_remote_image_index = None;
    }

    fn remove_selected_remote_image(&mut self, selected_index: usize) {
        if selected_index >= self.remote_image_urls.len() {
            self.clear_remote_image_selection();
            return;
        }
        self.remote_image_urls.remove(selected_index);
        self.selected_remote_image_index = if self.remote_image_urls.is_empty() {
            None
        } else {
            Some(selected_index.min(self.remote_image_urls.len() - 1))
        };
        self.relabel_attached_images_and_update_placeholders();
        self.sync_popups();
    }

    fn handle_remote_image_selection_key(
        &mut self,
        key_event: &KeyEvent,
    ) -> Option<(InputResult, bool)> {
        if self.remote_image_urls.is_empty()
            || key_event.modifiers != KeyModifiers::NONE
            || key_event.kind != KeyEventKind::Press
        {
            return None;
        }

        match key_event.code {
            KeyCode::Up => {
                if let Some(selected) = self.selected_remote_image_index {
                    self.selected_remote_image_index = Some(selected.saturating_sub(1));
                    Some((InputResult::None, true))
                } else if self.textarea.cursor() == 0 {
                    self.selected_remote_image_index = Some(self.remote_image_urls.len() - 1);
                    Some((InputResult::None, true))
                } else {
                    None
                }
            }
            KeyCode::Down => {
                if let Some(selected) = self.selected_remote_image_index {
                    if selected + 1 < self.remote_image_urls.len() {
                        self.selected_remote_image_index = Some(selected + 1);
                    } else {
                        self.clear_remote_image_selection();
                    }
                    Some((InputResult::None, true))
                } else {
                    None
                }
            }
            KeyCode::Delete | KeyCode::Backspace => {
                if let Some(selected) = self.selected_remote_image_index {
                    self.remove_selected_remote_image(selected);
                    Some((InputResult::None, true))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Handle key event when no popup is visible.
    fn handle_key_event_without_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if let Some((result, redraw)) = self.handle_remote_image_selection_key(&key_event) {
            return (result, redraw);
        }
        if self.selected_remote_image_index.is_some() {
            self.clear_remote_image_selection();
        }
        if self.handle_shortcut_overlay_key(&key_event) {
            return (InputResult::None, true);
        }
        if key_event.code == KeyCode::Esc {
            if self.is_empty() {
                let next_mode = esc_hint_mode(self.footer_mode, self.is_task_running);
                if next_mode != self.footer_mode {
                    self.footer_mode = next_mode;
                    return (InputResult::None, true);
                }
            }
        } else {
            self.footer_mode = reset_mode_after_activity(self.footer_mode);
        }
        match key_event {
            KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: crossterm::event::KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                ..
            } if self.is_empty() => (InputResult::None, false),
            // -------------------------------------------------------------
            // History navigation (Up / Down) – only when the composer is not
            // empty or when the cursor is at the correct position, to avoid
            // interfering with normal cursor movement.
            // -------------------------------------------------------------
            KeyEvent {
                code: KeyCode::Up | KeyCode::Down,
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('p') | KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if self
                    .history
                    .should_handle_navigation(self.textarea.text(), self.textarea.cursor())
                {
                    let replace_entry = match key_event.code {
                        KeyCode::Up => self.history.navigate_up(&self.app_event_tx),
                        KeyCode::Down => self.history.navigate_down(&self.app_event_tx),
                        KeyCode::Char('p') => self.history.navigate_up(&self.app_event_tx),
                        KeyCode::Char('n') => self.history.navigate_down(&self.app_event_tx),
                        _ => unreachable!(),
                    };
                    if let Some(entry) = replace_entry {
                        self.apply_history_entry(entry);
                        return (InputResult::None, true);
                    }
                }
                self.handle_input_basic(key_event)
            }
            KeyEvent {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                ..
            } if !self.is_bang_shell_command() => self.handle_submission(self.is_task_running),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.handle_submission(/*should_queue*/ false),
            input => self.handle_input_basic(input),
        }
    }

    fn is_bang_shell_command(&self) -> bool {
        self.textarea.text().trim_start().starts_with('!')
    }

    /// Applies any due `PasteBurst` flush at time `now`.
    ///
    /// Converts [`PasteBurst::flush_if_due`] results into concrete textarea mutations.
    ///
    /// Callers:
    ///
    /// - UI ticks via [`ChatComposer::flush_paste_burst_if_due`], so held first-chars can render.
    /// - Input handling via [`ChatComposer::handle_input_basic`], so a due burst does not lag.
    fn handle_paste_burst_flush(&mut self, now: Instant) -> bool {
        if self.finalize_active_large_paste_if_idle(now) {
            return true;
        }
        match self.paste_burst.flush_if_due(now) {
            FlushResult::Paste(pasted) => {
                self.handle_paste(pasted);
                true
            }
            FlushResult::Typed(ch) => {
                self.textarea.insert_str(ch.to_string().as_str());
                self.sync_popups();
                true
            }
            FlushResult::None => false,
        }
    }

    /// Handles keys that mutate the textarea, including paste-burst detection.
    ///
    /// Acts as the lowest-level keypath for keys that mutate the textarea. It is also where plain
    /// character streams are converted into explicit paste operations on terminals that do not
    /// reliably provide bracketed paste.
    ///
    /// Ordering is important:
    ///
    /// - Always flush any *due* paste burst first so buffered text does not lag behind unrelated
    ///   edits.
    /// - Then handle the incoming key, intercepting only "plain" (no Ctrl/Alt) char input.
    /// - For non-plain keys, flush via `flush_before_modified_input()` before applying the key;
    ///   otherwise `clear_window_after_non_char()` can leave buffered text waiting without a
    ///   timestamp to time out against.
    fn handle_input_basic(&mut self, input: KeyEvent) -> (InputResult, bool) {
        // Ignore key releases here to avoid treating them as additional input
        // (e.g., appending the same character twice via paste-burst logic).
        if !matches!(input.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return (InputResult::None, false);
        }

        self.handle_input_basic_with_time(input, Instant::now())
    }

    fn handle_input_basic_with_time(
        &mut self,
        input: KeyEvent,
        now: Instant,
    ) -> (InputResult, bool) {
        // If we have a buffered non-bracketed paste burst and enough time has
        // elapsed since the last char, flush it before handling a new input.
        self.handle_paste_burst_flush(now);

        if !matches!(input.code, KeyCode::Esc) {
            self.footer_mode = reset_mode_after_activity(self.footer_mode);
        }

        // If we're capturing a burst and receive Enter, accumulate it instead of inserting.
        if matches!(input.code, KeyCode::Enter)
            && !self.disable_paste_burst
            && self.paste_burst.is_active()
            && self.paste_burst.append_newline_if_active(now)
        {
            self.promote_paste_burst_to_large_placeholder_if_needed(now);
            return (InputResult::None, true);
        }

        // Intercept plain Char inputs to optionally accumulate into a burst buffer.
        //
        // This is intentionally limited to "plain" (no Ctrl/Alt) chars so shortcuts keep their
        // normal semantics, and so we can aggressively flush/clear any burst state when non-char
        // keys are pressed.
        if let KeyEvent {
            code: KeyCode::Char(ch),
            modifiers,
            ..
        } = input
        {
            let has_ctrl_or_alt = has_ctrl_or_alt(modifiers);
            if !has_ctrl_or_alt && self.append_to_active_large_paste(ch, now) {
                return (InputResult::None, true);
            }
            if !has_ctrl_or_alt && !self.disable_paste_burst {
                // Non-ASCII characters (e.g., from IMEs) can arrive in quick bursts, so avoid
                // holding the first char while still allowing burst detection for paste input.
                if !ch.is_ascii() {
                    return self.handle_non_ascii_char(input, now);
                }

                match self.paste_burst.on_plain_char(ch, now) {
                    CharDecision::BufferAppend => {
                        self.paste_burst.append_char_to_buffer(ch, now);
                        self.promote_paste_burst_to_large_placeholder_if_needed(now);
                        return (InputResult::None, true);
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        let cur = self.textarea.cursor();
                        let txt = self.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) =
                            self.paste_burst
                                .decide_begin_buffer(now, before, retro_chars as usize)
                        {
                            if !grab.grabbed.is_empty() {
                                self.textarea.replace_range(grab.start_byte..safe_cur, "");
                            }
                            self.paste_burst.append_char_to_buffer(ch, now);
                            self.promote_paste_burst_to_large_placeholder_if_needed(now);
                            return (InputResult::None, true);
                        }
                        // If decide_begin_buffer opted not to start buffering,
                        // fall through to normal insertion below.
                    }
                    CharDecision::BeginBufferFromPending => {
                        // First char was held; now append the current one.
                        self.paste_burst.append_char_to_buffer(ch, now);
                        self.promote_paste_burst_to_large_placeholder_if_needed(now);
                        return (InputResult::None, true);
                    }
                    CharDecision::RetainFirstChar => {
                        // Keep the first fast char pending momentarily.
                        return (InputResult::None, true);
                    }
                }
            }
            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.handle_paste(pasted);
            }
        }

        // Flush any buffered burst before applying a non-char input (arrow keys, etc).
        //
        // `clear_window_after_non_char()` clears `last_plain_char_time`. If we cleared that while
        // `PasteBurst.buffer` is non-empty, `flush_if_due()` would no longer have a timestamp to
        // time out against, and the buffered paste could remain stuck until another plain char
        // arrives.
        if !matches!(input.code, KeyCode::Char(_) | KeyCode::Enter)
            && let Some(pasted) = self.paste_burst.flush_before_modified_input()
        {
            self.handle_paste(pasted);
        }
        // For non-char inputs (or after flushing), handle normally.
        // Track element removals so we can drop any corresponding placeholders without scanning
        // the full text. (Placeholders are atomic elements; when deleted, the element disappears.)
        let elements_before = if self.pending_pastes.is_empty()
            && self.attached_images.is_empty()
            && self.remote_image_urls.is_empty()
        {
            None
        } else {
            Some(self.textarea.element_payloads())
        };

        self.textarea.input(input);

        if let Some(elements_before) = elements_before {
            self.reconcile_deleted_elements(elements_before);
        }

        // Update paste-burst heuristic for plain Char (no Ctrl/Alt) events.
        let crossterm::event::KeyEvent {
            code, modifiers, ..
        } = input;
        match code {
            KeyCode::Char(_) => {
                let has_ctrl_or_alt = has_ctrl_or_alt(modifiers);
                if has_ctrl_or_alt {
                    self.paste_burst.clear_window_after_non_char();
                }
            }
            KeyCode::Enter => {
                // Keep burst window alive (supports blank lines in paste).
            }
            _ => {
                // Other keys: clear burst window (buffer should have been flushed above if needed).
                self.paste_burst.clear_window_after_non_char();
            }
        }

        (InputResult::None, true)
    }

    fn reconcile_deleted_elements(&mut self, elements_before: Vec<String>) {
        let elements_after: HashSet<String> =
            self.textarea.element_payloads().into_iter().collect();

        let mut removed_any_image = false;
        for removed in elements_before
            .into_iter()
            .filter(|payload| !elements_after.contains(payload))
        {
            self.pending_pastes.retain(|(ph, _)| ph != &removed);

            if let Some(idx) = self
                .attached_images
                .iter()
                .position(|img| img.placeholder == removed)
            {
                self.attached_images.remove(idx);
                removed_any_image = true;
            }
        }

        if removed_any_image {
            self.relabel_attached_images_and_update_placeholders();
        }
    }

    fn relabel_attached_images_and_update_placeholders(&mut self) {
        for idx in 0..self.attached_images.len() {
            let expected = local_image_label_text(self.remote_image_urls.len() + idx + 1);
            let current = self.attached_images[idx].placeholder.clone();
            if current == expected {
                continue;
            }

            self.attached_images[idx].placeholder = expected.clone();
            let _renamed = self.textarea.replace_element_payload(&current, &expected);
        }
    }

    fn handle_shortcut_overlay_key(&mut self, key_event: &KeyEvent) -> bool {
        if key_event.kind != KeyEventKind::Press {
            return false;
        }

        let toggles = matches!(key_event.code, KeyCode::Char('?'))
            && !has_ctrl_or_alt(key_event.modifiers)
            && self.is_empty()
            && !self.is_in_paste_burst();

        if !toggles {
            return false;
        }

        let next = toggle_shortcut_mode(
            self.footer_mode,
            self.quit_shortcut_hint_visible(),
            self.is_empty(),
        );
        let changed = next != self.footer_mode;
        self.footer_mode = next;
        changed
    }

    fn footer_props(&self) -> FooterProps {
        let mode = self.footer_mode();
        let is_wsl = {
            #[cfg(target_os = "linux")]
            {
                mode == FooterMode::ShortcutOverlay && crate::clipboard_paste::is_probably_wsl()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        };

        FooterProps {
            mode,
            esc_backtrack_hint: self.esc_backtrack_hint,
            use_shift_enter_hint: self.use_shift_enter_hint,
            is_task_running: self.is_task_running,
            quit_shortcut_key: self.quit_shortcut_key,
            collaboration_modes_enabled: self.collaboration_modes_enabled,
            is_wsl,
            context_window_percent: self.context_window_percent,
            context_window_used_tokens: self.context_window_used_tokens,
            status_line_value: self.status_line_value.clone(),
            status_line_enabled: self.status_line_enabled,
            active_agent_label: self.active_agent_label.clone(),
        }
    }

    /// Resolve the effective footer mode via a small priority waterfall.
    ///
    /// The base mode is derived solely from whether the composer is empty:
    /// `ComposerEmpty` iff empty, otherwise `ComposerHasDraft`. Transient
    /// modes (Esc hint, overlay, quit reminder) can override that base when
    /// their conditions are active.
    fn footer_mode(&self) -> FooterMode {
        let base_mode = if self.is_empty() {
            FooterMode::ComposerEmpty
        } else {
            FooterMode::ComposerHasDraft
        };

        match self.footer_mode {
            FooterMode::EscHint => FooterMode::EscHint,
            FooterMode::ShortcutOverlay => FooterMode::ShortcutOverlay,
            FooterMode::QuitShortcutReminder if self.quit_shortcut_hint_visible() => {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
                if self.quit_shortcut_hint_visible() =>
            {
                FooterMode::QuitShortcutReminder
            }
            FooterMode::QuitShortcutReminder => base_mode,
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => base_mode,
        }
    }

    fn custom_footer_height(&self) -> Option<u16> {
        if self.footer_flash_visible() {
            return Some(1);
        }
        self.footer_hint_override
            .as_ref()
            .map(|items| if items.is_empty() { 0 } else { 1 })
    }

    pub(crate) fn sync_popups(&mut self) {
        self.sync_slash_command_elements();
        if !self.popups_enabled() {
            self.active_popup = ActivePopup::None;
            return;
        }
        let file_token = Self::current_at_token(&self.textarea);
        let browsing_history = self
            .history
            .should_handle_navigation(self.textarea.text(), self.textarea.cursor());
        // When browsing input history (shell-style Up/Down recall), skip all popup
        // synchronization so nothing steals focus from continued history navigation.
        if browsing_history {
            if self.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.current_file_query = None;
            }
            self.active_popup = ActivePopup::None;
            return;
        }
        let mention_token = self.current_mention_token();

        let allow_command_popup =
            self.slash_commands_enabled() && file_token.is_none() && mention_token.is_none();
        self.sync_command_popup(allow_command_popup);

        if matches!(self.active_popup, ActivePopup::Command(_)) {
            if self.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.current_file_query = None;
            }
            self.dismissed_file_popup_token = None;
            self.dismissed_mention_popup_token = None;
            return;
        }

        if let Some(token) = mention_token {
            if self.current_file_query.is_some() {
                self.app_event_tx
                    .send(AppEvent::StartFileSearch(String::new()));
                self.current_file_query = None;
            }
            self.sync_mention_popup(token);
            return;
        }
        self.dismissed_mention_popup_token = None;

        if let Some(token) = file_token
            && self.file_search_enabled()
        {
            self.sync_file_search_popup(token);
            return;
        }

        if self.current_file_query.is_some() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
            self.current_file_query = None;
        }
        self.dismissed_file_popup_token = None;
        if matches!(
            self.active_popup,
            ActivePopup::File(_) | ActivePopup::Skill(_)
        ) {
            self.active_popup = ActivePopup::None;
        }
    }

    /// Keep slash command elements aligned with the current first line.
    fn sync_slash_command_elements(&mut self) {
        if !self.slash_commands_enabled() {
            return;
        }
        let text = self.textarea.text();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        let first_line = &text[..first_line_end];
        let desired_range = self.slash_command_element_range(first_line);
        // Slash commands are only valid at byte 0 of the first line.
        // Any slash-shaped element not matching the current desired prefix is stale.
        let mut has_desired = false;
        let mut stale_ranges = Vec::new();
        for elem in self.textarea.text_elements() {
            let Some(payload) = elem.placeholder(text) else {
                continue;
            };
            if payload.strip_prefix('/').is_none() {
                continue;
            }
            let range = elem.byte_range.start..elem.byte_range.end;
            if desired_range.as_ref() == Some(&range) {
                has_desired = true;
            } else {
                stale_ranges.push(range);
            }
        }

        for range in stale_ranges {
            self.textarea.remove_element_range(range);
        }

        if let Some(range) = desired_range
            && !has_desired
        {
            self.textarea.add_element_range(range);
        }
    }

    fn slash_command_element_range(&self, first_line: &str) -> Option<Range<usize>> {
        let (name, _rest, _rest_offset) = parse_slash_name(first_line)?;
        if name.contains('/') {
            return None;
        }
        let element_end = 1 + name.len();
        let has_space_after = first_line
            .get(element_end..)
            .and_then(|tail| tail.chars().next())
            .is_some_and(char::is_whitespace);
        if !has_space_after {
            return None;
        }
        if self.is_known_slash_name(name) {
            Some(0..element_end)
        } else {
            None
        }
    }

    fn is_known_slash_name(&self, name: &str) -> bool {
        slash_commands::find_builtin_command(name, self.builtin_command_flags()).is_some()
    }

    /// If the cursor is currently within a slash command on the first line,
    /// extract the command name and the rest of the line after it.
    /// Returns None if the cursor is outside a slash command.
    fn slash_command_under_cursor(first_line: &str, cursor: usize) -> Option<(&str, &str)> {
        if !first_line.starts_with('/') {
            return None;
        }

        let name_start = 1usize;
        let name_end = first_line[name_start..]
            .find(char::is_whitespace)
            .map(|idx| name_start + idx)
            .unwrap_or_else(|| first_line.len());

        if cursor > name_end {
            return None;
        }

        let name = &first_line[name_start..name_end];
        let rest_start = first_line[name_end..]
            .find(|c: char| !c.is_whitespace())
            .map(|idx| name_end + idx)
            .unwrap_or(name_end);
        let rest = &first_line[rest_start..];

        Some((name, rest))
    }

    /// Heuristic for whether the typed slash command looks like a valid
    /// prefix for any known built-in command.
    /// Empty names only count when there is no extra content after the '/'.
    fn looks_like_slash_prefix(&self, name: &str, rest_after_name: &str) -> bool {
        if !self.slash_commands_enabled() {
            return false;
        }
        if name.is_empty() {
            return rest_after_name.is_empty();
        }

        slash_commands::has_builtin_prefix(name, self.builtin_command_flags())
    }

    /// Synchronize `self.command_popup` with the current text in the
    /// textarea. This must be called after every modification that can change
    /// the text so the popup is shown/updated/hidden as appropriate.
    fn sync_command_popup(&mut self, allow: bool) {
        if !allow {
            if matches!(self.active_popup, ActivePopup::Command(_)) {
                self.active_popup = ActivePopup::None;
            }
            return;
        }
        // Determine whether the caret is inside the initial '/name' token on the first line.
        let text = self.textarea.text();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        let first_line = &text[..first_line_end];
        let cursor = self.textarea.cursor();
        let caret_on_first_line = cursor <= first_line_end;

        let is_editing_slash_command_name = caret_on_first_line
            && Self::slash_command_under_cursor(first_line, cursor)
                .is_some_and(|(name, rest)| self.looks_like_slash_prefix(name, rest));

        // If the cursor is currently positioned within an `@token`, prefer the
        // file-search popup over the slash popup so users can insert a file path
        // as an argument to the command (e.g., "/review @docs/...").
        if Self::current_at_token(&self.textarea).is_some() {
            if matches!(self.active_popup, ActivePopup::Command(_)) {
                self.active_popup = ActivePopup::None;
            }
            return;
        }
        match &mut self.active_popup {
            ActivePopup::Command(popup) => {
                if is_editing_slash_command_name {
                    popup.on_composer_text_change(first_line.to_string());
                } else {
                    self.active_popup = ActivePopup::None;
                }
            }
            _ => {
                if is_editing_slash_command_name {
                    let collaboration_modes_enabled = self.collaboration_modes_enabled;
                    let connectors_enabled = self.connectors_enabled;
                    let plugins_command_enabled = self.plugins_command_enabled;
                    let fast_command_enabled = self.fast_command_enabled;
                    let personality_command_enabled = self.personality_command_enabled;
                    let realtime_conversation_enabled = self.realtime_conversation_enabled;
                    let audio_device_selection_enabled = self.audio_device_selection_enabled;
                    let mut command_popup = CommandPopup::new(
                        CommandPopupFlags {
                            collaboration_modes_enabled,
                            connectors_enabled,
                            plugins_command_enabled,
                            fast_command_enabled,
                            personality_command_enabled,
                            realtime_conversation_enabled,
                            audio_device_selection_enabled,
                            windows_degraded_sandbox_active: self.windows_degraded_sandbox_active,
                        },
                        self.accent_color,
                    );
                    command_popup.on_composer_text_change(first_line.to_string());
                    self.active_popup = ActivePopup::Command(command_popup);
                }
            }
        }
    }

    /// Synchronize `self.file_search_popup` with the current text in the textarea.
    /// Note this is only called when self.active_popup is NOT Command.
    fn sync_file_search_popup(&mut self, query: String) {
        // If user dismissed popup for this exact query, don't reopen until text changes.
        if self.dismissed_file_popup_token.as_ref() == Some(&query) {
            return;
        }

        if query.is_empty() {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(String::new()));
        } else {
            self.app_event_tx
                .send(AppEvent::StartFileSearch(query.clone()));
        }

        match &mut self.active_popup {
            ActivePopup::File(popup) => {
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
            }
            _ => {
                let mut popup = FileSearchPopup::new(self.accent_color);
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
                self.active_popup = ActivePopup::File(popup);
            }
        }

        if query.is_empty() {
            self.current_file_query = None;
        } else {
            self.current_file_query = Some(query);
        }
        self.dismissed_file_popup_token = None;
    }

    fn sync_mention_popup(&mut self, query: String) {
        if self.dismissed_mention_popup_token.as_ref() == Some(&query) {
            return;
        }

        let mentions = self.mention_items();
        if mentions.is_empty() {
            self.active_popup = ActivePopup::None;
            return;
        }

        {
            let mut popup = SkillPopup::new(mentions, self.accent_color);
            popup.set_query(&query);
            self.active_popup = ActivePopup::Skill(popup);
        }
    }

    fn mention_items(&self) -> Vec<MentionItem> {
        let mut mentions = Vec::new();
        if let Some(skills) = self.skills.as_ref() {
            for skill in skills {
                let display_name = skill_display_name(skill).to_string();
                let description = skill_description(skill);
                let skill_name = skill.name.clone();
                let search_terms = if display_name == skill.name {
                    vec![skill_name.clone()]
                } else {
                    vec![skill_name.clone(), display_name.clone()]
                };
                mentions.push(MentionItem {
                    display_name,
                    description,
                    insert_text: format!("${skill_name}"),
                    search_terms,
                    path: Some(skill.path_to_skills_md.to_string_lossy().into_owned()),
                    category_tag: Some("[Skill]".to_string()),
                    sort_rank: 1,
                });
            }
        }

        mentions
    }

    #[allow(dead_code)]
    fn set_has_focus(&mut self, has_focus: bool) {
        self.has_focus = has_focus;
    }

    #[allow(dead_code)]
    pub(crate) fn set_input_enabled(&mut self, enabled: bool, placeholder: Option<String>) {
        self.input_enabled = enabled;
        self.input_disabled_placeholder = if enabled { None } else { placeholder };

        // Avoid leaving interactive popups open while input is blocked.
        if !enabled && !matches!(self.active_popup, ActivePopup::None) {
            self.active_popup = ActivePopup::None;
        }
    }

    #[allow(dead_code)]
    pub fn set_task_running(&mut self, running: bool) {
        self.is_task_running = running;
    }

    #[allow(dead_code)]
    pub(crate) fn set_context_window(&mut self, percent: Option<i64>, used_tokens: Option<i64>) {
        if self.context_window_percent == percent && self.context_window_used_tokens == used_tokens
        {
            return;
        }
        self.context_window_percent = percent;
        self.context_window_used_tokens = used_tokens;
    }

    #[allow(dead_code)]
    pub(crate) fn set_esc_backtrack_hint(&mut self, show: bool) {
        self.esc_backtrack_hint = show;
        if show {
            self.footer_mode = esc_hint_mode(self.footer_mode, self.is_task_running);
        } else {
            self.footer_mode = reset_mode_after_activity(self.footer_mode);
        }
    }

    pub(crate) fn set_status_line(&mut self, status_line: Option<Line<'static>>) -> bool {
        if self.status_line_value == status_line {
            return false;
        }
        self.status_line_value = status_line;
        true
    }

    pub(crate) fn set_status_line_enabled(&mut self, enabled: bool) -> bool {
        if self.status_line_enabled == enabled {
            return false;
        }
        self.status_line_enabled = enabled;
        true
    }

    #[allow(dead_code)]
    /// Replaces the contextual footer label for the currently viewed agent.
    ///
    /// Returning `false` means the value was unchanged, so callers can skip redraw work. This
    /// field is intentionally just cached presentation state; `ChatComposer` does not infer which
    /// thread is active on its own.
    pub(crate) fn set_active_agent_label(&mut self, active_agent_label: Option<String>) -> bool {
        if self.active_agent_label == active_agent_label {
            return false;
        }
        self.active_agent_label = active_agent_label;
        true
    }
}

#[cfg(not(target_os = "linux"))]
impl ChatComposer {
    #[allow(dead_code)]
    pub fn update_recording_meter_in_place(&mut self, id: &str, text: &str) -> bool {
        self.textarea.update_named_element_by_id(id, text)
    }

    #[allow(dead_code)]
    pub fn insert_recording_meter_placeholder(&mut self, text: &str) -> String {
        let id = self.next_id();
        self.textarea.insert_named_element(text, id.clone());
        id
    }

    #[allow(dead_code)]
    pub fn remove_recording_meter_placeholder(&mut self, id: &str) {
        let _ = self.textarea.replace_element_by_id(id, "");
    }
}

fn skill_display_name(skill: &SkillMetadata) -> &str {
    skill
        .interface
        .as_ref()
        .and_then(|interface| interface.display_name.as_deref())
        .unwrap_or(&skill.name)
}

fn skill_description(skill: &SkillMetadata) -> Option<String> {
    let description = skill
        .interface
        .as_ref()
        .and_then(|interface| interface.short_description.as_deref())
        .or(skill.short_description.as_deref())
        .unwrap_or(&skill.description);
    let trimmed = description.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn local_image_label_text(index: usize) -> String {
    format!("[Image #{index}]")
}

fn is_mention_name_char(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
}

fn find_next_mention_token_range(text: &str, token: &str, from: usize) -> Option<Range<usize>> {
    if token.is_empty() || from >= text.len() {
        return None;
    }
    let bytes = text.as_bytes();
    let token_bytes = token.as_bytes();
    let mut index = from;

    while index < bytes.len() {
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }

        let end = index.saturating_add(token_bytes.len());
        if end > bytes.len() {
            return None;
        }
        if &bytes[index..end] != token_bytes {
            index += 1;
            continue;
        }

        if bytes
            .get(end)
            .is_none_or(|byte| !is_mention_name_char(*byte))
        {
            return Some(index..end);
        }

        index = end;
    }

    None
}

impl Renderable for ChatComposer {
    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if !self.input_enabled || self.selected_remote_image_index.is_some() {
            return None;
        }

        let [_, _, textarea_rect, _] = self.layout_areas(area);
        let state = *self.textarea_state.borrow();
        self.textarea.cursor_pos_with_state(textarea_rect, state)
    }

    fn desired_height(&self, width: u16) -> u16 {
        let footer_props = self.footer_props();
        let footer_hint_height = self
            .custom_footer_height()
            .unwrap_or_else(|| footer_height(&footer_props));
        let footer_spacing = Self::footer_spacing(footer_hint_height);
        let footer_total_height = footer_hint_height + footer_spacing;
        const COLS_WITH_MARGIN: u16 = LIVE_PREFIX_COLS + 1;
        let inner_width = width.saturating_sub(COLS_WITH_MARGIN);
        let remote_images_height: u16 = self
            .remote_images_lines(inner_width)
            .len()
            .try_into()
            .unwrap_or(u16::MAX);
        let remote_images_separator = u16::from(remote_images_height > 0);
        self.textarea.desired_height(inner_width)
            + remote_images_height
            + remote_images_separator
            + 2
            + match &self.active_popup {
                ActivePopup::None => footer_total_height,
                ActivePopup::Command(c) => c.calculate_required_height(width),
                ActivePopup::File(c) => c.calculate_required_height(),
                ActivePopup::Skill(c) => c.calculate_required_height(width),
            }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_mask(area, buf, /*mask_char*/ None);
    }
}

impl ChatComposer {
    pub(crate) fn render_with_mask(&self, area: Rect, buf: &mut Buffer, mask_char: Option<char>) {
        let [composer_rect, remote_images_rect, textarea_rect, popup_rect] =
            self.layout_areas(area);
        match &self.active_popup {
            ActivePopup::Command(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::File(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::Skill(popup) => {
                popup.render_ref(popup_rect, buf);
            }
            ActivePopup::None => {
                let footer_props = self.footer_props();
                let show_cycle_hint =
                    !footer_props.is_task_running && self.collaboration_mode_indicator.is_some();
                let show_shortcuts_hint = match footer_props.mode {
                    FooterMode::ComposerEmpty => !self.is_in_paste_burst(),
                    FooterMode::ComposerHasDraft => false,
                    FooterMode::QuitShortcutReminder
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let show_queue_hint = match footer_props.mode {
                    FooterMode::ComposerHasDraft => footer_props.is_task_running,
                    FooterMode::QuitShortcutReminder
                    | FooterMode::ComposerEmpty
                    | FooterMode::ShortcutOverlay
                    | FooterMode::EscHint => false,
                };
                let custom_height = self.custom_footer_height();
                let footer_hint_height =
                    custom_height.unwrap_or_else(|| footer_height(&footer_props));
                let footer_spacing = Self::footer_spacing(footer_hint_height);
                let hint_rect = if footer_spacing > 0 && footer_hint_height > 0 {
                    let [_, hint_rect] = Layout::vertical([
                        Constraint::Length(footer_spacing),
                        Constraint::Length(footer_hint_height),
                    ])
                    .areas(popup_rect);
                    hint_rect
                } else {
                    popup_rect
                };
                let available_width =
                    hint_rect.width.saturating_sub(FOOTER_INDENT_COLS as u16) as usize;
                let status_line_active = uses_passive_footer_status_layout(&footer_props);
                let combined_status_line = if status_line_active {
                    passive_footer_status_line(&footer_props).map(ratatui::prelude::Stylize::dim)
                } else {
                    None
                };
                let mut truncated_status_line = if status_line_active {
                    combined_status_line.as_ref().map(|line| {
                        truncate_line_with_ellipsis_if_overflow(line.clone(), available_width)
                    })
                } else {
                    None
                };
                let left_mode_indicator = if status_line_active {
                    None
                } else {
                    self.collaboration_mode_indicator
                };
                let mut left_width = if self.footer_flash_visible() {
                    self.footer_flash
                        .as_ref()
                        .map(|flash| flash.line.width() as u16)
                        .unwrap_or(0)
                } else if let Some(items) = self.footer_hint_override.as_ref() {
                    footer_hint_items_width(items)
                } else if status_line_active {
                    truncated_status_line
                        .as_ref()
                        .map(|line| line.width() as u16)
                        .unwrap_or(0)
                } else {
                    footer_line_width(
                        &footer_props,
                        left_mode_indicator,
                        show_cycle_hint,
                        show_shortcuts_hint,
                        show_queue_hint,
                    )
                };
                let right_line = if status_line_active {
                    let full =
                        mode_indicator_line(self.collaboration_mode_indicator, show_cycle_hint);
                    let compact = mode_indicator_line(
                        self.collaboration_mode_indicator,
                        /*show_cycle_hint*/ false,
                    );
                    let full_width = full.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                    if can_show_left_with_context(hint_rect, left_width, full_width) {
                        full
                    } else {
                        compact
                    }
                } else {
                    Some(context_window_line(
                        footer_props.context_window_percent,
                        footer_props.context_window_used_tokens,
                    ))
                };
                let right_width = right_line.as_ref().map(|l| l.width() as u16).unwrap_or(0);
                if status_line_active
                    && let Some(max_left) = max_left_width_for_right(hint_rect, right_width)
                    && left_width > max_left
                    && let Some(line) = combined_status_line.as_ref().map(|line| {
                        truncate_line_with_ellipsis_if_overflow(line.clone(), max_left as usize)
                    })
                {
                    left_width = line.width() as u16;
                    truncated_status_line = Some(line);
                }
                let can_show_left_and_context =
                    can_show_left_with_context(hint_rect, left_width, right_width);
                let has_override =
                    self.footer_flash_visible() || self.footer_hint_override.is_some();
                let single_line_layout = if has_override || status_line_active {
                    None
                } else {
                    match footer_props.mode {
                        FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft => {
                            // Both of these modes render the single-line footer style (with
                            // either the shortcuts hint or the optional queue hint). We still
                            // want the single-line collapse rules so the mode label can win over
                            // the context indicator on narrow widths.
                            Some(single_line_footer_layout(
                                hint_rect,
                                right_width,
                                left_mode_indicator,
                                show_cycle_hint,
                                show_shortcuts_hint,
                                show_queue_hint,
                            ))
                        }
                        FooterMode::EscHint
                        | FooterMode::QuitShortcutReminder
                        | FooterMode::ShortcutOverlay => None,
                    }
                };
                let show_right = if matches!(
                    footer_props.mode,
                    FooterMode::EscHint
                        | FooterMode::QuitShortcutReminder
                        | FooterMode::ShortcutOverlay
                ) {
                    false
                } else {
                    single_line_layout
                        .as_ref()
                        .map(|(_, show_context)| *show_context)
                        .unwrap_or(can_show_left_and_context)
                };

                if let Some((summary_left, _)) = single_line_layout {
                    match summary_left {
                        SummaryLeft::Default => {
                            if status_line_active {
                                if let Some(line) = truncated_status_line.clone() {
                                    render_footer_line(hint_rect, buf, line);
                                } else {
                                    render_footer_from_props(
                                        hint_rect,
                                        buf,
                                        &footer_props,
                                        left_mode_indicator,
                                        show_cycle_hint,
                                        show_shortcuts_hint,
                                        show_queue_hint,
                                    );
                                }
                            } else {
                                render_footer_from_props(
                                    hint_rect,
                                    buf,
                                    &footer_props,
                                    left_mode_indicator,
                                    show_cycle_hint,
                                    show_shortcuts_hint,
                                    show_queue_hint,
                                );
                            }
                        }
                        SummaryLeft::Custom(line) => {
                            render_footer_line(hint_rect, buf, line);
                        }
                        SummaryLeft::None => {}
                    }
                } else if self.footer_flash_visible() {
                    if let Some(flash) = self.footer_flash.as_ref() {
                        flash.line.render(inset_footer_hint_area(hint_rect), buf);
                    }
                } else if let Some(items) = self.footer_hint_override.as_ref() {
                    render_footer_hint_items(hint_rect, buf, items);
                } else if status_line_active {
                    if let Some(line) = truncated_status_line {
                        render_footer_line(hint_rect, buf, line);
                    }
                } else {
                    render_footer_from_props(
                        hint_rect,
                        buf,
                        &footer_props,
                        self.collaboration_mode_indicator,
                        show_cycle_hint,
                        show_shortcuts_hint,
                        show_queue_hint,
                    );
                }

                if show_right && let Some(line) = &right_line {
                    render_context_right(hint_rect, buf, line);
                }
            }
        }
        self.render_textarea(
            composer_rect,
            remote_images_rect,
            textarea_rect,
            buf,
            mask_char,
        );
    }

    /// Paint the composer's text input area, prompt chevron, and placeholder text.
    ///
    /// In Zellij sessions the textarea uses explicit `Color::Reset` foreground styling
    /// to prevent the multiplexer's pane chrome from bleeding into cell styles, and
    /// substitutes hardcoded colors for `.bold()` / `.dim()` modifiers that Zellij
    /// renders inconsistently. The standard path is unchanged.
    fn render_textarea(
        &self,
        composer_rect: Rect,
        remote_images_rect: Rect,
        textarea_rect: Rect,
        buf: &mut Buffer,
        mask_char: Option<char>,
    ) {
        let is_zellij = self.is_zellij;
        let style = user_message_style();
        let textarea_style = style.fg(ratatui::style::Color::Reset);
        Block::default().style(style).render_ref(composer_rect, buf);
        if !remote_images_rect.is_empty() {
            Paragraph::new(self.remote_images_lines(remote_images_rect.width))
                .style(style)
                .render_ref(remote_images_rect, buf);
        }
        if is_zellij && !textarea_rect.is_empty() {
            buf.set_style(textarea_rect, textarea_style);
        }
        if !textarea_rect.is_empty() {
            let prompt = if self.input_enabled {
                if is_zellij {
                    Span::styled("┃", style.fg(self.accent_color))
                } else {
                    Span::styled("┃", Style::default().fg(self.accent_color))
                }
            } else if is_zellij {
                Span::styled("┃", style.fg(ratatui::style::Color::DarkGray))
            } else {
                "┃".dark_gray()
            };
            buf.set_span(
                textarea_rect.x - LIVE_PREFIX_COLS,
                textarea_rect.y,
                &prompt,
                textarea_rect.width,
            );
        }

        let mut state = self.textarea_state.borrow_mut();
        let textarea_is_empty = self.textarea.text().is_empty();
        if let Some(mask_char) = mask_char {
            self.textarea.render_ref_masked(
                textarea_rect,
                buf,
                &mut state,
                mask_char,
                if is_zellij {
                    textarea_style
                } else {
                    ratatui::style::Style::default()
                },
            );
        } else if is_zellij && textarea_is_empty {
            buf.set_style(textarea_rect, textarea_style);
        } else if is_zellij {
            self.textarea
                .render_ref_styled(textarea_rect, buf, &mut state, textarea_style);
        } else {
            StatefulWidgetRef::render_ref(&(&self.textarea), textarea_rect, buf, &mut state);
        }
        if textarea_is_empty {
            let text = if self.input_enabled {
                self.placeholder_text.as_str().to_string()
            } else {
                self.input_disabled_placeholder
                    .as_deref()
                    .unwrap_or("Input disabled.")
                    .to_string()
            };
            if !textarea_rect.is_empty() {
                if is_zellij {
                    buf.set_string(
                        textarea_rect.x,
                        textarea_rect.y,
                        text,
                        textarea_style.fg(ratatui::style::Color::White).italic(),
                    );
                } else {
                    let placeholder = Span::from(text).dim();
                    let line = Line::from(vec![placeholder]);
                    line.render_ref(textarea_rect.inner(Margin::new(0, 0)), buf);
                }
            }
        }
    }
}
