use ratatui::style::{Color, Modifier, Style};

use crate::{app::TuiApp, events::TranscriptItemKind};

const SLATE_200: Color = Color::Rgb(213, 219, 227);
const SLATE_400: Color = Color::Rgb(138, 148, 163);
const SLATE_500: Color = Color::Rgb(113, 124, 141);
const SLATE_700: Color = Color::Rgb(58, 67, 82);
const CYAN_300: Color = Color::Rgb(112, 211, 228);
const AMBER_300: Color = Color::Rgb(245, 201, 117);
const RED_300: Color = Color::Rgb(244, 137, 137);
const SURFACE: Color = Color::Rgb(24, 28, 34);

pub(super) fn prompt() -> Style {
    Style::new().fg(CYAN_300).add_modifier(Modifier::BOLD)
}

pub(super) fn muted() -> Style {
    Style::new().fg(SLATE_500)
}

pub(super) fn selected() -> Style {
    Style::new().fg(Color::Black).bg(SLATE_200)
}

pub(super) fn panel_title() -> Style {
    muted().add_modifier(Modifier::BOLD)
}

pub(super) fn composer_border(app: &TuiApp) -> Style {
    if app.busy {
        Style::new().fg(AMBER_300).add_modifier(Modifier::BOLD)
    } else if app.onboarding_prompt.is_some() {
        prompt()
    } else {
        Style::new().fg(SLATE_700)
    }
}

pub(super) fn overlay_border() -> Style {
    Style::new().fg(SLATE_400)
}

pub(super) fn menu_surface() -> Style {
    Style::new().bg(SURFACE)
}

pub(super) fn transcript_prefix(kind: TranscriptItemKind) -> Style {
    match kind {
        TranscriptItemKind::User => Style::new().fg(CYAN_300).add_modifier(Modifier::BOLD),
        TranscriptItemKind::Assistant => Style::new().fg(SLATE_400).add_modifier(Modifier::BOLD),
        TranscriptItemKind::Reasoning => Style::new().fg(AMBER_300).add_modifier(Modifier::BOLD),
        TranscriptItemKind::ToolCall | TranscriptItemKind::ToolResult => {
            Style::new().fg(AMBER_300).add_modifier(Modifier::BOLD)
        }
        TranscriptItemKind::Error => Style::new().fg(RED_300).add_modifier(Modifier::BOLD),
        TranscriptItemKind::System => Style::new().fg(SLATE_400).add_modifier(Modifier::BOLD),
    }
}

pub(super) fn transcript_title(kind: TranscriptItemKind) -> Style {
    match kind {
        TranscriptItemKind::ToolCall | TranscriptItemKind::ToolResult => {
            Style::new().fg(SLATE_200).add_modifier(Modifier::BOLD)
        }
        TranscriptItemKind::Reasoning => Style::new().fg(AMBER_300).add_modifier(Modifier::BOLD),
        TranscriptItemKind::Error => Style::new().fg(RED_300).add_modifier(Modifier::BOLD),
        TranscriptItemKind::System => Style::new().fg(SLATE_200).add_modifier(Modifier::BOLD),
        TranscriptItemKind::User => Style::new().fg(CYAN_300).add_modifier(Modifier::BOLD),
        TranscriptItemKind::Assistant => Style::new().fg(SLATE_200),
    }
}

pub(super) fn transcript_body(kind: TranscriptItemKind) -> Style {
    match kind {
        TranscriptItemKind::User => Style::new().fg(SLATE_200),
        TranscriptItemKind::Assistant => Style::new().fg(SLATE_200),
        TranscriptItemKind::Reasoning => Style::new().fg(SLATE_200),
        TranscriptItemKind::ToolCall => Style::new().fg(SLATE_400),
        TranscriptItemKind::ToolResult => Style::new().fg(SLATE_400),
        TranscriptItemKind::Error => Style::new().fg(RED_300),
        TranscriptItemKind::System => Style::new().fg(SLATE_400),
    }
}
