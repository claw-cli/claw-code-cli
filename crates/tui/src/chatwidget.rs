//! Devo TUI chat surface.
//!
//! `ChatWidget` owns the v2 conversation surface: committed history cells, the
//! active bottom input pane, and the Claw-local app events produced by user
//! interaction. Protocol thinking choices come from `devo_protocol::thinking`
//! through `Model` instead of a TUI-local reasoning enum.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
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
use ratatui::text::Text;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPane;
use crate::bottom_pane::BottomPaneParams;
use crate::bottom_pane::InputResult;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::MentionBinding;
use crate::bottom_pane::ModelPickerEntry;
use crate::events::SessionListEntry;
use crate::events::TranscriptItem;
use crate::events::TranscriptItemKind;
use crate::events::WorkerEvent;
use crate::exec_cell::truncated_tool_output_preview;
use crate::get_git_diff::get_git_diff;
use crate::history_cell;
use crate::history_cell::AI_REPLY_WRAP_WIDTH;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::ScrollbackLine;
use crate::markdown::append_markdown;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
use crate::streaming::controller::StreamController;
use crate::tui::frame_requester::FrameRequester;
use devo_utils::ansi_escape::ansi_escape_line;

/// Common initialization parameters shared by `ChatWidget` constructors.
pub(crate) struct ChatWidgetInit {
    pub(crate) frame_requester: FrameRequester,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) initial_session: TuiSessionState,
    pub(crate) initial_thinking_selection: Option<String>,
    pub(crate) initial_user_message: Option<UserMessage>,
    pub(crate) enhanced_keys_supported: bool,
    pub(crate) is_first_run: bool,
    pub(crate) available_models: Vec<Model>,
    pub(crate) show_model_onboarding: bool,
    pub(crate) startup_tooltip_override: Option<String>,
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
}

impl TuiSessionState {
    pub(crate) fn new(cwd: PathBuf, model: Option<Model>) -> Self {
        let provider = model.as_ref().map(Model::provider_wire_api);
        Self {
            cwd,
            model,
            provider,
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
    lines: Vec<Line<'static>>,
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
    active_assistant_text: String,
    active_reasoning_text: String,
    stream_controller: Option<StreamController>,
    available_models: Vec<Model>,
    onboarding_step: Option<OnboardingStep>,
    resume_browser: Option<ResumeBrowserState>,
    picker_mode: Option<PickerMode>,
    turn_count: usize,
    total_input_tokens: usize,
    total_output_tokens: usize,
    busy: bool,
}

impl ChatWidget {
    fn build_header_box(
        cwd: &std::path::Path,
        model: Option<&Model>,
        is_first_run: bool,
        startup_tooltip_override: Option<String>,
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
            model.display_name.clone(),
            model.thinking_capability.clone(),
            model.default_reasoning_effort,
            model.thinking_implementation.clone(),
            is_first_run,
            startup_tooltip_override,
            /*show_fast_status*/ false,
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
        Line::from("• ".cyan())
    }

    fn pending_dot_prefix() -> Line<'static> {
        Line::from("• ".cyan())
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
        Style::default().fg(Color::Rgb(176, 176, 176))
    }

    fn tool_dot_prefix() -> Line<'static> {
        Line::from("• ".green())
    }

    fn failed_dot_prefix() -> Line<'static> {
        Line::from("• ").red()
    }

    fn dot_prefix(status: DotStatus) -> Line<'static> {
        match status {
            DotStatus::Pending => Self::pending_dot_prefix(),
            DotStatus::Completed => Self::completed_dot_prefix(),
            DotStatus::Failed => Self::failed_dot_prefix(),
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

    fn context_budget(&self) -> Option<(usize, usize, usize)> {
        let model = self.session.model.as_ref()?;
        let total = model.context_window as usize;
        let usable = total.saturating_mul(model.effective_context_window_percent() as usize) / 100;
        let used = self.total_input_tokens.min(usable);
        Some((used, usable, total))
    }

    fn session_summary_text(&self) -> String {
        let model = self
            .session
            .model
            .as_ref()
            .map(|model| model.slug.as_str())
            .unwrap_or("unknown");
        let thinking = self.thinking_selection.as_deref().unwrap_or("default");
        let context = self.context_budget().map_or_else(
            || "context n/a".to_string(),
            |(used, usable, total)| {
                format!(
                    "context {} / {} usable ({} total)",
                    Self::format_token_count(used),
                    Self::format_token_count(usable),
                    Self::format_token_count(total)
                )
            },
        );

        format!(
            "{model} | thinking {thinking} | tokens {} in / {} out | {context}",
            self.total_input_tokens, self.total_output_tokens
        )
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
        self.history.push(Self::build_header_box(
            &self.session.cwd,
            self.session.model.as_ref(),
            is_first_run,
            startup_tooltip_override,
        ));
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
        self.push_session_header(
            /*is_first_run*/ false, /*startup_tooltip_override*/ None,
        );

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
        for item in history_items {
            self.add_transcript_item_without_redraw(item);
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

    fn clear_for_session_switch(&mut self) {
        self.history.clear();
        self.next_history_flush_index = 0;
        self.active_cell = None;
        self.active_cell_revision = 0;
        self.active_tool_calls.clear();
        self.pending_tool_calls.clear();
        self.active_assistant_text.clear();
        self.active_reasoning_text.clear();
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
            initial_user_message,
            enhanced_keys_supported,
            is_first_run,
            available_models,
            show_model_onboarding,
            startup_tooltip_override,
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

        // Build the bottom composer first, since the widget delegates all live input handling there.
        let bottom_pane = BottomPane::new(BottomPaneParams {
            app_event_tx: app_event_tx.clone(),
            frame_requester: frame_requester.clone(),
            has_input_focus: true,
            enhanced_keys_supported,
            placeholder_text: "Ask Devo".to_string(),
            disable_paste_burst: false,
            skills: None,
            animations_enabled: true,
        });

        let history: Vec<Box<dyn HistoryCell>> = vec![Self::build_header_box(
            &initial_session.cwd,
            initial_session.model.as_ref(),
            is_first_run,
            startup_tooltip_override,
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
            active_assistant_text: String::new(),
            active_reasoning_text: String::new(),
            stream_controller: None,
            available_models,
            onboarding_step: None,
            resume_browser: None,
            picker_mode: None,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            busy: false,
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
        match self.bottom_pane.handle_key_event(key) {
            InputResult::Submitted {
                text,
                text_elements,
                local_images,
                mention_bindings,
            } => {
                let user_message = UserMessage {
                    text,
                    local_images,
                    remote_image_urls: Vec::new(),
                    text_elements,
                    mention_bindings,
                };
                self.submit_user_message(user_message);
            }
            InputResult::Command { command, argument } => {
                self.handle_slash_command(command, argument);
            }
            InputResult::ModelSelected { model } => match self.picker_mode.take() {
                Some(PickerMode::Thinking) => self.apply_thinking_selection(model),
                _ => self.apply_model_selection(model),
            },
            InputResult::None => {}
        }
    }

    pub(crate) fn handle_paste(&mut self, text: String) {
        if self.resume_browser.is_some() {
            return;
        }
        self.bottom_pane.handle_paste(text);
    }

    pub(crate) fn pre_draw_tick(&mut self) {
        self.bottom_pane.pre_draw_tick();
    }

    pub(crate) fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Redraw => self.frame_requester.schedule_frame(),
            AppEvent::SubmitUserInput { text } => self.submit_text(text),
            AppEvent::ModelSelected { model } => {
                self.apply_model_selection(model);
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
                self.set_status_message(format!("Command queued: {}", command.kind()));
            }
            AppEvent::Exit(_)
            | AppEvent::OpenSlashCommandPopup
            | AppEvent::ClosePopup
            | AppEvent::RunSlashCommand { .. }
            | AppEvent::OpenModelPicker
            | AppEvent::OpenThinkingPicker
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
            WorkerEvent::TurnStarted { model, thinking } => {
                self.update_session_request_model(model);
                self.thinking_selection = thinking;
                self.busy = true;
                self.active_assistant_text.clear();
                self.active_reasoning_text.clear();
                self.stream_controller = None;
                self.bottom_pane.set_task_running(true);
            }
            WorkerEvent::TextDelta(text) => {
                self.push_assistant_stream_delta(&text);
                self.set_status_message("Generating");
            }
            WorkerEvent::ReasoningDelta(text) => {
                self.active_reasoning_text.push_str(&text);
                self.set_status_message("Thinking");
            }
            WorkerEvent::AssistantMessageCompleted(text) => {
                if self.stream_controller.is_none() {
                    self.active_assistant_text = text;
                }
                self.set_status_message("Generating");
            }
            WorkerEvent::ReasoningCompleted(text) => {
                self.active_reasoning_text = text;
                self.set_status_message("Thinking");
            }
            WorkerEvent::ToolCall {
                tool_use_id,
                summary,
                detail,
            } => {
                // Do not commit active streams here — pending tool calls share the
                // active viewport alongside reasoning/assistant text.
                let message = detail
                    .map(|detail| format!("{summary}\n{detail}"))
                    .unwrap_or(summary);
                let tool_call = ActiveToolCall {
                    tool_use_id: tool_use_id.clone(),
                    lines: vec![Line::from(message).patch_style(Self::tool_text_style())],
                };
                self.active_tool_calls
                    .insert(tool_use_id.clone(), tool_call.clone());
                self.pending_tool_calls.push(tool_call);
                self.frame_requester.schedule_frame();
                self.set_status_message("Tool started");
            }
            WorkerEvent::ToolResult {
                tool_use_id,
                title,
                preview,
                is_error,
                truncated,
            } => {
                self.commit_active_streams(DotStatus::Completed);
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
                let active_tool_lines = self
                    .active_tool_calls
                    .remove(&tool_use_id)
                    .map(|tool| tool.lines);
                if let Some(lines) = active_tool_lines {
                    self.add_to_history(history_cell::AgentMessageCell::new_with_prefix(
                        lines,
                        Self::dot_prefix(dot_status),
                        "  ",
                        false,
                    ));
                } else if !title.is_empty() {
                    self.add_to_history(history_cell::AgentMessageCell::new_with_prefix(
                        vec![Line::from(title.clone()).patch_style(Self::tool_text_style())],
                        Self::dot_prefix(dot_status),
                        "  ",
                        false,
                    ));
                }

                let mut lines = Vec::new();
                let mut preview_lines = self.tool_preview_lines(&preview);
                if truncated && preview_lines.is_empty() {
                    preview_lines.push(Line::from("…").patch_style(Self::tool_text_style()));
                }
                if !title.is_empty() {
                    lines.push(Line::from(title).patch_style(Self::tool_text_style()));
                }
                lines.extend(preview_lines);
                self.add_to_history(history_cell::AgentMessageCell::new_with_prefix(
                    lines,
                    Self::dot_prefix(dot_status),
                    "  ",
                    false,
                ));
                self.set_status_message(if is_error {
                    "Tool returned an error"
                } else {
                    "Tool completed"
                });
            }
            WorkerEvent::UsageUpdated {
                total_input_tokens,
                total_output_tokens,
            } => {
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.frame_requester.schedule_frame();
            }
            WorkerEvent::TurnFinished {
                stop_reason: _,
                turn_count,
                total_input_tokens,
                total_output_tokens,
            } => {
                self.commit_active_streams(DotStatus::Completed);
                self.active_tool_calls.clear();
                self.pending_tool_calls.clear();
                self.busy = false;
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.bottom_pane.set_task_running(false);
                self.set_status_message("Ready");
            }
            WorkerEvent::TurnFailed {
                message,
                turn_count,
                total_input_tokens,
                total_output_tokens,
            } => {
                self.commit_active_streams(DotStatus::Failed);
                self.active_tool_calls.clear();
                self.pending_tool_calls.clear();
                self.busy = false;
                self.turn_count = turn_count;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.add_to_history(history_cell::new_error_event(message));
                self.bottom_pane.set_task_running(false);
                self.set_status_message("Query failed; see error above");
            }
            WorkerEvent::ProviderValidationSucceeded { reply_preview } => {
                if let Some(OnboardingStep::Validating { model, .. }) = self.onboarding_step.take()
                {
                    self.update_session_request_model(model);
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
                if let Some(OnboardingStep::Validating {
                    model, base_url, ..
                }) = self.onboarding_step.take()
                {
                    self.onboarding_step = Some(OnboardingStep::ApiKey { model, base_url });
                    self.set_onboarding_placeholder("API key");
                }
                self.busy = false;
                self.add_to_history(history_cell::new_error_event_with_hint(
                    message,
                    Some("provider validation failed".to_string()),
                ));
                self.set_status_message("Provider validation failed");
            }
            WorkerEvent::SessionsListed { sessions } => {
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
            } => {
                self.session.cwd = cwd;
                self.update_session_request_model(model);
                self.thinking_selection = thinking;
                self.active_assistant_text.clear();
                self.active_reasoning_text.clear();
                self.stream_controller = None;
                self.history.clear();
                self.next_history_flush_index = 0;
                self.busy = false;
                self.turn_count = 0;
                self.total_input_tokens = 0;
                self.total_output_tokens = 0;
                self.push_session_header(
                    /*is_first_run*/ false, /*startup_tooltip_override*/ None,
                );
                self.set_status_message("New session ready; send a prompt to start it");
            }
            WorkerEvent::SessionSwitched {
                session_id,
                cwd,
                title,
                model,
                thinking,
                total_input_tokens,
                total_output_tokens,
                history_items,
                loaded_item_count,
            } => {
                self.session.cwd = cwd;
                if let Some(model) = model {
                    self.update_session_request_model(model);
                }
                self.thinking_selection = thinking;
                self.history.clear();
                self.next_history_flush_index = 0;
                self.active_assistant_text.clear();
                self.active_reasoning_text.clear();
                self.stream_controller = None;
                self.total_input_tokens = total_input_tokens;
                self.total_output_tokens = total_output_tokens;
                self.rebuild_restored_session_history(
                    history_items,
                    loaded_item_count,
                    &session_id,
                    title.as_deref(),
                );
                self.set_status_message("Session switched");
            }
            WorkerEvent::SessionRenamed { session_id, title } => {
                self.add_to_history(history_cell::new_info_event(
                    format!("renamed {session_id} to {title}"),
                    None,
                ));
                self.set_status_message("Session renamed");
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
        }
    }

    pub(crate) fn submit_text(&mut self, text: String) {
        self.submit_user_message(UserMessage::from(text));
    }

    fn submit_user_message(&mut self, user_message: UserMessage) {
        if self.onboarding_step.is_some()
            && self.handle_onboarding_input(user_message.text.trim().to_string())
        {
            return;
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
                /*approval_policy*/ None,
            )));
        self.set_status_message("Submitted locally");
    }

    fn handle_slash_command(&mut self, command: SlashCommand, argument: String) {
        match command {
            SlashCommand::Exit => {
                self.app_event_tx
                    .send(AppEvent::Exit(crate::app_event::ExitMode::ShutdownFirst));
            }
            SlashCommand::Clear => {
                self.history.clear();
                self.next_history_flush_index = 0;
                self.active_assistant_text.clear();
                self.active_reasoning_text.clear();
                self.set_status_message("Transcript cleared");
            }
            SlashCommand::Onboard => {
                self.begin_onboarding();
            }
            SlashCommand::Status => {
                let context_line = self.context_budget().map_or_else(
                    || "  context:  n/a".to_string(),
                    |(used, usable, total)| {
                        format!(
                            "  context:  {} / {} usable ({} total)",
                            Self::format_token_count(used),
                            Self::format_token_count(usable),
                            Self::format_token_count(total)
                        )
                    },
                );
                self.add_to_history(PlainHistoryCell::new(vec![
                    Line::from("Status".bold()),
                    Line::from(""),
                    Line::from(format!(
                        "  model:    {}\n  thinking: {}\n  tokens:   {} in / {} out\n{}\n  busy:     {}",
                        self.session
                            .model
                            .as_ref()
                            .map(|model| model.slug.as_str())
                            .unwrap_or("unknown"),
                        self.thinking_selection.as_deref().unwrap_or("default"),
                        self.total_input_tokens,
                        self.total_output_tokens,
                        context_line,
                        self.busy
                    )),
                ]));
                self.set_status_message("Session status shown");
            }
            SlashCommand::Model => {
                if argument.is_empty() {
                    self.open_model_picker();
                } else {
                    self.apply_model_selection(argument);
                }
            }
            SlashCommand::Thinking => {
                self.open_thinking_picker();
            }
            SlashCommand::New => {
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                        command: "session new".to_string(),
                    }));
                self.set_status_message("New session requested");
            }
            SlashCommand::Resume => {
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                        command: "session list".to_string(),
                    }));
                self.set_status_message("Loading sessions");
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

    // TODO: Now, the onboarding TUI is too simple and crude, should be a more designed, specifially designed for onboarding.
    fn begin_onboarding(&mut self) {
        self.onboarding_step = Some(OnboardingStep::ModelName);
        self.set_onboarding_placeholder("model name");
        let mut lines = vec![
            Line::from("Onboarding".bold()),
            Line::from("Choose a model, then enter optional base URL and API key.".dim()),
        ];
        for model in self.available_models.iter().take(12) {
            let description = model.description.as_deref().unwrap_or_default();
            let suffix = if description.is_empty() {
                String::new()
            } else {
                format!(" - {description}")
            };
            lines.push(Line::from(format!("  {}{}", model.slug, suffix)));
        }
        lines.push(Line::from("Type a model slug or custom model name.").dim());
        self.add_to_history(PlainHistoryCell::new(lines));
        self.bottom_pane.set_allow_empty_submit(false);
        self.set_status_message("Onboarding: enter model name");
    }

    fn handle_onboarding_input(&mut self, text: String) -> bool {
        let Some(step) = self.onboarding_step.take() else {
            return false;
        };

        match step {
            OnboardingStep::ModelName => {
                if text.is_empty() {
                    self.onboarding_step = Some(OnboardingStep::ModelName);
                    self.set_onboarding_placeholder("model name");
                    self.set_status_message("Onboarding: enter model name");
                    return true;
                }
                self.onboarding_step = Some(OnboardingStep::BaseUrl {
                    model: text.clone(),
                });
                self.set_onboarding_placeholder("base URL");
                self.bottom_pane.set_allow_empty_submit(true);
                self.add_to_history(history_cell::new_info_event(
                    format!("model: {text}"),
                    Some("onboarding".to_string()),
                ));
                self.set_status_message(
                    "Onboarding: enter base URL, or press Enter to use default",
                );
                true
            }
            OnboardingStep::BaseUrl { model } => {
                let base_url = if text.is_empty() {
                    None
                } else if text.starts_with("http://") || text.starts_with("https://") {
                    Some(text)
                } else {
                    self.onboarding_step = Some(OnboardingStep::BaseUrl { model });
                    self.set_onboarding_placeholder("base URL");
                    self.bottom_pane.set_allow_empty_submit(true);
                    self.add_to_history(history_cell::new_error_event(
                        "Base URL must start with http:// or https://".to_string(),
                    ));
                    self.set_status_message("Onboarding: enter base URL");
                    return true;
                };
                self.onboarding_step = Some(OnboardingStep::ApiKey {
                    model,
                    base_url: base_url.clone(),
                });
                self.set_onboarding_placeholder("API key");
                self.bottom_pane.set_allow_empty_submit(true);
                self.add_to_history(history_cell::new_info_event(
                    format!("base url: {}", base_url.as_deref().unwrap_or("(default)")),
                    Some("onboarding".to_string()),
                ));
                self.set_status_message("Onboarding: enter API key, or press Enter to skip");
                true
            }
            OnboardingStep::ApiKey { model, base_url } => {
                let api_key = if text.is_empty() { None } else { Some(text) };
                self.onboarding_step = Some(OnboardingStep::Validating {
                    model: model.clone(),
                    base_url: base_url.clone(),
                    api_key: api_key.clone(),
                });
                self.bottom_pane
                    .set_placeholder_text("Onboarding: validating connection".to_string());
                self.bottom_pane.set_allow_empty_submit(false);
                let payload = serde_json::json!({
                    "model": model,
                    "base_url": base_url,
                    "api_key": api_key,
                });
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                        command: format!("onboard {payload}"),
                    }));
                self.set_status_message("Onboarding: validating provider connection");
                true
            }
            OnboardingStep::Validating {
                model,
                base_url,
                api_key,
            } => {
                self.onboarding_step = Some(OnboardingStep::Validating {
                    model,
                    base_url,
                    api_key,
                });
                self.set_status_message("Onboarding validation is already running");
                true
            }
        }
    }

    pub(crate) fn set_model(&mut self, model: Model) {
        self.thinking_selection = model.default_thinking_selection();
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
            self.session.provider = Some(model.provider_wire_api());
            self.session.model = Some(model);
            return;
        }

        if let Some(model) = self.session.model.as_mut() {
            model.slug = slug.clone();
            model.display_name = slug;
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
        let mut lines = if title == "Assistant" || title == "Reasoning" {
            Vec::new()
        } else {
            vec![Line::from(title.to_string()).bold()]
        };
        let markdown_width = usize::from(self.last_known_width().max(1)).min(AI_REPLY_WRAP_WIDTH);
        append_markdown(
            body,
            Some(markdown_width),
            Some(&self.session.cwd),
            &mut lines,
        );
        if title == "Assistant" || title == "Reasoning" {
            self.add_history_entry_without_redraw(Box::new(
                history_cell::AgentMessageCell::new_ai_response_with_prefix(
                    lines,
                    Self::dot_prefix(status),
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
        let mut lines = Vec::new();
        let markdown_width = usize::from(self.last_known_width().max(1)).min(AI_REPLY_WRAP_WIDTH);
        append_markdown(
            body,
            Some(markdown_width),
            Some(&self.session.cwd),
            &mut lines,
        );
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
                        vec![Line::from(item.title).patch_style(Self::tool_text_style())],
                        Self::tool_dot_prefix(),
                        "  ",
                        false,
                    ),
                ));
            }
            TranscriptItemKind::ToolResult => {
                let mut lines = vec![Line::from(item.title).patch_style(Self::tool_text_style())];
                lines.extend(self.tool_preview_lines(&item.body));
                self.add_history_entry_without_redraw(Box::new(
                    history_cell::AgentMessageCell::new_with_prefix(
                        lines,
                        Self::tool_dot_prefix(),
                        "  ",
                        false,
                    ),
                ));
            }
            TranscriptItemKind::Error => self.add_history_entry_without_redraw(Box::new(
                history_cell::new_error_event_with_hint(item.body, Some(item.title)),
            )),
            TranscriptItemKind::System => {
                self.add_history_entry_without_redraw(Box::new(history_cell::new_info_event(
                    item.title,
                    Some(item.body),
                )));
            }
        }
    }

    fn commit_active_streams(&mut self, status: DotStatus) {
        if !self.active_reasoning_text.trim().is_empty() {
            let reasoning_text = std::mem::take(&mut self.active_reasoning_text);
            self.add_markdown_history_with_status("Reasoning", &reasoning_text, status);
        }
        self.finalize_assistant_stream();
        if !self.active_assistant_text.trim().is_empty() {
            let text = std::mem::take(&mut self.active_assistant_text);
            self.add_markdown_history_with_status("Assistant", &text, status);
        }
    }

    fn push_assistant_stream_delta(&mut self, text: &str) {
        if self.stream_controller.is_none() {
            let markdown_width = usize::from(self.last_known_width().saturating_sub(2).max(1))
                .min(AI_REPLY_WRAP_WIDTH);
            self.stream_controller = Some(StreamController::new(
                Some(markdown_width),
                &self.session.cwd,
            ));
        }

        if let Some(controller) = self.stream_controller.as_mut() {
            controller.push(text);
        }

        self.flush_assistant_stream_commits();
        self.frame_requester.schedule_frame();
    }

    fn flush_assistant_stream_commits(&mut self) {
        let Some(controller) = self.stream_controller.as_mut() else {
            return;
        };

        loop {
            let (cell, idle) = controller.on_commit_tick();
            let emitted = cell.is_some();
            if let Some(cell) = cell {
                self.history.push(cell);
            }
            if idle || !emitted {
                break;
            }
        }
    }

    fn finalize_assistant_stream(&mut self) {
        let Some(mut controller) = self.stream_controller.take() else {
            return;
        };

        if let Some(cell) = controller.finalize() {
            self.history.push(cell);
        }
    }

    fn last_known_width(&self) -> u16 {
        crossterm::terminal::size()
            .map(|(width, _height)| width)
            .unwrap_or(80)
    }

    fn tool_preview_lines(&self, preview: &str) -> Vec<Line<'static>> {
        let width = self.last_known_width().saturating_sub(2).max(1);
        let mut preview_lines = truncated_tool_output_preview(preview, width, 2);
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
        self.frame_requester.schedule_frame();
    }

    pub(crate) fn current_model(&self) -> Option<&Model> {
        self.session.model.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn current_cwd(&self) -> &std::path::Path {
        &self.session.cwd
    }

    #[cfg(test)]
    pub(crate) fn placeholder_text(&self) -> &str {
        self.bottom_pane.placeholder_text()
    }

    pub(crate) fn current_thinking_selection(&self) -> Option<&str> {
        self.thinking_selection.as_deref()
    }

    pub(crate) fn current_reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.session
            .model
            .as_ref()
            .map(|model| model.resolve_thinking_selection(self.thinking_selection.as_deref()))
            .and_then(|resolved| resolved.effective_reasoning_effort)
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

    fn active_viewport_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if let Some(active_cell) = &self.active_cell {
            lines.extend(active_cell.display_lines(width));
        }
        if !self.active_reasoning_text.trim().is_empty() {
            lines.extend(
                self.bulleted_markdown_cell(
                    &self.active_reasoning_text,
                    Self::pending_dot_prefix(),
                )
                .display_lines(width),
            );
        }
        // Pending tool calls are shown with a pending (cyan) dot until their results arrive.
        for pending in &self.pending_tool_calls {
            lines.extend(
                history_cell::AgentMessageCell::new_with_prefix(
                    pending.lines.clone(),
                    Self::pending_dot_prefix(),
                    "  ",
                    false,
                )
                .display_lines(width),
            );
        }
        if !self.active_assistant_text.trim().is_empty() {
            lines.extend(
                self.bulleted_markdown_cell(
                    &self.active_assistant_text,
                    Self::pending_dot_prefix(),
                )
                .display_lines(width),
            );
        }
        if let Some(controller) = &self.stream_controller {
            let pending_lines = controller.pending_lines();
            if !pending_lines.is_empty() {
                lines.extend(
                    history_cell::AgentMessageCell::new_ai_response_with_prefix(
                        pending_lines,
                        Self::pending_dot_prefix(),
                        "  ",
                        false,
                    )
                    .display_lines(width),
                );
            }
        }
        Self::trim_trailing_blank_lines(&mut lines);
        lines
    }

    pub(crate) fn drain_scrollback_lines(&mut self, width: u16) -> Vec<ScrollbackLine> {
        let width = width.max(1);
        let mut lines = Vec::new();
        for cell in self.history.iter().skip(self.next_history_flush_index) {
            let wrap_policy = cell.scrollback_wrap_policy();
            lines.extend(
                cell.display_lines(width)
                    .into_iter()
                    .map(|line| ScrollbackLine::new(line, wrap_policy)),
            );
        }
        self.next_history_flush_index = self.history.len();
        lines
    }

    fn open_model_picker(&mut self) {
        self.picker_mode = Some(PickerMode::Model);
        let current_slug = self.session.model.as_ref().map(|model| model.slug.as_str());
        let entries = self
            .available_models
            .iter()
            .map(|model| ModelPickerEntry {
                slug: model.slug.clone(),
                display_name: model.display_name.clone(),
                description: model.description.clone(),
                is_current: current_slug == Some(model.slug.as_str()),
            })
            .collect();
        self.bottom_pane.open_model_picker(entries);
        self.set_status_message("Select a model");
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

    fn open_resume_browser(&mut self, sessions: Vec<SessionListEntry>) {
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
        self.resume_browser.is_some()
    }
}

impl Renderable for ChatWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
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
            Paragraph::new(Text::from(viewport_lines))
                .wrap(Wrap { trim: false })
                .render(history_area, buf);
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
