use std::time::Instant;

use crate::app_command::InputHistoryDirection;
use devo_core::SessionId;
use devo_protocol::ProviderWireApi;
use devo_protocol::ReasoningEffort;
const TOOL_RESULT_FOLD_FINAL_STAGE: u8 = 3;

/// One persisted session entry shown in the interactive session picker panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionListEntry {
    /// Stable session identifier used when switching the active session.
    pub session_id: SessionId,
    /// Human-readable session title shown to the user.
    pub title: String,
    /// Timestamp summary rendered beside the title for quick scanning.
    pub updated_at: String,
    /// Whether this entry is the currently active session.
    pub is_active: bool,
}

/// One persisted model profile available for switching in the interactive model picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedModelEntry {
    /// Stable model slug or custom model name.
    pub model: String,
    /// Concrete wire protocol stored for this model's provider profile.
    pub wire_api: ProviderWireApi,
    /// Optional provider base URL override stored with the model.
    pub base_url: Option<String>,
    /// Optional API key override stored with the model.
    pub api_key: Option<String>,
}

use devo_protocol::TurnId;

/// One event emitted by the background query worker into the interactive UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WorkerEvent {
    /// A new assistant turn has started.
    TurnStarted {
        /// The model slug resolved by the server for this turn.
        model: String,
        /// The logical thinking selection used for this turn.
        thinking: Option<String>,
        /// The effective reasoning effort observed for this turn.
        reasoning_effort: Option<ReasoningEffort>,
        /// The server-assigned turn identifier.
        turn_id: TurnId,
    },
    /// Input queue state updated by the server.
    InputQueueUpdated {
        pending_count: usize,
        pending_texts: Vec<String>,
    },
    /// A steer (/btw) was accepted by the server.
    SteerAccepted { turn_id: TurnId },
    /// Incremental assistant text.
    TextDelta(String),
    /// Incremental reasoning text.
    ReasoningDelta(String),
    /// Final assistant text for a completed item.
    AssistantMessageCompleted(String),
    /// Final reasoning text for a completed item.
    ReasoningCompleted(String),
    /// A tool call started.
    ToolCall {
        /// Stable identifier used to match the later tool result.
        tool_use_id: String,
        /// Human-readable summary line for the tool execution.
        summary: String,
    },
    /// Incremental output delta from a running tool.
    ToolOutputDelta {
        /// Stable identifier matching the corresponding tool call.
        tool_use_id: String,
        /// Streaming output text chunk.
        delta: String,
    },
    /// A tool call finished.
    ToolResult {
        /// Stable identifier used to match the corresponding tool call.
        tool_use_id: String,
        /// Human-readable title for the tool result when no prior tool-call row is cached.
        title: String,
        /// Human-readable output preview shown in the transcript.
        preview: String,
        /// Whether the tool returned an error.
        is_error: bool,
        /// Whether the preview was truncated for display.
        truncated: bool,
    },
    /// Live usage update for the active turn.
    UsageUpdated {
        /// Total input tokens accumulated in the session.
        total_input_tokens: usize,
        /// Total output tokens accumulated in the session.
        total_output_tokens: usize,
        /// Last completed query token usage, measured as input plus output tokens.
        last_query_total_tokens: usize,
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
        /// Last completed turn token usage, measured as input plus output tokens.
        last_query_total_tokens: usize,
        /// Estimated prompt tokens for the just-completed request.
        prompt_token_estimate: usize,
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
        /// Estimated prompt tokens for the last attempted request.
        prompt_token_estimate: usize,
    },
    /// Provider validation succeeded during onboarding.
    ProviderValidationSucceeded {
        /// Short human-readable confirmation from the probe request.
        reply_preview: String,
    },
    /// Provider validation failed during onboarding.
    ProviderValidationFailed {
        /// Human-readable failure reason from the probe request.
        message: String,
    },
    /// Current known sessions were listed from the server.
    SessionsListed {
        /// Structured sessions rendered into the bottom picker panel.
        sessions: Vec<SessionListEntry>,
    },
    /// Current known skills were listed from the server.
    SkillsListed {
        /// Pre-rendered skill summary shown in the bottom panel.
        body: String,
    },
    /// The interactive client cleared its active session and is waiting for the next prompt.
    NewSessionPrepared {
        /// Working directory for the next newly-created session.
        cwd: std::path::PathBuf,
        /// Model currently configured for the next newly-created session.
        model: String,
        /// Thinking selection currently configured for the next newly-created session.
        thinking: Option<String>,
        /// Effective reasoning effort currently configured for the next session.
        reasoning_effort: Option<ReasoningEffort>,
        /// Last completed turn token usage for the fresh session.
        last_query_total_tokens: usize,
    },
    /// The active session changed.
    SessionSwitched {
        /// The new active session identifier.
        session_id: String,
        /// Working directory restored from the resumed session metadata.
        cwd: std::path::PathBuf,
        /// Optional human-readable session title.
        title: Option<String>,
        /// The model restored from the resumed session, when one exists.
        model: Option<String>,
        /// The thinking selection restored from the resumed session, when one exists.
        thinking: Option<String>,
        /// The effective reasoning effort restored from session context, when one exists.
        reasoning_effort: Option<ReasoningEffort>,
        /// Total input tokens accumulated for the resumed session.
        total_input_tokens: usize,
        /// Total output tokens accumulated for the resumed session.
        total_output_tokens: usize,
        /// Last completed turn token usage, measured as input plus output tokens.
        last_query_total_tokens: usize,
        /// Estimated prompt tokens currently visible to the model.
        prompt_token_estimate: usize,
        /// Replay-friendly transcript items loaded from the resumed session.
        history_items: Vec<TranscriptItem>,
        /// Number of persisted items loaded for the resumed session.
        loaded_item_count: u64,
        /// Pending turn input texts queued for the next turn.
        pending_texts: Vec<String>,
    },
    /// The current session title changed.
    SessionRenamed {
        /// The renamed session identifier.
        session_id: String,
        /// The new session title.
        title: String,
    },
    /// The active session started a proactive compaction request.
    SessionCompactionStarted,
    /// The active session completed a proactive compaction request.
    SessionCompacted {
        /// Total input tokens accumulated in the compacted session.
        total_input_tokens: usize,
        /// Total output tokens accumulated in the compacted session.
        total_output_tokens: usize,
        /// Estimated prompt tokens currently visible to the model.
        prompt_token_estimate: usize,
    },
    /// The active session compaction request failed.
    SessionCompactionFailed {
        /// Human-readable failure reason.
        message: String,
    },
    /// The current session title changed due to automatic or explicit server-side updates.
    SessionTitleUpdated {
        /// The updated session identifier.
        session_id: String,
        /// The new best-known title.
        title: String,
    },
    /// One input-history query completed.
    InputHistoryLoaded {
        /// Which direction was requested.
        direction: InputHistoryDirection,
        /// History entry text, or `None` if there is no matching entry.
        text: Option<String>,
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
    /// Time when the tool output should start folding away.
    pub fold_next_at: Option<Instant>,
    /// Current fold stage for tool outputs.
    pub fold_stage: u8,
    /// Duration of the turn that produced this item (milliseconds), if known.
    pub duration_ms: Option<u64>,
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
            fold_next_at: None,
            fold_stage: 0,
            duration_ms: None,
        }
    }

    /// Creates a compact tool-call transcript item that only keeps the title row.
    pub(crate) fn tool_call(title: impl Into<String>) -> Self {
        Self::new(TranscriptItemKind::ToolCall, title, String::new())
    }

    /// Creates a restored historical tool-result item in its already-compacted state.
    pub(crate) fn restored_tool_result(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(TranscriptItemKind::ToolResult, title, body)
            .with_fold_stage(TOOL_RESULT_FOLD_FINAL_STAGE)
    }

    /// Creates a tool error item that stays expanded because errors should remain visible.
    pub(crate) fn tool_error(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(TranscriptItemKind::Error, title, body)
    }

    /// Forces a specific fold stage without scheduling the animation.
    pub(crate) fn with_fold_stage(mut self, stage: u8) -> Self {
        self.fold_stage = stage;
        self.fold_next_at = None;
        self
    }

    /// Attaches turn duration metadata to this transcript item.
    pub(crate) fn with_duration(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }
}

#[allow(dead_code)]
/// Visual category for one transcript item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptItemKind {
    /// User-authored prompt text.
    User,
    /// Assistant-authored text.
    Assistant,
    /// Model reasoning/thinking text.
    Reasoning,
    /// Tool execution start marker.
    ToolCall,
    /// Successful tool result.
    ToolResult,
    /// Failed tool result or runtime error.
    Error,
    /// Local UI/system note that is not model-authored content.
    System,
    /// Turn summary with model name and duration.
    TurnSummary,
}
