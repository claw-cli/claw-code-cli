use ratatui::prelude::Stylize;
use ratatui::text::Span;
use std::time::Instant;

/// Spinner frames for the unicode spinner animation.
pub(super) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Spinner animation interval in milliseconds.
pub(super) const SPINNER_INTERVAL_MS: u128 = 80;

/// Returns a unicode spinner frame based on elapsed time.
pub(super) fn spinner_frame(start_time: Option<Instant>) -> &'static str {
    let elapsed = start_time.map(|st| st.elapsed()).unwrap_or_default();
    let frame_index = (elapsed.as_millis() / SPINNER_INTERVAL_MS) as usize;
    SPINNER_FRAMES[frame_index % SPINNER_FRAMES.len()]
}

/// Renders a unicode spinner with shimmer effect for true-color terminals.
pub(super) fn unicode_spinner(start_time: Option<Instant>) -> Span<'static> {
    let frame = spinner_frame(start_time);

    if supports_color::on_cached(supports_color::Stream::Stdout)
        .map(|level| level.has_16m)
        .unwrap_or(false)
    {
        let shimmer = crate::shimmer::shimmer_spans(frame);
        shimmer[0].clone()
    } else {
        Span::from(frame).dim()
    }
}
