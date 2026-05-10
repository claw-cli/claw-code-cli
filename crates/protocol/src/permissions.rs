use serde::Deserialize;
use serde::Serialize;

use crate::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum PermissionPreset {
    ReadOnly,
    #[default]
    Default,
    AutoReview,
    FullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalsReviewer {
    #[default]
    User,
    AutoReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPermissionsUpdateParams {
    pub session_id: SessionId,
    pub preset: PermissionPreset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPermissionsUpdateResult {
    pub session_id: SessionId,
    pub preset: PermissionPreset,
    pub reviewer: ApprovalsReviewer,
}
