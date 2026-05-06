use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use itertools::Itertools as _;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use super::selection_popup_common::render_menu_surface;
use super::selection_popup_common::wrap_styled_line;
use crate::app_event_sender::AppEventSender;
use crate::key_hint::KeyBinding;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;

use super::CancellationEvent;
use super::bottom_pane_view::BottomPaneView;
use super::popup_consts::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
pub(crate) use super::selection_popup_common::ColumnWidthMode;
use super::selection_popup_common::GenericDisplayRow;
use super::selection_popup_common::measure_rows_height;
use super::selection_popup_common::measure_rows_height_stable_col_widths;
use super::selection_popup_common::measure_rows_height_with_col_width_mode;
use super::selection_popup_common::render_rows;
use super::selection_popup_common::render_rows_stable_col_widths;
use super::selection_popup_common::render_rows_with_col_width_mode;
use unicode_width::UnicodeWidthStr;

/// Minimum list width (in content columns) required before the side-by-side
/// layout is activated. Keeps the list usable even when sharing horizontal
/// space with the side content panel.
const MIN_LIST_WIDTH_FOR_SIDE: u16 = 40;

/// Horizontal gap (in columns) between the list area and the side content
/// panel when side-by-side layout is active.
const SIDE_CONTENT_GAP: u16 = 2;

/// Shared menu-surface horizontal inset (2 cells per side) used by selection popups.
const MENU_SURFACE_HORIZONTAL_INSET: u16 = 4;

/// Controls how the side content panel is sized relative to the popup width.
///
/// When the computed side width falls below `side_content_min_width` or the
/// remaining list area would be narrower than [`MIN_LIST_WIDTH_FOR_SIDE`], the
/// side-by-side layout is abandoned and the stacked fallback is used instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SideContentWidth {
    /// Fixed number of columns.  `Fixed(0)` disables side content entirely.
    Fixed(u16),
    /// Exact 50/50 split of the content area (minus the inter-column gap).
    #[allow(dead_code)]
    Half,
}

impl Default for SideContentWidth {
    fn default() -> Self {
        Self::Fixed(0)
    }
}

/// Returns the popup content width after subtracting the shared menu-surface
/// horizontal inset (2 columns on each side).
pub(crate) fn popup_content_width(total_width: u16) -> u16 {
    total_width.saturating_sub(MENU_SURFACE_HORIZONTAL_INSET)
}

/// Returns side-by-side layout widths as `(list_width, side_width)` when the
/// layout can fit. Returns `None` when the side panel is disabled/too narrow or
/// when the remaining list width would become unusably small.
pub(crate) fn side_by_side_layout_widths(
    content_width: u16,
    side_content_width: SideContentWidth,
    side_content_min_width: u16,
) -> Option<(u16, u16)> {
    let side_width = match side_content_width {
        SideContentWidth::Fixed(0) => return None,
        SideContentWidth::Fixed(width) => width,
        SideContentWidth::Half => content_width.saturating_sub(SIDE_CONTENT_GAP) / 2,
    };
    if side_width < side_content_min_width {
        return None;
    }
    let list_width = content_width.saturating_sub(SIDE_CONTENT_GAP + side_width);
    (list_width >= MIN_LIST_WIDTH_FOR_SIDE).then_some((list_width, side_width))
}

/// One selectable item in the generic selection list.
pub(crate) type SelectionAction = Box<dyn Fn(&AppEventSender) + Send + Sync>;

/// Callback invoked whenever the highlighted item changes (arrow keys, search
/// filter, number-key jump).  Receives the *actual* index into the unfiltered
/// `items` list and the event sender.  Used by the theme picker for live preview.
pub(crate) type OnSelectionChangedCallback =
    Option<Box<dyn Fn(usize, &AppEventSender) + Send + Sync>>;

/// Callback invoked when the picker is dismissed without accepting (Esc or
/// Ctrl+C).  Used by the theme picker to restore the pre-open theme.
pub(crate) type OnCancelCallback = Option<Box<dyn Fn(&AppEventSender) + Send + Sync>>;

/// One row in a [`ListSelectionView`] selection list.
///
/// This is the source-of-truth model for row state before filtering and
/// formatting into render rows. A row is treated as disabled when either
/// `is_disabled` is true or `disabled_reason` is present; disabled rows cannot
/// be accepted and are skipped by keyboard navigation.
#[derive(Default)]
pub(crate) struct SelectionItem {
    pub name: String,
    pub name_prefix_spans: Vec<Span<'static>>,
    pub display_shortcut: Option<KeyBinding>,
    pub description: Option<String>,
    pub selected_description: Option<String>,
    pub is_current: bool,
    pub is_default: bool,
    pub is_disabled: bool,
    pub actions: Vec<SelectionAction>,
    pub dismiss_on_select: bool,
    pub search_value: Option<String>,
    pub disabled_reason: Option<String>,
}

/// Construction-time configuration for [`ListSelectionView`].
///
/// This config is consumed once by [`ListSelectionView::new`]. After
/// construction, mutable interaction state (filtering, scrolling, and selected
/// row) lives on the view itself.
///
/// `col_width_mode` controls column width mode in selection lists:
/// `AutoVisible` (default) measures only rows visible in the viewport
/// `AutoAllRows` measures all rows to ensure stable column widths as the user scrolls
/// `Fixed` used a fixed 30/70  split between columns
pub(crate) struct SelectionViewParams {
    pub view_id: Option<&'static str>,
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub footer_note: Option<Line<'static>>,
    pub footer_hint: Option<Line<'static>>,
    pub items: Vec<SelectionItem>,
    pub is_searchable: bool,
    pub search_placeholder: Option<String>,
    pub col_width_mode: ColumnWidthMode,
    pub header: Box<dyn Renderable>,
    pub initial_selected_idx: Option<usize>,

    /// Rich content rendered beside (wide terminals) or below (narrow terminals)
    /// the list items, inside the bordered menu surface. Used by the theme picker
    /// to show a syntax-highlighted preview.
    pub side_content: Box<dyn Renderable>,

    /// Width mode for side content when side-by-side layout is active.
    pub side_content_width: SideContentWidth,

    /// Minimum side panel width required before side-by-side layout activates.
    pub side_content_min_width: u16,

    /// Optional fallback content rendered when side-by-side does not fit.
    /// When absent, `side_content` is reused.
    pub stacked_side_content: Option<Box<dyn Renderable>>,

    /// Keep side-content background colors after rendering in side-by-side mode.
    /// Disabled by default so existing popups preserve their reset-background look.
    pub preserve_side_content_bg: bool,

    /// Called when the highlighted item changes (navigation, filter, number-key).
    /// Receives the *actual* item index, not the filtered/visible index.
    pub on_selection_changed: OnSelectionChangedCallback,

    /// Called when the picker is dismissed via Esc/Ctrl+C without selecting.
    pub on_cancel: OnCancelCallback,
}

impl Default for SelectionViewParams {
    fn default() -> Self {
        Self {
            view_id: None,
            title: None,
            subtitle: None,
            footer_note: None,
            footer_hint: None,
            items: Vec::new(),
            is_searchable: false,
            search_placeholder: None,
            col_width_mode: ColumnWidthMode::AutoVisible,
            header: Box::new(()),
            initial_selected_idx: None,
            side_content: Box::new(()),
            side_content_width: SideContentWidth::default(),
            side_content_min_width: 0,
            stacked_side_content: None,
            preserve_side_content_bg: false,
            on_selection_changed: None,
            on_cancel: None,
        }
    }
}

/// Runtime state for rendering and interacting with a list-based selection popup.
///
/// This type is the single authority for filtered index mapping between
/// visible rows and source items and for preserving selection while filters
/// change.
pub(crate) struct ListSelectionView {
    view_id: Option<&'static str>,
    footer_note: Option<Line<'static>>,
    footer_hint: Option<Line<'static>>,
    items: Vec<SelectionItem>,
    state: ScrollState,
    complete: bool,
    app_event_tx: AppEventSender,
    is_searchable: bool,
    search_query: String,
    search_placeholder: Option<String>,
    col_width_mode: ColumnWidthMode,
    filtered_indices: Vec<usize>,
    last_selected_actual_idx: Option<usize>,
    header: Box<dyn Renderable>,
    initial_selected_idx: Option<usize>,
    side_content: Box<dyn Renderable>,
    side_content_width: SideContentWidth,
    side_content_min_width: u16,
    stacked_side_content: Option<Box<dyn Renderable>>,
    preserve_side_content_bg: bool,

    /// Called when the highlighted item changes (navigation, filter, number-key).
    on_selection_changed: OnSelectionChangedCallback,

    /// Called when the picker is dismissed via Esc/Ctrl+C without selecting.
    on_cancel: OnCancelCallback,

    accent_color: Color,
}

impl ListSelectionView {
    #[allow(dead_code)]
    /// Create a selection popup view with filtering, scrolling, and callbacks wired.
    ///
    /// The constructor normalizes header/title composition and immediately
    /// applies filtering so `ScrollState` starts in a valid visible range.
    /// When search is enabled, rows without `search_value` will disappear as
    /// soon as the query is non-empty, which can look like dropped data unless
    /// callers intentionally populate that field.
    pub fn new(
        params: SelectionViewParams,
        app_event_tx: AppEventSender,
        accent_color: Color,
    ) -> Self {
        let mut header = params.header;
        if params.title.is_some() || params.subtitle.is_some() {
            let title = params.title.map(|title| Line::from(title.bold()));
            let subtitle = params.subtitle.map(|subtitle| Line::from(subtitle.dim()));
            header = Box::new(ColumnRenderable::with([
                header,
                Box::new(title),
                Box::new(subtitle),
            ]));
        }
        let mut s = Self {
            view_id: params.view_id,
            footer_note: params.footer_note,
            footer_hint: params.footer_hint,
            items: params.items,
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            is_searchable: params.is_searchable,
            search_query: String::new(),
            search_placeholder: if params.is_searchable {
                params.search_placeholder
            } else {
                None
            },
            col_width_mode: params.col_width_mode,
            filtered_indices: Vec::new(),
            last_selected_actual_idx: None,
            header,
            initial_selected_idx: params.initial_selected_idx,
            side_content: params.side_content,
            side_content_width: params.side_content_width,
            side_content_min_width: params.side_content_min_width,
            stacked_side_content: params.stacked_side_content,
            preserve_side_content_bg: params.preserve_side_content_bg,
            on_selection_changed: params.on_selection_changed,
            on_cancel: params.on_cancel,
            accent_color,
        };
        s.apply_filter();
        s
    }

    fn visible_len(&self) -> usize {
        self.filtered_indices.len()
    }

    fn max_visible_rows(len: usize) -> usize {
        MAX_POPUP_ROWS.min(len.max(1))
    }

    fn selected_actual_idx(&self) -> Option<usize> {
        self.state
            .selected_idx
            .and_then(|visible_idx| self.filtered_indices.get(visible_idx).copied())
    }

    fn apply_filter(&mut self) {
        let previously_selected = self
            .selected_actual_idx()
            .or_else(|| {
                (!self.is_searchable)
                    .then(|| self.items.iter().position(|item| item.is_current))
                    .flatten()
            })
            .or_else(|| self.initial_selected_idx.take());

        if self.is_searchable && !self.search_query.is_empty() {
            let query_lower = self.search_query.to_lowercase();
            self.filtered_indices = self
                .items
                .iter()
                .positions(|item| {
                    item.search_value
                        .as_ref()
                        .is_some_and(|v| v.to_lowercase().contains(&query_lower))
                })
                .collect();
        } else {
            self.filtered_indices = (0..self.items.len()).collect();
        }

        let len = self.filtered_indices.len();
        self.state.selected_idx = self
            .state
            .selected_idx
            .and_then(|visible_idx| {
                self.filtered_indices
                    .get(visible_idx)
                    .and_then(|idx| self.filtered_indices.iter().position(|cur| cur == idx))
            })
            .or_else(|| {
                previously_selected.and_then(|actual_idx| {
                    self.filtered_indices
                        .iter()
                        .position(|idx| *idx == actual_idx)
                })
            })
            .or_else(|| (len > 0).then_some(0));

        let visible = Self::max_visible_rows(len);
        self.state.clamp_selection(len);
        self.state.ensure_visible(len, visible);

        // Notify the callback when filtering changes the selected actual item
        // so live preview stays in sync (e.g. typing in the theme picker).
        let new_actual = self.selected_actual_idx();
        if new_actual != previously_selected {
            self.fire_selection_changed();
        }
    }

    fn build_rows(&self) -> Vec<GenericDisplayRow> {
        self.filtered_indices
            .iter()
            .enumerate()
            .filter_map(|(visible_idx, actual_idx)| {
                self.items.get(*actual_idx).map(|item| {
                    let is_selected = self.state.selected_idx == Some(visible_idx);
                    let prefix = if is_selected { '›' } else { ' ' };
                    let name = item.name.as_str();
                    let marker = if item.is_current {
                        " (current)"
                    } else if item.is_default {
                        " (default)"
                    } else {
                        ""
                    };
                    let name_with_marker = format!("{name}{marker}");
                    let is_disabled = item.is_disabled || item.disabled_reason.is_some();
                    let n = visible_idx + 1;
                    let wrap_prefix = if self.is_searchable {
                        // The number keys don't work when search is enabled (since we let the
                        // numbers be used for the search query).
                        format!("{prefix} ")
                    } else if is_disabled {
                        format!("{prefix} {}", " ".repeat(n.to_string().len() + 2))
                    } else {
                        format!("{prefix} {n}. ")
                    };
                    let wrap_prefix_width = UnicodeWidthStr::width(wrap_prefix.as_str());
                    let mut name_prefix_spans = Vec::new();
                    name_prefix_spans.push(wrap_prefix.into());
                    name_prefix_spans.extend(item.name_prefix_spans.clone());
                    let description = is_selected
                        .then(|| item.selected_description.clone())
                        .flatten()
                        .or_else(|| item.description.clone());
                    let wrap_indent = description.is_none().then_some(wrap_prefix_width);
                    GenericDisplayRow {
                        name: name_with_marker,
                        name_prefix_spans,
                        display_shortcut: item.display_shortcut,
                        match_indices: None,
                        description,
                        category_tag: None,
                        wrap_indent,
                        is_disabled,
                        disabled_reason: item.disabled_reason.clone(),
                    }
                })
            })
            .collect()
    }

    fn move_up(&mut self) {
        let before = self.selected_actual_idx();
        let len = self.visible_len();
        self.state.move_up_wrap(len);
        let visible = Self::max_visible_rows(len);
        self.state.ensure_visible(len, visible);
        self.skip_disabled_up();
        if self.selected_actual_idx() != before {
            self.fire_selection_changed();
        }
    }

    fn move_down(&mut self) {
        let before = self.selected_actual_idx();
        let len = self.visible_len();
        self.state.move_down_wrap(len);
        let visible = Self::max_visible_rows(len);
        self.state.ensure_visible(len, visible);
        self.skip_disabled_down();
        if self.selected_actual_idx() != before {
            self.fire_selection_changed();
        }
    }

    fn fire_selection_changed(&self) {
        if let Some(cb) = &self.on_selection_changed
            && let Some(actual) = self.selected_actual_idx()
        {
            cb(actual, &self.app_event_tx);
        }
    }

    fn accept(&mut self) {
        let selected_item = self
            .state
            .selected_idx
            .and_then(|idx| self.filtered_indices.get(idx))
            .and_then(|actual_idx| self.items.get(*actual_idx));
        if let Some(item) = selected_item
            && item.disabled_reason.is_none()
            && !item.is_disabled
        {
            if let Some(idx) = self.state.selected_idx
                && let Some(actual_idx) = self.filtered_indices.get(idx)
            {
                self.last_selected_actual_idx = Some(*actual_idx);
            }
            for act in &item.actions {
                act(&self.app_event_tx);
            }
            if item.dismiss_on_select {
                self.complete = true;
            }
        } else if selected_item.is_none() {
            if let Some(cb) = &self.on_cancel {
                cb(&self.app_event_tx);
            }
            self.complete = true;
        }
    }

    #[allow(dead_code)]
    #[cfg(test)]
    pub(crate) fn set_search_query(&mut self, query: String) {
        self.search_query = query;
        self.apply_filter();
    }

    #[allow(dead_code)]
    pub(crate) fn take_last_selected_index(&mut self) -> Option<usize> {
        self.last_selected_actual_idx.take()
    }

    fn rows_width(total_width: u16) -> u16 {
        total_width.saturating_sub(2)
    }

    fn clear_to_terminal_bg(buf: &mut Buffer, area: Rect) {
        let buf_area = buf.area();
        let min_x = area.x.max(buf_area.x);
        let min_y = area.y.max(buf_area.y);
        let max_x = area
            .x
            .saturating_add(area.width)
            .min(buf_area.x.saturating_add(buf_area.width));
        let max_y = area
            .y
            .saturating_add(area.height)
            .min(buf_area.y.saturating_add(buf_area.height));
        for y in min_y..max_y {
            for x in min_x..max_x {
                buf[(x, y)]
                    .set_symbol(" ")
                    .set_style(ratatui::style::Style::reset());
            }
        }
    }

    fn force_bg_to_terminal_bg(buf: &mut Buffer, area: Rect) {
        let buf_area = buf.area();
        let min_x = area.x.max(buf_area.x);
        let min_y = area.y.max(buf_area.y);
        let max_x = area
            .x
            .saturating_add(area.width)
            .min(buf_area.x.saturating_add(buf_area.width));
        let max_y = area
            .y
            .saturating_add(area.height)
            .min(buf_area.y.saturating_add(buf_area.height));
        for y in min_y..max_y {
            for x in min_x..max_x {
                buf[(x, y)].set_bg(ratatui::style::Color::Reset);
            }
        }
    }

    fn stacked_side_content(&self) -> &dyn Renderable {
        self.stacked_side_content
            .as_deref()
            .unwrap_or_else(|| self.side_content.as_ref())
    }

    /// Returns `Some(side_width)` when the content area is wide enough for a
    /// side-by-side layout (list + gap + side panel), `None` otherwise.
    fn side_layout_width(&self, content_width: u16) -> Option<u16> {
        side_by_side_layout_widths(
            content_width,
            self.side_content_width,
            self.side_content_min_width,
        )
        .map(|(_, side_width)| side_width)
    }

    fn skip_disabled_down(&mut self) {
        let len = self.visible_len();
        for _ in 0..len {
            if let Some(idx) = self.state.selected_idx
                && let Some(actual_idx) = self.filtered_indices.get(idx)
                && self
                    .items
                    .get(*actual_idx)
                    .is_some_and(|item| item.disabled_reason.is_some() || item.is_disabled)
            {
                self.state.move_down_wrap(len);
            } else {
                break;
            }
        }
    }

    fn skip_disabled_up(&mut self) {
        let len = self.visible_len();
        for _ in 0..len {
            if let Some(idx) = self.state.selected_idx
                && let Some(actual_idx) = self.filtered_indices.get(idx)
                && self
                    .items
                    .get(*actual_idx)
                    .is_some_and(|item| item.disabled_reason.is_some() || item.is_disabled)
            {
                self.state.move_up_wrap(len);
            } else {
                break;
            }
        }
    }
}

impl BottomPaneView for ListSelectionView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            // Some terminals (or configurations) send Control key chords as
            // C0 control characters without reporting the CONTROL modifier.
            // Handle fallbacks for Ctrl-P/N here so navigation works everywhere.
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{0010}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^P */ => self.move_up(),
            KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } if !self.is_searchable => self.move_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{000e}'),
                modifiers: KeyModifiers::NONE,
                ..
            } /* ^N */ => self.move_down(),
            KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } if !self.is_searchable => self.move_down(),
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } if self.is_searchable => {
                self.search_query.pop();
                self.apply_filter();
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.on_ctrl_c();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if self.is_searchable
                && !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.search_query.push(c);
                self.apply_filter();
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !self.is_searchable
                && !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some((visible_idx, _)) = self
                    .filtered_indices
                    .iter()
                    .enumerate()
                    .find(|(_, actual_idx)| {
                        self.items.get(**actual_idx).is_some_and(|item| {
                            item.display_shortcut
                                .as_ref()
                                .is_some_and(|shortcut| shortcut.is_press(key_event))
                                && item.disabled_reason.is_none()
                                && !item.is_disabled
                        })
                    })
                {
                    self.state.selected_idx = Some(visible_idx);
                    self.accept();
                } else if let Some(idx) = c
                    .to_digit(10)
                    .map(|d| d as usize)
                    .and_then(|d| d.checked_sub(1))
                    && idx < self.items.len()
                    && self
                        .items
                        .get(idx)
                        .is_some_and(|item| item.disabled_reason.is_none() && !item.is_disabled)
                {
                    self.state.selected_idx = Some(idx);
                    self.accept();
                }
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.accept(),
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn view_id(&self) -> Option<&'static str> {
        self.view_id
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_actual_idx()
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        if let Some(cb) = &self.on_cancel {
            cb(&self.app_event_tx);
        }
        self.complete = true;
        CancellationEvent::Handled
    }
}

impl Renderable for ListSelectionView {
    fn desired_height(&self, width: u16) -> u16 {
        // Inner content width after menu surface horizontal insets (2 per side).
        let inner_width = popup_content_width(width);

        // When side-by-side is active, measure the list at the reduced width
        // that accounts for the gap and side panel.
        let effective_rows_width = if let Some(side_w) = self.side_layout_width(inner_width) {
            Self::rows_width(width).saturating_sub(SIDE_CONTENT_GAP + side_w)
        } else {
            Self::rows_width(width)
        };

        // Measure wrapped height for up to MAX_POPUP_ROWS items.
        let rows = self.build_rows();
        let rows_height = match self.col_width_mode {
            ColumnWidthMode::AutoVisible => measure_rows_height(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::AutoAllRows => measure_rows_height_stable_col_widths(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::Fixed => measure_rows_height_with_col_width_mode(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
                ColumnWidthMode::Fixed,
            ),
        };

        let mut height = self.header.desired_height(inner_width);
        height = height.saturating_add(rows_height + 3);
        if self.is_searchable {
            height = height.saturating_add(1);
        }

        // Side content: when the terminal is wide enough the panel sits beside
        // the list and shares vertical space; otherwise it stacks below.
        if self.side_layout_width(inner_width).is_some() {
            // Side-by-side — side content shares list rows vertically so it
            // doesn't add to total height.
        } else {
            let side_h = self.stacked_side_content().desired_height(inner_width);
            if side_h > 0 {
                height = height.saturating_add(1 + side_h);
            }
        }

        if let Some(note) = &self.footer_note {
            let note_width = width.saturating_sub(2);
            let note_lines = wrap_styled_line(note, note_width);
            height = height.saturating_add(note_lines.len() as u16);
        }
        if self.footer_hint.is_some() {
            height = height.saturating_add(1);
        }
        height
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let note_width = area.width.saturating_sub(2);
        let note_lines = self
            .footer_note
            .as_ref()
            .map(|note| wrap_styled_line(note, note_width));
        let note_height = note_lines.as_ref().map_or(0, |lines| lines.len() as u16);
        let footer_rows = note_height + u16::from(self.footer_hint.is_some());
        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(footer_rows)]).areas(area);

        let outer_content_area = content_area;
        // Paint the shared menu surface and then layout inside the returned inset.
        let content_area = render_menu_surface(outer_content_area, buf);

        let inner_width = popup_content_width(outer_content_area.width);
        let side_w = self.side_layout_width(inner_width);

        // When side-by-side is active, shrink the list to make room.
        let full_rows_width = Self::rows_width(outer_content_area.width);
        let effective_rows_width = if let Some(sw) = side_w {
            full_rows_width.saturating_sub(SIDE_CONTENT_GAP + sw)
        } else {
            full_rows_width
        };

        let header_height = self.header.desired_height(inner_width);
        let rows = self.build_rows();
        let rows_height = match self.col_width_mode {
            ColumnWidthMode::AutoVisible => measure_rows_height(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::AutoAllRows => measure_rows_height_stable_col_widths(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
            ),
            ColumnWidthMode::Fixed => measure_rows_height_with_col_width_mode(
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                effective_rows_width.saturating_add(1),
                ColumnWidthMode::Fixed,
            ),
        };

        // Stacked (fallback) side content height — only used when not side-by-side.
        let stacked_side_h = if side_w.is_none() {
            self.stacked_side_content().desired_height(inner_width)
        } else {
            0
        };
        let stacked_gap = if stacked_side_h > 0 { 1 } else { 0 };

        let [header_area, _, search_area, list_area, _, stacked_side_area] = Layout::vertical([
            Constraint::Max(header_height),
            Constraint::Max(1),
            Constraint::Length(if self.is_searchable { 1 } else { 0 }),
            Constraint::Length(rows_height),
            Constraint::Length(stacked_gap),
            Constraint::Length(stacked_side_h),
        ])
        .areas(content_area);

        // -- Header --
        if header_area.height < header_height {
            let [header_area, elision_area] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(header_area);
            self.header.render(header_area, buf);
            Paragraph::new(vec![
                Line::from(format!("[… {header_height} lines] ctrl + a view all")).dim(),
            ])
            .render(elision_area, buf);
        } else {
            self.header.render(header_area, buf);
        }

        // -- Search bar --
        if self.is_searchable {
            Line::from(self.search_query.clone()).render(search_area, buf);
            let query_span: Span<'static> = if self.search_query.is_empty() {
                self.search_placeholder
                    .as_ref()
                    .map(|placeholder| placeholder.clone().dim())
                    .unwrap_or_else(|| "".into())
            } else {
                self.search_query.clone().into()
            };
            Line::from(query_span).render(search_area, buf);
        }

        // -- List rows --
        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(2),
                y: list_area.y,
                width: effective_rows_width.max(1),
                height: list_area.height,
            };
            match self.col_width_mode {
                ColumnWidthMode::AutoVisible => render_rows(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                    self.accent_color,
                ),
                ColumnWidthMode::AutoAllRows => render_rows_stable_col_widths(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                    self.accent_color,
                ),
                ColumnWidthMode::Fixed => render_rows_with_col_width_mode(
                    render_area,
                    buf,
                    &rows,
                    &self.state,
                    render_area.height as usize,
                    "no matches",
                    ColumnWidthMode::Fixed,
                    self.accent_color,
                ),
            };
        }

        // -- Side content (preview panel) --
        if let Some(sw) = side_w {
            // Side-by-side: render to the right half of the popup content
            // area so preview content can center vertically in that panel.
            let side_x = content_area.x + content_area.width - sw;
            let side_area = Rect::new(side_x, content_area.y, sw, content_area.height);

            // Clear the menu-surface background behind the side panel so the
            // preview appears on the terminal's own background.
            let clear_x = side_x.saturating_sub(SIDE_CONTENT_GAP);
            let clear_w = outer_content_area
                .x
                .saturating_add(outer_content_area.width)
                .saturating_sub(clear_x);
            Self::clear_to_terminal_bg(
                buf,
                Rect::new(
                    clear_x,
                    outer_content_area.y,
                    clear_w,
                    outer_content_area.height,
                ),
            );
            self.side_content.render(side_area, buf);
            if !self.preserve_side_content_bg {
                Self::force_bg_to_terminal_bg(
                    buf,
                    Rect::new(
                        clear_x,
                        outer_content_area.y,
                        clear_w,
                        outer_content_area.height,
                    ),
                );
            }
        } else if stacked_side_area.height > 0 {
            // Stacked fallback: render below the list (same as old footer_content).
            let clear_height = (outer_content_area.y + outer_content_area.height)
                .saturating_sub(stacked_side_area.y);
            let clear_area = Rect::new(
                outer_content_area.x,
                stacked_side_area.y,
                outer_content_area.width,
                clear_height,
            );
            Self::clear_to_terminal_bg(buf, clear_area);
            self.stacked_side_content().render(stacked_side_area, buf);
        }

        if footer_area.height > 0 {
            let [note_area, hint_area] = Layout::vertical([
                Constraint::Length(note_height),
                Constraint::Length(if self.footer_hint.is_some() { 1 } else { 0 }),
            ])
            .areas(footer_area);

            if let Some(lines) = note_lines {
                let note_area = Rect {
                    x: note_area.x + 2,
                    y: note_area.y,
                    width: note_area.width.saturating_sub(2),
                    height: note_area.height,
                };
                for (idx, line) in lines.iter().enumerate() {
                    if idx as u16 >= note_area.height {
                        break;
                    }
                    let line_area = Rect {
                        x: note_area.x,
                        y: note_area.y + idx as u16,
                        width: note_area.width,
                        height: 1,
                    };
                    line.clone().render(line_area, buf);
                }
            }

            if let Some(hint) = &self.footer_hint {
                let hint_area = Rect {
                    x: hint_area.x + 2,
                    y: hint_area.y,
                    width: hint_area.width.saturating_sub(2),
                    height: hint_area.height,
                };
                hint.clone().dim().render(hint_area, buf);
            }
        }
    }
}
