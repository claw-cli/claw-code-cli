use crate::render::renderable::Renderable;
use crossterm::event::KeyEvent;

use super::CancellationEvent;

/// Trait implemented by every view that can be shown in the bottom pane.
pub(crate) trait BottomPaneView: Renderable {
    /// Handle a key event while the view is active. A redraw is always
    /// scheduled after this call.
    fn handle_key_event(&mut self, _key_event: KeyEvent) {}

    /// Return `true` if the view has finished and should be removed.
    fn is_complete(&self) -> bool {
        false
    }

    #[allow(dead_code)]
    /// Stable identifier for views that need external refreshes while open.
    fn view_id(&self) -> Option<&'static str> {
        None
    }

    #[allow(dead_code)]
    /// Actual item index for list-based views that want to preserve selection
    /// across external refreshes.
    fn selected_index(&self) -> Option<usize> {
        None
    }

    fn take_model_selection(&mut self) -> Option<String> {
        None
    }

    fn take_theme_selection(&mut self) -> Option<String> {
        None
    }

    /// Handle Ctrl-C while this view is active.
    fn on_ctrl_c(&mut self) -> CancellationEvent {
        CancellationEvent::NotHandled
    }

    /// Return true if Esc should be routed through `handle_key_event` instead
    /// of the `on_ctrl_c` cancellation path.
    fn prefer_esc_to_handle_key_event(&self) -> bool {
        false
    }

    #[allow(dead_code)]
    /// Optional paste handler. Return true if the view modified its state and
    /// needs a redraw.
    fn handle_paste(&mut self, _pasted: String) -> bool {
        false
    }

    #[allow(dead_code)]
    /// Flush any pending paste-burst state. Return true if state changed.
    ///
    /// This lets a modal that reuses `ChatComposer` participate in the same
    /// time-based paste burst flushing as the primary composer.
    fn flush_paste_burst_if_due(&mut self) -> bool {
        false
    }

    /// Whether the view is currently holding paste-burst transient state.
    ///
    /// When `true`, the bottom pane will schedule a short delayed redraw to
    /// give the burst time window a chance to flush.
    fn is_in_paste_burst(&self) -> bool {
        false
    }
}
