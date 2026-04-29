use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::key_hint;
use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;

const PREVIEW_LINE_LIMIT: usize = 5;

/// Renders queued / pending messages that haven't been sent to the model yet.
///
/// Inspired by OpenCode's inline `QUEUED` badge. Three sections:
/// - **pending** — submitted while turn was active, delivered after next tool call
/// - **rejected** — blocked by hooks, resubmitted at turn end
/// - **queued** — follow-up messages waiting for the next turn
pub(crate) struct PendingInputPreview {
    pub pending_steers: Vec<String>,
    pub rejected_steers: Vec<String>,
    pub queued_messages: Vec<String>,
    edit_binding: key_hint::KeyBinding,
}

impl PendingInputPreview {
    pub(crate) fn new() -> Self {
        Self {
            pending_steers: Vec::new(),
            rejected_steers: Vec::new(),
            queued_messages: Vec::new(),
            edit_binding: key_hint::alt(KeyCode::Up),
        }
    }

    pub(crate) fn set_edit_binding(&mut self, binding: key_hint::KeyBinding) {
        self.edit_binding = binding;
    }

    fn push_truncated(lines: &mut Vec<Line<'static>>, wrapped: Vec<Line<'static>>) {
        let len = wrapped.len();
        lines.extend(wrapped.into_iter().take(PREVIEW_LINE_LIMIT));
        if len > PREVIEW_LINE_LIMIT {
            lines.push(Line::from("    …".dark_gray()));
        }
    }

    fn build_lines(&self, width: u16) -> Vec<Line<'static>> {
        let has_any = !self.pending_steers.is_empty()
            || !self.rejected_steers.is_empty()
            || !self.queued_messages.is_empty();
        if !has_any || width < 4 {
            return vec![];
        }

        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(
            "  ─────────────────────────────────".dark_gray(),
        ));

        // ── Pending steers ──
        if !self.pending_steers.is_empty() {
            lines.push(Line::from(vec![
                "  ◆ ".into(),
                "PENDING".yellow().bold(),
                format!("  {}", self.pending_steers.len()).dark_gray(),
            ]));
            lines.push(Line::from(
                "   Will be sent after the current tool call".dark_gray(),
            ));
            for steer in &self.pending_steers {
                let wrapped = adaptive_wrap_lines(
                    steer.lines().map(|line| Line::from(line.yellow())),
                    RtOptions::new(width as usize)
                        .initial_indent(Line::from(String::from("    ↳ ")))
                        .subsequent_indent(Line::from("      ")),
                );
                Self::push_truncated(&mut lines, wrapped);
            }
            lines.push(
                Line::from(vec![
                    "    ".into(),
                    "[".dark_gray(),
                    key_hint::plain(KeyCode::Esc).into(),
                    " interrupt and send now]".dark_gray(),
                ])
                .dark_gray(),
            );
        }

        // ── Rejected steers ──
        if !self.rejected_steers.is_empty() {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(vec![
                "  ◆ ".dark_gray(),
                "BLOCKED".red().bold(),
                format!("  {}", self.rejected_steers.len()).dark_gray(),
            ]));
            for steer in &self.rejected_steers {
                let wrapped = adaptive_wrap_lines(
                    steer.lines().map(|line| Line::from(line.dark_gray())),
                    RtOptions::new(width as usize)
                        .initial_indent(Line::from("    ↳ ".dark_gray()))
                        .subsequent_indent(Line::from("      ")),
                );
                Self::push_truncated(&mut lines, wrapped);
            }
            lines.push(Line::from(
                "   Resubmitted when the current turn ends".dark_gray(),
            ));
        }

        // ── Queued messages ──
        if !self.queued_messages.is_empty() {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(vec![
                "  ◆ ".dark_gray(),
                "QUEUED".cyan().bold(),
                format!("  {}", self.queued_messages.len()).dark_gray(),
            ]));
            for msg in &self.queued_messages {
                let wrapped = adaptive_wrap_lines(
                    msg.lines()
                        .map(|line| Line::from(line.italic().dark_gray())),
                    RtOptions::new(width as usize)
                        .initial_indent(Line::from("    ↳ ".dark_gray().italic()))
                        .subsequent_indent(Line::from("      ")),
                );
                Self::push_truncated(&mut lines, wrapped);
            }
            lines.push(
                Line::from(vec![
                    "    ".into(),
                    self.edit_binding.into(),
                    " edit last".dark_gray(),
                ])
                .dark_gray(),
            );
        }

        lines
    }
}

impl Renderable for PendingInputPreview {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let lines = self.build_lines(area.width);
        if lines.is_empty() {
            return;
        }

        Paragraph::new(lines).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let lines = self.build_lines(width);
        if lines.is_empty() {
            return 0;
        }
        lines.len() as u16
    }
}
