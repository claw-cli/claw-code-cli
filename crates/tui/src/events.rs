use ratatui::style::Color;

/// One event emitted by the background query worker into the interactive UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerEvent {
    /// A new assistant turn has started.
    TurnStarted,
    /// Incremental assistant text.
    TextDelta(String),
    /// A tool call started.
    ToolCall { name: String },
    /// A tool call finished.
    ToolResult { content: String, is_error: bool },
    /// The current turn reported token usage.
    Usage {
        /// Input tokens used by the current turn.
        input_tokens: usize,
        /// Output tokens used by the current turn.
        output_tokens: usize,
    },
    /// The current turn completed successfully.
    TurnFinished {
        /// Human-readable stop reason.
        stop_reason: String,
        /// Total turns completed in the session.
        turn_count: usize,
        /// Total input tokens accumulated in the session.
        total_input_tokens: usize,
        /// Total output tokens accumulated in the session.
        total_output_tokens: usize,
    },
    /// The current turn failed.
    TurnFailed {
        /// Human-readable error text to surface in the transcript and status bar.
        message: String,
        /// Total turns completed in the session so far.
        turn_count: usize,
        /// Total input tokens accumulated in the session.
        total_input_tokens: usize,
        /// Total output tokens accumulated in the session.
        total_output_tokens: usize,
    },
}

/// One rendered transcript item shown in the history pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptItem {
    /// Stable kind used for styling and incremental updates.
    pub kind: TranscriptItemKind,
    /// Short title rendered above or before the body.
    pub title: String,
    /// Main text body for the transcript item.
    pub body: String,
}

impl TranscriptItem {
    /// Creates a new transcript item with the supplied title and body.
    pub(crate) fn new(
        kind: TranscriptItemKind,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            title: title.into(),
            body: body.into(),
        }
    }
}

/// Visual category for one transcript item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptItemKind {
    /// User-authored prompt text.
    User,
    /// Assistant-authored text.
    Assistant,
    /// Tool execution start marker.
    ToolCall,
    /// Successful tool result.
    ToolResult,
    /// Failed tool result or runtime error.
    Error,
}

impl TranscriptItemKind {
    /// Returns the accent color used for the item title.
    pub(crate) fn accent(self) -> Color {
        match self {
            TranscriptItemKind::User => Color::Cyan,
            TranscriptItemKind::Assistant => Color::Green,
            TranscriptItemKind::ToolCall => Color::Yellow,
            TranscriptItemKind::ToolResult => Color::Blue,
            TranscriptItemKind::Error => Color::Red,
        }
    }
}
