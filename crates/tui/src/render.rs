use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
    Frame,
};

use crate::{app::TuiApp, events::TranscriptItemKind};

/// Draws the full interactive UI for the current application state.
pub(crate) fn draw(frame: &mut Frame, app: &TuiApp) {
    let content_area = centered_content_area(frame.area());
    let composer_height = composer_height(app, content_area);
    let [header_area, transcript_area, composer_area, footer_area] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(6),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(content_area);

    frame.render_widget(render_header(app), header_area);
    frame.render_widget(render_transcript(app, transcript_area), transcript_area);
    frame.render_widget(render_composer(app), composer_area);
    frame.render_widget(render_footer(app), footer_area);

    let cursor = composer_cursor(app, composer_area);
    frame.set_cursor_position(cursor);
}

fn centered_content_area(area: Rect) -> Rect {
    const MAX_CONTENT_WIDTH: u16 = 100;

    if area.width <= MAX_CONTENT_WIDTH {
        return area;
    }

    let horizontal_padding = (area.width - MAX_CONTENT_WIDTH) / 2;
    let [_, content, _] = Layout::horizontal([
        Constraint::Length(horizontal_padding),
        Constraint::Length(MAX_CONTENT_WIDTH),
        Constraint::Min(0),
    ])
    .areas(area);
    content
}

fn render_header(_app: &TuiApp) -> Paragraph<'static> {
    Paragraph::new(Text::from(vec![
        Line::from(vec![Span::styled(
            "   ________               _______ ",
            Style::new().cyan().add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            "  / ____/ /___ __      __/ ____/ |",
            Style::new().cyan().add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            " / /   / / __ `/ | /| / / /   | |",
            Style::new().cyan().add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            "/ /___/ / /_/ /| |/ |/ / /___ | |",
            Style::new().cyan().add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            "\\____/_/\\__,_/ |__/|__/\\____/ |_|",
            Style::new().cyan().add_modifier(Modifier::BOLD),
        )]),
    ]))
}

fn render_transcript(app: &TuiApp, area: Rect) -> Paragraph<'static> {
    let width = area.width.max(1);
    let content = transcript_text(app);
    let max_scroll = transcript_line_count(app, width).saturating_sub(area.height);
    let scroll = if app.follow_output {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };

    Paragraph::new(content)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
}

fn render_composer(app: &TuiApp) -> Paragraph<'_> {
    let mut input_lines = app.input.text().lines();
    let first_line = input_lines.next().unwrap_or_default().to_string();
    let mut lines = vec![Line::from(vec![
        Span::styled("> ", Style::new().cyan().add_modifier(Modifier::BOLD)),
        if first_line.is_empty() {
            Span::styled("Type a message or / for commands", Style::new().dark_gray())
        } else {
            Span::raw(first_line)
        },
    ])];
    for line in input_lines {
        lines.push(Line::from(format!("  {line}")));
    }
    let suggestions = app.slash_suggestions();
    if !suggestions.is_empty() {
        lines.push(Line::from(""));
        for (index, suggestion) in suggestions.iter().enumerate() {
            let selected = index == app.slash_selection.min(suggestions.len() - 1);
            let bullet_style = if selected {
                Style::new().black().on_gray().add_modifier(Modifier::BOLD)
            } else {
                Style::new().dark_gray()
            };
            let text_style = if selected {
                Style::new().black().on_gray()
            } else {
                Style::new().dark_gray()
            };
            lines.push(Line::from(vec![
                Span::styled("  ", text_style),
                Span::styled(if selected { ">" } else { "•" }, bullet_style),
                Span::styled(
                    format!(" {}  {}", suggestion.name, suggestion.description),
                    text_style,
                ),
            ]));
        }
    }

    Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false })
}

fn render_footer(app: &TuiApp) -> Paragraph<'static> {
    let cwd_name = app
        .cwd
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| app.cwd.to_string_lossy().into_owned());
    let footer = format!("model {}   cwd {}", app.model, cwd_name);
    Paragraph::new(footer).style(Style::new().dark_gray())
}

fn transcript_text(app: &TuiApp) -> Text<'static> {
    if app.transcript.is_empty() {
        return Text::from(vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                "No conversation yet. Ask ClawCR to inspect code, explain behavior, or make changes.",
                Style::new().dark_gray(),
            )]),
        ]);
    }

    let mut lines = Vec::new();
    for item in &app.transcript {
        append_transcript_item(&mut lines, item, app.spinner_index);
        lines.push(Line::from(""));
    }
    Text::from(lines)
}

fn transcript_line_count(app: &TuiApp, inner_width: u16) -> u16 {
    if app.transcript.is_empty() {
        return 2;
    }

    app.transcript
        .iter()
        .map(|item| transcript_item_line_count(item, inner_width) + 1)
        .sum()
}

fn append_transcript_item(
    lines: &mut Vec<Line<'static>>,
    item: &crate::events::TranscriptItem,
    spinner_index: usize,
) {
    match item.kind {
        TranscriptItemKind::User => {
            lines.push(Line::from(vec![
                Span::styled(
                    "> ",
                    Style::new()
                        .fg(item.kind.accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(item.body.clone()),
            ]));
        }
        TranscriptItemKind::Assistant => {
            lines.push(Line::from(vec![
                Span::styled(
                    "• ",
                    Style::new()
                        .fg(item.kind.accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(item.body.clone(), Style::new().fg(item.kind.accent())),
            ]));
        }
        TranscriptItemKind::System if item.title == "Thinking" => {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠴", "⠦"][spinner_index % 6];
            lines.push(Line::from(vec![
                Span::styled("• ", Style::new().yellow().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("{spinner} Thinking"),
                    Style::new().yellow().add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        TranscriptItemKind::ToolCall
        | TranscriptItemKind::ToolResult
        | TranscriptItemKind::System
        | TranscriptItemKind::Error => {
            lines.push(Line::from(vec![
                Span::styled(
                    "• ",
                    Style::new()
                        .fg(item.kind.accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(item.title.clone(), title_style(item.kind)),
            ]));
            append_transcript_body(lines, item);
        }
    }
}

fn append_transcript_body(lines: &mut Vec<Line<'static>>, item: &crate::events::TranscriptItem) {
    if item.body.is_empty() {
        return;
    }

    let mut body_lines = item.body.lines();
    if let Some(first) = body_lines.next() {
        lines.push(Line::from(styled_body_line(
            format!("  └ {first}"),
            item.kind,
        )));
    }
    for line in body_lines {
        lines.push(Line::from(styled_body_line(
            format!("    {line}"),
            item.kind,
        )));
    }
}

fn styled_body_line(text: String, kind: TranscriptItemKind) -> Vec<Span<'static>> {
    match kind {
        TranscriptItemKind::Error => vec![Span::styled(text, Style::new().fg(kind.accent()))],
        TranscriptItemKind::ToolCall | TranscriptItemKind::ToolResult => {
            vec![Span::styled(text, Style::new().dark_gray())]
        }
        _ => vec![Span::raw(text)],
    }
}

fn title_style(kind: TranscriptItemKind) -> Style {
    match kind {
        TranscriptItemKind::ToolCall | TranscriptItemKind::ToolResult => {
            Style::new().dark_gray().add_modifier(Modifier::BOLD)
        }
        _ => Style::new().fg(kind.accent()).add_modifier(Modifier::BOLD),
    }
}

fn wrapped_line_count(text: &str, inner_width: u16) -> u16 {
    if text.is_empty() {
        return 1;
    }

    let width = usize::from(inner_width.max(1));
    text.lines()
        .map(|line| {
            let length = line.chars().count().max(1);
            length.div_ceil(width) as u16
        })
        .sum()
}

fn transcript_item_line_count(item: &crate::events::TranscriptItem, inner_width: u16) -> u16 {
    match item.kind {
        TranscriptItemKind::User | TranscriptItemKind::Assistant => {
            wrapped_line_count(&item.body, inner_width.saturating_sub(2).max(1))
        }
        TranscriptItemKind::ToolCall
        | TranscriptItemKind::ToolResult
        | TranscriptItemKind::System
        | TranscriptItemKind::Error => 1 + body_line_count(item, inner_width),
    }
}

fn body_line_count(item: &crate::events::TranscriptItem, inner_width: u16) -> u16 {
    if item.body.is_empty() {
        return 0;
    }

    let continuation_width = inner_width.saturating_sub(4).max(1);
    let first_width = inner_width.saturating_sub(4).max(1);
    let mut body_lines = item.body.lines();
    let Some(first_line) = body_lines.next() else {
        return 0;
    };

    wrapped_line_count(first_line, first_width)
        + body_lines
            .map(|line| wrapped_line_count(line, continuation_width))
            .sum::<u16>()
}

fn composer_height(app: &TuiApp, area: Rect) -> u16 {
    let inner_width = area.width.saturating_sub(2).max(1);
    let suggestion_height = app.slash_suggestions().len() as u16;
    let suggestion_padding = if suggestion_height > 0 { 1 } else { 0 };
    app.input
        .visual_line_count(inner_width)
        .saturating_add(suggestion_height)
        .saturating_add(suggestion_padding)
        .clamp(1, 8)
}

fn composer_cursor(app: &TuiApp, area: Rect) -> (u16, u16) {
    let inner_width = area.width.saturating_sub(2).max(1);
    let (cursor_x, cursor_y) = app.input.visual_cursor(inner_width);
    (
        area.x + 2 + cursor_x.min(inner_width.saturating_sub(1)),
        area.y + cursor_y.min(area.height.saturating_sub(1)),
    )
}
