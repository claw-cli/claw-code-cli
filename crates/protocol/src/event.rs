use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::session::{SessionMetadata, SessionRuntimeStatus};
use crate::turn::TurnMetadata;
use crate::{ItemId, SessionId, TurnId, TurnUsage};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventContext {
    pub session_id: SessionId,
    pub turn_id: Option<TurnId>,
    pub item_id: Option<ItemId>,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemEnvelope {
    pub item_id: ItemId,
    pub item_kind: ItemKind,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallPayload {
    pub tool_call_id: String,
    pub tool_name: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultPayload {
    pub tool_call_id: String,
    pub tool_name: Option<String>,
    pub content: serde_json::Value,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemEventPayload {
    pub context: EventContext,
    pub item: ItemEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemDeltaPayload {
    pub context: EventContext,
    pub delta: String,
    pub stream_index: Option<u32>,
    pub channel: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnEventPayload {
    pub session_id: SessionId,
    pub turn: TurnMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsageUpdatedPayload {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub usage: TurnUsage,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEventPayload {
    pub session: SessionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatusChangedPayload {
    pub session_id: SessionId,
    pub status: SessionRuntimeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCompactionFailedPayload {
    pub session_id: SessionId,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRequestResolvedPayload {
    pub session_id: SessionId,
    pub request_id: SmolStr,
    pub turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputQueueUpdatedPayload {
    pub session_id: SessionId,
    pub pending_count: usize,
    pub pending_texts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerAcceptedPayload {
    pub session_id: SessionId,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    UserMessage,
    AgentMessage,
    Reasoning,
    Plan,
    ToolCall,
    ToolResult,
    CommandExecution,
    FileChange,
    McpToolCall,
    WebSearch,
    ImageView,
    ContextCompaction,
    ApprovalRequest,
    ApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemDeltaKind {
    AgentMessageDelta,
    ReasoningSummaryTextDelta,
    ReasoningTextDelta,
    CommandExecutionOutputDelta,
    FileChangeOutputDelta,
    PlanDelta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRequestKind {
    ItemCommandExecutionRequestApproval,
    ItemFileChangeRequestApproval,
    ItemPermissionsRequestApproval,
    ItemToolRequestUserInput,
    McpServerElicitationRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingServerRequestContext {
    pub request_id: SmolStr,
    pub request_kind: ServerRequestKind,
    pub session_id: SessionId,
    pub turn_id: Option<TurnId>,
    pub item_id: Option<ItemId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequestPayload {
    pub request: PendingServerRequestContext,
    pub approval_id: SmolStr,
    pub action_summary: String,
    pub justification: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUserInputPayload {
    pub request: PendingServerRequestContext,
    pub prompt: String,
    pub schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerEvent {
    SessionStarted(SessionEventPayload),
    SessionTitleUpdated(SessionEventPayload),
    SessionCompactionStarted(SessionEventPayload),
    SessionCompactionCompleted(SessionEventPayload),
    SessionCompactionFailed(SessionCompactionFailedPayload),
    SessionStatusChanged(SessionStatusChangedPayload),
    SessionArchived(SessionEventPayload),
    SessionUnarchived(SessionEventPayload),
    SessionClosed(SessionEventPayload),
    TurnStarted(TurnEventPayload),
    TurnCompleted(TurnEventPayload),
    TurnInterrupted(TurnEventPayload),
    TurnFailed(TurnEventPayload),
    TurnPlanUpdated(TurnEventPayload),
    TurnDiffUpdated(TurnEventPayload),
    TurnUsageUpdated(TurnUsageUpdatedPayload),
    InputQueueUpdated(InputQueueUpdatedPayload),
    SteerAccepted(SteerAcceptedPayload),
    ItemStarted(ItemEventPayload),
    ItemCompleted(ItemEventPayload),
    ItemDelta {
        delta_kind: ItemDeltaKind,
        payload: ItemDeltaPayload,
    },
    ServerRequestResolved(ServerRequestResolvedPayload),
}

impl ServerEvent {
    pub fn session_id(&self) -> Option<SessionId> {
        match self {
            Self::SessionStarted(payload)
            | Self::SessionTitleUpdated(payload)
            | Self::SessionCompactionStarted(payload)
            | Self::SessionCompactionCompleted(payload)
            | Self::SessionArchived(payload)
            | Self::SessionUnarchived(payload)
            | Self::SessionClosed(payload) => Some(payload.session.session_id),
            Self::SessionCompactionFailed(payload) => Some(payload.session_id),
            Self::SessionStatusChanged(payload) => Some(payload.session_id),
            Self::TurnStarted(payload)
            | Self::TurnCompleted(payload)
            | Self::TurnInterrupted(payload)
            | Self::TurnFailed(payload)
            | Self::TurnPlanUpdated(payload)
            | Self::TurnDiffUpdated(payload) => Some(payload.session_id),
            Self::TurnUsageUpdated(payload) => Some(payload.session_id),
            Self::InputQueueUpdated(payload) => Some(payload.session_id),
            Self::SteerAccepted(payload) => Some(payload.session_id),
            Self::ItemStarted(payload) | Self::ItemCompleted(payload) => {
                Some(payload.context.session_id)
            }
            Self::ItemDelta { payload, .. } => Some(payload.context.session_id),
            Self::ServerRequestResolved(payload) => Some(payload.session_id),
        }
    }

    pub fn method_name(&self) -> &'static str {
        match self {
            Self::SessionStarted(_) => "session/started",
            Self::SessionTitleUpdated(_) => "session/title/updated",
            Self::SessionCompactionStarted(_) => "session/compaction/started",
            Self::SessionCompactionCompleted(_) => "session/compaction/completed",
            Self::SessionCompactionFailed(_) => "session/compaction/failed",
            Self::SessionStatusChanged(_) => "session/status/changed",
            Self::SessionArchived(_) => "session/archived",
            Self::SessionUnarchived(_) => "session/unarchived",
            Self::SessionClosed(_) => "session/closed",
            Self::TurnStarted(_) => "turn/started",
            Self::TurnCompleted(_) => "turn/completed",
            Self::TurnInterrupted(_) => "turn/interrupted",
            Self::TurnFailed(_) => "turn/failed",
            Self::TurnPlanUpdated(_) => "turn/plan/updated",
            Self::TurnDiffUpdated(_) => "turn/diff/updated",
            Self::TurnUsageUpdated(_) => "turn/usage/updated",
            Self::InputQueueUpdated(_) => "inputQueue/updated",
            Self::SteerAccepted(_) => "steer/accepted",
            Self::ItemStarted(_) => "item/started",
            Self::ItemCompleted(_) => "item/completed",
            Self::ItemDelta { delta_kind, .. } => match delta_kind {
                ItemDeltaKind::AgentMessageDelta => "item/agentMessage/delta",
                ItemDeltaKind::ReasoningSummaryTextDelta => "item/reasoning/summaryTextDelta",
                ItemDeltaKind::ReasoningTextDelta => "item/reasoning/textDelta",
                ItemDeltaKind::CommandExecutionOutputDelta => "item/commandExecution/outputDelta",
                ItemDeltaKind::FileChangeOutputDelta => "item/fileChange/outputDelta",
                ItemDeltaKind::PlanDelta => "item/plan/delta",
            },
            Self::ServerRequestResolved(_) => "serverRequest/resolved",
        }
    }

    pub fn with_seq(mut self, seq: u64) -> Self {
        match &mut self {
            Self::ItemStarted(payload) | Self::ItemCompleted(payload) => {
                payload.context.seq = seq;
            }
            Self::ItemDelta { payload, .. } => payload.context.seq = seq,
            Self::TurnUsageUpdated(_) | Self::InputQueueUpdated(_) | Self::SteerAccepted(_) => {}
            _ => {}
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn input_queue_updated_event_roundtrips() {
        let payload = InputQueueUpdatedPayload {
            session_id: SessionId::new(),
            pending_count: 3,
            pending_texts: vec!["first".into(), "second".into()],
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        let restored: InputQueueUpdatedPayload = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.pending_count, 3);
        assert_eq!(restored.pending_texts, vec!["first", "second"]);
    }

    #[test]
    fn steer_accepted_event_roundtrips() {
        let turn_id = TurnId::new();
        let payload = SteerAcceptedPayload {
            session_id: SessionId::new(),
            turn_id,
        };
        let json = serde_json::to_string(&payload).expect("serialize");
        let restored: SteerAcceptedPayload = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.turn_id, turn_id);
    }

    #[test]
    fn server_event_input_queue_updated_method_name() {
        let event = ServerEvent::InputQueueUpdated(InputQueueUpdatedPayload {
            session_id: SessionId::new(),
            pending_count: 0,
            pending_texts: vec![],
        });
        assert_eq!(event.method_name(), "inputQueue/updated");
        assert!(event.session_id().is_some());
    }

    #[test]
    fn server_event_steer_accepted_method_name() {
        let event = ServerEvent::SteerAccepted(SteerAcceptedPayload {
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
        });
        assert_eq!(event.method_name(), "steer/accepted");
        assert!(event.session_id().is_some());
    }
}
