use std::collections::VecDeque;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{ItemId, SessionId, TurnId, TurnStatus, TurnUsage};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMetadata {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub sequence: u32,
    pub status: TurnStatus,
    pub kind: TurnKind,
    pub model: String,
    pub thinking: Option<String>,
    pub request_model: String,
    pub request_thinking: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub usage: Option<TurnUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    Text { text: String },
    Skill { id: String },
    LocalImage { path: PathBuf },
    Mention { path: String, name: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartParams {
    pub session_id: SessionId,
    pub input: Vec<InputItem>,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub sandbox: Option<String>,
    pub approval_policy: Option<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnStartResult {
    pub turn_id: TurnId,
    pub status: TurnStatus,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnInterruptParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnInterruptResult {
    pub turn_id: TurnId,
    pub status: TurnStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSteerParams {
    pub session_id: SessionId,
    pub expected_turn_id: TurnId,
    pub input: Vec<InputItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSteerResult {
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    Regular,
    Review,
    ManualCompaction,
    Other(String),
}

impl Default for TurnKind {
    fn default() -> Self {
        Self::Regular
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteerInputRecord {
    pub item_id: ItemId,
    pub received_at: DateTime<Utc>,
    pub input: Vec<InputItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTurnSteeringState {
    pub turn_id: TurnId,
    pub turn_kind: TurnKind,
    pub pending_inputs: VecDeque<SteerInputRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInputItem {
    pub kind: PendingInputKind,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PendingInputKind {
    UserText { text: String },
    ToolCallBlockedByHook { tool_use_id: String, reason: String },
    BudgetLimitSteering,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn turn_metadata_roundtrips_with_logical_and_request_fields() {
        let metadata = TurnMetadata {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            sequence: 1,
            status: TurnStatus::Completed,
            kind: TurnKind::Regular,
            model: "logical-model".to_string(),
            thinking: Some("high".to_string()),
            request_model: "provider-model".to_string(),
            request_thinking: Some("medium".to_string()),
            started_at: Utc::now(),
            completed_at: Some(Utc::now()),
            usage: Some(TurnUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            }),
        };

        let json = serde_json::to_string(&metadata).expect("serialize");
        let restored: TurnMetadata = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored, metadata);
    }

    #[test]
    fn pending_input_item_user_text_roundtrips() {
        let item = PendingInputItem {
            kind: PendingInputKind::UserText {
                text: "hello".into(),
            },
            metadata: Some(serde_json::json!({"source": "tui"})),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&item).expect("serialize");
        let restored: PendingInputItem = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(item.created_at, restored.created_at);
        assert_eq!(item.metadata, restored.metadata);
        assert_eq!(format!("{:?}", item.kind), format!("{:?}", restored.kind));
    }

    #[test]
    fn pending_input_item_tool_call_blocked_roundtrips() {
        let item = PendingInputItem {
            kind: PendingInputKind::ToolCallBlockedByHook {
                tool_use_id: "tool-1".into(),
                reason: "blocked by safety".into(),
            },
            metadata: None,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&item).expect("serialize");
        let restored: PendingInputItem = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(item.created_at, restored.created_at);
    }

    #[test]
    fn pending_input_item_budget_limit_steering_roundtrips() {
        let item = PendingInputItem {
            kind: PendingInputKind::BudgetLimitSteering,
            metadata: None,
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&item).expect("serialize");
        let restored: PendingInputItem = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(
            restored.kind,
            PendingInputKind::BudgetLimitSteering
        ));
    }

    #[test]
    fn pending_input_kind_serializes_tagged_shape() {
        let json = serde_json::json!({"type": "user_text", "text": "hello"});
        let kind: PendingInputKind = serde_json::from_value(json).expect("deserialize");
        assert!(matches!(kind, PendingInputKind::UserText { .. }));
    }

    #[test]
    fn turn_kind_default_is_regular() {
        assert_eq!(TurnKind::default(), TurnKind::Regular);
    }
}
