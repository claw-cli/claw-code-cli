use crate::history_cell::HistoryCell;
use crate::history_cell::{self};
use ratatui::style::Stylize;
use ratatui::text::Line;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use super::StreamState;

/// Controller that manages newline-gated streaming, header emission, and
/// commit animation across streams.
pub(crate) struct StreamController {
    state: StreamState,
    finishing_after_drain: bool,
    header_emitted: bool,
}

impl StreamController {
    /// Create a controller whose markdown renderer shortens local file links relative to `cwd`.
    ///
    /// The controller snapshots the path into stream state so later commit ticks and finalization
    /// render against the same session cwd that was active when streaming started.
    pub(crate) fn new(width: Option<usize>, cwd: &Path) -> Self {
        Self {
            state: StreamState::new(width, cwd),
            finishing_after_drain: false,
            header_emitted: false,
        }
    }

    /// Push a delta; if it contains a newline, commit completed lines and start animation.
    pub(crate) fn push(&mut self, delta: &str) -> bool {
        let state = &mut self.state;
        if !delta.is_empty() {
            state.has_seen_delta = true;
        }
        state.collector.push_delta(delta);
        if delta.contains('\n') {
            let newly_completed = state.collector.commit_complete_lines();
            if !newly_completed.is_empty() {
                state.enqueue(newly_completed);
                return true;
            }
        }
        false
    }

    /// Finalize the active stream. Drain and emit now.
    pub(crate) fn finalize(&mut self) -> Option<Box<dyn HistoryCell>> {
        // Finalize collector first.
        let remaining = {
            let state = &mut self.state;
            state.collector.finalize_and_drain()
        };
        // Collect all output first to avoid emitting headers when there is no content.
        let mut out_lines = Vec::new();
        {
            let state = &mut self.state;
            if !remaining.is_empty() {
                state.enqueue(remaining);
            }
            let step = state.drain_all();
            out_lines.extend(step);
        }

        // Cleanup
        self.state.clear();
        self.finishing_after_drain = false;
        self.emit(out_lines)
    }

    /// Step animation: commit at most one queued line and handle end-of-drain cleanup.
    pub(crate) fn on_commit_tick(&mut self) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.state.step();
        (self.emit(step), self.state.is_idle())
    }

    /// Step animation: commit at most `max_lines` queued lines.
    ///
    /// This is intended for adaptive catch-up drains. Callers should keep `max_lines` bounded; a
    /// very large value can collapse perceived animation into a single jump.
    pub(crate) fn on_commit_tick_batch(
        &mut self,
        max_lines: usize,
    ) -> (Option<Box<dyn HistoryCell>>, bool) {
        let step = self.state.drain_n(max_lines.max(1));
        (self.emit(step), self.state.is_idle())
    }

    /// Returns the current number of queued lines waiting to be displayed.
    pub(crate) fn queued_lines(&self) -> usize {
        self.state.queued_len()
    }

    /// Returns the age of the oldest queued line.
    pub(crate) fn oldest_queued_age(&self, now: Instant) -> Option<Duration> {
        self.state.oldest_queued_age(now)
    }

    /// Render the current uncommitted tail that should remain in the live viewport.
    pub(crate) fn pending_lines(&self) -> Vec<Line<'static>> {
        self.state.collector.pending_lines()
    }

    fn emit(&mut self, lines: Vec<Line<'static>>) -> Option<Box<dyn HistoryCell>> {
        if lines.is_empty() {
            return None;
        }
        let initial_prefix = if self.header_emitted {
            "  ".into()
        } else {
            Line::from("• ".cyan())
        };
        let is_stream_continuation = self.header_emitted;
        self.header_emitted = true;
        Some(Box::new(
            history_cell::AgentMessageCell::new_ai_response_with_prefix(
                lines,
                initial_prefix,
                "  ",
                is_stream_continuation,
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use std::path::PathBuf;

    fn test_cwd() -> PathBuf {
        // These tests only need a stable absolute cwd; using temp_dir() avoids baking Unix- or
        // Windows-specific root semantics into the fixtures.
        std::env::temp_dir()
    }

    fn lines_to_plain_strings(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[test]
    fn first_committed_stream_chunk_uses_blue_dot_prefix() {
        let mut ctrl = StreamController::new(Some(80), &test_cwd());

        ctrl.push("hello\n");
        let (cell, _idle) = ctrl.on_commit_tick();

        let cell = cell.expect("expected committed cell");
        let rendered = cell.display_lines(80);
        let first_line = rendered.first().expect("expected rendered line");
        let first_span = first_line.spans.first().expect("expected prefix span");

        assert_eq!(first_span.content.as_ref(), "• ");
        assert_eq!(first_span.style.fg, Some(Color::Cyan));
    }

    #[tokio::test]
    async fn controller_loose_vs_tight_with_commit_ticks_matches_full() {
        let mut ctrl = StreamController::new(/*width*/ None, &test_cwd());
        let mut lines = Vec::new();

        // Exact deltas from the session log (section: Loose vs. tight list items)
        let deltas = vec![
            "\n\n",
            "Loose",
            " vs",
            ".",
            " tight",
            " list",
            " items",
            ":\n",
            "1",
            ".",
            " Tight",
            " item",
            "\n",
            "2",
            ".",
            " Another",
            " tight",
            " item",
            "\n\n",
            "1",
            ".",
            " Loose",
            " item",
            " with",
            " its",
            " own",
            " paragraph",
            ".\n\n",
            "  ",
            " This",
            " paragraph",
            " belongs",
            " to",
            " the",
            " same",
            " list",
            " item",
            ".\n\n",
            "2",
            ".",
            " Second",
            " loose",
            " item",
            " with",
            " a",
            " nested",
            " list",
            " after",
            " a",
            " blank",
            " line",
            ".\n\n",
            "  ",
            " -",
            " Nested",
            " bullet",
            " under",
            " a",
            " loose",
            " item",
            "\n",
            "  ",
            " -",
            " Another",
            " nested",
            " bullet",
            "\n\n",
        ];

        // Simulate streaming with a commit tick attempt after each delta.
        for d in deltas.iter() {
            ctrl.push(d);
            while let (Some(cell), idle) = ctrl.on_commit_tick() {
                lines.extend(cell.transcript_lines(u16::MAX));
                if idle {
                    break;
                }
            }
        }
        // Finalize and flush remaining lines now.
        if let Some(cell) = ctrl.finalize() {
            lines.extend(cell.transcript_lines(u16::MAX));
        }

        let streamed: Vec<_> = lines_to_plain_strings(&lines)
            .into_iter()
            // skip • and 2-space indentation
            .map(|s| s.chars().skip(2).collect::<String>())
            .collect();

        // Full render of the same source
        let source: String = deltas.iter().copied().collect();
        let mut rendered: Vec<ratatui::text::Line<'static>> = Vec::new();
        let test_cwd = test_cwd();
        crate::markdown::append_markdown(
            &source,
            /*width*/ None,
            Some(test_cwd.as_path()),
            &mut rendered,
        );
        let rendered_strs = lines_to_plain_strings(&rendered);

        assert_eq!(streamed, rendered_strs);

        // Also assert exact expected plain strings for clarity.
        let expected = vec![
            "Loose vs. tight list items:".to_string(),
            "".to_string(),
            "1. Tight item".to_string(),
            "2. Another tight item".to_string(),
            "3. Loose item with its own paragraph.".to_string(),
            "".to_string(),
            "   This paragraph belongs to the same list item.".to_string(),
            "4. Second loose item with a nested list after a blank line.".to_string(),
            "    - Nested bullet under a loose item".to_string(),
            "    - Another nested bullet".to_string(),
        ];
        assert_eq!(
            streamed, expected,
            "expected exact rendered lines for loose/tight section"
        );
    }
}
