mod composer;
mod layout;
mod theme;
mod transcript;

use ratatui::{
    layout::{Constraint, Flex, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap},
    Frame,
};
use std::{
    collections::HashMap,
    path::Path,
    process::Command,
    sync::{LazyLock, Mutex},
};

use crate::{
    app::{AuxPanel, AuxPanelContent, TuiApp},
    events::{ModelListEntry, ThinkingListEntry},
};
use clawcr_core::{BuiltinModelCatalog, ModelCatalog};

const MIN_OVERLAY_WIDTH: u16 = 44;
const MAX_OVERLAY_WIDTH: u16 = 76;
const ONBOARDING_OVERLAY_WIDTH: u16 = 88;
const MAX_LIST_OVERLAY_HEIGHT: u16 = 14;
const MAX_ONBOARDING_LIST_OVERLAY_HEIGHT: u16 = 18;
const MAX_TEXT_OVERLAY_HEIGHT: u16 = 12;
const BRAND_HEADER_HEIGHT: u16 = 6;
static GIT_BRANCH_CACHE: LazyLock<Mutex<HashMap<String, Option<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static BUILTIN_MODEL_CATALOG: LazyLock<Option<BuiltinModelCatalog>> =
    LazyLock::new(|| BuiltinModelCatalog::load().ok());

pub(crate) fn draw(frame: &mut Frame, app: &TuiApp) {
    frame.render_widget(Clear, frame.area());
    let content_area = centered_content_area(frame.area());
    let composer_height = composer_height(app, content_area);
    let transcript_height = transcript_height(app, content_area);
    let [transcript_area, spacer_area, composer_area, footer_area] = Layout::vertical([
        Constraint::Min(transcript_height),
        Constraint::Length(1),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(content_area);

    frame.render_widget(
        transcript::render(
            app,
            transcript_area.width.max(1),
            transcript_area.height.max(1),
        ),
        transcript_area,
    );
    frame.render_widget(Paragraph::new(""), spacer_area);

    let composer_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::composer_border(app))
        .title(composer_title(app));
    let composer_inner = composer_block.inner(composer_area);
    frame.render_widget(composer_block, composer_area);
    if let Some(panel) = inline_aux_panel(app) {
        let input_height = composer::line_count(app, layout::inner_width(composer_area)).max(1);
        let [input_area, panel_render_area] = Layout::vertical([
            Constraint::Length(input_height),
            Constraint::Length(panel.height),
        ])
        .areas(composer_inner);
        frame.render_widget(
            composer::render(app, layout::inner_width(composer_area)),
            input_area,
        );
        render_inline_aux_panel(frame, panel_render_area, app, panel);
    } else {
        frame.render_widget(
            composer::render(app, layout::inner_width(composer_area)),
            composer_inner,
        );
    }
    frame.render_widget(render_footer(app), footer_area);

    render_overlay(frame, app, content_area, transcript_area, composer_area);

    frame.set_cursor_position(composer::cursor(app, composer_area));
}

pub(crate) fn centered_content_area(area: Rect) -> Rect {
    layout::centered_content_area(area)
}

pub(crate) fn get_max_scroll(app: &TuiApp, area: Rect) -> u16 {
    let line_count = transcript::line_count(app, area.width.max(1));
    line_count.saturating_sub(area.height)
}

pub(crate) fn transcript_height(app: &TuiApp, area: Rect) -> u16 {
    let line_count = transcript::line_count(app, area.width.max(1)).max(1);
    let composer_height = composer_height(app, area);
    let available = area
        .height
        .saturating_sub(composer_height.saturating_add(2));
    line_count.min(available.max(1))
}

pub(crate) fn composer_height(app: &TuiApp, area: Rect) -> u16 {
    let base_height = composer::line_count(app, layout::inner_width(area))
        .saturating_add(2)
        .clamp(
            3,
            area.height
                .saturating_sub(1)
                .max(3)
                .min(layout::COMPOSER_MAX_HEIGHT),
        );
    base_height
        .saturating_add(aux_panel_height(app))
        .min(area.height.saturating_sub(1).max(3))
}

fn composer_title(app: &TuiApp) -> Line<'static> {
    if let Some(prompt) = app.onboarding_prompt.as_deref() {
        return Line::from(vec![
            Span::styled(" onboarding ", theme::prompt()),
            Span::styled(format!(" {prompt} "), theme::muted()),
        ]);
    }

    let state = if app.busy { " running " } else { " ready " };
    Line::from(vec![
        Span::styled(" compose ", theme::prompt()),
        Span::styled(state, theme::muted()),
    ])
}

fn render_footer(app: &TuiApp) -> Paragraph<'static> {
    if app.onboarding_prompt.is_some() {
        return Paragraph::new(Line::from(""));
    }

    let cwd = app.cwd.to_string_lossy().into_owned();
    let branch = resolve_git_branch(&app.cwd).unwrap_or_else(|| "no-git".to_string());
    let token_usage = format!(
        "tokens {} in / {} out",
        format_token_count(app.total_input_tokens),
        format_token_count(app.total_output_tokens)
    );
    let context_usage = render_context_usage(app);
    let mut spans = vec![
        Span::styled(app.model.clone(), theme::muted()),
        Span::styled("  |  ", theme::muted()),
    ];
    spans.extend([
        Span::styled(token_usage, theme::muted()),
        Span::styled("  |  ", theme::muted()),
        Span::styled(context_usage, theme::muted()),
        Span::styled("  |  ", theme::muted()),
        Span::styled(format!("{cwd} ({branch})"), theme::muted()),
    ]);
    Paragraph::new(Line::from(spans))
}

fn render_context_usage(app: &TuiApp) -> String {
    let Some(catalog) = &*BUILTIN_MODEL_CATALOG else {
        return "ctx n/a".to_string();
    };
    let Some(model) = catalog.get(&app.model) else {
        return "ctx n/a".to_string();
    };

    let input_budget = usize::try_from(model.context_window)
        .unwrap_or_default()
        .saturating_mul(usize::from(model.effective_context_window_percent))
        / 100;
    if input_budget == 0 {
        return "ctx n/a".to_string();
    }

    let used = app.total_input_tokens.min(input_budget);
    let used_percent = used.saturating_mul(100) / input_budget;
    format!(
        "ctx {} / {} ({used_percent}%)",
        format_token_count(used),
        format_token_count(input_budget),
    )
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

fn resolve_git_branch(cwd: &Path) -> Option<String> {
    let key = cwd.to_string_lossy().into_owned();
    if let Ok(cache) = GIT_BRANCH_CACHE.lock() {
        if let Some(branch) = cache.get(&key) {
            return branch.clone();
        }
    }

    let branch = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if text.is_empty() || text == "HEAD" {
                    None
                } else {
                    Some(text)
                }
            } else {
                None
            }
        });

    if let Ok(mut cache) = GIT_BRANCH_CACHE.lock() {
        cache.insert(key, branch.clone());
    }
    branch
}

fn render_overlay(
    frame: &mut Frame,
    app: &TuiApp,
    content_area: Rect,
    transcript_area: Rect,
    composer_area: Rect,
) {
    if let Some(panel) = &app.aux_panel {
        if inline_aux_panel(app).is_some() {
            return;
        }
        render_aux_panel_overlay(frame, app, content_area, transcript_area, panel);
        return;
    }

    let suggestions = app.slash_suggestions();
    if suggestions.is_empty() {
        return;
    }

    let overlay_area = composer_popup_area(
        content_area,
        composer_area,
        suggestions.len().saturating_add(2) as u16,
        1,
    );
    let items: Vec<ListItem<'static>> = suggestions
        .iter()
        .map(|suggestion| {
            ListItem::new(Line::from(vec![
                Span::styled(suggestion.name, theme::panel_title()),
                Span::styled("  ", theme::muted()),
                Span::styled(suggestion.description, theme::muted()),
            ]))
        })
        .collect();
    let mut state = ListState::default();
    state.select(Some(
        app.slash_selection.min(suggestions.len().saturating_sub(1)),
    ));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::overlay_border())
                .title(" Commands ")
                .padding(Padding::horizontal(1)),
        )
        .highlight_style(theme::selected().add_modifier(Modifier::BOLD))
        .highlight_symbol("› ");
    frame.render_widget(Clear, overlay_area);
    frame.render_stateful_widget(list, overlay_area, &mut state);
}

fn render_aux_panel_overlay(
    frame: &mut Frame,
    app: &TuiApp,
    content_area: Rect,
    transcript_area: Rect,
    panel: &AuxPanel,
) {
    match &panel.content {
        AuxPanelContent::Text(body) => {
            let overlay_area = if app.onboarding_prompt.is_some() {
                centered_popup_area(content_area, MAX_TEXT_OVERLAY_HEIGHT, 0)
            } else {
                bottom_popup_area(transcript_area, MAX_TEXT_OVERLAY_HEIGHT, 0)
            };
            let text = Paragraph::new(body.clone())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::overlay_border())
                        .title(format!(" {} ", panel.title))
                        .padding(Padding::horizontal(1)),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(Clear, overlay_area);
            frame.render_widget(text, overlay_area);
        }
        AuxPanelContent::SessionList(entries) => {
            let overlay_area = if app.onboarding_prompt.is_some() {
                centered_popup_area(
                    content_area,
                    entries.len().saturating_add(4) as u16,
                    entries.len(),
                )
            } else {
                bottom_popup_area(
                    transcript_area,
                    entries.len().saturating_add(4) as u16,
                    entries.len(),
                )
            };
            let items = session_items(entries);
            let mut state = ListState::default();
            if !entries.is_empty() {
                state.select(Some(
                    app.aux_panel_selection.min(entries.len().saturating_sub(1)),
                ));
            }
            let list = List::new(items)
                .block(overlay_block(panel.title.as_str(), false))
                .highlight_style(theme::selected().add_modifier(Modifier::BOLD))
                .highlight_symbol("› ");
            frame.render_widget(Clear, overlay_area);
            frame.render_stateful_widget(list, overlay_area, &mut state);
        }
        AuxPanelContent::ModelList(entries) => {
            let overlay_area = if app.show_model_onboarding {
                onboarding_popup_area(
                    content_area,
                    entries.len().saturating_mul(2).saturating_add(4) as u16,
                )
            } else if app.onboarding_prompt.is_some() {
                centered_popup_area(
                    content_area,
                    entries.len().saturating_add(4) as u16,
                    entries.len(),
                )
            } else {
                bottom_popup_area(
                    transcript_area,
                    entries.len().saturating_add(4) as u16,
                    entries.len(),
                )
            };
            let items = model_items(app, entries);
            let mut state = ListState::default();
            if !entries.is_empty() {
                state.select(Some(
                    app.aux_panel_selection.min(entries.len().saturating_sub(1)),
                ));
            }
            let list = List::new(items)
                .block(overlay_block(
                    panel.title.as_str(),
                    app.show_model_onboarding,
                ))
                .highlight_style(theme::selected().add_modifier(Modifier::BOLD))
                .highlight_symbol("› ");
            frame.render_widget(Clear, overlay_area);
            frame.render_stateful_widget(list, overlay_area, &mut state);
        }
        AuxPanelContent::ThinkingList(entries) => {
            let items = thinking_items(entries);
            let mut state = ListState::default();
            if !entries.is_empty() {
                state.select(Some(
                    app.aux_panel_selection.min(entries.len().saturating_sub(1)),
                ));
            }
            let overlay_area = if app.onboarding_prompt.is_some() {
                centered_popup_area(
                    content_area,
                    entries.len().saturating_add(4) as u16,
                    entries.len(),
                )
            } else {
                bottom_popup_area(
                    transcript_area,
                    entries.len().saturating_add(4) as u16,
                    entries.len(),
                )
            };
            let list = List::new(items)
                .block(overlay_block(&panel.title, false))
                .highlight_style(theme::selected().add_modifier(Modifier::BOLD))
                .highlight_symbol("› ");
            frame.render_stateful_widget(list, overlay_area, &mut state);
        }
    }
}

fn inline_model_panel_height(entries: &[ModelListEntry]) -> u16 {
    entries.len().saturating_mul(2).saturating_add(1).min(8) as u16
}

struct InlineAuxPanel<'a> {
    title: &'a str,
    content: &'a AuxPanelContent,
    height: u16,
}

fn inline_aux_panel(app: &TuiApp) -> Option<InlineAuxPanel<'_>> {
    let panel = app.aux_panel.as_ref()?;
    let height = aux_panel_height(app);
    if height == 0 {
        return None;
    }

    Some(InlineAuxPanel {
        title: panel.title.as_str(),
        content: &panel.content,
        height,
    })
}

fn render_inline_aux_panel(frame: &mut Frame, area: Rect, app: &TuiApp, panel: InlineAuxPanel<'_>) {
    match panel.content {
        AuxPanelContent::Text(body) => {
            let text = Paragraph::new(body.clone())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme::overlay_border())
                        .title(format!(" {} ", panel.title))
                        .padding(Padding::horizontal(1)),
                )
                .wrap(Wrap { trim: false });
            frame.render_widget(text, area);
        }
        AuxPanelContent::SessionList(entries) => {
            let items = session_items(entries);
            let mut state = ListState::default();
            if !entries.is_empty() {
                state.select(Some(
                    app.aux_panel_selection.min(entries.len().saturating_sub(1)),
                ));
            }
            let list = List::new(items)
                .block(overlay_block(&panel.title, false))
                .highlight_style(theme::selected().add_modifier(Modifier::BOLD))
                .highlight_symbol("› ");
            frame.render_stateful_widget(list, area, &mut state);
        }
        AuxPanelContent::ThinkingList(entries) => {
            let items = thinking_items(entries);
            let mut state = ListState::default();
            if !entries.is_empty() {
                state.select(Some(
                    app.aux_panel_selection.min(entries.len().saturating_sub(1)),
                ));
            }
            let list = List::new(items)
                .block(overlay_block(&panel.title, false))
                .highlight_style(theme::selected().add_modifier(Modifier::BOLD))
                .highlight_symbol("› ");
            frame.render_stateful_widget(list, area, &mut state);
        }
        AuxPanelContent::ModelList(entries) => {
            let items = model_items(app, entries);
            let mut state = ListState::default();
            if !entries.is_empty() {
                state.select(Some(
                    app.aux_panel_selection.min(entries.len().saturating_sub(1)),
                ));
            }
            let list = List::new(items)
                .highlight_style(theme::selected().add_modifier(Modifier::BOLD))
                .highlight_symbol("› ");
            frame.render_stateful_widget(list, area, &mut state);
        }
    }
}

fn overlay_block(title: &str, hide_title: bool) -> Block<'static> {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::overlay_border())
        .padding(Padding::horizontal(1));
    if hide_title || title.is_empty() {
        block.title(" Esc back ")
    } else {
        block.title(format!(" {title} "))
    }
}

fn bottom_popup_area(base_area: Rect, desired_height: u16, item_count: usize) -> Rect {
    let width = base_area.width.clamp(MIN_OVERLAY_WIDTH, MAX_OVERLAY_WIDTH);
    let height = desired_height
        .max(4)
        .min(base_area.height.saturating_sub(1).max(4))
        .min(if item_count == 0 {
            MAX_TEXT_OVERLAY_HEIGHT
        } else {
            MAX_LIST_OVERLAY_HEIGHT
        });
    Rect {
        x: base_area.x + base_area.width.saturating_sub(width),
        y: base_area.y + base_area.height.saturating_sub(height),
        width,
        height,
    }
}

fn composer_popup_area(
    content_area: Rect,
    composer_area: Rect,
    desired_height: u16,
    item_count: usize,
) -> Rect {
    let width = composer_area
        .width
        .clamp(MIN_OVERLAY_WIDTH, MAX_OVERLAY_WIDTH);
    let height = desired_height
        .max(4)
        .min(content_area.height.saturating_sub(1).max(4))
        .min(if item_count == 0 {
            MAX_TEXT_OVERLAY_HEIGHT
        } else {
            MAX_LIST_OVERLAY_HEIGHT
        });
    Rect {
        x: composer_area.x,
        y: composer_area.y.saturating_sub(height),
        width,
        height,
    }
}

fn centered_popup_area(base_area: Rect, desired_height: u16, item_count: usize) -> Rect {
    let width = base_area.width.clamp(MIN_OVERLAY_WIDTH, MAX_OVERLAY_WIDTH);
    let height = desired_height
        .max(4)
        .min(base_area.height.saturating_sub(2).max(4))
        .min(if item_count == 0 {
            MAX_TEXT_OVERLAY_HEIGHT
        } else {
            MAX_LIST_OVERLAY_HEIGHT
        });
    let [area] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(base_area);
    let [area] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn onboarding_popup_area(base_area: Rect, desired_height: u16) -> Rect {
    let width = base_area.width.min(ONBOARDING_OVERLAY_WIDTH).max(56);
    let y = base_area.y.saturating_add(BRAND_HEADER_HEIGHT);
    let available_height = base_area
        .height
        .saturating_sub(BRAND_HEADER_HEIGHT)
        .saturating_sub(1)
        .max(8);
    let height = desired_height
        .max(8)
        .min(available_height)
        .min(MAX_ONBOARDING_LIST_OVERLAY_HEIGHT);
    Rect {
        x: base_area.x,
        y,
        width,
        height,
    }
}

fn session_items(entries: &[crate::events::SessionListEntry]) -> Vec<ListItem<'static>> {
    if entries.is_empty() {
        return vec![ListItem::new(Line::from(vec![Span::styled(
            "No saved sessions found.",
            theme::muted(),
        )]))];
    }

    entries
        .iter()
        .map(|entry| {
            let marker = if entry.is_active { "current" } else { "saved" };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(entry.title.clone(), theme::panel_title()),
                    Span::styled("  ", theme::muted()),
                    Span::styled(format!("[{marker}]"), theme::muted()),
                ]),
                Line::from(vec![
                    Span::styled(entry.session_id.to_string(), theme::muted()),
                    Span::styled("  ", theme::muted()),
                    Span::styled(entry.updated_at.clone(), theme::muted()),
                ]),
            ])
        })
        .collect()
}

fn model_items(app: &TuiApp, entries: &[ModelListEntry]) -> Vec<ListItem<'static>> {
    if entries.is_empty() {
        return vec![ListItem::new(Line::from(vec![Span::styled(
            "No models available.",
            theme::muted(),
        )]))];
    }

    entries
        .iter()
        .map(|entry| {
            if app.show_model_onboarding {
                let description = entry
                    .description
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(if entry.is_custom_mode {
                        "Type a model name manually"
                    } else {
                        ""
                    });
                let title = if entry.is_current {
                    format!("{}  current", entry.display_name)
                } else {
                    entry.display_name.clone()
                };
                return ListItem::new(vec![
                    Line::from(vec![Span::styled(title, theme::panel_title())]),
                    Line::from(vec![Span::styled(description.to_string(), theme::muted())]),
                ]);
            }

            let description = entry
                .description
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(if entry.is_custom_mode {
                    "Open onboarding to add another model"
                } else {
                    "saved model"
                });
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        entry.display_name.clone(),
                        if entry.is_custom_mode {
                            theme::prompt()
                        } else if entry.is_current {
                            Style::new().add_modifier(Modifier::BOLD)
                        } else {
                            theme::panel_title()
                        },
                    ),
                    if entry.is_current {
                        Span::styled("  current", theme::muted())
                    } else {
                        Span::raw("")
                    },
                ]),
                Line::from(vec![Span::styled(description.to_string(), theme::muted())]),
            ])
        })
        .collect()
}

fn thinking_items(entries: &[ThinkingListEntry]) -> Vec<ListItem<'static>> {
    if entries.is_empty() {
        return vec![ListItem::new(Line::from(vec![Span::styled(
            "No thinking options available.",
            theme::muted(),
        )]))];
    }

    entries
        .iter()
        .map(|entry| {
            let title = if entry.is_current {
                format!("{}  current", entry.label)
            } else {
                entry.label.clone()
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(title, theme::panel_title()),
                    Span::styled("  ", theme::muted()),
                    Span::styled(format!("[{}]", entry.value), theme::muted()),
                ]),
                Line::from(vec![Span::styled(
                    entry.description.clone(),
                    theme::muted(),
                )]),
            ])
        })
        .collect()
}

fn text_panel_height(body: &str) -> u16 {
    body.lines()
        .count()
        .saturating_add(2)
        .clamp(4, MAX_TEXT_OVERLAY_HEIGHT as usize) as u16
}

fn session_panel_height(entries: &[crate::events::SessionListEntry]) -> u16 {
    entries
        .len()
        .saturating_mul(2)
        .saturating_add(2)
        .clamp(4, MAX_LIST_OVERLAY_HEIGHT as usize) as u16
}

fn thinking_panel_height(entries: &[ThinkingListEntry]) -> u16 {
    entries
        .len()
        .saturating_mul(2)
        .saturating_add(2)
        .clamp(4, MAX_LIST_OVERLAY_HEIGHT as usize) as u16
}

fn aux_panel_height(app: &TuiApp) -> u16 {
    let Some(panel) = app.aux_panel.as_ref() else {
        return 0;
    };
    if app.show_model_onboarding {
        return 0;
    }

    match &panel.content {
        AuxPanelContent::Text(body) => text_panel_height(body),
        AuxPanelContent::SessionList(entries) => session_panel_height(entries),
        AuxPanelContent::ThinkingList(entries) => thinking_panel_height(entries),
        AuxPanelContent::ModelList(entries) => inline_model_panel_height(entries),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::{bottom_popup_area, centered_content_area};

    #[test]
    fn centers_wide_layouts() {
        let area = centered_content_area(Rect::new(5, 2, 160, 40));
        assert_eq!(area, Rect::new(5, 2, 160, 40));
    }

    #[test]
    fn bottom_popup_stays_inside_transcript_area() {
        let area = bottom_popup_area(Rect::new(10, 5, 90, 18), 20, 12);
        assert_eq!(area, Rect::new(24, 9, 76, 14));
    }
}
