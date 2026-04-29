use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::conversation::{ItemId, SessionId, SessionTitleState, TurnId, TurnStatus, TurnUsage};
use crate::{SessionContext, TurnContext, TurnKind};

/// Stores persistent metadata for one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// The stable session identifier.
    pub id: SessionId,
    /// The absolute rollout path used for canonical JSONL persistence.
    pub rollout_path: PathBuf,
    /// The timestamp when the session was created.
    pub created_at: DateTime<Utc>,
    /// The timestamp of the most recent update to session metadata.
    pub updated_at: DateTime<Utc>,
    /// The session source kind, such as CLI or API.
    pub source: String,
    /// Optional nickname assigned to a spawned sub-agent session.
    pub agent_nickname: Option<String>,
    /// Optional role assigned to a spawned sub-agent session.
    pub agent_role: Option<String>,
    /// Optional canonical agent path associated with a spawned sub-agent.
    pub agent_path: Option<String>,
    /// The model provider last observed for this session.
    pub model_provider: String,
    /// The latest resolved model slug for the session.
    pub model: Option<String>,
    /// The logical thinking selection used as the default for the next turn.
    pub thinking: Option<String>,
    /// The working directory associated with the session.
    pub cwd: PathBuf,
    /// The CLI version that created the session.
    pub cli_version: String,
    /// The current best-known title for the session.
    pub title: Option<String>,
    /// The lifecycle state for the current session title.
    pub title_state: SessionTitleState,
    /// The active sandbox policy description for the session.
    pub sandbox_policy: String,
    /// The active approval mode description for the session.
    pub approval_mode: String,
    /// The last observed aggregate token count for the session.
    pub tokens_used: i64,
    /// The first user message stored for preview or title derivation.
    pub first_user_message: Option<String>,
    /// The time when the session was archived, if it has been archived.
    pub archived_at: Option<DateTime<Utc>>,
    /// The git commit SHA associated with the session workspace, if known.
    pub git_sha: Option<String>,
    /// The git branch associated with the session workspace, if known.
    pub git_branch: Option<String>,
    /// The git origin URL associated with the session workspace, if known.
    pub git_origin_url: Option<String>,
    /// The parent session identifier when this session was created by forking.
    pub parent_session_id: Option<SessionId>,
    /// The latest locked session context known for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_context: Option<SessionContext>,
    /// The latest turn context snapshot known for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_turn_context: Option<TurnContext>,
    /// The schema version for persisted session metadata.
    pub schema_version: u32,
}

/// Stores persistent metadata for one turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    /// The stable turn identifier.
    pub id: TurnId,
    /// The session that owns this turn.
    pub session_id: SessionId,
    /// The strictly increasing sequence number within the session.
    pub sequence: u32,
    /// The time when the turn started.
    pub started_at: DateTime<Utc>,
    /// The time when the turn reached a terminal state, if it has completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// The current lifecycle status of the turn.
    pub status: TurnStatus,
    /// The kind of turn (Regular, Review, ManualCompaction, etc.).
    #[serde(default)]
    pub kind: TurnKind,
    /// The logical model selection used for the turn.
    pub model: String,
    /// The logical thinking selection used for the turn.
    pub thinking: Option<String>,
    /// The concrete request model used to execute the turn.
    pub request_model: String,
    /// The concrete request thinking parameter used to execute the turn.
    pub request_thinking: Option<String>,
    /// The estimated input-token count at turn start, when available.
    pub input_token_estimate: Option<u32>,
    /// The authoritative provider token usage, when available.
    pub usage: Option<TurnUsage>,
    /// The locked session context used to build the stable request prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_context: Option<SessionContext>,
    /// The turn context used for this user-visible turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_context: Option<TurnContext>,
    /// The schema version for persisted turn metadata.
    pub schema_version: u32,
}

/// Carries a simple text payload for lightweight item kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextItem {
    /// The textual payload for the item.
    pub text: String,
}

/// Stores one tool-call request as a persisted item payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallItem {
    /// The stable identifier of the tool call.
    pub tool_call_id: String,
    /// The stable runtime name of the requested tool.
    pub tool_name: String,
    /// The validated JSON input passed to the tool.
    pub input: serde_json::Value,
}

/// Stores one tool-progress update as a persisted item payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolProgressItem {
    /// The tool call this progress record belongs to.
    pub tool_call_id: String,
    /// The human-readable progress message.
    pub message: String,
}

/// Stores one terminal tool result as a persisted item payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultItem {
    /// The tool call this result belongs to.
    pub tool_call_id: String,
    /// The runtime tool name when it is available at result time.
    pub tool_name: Option<String>,
    /// The normalized structured output returned by the tool.
    pub output: serde_json::Value,
    /// Whether the result represents an error outcome.
    pub is_error: bool,
}

/// Stores one approval request as a persisted item payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequestItem {
    /// The stable approval request identifier.
    pub approval_id: String,
    /// A concise summary of the action awaiting approval.
    pub action_summary: String,
    /// The justification shown to the user.
    pub justification: String,
}

/// Stores one approval decision as a persisted item payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalDecisionItem {
    /// The approval request identifier this decision answers.
    pub approval_id: String,
    /// The decision taken by the user or policy.
    pub decision: String,
    /// The scope attached to the decision.
    pub scope: String,
}

/// Enumerates the canonical persisted item kinds used by the conversation model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnItem {
    /// A normal user message.
    UserMessage(TextItem),
    /// Same-turn steering input appended while a turn is active.
    SteerInput(TextItem),
    /// A hook or system-generated prompt fragment.
    HookPrompt(TextItem),
    /// A user-visible assistant message.
    AgentMessage(TextItem),
    /// A planning item emitted by the runtime or model.
    Plan(TextItem),
    /// A reasoning summary or reasoning payload item.
    Reasoning(TextItem),
    /// A tool-call item.
    ToolCall(ToolCallItem),
    /// A tool-progress item.
    ToolProgress(ToolProgressItem),
    /// A terminal tool-result item.
    ToolResult(ToolResultItem),
    /// An approval-request item.
    ApprovalRequest(ApprovalRequestItem),
    /// An approval-decision item.
    ApprovalDecision(ApprovalDecisionItem),
    /// A web-search item.
    WebSearch(TextItem),
    /// An image-generation item.
    ImageGeneration(TextItem),
    /// A context-compaction summary item.
    ContextCompaction(TextItem),
}

/// Stores optional high-level progress notes for an item append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worklog {
    /// A human-readable summary of the work performed.
    pub summary: String,
}

/// Stores a normalized turn-scoped error payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnError {
    /// The stable machine-readable error code.
    pub code: String,
    /// The human-readable error message.
    pub message: String,
}

/// Stores one persisted item record in the canonical journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemRecord {
    /// The stable item identifier.
    pub id: ItemId,
    /// The owning session identifier.
    pub session_id: SessionId,
    /// The owning turn identifier.
    pub turn_id: TurnId,
    /// The strictly increasing per-session item sequence number.
    pub seq: u64,
    /// The timestamp when the item record was persisted.
    pub timestamp: DateTime<Utc>,
    /// Optional attempt-placement metadata used by higher-level orchestration.
    pub attempt_placement: Option<i64>,
    /// The turn status observed when this item was appended.
    pub turn_status: Option<TurnStatus>,
    /// Additional related turn identifiers when an item spans turn boundaries.
    pub sibling_turn_ids: Vec<TurnId>,
    /// Input-side item payloads captured for this record.
    pub input_items: Vec<TurnItem>,
    /// Output-side item payloads captured for this record.
    pub output_items: Vec<TurnItem>,
    /// Optional worklog metadata associated with the item.
    pub worklog: Option<Worklog>,
    /// Optional terminal error metadata associated with the item.
    pub error: Option<TurnError>,
    /// The schema version for persisted item records.
    pub schema_version: u32,
}

/// Stores the first canonical line written for a session rollout file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMetaLine {
    /// The time when this rollout line was persisted.
    pub timestamp: DateTime<Utc>,
    /// The session metadata payload carried by the line.
    pub session: SessionRecord,
}

/// Stores one turn metadata line in the rollout file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnLine {
    /// The time when this rollout line was persisted.
    pub timestamp: DateTime<Utc>,
    /// The turn metadata payload carried by the line.
    pub turn: TurnRecord,
}

/// Stores one item-record line in the rollout file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemLine {
    /// The time when this rollout line was persisted.
    pub timestamp: DateTime<Utc>,
    /// The item-record payload carried by the line.
    pub item: ItemRecord,
}

/// Stores one session-title update line in the rollout file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitleUpdatedLine {
    /// The time when this rollout line was persisted.
    pub timestamp: DateTime<Utc>,
    /// The session whose title changed.
    pub session_id: SessionId,
    /// The new title value.
    pub title: String,
    /// The new title lifecycle state.
    pub title_state: SessionTitleState,
    /// The previous title value, when there was one.
    pub previous_title: Option<String>,
}

/// Stores one compaction snapshot reference in the rollout file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionSnapshotLine {
    /// The time when this rollout line was persisted.
    pub timestamp: DateTime<Utc>,
    /// The session that owns the compaction snapshot.
    pub session_id: SessionId,
    /// The turn during which compaction occurred.
    pub turn_id: TurnId,
    /// The summary item that represents the compacted history.
    pub summary_item_id: ItemId,
    /// The pre-existing item ids that remain after compaction, in prompt order.
    pub preserved_item_ids: Vec<ItemId>,
}

/// Enumerates every canonical line type written to the rollout journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RolloutLine {
    /// Session metadata line.
    SessionMeta(Box<SessionMetaLine>),
    /// Turn metadata line.
    Turn(Box<TurnLine>),
    /// Item record line.
    Item(ItemLine),
    /// Session-title update line.
    SessionTitleUpdated(SessionTitleUpdatedLine),
    /// Compaction snapshot line.
    CompactionSnapshot(Box<CompactionSnapshotLine>),
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use pretty_assertions::assert_eq;

    use super::{
        ItemRecord, RolloutLine, SessionRecord, TextItem, ToolCallItem, TurnItem, TurnLine,
        TurnRecord,
    };
    use crate::conversation::{ItemId, SessionId, SessionTitleState, TurnId, TurnStatus};

    #[test]
    fn session_record_supports_unset_title() {
        let session = SessionRecord {
            id: SessionId::new(),
            rollout_path: "rollout.jsonl".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            source: "cli".into(),
            agent_nickname: None,
            agent_role: None,
            agent_path: None,
            model_provider: "test".into(),
            model: None,
            thinking: None,
            cwd: ".".into(),
            cli_version: "0.1.0".into(),
            title: None,
            title_state: SessionTitleState::Unset,
            sandbox_policy: "workspace-write".into(),
            approval_mode: "on-request".into(),
            tokens_used: 0,
            first_user_message: None,
            archived_at: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
            parent_session_id: None,
            session_context: None,
            latest_turn_context: None,
            schema_version: 2,
        };

        assert!(session.title.is_none());
        assert_eq!(session.title_state, SessionTitleState::Unset);
    }

    #[test]
    fn item_record_carries_turn_and_session_identity() {
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let item = ItemRecord {
            id: ItemId::new(),
            session_id,
            turn_id,
            seq: 1,
            timestamp: Utc::now(),
            attempt_placement: None,
            turn_status: Some(TurnStatus::Running),
            sibling_turn_ids: Vec::new(),
            input_items: vec![TurnItem::ToolCall(ToolCallItem {
                tool_call_id: "call-1".into(),
                tool_name: "shell_command".into(),
                input: serde_json::json!({"command":"pwd"}),
            })],
            output_items: vec![TurnItem::AgentMessage(TextItem {
                text: "running".into(),
            })],
            worklog: None,
            error: None,
            schema_version: 1,
        };

        assert_eq!(item.session_id, session_id);
        assert_eq!(item.turn_id, turn_id);
    }

    #[test]
    fn rollout_line_roundtrip() {
        let line = RolloutLine::Turn(Box::new(TurnLine {
            timestamp: Utc::now(),
            turn: TurnRecord {
                id: TurnId::new(),
                session_id: SessionId::new(),
                sequence: 1,
                started_at: Utc::now(),
                completed_at: None,
                status: TurnStatus::Pending,
                kind: crate::TurnKind::Regular,
                model: "test-model".into(),
                thinking: None,
                request_model: "test-model".into(),
                request_thinking: None,
                input_token_estimate: Some(42),
                usage: None,
                session_context: None,
                turn_context: None,
                schema_version: 2,
            },
        }));

        let json = serde_json::to_string(&line).expect("serialize");
        let restored: RolloutLine = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(line, restored);
    }
}
