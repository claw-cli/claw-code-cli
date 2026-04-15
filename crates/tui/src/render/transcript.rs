use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};
use textwrap::Options;

use crate::{
    app::TuiApp,
    events::{TranscriptItem, TranscriptItemKind},
};

use super::theme;

pub(super) fn render(app: &TuiApp, area_width: u16, area_height: u16) -> Paragraph<'static> {
    let content = transcript_text(app, area_width.max(1));
    let max_scroll = content.lines.len().saturating_sub(area_height as usize) as u16;
    let scroll = if app.follow_output {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };

    Paragraph::new(content).scroll((scroll, 0))
}

pub(super) fn line_count(app: &TuiApp, inner_width: u16) -> u16 {
    transcript_text(app, inner_width.max(1)).lines.len() as u16
}

fn transcript_text(app: &TuiApp, inner_width: u16) -> Text<'static> {
    let mut lines = Vec::new();

    if app.transcript.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            "No conversation yet. Ask ClawCR to inspect code, explain behavior, or make changes.",
            theme::muted(),
        )]));
        return Text::from(lines);
    }

    let mut previous_kind = None;
    for item in &app.transcript {
        if previous_kind.is_some()
            && (matches!(item.kind, TranscriptItemKind::User)
                || matches!(previous_kind, Some(TranscriptItemKind::User))
                || matches!(item.kind, TranscriptItemKind::ToolCall)
                || matches!(item.kind, TranscriptItemKind::Error)
                || matches!(item.kind, TranscriptItemKind::System))
        {
            lines.push(Line::from(""));
        }
        append_transcript_item(&mut lines, item, app.spinner_index, inner_width);
        previous_kind = Some(item.kind);
    }
    Text::from(lines)
}

fn append_transcript_item(
    lines: &mut Vec<Line<'static>>,
    item: &TranscriptItem,
    spinner_index: usize,
    inner_width: u16,
) {
    match item.kind {
        TranscriptItemKind::User => {
            append_plain_message(lines, item, "> ", "  ", inner_width);
        }
        TranscriptItemKind::Assistant => {
            append_plain_message(lines, item, "• ", "  ", inner_width);
        }
        TranscriptItemKind::Reasoning => {
            append_wrapped_title(lines, &item.title, item.kind, inner_width);
            append_transcript_body(lines, item, inner_width);
        }
        TranscriptItemKind::System if item.title == "Thinking" => {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠴", "⠦"][spinner_index % 6];
            append_wrapped_styled_text(
                lines,
                &format!("{spinner} Thinking"),
                "• ",
                "  ",
                inner_width,
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            );
        }
        TranscriptItemKind::System if item.title == "Interrupted" => {
            append_wrapped_styled_text(
                lines,
                "Interrupted",
                "• ",
                "  ",
                inner_width,
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            );
        }
        TranscriptItemKind::ToolCall
        | TranscriptItemKind::ToolResult
        | TranscriptItemKind::System
        | TranscriptItemKind::Error => {
            let title_kind = if item.kind == TranscriptItemKind::ToolResult {
                TranscriptItemKind::ToolCall
            } else {
                item.kind
            };
            append_wrapped_title(lines, &item.title, title_kind, inner_width);
            if item.kind != TranscriptItemKind::ToolCall {
                append_transcript_body(lines, item, inner_width);
            }
        }
    }
}

fn append_plain_message(
    lines: &mut Vec<Line<'static>>,
    item: &TranscriptItem,
    first_prefix: &'static str,
    continuation_prefix: &'static str,
    inner_width: u16,
) {
    append_wrapped_styled_text(
        lines,
        item.body.trim_end_matches('\n'),
        first_prefix,
        continuation_prefix,
        inner_width,
        theme::transcript_body(item.kind),
    );
}

fn append_transcript_body(lines: &mut Vec<Line<'static>>, item: &TranscriptItem, inner_width: u16) {
    let body = rendered_transcript_body(item);
    if body.is_empty() {
        return;
    }

    append_wrapped_styled_text(
        lines,
        &body,
        "  └ ",
        "    ",
        inner_width,
        theme::transcript_body(item.kind),
    );
}

fn rendered_transcript_body(item: &TranscriptItem) -> String {
    match item.kind {
        TranscriptItemKind::ToolResult => match item.fold_stage {
            0 => item.body.trim_end_matches('\n').to_string(),
            1 => fold_tool_output(&item.body, 4),
            2 => fold_tool_output(&item.body, 1),
            _ => String::new(),
        },
        _ => item.body.trim_end_matches('\n').to_string(),
    }
}

fn fold_tool_output(body: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = body.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    if lines.len() <= max_lines {
        return body.trim_end_matches('\n').to_string();
    }

    let mut folded = lines[..max_lines].join("\n");
    folded.push_str("\n...");
    folded
}

fn append_wrapped_title(
    lines: &mut Vec<Line<'static>>,
    title: &str,
    kind: TranscriptItemKind,
    inner_width: u16,
) {
    let prefix = "• ";
    let continuation = "  ";
    let content_width = inner_width.saturating_sub(prefix.len() as u16).max(1) as usize;
    let wrapped = textwrap::wrap(title, Options::new(content_width).break_words(false));
    for (index, segment) in wrapped.iter().enumerate() {
        let prefix_text = if index == 0 { prefix } else { continuation };
        lines.push(Line::from(vec![
            Span::styled(prefix_text, theme::transcript_prefix(kind)),
            Span::styled(segment.to_string(), theme::transcript_title(kind)),
        ]));
    }
}

fn append_wrapped_styled_text(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    first_prefix: &'static str,
    continuation_prefix: &'static str,
    inner_width: u16,
    style: Style,
) {
    let prefix_kind = match first_prefix {
        "> " => TranscriptItemKind::User,
        "• " => TranscriptItemKind::Assistant,
        "  └ " => TranscriptItemKind::ToolResult,
        _ => TranscriptItemKind::System,
    };
    let prefix_style = theme::transcript_prefix(prefix_kind);
    if text.is_empty() {
        lines.push(Line::from(vec![Span::styled(first_prefix, prefix_style)]));
        return;
    }

    let first_width = inner_width.saturating_sub(first_prefix.len() as u16).max(1) as usize;
    let continuation_width = inner_width
        .saturating_sub(continuation_prefix.len() as u16)
        .max(1) as usize;
    let mut first_visual_line = true;

    for logical_line in text.split('\n') {
        let options = if first_visual_line {
            Options::new(first_width).break_words(false)
        } else {
            Options::new(continuation_width).break_words(false)
        };
        let wrapped = textwrap::wrap(logical_line, options);
        if wrapped.is_empty() {
            let prefix = if first_visual_line {
                first_prefix
            } else {
                continuation_prefix
            };
            lines.push(Line::from(vec![Span::styled(prefix, prefix_style)]));
            first_visual_line = false;
            continue;
        }

        for segment in wrapped {
            let prefix = if first_visual_line {
                first_prefix
            } else {
                continuation_prefix
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::styled(segment.to_string(), style),
            ]));
            first_visual_line = false;
        }
    }
}
