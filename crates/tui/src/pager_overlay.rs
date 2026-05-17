//! Pager-style overlays rendered in the terminal alternate screen.
//!
//! These overlays own the full-screen scrolling UI, while the host owns when
//! alternate screen mode is entered and left.

use std::cell::Cell;
use std::fmt;
use std::io::Result;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::buffer::Cell as BufferCell;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;

use crate::chatwidget::ActiveCellTranscriptKey;
use crate::chatwidget::TranscriptOverlayCell;
use crate::render::Insets;
use crate::render::renderable::InsetRenderable;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use crate::tui;
use crate::tui::TuiEvent;

pub(crate) enum Overlay {
    Transcript(Box<TranscriptOverlay>),
    Static(Box<StaticOverlay>),
}

struct UserTranscriptCellRenderable {
    lines: Vec<Line<'static>>,
    style: Style,
}

impl Renderable for UserTranscriptCellRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buf[(x, y)].set_style(self.style);
            }
        }
        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        Paragraph::new(Text::from(self.lines.clone()))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }
}

impl fmt::Debug for Overlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transcript(_) => f.write_str("Overlay::Transcript"),
            Self::Static(_) => f.write_str("Overlay::Static"),
        }
    }
}

impl Overlay {
    pub(crate) fn new_transcript(cells: Vec<TranscriptOverlayCell>, width: u16) -> Self {
        Self::Transcript(Box::new(TranscriptOverlay::new(cells, width)))
    }

    pub(crate) fn new_static_with_lines(lines: Vec<Line<'static>>, title: String) -> Self {
        Self::Static(Box::new(StaticOverlay::new(lines, title)))
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
}

struct PagerView {
    renderables: Vec<Box<dyn Renderable>>,
    scroll_offset: usize,
    title: String,
    last_content_height: Option<usize>,
    last_rendered_height: Option<usize>,
}

impl fmt::Debug for PagerView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PagerView")
            .field("renderables", &self.renderables.len())
            .field("scroll_offset", &self.scroll_offset)
            .field("title", &self.title)
            .field("last_content_height", &self.last_content_height)
            .field("last_rendered_height", &self.last_rendered_height)
            .finish()
    }
}

impl PagerView {
    fn new(renderables: Vec<Box<dyn Renderable>>, title: String, scroll_offset: usize) -> Self {
        Self {
            renderables,
            scroll_offset,
            title,
            last_content_height: None,
            last_rendered_height: None,
        }
    }

    fn content_height(&self, width: u16) -> usize {
        self.renderables
            .iter()
            .map(|renderable| renderable.desired_height(width.max(1)) as usize)
            .sum()
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        Clear.render(area, buf);
        if area.is_empty() {
            return;
        }

        self.render_header(area, buf);
        let content_area = self.content_area(area);
        self.last_content_height = Some(content_area.height as usize);
        let content_height = self.content_height(content_area.width);
        self.last_rendered_height = Some(content_height);

        let max_scroll = content_height.saturating_sub(content_area.height as usize);
        self.scroll_offset = self.scroll_offset.min(max_scroll);

        self.render_content(content_area, buf);
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

    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y as isize - self.scroll_offset as isize;
        let content_top = area.y as isize;
        let content_bottom = area.bottom() as isize;
        let mut drawn_bottom = area.y;

        for renderable in &self.renderables {
            let top = y;
            let height = renderable.desired_height(area.width.max(1)) as isize;
            y += height;
            let bottom = y;

            if bottom <= content_top {
                continue;
            }
            if top >= content_bottom {
                break;
            }

            if top < content_top {
                let offset = u16::try_from(content_top.saturating_sub(top)).unwrap_or(u16::MAX);
                let drawn = render_offset_content(area, buf, &**renderable, offset);
                drawn_bottom = drawn_bottom.max(area.y.saturating_add(drawn));
            } else {
                let draw_y = u16::try_from(top).unwrap_or(u16::MAX);
                let draw_bottom = bottom.min(content_bottom);
                let draw_height = u16::try_from(draw_bottom.saturating_sub(top)).unwrap_or(0);
                let draw_area = Rect::new(area.x, draw_y, area.width, draw_height);
                renderable.render(draw_area, buf);
                drawn_bottom = drawn_bottom.max(draw_area.y.saturating_add(draw_area.height));
            }
        }

        for row in drawn_bottom..area.bottom() {
            if area.width == 0 {
                break;
            }
            buf[(area.x, row)] = BufferCell::from('~');
            for col in area.x.saturating_add(1)..area.right() {
                buf[(col, row)] = BufferCell::from(' ');
            }
        }
    }

    fn render_bottom_bar(
        &self,
        full_area: Rect,
        content_area: Rect,
        buf: &mut Buffer,
        total_len: usize,
    ) {
        if full_area.height < 2 {
            return;
        }
        let first_y = content_area
            .bottom()
            .min(full_area.bottom().saturating_sub(2));
        let first = Rect::new(full_area.x, first_y, full_area.width, 1);
        let second = Rect::new(full_area.x, first_y.saturating_add(1), full_area.width, 1);
        Span::from("-".repeat(first.width as usize))
            .dim()
            .render_ref(first, buf);
        Span::from("-".repeat(second.width as usize))
            .dim()
            .render_ref(second, buf);

        let hints1 = " ↑/↓ to scroll   pgup/pgdn to page   home/end to jump ";
        let hints2 = " q to quit   esc/← to edit prev   → to edit next   enter to edit message ";
        Span::from(hints1)
            .dim()
            .render_ref(Rect::new(first.x, first.y, first.width, 1), buf);
        Span::from(hints2)
            .dim()
            .render_ref(Rect::new(second.x, second.y, second.width, 1), buf);

        let percent = self.scroll_percent(total_len, content_area.height as usize);
        let pct_text = format!(" {percent}% ");
        let pct_w = pct_text.chars().count() as u16;
        if first.width > pct_w {
            let pct_x = first.x + first.width.saturating_sub(pct_w);
            Span::from(pct_text)
                .dim()
                .render_ref(Rect::new(pct_x, first.y, pct_w, 1), buf);
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

    fn scroll_chunk_into_view(&mut self, renderable_index: usize) {
        let width = self
            .last_content_height
            .map(|_| self.content_area(Rect::new(0, 0, 80, 24)).width)
            .unwrap_or(80);
        let width = width.max(1);
        let mut start = 0usize;
        for (idx, renderable) in self.renderables.iter().enumerate() {
            let height = renderable.desired_height(width) as usize;
            let end = start.saturating_add(height);
            if idx == renderable_index {
                if self.scroll_offset > start {
                    self.scroll_offset = start;
                } else if let Some(visible_height) = self.last_content_height
                    && end > self.scroll_offset.saturating_add(visible_height)
                {
                    self.scroll_offset = end.saturating_sub(visible_height);
                }
                return;
            }
            start = end;
        }
    }

    fn content_area(&self, area: Rect) -> Rect {
        Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(3),
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

struct CachedRenderable {
    renderable: Box<dyn Renderable>,
    height: Cell<Option<u16>>,
    last_width: Cell<Option<u16>>,
}

impl CachedRenderable {
    fn new(renderable: impl Into<Box<dyn Renderable>>) -> Self {
        Self {
            renderable: renderable.into(),
            height: Cell::new(None),
            last_width: Cell::new(None),
        }
    }
}

impl Renderable for CachedRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.renderable.render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let width = width.max(1);
        if self.last_width.get() != Some(width) {
            self.height.set(Some(self.renderable.desired_height(width)));
            self.last_width.set(Some(width));
        }
        self.height.get().unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommittedCellsKey {
    width: u16,
    cell_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LiveTailKey {
    width: u16,
    revision: u64,
    is_stream_continuation: bool,
    animation_tick: Option<u64>,
}

pub(crate) struct TranscriptOverlay {
    view: PagerView,
    cells: Vec<TranscriptOverlayCell>,
    committed_key: CommittedCellsKey,
    live_tail: Option<TranscriptOverlayCell>,
    live_tail_key: Option<LiveTailKey>,
    selected_user_index: Option<usize>,
    is_done: bool,
}

impl fmt::Debug for TranscriptOverlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscriptOverlay")
            .field("view", &self.view)
            .field("cells", &self.cells.len())
            .field("committed_key", &self.committed_key)
            .field("live_tail", &self.live_tail.is_some())
            .field("live_tail_key", &self.live_tail_key)
            .field("is_done", &self.is_done)
            .finish()
    }
}

impl TranscriptOverlay {
    fn new(cells: Vec<TranscriptOverlayCell>, width: u16) -> Self {
        let committed_key = CommittedCellsKey {
            width: width.max(1),
            cell_count: cells.len(),
        };
        Self {
            view: PagerView::new(
                Self::render_cells(&cells, None),
                "T R A N S C R I P T".to_string(),
                usize::MAX,
            ),
            cells,
            committed_key,
            live_tail: None,
            live_tail_key: None,
            selected_user_index: None,
            is_done: false,
        }
    }

    pub(crate) fn needs_committed_cells_sync(&self, width: u16, cell_count: usize) -> bool {
        self.committed_key
            != (CommittedCellsKey {
                width: width.max(1),
                cell_count,
            })
    }

    pub(crate) fn replace_committed_cells(
        &mut self,
        width: u16,
        cells: Vec<TranscriptOverlayCell>,
    ) {
        let next_key = CommittedCellsKey {
            width: width.max(1),
            cell_count: cells.len(),
        };
        if self.committed_key == next_key {
            return;
        }

        let follow_bottom = self.view.is_scrolled_to_bottom();
        self.cells = cells;
        self.committed_key = next_key;
        self.rebuild_renderables();
        if follow_bottom {
            self.view.scroll_offset = usize::MAX;
        }
    }

    pub(crate) fn sync_live_tail(
        &mut self,
        width: u16,
        active_key: Option<ActiveCellTranscriptKey>,
        compute_lines: impl FnOnce(u16) -> Option<Vec<Line<'static>>>,
    ) -> bool {
        let next_key = active_key.map(|key| LiveTailKey {
            width: width.max(1),
            revision: key.revision,
            is_stream_continuation: key.is_stream_continuation,
            animation_tick: key.animation_tick,
        });

        if self.live_tail_key == next_key {
            return false;
        }

        let follow_bottom = self.view.is_scrolled_to_bottom();
        self.live_tail_key = next_key;
        self.live_tail = next_key.and_then(|key| {
            let lines = compute_lines(key.width).unwrap_or_default();
            (!lines.is_empty()).then_some(TranscriptOverlayCell {
                lines,
                is_stream_continuation: key.is_stream_continuation,
                user_message: None,
                is_selected_user: false,
            })
        });
        self.rebuild_renderables();
        if follow_bottom {
            self.view.scroll_offset = usize::MAX;
        }
        true
    }

    pub(crate) fn is_scrolled_to_bottom(&self) -> bool {
        self.view.is_scrolled_to_bottom()
    }

    pub(crate) fn begin_backtrack_preview(&mut self) {
        self.selected_user_index = self.user_positions().last().copied();
        self.sync_selected_user_highlight();
        if let Some(idx) = self.selected_user_index {
            self.view.scroll_chunk_into_view(idx);
        }
    }

    pub(crate) fn select_prev_user(&mut self) {
        let positions = self.user_positions();
        if positions.is_empty() {
            self.selected_user_index = None;
            self.sync_selected_user_highlight();
            return;
        }
        self.selected_user_index = Some(match self.selected_user_index {
            None => *positions.last().unwrap_or(&positions[0]),
            Some(current) => positions
                .iter()
                .rev()
                .copied()
                .find(|idx| *idx < current)
                .unwrap_or(positions[0]),
        });
        self.sync_selected_user_highlight();
        if let Some(idx) = self.selected_user_index {
            self.view.scroll_chunk_into_view(idx);
        }
    }

    pub(crate) fn select_next_user(&mut self) {
        let positions = self.user_positions();
        if positions.is_empty() {
            self.selected_user_index = None;
            self.sync_selected_user_highlight();
            return;
        }
        self.selected_user_index = Some(match self.selected_user_index {
            None => *positions.last().unwrap_or(&positions[0]),
            Some(current) => positions
                .iter()
                .copied()
                .find(|idx| *idx > current)
                .unwrap_or(*positions.last().unwrap_or(&current)),
        });
        self.sync_selected_user_highlight();
        if let Some(idx) = self.selected_user_index {
            self.view.scroll_chunk_into_view(idx);
        }
    }

    pub(crate) fn selected_user_message(&self) -> Option<crate::chatwidget::UserMessage> {
        self.selected_user_index
            .and_then(|idx| self.cells.get(idx))
            .and_then(|cell| cell.user_message.clone())
    }

    #[cfg(test)]
    pub(crate) fn selected_user_index_for_test(&self) -> Option<usize> {
        self.selected_user_index
    }

    pub(crate) fn selected_user_history_position(&self) -> Option<usize> {
        let positions = self.user_positions();
        self.selected_user_index
            .and_then(|selected| positions.iter().position(|idx| *idx == selected))
    }

    fn user_positions(&self) -> Vec<usize> {
        self.cells
            .iter()
            .enumerate()
            .filter_map(|(idx, cell)| cell.user_message.is_some().then_some(idx))
            .collect()
    }

    fn sync_selected_user_highlight(&mut self) {
        for (idx, cell) in self.cells.iter_mut().enumerate() {
            cell.is_selected_user = cell.user_message.is_some() && self.selected_user_index == Some(idx);
        }
        self.rebuild_renderables();
    }

    fn rebuild_renderables(&mut self) {
        self.view.renderables = Self::render_cells(&self.cells, self.live_tail.as_ref());
    }

    fn render_cells(
        cells: &[TranscriptOverlayCell],
        live_tail: Option<&TranscriptOverlayCell>,
    ) -> Vec<Box<dyn Renderable>> {
        let mut renderables = Vec::new();
        for cell in cells {
            renderables.push(Self::cell_renderable(cell.clone(), !renderables.is_empty()));
        }
        if let Some(tail) = live_tail {
            renderables.push(Self::cell_renderable(tail.clone(), !renderables.is_empty()));
        }
        renderables
    }

    fn cell_renderable(cell: TranscriptOverlayCell, has_prior_cells: bool) -> Box<dyn Renderable> {
        let mut renderable: Box<dyn Renderable> = if cell.user_message.is_some() {
            let mut style = user_message_style();
            if style.bg.is_none() {
                style = style.bg(Color::Rgb(40, 44, 52));
            }
            if cell.is_selected_user {
                style = style
                    .bg(Color::Rgb(52, 70, 110))
                    .fg(Color::Rgb(120, 200, 255));
            }
            Box::new(CachedRenderable::new(UserTranscriptCellRenderable {
                lines: cell.lines,
                style,
            }))
        } else {
            let paragraph = Paragraph::new(Text::from(cell.lines)).wrap(Wrap { trim: false });
            Box::new(CachedRenderable::new(paragraph))
        };
        if has_prior_cells && !cell.is_stream_continuation {
            renderable = Box::new(InsetRenderable::new(
                renderable,
                Insets::tlbr(
                    /*top*/ 1, /*left*/ 0, /*bottom*/ 0, /*right*/ 0,
                ),
            ));
        }
        renderable
    }

    fn handle_event(&mut self, tui: &mut tui::Tui, event: TuiEvent) -> Result<()> {
        match event {
            TuiEvent::Key(key_event) => {
                if ctrl_t_key(key_event) || close_key(key_event) {
                    self.is_done = true;
                } else if is_press_or_repeat(key_event)
                    && matches!(key_event.code, KeyCode::Esc | KeyCode::Left)
                {
                    self.select_prev_user();
                    tui.frame_requester()
                        .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
                } else if is_press_or_repeat(key_event) && key_event.code == KeyCode::Right {
                    self.select_next_user();
                    tui.frame_requester()
                        .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
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

pub(crate) struct StaticOverlay {
    view: PagerView,
    is_done: bool,
}

impl fmt::Debug for StaticOverlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticOverlay")
            .field("view", &self.view)
            .field("is_done", &self.is_done)
            .finish()
    }
}

impl StaticOverlay {
    fn new(lines: Vec<Line<'static>>, title: String) -> Self {
        Self {
            view: PagerView::new(vec![lines_renderable(lines)], title, 0),
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

fn lines_renderable(lines: Vec<Line<'static>>) -> Box<dyn Renderable> {
    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    Box::new(CachedRenderable::new(paragraph))
}

fn render_offset_content(
    area: Rect,
    buf: &mut Buffer,
    renderable: &dyn Renderable,
    scroll_offset: u16,
) -> u16 {
    let height = renderable.desired_height(area.width.max(1));
    let mut tall_buf = Buffer::empty(Rect::new(
        0,
        0,
        area.width,
        height.min(area.height.saturating_add(scroll_offset)),
    ));
    renderable.render(*tall_buf.area(), &mut tall_buf);
    let copy_height = area
        .height
        .min(tall_buf.area().height.saturating_sub(scroll_offset));
    for y in 0..copy_height {
        let src_y = y.saturating_add(scroll_offset);
        for x in 0..area.width {
            buf[(area.x + x, area.y + y)] = tall_buf[(x, src_y)].clone();
        }
    }
    copy_height
}

fn is_press_or_repeat(key_event: KeyEvent) -> bool {
    matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

fn close_key(key_event: KeyEvent) -> bool {
    is_press_or_repeat(key_event)
        && (matches!(key_event.code, KeyCode::Char('q'))
            || (key_event.code == KeyCode::Char('c')
                && key_event.modifiers.contains(KeyModifiers::CONTROL)))
}

fn ctrl_t_key(key_event: KeyEvent) -> bool {
    is_press_or_repeat(key_event)
        && key_event.code == KeyCode::Char('t')
        && key_event.modifiers.contains(KeyModifiers::CONTROL)
}

#[cfg(test)]
fn transcript_hint_lines_for_test() -> (String, String) {
    (
        " ↑/↓ to scroll   pgup/pgdn to page   home/end to jump ".to_string(),
        " q to quit   esc/← to edit prev   → to edit next   enter to edit message ".to_string(),
    )
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

    fn transcript_cell(text: impl Into<String>) -> TranscriptOverlayCell {
        TranscriptOverlayCell {
            lines: vec![Line::from(text.into())],
            is_stream_continuation: false,
            user_message: None,
            is_selected_user: false,
        }
    }

    fn active_key(revision: u64) -> ActiveCellTranscriptKey {
        ActiveCellTranscriptKey {
            revision,
            is_stream_continuation: false,
            animation_tick: None,
        }
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
            vec![lines_renderable(
                (0..20)
                    .map(|index| Line::from(format!("line {index}")))
                    .collect(),
            )],
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
        let mut overlay = TranscriptOverlay::new(vec![transcript_cell("hello")], 80);

        let key = KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(ctrl_t_key(key));
        overlay.is_done = ctrl_t_key(key);

        assert!(overlay.is_done());
    }

    #[test]
    fn unchanged_live_tail_key_does_not_recompute_tail() {
        let mut overlay = TranscriptOverlay::new(Vec::new(), 40);
        let calls = Cell::new(0);
        let key = active_key(7);

        assert!(overlay.sync_live_tail(40, Some(key), |_| {
            calls.set(calls.get() + 1);
            Some(vec![Line::from("tail")])
        }));
        assert!(!overlay.sync_live_tail(40, Some(key), |_| {
            calls.set(calls.get() + 1);
            Some(vec![Line::from("changed tail")])
        }));

        assert_eq!(1, calls.get());
    }

    #[test]
    fn unchanged_live_tail_key_preserves_manual_scroll_position() {
        let cells = (0..30)
            .map(|index| transcript_cell(format!("line {index}")))
            .collect();
        let mut overlay = TranscriptOverlay::new(cells, 30);
        let area = Rect::new(0, 0, 30, 6);
        let mut buf = Buffer::empty(area);
        overlay.view.render(area, &mut buf);
        overlay.view.scroll_offset = 5;

        assert!(overlay.sync_live_tail(30, Some(active_key(1)), |_| {
            Some(vec![Line::from("live tail")])
        }));
        assert_eq!(5, overlay.view.scroll_offset);

        overlay.view.scroll_offset = 4;
        assert!(!overlay.sync_live_tail(30, Some(active_key(1)), |_| {
            Some(vec![Line::from("new live tail")])
        }));

        assert_eq!(4, overlay.view.scroll_offset);
    }

    #[test]
    fn transcript_overlay_backtrack_navigation_selects_user_cells() {
        let mut overlay = TranscriptOverlay::new(
            vec![
                TranscriptOverlayCell {
                    lines: vec![Line::from("assistant 1")],
                    is_stream_continuation: false,
                    user_message: None,
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 1")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 1")),
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("assistant 2")],
                    is_stream_continuation: false,
                    user_message: None,
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 2")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 2")),
                    is_selected_user: false,
                },
            ],
            80,
        );

        overlay.begin_backtrack_preview();
        assert_eq!(
            overlay.selected_user_message().map(|m| m.text),
            Some("user 2".to_string())
        );

        overlay.select_prev_user();
        assert_eq!(
            overlay.selected_user_message().map(|m| m.text),
            Some("user 1".to_string())
        );

        overlay.select_next_user();
        assert_eq!(
            overlay.selected_user_message().map(|m| m.text),
            Some("user 2".to_string())
        );
    }

    #[test]
    fn transcript_overlay_enter_target_is_latest_user_after_begin_preview() {
        let mut overlay = TranscriptOverlay::new(
            vec![
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 1")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 1")),
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 2")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 2")),
                    is_selected_user: false,
                },
            ],
            80,
        );

        overlay.begin_backtrack_preview();
        assert_eq!(
            overlay.selected_user_message().map(|m| m.text),
            Some("user 2".to_string())
        );
    }

    #[test]
    fn transcript_overlay_selected_user_message_returns_full_payload() {
        let mut overlay = TranscriptOverlay::new(
            vec![
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 1")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 1")),
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 2")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 2")),
                    is_selected_user: false,
                },
            ],
            80,
        );

        overlay.begin_backtrack_preview();
        let selected = overlay.selected_user_message().expect("selected latest user");
        assert_eq!(selected.text, "user 2");
    }

    #[test]
    fn transcript_overlay_hint_lines_match_backtrack_copy() {
        let (line1, line2) = transcript_hint_lines_for_test();
        assert!(line1.contains("↑/↓ to scroll"));
        assert!(line1.contains("pgup/pgdn to page"));
        assert!(line1.contains("home/end to jump"));
        assert!(line2.contains("q to quit"));
        assert!(line2.contains("esc/← to edit prev"));
        assert!(line2.contains("→ to edit next"));
        assert!(line2.contains("enter to edit message"));
    }

    #[test]
    fn transcript_overlay_selected_user_index_tracks_history_position() {
        let mut overlay = TranscriptOverlay::new(
            vec![
                TranscriptOverlayCell {
                    lines: vec![Line::from("assistant 1")],
                    is_stream_continuation: false,
                    user_message: None,
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 1")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 1")),
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("assistant 2")],
                    is_stream_continuation: false,
                    user_message: None,
                    is_selected_user: false,
                },
                TranscriptOverlayCell {
                    lines: vec![Line::from("user 2")],
                    is_stream_continuation: false,
                    user_message: Some(crate::chatwidget::UserMessage::from("user 2")),
                    is_selected_user: false,
                },
            ],
            80,
        );

        overlay.begin_backtrack_preview();
        assert_eq!(overlay.selected_user_history_position(), Some(1));
        overlay.select_prev_user();
        assert_eq!(overlay.selected_user_history_position(), Some(0));
    }

    #[test]
    fn transcript_overlay_user_selection_renders_full_row_highlight() {
        let mut overlay = TranscriptOverlay::new(
            vec![TranscriptOverlayCell {
                lines: vec![
                    Line::from("▌"),
                    Line::from("▌ selected message"),
                    Line::from("▌"),
                ],
                is_stream_continuation: false,
                user_message: Some(crate::chatwidget::UserMessage::from("selected message")),
                is_selected_user: false,
            }],
            40,
        );
        overlay.begin_backtrack_preview();

        let area = Rect::new(0, 0, 40, 8);
        let mut buf = Buffer::empty(area);
        overlay.view.render(area, &mut buf);

        let target_row = (0..area.height)
            .find(|row| {
                (0..area.width)
                    .map(|col| buf[(col, *row)].symbol())
                    .collect::<String>()
                    .contains("selected message")
            })
            .expect("selected message row");
        let bg0 = buf[(0, target_row)].style().bg;
        let bg10 = buf[(10, target_row)].style().bg;
        let bg30 = buf[(30, target_row)].style().bg;
        assert_eq!(bg0, bg10);
        assert_eq!(bg10, bg30);
        assert_ne!(bg0, Some(Color::Reset));
    }
}
