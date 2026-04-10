use clawcr_core::{ItemId, SessionId, TurnId};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::session::{SessionRuntimeStatus, SessionSummary};
use crate::turn::TurnSummary;

/// Carries the common correlation metadata attached to streamed events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventContext {
    /// The session associated with the event.
    pub session_id: SessionId,
    /// The turn associated with the event, when one exists.
    pub turn_id: Option<TurnId>,
    /// The item associated with the event, when one exists.
    pub item_id: Option<ItemId>,
    /// The per-connection monotonic event sequence number.
    pub seq: u64,
}

/// Carries one typed item envelope projected onto the event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemEnvelope {
    /// The stable item identifier.
    pub item_id: ItemId,
    /// The explicit item kind tag.
    pub item_kind: ItemKind,
    /// The item payload content.
    pub payload: serde_json::Value,
}

/// Carries the payload for a streamed item event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemEventPayload {
    /// The event correlation context.
    pub context: EventContext,
    /// The authoritative item envelope.
    pub item: ItemEnvelope,
}

/// Carries one item delta payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemDeltaPayload {
    /// The event correlation context.
    pub context: EventContext,
    /// The streamed delta fragment.
    pub delta: String,
    /// The optional grouping index for multi-stream items.
    pub stream_index: Option<u32>,
    /// The optional stream channel name.
    pub channel: Option<String>,
}

/// Carries one turn event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEventPayload {
    /// The session associated with the event.
    pub session_id: SessionId,
    /// The full turn summary visible to the client.
    pub turn: TurnSummary,
}

/// Carries a live turn usage update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsageUpdatedPayload {
    /// The session associated with the event.
    pub session_id: SessionId,
    /// The active turn identifier.
    pub turn_id: TurnId,
    /// The current turn usage snapshot.
    pub usage: clawcr_core::TurnUsage,
    /// The live session input token total.
    pub total_input_tokens: usize,
    /// The live session output token total.
    pub total_output_tokens: usize,
}

/// Carries one session event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventPayload {
    /// The full session summary visible to the client.
    pub session: SessionSummary,
}

/// Carries one session-status change payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatusChangedPayload {
    /// The session whose status changed.
    pub session_id: SessionId,
    /// The new runtime status.
    pub status: SessionRuntimeStatus,
}

/// Carries one server-request resolution payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRequestResolvedPayload {
    /// The session associated with the request.
    pub session_id: SessionId,
    /// The resolved or cleared request identifier.
    pub request_id: SmolStr,
    /// The associated turn, when one exists.
    pub turn_id: Option<TurnId>,
}

/// Enumerates the explicit item kinds exposed by the wire protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A user-message item.
    UserMessage,
    /// An agent-message item.
    AgentMessage,
    /// A reasoning item.
    Reasoning,
    /// A plan item.
    Plan,
    /// A tool-call item.
    ToolCall,
    /// A tool-result item.
    ToolResult,
    /// A command-execution item.
    CommandExecution,
    /// A file-change item.
    FileChange,
    /// An MCP tool-call item.
    McpToolCall,
    /// A web-search item.
    WebSearch,
    /// An image-view item.
    ImageView,
    /// A context-compaction item.
    ContextCompaction,
    /// An approval-request item.
    ApprovalRequest,
    /// An approval-decision item.
    ApprovalDecision,
}

/// Enumerates the item-specific delta kinds required by the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemDeltaKind {
    /// Assistant message text delta.
    AgentMessageDelta,
    /// Reasoning summary-text delta.
    ReasoningSummaryTextDelta,
    /// Raw reasoning text delta.
    ReasoningTextDelta,
    /// Command output delta.
    CommandExecutionOutputDelta,
    /// File-change output delta.
    FileChangeOutputDelta,
    /// Plan text delta.
    PlanDelta,
}

/// Enumerates the server-initiated request families defined by the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRequestKind {
    /// Request approval for a command-execution item.
    ItemCommandExecutionRequestApproval,
    /// Request approval for a file-change item.
    ItemFileChangeRequestApproval,
    /// Request approval for a generic permissions item.
    ItemPermissionsRequestApproval,
    /// Request structured user input for a tool interaction.
    ItemToolRequestUserInput,
    /// Request MCP elicitation input from the client.
    McpServerElicitationRequest,
}

/// Carries the common metadata for a pending server-initiated request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingServerRequestContext {
    /// The stable server-generated request identifier.
    pub request_id: SmolStr,
    /// The request family.
    pub request_kind: ServerRequestKind,
    /// The session associated with the request.
    pub session_id: SessionId,
    /// The turn associated with the request, when one exists.
    pub turn_id: Option<TurnId>,
    /// The target item identifier, when one exists.
    pub item_id: Option<ItemId>,
}

/// Carries one server-initiated approval request payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequestPayload {
    /// The common metadata for the pending request.
    pub request: PendingServerRequestContext,
    /// The stable approval identifier expected by `approval/respond`.
    pub approval_id: SmolStr,
    /// The concise action summary shown to the user.
    pub action_summary: String,
    /// The justification shown to the user.
    pub justification: String,
}

/// Carries one server-initiated request-user-input payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUserInputPayload {
    /// The common metadata for the pending request.
    pub request: PendingServerRequestContext,
    /// The human-readable input prompt.
    pub prompt: String,
    /// Optional structured schema or UI hints.
    pub schema: Option<serde_json::Value>,
}

/// Enumerates the outbound server events defined by the protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerEvent {
    /// A session was started or introduced.
    SessionStarted(SessionEventPayload),
    /// A session title changed.
    SessionTitleUpdated(SessionEventPayload),
    /// A session status changed.
    SessionStatusChanged(SessionStatusChangedPayload),
    /// A session was archived.
    SessionArchived(SessionEventPayload),
    /// A session was unarchived.
    SessionUnarchived(SessionEventPayload),
    /// A session was closed or unloaded.
    SessionClosed(SessionEventPayload),
    /// A turn started.
    TurnStarted(TurnEventPayload),
    /// A turn completed with its final status.
    TurnCompleted(TurnEventPayload),
    /// A low-latency turn interruption hint.
    TurnInterrupted(TurnEventPayload),
    /// A low-latency turn failure hint.
    TurnFailed(TurnEventPayload),
    /// A turn plan changed.
    TurnPlanUpdated(TurnEventPayload),
    /// A turn diff changed.
    TurnDiffUpdated(TurnEventPayload),
    /// A live turn usage update arrived.
    TurnUsageUpdated(TurnUsageUpdatedPayload),
    /// An item started.
    ItemStarted(ItemEventPayload),
    /// An item completed.
    ItemCompleted(ItemEventPayload),
    /// An item delta was emitted.
    ItemDelta {
        /// The delta kind.
        delta_kind: ItemDeltaKind,
        /// The delta payload.
        payload: ItemDeltaPayload,
    },
    /// A pending server request was resolved or cleared.
    ServerRequestResolved(ServerRequestResolvedPayload),
}
