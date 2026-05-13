//! Pager-style overlays rendered in the terminal alternate screen.
//!
//! These overlays are intentionally small: they own full-screen scrolling UI,
//! while the host owns when alternate screen mode is entered and left.

use std::io::Result;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;

use crate::tui;
use crate::tui::TuiEvent;

pub(crate) enum Overlay {
    Transcript(TranscriptOverlay),
    Static(StaticOverlay),
}

impl std::fmt::Debug for Overlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transcript(_) => f.write_str("Overlay::Transcript"),
            Self::Static(_) => f.write_str("Overlay::Static"),
        }
    }
}

impl Overlay {
    pub(crate) fn new_transcript(lines: Vec<Line<'static>>) -> Self {
        Self::Transcript(TranscriptOverlay::new(lines))
    }

    pub(crate) fn new_static_with_lines(lines: Vec<Line<'static>>, title: String) -> Self {
        Self::Static(StaticOverlay::new(lines, title))
    }

    pub(crate) fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match self {
            Overlay::Transcript(overlay) => overlay.handle_event(tui, event),
            Overlay::Static(overlay) => overlay.handle_event(tui, event),
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        match self {
            Overlay::Transcript(overlay) => overlay.is_done(),
            Overlay::Static(overlay) => overlay.is_done(),
        }
    }

    pub(crate) fn set_transcript_lines(&mut self, lines: Vec<Line<'static>>) {
        if let Overlay::Transcript(overlay) = self {
            overlay.set_lines(lines);
        }
    }
}

#[derive(Debug)]
struct PagerView {
    lines: Vec<Line<'static>>,
    scroll_offset: usize,
    title: String,
    last_content_height: Option<usize>,
    last_rendered_height: Option<usize>,
}

impl PagerView {
    fn new(lines: Vec<Line<'static>>, title: String, scroll_offset: usize) -> Self {
        Self {
            lines,
            scroll_offset,
            title,
            last_content_height: None,
            last_rendered_height: None,
        }
    }

    fn set_lines(&mut self, lines: Vec<Line<'static>>) {
        let follow_bottom = self.is_scrolled_to_bottom();
        self.lines = lines;
        if follow_bottom {
            self.scroll_offset = usize::MAX;
        }
    }

    fn content_height(&self, width: u16) -> usize {
        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .line_count(width.max(1))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        if area.is_empty() {
            return;
        }

        self.render_header(area, buf);
        let content_area = self.content_area(area);
        self.last_content_height = Some(content_area.height as usize);
        let content_height = self.content_height(content_area.width.max(1));
        self.last_rendered_height = Some(content_height);

        let max_scroll = content_height.saturating_sub(content_area.height as usize);
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .scroll((u16::try_from(self.scroll_offset).unwrap_or(u16::MAX), 0))
            .render(content_area, buf);

        self.render_bottom_bar(area, content_area, buf, content_height);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let header = format!("/ {}", self.title);
        Span::from("/ ".repeat(area.width as usize / 2))
            .dim()
            .render_ref(Rect::new(area.x, area.y, area.width, 1), buf);
        Span::from(header)
            .dim()
            .render_ref(Rect::new(area.x, area.y, area.width, 1), buf);
    }

    fn render_bottom_bar(
        &self,
        full_area: Rect,
        content_area: Rect,
        buf: &mut Buffer,
        total_len: usize,
    ) {
        if full_area.height == 0 {
            return;
        }

        let y = content_area
            .bottom()
            .min(full_area.bottom().saturating_sub(1));
        let rect = Rect::new(full_area.x, y, full_area.width, 1);
        Span::from("-".repeat(rect.width as usize))
            .dim()
            .render_ref(rect, buf);

        let hints = " Q/Ctrl+C/ESC close  Up/Down scroll  PgUp/PgDn page ";
        Span::from(hints)
            .dim()
            .render_ref(Rect::new(rect.x, rect.y, rect.width, 1), buf);

        let percent = self.scroll_percent(total_len, content_area.height as usize);
        let pct_text = format!(" {percent}% ");
        let pct_w = pct_text.chars().count() as u16;
        if rect.width > pct_w {
            let pct_x = rect.x + rect.width.saturating_sub(pct_w);
            Span::from(pct_text)
                .dim()
                .render_ref(Rect::new(pct_x, rect.y, pct_w, 1), buf);
        }
    }

    fn scroll_percent(&self, total_len: usize, visible_len: usize) -> u8 {
        let max_scroll = total_len.saturating_sub(visible_len);
        if max_scroll == 0 {
            100
        } else {
            (((self.scroll_offset.min(max_scroll)) as f32 / max_scroll as f32) * 100.0).round()
                as u8
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent, viewport_area: Rect) -> bool {
        if !is_press_or_repeat(key_event) {
            return false;
        }

        match key_event.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            KeyCode::PageUp | KeyCode::Char('b')
                if key_event.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_sub(self.page_height(viewport_area));
            }
            KeyCode::PageDown | KeyCode::Char('f')
                if key_event.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_add(self.page_height(viewport_area));
            }
            KeyCode::Char(' ')
                if key_event.modifiers.is_empty() || key_event.modifiers == KeyModifiers::NONE =>
            {
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_add(self.page_height(viewport_area));
            }
            KeyCode::Char(' ') if key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_offset = self
                    .scroll_offset
                    .saturating_sub(self.page_height(viewport_area));
            }
            KeyCode::Char('d') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                let half_page = self.page_height(viewport_area).saturating_add(1) / 2;
                self.scroll_offset = self.scroll_offset.saturating_add(half_page);
            }
            KeyCode::Char('u') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                let half_page = self.page_height(viewport_area).saturating_add(1) / 2;
                self.scroll_offset = self.scroll_offset.saturating_sub(half_page);
            }
            KeyCode::Home => {
                self.scroll_offset = 0;
            }
            KeyCode::End => {
                self.scroll_offset = usize::MAX;
            }
            _ => return false,
        }
        true
    }

    fn page_height(&self, viewport_area: Rect) -> usize {
        self.last_content_height
            .unwrap_or_else(|| self.content_area(viewport_area).height as usize)
            .max(1)
    }

    fn content_area(&self, area: Rect) -> Rect {
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(2),
        )
    }

    fn is_scrolled_to_bottom(&self) -> bool {
        if self.scroll_offset == usize::MAX {
            return true;
        }
        let Some(visible_height) = self.last_content_height else {
            return false;
        };
        let Some(total_height) = self.last_rendered_height else {
            return false;
        };
        if total_height <= visible_height {
            return true;
        }
        self.scroll_offset >= total_height.saturating_sub(visible_height)
    }
}

#[derive(Debug)]
pub(crate) struct TranscriptOverlay {
    view: PagerView,
    is_done: bool,
}

impl TranscriptOverlay {
    fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            view: PagerView::new(lines, "T R A N S C R I P T".to_string(), usize::MAX),
            is_done: false,
        }
    }

    fn set_lines(&mut self, lines: Vec<Line<'static>>) {
        self.view.set_lines(lines);
    }

    fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => {
                if close_key(key_event) || ctrl_t_key(key_event) {
                    self.is_done = true;
                } else if self
                    .view
                    .handle_key_event(key_event, tui.terminal.viewport_area)
                {
                    tui.frame_requester()
                        .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
                }
            }
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| {
                    self.view.render(frame.area(), frame.buffer_mut());
                })?;
            }
            TuiEvent::Paste(_) => {}
        }
        Ok(())
    }

    fn is_done(&self) -> bool {
        self.is_done
    }
}

#[derive(Debug)]
pub(crate) struct StaticOverlay {
    view: PagerView,
    is_done: bool,
}

impl StaticOverlay {
    fn new(lines: Vec<Line<'static>>, title: String) -> Self {
        Self {
            view: PagerView::new(lines, title, 0),
            is_done: false,
        }
    }

    fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => {
                if close_key(key_event) || ctrl_t_key(key_event) {
                    self.is_done = true;
                } else if self
                    .view
                    .handle_key_event(key_event, tui.terminal.viewport_area)
                {
                    tui.frame_requester()
                        .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
                }
            }
            TuiEvent::Draw => {
                tui.draw(u16::MAX, |frame| {
                    self.view.render(frame.area(), frame.buffer_mut());
                })?;
            }
            TuiEvent::Paste(_) => {}
        }
        Ok(())
    }

    fn is_done(&self) -> bool {
        self.is_done
    }
}

fn is_press_or_repeat(key_event: KeyEvent) -> bool {
    matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

fn close_key(key_event: KeyEvent) -> bool {
    is_press_or_repeat(key_event)
        && (matches!(key_event.code, KeyCode::Char('q') | KeyCode::Esc)
            || (key_event.code == KeyCode::Char('c')
                && key_event.modifiers.contains(KeyModifiers::CONTROL)))
}

fn ctrl_t_key(key_event: KeyEvent) -> bool {
    is_press_or_repeat(key_event)
        && key_event.code == KeyCode::Char('t')
        && key_event.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn buffer_to_text(buf: &Buffer, area: Rect) -> String {
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn static_overlay_renders_title_content_and_percent() {
        let mut overlay = StaticOverlay::new(
            vec![Line::from("diff --git a/file b/file"), Line::from("+added")],
            "D I F F".to_string(),
        );
        let area = Rect::new(0, 0, 60, 8);
        let mut buf = Buffer::empty(area);

        overlay.view.render(area, &mut buf);

        let rendered = buffer_to_text(&buf, area);
        assert!(rendered.contains("D I F F"));
        assert!(rendered.contains("diff --git"));
        assert!(rendered.contains("100%"));
    }

    #[test]
    fn pager_scrolls_down_and_back_up() {
        let mut view = PagerView::new(
            (0..20)
                .map(|index| Line::from(format!("line {index}")))
                .collect(),
            "T E S T".to_string(),
            0,
        );
        let area = Rect::new(0, 0, 30, 6);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        assert_eq!(0, view.scroll_offset);
        assert!(view.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), area));
        view.render(area, &mut buf);
        assert_eq!(1, view.scroll_offset);

        assert!(view.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), area));
        view.render(area, &mut buf);
        assert_eq!(0, view.scroll_offset);
    }

    #[test]
    fn transcript_overlay_closes_with_ctrl_t() {
        let mut overlay = TranscriptOverlay::new(vec![Line::from("hello")]);

        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(ctrl_t_key(key));
        overlay.is_done = ctrl_t_key(key);

        assert!(overlay.is_done());
    }
}
