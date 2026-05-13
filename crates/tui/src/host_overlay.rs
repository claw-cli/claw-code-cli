//! Host-side lifecycle helpers for alternate-screen overlays.

use anyhow::Result;
use devo_utils::ansi_escape::ansi_escape_line;
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::time::Duration;

use crate::chatwidget::ChatWidget;
use crate::pager_overlay::Overlay;
use crate::tui::Tui;
use crate::tui::TuiEvent;

#[derive(Debug, Default)]
pub(crate) struct OverlayState {
    overlay: Option<Overlay>,
}

impl OverlayState {
    pub(crate) fn is_active(&self) -> bool {
        self.overlay.is_some()
    }

    pub(crate) fn handle_tui_event(
        &mut self,
        tui_event: TuiEvent,
        tui: &mut Tui,
        chat_widget: &mut ChatWidget,
    ) -> Result<()> {
        let Some(overlay) = self.overlay.as_mut() else {
            return Ok(());
        };

        if matches!(tui_event, TuiEvent::Draw) {
            let width = tui.terminal.size()?.width.max(1);
            overlay.set_transcript_lines(chat_widget.transcript_overlay_lines(width));
        }

        overlay.handle_event(tui, tui_event)?;
        if overlay.is_done() {
            self.overlay = None;
            tui.leave_alt_screen()?;
            tui.frame_requester().schedule_frame();
        } else if chat_widget.transcript_overlay_has_live_tail() {
            tui.frame_requester()
                .schedule_frame_in(Duration::from_millis(50));
        }

        Ok(())
    }

    pub(crate) fn open_transcript(
        &mut self,
        tui: &mut Tui,
        chat_widget: &ChatWidget,
    ) -> Result<()> {
        let width = tui.terminal.size()?.width.max(1);
        tui.enter_alt_screen()?;
        self.overlay = Some(Overlay::new_transcript(
            chat_widget.transcript_overlay_lines(width),
        ));
        tui.frame_requester().schedule_frame();
        Ok(())
    }

    pub(crate) fn open_diff(
        &mut self,
        tui: &mut Tui,
        chat_widget: &mut ChatWidget,
        text: String,
    ) -> Result<()> {
        tui.enter_alt_screen()?;
        self.overlay = Some(Overlay::new_static_with_lines(
            diff_overlay_lines(&text),
            "D I F F".to_string(),
        ));
        chat_widget.set_status_message("Diff shown");
        tui.frame_requester().schedule_frame();
        Ok(())
    }
}

fn diff_overlay_lines(text: &str) -> Vec<Line<'static>> {
    if text.trim().is_empty() {
        vec!["No changes detected.".italic().into()]
    } else {
        text.lines().map(ansi_escape_line).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn diff_overlay_lines_render_empty_diff_message() {
        let lines = diff_overlay_lines("");
        assert_eq!(1, lines.len());
        let text = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!("No changes detected.", text);
    }
}
