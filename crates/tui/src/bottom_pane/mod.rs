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
mod paste_burst;
mod pending_input_preview;
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

use crate::app_command::AppCommand;
use crate::app_command::InputHistoryDirection;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::bottom_pane_view::BottomPaneView;
use crate::bottom_pane::pending_input_preview::PendingInputPreview;
use crate::bottom_pane::pending_thread_approvals::PendingThreadApprovals;
use crate::bottom_pane::unified_exec_footer::UnifiedExecFooter;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
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
}

pub(crate) struct BottomPane {
    composer: ChatComposer,
    view_stack: Vec<Box<dyn BottomPaneView>>,
    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    unified_exec_footer: UnifiedExecFooter,
    pending_input_preview: PendingInputPreview,
    pending_thread_approvals: PendingThreadApprovals,
    placeholder_text: String,
    session_summary: Option<String>,
    status_message: Option<String>,
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
            app_event_tx,
            frame_requester,
            unified_exec_footer: UnifiedExecFooter::new(),
            pending_input_preview: PendingInputPreview::new(),
            pending_thread_approvals: PendingThreadApprovals::new(),
            placeholder_text,
            session_summary: None,
            status_message: None,
            allow_empty_submit: false,
            external_history_active: false,
            external_history_draft: None,
        }
    }

    pub(crate) fn handle_key_event(&mut self, key: KeyEvent) -> InputResult {
        if !self.view_stack.is_empty() {
            return self.handle_view_key_event(key);
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

    pub(crate) fn set_status_message(&mut self, message: impl Into<String>) {
        self.status_message = Some(message.into());
        self.sync_status_line();
    }

    pub(crate) fn set_session_summary(&mut self, summary: impl Into<String>) {
        self.session_summary = Some(summary.into());
        self.sync_status_line();
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

    fn sync_status_line(&mut self) {
        let status_line = match (&self.session_summary, &self.status_message) {
            (Some(summary), Some(status)) => {
                Some(Line::from(format!("{status}  |  {summary}")).dim())
            }
            (Some(summary), None) => Some(Line::from(summary.clone()).dim()),
            (None, Some(status)) => Some(Line::from(status.clone()).dim()),
            (None, None) => None,
        };
        let changed = self.composer.set_status_line(status_line);
        let enabled_changed = self.composer.set_status_line_enabled(
            self.session_summary.is_some() || self.status_message.is_some(),
        );
        if changed || enabled_changed {
            self.request_redraw();
        }
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

impl Renderable for BottomPane {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        if let Some(view) = self.active_view() {
            view.render(area, buf);
            return;
        }
        let children: [&dyn Renderable; 4] = [
            &self.unified_exec_footer,
            &self.pending_thread_approvals,
            &self.pending_input_preview,
            &self.composer,
        ];
        self.render_children(area, buf, &children);
    }

    fn desired_height(&self, width: u16) -> u16 {
        if let Some(view) = self.active_view() {
            return view.desired_height(width);
        }
        let children: [&dyn Renderable; 4] = [
            &self.unified_exec_footer,
            &self.pending_thread_approvals,
            &self.pending_input_preview,
            &self.composer,
        ];
        self.desired_children_height(width, &children)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        if let Some(view) = self.active_view() {
            return view.cursor_pos(area);
        }
        let children: [&dyn Renderable; 4] = [
            &self.unified_exec_footer,
            &self.pending_thread_approvals,
            &self.pending_input_preview,
            &self.composer,
        ];
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
