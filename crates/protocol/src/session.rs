use std::path::PathBuf;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use crate::ReasoningEffort;
use crate::SessionId;
use crate::SessionTitleState;
use crate::turn::TurnMetadata;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRuntimeStatus {
    Idle,
    ActiveTurn,
    WaitingClient,
    Archived,
    Unloaded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub title: Option<String>,
    pub title_state: SessionTitleState,
    pub ephemeral: bool,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_cache_creation_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub prompt_token_estimate: usize,
    pub context_window_tokens_used: usize,
    pub status: SessionRuntimeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStartParams {
    pub cwd: PathBuf,
    pub ephemeral: bool,
    pub title: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStartResult {
    pub session: SessionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResumeParams {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResumeResult {
    pub session: SessionMetadata,
    pub latest_turn: Option<TurnMetadata>,
    pub loaded_item_count: u64,
    pub history_items: Vec<SessionHistoryItem>,
    /// Pending turn input texts queued for the next turn.
    pub pending_texts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionHistoryItemKind {
    User,
    Assistant,
    Reasoning,
    ToolCall,
    ToolResult,
    Error,
    TurnSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHistoryItem {
    pub tool_call_id: Option<String>,
    pub kind: SessionHistoryItemKind,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

impl SessionHistoryItem {
    pub fn new(
        tool_call_id: Option<String>,
        kind: SessionHistoryItemKind,
        title: String,
        body: String,
    ) -> Self {
        Self {
            tool_call_id,
            kind,
            title,
            body,
            duration_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitleUpdateParams {
    pub session_id: SessionId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTitleUpdateResult {
    pub session: SessionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadataUpdateParams {
    pub session_id: SessionId,
    pub model: Option<String>,
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadataUpdateResult {
    pub session: SessionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCompactParams {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCompactResult {
    pub session: SessionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionForkParams {
    pub session_id: SessionId,
    pub title: Option<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_turn_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionForkResult {
    pub session: SessionMetadata,
    pub forked_from_session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRollbackParams {
    pub session_id: SessionId,
    pub user_turn_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRollbackResult {
    pub session: SessionMetadata,
    pub latest_turn: Option<TurnMetadata>,
    pub loaded_item_count: u64,
    pub history_items: Vec<SessionHistoryItem>,
    pub pending_texts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::SessionTitleState;

    #[test]
    fn session_metadata_roundtrips_with_model_and_thinking() {
        let metadata = SessionMetadata {
            session_id: SessionId::new(),
            cwd: "/tmp".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title: Some("Test".to_string()),
            title_state: SessionTitleState::Unset,
            ephemeral: false,
            model: Some("test-model".to_string()),
            thinking: Some("medium".to_string()),
            reasoning_effort: Some(crate::ReasoningEffort::Medium),
            total_input_tokens: 12,
            total_output_tokens: 34,
            total_cache_creation_tokens: 5,
            total_cache_read_tokens: 7,
            prompt_token_estimate: 21,
            context_window_tokens_used: 21,
            status: SessionRuntimeStatus::Idle,
        };

        let json = serde_json::to_string(&metadata).expect("serialize");
        let restored: SessionMetadata = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, metadata);
    }
}
