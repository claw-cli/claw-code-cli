use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use devo_protocol::user_input::TextElement;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

pub(crate) mod bottom_pane_view;
mod chat_composer;
mod chat_composer_history;
mod command_popup;
mod file_search_popup;
mod footer;
mod list_selection_view;
mod onboarding_view;
mod paste_burst;
mod pending_thread_approvals;
mod popup_consts;
mod prompt_args;
mod scroll_state;
mod selection_popup_common;
mod skill_popup;
pub(crate) mod slash_commands;
pub(crate) mod textarea;
mod unified_exec_footer;

pub(crate) use chat_composer::ChatComposer;
use chat_composer::ChatComposerConfig;
use chat_composer::InputResult as ComposerInputResult;
pub(crate) use onboarding_view::OnboardingResult;
pub(crate) use onboarding_view::OnboardingView;

use crate::app_command::AppCommand;
use crate::app_command::InputHistoryDirection;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::pending_thread_approvals::PendingThreadApprovals;
use crate::bottom_pane::unified_exec_footer::UnifiedExecFooter;
use crate::render::line_utils::prefix_lines;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
use crate::status_indicator_widget::StatusIndicatorWidget;
use crate::tui::frame_requester::FrameRequester;

pub(crate) const QUIT_SHORTCUT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CancellationEvent {
    Handled,
    NotHandled,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LocalImageAttachment {
    pub(crate) placeholder: String,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct MentionBinding {
    pub(crate) mention: String,
    pub(crate) path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SkillInterfaceMetadata {
    pub(crate) display_name: Option<String>,
    pub(crate) short_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillMetadata {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) short_description: Option<String>,
    pub(crate) interface: Option<SkillInterfaceMetadata>,
    pub(crate) path_to_skills_md: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginCapabilitySummary {
    pub(crate) config_name: String,
    pub(crate) display_name: String,
    pub(crate) description: Option<String>,
    pub(crate) has_skills: bool,
    pub(crate) mcp_server_names: Vec<String>,
    pub(crate) app_connector_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InputResult {
    Submitted {
        text: String,
        text_elements: Vec<TextElement>,
        local_images: Vec<LocalImageAttachment>,
        mention_bindings: Vec<MentionBinding>,
    },
    Command {
        command: SlashCommand,
        argument: String,
    },
    ModelSelected {
        model: String,
    },
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelPickerEntry {
    pub(crate) slug: String,
    pub(crate) display_name: String,
    pub(crate) description: Option<String>,
    pub(crate) is_current: bool,
}

pub(crate) struct BottomPaneParams {
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) frame_requester: FrameRequester,
    pub(crate) has_input_focus: bool,
    pub(crate) enhanced_keys_supported: bool,
    pub(crate) placeholder_text: String,
    pub(crate) disable_paste_burst: bool,
    pub(crate) skills: Option<Vec<SkillMetadata>>,
    pub(crate) animations_enabled: bool,
}

/// Owns the lifecycle of a single onboarding flow.
///
/// Separated from the generic `view_stack` so that onboarding does not block
/// task-interrupt routing (Esc) or pollute the `BottomPaneView` trait with
/// onboarding-specific methods. The result is extracted and preserved here
/// before the view is dropped so callers can retrieve it at any point.
pub(crate) struct OnboardingHandle {
    view: OnboardingView,
    /// Result extracted from the view when it completes. Held here so callers
    /// can retrieve it even after the handle has been reset.
    completed_result: Option<OnboardingResult>,
}

impl OnboardingHandle {
    pub(crate) fn new(
        models: &[devo_protocol::Model],
        app_event_tx: AppEventSender,
        frame_requester: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            view: OnboardingView::new(models, app_event_tx, frame_requester, animations_enabled),
            completed_result: None,
        }
    }

    pub(crate) fn handle_key_event(&mut self, key: KeyEvent) {
        self.view.handle_key_event(key);
        if self.view.is_complete() {
            // Extract result before it's overwritten
            self.completed_result = self.view.take_result();
        }
    }

    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer) {
        self.view.render(area, buf);
    }

    pub(crate) fn desired_height(&self, width: u16) -> u16 {
        self.view.desired_height(width)
    }

    pub(crate) fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.view.cursor_pos(area)
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.view.is_complete()
    }

    pub(crate) fn is_active(&self) -> bool {
        !self.view.is_complete()
    }

    pub(crate) fn take_result(&mut self) -> Option<OnboardingResult> {
        self.completed_result
            .take()
            .or_else(|| self.view.take_result())
    }

    pub(crate) fn on_validation_succeeded(&mut self, reply_preview: String) {
        self.view.on_validation_succeeded(reply_preview);
        if self.view.is_complete() {
            self.completed_result = self.view.take_result();
        }
    }

    pub(crate) fn on_validation_failed(&mut self, error_message: String) {
        self.view.on_validation_failed(error_message);
    }

    pub(crate) fn cancel(&mut self) {
        if !self.view.is_complete() {
            self.view.cancel();
            self.completed_result = self.view.take_result();
        }
    }
}

pub(crate) struct BottomPane {
    composer: ChatComposer,
    view_stack: Vec<Box<dyn BottomPaneView>>,
    onboarding: Option<OnboardingHandle>,
    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    unified_exec_footer: UnifiedExecFooter,
    pending_thread_approvals: PendingThreadApprovals,
    /// User messages queued while a turn was active, shown above the composer
    /// as pending cells. Each entry is the raw text of one queued prompt.
    pending_cell_texts: Vec<String>,
    placeholder_text: String,
    /// Status indicator shown above the composer while a task is running.
    status: Option<StatusIndicatorWidget>,
    is_task_running: bool,
    pending_interrupt_esc: bool,
    animations_enabled: bool,
    has_input_focus: bool,
    allow_empty_submit: bool,
    external_history_active: bool,
    external_history_draft: Option<String>,
}

impl BottomPane {
    pub(crate) fn new(params: BottomPaneParams) -> Self {
        let BottomPaneParams {
            app_event_tx,
            frame_requester,
            has_input_focus,
            enhanced_keys_supported,
            placeholder_text,
            disable_paste_burst,
            skills,
            animations_enabled,
        } = params;
        let mut composer = ChatComposer::new_with_config(
            has_input_focus,
            app_event_tx.clone(),
            enhanced_keys_supported,
            placeholder_text.clone(),
            disable_paste_burst,
            ChatComposerConfig {
                file_search_enabled: false,
                ..ChatComposerConfig::default()
            },
        );
        composer.set_frame_requester(frame_requester.clone());
        composer.set_skill_mentions(skills);
        Self {
            composer,
            view_stack: Vec::new(),
            onboarding: None,
            app_event_tx,
            frame_requester,
            unified_exec_footer: UnifiedExecFooter::new(),
            pending_thread_approvals: PendingThreadApprovals::new(),
            pending_cell_texts: Vec::new(),
            placeholder_text,
            status: None,
            is_task_running: false,
            pending_interrupt_esc: false,
            animations_enabled,
            has_input_focus,
            allow_empty_submit: false,
            external_history_active: false,
            external_history_draft: None,
        }
    }

    pub(crate) fn handle_key_event(&mut self, key: KeyEvent) -> InputResult {
        // Route to onboarding first — it takes priority over views and composer.
        if let Some(handle) = self.onboarding.as_mut() {
            handle.handle_key_event(key);
            self.request_redraw();
            return InputResult::None;
        }

        if !self.view_stack.is_empty() {
            return self.handle_view_key_event(key);
        }

        if key.code == KeyCode::Esc
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && self.is_task_running
            && !self.composer.popup_active()
        {
            if self.pending_interrupt_esc {
                self.pending_interrupt_esc = false;
                self.app_event_tx.send(AppEvent::Interrupt);
                self.restore_status_indicator();
            } else {
                self.pending_interrupt_esc = true;
                if let Some(status) = self.status.as_mut() {
                    status.set_interrupt_hint_visible(false);
                    status.update_inline_message(Some("Press ESC again to stop".to_string()));
                }
            }
            self.request_redraw();
            return InputResult::None;
        }

        if self.should_route_external_history(key) {
            return self.request_external_history(key);
        }

        if self.allow_empty_submit
            && key.code == KeyCode::Enter
            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
            && self.composer.is_empty()
        {
            self.reset_external_history_navigation();
            return InputResult::Submitted {
                text: String::new(),
                text_elements: Vec::new(),
                local_images: Vec::new(),
                mention_bindings: Vec::new(),
            };
        }

        let (input_result, needs_redraw) = self.composer.handle_key_event(key);
        if needs_redraw {
            self.request_redraw();
        }
        if self.composer.is_in_paste_burst() {
            self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
        }
        self.map_composer_input_result(input_result)
    }

    pub fn handle_paste(&mut self, pasted: String) {
        if !self.view_stack.is_empty() {
            let (needs_redraw, view_complete) = {
                let last_index = self.view_stack.len() - 1;
                let view = &mut self.view_stack[last_index];
                (view.handle_paste(pasted), view.is_complete())
            };
            if view_complete {
                self.view_stack.clear();
                self.on_active_view_complete();
            }
            if needs_redraw {
                self.request_redraw();
            }
        } else {
            let needs_redraw = self.composer.handle_paste(pasted);
            self.composer.sync_popups();
            if needs_redraw {
                self.request_redraw();
            }
        }
    }

    fn on_active_view_complete(&mut self) {
        self.set_composer_input_enabled(/*enabled*/ true, /*placeholder*/ None);
    }

    pub(crate) fn set_composer_input_enabled(
        &mut self,
        enabled: bool,
        placeholder: Option<String>,
    ) {
        self.composer.set_input_enabled(enabled, placeholder);
        self.request_redraw();
    }

    pub(crate) fn pre_draw_tick(&mut self) {
        self.composer.sync_popups();
        if self.composer.flush_paste_burst_if_due() {
            self.request_redraw();
        } else if self.composer.is_in_paste_burst() {
            self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
        }
    }

    pub(crate) fn set_placeholder_text(&mut self, placeholder: impl Into<String>) {
        let placeholder = placeholder.into();
        self.placeholder_text = placeholder.clone();
        self.composer.set_placeholder_text(placeholder);
        self.request_redraw();
    }

    pub(crate) fn clear_composer(&mut self) {
        self.composer
            .set_text_content(String::new(), Vec::new(), Vec::new());
        self.external_history_active = false;
        self.external_history_draft = None;
        self.request_redraw();
    }

    #[allow(dead_code)]
    pub(crate) fn composer_text(&self) -> String {
        self.composer.current_text()
    }

    #[cfg(test)]
    pub(crate) fn placeholder_text(&self) -> &str {
        &self.placeholder_text
    }

    pub(crate) fn set_allow_empty_submit(&mut self, enabled: bool) {
        self.allow_empty_submit = enabled;
    }

    pub(crate) fn start_onboarding(&mut self, models: &[devo_protocol::Model]) {
        self.onboarding = Some(OnboardingHandle::new(
            models,
            self.app_event_tx.clone(),
            self.frame_requester.clone(),
            self.animations_enabled,
        ));
        self.request_redraw();
    }

    pub(crate) fn poll_onboarding_result(&mut self) -> Option<OnboardingResult> {
        let result = self.onboarding.as_mut().and_then(|h| h.take_result());
        if result.is_some() {
            self.onboarding = None;
        }
        result
    }

    pub(crate) fn is_onboarding_active(&self) -> bool {
        self.onboarding.as_ref().is_some_and(|h| h.is_active())
    }

    pub(crate) fn onboarding_on_validation_succeeded(&mut self, reply_preview: String) {
        if let Some(handle) = &mut self.onboarding {
            handle.on_validation_succeeded(reply_preview);
            self.request_redraw();
        }
    }

    pub(crate) fn onboarding_on_validation_failed(&mut self, error_message: String) {
        if let Some(handle) = &mut self.onboarding {
            handle.on_validation_failed(error_message);
            self.request_redraw();
        }
    }

    pub(crate) fn open_model_picker(&mut self, entries: Vec<ModelPickerEntry>) {
        self.push_view(Box::new(ModelPickerView::new(entries)));
    }

    pub(crate) fn restore_input_from_history(&mut self, text: Option<String>) {
        match text {
            Some(text) => {
                self.composer.set_text_content(text, Vec::new(), Vec::new());
                self.external_history_active = true;
            }
            None => {
                let draft = self.external_history_draft.take().unwrap_or_default();
                self.composer
                    .set_text_content(draft, Vec::new(), Vec::new());
                self.external_history_active = false;
            }
        }
        self.request_redraw();
    }

    #[allow(dead_code)]
    pub(crate) fn set_status_line(&mut self, status_line: Option<Line<'static>>) {
        if self.composer.set_status_line(status_line) {
            self.request_redraw();
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_status_line_enabled(&mut self, enabled: bool) {
        if self.composer.set_status_line_enabled(enabled) {
            self.request_redraw();
        }
    }

    pub(crate) fn set_task_running(&mut self, running: bool) {
        let was_running = self.is_task_running;
        self.is_task_running = running;
        if running {
            self.pending_interrupt_esc = false;
            if !was_running {
                if self.status.is_none() {
                    self.status = Some(StatusIndicatorWidget::new(
                        self.app_event_tx.clone(),
                        self.frame_requester.clone(),
                        self.animations_enabled,
                    ));
                }
                if let Some(status) = self.status.as_mut() {
                    status.set_interrupt_hint_visible(true);
                }
                self.request_redraw();
            }
        } else {
            self.hide_status_indicator();
        }
    }

    pub(crate) fn hide_status_indicator(&mut self) {
        if self.status.take().is_some() {
            self.pending_interrupt_esc = false;
            self.request_redraw();
        }
    }

    fn restore_status_indicator(&mut self) {
        self.pending_interrupt_esc = false;
        if let Some(status) = self.status.as_mut() {
            status.set_interrupt_hint_visible(true);
            status.update_inline_message(None);
        }
    }

    pub(crate) fn push_pending_cell(&mut self, text: String) {
        self.pending_cell_texts.push(text);
        self.request_redraw();
    }

    /// Pop the oldest pending cell (FIFO). Returns its text, or None if empty.
    pub(crate) fn pop_oldest_pending_cell(&mut self) -> Option<String> {
        if self.pending_cell_texts.is_empty() {
            return None;
        }
        let result = Some(self.pending_cell_texts.remove(0));
        self.request_redraw();
        result
    }

    pub(crate) fn has_pending_cells(&self) -> bool {
        !self.pending_cell_texts.is_empty()
    }

    pub(crate) fn clear_pending_cells(&mut self) {
        if !self.pending_cell_texts.is_empty() {
            self.pending_cell_texts.clear();
            self.request_redraw();
        }
    }

    pub(crate) fn ensure_status_indicator(&mut self) {
        if self.status.is_none() {
            self.status = Some(StatusIndicatorWidget::new(
                self.app_event_tx.clone(),
                self.frame_requester.clone(),
                self.animations_enabled,
            ));
            self.request_redraw();
        }
    }

    pub(crate) fn status_widget(&self) -> Option<&StatusIndicatorWidget> {
        self.status.as_ref()
    }

    pub(crate) fn status_widget_mut(&mut self) -> Option<&mut StatusIndicatorWidget> {
        self.status.as_mut()
    }

    #[cfg(test)]
    pub(crate) fn status_indicator_visible(&self) -> bool {
        self.status.is_some()
    }

    fn active_view(&self) -> Option<&dyn BottomPaneView> {
        self.view_stack.last().map(std::convert::AsRef::as_ref)
    }

    fn push_view(&mut self, view: Box<dyn BottomPaneView>) {
        self.view_stack.push(view);
        self.request_redraw();
    }

    fn handle_view_key_event(&mut self, key: KeyEvent) -> InputResult {
        if matches!(key.kind, KeyEventKind::Release) {
            return InputResult::None;
        }

        let last_index = self.view_stack.len() - 1;
        let view = &mut self.view_stack[last_index];
        let prefer_esc = key.code == KeyCode::Esc && view.prefer_esc_to_handle_key_event();
        let completed_by_cancel = key.code == KeyCode::Esc
            && !prefer_esc
            && matches!(view.on_ctrl_c(), CancellationEvent::Handled)
            && view.is_complete();
        if !completed_by_cancel {
            view.handle_key_event(key);
        }

        let view_complete = self
            .view_stack
            .last()
            .is_some_and(|view| view.is_complete());
        let view_in_paste_burst = self
            .view_stack
            .last()
            .is_some_and(|view| view.is_in_paste_burst());

        if view_complete {
            let mut view = self.view_stack.pop().expect("active view exists");
            let selected_model = view.take_model_selection();
            self.request_redraw();
            return selected_model
                .map(|model| InputResult::ModelSelected { model })
                .unwrap_or(InputResult::None);
        }

        if view_in_paste_burst {
            self.request_redraw_in(ChatComposer::recommended_paste_flush_delay());
        }
        self.request_redraw();
        InputResult::None
    }

    fn map_composer_input_result(&mut self, input_result: ComposerInputResult) -> InputResult {
        match input_result {
            ComposerInputResult::Submitted {
                text,
                text_elements,
            }
            | ComposerInputResult::Queued {
                text,
                text_elements,
            } => {
                self.reset_external_history_navigation();
                InputResult::Submitted {
                    text,
                    text_elements,
                    local_images: self
                        .composer
                        .take_recent_submission_images_with_placeholders(),
                    mention_bindings: self.composer.take_recent_submission_mention_bindings(),
                }
            }
            ComposerInputResult::Command(command) => {
                self.reset_external_history_navigation();
                InputResult::Command {
                    command,
                    argument: String::new(),
                }
            }
            ComposerInputResult::CommandWithArgs(command, argument, _text_elements) => {
                self.reset_external_history_navigation();
                InputResult::Command { command, argument }
            }
            ComposerInputResult::None => InputResult::None,
        }
    }

    fn should_route_external_history(&self, key: KeyEvent) -> bool {
        if self.composer.popup_active() {
            return false;
        }
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return false;
        }
        matches!(key.code, KeyCode::Up | KeyCode::Down)
            && (self.composer.is_empty() || self.external_history_active)
    }

    fn request_external_history(&mut self, key: KeyEvent) -> InputResult {
        if !self.external_history_active {
            self.external_history_draft = Some(self.composer.current_text());
        }
        let direction = match key.code {
            KeyCode::Up => InputHistoryDirection::Previous,
            KeyCode::Down => InputHistoryDirection::Next,
            _ => return InputResult::None,
        };
        self.app_event_tx
            .send(AppEvent::Command(AppCommand::browse_input_history(
                direction,
            )));
        InputResult::None
    }

    fn reset_external_history_navigation(&mut self) {
        self.external_history_active = false;
        self.external_history_draft = None;
    }

    fn render_children(&self, area: Rect, buf: &mut Buffer, children: &[&dyn Renderable]) {
        let mut y = area.y;
        for child in children {
            let height = child.desired_height(area.width);
            if height == 0 {
                continue;
            }
            let child_area = Rect::new(area.x, y, area.width, height).intersection(area);
            if !child_area.is_empty() {
                child.render(child_area, buf);
            }
            y = y.saturating_add(height);
            if y >= area.bottom() {
                break;
            }
        }
    }

    fn desired_children_height(&self, width: u16, children: &[&dyn Renderable]) -> u16 {
        children.iter().fold(0u16, |height, child| {
            height.saturating_add(child.desired_height(width))
        })
    }

    fn child_cursor_pos(&self, area: Rect, children: &[&dyn Renderable]) -> Option<(u16, u16)> {
        let mut y = area.y;
        for child in children {
            let height = child.desired_height(area.width);
            if height == 0 {
                continue;
            }
            let child_area = Rect::new(area.x, y, area.width, height).intersection(area);
            if let Some(cursor) = child.cursor_pos(child_area) {
                return Some(cursor);
            }
            y = y.saturating_add(height);
        }
        None
    }

    fn request_redraw(&self) {
        self.frame_requester.schedule_frame();
    }

    fn request_redraw_in(&self, dur: Duration) {
        self.frame_requester.schedule_frame_in(dur);
    }
}

/// Thin renderable wrapper around a slice of pending cell texts.
/// Each cell is rendered with a `┃` prefix and a `QUEUED` badge, matching the
/// style of a normal user input cell in the history transcript.
struct PendingCellList<'a> {
    texts: &'a [String],
}

impl Renderable for PendingCellList<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.texts.is_empty() {
            return;
        }
        let mut lines: Vec<Line<'static>> = Vec::new();
        for text in self.texts {
            let wrapped = crate::wrapping::adaptive_wrap_lines(
                text.lines().map(|line| Line::from(line.to_string())),
                crate::wrapping::RtOptions::new(area.width as usize)
                    .subsequent_indent(Line::from("┃ ".cyan())),
            );
            lines.push(Line::from(""));
            if !wrapped.is_empty() {
                lines.extend(prefix_lines(wrapped, "┃ ".cyan(), "┃ ".cyan()));
            }
            lines.push(Line::from("  QUEUED".cyan().bold()));
        }
        Paragraph::new(lines).render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        if self.texts.is_empty() {
            return 0;
        }
        // Each cell: blank line + wrapped content + QUEUED badge
        let content_lines: usize = self
            .texts
            .iter()
            .map(|t| {
                let line_count = t.lines().count();
                // blank + content + QUEUED
                line_count + 2
            })
            .sum();
        content_lines as u16
    }
}

impl Renderable for BottomPane {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        if let Some(handle) = &self.onboarding {
            handle.render(area, buf);
            return;
        }
        if let Some(view) = self.active_view() {
            view.render(area, buf);
            return;
        }
        let mut children: Vec<&dyn Renderable> = Vec::with_capacity(5);
        // Status indicator above the composer while a task is running.
        if let Some(status) = &self.status {
            children.push(status);
        }
        // Avoid double-surfacing the unified-exec summary when the status row is active.
        if self.status.is_none() && !self.unified_exec_footer.is_empty() {
            children.push(&self.unified_exec_footer);
        }
        let pending_cells = PendingCellList {
            texts: &self.pending_cell_texts,
        };
        if pending_cells.desired_height(area.width) > 0 {
            children.push(&pending_cells);
        }
        children.push(&self.pending_thread_approvals);
        children.push(&self.composer);
        self.render_children(area, buf, &children);
    }

    fn desired_height(&self, width: u16) -> u16 {
        if let Some(handle) = &self.onboarding {
            return handle.desired_height(width);
        }
        if let Some(view) = self.active_view() {
            return view.desired_height(width);
        }
        let mut children: Vec<&dyn Renderable> = Vec::with_capacity(5);
        if let Some(status) = &self.status {
            children.push(status);
        }
        if self.status.is_none() && !self.unified_exec_footer.is_empty() {
            children.push(&self.unified_exec_footer);
        }
        let pending_cells = PendingCellList {
            texts: &self.pending_cell_texts,
        };
        if pending_cells.desired_height(width) > 0 {
            children.push(&pending_cells);
        }
        children.push(&self.pending_thread_approvals);
        children.push(&self.composer);
        self.desired_children_height(width, &children)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if let Some(handle) = &self.onboarding {
            return handle.cursor_pos(area);
        }
        if let Some(view) = self.active_view() {
            return view.cursor_pos(area);
        }
        let mut children: Vec<&dyn Renderable> = Vec::with_capacity(5);
        if let Some(status) = &self.status {
            children.push(status);
        }
        if self.status.is_none() && !self.unified_exec_footer.is_empty() {
            children.push(&self.unified_exec_footer);
        }
        let pending_cells = PendingCellList {
            texts: &self.pending_cell_texts,
        };
        if pending_cells.desired_height(area.width) > 0 {
            children.push(&pending_cells);
        }
        children.push(&self.pending_thread_approvals);
        children.push(&self.composer);
        self.child_cursor_pos(area, &children)
    }
}

struct ModelPickerView {
    entries: Vec<ModelPickerEntry>,
    selection: usize,
    complete: bool,
    selected_model: Option<String>,
}

impl ModelPickerView {
    fn new(entries: Vec<ModelPickerEntry>) -> Self {
        let selection = entries
            .iter()
            .position(|entry| entry.is_current)
            .unwrap_or(0);
        Self {
            entries,
            selection,
            complete: false,
            selected_model: None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.selection = 0;
        } else {
            self.selection =
                (self.selection as isize + delta).rem_euclid(self.entries.len() as isize) as usize;
        }
    }

    fn accept(&mut self) {
        self.selected_model = self
            .entries
            .get(self.selection)
            .map(|entry| entry.slug.clone());
        self.complete = true;
    }

    fn render_lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from("Select model").bold()];
        for (index, entry) in self.entries.iter().enumerate() {
            let mut title = if index == self.selection {
                Line::from(format!("  {}", entry.display_name)).bold()
            } else {
                Line::from(format!("  {}", entry.display_name)).dim()
            };
            if entry.is_current {
                title.spans.push("  ".into());
                title.spans.push("current".dark_gray());
            }
            lines.push(title);
            if let Some(description) = entry.description.as_deref()
                && !description.trim().is_empty()
            {
                lines.push(Line::from(format!("    {description}")).dim());
            }
        }
        lines
    }
}

impl BottomPaneView for ModelPickerView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Esc => self.complete = true,
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Enter => self.accept(),
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.complete = true;
        CancellationEvent::Handled
    }

    fn take_model_selection(&mut self) -> Option<String> {
        self.selected_model.take()
    }
}

impl Renderable for ModelPickerView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.render_lines()).render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::try_from(self.render_lines().len()).unwrap_or(u16::MAX)
    }
}
