use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use crate::render::renderable::Renderable;
use crate::theme::Theme;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;

pub(crate) struct ThemePickerView {
    entries: Vec<String>,
    current_name: String,
    selection: usize,
    complete: bool,
    selected_name: Option<String>,
}

impl ThemePickerView {
    pub(crate) fn new(themes: &[Theme], current_name: String) -> Self {
        let entries: Vec<String> = themes.iter().map(|t| t.name.clone()).collect();
        let selection = entries
            .iter()
            .position(|name| *name == current_name)
            .unwrap_or(0);
        Self {
            entries,
            current_name,
            selection,
            complete: false,
            selected_name: None,
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
        self.selected_name = self.entries.get(self.selection).cloned();
        self.complete = true;
    }

    fn render_lines(&self) -> Vec<Line<'static>> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let mut line: Line<'static> = if name == &self.current_name {
                    format!("  {name}  current").into()
                } else {
                    format!("  {name}").into()
                };
                if index == self.selection {
                    line = line.bold();
                }
                line
            })
            .collect()
    }
}

impl BottomPaneView for ThemePickerView {
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

    fn take_theme_selection(&mut self) -> Option<String> {
        self.selected_name.take()
    }
}

impl Renderable for ThemePickerView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.render_lines()).render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::try_from(self.render_lines().len()).unwrap_or(u16::MAX)
    }
}
