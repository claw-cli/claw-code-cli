use pretty_assertions::assert_eq;
use ratatui::backend::Backend;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::custom_terminal::Terminal;
use crate::insert_history::insert_history_lines;
use crate::test_backend::VT100Backend;

#[test]
fn clear_managed_inline_area_preserves_rows_above_devo() {
    let width: u16 = 24;
    let height: u16 = 8;
    let backend = VT100Backend::new(width, height);
    let mut term = Terminal::with_options(backend).expect("terminal");
    term.set_viewport_area(Rect::new(0, 2, width, 2));

    insert_history_lines(&mut term, vec![Line::from("devo line").into()]).expect("insert history");
    let rows_before: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
    let devo_row = rows_before
        .iter()
        .position(|row| row.contains("devo line"))
        .expect("expected devo line on screen");
    assert_eq!(2, devo_row);

    term.clear_managed_inline_area()
        .expect("clear managed inline area");

    let rows_after: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
    assert_eq!(rows_before[0], rows_after[0]);
    assert_eq!(rows_before[1], rows_after[1]);
    assert_eq!(2, term.viewport_area.y);
    assert_eq!("", rows_after[2].trim_end());
    assert!(
        rows_after.iter().all(|row| !row.contains("devo line")),
        "expected devo-managed rows to be cleared, rows: {rows_after:?}"
    );
}

#[test]
fn clear_screen_area_only_clears_target_rows() {
    let width: u16 = 24;
    let height: u16 = 8;
    let backend = VT100Backend::new(width, height);
    let mut term = Terminal::with_options(backend).expect("terminal");

    term.backend_mut()
        .set_cursor_position(Position { x: 0, y: 1 })
        .expect("cursor position");
    std::io::Write::write_all(term.backend_mut(), b"keep row").expect("write preserved row");

    term.backend_mut()
        .set_cursor_position(Position { x: 0, y: 3 })
        .expect("cursor position");
    std::io::Write::write_all(term.backend_mut(), b"clear me").expect("write cleared row");

    term.clear_screen_area(Rect::new(0, 3, width, 1))
        .expect("clear target area");

    let rows_after: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
    assert!(rows_after[1].contains("keep row"));
    assert_eq!("", rows_after[3].trim_end());
}

#[test]
fn clear_visible_screen_resets_viewport_to_top() {
    let width: u16 = 24;
    let height: u16 = 8;
    let backend = VT100Backend::new(width, height);
    let mut term = Terminal::with_options(backend).expect("terminal");
    term.set_viewport_area(Rect::new(0, 5, width, 2));

    term.clear_visible_screen().expect("clear visible screen");

    assert_eq!(0, term.viewport_area.y);
}

#[test]
fn clear_inline_viewport_preserves_inserted_history_rows() {
    let width: u16 = 24;
    let height: u16 = 8;
    let backend = VT100Backend::new(width, height);
    let mut term = Terminal::with_options(backend).expect("terminal");
    term.set_viewport_area(Rect::new(0, 3, width, 2));

    insert_history_lines(&mut term, vec![Line::from("history row").into()])
        .expect("insert history");
    let history_row = term.viewport_area.top().saturating_sub(1);
    let viewport_top = term.viewport_area.top();

    term.backend_mut()
        .set_cursor_position(Position {
            x: 0,
            y: viewport_top,
        })
        .expect("cursor position");
    std::io::Write::write_all(term.backend_mut(), b"live row").expect("write live row");

    term.clear_inline_viewport().expect("clear inline viewport");

    let rows_after: Vec<String> = term.backend().vt100().screen().rows(0, width).collect();
    assert!(rows_after[history_row as usize].contains("history row"));
    assert_eq!("", rows_after[term.viewport_area.top() as usize].trim_end());
    assert!(
        rows_after.iter().all(|row| !row.contains("live row")),
        "expected live viewport rows to be cleared, rows: {rows_after:?}"
    );
}
