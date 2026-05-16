//! Devo TUI chat surface.
//!
//! `ChatWidget` owns the v2 conversation surface: committed history cells, the
//! active bottom input pane, and the Claw-local app events produced by user
//! interaction. Protocol thinking choices come from `devo_protocol::thinking`
//! through `Model` instead of a TUI-local reasoning enum.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use devo_core::ItemId;
use devo_protocol::InputItem;
use devo_protocol::Model;
use devo_protocol::ProviderWireApi;
use devo_protocol::ReasoningEffort;
use devo_protocol::ReasoningEffortPreset;
use devo_protocol::ThinkingCapability;
use devo_protocol::ThinkingImplementation;
use devo_protocol::ThinkingPreset;
use devo_protocol::user_input::TextElement;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use devo_protocol::TurnId;

use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::ApprovalOverlay;
use crate::bottom_pane::ApprovalOverlayRequest;
use crate::bottom_pane::BottomPane;
use crate::bottom_pane::BottomPaneParams;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::ModelPickerEntry;
use crate::bottom_pane::list_selection_view::ListSelectionView;
use crate::bottom_pane::list_selection_view::SelectionItem;
use crate::bottom_pane::list_selection_view::SelectionViewParams;
use crate::events::SessionListEntry;
use crate::events::PlanStep;
use crate::events::PlanStepStatus;
use crate::events::TextItemKind;
use crate::events::TranscriptItem;
use crate::events::TranscriptItemKind;
use crate::events::WorkerEvent;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::ExecCell;
use crate::exec_cell::new_active_exec_command;
use crate::exec_cell::truncated_tool_output_preview;
use crate::get_git_diff::get_git_diff;
use crate::history_cell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::ScrollbackLine;
use crate::markdown::append_markdown;
use crate::render::renderable::Renderable;
use crate::render::line_utils::prefix_lines;
use crate::slash_command::SlashCommand;
use crate::startup_header::STARTUP_HEADER_ANIMATION_INTERVAL;
use crate::streaming::chunking::AdaptiveChunkingPolicy;
use crate::streaming::commit_tick::CommitTickScope;
use crate::streaming::commit_tick::run_commit_tick;
use crate::streaming::controller::StreamController;
use crate::theme::ThemeSet;
use crate::tool_result_cell::ToolResultCell;
use crate::tui::frame_requester::FrameRequester;
use devo_utils::ansi_escape::ansi_escape_line;
use devo_utils::shell_command::parse_command::parse_command;
use devo_protocol::{SessionHistoryItem, SessionHistoryMetadata, SessionPlanStepStatus};

/// Common initialization parameters shared by `ChatWidget` constructors.
pub(crate) struct ChatWidgetInit {
    pub(crate) frame_requester: FrameRequester,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) initial_session: TuiSessionState,
    pub(crate) initial_thinking_selection: Option<String>,
    pub(crate) initial_permission_preset: devo_protocol::PermissionPreset,
    pub(crate) initial_user_message: Option<UserMessage>,
    pub(crate) enhanced_keys_supported: bool,
    pub(crate) is_first_run: bool,
    pub(crate) available_models: Vec<Model>,
    /// Configured model slugs from config.toml used by the /model picker.
    pub(crate) saved_model_slugs: Vec<String>,
    pub(crate) show_model_onboarding: bool,
    pub(crate) startup_tooltip_override: Option<String>,
    pub(crate) initial_theme_name: Option<String>,
}

/// Resolved runtime session projection owned by the chat widget.
///
/// Unlike `InitialTuiSession`, this is internal TUI state: the model slug has already been resolved
/// into model metadata when available, and provider is derived from that projection.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TuiSessionState {
    pub(crate) cwd: PathBuf,
    pub(crate) model: Option<Model>,
    pub(crate) provider: Option<ProviderWireApi>,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
}

impl TuiSessionState {
    pub(crate) fn new(cwd: PathBuf, model: Option<Model>) -> Self {
        let provider = model.as_ref().map(Model::provider_wire_api);
        Self {
            cwd,
            model,
            provider,
            reasoning_effort: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ExternalEditorState {
    #[default]
    Closed,
    Requested,
    Active,
}

/// Snapshot of active-cell state that affects transcript overlay rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActiveCellTranscriptKey {
    pub(crate) revision: u64,
    pub(crate) is_stream_continuation: bool,
    pub(crate) animation_tick: Option<u64>,
}

/// Snapshot of one committed transcript cell for the Ctrl+T overlay.
#[derive(Clone, Debug)]
pub(crate) struct TranscriptOverlayCell {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) is_stream_continuation: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct UserMessage {
    pub(crate) text: String,
    pub(crate) local_images: Vec<LocalImageAttachment>,
    pub(crate) remote_image_urls: Vec<String>,
    pub(crate) text_elements: Vec<TextElement>,
    pub(crate) mention_bindings: Vec<MentionBinding>,
}

impl From<String> for UserMessage {
    fn from(text: String) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }
}

impl From<&str> for UserMessage {
    fn from(text: &str) -> Self {
        text.to_string().into()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThinkingListEntry {
    pub(crate) is_current: bool,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OnboardingStep {
    ModelName,
    BaseUrl {
        model: String,
    },
    ApiKey {
        model: String,
        base_url: Option<String>,
    },
    Validating {
        model: String,
        base_url: Option<String>,
        api_key: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct ResumeBrowserState {
    sessions: Vec<SessionListEntry>,
    selection: usize,
}

#[derive(Debug, Clone)]
struct ActiveToolCall {
    tool_use_id: String,
    title: String,
    lines: Vec<Line<'static>>,
    exec_like: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DotStatus {
    Pending,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerMode {
    Model,
    Thinking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingModelSelection {
    slug: String,
    thinking_selection: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingApprovalRequest {
    session_id: devo_protocol::SessionId,
    turn_id: TurnId,
    approval_id: String,
    action_summary: String,
}

struct ActiveTextItem {
    item_id: ActiveTextItemId,
    kind: TextItemKind,
    status: DotStatus,
    stream_controller: Option<StreamController>,
    raw_text: String,
    cell: Option<history_cell::AgentMessageCell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveTextItemId {
    Server(ItemId),
    Legacy(TextItemKind),
}

impl ActiveTextItemId {
    fn log_label(self) -> String {
        match self {
            Self::Server(item_id) => item_id.to_string(),
            Self::Legacy(kind) => format!("legacy-{kind:?}"),
        }
    }
}

fn permission_preset_items(current: devo_protocol::PermissionPreset) -> Vec<SelectionItem> {
    [
        (
            devo_protocol::PermissionPreset::ReadOnly,
            "Read Only",
            "Devo can read files in the current workspace. Approval is required to edit files, run commands, or access the internet.",
        ),
        (
            devo_protocol::PermissionPreset::Default,
            "Default",
            "Devo can read and edit files in the current workspace, and run commands. Approval is required to access the internet or edit other files.",
        ),
        (
            devo_protocol::PermissionPreset::AutoReview,
            "Auto-review",
            "Same workspace-write permissions as Default, but eligible approvals are routed through the auto-reviewer before interrupting you.",
        ),
        (
            devo_protocol::PermissionPreset::FullAccess,
            "Full Access",
            "Devo can edit files outside this workspace and access the internet without asking for approval. Exercise caution when using.",
        ),
    ]
    .into_iter()
    .map(|(preset, label, description)| {
        let name = if preset == current {
            format!("{label} (current)")
        } else {
            label.to_string()
        };
        SelectionItem {
            name,
            description: Some(description.to_string()),
            is_current: preset == current,
            dismiss_on_select: true,
            actions: vec![Box::new(move |app_event_tx| {
                app_event_tx.send(AppEvent::Command(AppCommand::UpdatePermissions {
                    preset,
                }));
            })],
            ..Default::default()
        }
    })
    .collect()
}

fn permission_preset_label(preset: devo_protocol::PermissionPreset) -> &'static str {
    match preset {
        devo_protocol::PermissionPreset::ReadOnly => "Read Only",
        devo_protocol::PermissionPreset::Default => "Default",
        devo_protocol::PermissionPreset::AutoReview => "Auto-review",
        devo_protocol::PermissionPreset::FullAccess => "Full Access",
    }
}

pub(crate) struct ChatWidget {
    // App event, such as UserTurn, List Sessions, New Session, Onboard or Browser Input History
    app_event_tx: AppEventSender,
    // Frame requester for scheduling future frame draws on the TUI event loop.
    frame_requester: FrameRequester,
    // The session state utlized for TUI rendering, currently simple: cwd, Model, ProviderWireApi
    // TODO: Shoule expland the session state, and move thinking_selection into session state.
    session: TuiSessionState,
    thinking_selection: Option<String>,
    // sub widget, bottom pane, including such input textarea, slash command popup, status summary.
    bottom_pane: BottomPane,
    active_cell: Option<Box<dyn HistoryCell>>,
    active_cell_revision: u64,
    active_tool_calls: HashMap<String, ActiveToolCall>,
    pending_tool_calls: Vec<ActiveToolCall>,
    history: Vec<Box<dyn HistoryCell>>,
    next_history_flush_index: usize,
    queued_user_messages: VecDeque<UserMessage>,
    external_editor_state: ExternalEditorState,
    status_message: String,
    active_text_items: Vec<ActiveTextItem>,
    stream_chunking_policy: AdaptiveChunkingPolicy,
    available_models: Vec<Model>,
    saved_model_slugs: Vec<String>,
    onboarding_step: Option<OnboardingStep>,
    resume_browser: Option<ResumeBrowserState>,
    resume_browser_loading: bool,
    picker_mode: Option<PickerMode>,
    pending_model_selection: Option<PendingModelSelection>,
    theme_set: ThemeSet,
    active_theme_name: String,
    turn_count: usize,
    total_input_tokens: usize,
    total_output_tokens: usize,
    total_cache_read_tokens: usize,
    prompt_token_estimate: usize,
    last_query_input_tokens: usize,
    last_query_total_tokens: usize,
    last_plan_progress: Option<(usize, usize)>,
    queued_count: usize,
    active_turn_id: Option<TurnId>,
    pending_approval: Option<PendingApprovalRequest>,
    permission_preset: devo_protocol::PermissionPreset,
    busy: bool,
    selection_mode: bool,
    selected_user_cell_index: Option<usize>,
    user_cell_history_indices: Vec<usize>,
    startup_header_mascot_frame_index: usize,
    startup_header_next_animation_at: Instant,
}

impl ChatWidget {
    pub(crate) fn should_auto_show_git_diff(tool_title: &str, is_error: bool) -> bool {
        if is_error {
            return false;
        }
        let lower = tool_title.to_ascii_lowercase();
        lower.contains("write ")
            || lower.starts_with("write:")
            || lower.contains("edit ")
            || lower.starts_with("edit:")
            || lower.contains("apply_patch")
            || lower.contains("apply patch")
    }

    fn can_change_configuration(&self) -> bool {
        !self.busy
    }

    fn add_busy_configuration_message(&mut self, command: SlashCommand) {
        let noun = match command {
            SlashCommand::Model => "model",
            SlashCommand::Onboard => "provider",
            SlashCommand::Theme => "theme",
            SlashCommand::Compact => "session",
            SlashCommand::New => "session",
            SlashCommand::Resume => "session",
            SlashCommand::Permissions => "permissions",
            SlashCommand::Diff => "diff",
            SlashCommand::Exit | SlashCommand::Status | SlashCommand::Clear | SlashCommand::Btw => {
                return;
            }
        };
        self.add_to_history(PlainHistoryCell::new(vec![Line::from(format!(
            "Cannot change {noun} while generating"
        ))]));
        self.set_status_message(format!("Cannot change {noun} while generating"));
    }

    fn is_blank_line(line: &Line<'_>) -> bool {
        line.spans.iter().all(|span| span.content.trim().is_empty())
    }

    fn build_header_box(
        cwd: &std::path::Path,
        model: Option<&Model>,
        thinking_selection: Option<&str>,
        is_first_run: bool,
        startup_tooltip_override: Option<String>,
        accent_color: Color,
        mascot_frame_index: usize,
    ) -> Box<dyn HistoryCell> {
        let model = model.cloned().unwrap_or_else(|| Model {
            slug: "unknown".to_string(),
            display_name: "unknown".to_string(),
            provider: ProviderWireApi::OpenAIChatCompletions,
            ..Model::default()
        });
        Box::new(history_cell::new_session_info(
            cwd,
            &model.slug,
            model.slug.clone(),
            model.display_name.clone(),
            model.thinking_capability.clone(),
            model
                .resolve_thinking_selection(thinking_selection)
                .effective_reasoning_effort,
            model.thinking_implementation.clone(),
            is_first_run,
            startup_tooltip_override,
            /*show_fast_status*/ false,
            accent_color,
            mascot_frame_index,
        ))
    }

    fn trim_trailing_blank_lines(lines: &mut Vec<Line<'static>>) {
        while lines
            .last()
            .is_some_and(|line| line.spans.iter().all(|span| span.content.trim().is_empty()))
        {
            lines.pop();
        }
    }

    fn completed_dot_prefix() -> Line<'static> {
        Line::from(vec![
            Span::styled("▌", Style::default().fg(Color::Rgb(120, 220, 160))),
            " ".into(),
        ])
    }

    fn pending_dot_prefix() -> Line<'static> {
        Line::from(vec![
            Span::styled("▌", Style::default().fg(Color::Rgb(110, 200, 255))),
            " ".into(),
        ])
    }

    fn reasoning_dot_prefix(status: DotStatus) -> Line<'static> {
        let color = match status {
            DotStatus::Pending => Color::Rgb(210, 150, 60),
            DotStatus::Completed => Color::Rgb(120, 220, 160),
            DotStatus::Failed => Color::Rgb(255, 100, 100),
        };
        Line::from(vec![
            Span::styled("▌", Style::default().fg(color)),
            " ".into(),
        ])
    }

    fn truncate_display_text(value: &str, max_chars: usize) -> String {
        let mut rendered = String::new();
        for (count, ch) in value.chars().enumerate() {
            if count >= max_chars {
                break;
            }
            rendered.push(ch);
        }
        if value.chars().count() > max_chars && max_chars > 0 {
            let mut truncated = rendered
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>();
            truncated.push('…');
            truncated
        } else {
            rendered
        }
    }

    fn tool_text_style() -> Style {
        Style::default().fg(Color::Rgb(160, 163, 168))
    }

    fn tool_status_running_style() -> Style {
        Style::default().fg(Color::Rgb(106, 200, 255)).bold()
    }

    fn tool_status_done_style() -> Style {
        Style::default().fg(Color::Rgb(120, 220, 160)).bold()
    }

    fn running_tool_line(title: &str) -> Line<'static> {
        let normalized = title
            .strip_prefix("Running ")
            .or_else(|| title.strip_prefix("Ran "))
            .unwrap_or(title);
        Line::from(vec![
            Span::styled("Running ", Self::tool_status_running_style()),
            Span::styled(normalized.to_string(), Self::tool_text_style()),
        ])
    }

    fn ran_tool_line(title: &str) -> Line<'static> {
        let normalized = title
            .strip_prefix("Running ")
            .or_else(|| title.strip_prefix("Ran "))
            .unwrap_or(title);
        Line::from(vec![
            Span::styled("Ran ", Self::tool_status_done_style()),
            Span::styled(normalized.to_string(), Self::tool_text_style()),
        ])
    }

    fn tool_dot_prefix() -> Line<'static> {
        Line::from(vec![
            Span::styled("▌", Style::default().fg(Color::Rgb(120, 220, 160))),
            " ".into(),
        ])
    }

    fn failed_dot_prefix(&self) -> Line<'static> {
        let error_color = self.active_error_color();
        Line::from(vec![
            Span::styled("▌", Style::default().fg(error_color)),
            " ".into(),
        ])
    }

    fn dot_prefix(&self, status: DotStatus) -> Line<'static> {
        match status {
            DotStatus::Pending => Self::pending_dot_prefix(),
            DotStatus::Completed => Self::completed_dot_prefix(),
            DotStatus::Failed => self.failed_dot_prefix(),
        }
    }

    fn format_token_count(value: usize) -> String {
        if value >= 1_000_000 {
            format!("{:.1}M", value as f64 / 1_000_000.0)
        } else if value >= 1_000 {
            format!("{:.1}k", value as f64 / 1_000.0)
        } else {
            value.to_string()
        }
    }

    fn context_usage(&self) -> Option<(usize, usize, usize)> {
        let model = self.session.model.as_ref()?;
        let total = (model
            .context_window
            .saturating_mul(model.effective_context_window_percent() as u32)
            / 100) as usize;
        let used = self.last_query_input_tokens.min(total);
        let percent = if total == 0 {
            0
        } else {
            used.saturating_mul(100) / total
        };
        Some((used, total, percent))
    }

    fn format_compact_token_count(value: usize) -> String {
        if value >= 1_000_000 {
            format!("{:.1}M", value as f64 / 1_000_000.0)
        } else if value >= 1_000 {
            format!("{:.0}k", value as f64 / 1_000.0)
        } else {
            value.to_string()
        }
    }

    fn render_progress_bar(used: usize, total: usize, bar_width: usize) -> String {
        if total == 0 {
            return String::new();
        }
        let ratio = (used as f64 / total as f64).clamp(0.0, 1.0);
        let filled = (ratio * bar_width as f64).round() as usize;
        let empty = bar_width.saturating_sub(filled);
        let bar: String = std::iter::repeat_n('▰', filled)
            .chain(std::iter::repeat_n('▱', empty))
            .collect();
        let pct = (ratio * 100.0).round() as usize;
        format!("{bar} {pct}%")
    }

    fn percent_of(numerator: usize, denominator: usize) -> usize {
        if denominator == 0 {
            0
        } else {
            (numerator.saturating_mul(100) + denominator / 2) / denominator
        }
    }

    fn session_summary_text(&self) -> String {
        let model = self
            .session
            .model
            .as_ref()
            .map(|model| model.slug.as_str())
            .unwrap_or("unknown");
        let thinking = self.thinking_selection.as_deref().unwrap_or("default");
        let cached_input_percent =
            Self::percent_of(self.total_cache_read_tokens, self.total_input_tokens);
        let context = self
            .context_usage()
            .map_or_else(String::new, |(used, total, _percent)| {
                format!(
                    "{} {}/{}",
                    Self::render_progress_bar(used, total, 10),
                    Self::format_compact_token_count(used),
                    Self::format_compact_token_count(total)
                )
            });

        let mut parts: Vec<String> = Vec::new();
        parts.push(format!("{model} {thinking}"));
        parts.push(format!(
            "↑{}",
            Self::format_compact_token_count(self.total_input_tokens)
        ));
        parts.push(format!(
            "↺{} {}%",
            Self::format_compact_token_count(self.total_cache_read_tokens),
            cached_input_percent
        ));
        parts.push(format!(
            "↓{}",
            Self::format_compact_token_count(self.total_output_tokens)
        ));
        if !context.is_empty() {
            parts.push(context);
        }
        parts.join("  ")
    }

    fn sync_bottom_pane_summary(&mut self) {
        self.bottom_pane
            .set_status_line(Some(Line::from(self.session_summary_text()).dim()));
        self.bottom_pane.set_status_line_enabled(true);
    }

    fn push_session_header(
        &mut self,
        is_first_run: bool,
        startup_tooltip_override: Option<String>,
    ) {
        self.history
            .push(self.build_current_header_box(is_first_run, startup_tooltip_override));
    }

    fn build_current_header_box(
        &self,
        is_first_run: bool,
        startup_tooltip_override: Option<String>,
    ) -> Box<dyn HistoryCell> {
        let accent = self.active_accent_color();
        Self::build_header_box(
            &self.session.cwd,
            self.session.model.as_ref(),
            self.thinking_selection.as_deref(),
            is_first_run,
            startup_tooltip_override,
            accent,
            self.startup_header_mascot_frame_index,
        )
    }

    fn history_has_non_header_content(&self) -> bool {
        self.history.iter().any(|cell| {
            cell.as_any()
                .downcast_ref::<history_cell::SessionInfoCell>()
                .is_none()
        })
    }

    fn rebuild_restored_session_history(
        &mut self,
        history_items: Vec<TranscriptItem>,
        loaded_item_count: u64,
        session_id: &str,
        title: Option<&str>,
    ) {
        self.history.clear();
        self.next_history_flush_index = 0;

        tracing::trace!(
            session_id,
            loaded_item_count,
            restored_items = history_items.len(),
            restored_preview = ?history_items
                .iter()
                .take(10)
                .map(|item| (format!("{:?}", item.kind), item.title.clone()))
                .collect::<Vec<_>>(),
            synthetic_header_inserted = true,
            "rebuilding restored session transcript"
        );

        let loaded_any_history = !history_items.is_empty();
        for item in &history_items {
            self.add_transcript_item_without_redraw(item.clone());
        }

        if !loaded_any_history {
            self.add_history_entry_without_redraw(Box::new(history_cell::new_info_event(
                format!(
                    "switched to {session_id}; title: {}; loaded items: {loaded_item_count}",
                    title.unwrap_or("(untitled)")
                ),
                None,
            )));
        }
        self.frame_requester.schedule_frame();
    }

    fn rebuild_restored_session_history_from_rich_items(
        &mut self,
        history_items: &[SessionHistoryItem],
        loaded_item_count: u64,
        session_id: &str,
        title: Option<&str>,
    ) -> bool {
        self.history.clear();
        self.next_history_flush_index = 0;

        if history_items.is_empty() {
            self.add_history_entry_without_redraw(Box::new(history_cell::new_info_event(
                format!(
                    "switched to {session_id}; title: {}; loaded items: {loaded_item_count}",
                    title.unwrap_or("(untitled)")
                ),
                None,
            )));
            self.frame_requester.schedule_frame();
            return false;
        }

        let mut paired_result_by_call_id = HashMap::new();
        for (index, item) in history_items.iter().enumerate() {
            if matches!(
                item.kind,
                devo_protocol::SessionHistoryItemKind::ToolResult
                    | devo_protocol::SessionHistoryItemKind::Error
            ) && let Some(tool_call_id) = item.tool_call_id.as_deref()
            {
                paired_result_by_call_id
                    .entry(tool_call_id.to_string())
                    .or_insert(index);
            }
        }

        let metadata_owned_ids: HashSet<String> = history_items
            .iter()
            .filter_map(|item| item.tool_call_id.clone().filter(|_| item.metadata.is_some()))
            .collect();
        let mut consumed_indexes = HashSet::new();

        for (index, item) in history_items.iter().enumerate() {
            if consumed_indexes.contains(&index) {
                continue;
            }

            if let Some(metadata) = &item.metadata {
                if let Some(tool_call_id) = item.tool_call_id.as_deref()
                    && let Some(result_index) = paired_result_by_call_id.get(tool_call_id).copied()
                {
                    consumed_indexes.insert(result_index);
                }
                match metadata {
                    SessionHistoryMetadata::PlanUpdate { explanation, steps } => {
                        self.on_plan_updated(
                            explanation.clone(),
                            steps.iter().map(|step| crate::events::PlanStep {
                                text: step.text.clone(),
                                status: match step.status {
                                    SessionPlanStepStatus::Pending => crate::events::PlanStepStatus::Pending,
                                    SessionPlanStepStatus::InProgress => crate::events::PlanStepStatus::InProgress,
                                    SessionPlanStepStatus::Completed => crate::events::PlanStepStatus::Completed,
                                    SessionPlanStepStatus::Cancelled => crate::events::PlanStepStatus::Cancelled,
                                },
                            }).collect(),
                        );
                    }
                    SessionHistoryMetadata::Edited { changes } => {
                        self.add_history_entry_without_redraw(Box::new(
                            history_cell::new_patch_event(changes.clone(), &self.session.cwd),
                        ));
                    }
                    SessionHistoryMetadata::Explored { actions } => {
                        self.restore_explored_history_item(item, actions.clone());
                    }
                }
                continue;
            }

            if item.kind == devo_protocol::SessionHistoryItemKind::ToolCall
                && let Some(tool_call_id) = item.tool_call_id.as_deref()
            {
                if metadata_owned_ids.contains(tool_call_id) {
                    continue;
                }
                if let Some(result_index) = paired_result_by_call_id.get(tool_call_id).copied() {
                    consumed_indexes.insert(result_index);
                    let result_item = &history_items[result_index];
                    let title_line =
                        (!item.title.is_empty()).then(|| Self::ran_tool_line(&item.title));
                    self.add_history_entry_without_redraw(Box::new(ToolResultCell::new(
                        title_line,
                        result_item.body.clone(),
                        Self::tool_dot_prefix(),
                        Line::from("  "),
                        Self::tool_text_style(),
                        false,
                    )));
                    continue;
                }
            }

            match item.kind {
                devo_protocol::SessionHistoryItemKind::User => {
                    self.add_history_entry_without_redraw(Box::new(history_cell::new_user_prompt(
                        item.body.clone(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        self.active_accent_color(),
                    )));
                }
                devo_protocol::SessionHistoryItemKind::Assistant => {
                    self.add_markdown_history_without_redraw("Assistant", &item.body);
                }
                devo_protocol::SessionHistoryItemKind::Reasoning => {
                    self.add_markdown_history_without_redraw("Reasoning", &item.body);
                }
                devo_protocol::SessionHistoryItemKind::ToolCall => {
                    self.add_history_entry_without_redraw(Box::new(
                        history_cell::AgentMessageCell::new_with_prefix(
                            vec![Self::running_tool_line(&item.title)],
                            self.dot_prefix(DotStatus::Pending),
                            "  ",
                            false,
                        ),
                    ));
                }
                devo_protocol::SessionHistoryItemKind::ToolResult
                | devo_protocol::SessionHistoryItemKind::CommandExecution => {
                    self.add_history_entry_without_redraw(Box::new(ToolResultCell::new(
                        (!item.title.is_empty()).then(|| Self::ran_tool_line(&item.title)),
                        item.body.clone(),
                        Self::tool_dot_prefix(),
                        Line::from("  "),
                        Self::tool_text_style(),
                        false,
                    )));
                }
                devo_protocol::SessionHistoryItemKind::Error => {
                    self.add_history_entry_without_redraw(Box::new(ToolResultCell::new(
                        (!item.title.is_empty()).then(|| Self::ran_tool_line(&item.title)),
                        item.body.clone(),
                        self.failed_dot_prefix(),
                        Line::from("  "),
                        Self::tool_text_style(),
                        false,
                    )));
                }
                devo_protocol::SessionHistoryItemKind::TurnSummary => {
                    self.add_history_entry_without_redraw(Box::new(
                        history_cell::TurnSummaryCell::new(
                            item.title.clone(),
                            item.duration_ms,
                            self.active_accent_color(),
                        ),
                    ));
                }
            }
        }

        self.frame_requester.schedule_frame();
        true
    }

    fn clear_for_session_switch(&mut self) {
        self.history.clear();
        self.next_history_flush_index = 0;
        self.active_cell = None;
        self.active_cell_revision = 0;
        self.active_tool_calls.clear();
        self.pending_tool_calls.clear();
        self.active_text_items.clear();
        self.bottom_pane.clear_composer();
        self.set_status_message("Resuming session");
    }

    fn set_default_placeholder(&mut self) {
        self.bottom_pane
            .set_placeholder_text("Ask Devo".to_string());
    }

    fn set_onboarding_placeholder(&mut self, prompt: &str) {
        self.bottom_pane
            .set_placeholder_text(format!("Onboarding: enter {prompt}"));
    }

    pub(crate) fn new_with_app_event(common: ChatWidgetInit) -> Self {
        // Pull the constructor inputs apart up front so the setup below reads in stages.
        let ChatWidgetInit {
            frame_requester,
            app_event_tx,
            initial_session,
            initial_thinking_selection,
            initial_permission_preset,
            initial_user_message,
            enhanced_keys_supported,
            is_first_run,
            available_models,
            saved_model_slugs,
            show_model_onboarding,
            startup_tooltip_override,
            initial_theme_name,
        } = common;

        // Prefer an explicit startup selection, but fall back to the model's default thinking mode.
        let thinking_selection = initial_thinking_selection.or_else(|| {
            initial_session
                .model
                .as_ref()
                .and_then(Model::default_thinking_selection)
        });

        // Queue any startup user message so it is processed through the same path as normal input.
        let mut queued_user_messages = VecDeque::new();
        if let Some(initial_user_message) = initial_user_message {
            queued_user_messages.push_back(initial_user_message);
        }

        let theme_set = ThemeSet::default();
        let active_theme_name = initial_theme_name
            .filter(|name| theme_set.find(name).is_some())
            .unwrap_or_else(|| ThemeSet::default_theme().to_string());
        let initial_accent_color = theme_set
            .find(&active_theme_name)
            .map(|t| t.accent_color)
            .unwrap_or(Color::Cyan);

        // Build the bottom composer first, since the widget delegates all live input handling there.
        let mut bottom_pane = BottomPane::new(BottomPaneParams {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            has_input_focus: true,
            enhanced_keys_supported,
            placeholder_text: "Ask Devo".to_string(),
            disable_paste_burst: false,
            skills: None,
            animations_enabled: true,
        });
        bottom_pane.set_accent_color(initial_accent_color);

        let history: Vec<Box<dyn HistoryCell>> = vec![Self::build_header_box(
            &initial_session.cwd,
            initial_session.model.as_ref(),
            thinking_selection.as_deref(),
            is_first_run,
            startup_tooltip_override,
            initial_accent_color,
            0,
        )];

        // Assemble the full widget state from the initial session, composer, history, and queues.
        let mut widget = Self {
            app_event_tx,
            frame_requester,
            session: initial_session,
            thinking_selection,
            bottom_pane,
            active_cell: None,
            active_cell_revision: 0,
            active_tool_calls: HashMap::new(),
            pending_tool_calls: Vec::new(),
            history,
            next_history_flush_index: 0,
            queued_user_messages,
            external_editor_state: ExternalEditorState::Closed,
            status_message: "Ready".to_string(),
            active_text_items: Vec::new(),
            stream_chunking_policy: AdaptiveChunkingPolicy::default(),
            available_models,
            saved_model_slugs,
            onboarding_step: None,
            resume_browser: None,
            resume_browser_loading: false,
            picker_mode: None,
            pending_model_selection: None,
            theme_set,
            active_theme_name,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            prompt_token_estimate: 0,
            last_query_input_tokens: 0,
            last_query_total_tokens: 0,
            last_plan_progress: None,
            queued_count: 0,
            active_turn_id: None,
            pending_approval: None,
            permission_preset: initial_permission_preset,
            busy: false,
            selection_mode: false,
            selected_user_cell_index: None,
            user_cell_history_indices: Vec::new(),
            startup_header_mascot_frame_index: 0,
            startup_header_next_animation_at: Instant::now() + STARTUP_HEADER_ANIMATION_INTERVAL,
        };

        // Model onboarding can inject additional startup UI before the first frame is drawn.
        if show_model_onboarding {
            widget.begin_onboarding();
        }

        // Keep the bottom pane summary in sync with the assembled widget state.
        widget.sync_bottom_pane_summary();
        widget
    }

    pub(crate) fn handle_key_event(&mut self, key: KeyEvent) {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        if self.resume_browser.is_some() {
            self.handle_resume_browser_key_event(key);
            return;
        }
        if let Some(result) = self.bottom_pane.poll_onboarding_result() {
            self.handle_onboarding_result(result);
        }
        if self.handle_selection_mode_key(key) {
            return;
        }
        match self.bottom_pane.handle_key_event(key) {
            InputResult::Submitted {
                text,
                text_elements,
                local_images,
                mention_bindings,
            } => {
                if self.busy && !text.trim().is_empty() {
                    // Turn is active — show in bottom pane as pending cell.
                    self.bottom_pane.push_pending_cell(text.clone());
                    self.queued_count += 1;
                    self.app_event_tx
                        .send(AppEvent::Command(AppCommand::user_turn(
                            vec![devo_protocol::InputItem::Text { text }],
                            Some(self.session.cwd.clone()),
                            self.session.model.as_ref().map(|m| m.slug.clone()),
                            self.thinking_selection.clone(),
                            /*sandbox*/ None,
                            Some("on-request".to_string()),
                        )));
                    self.set_status_message("Message queued");
                } else {
                    let user_message = UserMessage {
                        text,
                        local_images,
                        remote_image_urls: Vec::new(),
                        text_elements,
                        mention_bindings,
                    };
                    self.submit_user_message(user_message);
                }
            }
            InputResult::Command { command, argument } => {
                self.handle_slash_command(command, argument);
            }
            InputResult::ModelSelected { model } => match self.picker_mode.take() {
                Some(PickerMode::Thinking) => self.apply_thinking_selection(model),
                _ => self.handle_model_picker_selection(model),
            },
            InputResult::ThemeSelected { name } => {
                self.apply_theme_selection(name);
            }
            InputResult::None => {}
        }
    }

    fn handle_selection_mode_key(&mut self, key: KeyEvent) -> bool {
        let alt_up = key.code == KeyCode::Up && key.modifiers.contains(KeyModifiers::ALT);
        let alt_down = key.code == KeyCode::Down && key.modifiers.contains(KeyModifiers::ALT);

        if !alt_up && !alt_down {
            if !self.selection_mode {
                return false;
            }
            match key.code {
                KeyCode::Esc => {
                    self.exit_selection_mode();
                    return true;
                }
                KeyCode::Enter => {
                    self.open_selection_action_menu();
                    return true;
                }
                _ => return false,
            }
        }

        if self.busy {
            return false;
        }

        self.refresh_user_cell_indices();
        let len = self.user_cell_history_indices.len();
        if len == 0 {
            return false;
        }

        if !self.selection_mode {
            self.selection_mode = true;
            // Start from the last user cell if no selection yet
            self.selected_user_cell_index = Some(len - 1);
            self.sync_selected_user_cell_highlight();
            self.update_selection_status();
            self.frame_requester.schedule_frame();
            return true;
        }

        let current = self.selected_user_cell_index.unwrap_or(0);
        let new = if alt_up {
            current.saturating_sub(1)
        } else {
            (current + 1).min(len - 1)
        };
        if new != current {
            self.selected_user_cell_index = Some(new);
            self.sync_selected_user_cell_highlight();
            self.update_selection_status();
            self.frame_requester.schedule_frame();
        }
        true
    }

    fn exit_selection_mode(&mut self) {
        self.selection_mode = false;
        self.selected_user_cell_index = None;
        self.sync_selected_user_cell_highlight();
        self.bottom_pane
            .set_status_line(Some(Line::from(self.session_summary_text()).dim()));
        self.bottom_pane.set_status_line_enabled(true);
        self.frame_requester.schedule_frame();
    }

    fn update_selection_status(&mut self) {
        if let Some(idx) = self.selected_user_cell_index {
            let turn_num = idx + 1;
            self.bottom_pane.set_status_line(Some(
                Line::from(format!(
                    "Selected turn {turn_num} · Enter to act  Esc to cancel"
                ))
                .dim(),
            ));
            self.bottom_pane.set_status_line_enabled(true);
        }
    }

    fn refresh_user_cell_indices(&mut self) {
        self.user_cell_history_indices = self
            .history
            .iter()
            .enumerate()
            .filter_map(|(i, cell)| {
                let cell_ref: &dyn HistoryCell = cell.as_ref();
                cell_ref
                    .as_any()
                    .downcast_ref::<history_cell::UserHistoryCell>()
                    .map(|_| i)
            })
            .collect();
    }

    fn sync_selected_user_cell_highlight(&mut self) {
        for (history_idx, cell) in self.history.iter_mut().enumerate() {
            let Some(user_cell) = cell
                .as_mut()
                .as_any_mut()
                .downcast_mut::<history_cell::UserHistoryCell>()
            else {
                continue;
            };
            let is_selected = self.selection_mode
                && self
                    .selected_user_cell_index
                    .and_then(|selected_idx| self.user_cell_history_indices.get(selected_idx))
                    .is_some_and(|selected_history_idx| *selected_history_idx == history_idx);
            user_cell.selected = is_selected;
        }
    }

    fn open_selection_action_menu(&mut self) {
        if !self.selection_mode {
            return;
        }
        let Some(selected_idx) = self.selected_user_cell_index else {
            self.exit_selection_mode();
            return;
        };
        let Some(history_idx) = self.user_cell_history_indices.get(selected_idx).copied() else {
            self.exit_selection_mode();
            return;
        };
        let Some(user_cell) = self.history.get(history_idx).and_then(|cell| {
            cell.as_ref()
                .as_any()
                .downcast_ref::<history_cell::UserHistoryCell>()
        }) else {
            self.exit_selection_mode();
            return;
        };

        let is_latest_user_turn = selected_idx + 1 == self.user_cell_history_indices.len();
        let selected_turn_index = u32::try_from(selected_idx).unwrap_or(u32::MAX);
        let selected_text = user_cell.message.clone();
        let mut items = Vec::new();

        items.push(SelectionItem {
            name: "Rollback".to_string(),
            description: None,
            selected_description: None,
            is_current: false,
            is_default: false,
            is_disabled: is_latest_user_turn,
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::Command(AppCommand::rollback_to_user_turn(
                    selected_turn_index,
                )));
            })],
            dismiss_on_select: true,
            search_value: None,
            disabled_reason: is_latest_user_turn
                .then_some("Latest user turn cannot be rolled back".to_string()),
            ..SelectionItem::default()
        });

        let fork_turn_index = selected_turn_index;
        items.push(SelectionItem {
            name: "Fork".to_string(),
            description: None,
            selected_description: None,
            is_current: false,
            is_default: false,
            is_disabled: is_latest_user_turn,
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::Command(AppCommand::fork_at_user_turn(
                    fork_turn_index,
                )));
            })],
            dismiss_on_select: true,
            search_value: None,
            disabled_reason: is_latest_user_turn
                .then_some("Latest user turn cannot be forked".to_string()),
            ..SelectionItem::default()
        });

        items.push(SelectionItem {
            name: "Cancel".to_string(),
            description: None,
            selected_description: None,
            is_current: false,
            is_default: false,
            is_disabled: false,
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::StatusMessageChanged {
                    message: "Selection cancelled".to_string(),
                });
            })],
            dismiss_on_select: true,
            search_value: None,
            disabled_reason: None,
            ..SelectionItem::default()
        });

        self.bottom_pane
            .open_popup_view(Box::new(ListSelectionView::new(
                SelectionViewParams {
                    items,
                    ..SelectionViewParams::default()
                },
                self.app_event_tx.clone(),
                self.active_accent_color(),
            )));
        self.bottom_pane
            .restore_input_from_history(Some(selected_text));
        self.set_status_message("Select an action");
    }

    pub(crate) fn handle_paste(&mut self, text: String) {
        if self.resume_browser.is_some() {
            return;
        }
        self.bottom_pane.handle_paste(text);
    }

    pub(crate) fn pre_draw_tick(&mut self) {
        self.advance_startup_header_animation();
        self.run_stream_commit_tick();
        self.bottom_pane.pre_draw_tick();
    }

    pub(crate) fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Redraw => self.frame_requester.schedule_frame(),
            AppEvent::SubmitUserInput { text } => self.submit_text(text),
            AppEvent::ModelSelected { model } => {
                self.handle_model_picker_selection(model);
            }
            AppEvent::ThemeSelected { name } => {
                self.apply_theme_selection(name);
            }
            AppEvent::ThinkingSelected { value } => self.set_thinking_selection(value),
            AppEvent::StatusMessageChanged { message } => self.set_status_message(message),
            AppEvent::HistoryEntryRequested { .. } => {
                self.set_status_message("Persistent composer history is not available");
            }
            AppEvent::ClearTranscript => {
                self.history.clear();
                self.next_history_flush_index = 0;
                self.frame_requester.schedule_frame();
            }
            AppEvent::Interrupt => self.set_status_message("Interrupted"),
            AppEvent::Command(command) => {
                if matches!(
                    &command,
                    AppCommand::RunUserShellCommand { command } if command == "session list"
                ) {
                    self.resume_browser = None;
                    self.resume_browser_loading = true;
                }
                if command == AppCommand::Compact {
                    self.busy = true;
                    self.bottom_pane.set_task_running(true);
                    self.set_status_message("Requesting session compaction");
                    return;
                }
                self.set_status_message(format!("Command queued: {}", command.kind()));
            }
            AppEvent::RunSlashCommand { command } => {
                if let Ok(command) = command.parse::<SlashCommand>() {
                    self.handle_slash_command(command, String::new());
                }
                self.frame_requester.schedule_frame();
            }
            AppEvent::Exit(_)
            | AppEvent::OpenSlashCommandPopup
            | AppEvent::ClosePopup
            | AppEvent::OpenModelPicker
            | AppEvent::OpenThinkingPicker
            | AppEvent::OpenThemePicker
            | AppEvent::StatusLineBranchUpdated { .. }
            | AppEvent::StartFileSearch(_)
            | AppEvent::StatusLineSetup { .. }
            | AppEvent::StatusLineSetupCancelled
            | AppEvent::TerminalTitleSetup { .. }
            | AppEvent::TerminalTitleSetupPreview { .. }
            | AppEvent::TerminalTitleSetupCancelled => {
                self.frame_requester.schedule_frame();
            }
            AppEvent::DiffResult(text) => {
                let lines: Vec<Line<'static>> = if text.trim().is_empty() {
                    vec!["No changes detected.".italic().into()]
                } else {
                    text.lines().map(ansi_escape_line).collect()
                };
                let mut all_lines = vec![Line::from("Git Diff".bold()), Line::from("")];
                all_lines.extend(lines);
                self.add_to_history(PlainHistoryCell::new(all_lines));
                self.set_status_message("Diff shown");
            }
        }
    }

    pub(crate) fn handle_worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::TurnStarted {
                model,
                thinking,
                reasoning_effort,
                turn_id,
                ..
            } => {
                self.active_turn_id = Some(turn_id);
                self.update_session_request_model(model);
                self.thinking_selection = thinking;
                self.session.reasoning_effort = reasoning_effort;
                self.refresh_header_box();
                self.busy = true;
                self.active_text_items.clear();
                self.stream_chunking_policy.reset();
                self.bottom_pane.set_task_running(true);
            }
            WorkerEvent::TextItemStarted { item_id, kind } => {
                self.start_text_item(ActiveTextItemId::Server(item_id), kind);
                self.set_status_message(match kind {
                    TextItemKind::Assistant => "Generating",
                    TextItemKind::Reasoning => "Thinking",
                });
            }
            WorkerEvent::TextItemDelta {
                item_id,
                kind,
                delta,
            } => {
                self.push_text_item_delta(ActiveTextItemId::Server(item_id), kind, &delta);
                self.set_status_message(match kind {
                    TextItemKind::Assistant => "Generating",
                    TextItemKind::Reasoning => "Thinking",
                });
            }
            WorkerEvent::TextItemCompleted {
                item_id,
                kind,
                final_text,
            } => {
                self.complete_text_item(ActiveTextItemId::Server(item_id), kind, final_text);
                self.set_status_message(match kind {
                    TextItemKind::Assistant => "Generating",
                    TextItemKind::Reasoning => "Thinking",
                });
            }
            WorkerEvent::TextDelta(text) => {
                if !self.has_server_active_item(TextItemKind::Assistant) {
                    self.push_text_item_delta(
                        ActiveTextItemId::Legacy(TextItemKind::Assistant),
                        TextItemKind::Assistant,
                        &text,
                    );
                }
                self.set_status_message("Generating");
            }
            WorkerEvent::ReasoningDelta(text) => {
                if !self.has_server_active_item(TextItemKind::Reasoning) {
                    self.push_text_item_delta(
                        ActiveTextItemId::Legacy(TextItemKind::Reasoning),
                        TextItemKind::Reasoning,
                        &text,
                    );
                }
                self.set_status_message("Thinking");
            }
            WorkerEvent::AssistantMessageCompleted(text) => {
                if !self.has_server_active_item(TextItemKind::Assistant) {
                    self.complete_text_item(
                        ActiveTextItemId::Legacy(TextItemKind::Assistant),
                        TextItemKind::Assistant,
                        text,
                    );
                }
                self.set_status_message("Generating");
            }
            WorkerEvent::ReasoningCompleted(text) => {
                if !self.has_server_active_item(TextItemKind::Reasoning) {
                    self.complete_text_item(
                        ActiveTextItemId::Legacy(TextItemKind::Reasoning),
                        TextItemKind::Reasoning,
                        text,
                    );
                }
                self.set_status_message("Thinking");
            }
            WorkerEvent::ToolCall {
                tool_use_id,
                summary,
                parsed_commands,
            } => {
                let command = crate::exec_command::split_command_string(&summary);
                let parsed = parsed_commands.unwrap_or_else(|| parse_command(&command));
                let exec_like = !parsed.is_empty()
                    && parsed
                        .iter()
                        .all(|parsed| !matches!(parsed, devo_protocol::parse_command::ParsedCommand::Unknown { .. }));
                if exec_like {
                    if let Some(cell) = self
                        .active_cell
                        .as_mut()
                        .and_then(|cell| cell.as_any_mut().downcast_mut::<ExecCell>())
                        && let Some(grouped) = cell.with_added_call(
                            tool_use_id.clone(),
                            command.clone(),
                            parsed.clone(),
                            devo_protocol::protocol::ExecCommandSource::Agent,
                            None,
                        )
                    {
                        *cell = grouped;
                        self.active_tool_calls.insert(
                            tool_use_id.clone(),
                            ActiveToolCall {
                                tool_use_id,
                                title: summary,
                                lines: Vec::new(),
                                exec_like: true,
                            },
                        );
                        self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
                        self.frame_requester.schedule_frame();
                        self.set_status_message("Tool started");
                        return;
                    }

                    self.flush_active_cell();
                    self.active_cell = Some(Box::new(new_active_exec_command(
                        tool_use_id.clone(),
                        command,
                        parsed,
                        devo_protocol::protocol::ExecCommandSource::Agent,
                        None,
                        true,
                    )));
                    self.active_tool_calls.insert(
                        tool_use_id.clone(),
                        ActiveToolCall {
                            tool_use_id,
                            title: summary,
                            lines: Vec::new(),
                            exec_like: true,
                        },
                    );
                    self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
                    self.frame_requester.schedule_frame();
                    self.set_status_message("Tool started");
                    return;
                }

                let title = summary;
                let tool_call = ActiveToolCall {
                    tool_use_id: tool_use_id.clone(),
                    title: title.clone(),
                    lines: vec![Self::running_tool_line(&title)],
                    exec_like: false,
                };
                self.active_tool_calls
                    .insert(tool_use_id.clone(), tool_call.clone());
                self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
                self.add_history_entry_without_redraw(Box::new(
                    history_cell::AgentMessageCell::new_with_prefix(
                        tool_call.lines,
                        self.dot_prefix(DotStatus::Pending),
                        "  ",
                        false,
                    ),
                ));
                self.frame_requester.schedule_frame();
                self.set_status_message("Tool started");
            }
            WorkerEvent::ToolOutputDelta { tool_use_id, delta } => {
                if let Some(tool_call) = self.active_tool_calls.get_mut(&tool_use_id) {
                    if tool_call.exec_like {
                        if let Some(cell) = self
                            .active_cell
                            .as_mut()
                            .and_then(|cell| cell.as_any_mut().downcast_mut::<ExecCell>())
                            && cell.append_output(&tool_use_id, &delta)
                        {
                            self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
                            self.frame_requester.schedule_frame();
                        }
                        return;
                    }
                    let line = Line::from(delta.clone()).patch_style(Self::tool_text_style());
                    tool_call.lines.push(line);
                    self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
                    self.frame_requester.schedule_frame();
                }
            }
            WorkerEvent::ToolResult {
                tool_use_id,
                title,
                preview,
                is_error,
                truncated,
            } => {
                // Remove from pending viewport entries — it will be committed to history below.
                if let Some(pos) = self
                    .pending_tool_calls
                    .iter()
                    .position(|tc| tc.tool_use_id == tool_use_id)
                {
                    self.pending_tool_calls.remove(pos);
                }
                let dot_status = if is_error {
                    DotStatus::Failed
                } else {
                    DotStatus::Completed
                };
                let resolved_title = self
                    .active_tool_calls
                    .remove(&tool_use_id)
                    .unwrap_or(ActiveToolCall {
                        tool_use_id: tool_use_id.clone(),
                        title,
                        lines: Vec::new(),
                        exec_like: false,
                    });

                if resolved_title.exec_like
                    && let Some(cell) = self
                        .active_cell
                        .as_mut()
                        .and_then(|cell| cell.as_any_mut().downcast_mut::<ExecCell>())
                {
                    let completed = cell.complete_call(
                        &tool_use_id,
                        CommandOutput {
                            exit_code: if is_error { 1 } else { 0 },
                            aggregated_output: preview.clone(),
                            formatted_output: preview.clone(),
                        },
                        std::time::Duration::from_millis(0),
                    );
                    if completed {
                        if cell.is_exploring_cell() {
                            self.active_cell_revision =
                                self.active_cell_revision.wrapping_add(1);
                            self.frame_requester.schedule_frame();
                        } else if cell.should_flush() {
                            self.flush_active_cell();
                        } else {
                            self.active_cell_revision =
                                self.active_cell_revision.wrapping_add(1);
                            self.frame_requester.schedule_frame();
                        }
                        self.set_status_message(if is_error {
                            "Tool returned an error"
                        } else {
                            "Tool completed"
                        });
                        return;
                    }
                }

                let resolved_title = resolved_title.title;

                let title_line =
                    (!resolved_title.is_empty()).then(|| Self::ran_tool_line(&resolved_title));
                if title_line.is_some() || !preview.is_empty() || truncated {
                    self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
                    self.add_to_history(ToolResultCell::new(
                        title_line,
                        preview,
                        self.dot_prefix(dot_status),
                        Line::from("  "),
                        Self::tool_text_style(),
                        truncated,
                    ));
                }
                self.set_status_message(if is_error {
                    "Tool returned an error"
                } else {
                    "Tool completed"
                });
                if Self::should_auto_show_git_diff(&resolved_title, is_error) {
                    let tx = self.app_event_tx.clone();
                    tokio::spawn(async move {
                        let text = match get_git_diff().await {
                            Ok((is_git_repo, diff_text)) => {
                                if is_git_repo {
                                    diff_text
                                } else {
                                    "`/diff` — _not inside a git repository_".to_string()
                                }
                            }
                            Err(e) => format!("Failed to compute diff: {e}"),
                        };
                        tx.send(AppEvent::DiffResult(text));
                    });
                }
            }
            WorkerEvent::PlanUpdated { explanation, steps } => {
                self.on_plan_updated(explanation, steps);
                self.set_status_message("Plan updated");
            }
            WorkerEvent::PatchApplied { changes } => {
                self.add_to_history(history_cell::new_patch_event(
                    changes,
                    &self.session.cwd,
                ));
                self.set_status_message("Patch applied");
            }
            WorkerEvent::ApprovalRequest {
                session_id,
                turn_id,
                approval_id,
                action_summary,
                justification,
                resource,
                available_scopes,
                path,
                host,
                target,
            } => {
                self.commit_active_streams(DotStatus::Completed);
                self.pending_approval = Some(PendingApprovalRequest {
                    session_id,
                    turn_id,
                    approval_id: approval_id.clone(),
                    action_summary: action_summary.clone(),
                });
                self.bottom_pane
                    .open_popup_view(Box::new(ApprovalOverlay::new(
                        ApprovalOverlayRequest {
                            session_id,
                            turn_id,
                            approval_id,
                            action_summary,
                            justification,
                            resource,
                            available_scopes,
                            path,
                            host,
                            target,
                        },
                        self.app_event_tx.clone(),
                        self.active_accent_color(),
                    )));
                self.busy = true;
                self.bottom_pane.set_task_running(false);
                self.set_status_message("Approval required");
            }
            WorkerEvent::ApprovalDecision {
                approval_id: _,
                decision,
                scope,
            } => {
                self.pending_approval = None;
                let symbol = if decision == "approve" { "✔" } else { "✗" };
                self.add_to_history(history_cell::new_info_event(
                    format!("{symbol} Permission request {decision} ({scope})"),
                    None,
                ));
                self.bottom_pane.set_task_running(self.busy);
            }
            WorkerEvent::UsageUpdated {
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                last_query_total_tokens,
                last_query_input_tokens,
            } => {
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.total_cache_read_tokens = total_cache_read_tokens;
                self.last_query_total_tokens = last_query_total_tokens;
                self.last_query_input_tokens = last_query_input_tokens;
                self.prompt_token_estimate = total_input_tokens;
                self.frame_requester.schedule_frame();
            }
            WorkerEvent::TurnFinished {
                stop_reason,
                turn_count,
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                last_query_total_tokens,
                last_query_input_tokens,
                prompt_token_estimate,
            } => {
                self.commit_active_streams(DotStatus::Completed);
                self.active_tool_calls.clear();
                self.pending_tool_calls.clear();
                self.pending_approval = None;
                self.busy = false;
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.total_cache_read_tokens = total_cache_read_tokens;
                self.last_query_total_tokens = last_query_total_tokens;
                self.last_query_input_tokens = last_query_input_tokens;
                self.prompt_token_estimate = prompt_token_estimate;
                let model_name = self
                    .session
                    .model
                    .as_ref()
                    .map(|m| m.display_name.clone())
                    .or_else(|| self.session.model.as_ref().map(|m| m.slug.clone()))
                    .unwrap_or_default();
                let accent_color = self.active_accent_color();
                let elapsed = self
                    .bottom_pane
                    .status_widget()
                    .map(|status| status.elapsed_seconds())
                    .filter(|&secs| secs > 0);
                self.bottom_pane.set_task_running(false);
                self.set_status_message("Ready");
                let was_interrupted = stop_reason.contains("Interrupted");
                let cell = if was_interrupted {
                    history_cell::TurnSummaryCell::new_interrupted(model_name, accent_color)
                } else {
                    history_cell::TurnSummaryCell::new(model_name, elapsed, accent_color)
                };
                self.add_to_history(cell);
            }
            WorkerEvent::TurnFailed {
                message,
                turn_count,
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                prompt_token_estimate,
                last_query_input_tokens,
            } => {
                self.resume_browser_loading = false;
                self.commit_active_streams(DotStatus::Failed);
                self.active_tool_calls.clear();
                self.pending_tool_calls.clear();
                self.pending_approval = None;
                self.busy = false;
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.total_cache_read_tokens = total_cache_read_tokens;
                self.last_query_input_tokens = last_query_input_tokens;
                self.prompt_token_estimate = prompt_token_estimate;
                let model_name = self
                    .session
                    .model
                    .as_ref()
                    .map(|m| m.display_name.clone())
                    .or_else(|| self.session.model.as_ref().map(|m| m.slug.clone()))
                    .unwrap_or_default();
                self.add_to_history(history_cell::TurnSummaryCell::new_interrupted(
                    model_name,
                    self.active_accent_color(),
                ));
                self.add_to_history(history_cell::new_error_event(message));
                self.bottom_pane.set_task_running(false);
                self.set_status_message("Query failed; see error above");
            }
            WorkerEvent::ProviderValidationSucceeded { reply_preview } => {
                self.bottom_pane
                    .onboarding_on_validation_succeeded(reply_preview.clone());
                if let Some(result) = self.bottom_pane.poll_onboarding_result() {
                    self.handle_onboarding_result(result);
                }
                self.add_to_history(history_cell::new_info_event(
                    format!("Validation reply: {reply_preview}"),
                    Some("provider validation succeeded".to_string()),
                ));
                self.busy = false;
                self.set_default_placeholder();
                self.set_status_message("Onboarding complete");
            }
            WorkerEvent::ProviderValidationFailed { message } => {
                self.bottom_pane
                    .onboarding_on_validation_failed(message.clone());
                self.busy = false;
                self.add_to_history(history_cell::new_error_event_with_hint(
                    message,
                    Some("provider validation failed".to_string()),
                ));
                self.set_status_message("Provider validation failed");
            }
            WorkerEvent::SessionsListed { sessions } => {
                self.resume_browser_loading = false;
                self.open_resume_browser(sessions);
            }
            WorkerEvent::SkillsListed { body } => {
                self.add_markdown_history("Skills", &body);
                self.set_status_message("Skills loaded");
            }
            WorkerEvent::NewSessionPrepared {
                cwd,
                model,
                thinking,
                reasoning_effort,
                last_query_total_tokens: _,
                last_query_input_tokens: _,
                total_cache_read_tokens: _,
            } => {
                self.resume_browser_loading = false;
                self.session.cwd = cwd;
                self.update_session_request_model(model);
                self.thinking_selection = thinking;
                self.session.reasoning_effort = reasoning_effort;
                let should_append_header = self.history_has_non_header_content();
                self.active_cell = None;
                self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
                self.active_tool_calls.clear();
                self.pending_tool_calls.clear();
                self.active_text_items.clear();
                self.stream_chunking_policy.reset();
                self.busy = false;
                self.turn_count = 0;
                self.total_input_tokens = 0;
                self.total_output_tokens = 0;
                self.total_cache_read_tokens = 0;
                self.last_query_total_tokens = 0;
                self.last_query_input_tokens = 0;
                self.prompt_token_estimate = 0;
                if should_append_header {
                    self.push_session_header(/*is_first_run*/ false, None);
                } else {
                    self.refresh_header_box();
                }
                self.set_status_message("New session ready; send a prompt to start it");
            }
            WorkerEvent::SessionSwitched {
                session_id,
                cwd,
                title,
                model,
                thinking,
                reasoning_effort,
                total_input_tokens,
                total_output_tokens,
                total_cache_read_tokens,
                last_query_total_tokens,
                last_query_input_tokens,
                prompt_token_estimate,
                history_items,
                rich_history_items,
                loaded_item_count,
                pending_texts,
            } => {
                self.resume_browser_loading = false;
                self.session.cwd = cwd;
                if let Some(model) = model {
                    self.update_session_request_model(model);
                }
                self.thinking_selection = thinking;
                self.session.reasoning_effort = reasoning_effort;
                self.history.clear();
                self.next_history_flush_index = 0;
                self.active_text_items.clear();
                self.stream_chunking_policy.reset();
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.total_cache_read_tokens = total_cache_read_tokens;
                self.last_query_total_tokens = last_query_total_tokens;
                self.last_query_input_tokens = last_query_input_tokens;
                self.prompt_token_estimate = prompt_token_estimate;
                if !self.rebuild_restored_session_history_from_rich_items(
                    &rich_history_items,
                    loaded_item_count,
                    &session_id,
                    title.as_deref(),
                ) {
                    self.rebuild_restored_session_history(
                        history_items,
                        loaded_item_count,
                        &session_id,
                        title.as_deref(),
                    );
                }
                // Restore pending queue state from the resumed session
                self.queued_count = pending_texts.len();
                self.bottom_pane.clear_pending_cells();
                for text in &pending_texts {
                    self.bottom_pane.push_pending_cell(text.clone());
                }
                self.busy = false;
                self.set_status_message("Session switched");
            }
            WorkerEvent::SessionRenamed { session_id, title } => {
                self.add_to_history(history_cell::new_info_event(
                    format!("renamed {session_id} to {title}"),
                    None,
                ));
                self.set_status_message("Session renamed");
            }
            WorkerEvent::SessionCompactionStarted => {
                self.busy = true;
                self.bottom_pane.set_task_running(true);
                self.set_status_message("Session compaction in progress");
            }
            WorkerEvent::SessionCompacted {
                total_input_tokens,
                total_output_tokens,
                prompt_token_estimate,
            } => {
                self.busy = false;
                self.bottom_pane.set_task_running(false);
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.prompt_token_estimate = prompt_token_estimate;
                self.add_to_history(history_cell::new_info_event(
                    "Session compaction done".to_string(),
                    None,
                ));
                self.set_status_message("Session compacted");
            }
            WorkerEvent::SessionCompactionFailed { message } => {
                self.busy = false;
                self.bottom_pane.set_task_running(false);
                self.add_to_history(history_cell::new_error_event_with_hint(
                    message,
                    Some("session compaction failed".to_string()),
                ));
                self.set_status_message("Session compaction failed");
            }
            WorkerEvent::SessionTitleUpdated {
                session_id: _,
                title,
            } => {
                self.set_status_message(format!("Session: {title}"));
            }
            WorkerEvent::InputHistoryLoaded { direction: _, text } => {
                self.bottom_pane.restore_input_from_history(text);
            }
            WorkerEvent::InputQueueUpdated { pending_count, .. } => {
                // If the queue shrunk, unqueue the oldest queued cells.
                while self.queued_count > pending_count {
                    self.unqueue_oldest_pending();
                }
                self.frame_requester.schedule_frame();
            }
            WorkerEvent::SteerAccepted { .. } => {
                self.set_status_message("Steer accepted");
            }
        }
    }

    fn on_plan_updated(&mut self, explanation: Option<String>, steps: Vec<PlanStep>) {
        let total = steps.len();
        let completed = steps
            .iter()
            .filter(|step| matches!(step.status, PlanStepStatus::Completed))
            .count();
        self.last_plan_progress = (total > 0).then_some((completed, total));

        let mut lines = vec![
            Line::from(vec![
                Span::styled("▌", Style::default().fg(Color::Rgb(120, 220, 160))),
                " ".into(),
                "Updated Plan".bold(),
            ]),
        ];
        if let Some(explanation) = explanation
            && !explanation.trim().is_empty()
        {
            lines.push(Line::from(""));
            lines.push(Line::from(explanation.italic()));
            lines.push(Line::from(""));
        }
        for step in steps {
            let (prefix, style) = match step.status {
                PlanStepStatus::Completed => ("✔ ", Style::default().green()),
                PlanStepStatus::InProgress => ("→ ", Style::default().cyan()),
                PlanStepStatus::Pending => ("□ ", Style::default().dim()),
                PlanStepStatus::Cancelled => ("✗ ", Style::default().red()),
            };
            lines.extend(prefix_lines(
                vec![Line::from(Span::styled(step.text, style))],
                Span::styled(format!("  {prefix}"), style),
                Span::from("    "),
            ));
        }
        if !lines.is_empty() {
            self.add_to_history(PlainHistoryCell::new(lines));
        }
        self.frame_requester.schedule_frame();
    }

    fn restore_explored_history_item(
        &mut self,
        item: &SessionHistoryItem,
        actions: Vec<devo_protocol::parse_command::ParsedCommand>,
    ) {
        let command = item.title.clone();
        let command_tokens = crate::exec_command::split_command_string(&command);
        if let Some(cell) = self
            .history
            .last_mut()
            .and_then(|cell| cell.as_any_mut().downcast_mut::<ExecCell>())
            && let Some(grouped) = cell.with_added_call(
                item.tool_call_id
                    .clone()
                    .unwrap_or_else(|| "restored".to_string()),
                command_tokens.clone(),
                actions.clone(),
                devo_protocol::protocol::ExecCommandSource::Agent,
                None,
            )
        {
            *cell = grouped;
            return;
        }

        let exec = new_active_exec_command(
            item.tool_call_id
                .clone()
                .unwrap_or_else(|| "restored".to_string()),
            command_tokens,
            actions,
            devo_protocol::protocol::ExecCommandSource::Agent,
            None,
            false,
        );
        self.add_history_entry_without_redraw(Box::new(exec));
    }

    pub(crate) fn submit_text(&mut self, text: String) {
        self.submit_user_message(UserMessage::from(text));
    }

    fn submit_user_message(&mut self, user_message: UserMessage) {
        if let Some(result) = self.bottom_pane.poll_onboarding_result() {
            self.handle_onboarding_result(result);
        }
        if user_message.text.trim().is_empty() {
            return;
        }

        let local_image_paths = user_message
            .local_images
            .iter()
            .map(|attachment| attachment.path.clone())
            .collect::<Vec<_>>();
        self.add_to_history(history_cell::new_user_prompt(
            user_message.text.clone(),
            user_message.text_elements.clone(),
            local_image_paths,
            user_message.remote_image_urls.clone(),
            self.active_accent_color(),
        ));

        self.app_event_tx
            .send(AppEvent::Command(AppCommand::user_turn(
                vec![InputItem::Text {
                    text: user_message.text,
                }],
                Some(self.session.cwd.clone()),
                self.session.model.as_ref().map(|model| model.slug.clone()),
                self.thinking_selection.clone(),
                /*sandbox*/ None,
                Some("on-request".to_string()),
            )));
        self.set_status_message("Submitted locally");
    }

    fn handle_slash_command(&mut self, command: SlashCommand, argument: String) {
        if !self.can_change_configuration() && !command.available_during_task() {
            self.add_busy_configuration_message(command);
            return;
        }

        match command {
            SlashCommand::Exit => {
                self.app_event_tx
                    .send(AppEvent::Exit(crate::app_event::ExitMode::ShutdownFirst));
            }
            SlashCommand::Clear => {
                self.history.clear();
                self.next_history_flush_index = 0;
                self.active_text_items.clear();
                self.stream_chunking_policy.reset();
                self.set_status_message("Transcript cleared");
            }
            SlashCommand::Onboard => {
                self.begin_onboarding();
            }
            SlashCommand::Status => {
                let model = self
                    .session
                    .model
                    .as_ref()
                    .map(|m| m.slug.as_str())
                    .unwrap_or("unknown");
                let thinking = self.thinking_selection.as_deref().unwrap_or("default");
                let cwd = self.session.cwd.display().to_string();
                let turns = self.turn_count;
                let tokens_in = Self::format_token_count(self.total_input_tokens);
                let tokens_out = Self::format_token_count(self.total_output_tokens);
                let lines = history_cell::with_border(vec![
                    Line::from("Session Status".bold()),
                    Line::from(""),
                    Line::from(format!("  model:       {model}")),
                    Line::from(format!("  thinking:    {thinking}")),
                    Line::from(format!("  cwd:         {cwd}")),
                    Line::from(format!("  turns:       {turns}")),
                    Line::from(format!(
                        "  tokens:      \u{2191}{tokens_in} \u{2193}{tokens_out}",
                    )),
                ]);
                self.add_to_history(PlainHistoryCell::new(lines));
                self.set_status_message("Session status shown");
            }
            SlashCommand::Permissions => {
                self.open_permissions_picker();
            }
            SlashCommand::Theme => {
                self.open_theme_picker();
            }
            SlashCommand::Model => {
                if argument.is_empty() {
                    self.open_model_picker();
                } else {
                    self.apply_model_selection(argument);
                }
            }
            SlashCommand::Compact => {
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::compact()));
            }
            SlashCommand::New => {
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                        command: "session new".to_string(),
                    }));
                self.set_status_message("New session requested");
            }
            SlashCommand::Resume => {
                self.resume_browser = None;
                self.resume_browser_loading = true;
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                        command: "session list".to_string(),
                    }));
                self.set_status_message("Loading sessions");
            }
            SlashCommand::Btw => {
                if let Some(turn_id) = self.active_turn_id {
                    self.add_to_history(history_cell::new_user_prompt(
                        format!("/btw {argument}"),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        self.active_accent_color(),
                    ));
                    self.app_event_tx
                        .send(AppEvent::Command(AppCommand::SteerTurn {
                            input: vec![devo_protocol::InputItem::Text { text: argument }],
                            expected_turn_id: turn_id,
                        }));
                    self.set_status_message("Steer sent");
                } else {
                    self.set_status_message("No active turn to steer");
                }
            }
            SlashCommand::Diff => {
                self.set_status_message("Computing diff");
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let text = match get_git_diff().await {
                        Ok((is_git_repo, diff_text)) => {
                            if is_git_repo {
                                diff_text
                            } else {
                                "`/diff` — _not inside a git repository_".to_string()
                            }
                        }
                        Err(e) => format!("Failed to compute diff: {e}"),
                    };
                    tx.send(AppEvent::DiffResult(text));
                });
            }
        }
    }

    fn begin_onboarding(&mut self) {
        self.onboarding_step = None;
        self.history.clear();
        self.next_history_flush_index = 0;
        self.bottom_pane.start_onboarding(&self.available_models);
        self.set_status_message("Onboarding");
    }

    fn handle_onboarding_result(&mut self, result: crate::bottom_pane::OnboardingResult) {
        use crate::bottom_pane::OnboardingResult;
        match result {
            OnboardingResult::ValidationSucceeded {
                model,
                provider: _,
                base_url: _,
                api_key: _,
            } => {
                self.update_session_request_model(model);
                self.add_to_history(history_cell::new_info_event(
                    "Provider configured successfully".to_string(),
                    Some("onboarding complete".to_string()),
                ));
                self.set_default_placeholder();
                self.set_status_message("Onboarding complete");
            }
            OnboardingResult::Validate {
                model,
                provider: _,
                base_url,
                api_key,
            } => {
                self.onboarding_step = Some(OnboardingStep::Validating {
                    model: model.clone(),
                    base_url: base_url.clone(),
                    api_key: api_key.clone(),
                });
                let payload = serde_json::json!({
                    "model": model,
                    "base_url": base_url,
                    "api_key": api_key,
                });
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                        command: format!("onboard {payload}"),
                    }));
                self.set_status_message("Validating provider connection");
            }
            OnboardingResult::Cancelled => {
                self.onboarding_step = None;
                self.set_default_placeholder();
                self.set_status_message("Ready");
            }
            _ => {}
        }
    }

    pub(crate) fn set_model(&mut self, model: Model) {
        self.thinking_selection = model.default_thinking_selection();
        self.session.reasoning_effort = model
            .resolve_thinking_selection(self.thinking_selection.as_deref())
            .effective_reasoning_effort;
        self.session.provider = Some(model.provider_wire_api());
        self.session.model = Some(model);
        if self.onboarding_step.is_none() {
            self.set_default_placeholder();
        }
        self.frame_requester.schedule_frame();
    }

    fn update_session_request_model(&mut self, slug: String) {
        if let Some(model) = self
            .available_models
            .iter()
            .find(|model| model.slug == slug)
            .cloned()
        {
            self.session.reasoning_effort = model
                .resolve_thinking_selection(self.thinking_selection.as_deref())
                .effective_reasoning_effort;
            self.session.provider = Some(model.provider_wire_api());
            self.session.model = Some(model);
            return;
        }

        if let Some(model) = self.session.model.as_mut() {
            model.slug = slug.clone();
            model.display_name = slug;
            self.session.reasoning_effort = model
                .resolve_thinking_selection(self.thinking_selection.as_deref())
                .effective_reasoning_effort;
            return;
        }

        self.session.model = Some(Model {
            slug: slug.clone(),
            display_name: slug,
            provider: self
                .session
                .provider
                .unwrap_or(ProviderWireApi::OpenAIChatCompletions),
            ..Model::default()
        });
        self.session.reasoning_effort = self
            .session
            .model
            .as_ref()
            .map(|model| model.resolve_thinking_selection(self.thinking_selection.as_deref()))
            .and_then(|resolved| resolved.effective_reasoning_effort);
    }

    fn add_markdown_history(&mut self, title: &str, body: &str) {
        self.add_markdown_history_with_status(title, body, DotStatus::Completed);
    }

    fn add_markdown_history_with_status(&mut self, title: &str, body: &str, status: DotStatus) {
        self.add_markdown_history_with_status_without_redraw(title, body, status);
        self.frame_requester.schedule_frame();
    }

    fn add_markdown_history_without_redraw(&mut self, title: &str, body: &str) {
        self.add_markdown_history_with_status_without_redraw(title, body, DotStatus::Completed);
    }

    fn add_markdown_history_with_status_without_redraw(
        &mut self,
        title: &str,
        body: &str,
        status: DotStatus,
    ) {
        let is_ai_message = title == "Assistant" || title == "Reasoning";
        let mut lines = if is_ai_message {
            Vec::new()
        } else {
            vec![Line::from(title.to_string()).bold()]
        };
        if title == "Reasoning" {
            let mut body_lines = Vec::new();
            append_markdown(
                body,
                /*width*/ None,
                Some(&self.session.cwd),
                &mut body_lines,
            );
            Self::patch_lines_style(&mut body_lines, Self::reasoning_text_style());
            if let Some(first_line) = body_lines.first_mut() {
                first_line.spans.insert(
                    0,
                    Span::styled("Thinking: ", Self::reasoning_heading_style()),
                );
            }
            lines.extend(body_lines);
        } else {
            append_markdown(body, None, Some(&self.session.cwd), &mut lines);
        }
        if is_ai_message {
            self.add_history_entry_without_redraw(Box::new(
                history_cell::AgentMessageCell::new_ai_response_with_prefix(
                    lines,
                    self.dot_prefix(status),
                    "  ",
                    false,
                ),
            ));
        } else {
            self.add_history_entry_without_redraw(Box::new(PlainHistoryCell::new(lines)));
        }
    }

    fn bulleted_markdown_lines(
        &self,
        body: &str,
        width: u16,
        prefix: Line<'static>,
    ) -> Vec<Line<'static>> {
        self.bulleted_markdown_cell(body, prefix)
            .display_lines(width.max(1))
    }

    fn bulleted_markdown_cell(
        &self,
        body: &str,
        prefix: Line<'static>,
    ) -> history_cell::AgentMessageCell {
        self.bulleted_markdown_cell_with_style(body, prefix, Style::default())
    }

    fn bulleted_markdown_cell_with_style(
        &self,
        body: &str,
        prefix: Line<'static>,
        style: Style,
    ) -> history_cell::AgentMessageCell {
        let mut lines = Vec::new();
        append_markdown(
            body,
            /*width*/ None,
            Some(&self.session.cwd),
            &mut lines,
        );
        Self::patch_lines_style(&mut lines, style);
        history_cell::AgentMessageCell::new_ai_response_with_prefix(lines, prefix, "  ", false)
    }

    fn add_transcript_item(&mut self, item: TranscriptItem) {
        self.add_transcript_item_without_redraw(item);
        self.frame_requester.schedule_frame();
    }

    fn add_transcript_item_without_redraw(&mut self, item: TranscriptItem) {
        match item.kind {
            TranscriptItemKind::User => {
                self.add_history_entry_without_redraw(Box::new(history_cell::new_user_prompt(
                    item.body,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    self.active_accent_color(),
                )));
            }
            TranscriptItemKind::Assistant => {
                self.add_markdown_history_without_redraw("Assistant", &item.body)
            }
            TranscriptItemKind::Reasoning => {
                self.add_markdown_history_without_redraw("Reasoning", &item.body);
            }
            TranscriptItemKind::ToolCall => {
                self.add_history_entry_without_redraw(Box::new(
                    history_cell::AgentMessageCell::new_with_prefix(
                        vec![Self::running_tool_line(&item.title)],
                        self.dot_prefix(DotStatus::Pending),
                        "  ",
                        false,
                    ),
                ));
            }
            TranscriptItemKind::ToolResult => {
                self.add_history_entry_without_redraw(Box::new(ToolResultCell::new(
                    (!item.title.is_empty()).then(|| Self::ran_tool_line(&item.title)),
                    item.body,
                    Self::tool_dot_prefix(),
                    Line::from("  "),
                    Self::tool_text_style(),
                    false,
                )));
            }
            TranscriptItemKind::Error => self.add_history_entry_without_redraw(Box::new(
                history_cell::new_error_event_with_hint(item.body, Some(item.title)),
            )),
            TranscriptItemKind::Approval => {}
            TranscriptItemKind::System => {
                self.add_history_entry_without_redraw(Box::new(history_cell::new_info_event(
                    item.title,
                    Some(item.body),
                )));
            }
            TranscriptItemKind::TurnSummary => {
                // item.title contains model name, item.duration_ms contains seconds
                self.add_history_entry_without_redraw(Box::new(
                    history_cell::TurnSummaryCell::new(
                        item.title.clone(),
                        item.duration_ms,
                        self.active_accent_color(),
                    ),
                ));
            }
        }
    }

    fn commit_active_streams(&mut self, status: DotStatus) {
        tracing::debug!(
            status = ?status,
            active_items = ?self.active_text_item_log_order(),
            "committing all active text items"
        );
        while !self.active_text_items.is_empty() {
            self.commit_text_item_at(0, status);
        }
    }

    fn start_text_item(&mut self, item_id: ActiveTextItemId, kind: TextItemKind) {
        if self
            .active_text_items
            .iter()
            .any(|item| item.item_id == item_id)
        {
            return;
        }

        let stream_controller = match kind {
            TextItemKind::Assistant => Some(StreamController::new(None, &self.session.cwd)),
            TextItemKind::Reasoning => None,
        };
        let insert_index = self.active_text_item_insert_index(kind);
        tracing::debug!(
            item_id = %item_id.log_label(),
            kind = ?kind,
            insert_index,
            before = ?self.active_text_item_log_order(),
            "starting active text item"
        );
        self.active_text_items.insert(
            insert_index,
            ActiveTextItem {
                item_id,
                kind,
                status: DotStatus::Pending,
                stream_controller,
                raw_text: String::new(),
                cell: None,
            },
        );
        tracing::trace!(
            after = ?self.active_text_item_log_order(),
            "active text item order after start"
        );
        self.stream_chunking_policy.reset();
    }

    fn push_text_item_delta(&mut self, item_id: ActiveTextItemId, kind: TextItemKind, delta: &str) {
        let index = self.ensure_text_item(item_id, kind);
        tracing::debug!(
            item_id = %item_id.log_label(),
            kind = ?kind,
            delta_len = delta.len(),
            active_items = ?self.active_text_item_log_order(),
            "received active text item delta"
        );
        match kind {
            TextItemKind::Assistant => {
                if let Some(controller) = self.active_text_items[index].stream_controller.as_mut() {
                    controller.push(delta);
                }
            }
            TextItemKind::Reasoning => {
                self.active_text_items[index].raw_text.push_str(delta);
            }
        }
        self.sync_text_item_cell(index);
        self.frame_requester.schedule_frame();
    }

    fn complete_text_item(
        &mut self,
        item_id: ActiveTextItemId,
        kind: TextItemKind,
        final_text: String,
    ) {
        let index = self.ensure_text_item(item_id, kind);
        tracing::debug!(
            item_id = %item_id.log_label(),
            kind = ?kind,
            final_text_len = final_text.len(),
            active_items = ?self.active_text_item_log_order(),
            "completed active text item"
        );
        self.active_text_items[index].status = DotStatus::Completed;
        if !final_text.trim().is_empty() {
            self.active_text_items[index].raw_text = final_text;
        }
        self.sync_text_item_cell(index);
        self.commit_completed_text_items();
    }

    fn ensure_text_item(&mut self, item_id: ActiveTextItemId, kind: TextItemKind) -> usize {
        if let Some(index) = self
            .active_text_items
            .iter()
            .position(|item| item.item_id == item_id)
        {
            return index;
        }

        self.start_text_item(item_id, kind);
        self.active_text_items
            .iter()
            .position(|item| item.item_id == item_id)
            .unwrap_or_else(|| self.active_text_items.len().saturating_sub(1))
    }

    fn has_server_active_item(&self, kind: TextItemKind) -> bool {
        self.active_text_items
            .iter()
            .any(|item| matches!(item.item_id, ActiveTextItemId::Server(_)) && item.kind == kind)
    }

    fn commit_text_item_at(&mut self, index: usize, status: DotStatus) {
        if index >= self.active_text_items.len() {
            return;
        }

        let mut item = self.active_text_items.remove(index);
        tracing::debug!(
            item_id = %item.item_id.log_label(),
            kind = ?item.kind,
            status = ?status,
            remaining = ?self.active_text_item_log_order(),
            "committing active text item"
        );
        match item.kind {
            TextItemKind::Assistant => {
                if let Some(controller) = item.stream_controller.as_mut() {
                    let (_cell, source) = controller.finalize();
                    if let Some(source) = source {
                        self.add_assistant_markdown_source(source, status);
                    } else if !item.raw_text.trim().is_empty() {
                        self.add_markdown_history_with_status_without_redraw(
                            "Assistant",
                            &item.raw_text,
                            status,
                        );
                    }
                } else if !item.raw_text.trim().is_empty() {
                    self.add_markdown_history_with_status_without_redraw(
                        "Assistant",
                        &item.raw_text,
                        status,
                    );
                }
            }
            TextItemKind::Reasoning => {
                if !item.raw_text.trim().is_empty() {
                    self.add_markdown_history_with_status("Reasoning", &item.raw_text, status);
                }
            }
        }
        self.stream_chunking_policy.reset();
    }

    fn add_assistant_markdown_source(&mut self, source: String, status: DotStatus) {
        if source.trim().is_empty() {
            return;
        }

        self.add_history_entry_without_redraw(Box::new(history_cell::AgentMarkdownCell::new(
            source,
            &self.session.cwd,
            self.dot_prefix(status),
            "  ",
        )));
    }

    fn active_text_item_insert_index(&self, kind: TextItemKind) -> usize {
        match kind {
            TextItemKind::Reasoning => self
                .active_text_items
                .iter()
                .position(|item| item.kind == TextItemKind::Assistant)
                .unwrap_or(self.active_text_items.len()),
            TextItemKind::Assistant => self.active_text_items.len(),
        }
    }

    fn commit_completed_text_items(&mut self) {
        let mut index = 0;
        while index < self.active_text_items.len() {
            let item = &self.active_text_items[index];
            if item.status != DotStatus::Completed {
                index += 1;
                continue;
            }

            if item.kind == TextItemKind::Assistant
                && self.active_text_items[..index]
                    .iter()
                    .any(|prior| prior.kind == TextItemKind::Reasoning)
            {
                tracing::debug!(
                    item_id = %item.item_id.log_label(),
                    active_items = ?self.active_text_item_log_order(),
                    "deferring assistant commit until prior reasoning item commits"
                );
                index += 1;
                continue;
            }

            self.commit_text_item_at(index, DotStatus::Completed);
        }
    }

    fn active_text_item_log_order(&self) -> Vec<String> {
        self.active_text_items
            .iter()
            .map(|item| {
                format!(
                    "{:?}:{}:{:?}",
                    item.kind,
                    item.item_id.log_label(),
                    item.status
                )
            })
            .collect()
    }

    fn run_stream_commit_tick(&mut self) {
        let now = Instant::now();
        let mut output_cells = Vec::new();
        let mut needs_followup = false;
        let mut changed_indexes = Vec::new();

        for (index, item) in self.active_text_items.iter_mut().enumerate() {
            let Some(controller) = item.stream_controller.as_mut() else {
                continue;
            };
            let output = run_commit_tick(
                &mut self.stream_chunking_policy,
                Some(controller),
                CommitTickScope::AnyMode,
                now,
            );
            if item.kind == TextItemKind::Assistant {
                if !output.cells.is_empty() {
                    changed_indexes.push(index);
                }
                if !output.all_idle {
                    needs_followup = true;
                }
                continue;
            }
            if !output.cells.is_empty() {
                output_cells.extend(output.cells);
                changed_indexes.push(index);
            }
            if !output.all_idle {
                needs_followup = true;
            }
        }

        for cell in output_cells {
            self.add_history_entry_without_redraw(cell);
        }
        for index in changed_indexes {
            self.sync_text_item_cell(index);
        }
        if needs_followup {
            self.frame_requester
                .schedule_frame_in(std::time::Duration::from_millis(16));
        }
        if !self.active_text_items.is_empty() {
            self.frame_requester.schedule_frame();
        }
    }

    fn sync_text_item_cell(&mut self, index: usize) {
        if index >= self.active_text_items.len() {
            return;
        }

        let cell = match self.active_text_items[index].kind {
            TextItemKind::Assistant => self.assistant_active_cell(&self.active_text_items[index]),
            TextItemKind::Reasoning => self.reasoning_active_cell(&self.active_text_items[index]),
        };
        self.active_text_items[index].cell = cell;
        self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
    }

    fn assistant_active_cell(
        &self,
        item: &ActiveTextItem,
    ) -> Option<history_cell::AgentMessageCell> {
        if let Some(controller) = &item.stream_controller {
            let lines = controller.live_lines();
            if lines.iter().any(|line| !Self::is_blank_line(line)) {
                return Some(history_cell::AgentMessageCell::new_ai_response_with_prefix(
                    lines,
                    Self::pending_dot_prefix(),
                    "  ",
                    false,
                ));
            }
        } else if !item.raw_text.trim().is_empty() {
            return Some(self.bulleted_markdown_cell(&item.raw_text, Self::pending_dot_prefix()));
        }
        None
    }

    fn reasoning_active_cell(
        &self,
        item: &ActiveTextItem,
    ) -> Option<history_cell::AgentMessageCell> {
        if item.raw_text.trim().is_empty() {
            return None;
        }

        let mut body_lines = Vec::new();
        append_markdown(
            &item.raw_text,
            None,
            Some(&self.session.cwd),
            &mut body_lines,
        );
        Self::patch_lines_style(&mut body_lines, Self::reasoning_text_style());
        if let Some(first_line) = body_lines.first_mut() {
            first_line.spans.insert(
                0,
                Span::styled("Thinking: ", Self::reasoning_heading_style()),
            );
        }
        Some(history_cell::AgentMessageCell::new_ai_response_with_prefix(
            body_lines,
            Self::reasoning_dot_prefix(item.status),
            "  ",
            false,
        ))
    }

    fn last_known_width(&self) -> u16 {
        crossterm::terminal::size()
            .map(|(width, _height)| width)
            .unwrap_or(80)
    }

    fn tool_preview_lines(&self, preview: &str) -> Vec<Line<'static>> {
        let width = self.last_known_width().saturating_sub(2).max(1);
        let mut preview_lines =
            truncated_tool_output_preview(preview, width, 2, crate::exec_cell::TOOL_CALL_MAX_LINES);
        for line in &mut preview_lines {
            line.spans = line
                .spans
                .clone()
                .into_iter()
                .map(|span| span.patch_style(Self::tool_text_style()))
                .collect();
        }
        preview_lines
    }

    pub(crate) fn set_thinking_selection(&mut self, selection: Option<String>) {
        self.thinking_selection = selection;
        self.session.reasoning_effort = self
            .session
            .model
            .as_ref()
            .map(|model| model.resolve_thinking_selection(self.thinking_selection.as_deref()))
            .and_then(|resolved| resolved.effective_reasoning_effort);
        self.refresh_header_box();
        self.frame_requester.schedule_frame();
    }

    fn refresh_header_box(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let accent = self.active_accent_color();
        self.history[0] = Self::build_header_box(
            &self.session.cwd,
            self.session.model.as_ref(),
            self.thinking_selection.as_deref(),
            /*is_first_run*/ false,
            None,
            accent,
            self.startup_header_mascot_frame_index,
        );
    }

    fn advance_startup_header_animation(&mut self) {
        let now = Instant::now();
        if self
            .history
            .first()
            .and_then(|cell| {
                cell.as_any()
                    .downcast_ref::<history_cell::SessionInfoCell>()
            })
            .is_none()
        {
            return;
        }

        self.frame_requester
            .schedule_frame_in(STARTUP_HEADER_ANIMATION_INTERVAL);
        if now < self.startup_header_next_animation_at {
            return;
        }

        self.startup_header_mascot_frame_index = (self.startup_header_mascot_frame_index + 1) % 3;
        self.startup_header_next_animation_at = now + STARTUP_HEADER_ANIMATION_INTERVAL;
        self.refresh_header_box();
    }

    pub(crate) fn current_model(&self) -> Option<&Model> {
        self.session.model.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn current_cwd(&self) -> &std::path::Path {
        &self.session.cwd
    }

    #[cfg(test)]
    pub(crate) fn startup_header_mascot_frame_index(&self) -> usize {
        self.startup_header_mascot_frame_index
    }

    #[cfg(test)]
    pub(crate) fn has_stream_controller(&self) -> bool {
        self.active_text_items
            .iter()
            .any(|item| item.stream_controller.is_some())
    }

    #[cfg(test)]
    pub(crate) fn force_startup_header_animation_due(&mut self) {
        self.startup_header_next_animation_at = Instant::now();
    }

    #[cfg(test)]
    pub(crate) fn force_task_elapsed_seconds(&mut self, secs: u64) {
        self.bottom_pane.set_task_running(true);
        if let Some(status) = self.bottom_pane.status_widget_mut() {
            let now = Instant::now();
            status.pause_timer_at(now);
            let resume_at = now
                .checked_sub(std::time::Duration::from_secs(secs))
                .unwrap_or(now);
            status.resume_timer_at(resume_at);
        }
    }

    #[cfg(test)]
    pub(crate) fn placeholder_text(&self) -> &str {
        self.bottom_pane.placeholder_text()
    }

    #[cfg(test)]
    pub(crate) fn status_summary_text(&self) -> String {
        self.session_summary_text()
    }

    pub(crate) fn current_thinking_selection(&self) -> Option<&str> {
        self.thinking_selection.as_deref()
    }

    pub(crate) fn current_reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.session.reasoning_effort.or_else(|| {
            self.session
                .model
                .as_ref()
                .map(|model| model.resolve_thinking_selection(self.thinking_selection.as_deref()))
                .and_then(|resolved| resolved.effective_reasoning_effort)
        })
    }

    fn reasoning_text_style() -> Style {
        Style::default().dim()
    }

    fn reasoning_heading_style() -> Style {
        Style::default().italic().fg(Color::Rgb(210, 150, 60))
    }

    fn patch_lines_style(lines: &mut [Line<'static>], style: Style) {
        if style == Style::default() {
            return;
        }

        for line in lines {
            line.spans = line
                .spans
                .drain(..)
                .map(|span| span.patch_style(style))
                .collect();
        }
    }

    fn normalized_thinking_selection_for_display(&self, model: &Model) -> Option<String> {
        let current = self
            .thinking_selection
            .as_deref()
            .map(str::trim)
            .filter(|selection| !selection.is_empty())
            .map(str::to_ascii_lowercase)
            .or_else(|| model.default_thinking_selection());

        match model.effective_thinking_capability() {
            ThinkingCapability::ToggleWithLevels(_) => {
                if matches!(current.as_deref(), Some("disabled")) {
                    Some(String::from("disabled"))
                } else {
                    model
                        .resolve_thinking_selection(current.as_deref())
                        .effective_reasoning_effort
                        .map(|effort| effort.label().to_lowercase())
                }
            }
            _ => current,
        }
    }

    fn display_thinking_selection(&self) -> Option<String> {
        let model = self.session.model.as_ref()?;
        self.normalized_thinking_selection_for_display(model)
    }

    pub(crate) fn thinking_entries(&self) -> Vec<ThinkingListEntry> {
        let Some(model) = &self.session.model else {
            return Vec::new();
        };

        let current = self
            .normalized_thinking_selection_for_display(model)
            .unwrap_or_default();

        model
            .effective_thinking_capability()
            .options()
            .into_iter()
            .map(|option| ThinkingListEntry {
                is_current: option.value == current || option.label.to_lowercase() == current,
                label: option.label,
                description: option.description,
                value: option.value,
            })
            .collect()
    }

    pub(crate) fn status_line_reasoning_effort_label(
        effort: Option<ReasoningEffort>,
    ) -> &'static str {
        match effort {
            Some(ReasoningEffort::None) | None => "default",
            Some(ReasoningEffort::Minimal) => "minimal",
            Some(ReasoningEffort::Low) => "low",
            Some(ReasoningEffort::Medium) => "medium",
            Some(ReasoningEffort::High) => "high",
            Some(ReasoningEffort::XHigh) => "xhigh",
            Some(ReasoningEffort::Max) => "max",
        }
    }

    pub(crate) fn reasoning_effort_label(effort: ReasoningEffort) -> &'static str {
        match effort {
            ReasoningEffort::None => "None",
            ReasoningEffort::Minimal => "Minimal",
            ReasoningEffort::Low => "Low",
            ReasoningEffort::Medium => "Medium",
            ReasoningEffort::High => "High",
            ReasoningEffort::XHigh => "Extra high",
            ReasoningEffort::Max => "max",
        }
    }

    pub(crate) fn thinking_label(
        capability: &ThinkingCapability,
        implementation: Option<&ThinkingImplementation>,
        default_reasoning_effort: Option<ReasoningEffort>,
    ) -> Option<&'static str> {
        if matches!(capability, ThinkingCapability::Unsupported)
            || matches!(implementation, Some(ThinkingImplementation::Disabled))
        {
            return None;
        }

        match capability {
            ThinkingCapability::Unsupported => None,
            ThinkingCapability::Toggle => Some("thinking"),
            ThinkingCapability::ToggleWithLevels(levels) => default_reasoning_effort
                .or_else(|| levels.first().copied())
                .map(|effort| Self::status_line_reasoning_effort_label(Some(effort))),
            ThinkingCapability::Levels(levels) => default_reasoning_effort
                .or_else(|| levels.first().copied())
                .map(|effort| Self::status_line_reasoning_effort_label(Some(effort))),
        }
    }

    pub(crate) fn reasoning_effort_options(model: &Model) -> Vec<ReasoningEffortPreset> {
        model.reasoning_effort_options()
    }

    pub(crate) fn thinking_options(model: &Model) -> Vec<ThinkingPreset> {
        model.effective_thinking_capability().options()
    }

    pub(crate) fn add_to_history(&mut self, cell: impl HistoryCell + 'static) {
        self.add_history_entry_without_redraw(Box::new(cell));
        self.frame_requester.schedule_frame();
    }

    fn flush_active_cell(&mut self) {
        if let Some(active) = self.active_cell.take() {
            self.add_history_entry_without_redraw(active);
        }
    }

    fn bump_active_cell_revision(&mut self) {
        self.active_cell_revision = self.active_cell_revision.wrapping_add(1);
    }

    /// Pop the oldest pending cell from the bottom pane and add it to history
    /// as a normal user input cell.
    fn unqueue_oldest_pending(&mut self) {
        if let Some(text) = self.bottom_pane.pop_oldest_pending_cell() {
            self.add_to_history(history_cell::new_user_prompt(
                text,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                self.active_accent_color(),
            ));
        }
        self.queued_count = self.queued_count.saturating_sub(1);
    }

    fn add_history_entry_without_redraw(&mut self, cell: Box<dyn HistoryCell>) {
        self.history.push(cell);
    }

    pub(crate) fn active_cell_transcript_key(&self) -> Option<ActiveCellTranscriptKey> {
        let active_cell = self.active_cell.as_ref()?;
        Some(ActiveCellTranscriptKey {
            revision: self.active_cell_revision,
            is_stream_continuation: active_cell.is_stream_continuation(),
            animation_tick: active_cell.transcript_animation_tick(),
        })
    }

    pub(crate) fn active_cell_transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.active_cell
            .as_ref()
            .map(|cell| cell.transcript_lines(width))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn active_cell_display_lines_for_test(&self, width: u16) -> Vec<Line<'static>> {
        self.active_cell
            .as_ref()
            .map(|cell| cell.display_lines(width))
            .unwrap_or_default()
    }

    pub(crate) fn transcript_overlay_cell_count(&self) -> usize {
        self.history.len()
    }

    pub(crate) fn transcript_overlay_cells(&self, width: u16) -> Vec<TranscriptOverlayCell> {
        let width = width.max(1);
        self.history
            .iter()
            .map(|cell| TranscriptOverlayCell {
                lines: cell.transcript_lines(width),
                is_stream_continuation: cell.is_stream_continuation(),
            })
            .collect()
    }

    pub(crate) fn transcript_overlay_live_tail_key(&self) -> Option<ActiveCellTranscriptKey> {
        if !self.transcript_overlay_has_live_tail() {
            return None;
        }

        let active_cell = self.active_cell.as_ref();
        Some(ActiveCellTranscriptKey {
            revision: self.active_cell_revision,
            is_stream_continuation: active_cell.is_some_and(|cell| cell.is_stream_continuation()),
            animation_tick: active_cell.and_then(|cell| cell.transcript_animation_tick()),
        })
    }

    pub(crate) fn transcript_overlay_live_tail_lines(
        &self,
        width: u16,
    ) -> Option<Vec<Line<'static>>> {
        self.transcript_overlay_has_live_tail()
            .then(|| self.live_transcript_lines(width.max(1)))
    }

    pub(crate) fn transcript_overlay_lines(&self, width: u16) -> Vec<Line<'static>> {
        let width = width.max(1);
        let mut lines = Vec::new();
        for cell in &self.history {
            Self::extend_lines_with_separator(&mut lines, cell.transcript_lines(width));
        }
        Self::extend_lines_with_separator(&mut lines, self.live_transcript_lines(width));
        Self::trim_trailing_blank_lines(&mut lines);
        lines
    }

    pub(crate) fn transcript_overlay_has_live_tail(&self) -> bool {
        self.active_cell.is_some()
            || !self.active_text_items.is_empty()
            || !self.active_tool_calls.is_empty()
            || !self.pending_tool_calls.is_empty()
    }

    pub(crate) fn external_editor_state(&self) -> ExternalEditorState {
        self.external_editor_state
    }

    pub(crate) fn set_external_editor_state(&mut self, state: ExternalEditorState) {
        self.external_editor_state = state;
    }

    pub(crate) fn queue_user_message(&mut self, user_message: UserMessage) {
        self.queued_user_messages.push_back(user_message);
        self.frame_requester.schedule_frame();
    }

    pub(crate) fn pop_next_queued_user_message(&mut self) -> Option<UserMessage> {
        self.queued_user_messages.pop_front()
    }

    pub(crate) fn set_status_message(&mut self, message: impl Into<String>) {
        self.status_message = message.into();
        self.sync_bottom_pane_summary();
        self.frame_requester.schedule_frame();
    }

    #[cfg(test)]
    pub(crate) fn last_plan_progress_for_test(&self) -> Option<(usize, usize)> {
        self.last_plan_progress
    }

    pub(crate) fn active_viewport_lines_for_test(&self, width: u16) -> Vec<Line<'static>> {
        self.active_viewport_lines(width)
    }

    fn active_viewport_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for item in &self.active_text_items {
            if let Some(cell) = &item.cell {
                Self::extend_lines_with_separator(&mut lines, cell.display_lines(width));
            }
        }
        // Pending tool calls are shown with a pending (cyan) dot until their results arrive.
        for pending in &self.pending_tool_calls {
            Self::extend_lines_with_separator(
                &mut lines,
                history_cell::AgentMessageCell::new_with_prefix(
                    pending.lines.clone(),
                    Self::pending_dot_prefix(),
                    "  ",
                    false,
                )
                .display_lines(width),
            );
        }
        Self::trim_trailing_blank_lines(&mut lines);
        lines
    }

    fn live_transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if let Some(cell) = &self.active_cell {
            Self::extend_lines_with_separator(&mut lines, cell.transcript_lines(width));
        }
        for item in &self.active_text_items {
            if let Some(cell) = &item.cell {
                Self::extend_lines_with_separator(&mut lines, cell.transcript_lines(width));
            }
        }
        let mut tool_calls = self.active_tool_calls.values().collect::<Vec<_>>();
        tool_calls.sort_by(|left, right| left.tool_use_id.cmp(&right.tool_use_id));
        for tool_call in tool_calls {
            Self::extend_lines_with_separator(
                &mut lines,
                history_cell::AgentMessageCell::new_with_prefix(
                    tool_call.lines.clone(),
                    Self::pending_dot_prefix(),
                    "  ",
                    false,
                )
                .transcript_lines(width),
            );
        }
        for pending in &self.pending_tool_calls {
            Self::extend_lines_with_separator(
                &mut lines,
                history_cell::AgentMessageCell::new_with_prefix(
                    pending.lines.clone(),
                    Self::pending_dot_prefix(),
                    "  ",
                    false,
                )
                .transcript_lines(width),
            );
        }
        Self::trim_trailing_blank_lines(&mut lines);
        lines
    }

    fn extend_lines_with_separator(target: &mut Vec<Line<'static>>, mut next: Vec<Line<'static>>) {
        if next.is_empty() {
            return;
        }

        let should_insert_separator = !target.is_empty()
            && target.last().is_some_and(|line| !Self::is_blank_line(line))
            && next.first().is_some_and(|line| !Self::is_blank_line(line));
        if should_insert_separator {
            target.push(Line::from(""));
        }
        target.append(&mut next);
    }

    pub(crate) fn drain_scrollback_lines(&mut self, width: u16) -> Vec<ScrollbackLine> {
        let width = width.max(1);
        let mut lines = Vec::new();
        for (index, cell) in self
            .history
            .iter()
            .skip(self.next_history_flush_index)
            .enumerate()
        {
            let cell_lines = cell.display_lines(width);
            let should_insert_separator = index > 0
                && !cell_lines.is_empty()
                && !lines.is_empty()
                && lines
                    .last()
                    .is_some_and(|line: &ScrollbackLine| !Self::is_blank_line(&line.line))
                && cell_lines
                    .first()
                    .is_some_and(|line| !Self::is_blank_line(line));
            if should_insert_separator {
                lines.push(ScrollbackLine::new(Line::from("")));
            }
            lines.extend(cell_lines.into_iter().map(ScrollbackLine::new));
        }
        self.next_history_flush_index = self.history.len();
        if !lines.is_empty() {
            lines.push(ScrollbackLine::new(Line::from("")));
        }
        lines
    }

    fn open_model_picker(&mut self) {
        self.picker_mode = Some(PickerMode::Model);
        self.pending_model_selection = None;
        let current_slug = self.session.model.as_ref().map(|model| model.slug.as_str());
        let entries = self
            .saved_model_slugs
            .iter()
            .filter_map(|slug| {
                self.available_models
                    .iter()
                    .find(|model| model.slug == *slug)
            })
            .map(|model| ModelPickerEntry {
                slug: model.slug.clone(),
                display_name: model.display_name.clone(),
                description: model.channel.clone(),
                is_current: current_slug == Some(model.slug.as_str()),
            })
            .collect();
        self.bottom_pane.open_model_picker(entries);
        self.set_status_message("Select a model");
    }

    fn handle_model_picker_selection(&mut self, slug: String) {
        let Some(selected_model) = self
            .available_models
            .iter()
            .find(|model| model.slug == slug)
            .cloned()
        else {
            self.apply_model_selection(slug);
            return;
        };

        let thinking_selection = selected_model.default_thinking_selection();
        self.pending_model_selection = Some(PendingModelSelection {
            slug: selected_model.slug.clone(),
            thinking_selection: thinking_selection.clone(),
        });
        self.session.provider = Some(selected_model.provider);
        self.session.model = Some(selected_model.clone());
        self.thinking_selection = thinking_selection;
        self.refresh_header_box();

        if selected_model
            .effective_thinking_capability()
            .options()
            .is_empty()
        {
            self.finalize_pending_model_selection();
            return;
        }

        self.open_thinking_picker();
    }

    fn open_theme_picker(&mut self) {
        self.bottom_pane
            .open_theme_picker(&self.theme_set.themes, self.active_theme_name.clone());
        self.set_status_message("Select a theme");
    }

    fn open_permissions_picker(&mut self) {
        let current = self.permission_preset;
        self.bottom_pane
            .open_popup_view(Box::new(ListSelectionView::new(
                SelectionViewParams {
                    title: Some("Update Model Permissions".to_string()),
                    footer_hint: Some(Line::from("Press enter to confirm or esc to go back")),
                    items: permission_preset_items(current),
                    ..SelectionViewParams::default()
                },
                self.app_event_tx.clone(),
                self.active_accent_color(),
            )));
        self.set_status_message("Select permissions");
    }

    pub(crate) fn note_permissions_updated(&mut self, preset: devo_protocol::PermissionPreset) {
        self.permission_preset = preset;
        let label = permission_preset_label(preset);
        self.add_to_history(history_cell::new_info_event(
            format!("Permissions updated to {label}"),
            None,
        ));
        self.set_status_message(format!("Permissions updated to {label}"));
    }

    fn apply_theme_selection(&mut self, name: String) {
        if let Some(theme) = self.theme_set.find(&name).cloned() {
            self.active_theme_name = name.clone();
            self.bottom_pane.set_accent_color(theme.accent_color);
            let _ = crate::onboarding::save_theme_selection(&name);
            self.set_status_message(format!("Theme set to {name}"));
            self.frame_requester.schedule_frame();
        }
    }

    fn active_accent_color(&self) -> Color {
        self.theme_set
            .find(&self.active_theme_name)
            .map(|t| t.accent_color)
            .unwrap_or(Color::Cyan)
    }

    fn active_error_color(&self) -> Color {
        self.theme_set
            .find(&self.active_theme_name)
            .map(|t| t.error_color)
            .unwrap_or(Color::Rgb(0xF8, 0x51, 0x49))
    }

    fn apply_model_selection(&mut self, slug: String) {
        if let Some(selected_model) = self
            .available_models
            .iter()
            .find(|model| model.slug == slug)
            .cloned()
        {
            self.thinking_selection = selected_model.default_thinking_selection();
            self.session.provider = Some(selected_model.provider);
            self.session.model = Some(selected_model.clone());
            self.app_event_tx
                .send(AppEvent::Command(AppCommand::override_turn_context(
                    /*cwd*/ None,
                    Some(selected_model.slug.clone()),
                    Some(self.thinking_selection.clone()),
                    /*sandbox*/ None,
                    /*approval_policy*/ None,
                )));
            self.set_status_message(format!("Model set to {}", selected_model.slug));
            return;
        }

        self.update_session_request_model(slug.clone());
        self.thinking_selection = self
            .session
            .model
            .as_ref()
            .and_then(Model::default_thinking_selection);
        self.app_event_tx
            .send(AppEvent::Command(AppCommand::override_turn_context(
                /*cwd*/ None,
                Some(slug.clone()),
                Some(self.thinking_selection.clone()),
                /*sandbox*/ None,
                /*approval_policy*/ None,
            )));
        self.set_status_message(format!("Model set to {slug}"));
    }

    fn open_thinking_picker(&mut self) {
        self.picker_mode = Some(PickerMode::Thinking);
        let entries = self.thinking_entries();
        if entries.is_empty() {
            self.set_status_message("Thinking Unsupported");
            return;
        }
        let model_entries = entries
            .into_iter()
            .map(|entry| ModelPickerEntry {
                slug: entry.value,
                display_name: entry.label,
                description: Some(entry.description),
                is_current: entry.is_current,
            })
            .collect();
        self.bottom_pane.open_model_picker(model_entries);
        self.set_status_message("Select a thinking mode");
    }

    fn apply_thinking_selection(&mut self, value: String) {
        self.thinking_selection = Some(value.clone());
        if let Some(pending) = self.pending_model_selection.as_mut() {
            pending.thinking_selection = Some(value);
            self.finalize_pending_model_selection();
            return;
        }

        self.refresh_header_box();
        self.app_event_tx
            .send(AppEvent::Command(AppCommand::override_turn_context(
                /*cwd*/ None,
                /*model*/ None,
                Some(Some(value.clone())),
                /*sandbox*/ None,
                /*approval_policy*/ None,
            )));
        self.set_status_message(format!("Thinking set to {value}"));
    }

    fn finalize_pending_model_selection(&mut self) {
        let Some(pending) = self.pending_model_selection.take() else {
            return;
        };

        self.picker_mode = None;
        self.thinking_selection = pending.thinking_selection.clone();
        self.refresh_header_box();
        self.app_event_tx
            .send(AppEvent::Command(AppCommand::override_turn_context(
                /*cwd*/ None,
                Some(pending.slug.clone()),
                Some(self.thinking_selection.clone()),
                /*sandbox*/ None,
                /*approval_policy*/ None,
            )));
        self.set_status_message(format!("Model set to {}", pending.slug));
    }

    fn open_resume_browser(&mut self, sessions: Vec<SessionListEntry>) {
        self.resume_browser_loading = false;
        let selection = sessions
            .iter()
            .position(|session| session.is_active)
            .unwrap_or(0);
        self.resume_browser = Some(ResumeBrowserState {
            sessions,
            selection,
        });
        self.set_status_message("Resume session");
    }

    fn handle_resume_browser_key_event(&mut self, key: KeyEvent) {
        if !matches!(key.kind, KeyEventKind::Press) {
            return;
        }
        let Some(browser) = self.resume_browser.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.resume_browser = None;
                self.resume_browser_loading = false;
                self.set_status_message("Ready");
            }
            KeyCode::Up => {
                if browser.sessions.is_empty() {
                    browser.selection = 0;
                } else {
                    browser.selection = (browser.selection as isize - 1)
                        .rem_euclid(browser.sessions.len() as isize)
                        as usize;
                }
                self.frame_requester.schedule_frame();
            }
            KeyCode::Down => {
                if browser.sessions.is_empty() {
                    browser.selection = 0;
                } else {
                    browser.selection = (browser.selection + 1) % browser.sessions.len();
                }
                self.frame_requester.schedule_frame();
            }
            KeyCode::Enter => {
                if let Some(selected) = browser.sessions.get(browser.selection) {
                    let session_id = selected.session_id;
                    self.resume_browser = None;
                    self.clear_for_session_switch();
                    self.app_event_tx
                        .send(AppEvent::Command(AppCommand::switch_session(session_id)));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn is_resume_browser_open(&self) -> bool {
        self.resume_browser_loading || self.resume_browser.is_some()
    }
}

impl Renderable for ChatWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.resume_browser_loading {
            let lines = vec![
                Line::from("Resume Session".bold()),
                Line::from("Loading saved sessions...".dim()),
                Line::from(""),
                Line::from("Please wait.".dim()),
            ];
            Paragraph::new(Text::from(lines))
                .block(Block::default().title("Devo Sessions"))
                .wrap(Wrap { trim: false })
                .render(area, buf);
            return;
        }

        if let Some(browser) = &self.resume_browser {
            Block::default().style(Style::default()).render(area, buf);
            let title_width = browser
                .sessions
                .iter()
                .map(|session| session.title.chars().count())
                .max()
                .unwrap_or(5)
                .clamp(5, 36);
            let mut lines = vec![
                Line::from("Resume Session".bold()),
                Line::from("Use Up/Down to select a session, Enter to resume.".dim()),
                Line::from("Esc to go back.".dim()),
                Line::from(""),
            ];
            if browser.sessions.is_empty() {
                lines.push(Line::from("No saved sessions found.".dim()));
            } else {
                lines.push(
                    Line::from(format!(
                        "  {:title_width$}  {:<16}  {}",
                        "Title",
                        "Session ID",
                        "Updated",
                        title_width = title_width
                    ))
                    .dim(),
                );
                lines.push(
                    Line::from(format!(
                        "  {}  {}  {}",
                        "-".repeat(title_width),
                        "-".repeat(16),
                        "-".repeat(19)
                    ))
                    .dim(),
                );
                for (index, session) in browser.sessions.iter().enumerate() {
                    let marker = if index == browser.selection { ">" } else { " " };
                    let current = if session.is_active { "  current" } else { "" };
                    let display_title = Self::truncate_display_text(&session.title, title_width);
                    let line = format!(
                        "{marker} {:title_width$}  {:<16}  {}{}",
                        display_title,
                        session.session_id,
                        session.updated_at,
                        current,
                        title_width = title_width
                    );
                    lines.push(if index == browser.selection {
                        Line::from(line).bold()
                    } else {
                        Line::from(line)
                    });
                }
            }
            Paragraph::new(Text::from(lines))
                .block(Block::default().title("Devo Sessions"))
                .wrap(Wrap { trim: false })
                .render(area, buf);
            return;
        }

        let bottom_height = self
            .bottom_pane
            .desired_height(area.width)
            .min(area.height.saturating_sub(1).max(3));
        let [history_area, bottom_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(bottom_height)]).areas(area);

        let viewport_lines = self.active_viewport_lines(history_area.width);
        if !viewport_lines.is_empty() {
            Paragraph::new(Text::from(viewport_lines)).render(history_area, buf);
        }

        self.bottom_pane.render(bottom_area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        if self.resume_browser.is_some() {
            return u16::MAX;
        }
        let history_height =
            u16::try_from(self.active_viewport_lines(width.max(1)).len()).unwrap_or(u16::MAX);
        history_height
            .saturating_add(self.bottom_pane.desired_height(width))
            .saturating_add(2)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if self.resume_browser.is_some() {
            return None;
        }
        let bottom_height = self
            .bottom_pane
            .desired_height(area.width)
            .min(area.height.saturating_sub(1).max(3));
        let [_, bottom_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(bottom_height)]).areas(area);
        self.bottom_pane.cursor_pos(bottom_area)
    }
}
