use std::time::Instant;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

use devo_protocol::Model;
use devo_protocol::ProviderWireApi;

use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
/// Simple content area with padding, no background styling.
fn onboarding_content_area(area: Rect) -> Rect {
    if area.height < 2 || area.width < 2 {
        return area;
    }
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}
use crate::app_command::AppCommand;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::exec_cell::spinner;
use crate::render::renderable::Renderable;
use crate::tui::frame_requester::FrameRequester;

const CUSTOM_MODEL_SENTINEL: &str = "__custom_model__";
const SPINNER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(80);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OnboardingResult {
    /// User selected a catalog model (slug).
    CatalogModelSelected { slug: String },
    /// User entered a custom model name and needs provider type selection.
    CustomModelEntered { model: String },
    /// User completed provider config and wants to validate.
    Validate {
        model: String,
        provider: ProviderWireApi,
        base_url: Option<String>,
        api_key: Option<String>,
    },
    /// Validation succeeded, config should be saved.
    ValidationSucceeded {
        model: String,
        provider: ProviderWireApi,
        base_url: Option<String>,
        api_key: Option<String>,
    },
    /// User cancelled onboarding.
    Cancelled,
}

#[derive(Debug)]
enum OnboardingState {
    /// Step 1: Select a model from catalog or enter custom.
    ModelSelection {
        items: Vec<ModelSelectionItem>,
        state: ScrollState,
        search_query: String,
        filtered_indices: Vec<usize>,
    },
    /// Step 1b: Enter custom model name.
    CustomModelName { input: String, cursor_pos: usize },
    /// Step 1c: Select provider type for custom model.
    ProviderSelection {
        model: String,
        items: Vec<ProviderSelectionItem>,
        selected_idx: usize,
    },
    /// Step 2: Enter base URL.
    BaseUrl {
        model: String,
        provider: ProviderWireApi,
        input: String,
        default_url: String,
        cursor_pos: usize,
    },
    /// Step 3: Enter API key.
    ApiKey {
        model: String,
        provider: ProviderWireApi,
        base_url: Option<String>,
        input: String,
        cursor_pos: usize,
    },
    /// Step 4: Validating connection.
    Validating {
        model: String,
        provider: ProviderWireApi,
        base_url: Option<String>,
        api_key: Option<String>,
        started_at: Instant,
    },
    /// Validation failed, show error and retry options.
    ValidationFailed {
        model: String,
        provider: ProviderWireApi,
        base_url: Option<String>,
        api_key: Option<String>,
        error_message: String,
        selected_action: usize,
    },
}

#[derive(Debug)]
struct ModelSelectionItem {
    slug: String,
    display_name: String,
    description: String,
    context_window: u32,
    thinking_label: String,
    is_custom: bool,
}

#[derive(Debug)]
struct ProviderSelectionItem {
    label: String,
    description: String,
    provider: ProviderWireApi,
}

pub(crate) struct OnboardingView {
    state: OnboardingState,
    complete: bool,
    result: Option<OnboardingResult>,
    app_event_tx: AppEventSender,
    frame_requester: FrameRequester,
    animations_enabled: bool,
}

impl OnboardingView {
    pub(crate) fn new(
        models: &[Model],
        app_event_tx: AppEventSender,
        frame_requester: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        let items: Vec<ModelSelectionItem> = models
            .iter()
            .map(|m| {
                let thinking_label = match &m.thinking_capability {
                    devo_protocol::ThinkingCapability::Unsupported => String::new(),
                    devo_protocol::ThinkingCapability::Toggle => "thinking".to_string(),
                    devo_protocol::ThinkingCapability::Levels(levels) => {
                        if levels.is_empty() {
                            String::new()
                        } else {
                            format!("thinking: {}", levels.len())
                        }
                    }
                    devo_protocol::ThinkingCapability::ToggleWithLevels(_) => {
                        "thinking".to_string()
                    }
                };
                ModelSelectionItem {
                    slug: m.slug.clone(),
                    display_name: m.display_name.clone(),
                    description: m.description.clone().unwrap_or_default(),
                    context_window: m.context_window,
                    thinking_label,
                    is_custom: false,
                }
            })
            .collect();

        let mut all_items = items;
        all_items.push(ModelSelectionItem {
            slug: CUSTOM_MODEL_SENTINEL.to_string(),
            display_name: "Custom Model".to_string(),
            description: "Enter a custom model slug".to_string(),
            context_window: 0,
            thinking_label: String::new(),
            is_custom: true,
        });

        let filtered_indices = (0..all_items.len()).collect();
        let mut state = ScrollState::new();
        state.selected_idx = Some(0);

        Self {
            state: OnboardingState::ModelSelection {
                items: all_items,
                state,
                search_query: String::new(),
                filtered_indices,
            },
            complete: false,
            result: None,
            app_event_tx,
            frame_requester,
            animations_enabled,
        }
    }

    pub(crate) fn take_result(&mut self) -> Option<OnboardingResult> {
        self.result.take()
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn cancel(&mut self) {
        self.complete = true;
        self.result = Some(OnboardingResult::Cancelled);
    }

    /// Called when validation succeeds.
    pub(crate) fn on_validation_succeeded(&mut self, _reply_preview: String) {
        if let OnboardingState::Validating {
            model,
            provider,
            base_url,
            api_key,
            ..
        } = &self.state
        {
            self.result = Some(OnboardingResult::ValidationSucceeded {
                model: model.clone(),
                provider: *provider,
                base_url: base_url.clone(),
                api_key: api_key.clone(),
            });
            self.complete = true;
        }
    }

    /// Called when validation fails.
    pub(crate) fn on_validation_failed(&mut self, error_message: String) {
        if let OnboardingState::Validating {
            model,
            provider,
            base_url,
            api_key,
            ..
        } = &self.state
        {
            self.state = OnboardingState::ValidationFailed {
                model: model.clone(),
                provider: *provider,
                base_url: base_url.clone(),
                api_key: api_key.clone(),
                error_message,
                selected_action: 0,
            };
        }
    }

    // ── Model Selection ──

    fn model_selection_handle_key(&mut self, key: KeyEvent) {
        let OnboardingState::ModelSelection {
            items,
            state,
            search_query,
            filtered_indices,
        } = &mut self.state
        else {
            return;
        };

        match key.code {
            KeyCode::Up | KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Self::model_move_up(state, filtered_indices, items);
            }
            KeyCode::Up => {
                Self::model_move_up(state, filtered_indices, items);
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                Self::model_move_up(state, filtered_indices, items);
            }
            KeyCode::Down | KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Self::model_move_down(state, filtered_indices, items);
            }
            KeyCode::Down => {
                Self::model_move_down(state, filtered_indices, items);
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                Self::model_move_down(state, filtered_indices, items);
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                search_query.push(c);
                Self::model_apply_filter(items, search_query, filtered_indices, state);
            }
            KeyCode::Backspace => {
                search_query.pop();
                Self::model_apply_filter(items, search_query, filtered_indices, state);
            }
            KeyCode::Enter => {
                if let Some(visible_idx) = state.selected_idx
                    && let Some(&actual_idx) = filtered_indices.get(visible_idx)
                    && let Some(item) = items.get(actual_idx)
                {
                    if item.is_custom {
                        self.state = OnboardingState::CustomModelName {
                            input: String::new(),
                            cursor_pos: 0,
                        };
                    } else {
                        // Catalog model selected, go to base URL step
                        let slug = item.slug.clone();
                        let provider = Self::infer_provider(&slug);
                        let default_url = Self::default_base_url(provider);
                        self.state = OnboardingState::BaseUrl {
                            model: slug,
                            provider,
                            input: default_url.clone(),
                            default_url,
                            cursor_pos: 0,
                        };
                    }
                }
            }
            _ => {}
        }
    }

    fn model_move_up(
        state: &mut ScrollState,
        filtered_indices: &[usize],
        _items: &[ModelSelectionItem],
    ) {
        let len = filtered_indices.len();
        if len == 0 {
            return;
        }
        let current = state.selected_idx.unwrap_or(0);
        state.selected_idx = Some(if current == 0 { len - 1 } else { current - 1 });
    }

    fn model_move_down(
        state: &mut ScrollState,
        filtered_indices: &[usize],
        _items: &[ModelSelectionItem],
    ) {
        let len = filtered_indices.len();
        if len == 0 {
            return;
        }
        let current = state.selected_idx.unwrap_or(0);
        state.selected_idx = Some((current + 1) % len);
    }

    fn model_apply_filter(
        items: &[ModelSelectionItem],
        query: &str,
        filtered_indices: &mut Vec<usize>,
        state: &mut ScrollState,
    ) {
        let query_lower = query.to_lowercase();
        if query.is_empty() {
            *filtered_indices = (0..items.len()).collect();
        } else {
            *filtered_indices = items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    item.slug.to_lowercase().contains(&query_lower)
                        || item.display_name.to_lowercase().contains(&query_lower)
                        || item.description.to_lowercase().contains(&query_lower)
                })
                .map(|(idx, _)| idx)
                .collect();
        }
        // Reset selection to first filtered item
        state.selected_idx = if filtered_indices.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    fn infer_provider(slug: &str) -> ProviderWireApi {
        let slug_lower = slug.to_lowercase();
        if slug_lower.contains("claude") || slug_lower.contains("anthropic") {
            ProviderWireApi::AnthropicMessages
        } else {
            ProviderWireApi::OpenAIChatCompletions
        }
    }

    fn default_base_url(provider: ProviderWireApi) -> String {
        match provider {
            ProviderWireApi::AnthropicMessages => "https://api.anthropic.com".to_string(),
            ProviderWireApi::OpenAIChatCompletions => "https://api.openai.com/v1".to_string(),
            ProviderWireApi::OpenAIResponses => "https://api.openai.com/v1".to_string(),
        }
    }

    fn provider_selection_items() -> Vec<ProviderSelectionItem> {
        vec![
            ProviderSelectionItem {
                label: "OpenAI Chat Completions".to_string(),
                description: "Most providers (OpenAI, Together, Groq, ...)".to_string(),
                provider: ProviderWireApi::OpenAIChatCompletions,
            },
            ProviderSelectionItem {
                label: "OpenAI Responses".to_string(),
                description: "OpenAI native Responses API".to_string(),
                provider: ProviderWireApi::OpenAIResponses,
            },
            ProviderSelectionItem {
                label: "Anthropic Messages".to_string(),
                description: "Claude models via Anthropic API".to_string(),
                provider: ProviderWireApi::AnthropicMessages,
            },
        ]
    }

    // ── Custom Model Name ──

    fn custom_model_name_handle_key(&mut self, key: KeyEvent) {
        let OnboardingState::CustomModelName { input, cursor_pos } = &mut self.state else {
            return;
        };

        match key.code {
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                input.insert(*cursor_pos, c);
                *cursor_pos += 1;
            }
            KeyCode::Backspace => {
                if *cursor_pos > 0 {
                    input.remove(*cursor_pos - 1);
                    *cursor_pos -= 1;
                }
            }
            KeyCode::Delete => {
                if *cursor_pos < input.len() {
                    input.remove(*cursor_pos);
                }
            }
            KeyCode::Left => {
                if *cursor_pos > 0 {
                    *cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                if *cursor_pos < input.len() {
                    *cursor_pos += 1;
                }
            }
            KeyCode::Home => {
                *cursor_pos = 0;
            }
            KeyCode::End => {
                *cursor_pos = input.len();
            }
            KeyCode::Enter => {
                let model = input.trim().to_string();
                if model.is_empty() {
                    return;
                }
                self.state = OnboardingState::ProviderSelection {
                    model,
                    items: Self::provider_selection_items(),
                    selected_idx: 0,
                };
            }
            KeyCode::Esc => {
                self.go_back_to_model_selection();
            }
            _ => {}
        }
    }

    // ── Provider Selection ──

    fn provider_selection_handle_key(&mut self, key: KeyEvent) {
        let OnboardingState::ProviderSelection {
            model,
            items,
            selected_idx,
        } = &mut self.state
        else {
            return;
        };

        match key.code {
            KeyCode::Up => {
                *selected_idx = if *selected_idx == 0 {
                    items.len() - 1
                } else {
                    *selected_idx - 1
                };
            }
            KeyCode::Down => {
                *selected_idx = (*selected_idx + 1) % items.len();
            }
            KeyCode::Enter => {
                if let Some(item) = items.get(*selected_idx) {
                    let provider = item.provider;
                    let default_url = Self::default_base_url(provider);
                    self.state = OnboardingState::BaseUrl {
                        model: model.clone(),
                        provider,
                        input: default_url.clone(),
                        default_url,
                        cursor_pos: 0,
                    };
                }
            }
            KeyCode::Esc => {
                self.go_back_to_model_selection();
            }
            _ => {}
        }
    }

    // ── Provider Config ──

    // ── Provider Config (legacy, no longer used) ──

    fn provider_config_handle_key(&mut self, _key: KeyEvent) {}

    // ── Base URL ──

    fn base_url_handle_key(&mut self, key: KeyEvent) {
        let OnboardingState::BaseUrl {
            model,
            provider,
            input,
            default_url: _,
            cursor_pos,
        } = &mut self.state
        else {
            return;
        };

        match key.code {
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                input.insert(*cursor_pos, c);
                *cursor_pos += 1;
            }
            KeyCode::Backspace => {
                if *cursor_pos > 0 {
                    input.remove(*cursor_pos - 1);
                    *cursor_pos -= 1;
                }
            }
            KeyCode::Delete => {
                if *cursor_pos < input.len() {
                    input.remove(*cursor_pos);
                }
            }
            KeyCode::Left => {
                if *cursor_pos > 0 {
                    *cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                if *cursor_pos < input.len() {
                    *cursor_pos += 1;
                }
            }
            KeyCode::Home => {
                *cursor_pos = 0;
            }
            KeyCode::End => {
                *cursor_pos = input.len();
            }
            KeyCode::Enter => {
                let model = model.clone();
                let provider = *provider;
                let base_url = input.trim().to_string();
                let base_url_val = if base_url.is_empty() {
                    None
                } else {
                    Some(base_url)
                };
                self.state = OnboardingState::ApiKey {
                    model,
                    provider,
                    base_url: base_url_val,
                    input: String::new(),
                    cursor_pos: 0,
                };
            }
            KeyCode::Esc => {
                self.go_back_to_model_selection();
            }
            _ => {}
        }
    }

    // ── API Key ──

    fn api_key_handle_key(&mut self, key: KeyEvent) {
        let OnboardingState::ApiKey {
            model,
            provider,
            base_url,
            input,
            cursor_pos,
        } = &mut self.state
        else {
            return;
        };

        match key.code {
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                input.insert(*cursor_pos, c);
                *cursor_pos += 1;
            }
            KeyCode::Backspace => {
                if *cursor_pos > 0 {
                    input.remove(*cursor_pos - 1);
                    *cursor_pos -= 1;
                }
            }
            KeyCode::Delete => {
                if *cursor_pos < input.len() {
                    input.remove(*cursor_pos);
                }
            }
            KeyCode::Left => {
                if *cursor_pos > 0 {
                    *cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                if *cursor_pos < input.len() {
                    *cursor_pos += 1;
                }
            }
            KeyCode::Home => {
                *cursor_pos = 0;
            }
            KeyCode::End => {
                *cursor_pos = input.len();
            }
            KeyCode::Enter => {
                let model = model.clone();
                let provider = *provider;
                let base_url = base_url.clone();
                let api_key_val = if input.trim().is_empty() {
                    None
                } else {
                    Some(input.trim().to_string())
                };
                self.state = OnboardingState::Validating {
                    model: model.clone(),
                    provider,
                    base_url: base_url.clone(),
                    api_key: api_key_val.clone(),
                    started_at: Instant::now(),
                };
                let payload = serde_json::json!({
                    "model": model,
                    "base_url": base_url,
                    "api_key": api_key_val,
                });
                self.app_event_tx
                    .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                        command: format!("onboard {payload}"),
                    }));
            }
            KeyCode::Esc => {
                // Go back to base URL step
                let model = model.clone();
                let provider = *provider;
                let default_url = Self::default_base_url(provider);
                self.state = OnboardingState::BaseUrl {
                    model,
                    provider,
                    input: default_url.clone(),
                    default_url,
                    cursor_pos: 0,
                };
            }
            _ => {}
        }
    }

    // ── Validation Failed ──

    fn validation_failed_handle_key(&mut self, key: KeyEvent) {
        let OnboardingState::ValidationFailed {
            model,
            provider,
            base_url,
            api_key,
            error_message: _,
            selected_action,
        } = &mut self.state
        else {
            return;
        };

        let actions = [
            "Retry with current settings",
            "Edit settings",
            "Choose different model",
        ];

        match key.code {
            KeyCode::Up => {
                *selected_action = if *selected_action == 0 {
                    actions.len() - 1
                } else {
                    *selected_action - 1
                };
            }
            KeyCode::Down => {
                *selected_action = (*selected_action + 1) % actions.len();
            }
            KeyCode::Enter => match *selected_action {
                0 => {
                    // Retry with current settings
                    let model = model.clone();
                    let provider = *provider;
                    let base_url = base_url.clone();
                    let api_key = api_key.clone();
                    self.state = OnboardingState::Validating {
                        model: model.clone(),
                        provider,
                        base_url: base_url.clone(),
                        api_key: api_key.clone(),
                        started_at: Instant::now(),
                    };
                    let payload = serde_json::json!({
                        "model": model,
                        "base_url": base_url,
                        "api_key": api_key,
                    });
                    self.app_event_tx
                        .send(AppEvent::Command(AppCommand::RunUserShellCommand {
                            command: format!("onboard {payload}"),
                        }));
                }
                1 => {
                    // Edit settings: go back to API key step
                    self.state = OnboardingState::ApiKey {
                        model: model.clone(),
                        provider: *provider,
                        base_url: base_url.clone(),
                        input: api_key.clone().unwrap_or_default(),
                        cursor_pos: 0,
                    };
                }
                2 => {
                    // Choose different model
                    self.go_back_to_model_selection();
                }
                _ => {}
            },
            KeyCode::Esc => {
                self.complete = true;
                self.result = Some(OnboardingResult::Cancelled);
            }
            _ => {}
        }
    }

    fn go_back_to_model_selection(&mut self) {
        // Rebuild model selection from the original catalog models
        // We store a minimal version; in practice the caller should provide models.
        // For now, we create a placeholder state.
        self.state = OnboardingState::ModelSelection {
            items: Vec::new(), // Will be populated on next render if needed
            state: ScrollState::new(),
            search_query: String::new(),
            filtered_indices: Vec::new(),
        };
        self.complete = true;
        self.result = Some(OnboardingResult::Cancelled);
    }

    // ── Rendering ──

    fn render_model_selection(
        items: &[ModelSelectionItem],
        state: &ScrollState,
        search_query: &str,
        filtered_indices: &[usize],
        area: Rect,
        buf: &mut Buffer,
    ) {
        if area.height < 3 {
            return;
        }

        let content_area = onboarding_content_area(area);

        let mut lines: Vec<Line<'static>> = Vec::new();

        // Title
        lines.push(Line::from(vec![Span::styled(
            "  Welcome to Devo",
            Style::default().bold(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Choose a model to get started.",
            Style::default().dim(),
        )]));
        lines.push(Line::from(""));

        // Search line
        let search_display = if search_query.is_empty() {
            Span::styled("Search models...", Style::default().dim())
        } else {
            Span::styled(search_query.to_string(), Style::default())
        };
        lines.push(Line::from(vec![
            Span::styled("  ▌ ", Style::default().cyan()),
            search_display,
        ]));
        lines.push(Line::from(""));

        // Model list
        let max_visible = MAX_POPUP_ROWS.min(filtered_indices.len().max(1));
        let scroll_offset = state
            .selected_idx
            .map(|sel| {
                if sel >= max_visible.saturating_sub(2) {
                    sel.saturating_sub(max_visible.saturating_sub(3))
                } else {
                    0
                }
            })
            .unwrap_or(0);

        for (vis_idx, &actual_idx) in filtered_indices
            .iter()
            .enumerate()
            .skip(scroll_offset)
            .take(max_visible)
        {
            if let Some(item) = items.get(actual_idx) {
                let is_selected = state.selected_idx == Some(vis_idx);
                let prefix = if is_selected { "› " } else { "  " };

                if item.is_custom {
                    lines.push(Line::from(""));
                    let name_style = if is_selected {
                        Style::default().bold()
                    } else {
                        Style::default().dim()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            prefix.to_string(),
                            if is_selected {
                                Style::default().cyan()
                            } else {
                                Style::default()
                            },
                        ),
                        Span::styled("── Custom Model ──", name_style),
                    ]));
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled("Enter any model slug", Style::default().dim()),
                    ]));
                } else {
                    let name_style = if is_selected {
                        Style::default().bold()
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            prefix.to_string(),
                            if is_selected {
                                Style::default().cyan()
                            } else {
                                Style::default()
                            },
                        ),
                        Span::styled(item.display_name.clone(), name_style),
                    ]));

                    // Description line with metadata
                    let mut meta_parts = Vec::new();
                    if !item.description.is_empty() {
                        meta_parts.push(item.description.clone());
                    }
                    if item.context_window > 0 {
                        meta_parts.push(format!("{}K ctx", item.context_window / 1000));
                    }
                    if !item.thinking_label.is_empty() {
                        meta_parts.push(item.thinking_label.clone());
                    }
                    if !meta_parts.is_empty() {
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(meta_parts.join(" · "), Style::default().dim()),
                        ]));
                    }
                }
            }
        }

        // Footer hint
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  ↑↓ Navigate  Enter Select  Type to search  Esc Cancel",
            Style::default().dim(),
        )]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }

    fn render_custom_model_name(input: &str, cursor_pos: usize, area: Rect, buf: &mut Buffer) {
        if area.height < 3 {
            return;
        }

        let content_area = onboarding_content_area(area);

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![Span::styled(
            "  Enter Model Name",
            Style::default().bold(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            "  Type the model slug for your custom model.",
            Style::default().dim(),
        )]));
        lines.push(Line::from(""));

        // Input field
        let byte_pos = input
            .char_indices()
            .nth(cursor_pos.min(input.chars().count()))
            .map(|(i, _)| i)
            .unwrap_or(input.len());
        let before_cursor = input[..byte_pos].to_string();
        lines.push(Line::from(vec![
            Span::styled("  ▸ ", Style::default().cyan()),
            Span::styled(before_cursor, Style::default()),
            Span::styled("▌", Style::default().cyan()),
        ]));

        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  Enter Confirm · Esc Back",
            Style::default().dim(),
        )]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }

    fn render_provider_selection(
        model: &str,
        items: &[ProviderSelectionItem],
        selected_idx: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if area.height < 3 {
            return;
        }

        let content_area = onboarding_content_area(area);

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![Span::styled(
            "  Select Provider Type",
            Style::default().bold(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!("  Model: {model}"),
            Style::default().dim(),
        )]));
        lines.push(Line::from(""));

        for (idx, item) in items.iter().enumerate() {
            let is_selected = idx == selected_idx;
            let prefix = if is_selected { "› " } else { "  " };
            let name_style = if is_selected {
                Style::default().bold()
            } else {
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    prefix.to_string(),
                    if is_selected {
                        Style::default().cyan()
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(item.label.clone(), name_style),
            ]));
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(item.description.clone(), Style::default().dim()),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  ↑↓ Navigate  Enter Select  Esc Back",
            Style::default().dim(),
        )]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }

    fn render_base_url(
        model: &str,
        provider: ProviderWireApi,
        input: &str,
        default_url: &str,
        cursor_pos: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if area.height < 3 {
            return;
        }

        let content_area = onboarding_content_area(area);

        let provider_name = match provider {
            ProviderWireApi::AnthropicMessages => "Anthropic",
            ProviderWireApi::OpenAIChatCompletions => "OpenAI Chat Completions",
            ProviderWireApi::OpenAIResponses => "OpenAI Responses",
        };

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![Span::styled(
            "  Configure Provider",
            Style::default().bold(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!("  Model: {model} ({provider_name})"),
            Style::default().dim(),
        )]));
        lines.push(Line::from(""));

        // Step indicator
        lines.push(Line::from(vec![Span::styled(
            "  Step 1/2: Base URL",
            Style::default().cyan(),
        )]));
        lines.push(Line::from(""));

        // Base URL field
        let display = if input.is_empty() {
            String::new()
        } else {
            input.to_string()
        };
        let byte_pos = display
            .char_indices()
            .nth(cursor_pos.min(display.chars().count()))
            .map(|(i, _)| i)
            .unwrap_or(display.len());
        let before_cursor = display[..byte_pos].to_string();
        lines.push(Line::from(vec![
            Span::styled("  ▸ ", Style::default().cyan()),
            Span::styled(before_cursor, Style::default()),
            Span::styled("▌", Style::default().cyan()),
        ]));

        if !default_url.is_empty() {
            lines.push(Line::from(vec![Span::styled(
                format!("  Default: {default_url}"),
                Style::default().dim(),
            )]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  Enter Continue · Esc Back",
            Style::default().dim(),
        )]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }

    fn render_api_key(
        model: &str,
        provider: ProviderWireApi,
        input: &str,
        cursor_pos: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if area.height < 3 {
            return;
        }

        let content_area = onboarding_content_area(area);

        let provider_name = match provider {
            ProviderWireApi::AnthropicMessages => "Anthropic",
            ProviderWireApi::OpenAIChatCompletions => "OpenAI Chat Completions",
            ProviderWireApi::OpenAIResponses => "OpenAI Responses",
        };

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![Span::styled(
            "  Configure Provider",
            Style::default().bold(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!("  Model: {model} ({provider_name})"),
            Style::default().dim(),
        )]));
        lines.push(Line::from(""));

        // Step indicator
        lines.push(Line::from(vec![Span::styled(
            "  Step 2/2: API Key",
            Style::default().cyan(),
        )]));
        lines.push(Line::from(""));

        // API key input field (masked)
        let masked_display = if input.is_empty() {
            String::new()
        } else {
            "•".repeat(input.len())
        };
        let byte_pos = masked_display
            .char_indices()
            .nth(cursor_pos.min(masked_display.chars().count()))
            .map(|(i, _)| i)
            .unwrap_or(masked_display.len());
        let before_cursor = masked_display[..byte_pos].to_string();
        lines.push(Line::from(vec![
            Span::styled("  ▸ ", Style::default().cyan()),
            Span::styled(before_cursor, Style::default()),
            Span::styled("▌", Style::default().cyan()),
        ]));

        lines.push(Line::from(vec![Span::styled(
            "  Leave empty and press Enter to skip",
            Style::default().dim(),
        )]));

        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  Enter Validate · Esc Back",
            Style::default().dim(),
        )]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }

    fn render_validating(
        model: &str,
        provider: ProviderWireApi,
        started_at: Instant,
        animations_enabled: bool,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if area.height < 3 {
            return;
        }

        let content_area = onboarding_content_area(area);

        let provider_name = match provider {
            ProviderWireApi::AnthropicMessages => "Anthropic",
            ProviderWireApi::OpenAIChatCompletions => "OpenAI Chat Completions",
            ProviderWireApi::OpenAIResponses => "OpenAI Responses",
        };
        let elapsed = started_at.elapsed().as_secs();
        let remaining = 20u64.saturating_sub(elapsed);

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(vec![Span::styled(
            "  Validating...",
            Style::default().bold(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!("  Model: {model} ({provider_name})"),
            Style::default().dim(),
        )]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            spinner(Some(started_at), animations_enabled),
            Span::raw(" Connecting to API..."),
        ]));
        lines.push(Line::from(vec![Span::styled(
            "  Testing with prompt: \"Reply with OK only.\"".to_string(),
            Style::default().dim(),
        )]));
        lines.push(Line::from(vec![Span::styled(
            format!("  Timeout: {remaining}s remaining"),
            Style::default().dim(),
        )]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  Esc Cancel",
            Style::default().dim(),
        )]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }

    fn render_validation_failed(
        error_message: &str,
        selected_action: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        if area.height < 3 {
            return;
        }

        let content_area = onboarding_content_area(area);

        let actions = [
            "Retry with current settings",
            "Edit settings",
            "Choose different model",
        ];

        let mut lines: Vec<Line<'static>> = vec![
            Line::from(vec![Span::styled(
                "  ✗ Validation Failed",
                Style::default().bold().red(),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::raw("  "),
                Span::styled(error_message.to_string(), Style::default().red()),
            ]),
            Line::from(""),
        ];

        for (idx, action) in actions.iter().enumerate() {
            let is_selected = idx == selected_action;
            let prefix = if is_selected { "› " } else { "  " };
            let style = if is_selected {
                Style::default().bold()
            } else {
                Style::default().dim()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    prefix.to_string(),
                    if is_selected {
                        Style::default().cyan()
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(action.to_string(), style),
            ]));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            "  ↑↓ Navigate  Enter Select  Esc Exit onboarding",
            Style::default().dim(),
        )]));

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(content_area, buf);
    }
}

impl OnboardingView {
    /// Entry point for key events, called by `OnboardingHandle`.
    pub(crate) fn handle_key_event(&mut self, key_event: KeyEvent) {
        if matches!(key_event.kind, KeyEventKind::Release) {
            return;
        }
        match &self.state {
            OnboardingState::ModelSelection { .. } => self.model_selection_handle_key(key_event),
            OnboardingState::CustomModelName { .. } => self.custom_model_name_handle_key(key_event),
            OnboardingState::ProviderSelection { .. } => {
                self.provider_selection_handle_key(key_event)
            }
            OnboardingState::BaseUrl { .. } => self.base_url_handle_key(key_event),
            OnboardingState::ApiKey { .. } => self.api_key_handle_key(key_event),
            OnboardingState::Validating { .. } => {
                if key_event.code == KeyCode::Esc {
                    self.complete = true;
                    self.result = Some(OnboardingResult::Cancelled);
                }
            }
            OnboardingState::ValidationFailed { .. } => {
                self.validation_failed_handle_key(key_event);
            }
        }
    }
}

impl Renderable for OnboardingView {
    fn desired_height(&self, _width: u16) -> u16 {
        match &self.state {
            OnboardingState::ModelSelection {
                filtered_indices, ..
            } => {
                // title + subtitle + blank + search + blank + items + blank + footer
                let items = MAX_POPUP_ROWS.min(filtered_indices.len().max(1)) as u16;
                items * 2 + 7
            }
            OnboardingState::CustomModelName { .. } => 8,
            OnboardingState::ProviderSelection { .. } => 14,
            OnboardingState::BaseUrl { .. } => 11,
            OnboardingState::ApiKey { .. } => 12,
            OnboardingState::Validating { .. } => 10,
            OnboardingState::ValidationFailed { .. } => 12,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        match &self.state {
            OnboardingState::ModelSelection {
                items,
                state,
                search_query,
                filtered_indices,
            } => {
                Self::render_model_selection(
                    items,
                    state,
                    search_query,
                    filtered_indices,
                    area,
                    buf,
                );
            }
            OnboardingState::CustomModelName { input, cursor_pos } => {
                Self::render_custom_model_name(input, *cursor_pos, area, buf);
            }
            OnboardingState::ProviderSelection {
                model,
                items,
                selected_idx,
            } => {
                Self::render_provider_selection(model, items, *selected_idx, area, buf);
            }
            OnboardingState::BaseUrl {
                model,
                provider,
                input,
                default_url,
                cursor_pos,
            } => {
                Self::render_base_url(model, *provider, input, default_url, *cursor_pos, area, buf);
            }
            OnboardingState::ApiKey {
                model,
                provider,
                input,
                cursor_pos,
                ..
            } => {
                Self::render_api_key(model, *provider, input, *cursor_pos, area, buf);
            }
            OnboardingState::Validating {
                model,
                provider,
                started_at,
                ..
            } => {
                if self.animations_enabled {
                    self.frame_requester.schedule_frame_in(SPINNER_INTERVAL);
                }
                Self::render_validating(
                    model,
                    *provider,
                    *started_at,
                    self.animations_enabled,
                    area,
                    buf,
                );
            }
            OnboardingState::ValidationFailed {
                error_message,
                selected_action,
                ..
            } => {
                Self::render_validation_failed(error_message, *selected_action, area, buf);
            }
        }
    }

    fn cursor_pos(&self, _area: Rect) -> Option<(u16, u16)> {
        // Hide terminal cursor; we use ▌ as visual cursor indicator.
        None
    }
}
