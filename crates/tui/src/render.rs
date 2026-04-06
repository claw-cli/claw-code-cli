use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::{app::TuiApp, events::TranscriptItemKind};

/// Draws the full interactive UI for the current application state.
pub(crate) fn draw(frame: &mut Frame, app: &TuiApp) {
    let composer_height = composer_height(app, frame.area());
    let [header_area, transcript_area, composer_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(composer_height),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(render_header(app), header_area);
    frame.render_widget(render_transcript(app, transcript_area), transcript_area);
    frame.render_widget(render_composer(app), composer_area);
    frame.render_widget(render_footer(app), footer_area);

    let cursor = composer_cursor(app, composer_area);
    frame.set_cursor_position(cursor);
}

fn render_header(app: &TuiApp) -> Paragraph<'static> {
    let spinner = if app.busy {
        ["⠋", "⠙", "⠹", "⠸", "⠴", "⠦"][app.spinner_index % 6]
    } else {
        "●"
    };
    let status_style = if app.busy {
        Style::new().yellow().add_modifier(Modifier::BOLD)
    } else {
        Style::new().green().add_modifier(Modifier::BOLD)
    };
    let cwd_name = app
        .cwd
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| app.cwd.to_string_lossy().into_owned());

    Paragraph::new(Text::from(vec![
        Line::from(vec![
            Span::styled(
                " ClawCR ",
                Style::new().black().on_cyan().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(format!("{spinner} {}", app.status_message), status_style),
        ]),
        Line::from(vec![
            Span::styled("model ", Style::new().dark_gray()),
            Span::styled(
                app.model.clone(),
                Style::new().white().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("provider ", Style::new().dark_gray()),
            Span::raw(app.provider_name.clone()),
            Span::raw("   "),
            Span::styled("cwd ", Style::new().dark_gray()),
            Span::raw(cwd_name),
        ]),
    ]))
    .block(Block::default().borders(Borders::BOTTOM))
}

fn render_transcript(app: &TuiApp, area: Rect) -> Paragraph<'static> {
    let width = area.width.saturating_sub(2).max(1);
    let content = transcript_text(app);
    let max_scroll =
        transcript_line_count(app, width).saturating_sub(area.height.saturating_sub(2));
    let scroll = if app.follow_output {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };

    Paragraph::new(content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Conversation "),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
}

fn render_composer(app: &TuiApp) -> Paragraph<'_> {
    let title = if app.busy {
        " Composer (model is busy) "
    } else {
        " Composer "
    };
    let body = if app.input.text().is_empty() {
        Text::from(vec![Line::from(vec![Span::styled(
            "Type a message. Enter sends, Shift+Enter inserts a newline.",
            Style::new().dark_gray(),
        )])])
    } else {
        Text::from(app.input.text().to_string())
    };

    Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false })
}

fn render_footer(app: &TuiApp) -> Paragraph<'static> {
    let usage = app
        .last_turn_usage
        .map(|(input, output)| format!("last turn {input} in / {output} out"))
        .unwrap_or_else(|| "last turn n/a".to_string());
    let footer = format!(
        "Ctrl+C quit  Ctrl+L clear  Esc clear input  PgUp/PgDn scroll  turns {}  total {} in / {} out  {}",
        app.turn_count, app.total_input_tokens, app.total_output_tokens, usage
    );
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
        lines.push(Line::from(vec![Span::styled(
            item.title.clone(),
            Style::new()
                .fg(item.kind.accent())
                .add_modifier(Modifier::BOLD),
        )]));
        if item.body.is_empty() {
            lines.push(Line::from(""));
        } else {
            for line in item.body.lines() {
                lines.push(Line::from(match item.kind {
                    TranscriptItemKind::Error => vec![Span::styled(
                        line.to_string(),
                        Style::new().fg(TranscriptItemKind::Error.accent()),
                    )],
                    _ => vec![Span::raw(line.to_string())],
                }));
            }
        }
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
        .map(|item| {
            let title_lines = 1;
            let body_lines = wrapped_line_count(&item.body, inner_width);
            title_lines + body_lines + 1
        })
        .sum()
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

fn composer_height(app: &TuiApp, area: Rect) -> u16 {
    let inner_width = area.width.saturating_sub(2).max(1);
    let body_height = app.input.visual_line_count(inner_width).clamp(1, 6);
    body_height + 2
}

fn composer_cursor(app: &TuiApp, area: Rect) -> (u16, u16) {
    let inner_width = area.width.saturating_sub(2).max(1);
    let (cursor_x, cursor_y) = app.input.visual_cursor(inner_width);
    (
        area.x + 1 + cursor_x.min(inner_width.saturating_sub(1)),
        area.y + 1 + cursor_y.min(area.height.saturating_sub(3)),
    )
}
