use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{SessionId, TurnId};

/// Describes a client response to a pending approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRespondParams {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub approval_id: SmolStr,
    pub decision: ApprovalDecisionValue,
    pub scope: ApprovalScopeValue,
}

/// Enumerates client decisions for approval requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionValue {
    Approve,
    Deny,
    Cancel,
}

/// Enumerates the scopes supported by approval responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScopeValue {
    Once,
    Turn,
    Session,
    PathPrefix,
    Host,
    Tool,
    CommandPrefix,
}

/// Describes the payload for `events/subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeParams {
    pub session_id: Option<SessionId>,
    pub event_types: Option<Vec<String>>,
}

/// Describes the response returned by `events/subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeResult {
    pub subscription_id: SmolStr,
}
