pub mod legacy_permissions;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// The fixed placeholder inserted when a secret is redacted from model-visible text.
pub const REDACTED_SECRET_PLACEHOLDER: &str = "[REDACTED_SECRET]";

/// Describes the confidence level of one secret detection match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SecretMatchConfidence {
    /// The detector is weakly confident the match is a real secret.
    Low,
    /// The detector is moderately confident the match is a real secret.
    Medium,
    /// The detector is strongly confident the match is a real secret.
    High,
}

/// Describes one secret substring identified by a detector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMatch {
    /// The byte offset where the secret starts.
    pub start: usize,
    /// The byte offset immediately after the secret ends.
    pub end: usize,
    /// The placeholder that should replace the secret.
    pub placeholder: String,
    /// The detector confidence for this match.
    pub confidence: SecretMatchConfidence,
}

/// Provides deterministic secret detection over model-bound text.
pub trait SecretDetector: Send + Sync {
    /// Returns the stable identifier for the detector implementation.
    fn detector_id(&self) -> &'static str;

    /// Returns every secret match detected in the supplied input.
    fn detect(&self, input: &str) -> Vec<SecretMatch>;
}

/// Exposes the active set of secret detectors.
pub trait SecretDetectorRegistry: Send + Sync {
    /// Returns every configured detector.
    fn all(&self) -> Vec<Arc<dyn SecretDetector>>;
}

/// Stores one accepted secret match together with the detector that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedSecretMatch {
    /// The stable detector identifier.
    pub detector_id: String,
    /// The accepted secret match.
    pub matched: SecretMatch,
}

/// Stores the telemetry for one redaction run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RedactionReport {
    /// The accepted matches, in application order.
    pub matches: Vec<AcceptedSecretMatch>,
}

/// Stores the result of applying deterministic redaction to one text fragment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionResult {
    /// The redacted text safe for model visibility.
    pub redacted_text: String,
    /// The redaction telemetry emitted during processing.
    pub report: RedactionReport,
}

/// Regex-based secret detector used by the default detector set.
pub struct RegexSecretDetector {
    /// The stable detector identifier.
    pub detector_id_value: &'static str,
    /// The compiled regex used for detection.
    pub regex: Regex,
    /// The placeholder used to replace matching text.
    pub placeholder: &'static str,
    /// The confidence assigned to each match.
    pub confidence: SecretMatchConfidence,
}

impl SecretDetector for RegexSecretDetector {
    fn detector_id(&self) -> &'static str {
        self.detector_id_value
    }

    fn detect(&self, input: &str) -> Vec<SecretMatch> {
        self.regex
            .find_iter(input)
            .map(|matched| SecretMatch {
                start: matched.start(),
                end: matched.end(),
                placeholder: self.placeholder.to_string(),
                confidence: self.confidence,
            })
            .collect()
    }
}

/// In-memory detector registry used by the runtime and tests.
#[derive(Default)]
pub struct InMemorySecretDetectorRegistry {
    /// The detectors owned by the registry.
    pub detectors: Vec<Arc<dyn SecretDetector>>,
}

impl InMemorySecretDetectorRegistry {
    /// Creates the default registry with the required built-in regex detectors.
    pub fn with_default_detectors() -> Self {
        let detectors: Vec<Arc<dyn SecretDetector>> = vec![
            Arc::new(RegexSecretDetector {
                detector_id_value: "openai_api_key",
                regex: Regex::new(r"sk-[A-Za-z0-9]{20,}").expect("valid regex"),
                placeholder: REDACTED_SECRET_PLACEHOLDER,
                confidence: SecretMatchConfidence::High,
            }),
            Arc::new(RegexSecretDetector {
                detector_id_value: "aws_access_key_id",
                regex: Regex::new(r"\bAKIA[0-9A-Z]{16}\b").expect("valid regex"),
                placeholder: REDACTED_SECRET_PLACEHOLDER,
                confidence: SecretMatchConfidence::High,
            }),
            Arc::new(RegexSecretDetector {
                detector_id_value: "bearer_token",
                regex: Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._\-]{16,}\b").expect("valid regex"),
                placeholder: REDACTED_SECRET_PLACEHOLDER,
                confidence: SecretMatchConfidence::High,
            }),
            Arc::new(RegexSecretDetector {
                detector_id_value: "password_assignment",
                regex: Regex::new(
                    r#"(?i)\b(api[_-]?key|token|secret|password)\b(\s*[:=]\s*)(["']?)[^\s"']{8,}"#,
                )
                .expect("valid regex"),
                placeholder: REDACTED_SECRET_PLACEHOLDER,
                confidence: SecretMatchConfidence::Medium,
            }),
        ];

        Self { detectors }
    }
}

impl SecretDetectorRegistry for InMemorySecretDetectorRegistry {
    fn all(&self) -> Vec<Arc<dyn SecretDetector>> {
        self.detectors.clone()
    }
}

/// Applies deterministic secret redaction using a detector registry.
pub struct SecretRedactor {
    /// The detector registry used during redaction.
    pub registry: Arc<dyn SecretDetectorRegistry>,
}

impl SecretRedactor {
    /// Creates a new secret redactor.
    pub fn new(registry: Arc<dyn SecretDetectorRegistry>) -> Self {
        Self { registry }
    }

    /// Redacts every accepted secret match from one input fragment.
    pub fn redact(&self, input: &str) -> RedactionResult {
        let accepted = self.merge_matches(input);
        let mut redacted = String::with_capacity(input.len());
        let mut cursor = 0usize;

        for accepted_match in &accepted {
            redacted.push_str(&input[cursor..accepted_match.matched.start]);
            redacted.push_str(&accepted_match.matched.placeholder);
            cursor = accepted_match.matched.end;
        }
        redacted.push_str(&input[cursor..]);

        RedactionResult {
            redacted_text: redacted,
            report: RedactionReport { matches: accepted },
        }
    }

    fn merge_matches(&self, input: &str) -> Vec<AcceptedSecretMatch> {
        let mut all_matches = self
            .registry
            .all()
            .into_iter()
            .flat_map(|detector| {
                let detector_id = detector.detector_id().to_string();
                detector
                    .detect(input)
                    .into_iter()
                    .map(move |matched| AcceptedSecretMatch {
                        detector_id: detector_id.clone(),
                        matched,
                    })
            })
            .collect::<Vec<_>>();

        all_matches.sort_by(|left, right| {
            let left_len = left.matched.end.saturating_sub(left.matched.start);
            let right_len = right.matched.end.saturating_sub(right.matched.start);
            right_len
                .cmp(&left_len)
                .then(right.matched.confidence.cmp(&left.matched.confidence))
                .then(left.matched.start.cmp(&right.matched.start))
                .then(left.detector_id.cmp(&right.detector_id))
        });

        let mut occupied = HashSet::new();
        let mut accepted = Vec::new();

        for candidate in all_matches {
            if (candidate.matched.start..candidate.matched.end)
                .any(|index| occupied.contains(&index))
            {
                continue;
            }

            for index in candidate.matched.start..candidate.matched.end {
                occupied.insert(index);
            }
            accepted.push(candidate);
        }

        accepted.sort_by_key(|entry| entry.matched.start);
        accepted
    }
}

/// Controls the top-level safety mode used for access decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafetyPolicyMode {
    /// Allow all operations without deterministic restriction.
    Unrestricted,
    /// Use deterministic static policy only.
    StaticPolicy,
    /// Use a model-guided classifier in addition to deterministic policy.
    ModelGuidedPolicy,
}

/// Selects the model used for model-guided safety policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyModelSelection {
    /// Use the active turn model for policy classification.
    UseTurnModel,
    /// Use a separately configured model slug for policy classification.
    UseConfiguredModel {
        /// The configured model slug used for policy classification.
        model_slug: String,
    },
}

/// Enumerates the kind of resource being accessed by a permission request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    /// A filesystem read operation.
    FileRead,
    /// A filesystem write operation.
    FileWrite,
    /// A process execution request.
    ShellExec,
    /// A network access request.
    Network,
    /// A custom tool-specific resource kind.
    Custom(String),
}

/// Stores one structured permission request emitted by a tool or runtime action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// The originating tool or subsystem name.
    pub tool_name: String,
    /// The kind of resource the action touches.
    pub resource: ResourceKind,
    /// A human-readable action summary.
    pub action_summary: String,
    /// A human-readable justification for why the action is needed.
    pub justification: String,
    /// An optional canonical path touched by the request.
    pub path: Option<PathBuf>,
    /// An optional host touched by the request.
    pub host: Option<String>,
    /// An optional free-form target string, such as a command line.
    pub target: Option<String>,
}

/// Enumerates the supported approval scopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalScope {
    /// Apply the approval exactly once.
    Once,
    /// Apply the approval for the current turn only.
    Turn,
    /// Apply the approval for the whole session.
    Session,
    /// Apply the approval to a canonical path prefix.
    PathPrefix {
        /// The approved canonical path prefix.
        path: PathBuf,
    },
    /// Apply the approval to one host.
    Host {
        /// The approved host name.
        host: String,
    },
    /// Apply the approval to one tool name.
    Tool {
        /// The approved tool name.
        tool_name: String,
    },
}

/// Enumerates the possible outcomes of a permission decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// Allow the request immediately.
    Allow,
    /// Deny the request with a human-readable reason.
    Deny {
        /// The denial reason.
        reason: String,
    },
    /// Ask the user to approve the request.
    Ask {
        /// The approval request identifier.
        approval_id: SmolStr,
        /// The message shown to the user.
        message: String,
        /// The scopes the user may select.
        available_scopes: Vec<ApprovalScope>,
    },
}

/// Stores approvals cached at different scopes for one runtime snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApprovalCache {
    /// Tool names approved for the whole session.
    pub tool_scopes: BTreeSet<String>,
    /// Hosts approved for the whole session.
    pub host_scopes: BTreeSet<String>,
    /// Canonical path prefixes approved for the whole session.
    pub path_scopes: BTreeSet<PathBuf>,
}

/// Describes the declared filesystem policy before approval merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FileSystemPolicyRecord {
    /// Roots that may be read.
    pub readable_roots: BTreeSet<PathBuf>,
    /// Roots that may be written.
    pub writable_roots: BTreeSet<PathBuf>,
    /// Explicit subpaths denied even if a parent root is allowed.
    pub denied_roots: BTreeSet<PathBuf>,
}

/// Describes the declared sandbox policy before approval merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicyRecord {
    /// The high-level sandbox mode.
    pub mode: SandboxMode,
    /// Whether the runtime should treat the workspace as writable by default.
    pub workspace_write: bool,
}

/// Enumerates the high-level sandbox execution modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxMode {
    /// Do not apply a sandbox.
    Unrestricted,
    /// Apply a restrictive local sandbox.
    Restricted,
    /// Defer execution to an external sandbox implementation.
    External,
}

/// Describes the declared network policy before approval merging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkPolicy {
    /// Deny all outbound network access.
    DenyAll,
    /// Allow all outbound network access.
    AllowAll,
    /// Allow only an explicit set of hosts.
    AllowHosts {
        /// The allowed host names.
        hosts: BTreeSet<String>,
    },
}

/// Stores the additional permissions granted by approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PermissionProfile {
    /// Additional readable roots granted by approval.
    pub readable_roots: BTreeSet<PathBuf>,
    /// Additional writable roots granted by approval.
    pub writable_roots: BTreeSet<PathBuf>,
    /// Additional hosts granted by approval.
    pub allowed_hosts: BTreeSet<String>,
}

/// Stores the fully merged sandbox policy used for one execution attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveSandboxPolicy {
    /// The effective sandbox mode.
    pub mode: SandboxMode,
    /// The readable roots after approval merging.
    pub readable_roots: BTreeSet<PathBuf>,
    /// The writable roots after approval merging.
    pub writable_roots: BTreeSet<PathBuf>,
    /// The denied roots after approval merging.
    pub denied_roots: BTreeSet<PathBuf>,
    /// The effective network policy after approval merging.
    pub network: NetworkPolicy,
}

/// Stores the policy snapshot passed to tools and prompts for one turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicySnapshot {
    /// The top-level policy mode.
    pub mode: SafetyPolicyMode,
    /// The configured policy-model selection.
    pub policy_model: PolicyModelSelection,
    /// The declared sandbox policy.
    pub sandbox_policy: SandboxPolicyRecord,
    /// The declared filesystem policy.
    pub file_system_policy: FileSystemPolicyRecord,
    /// The declared network policy.
    pub network_policy: NetworkPolicy,
    /// The cached approval scopes active for the turn or session.
    pub approval_cache: ApprovalCache,
    /// The merged effective sandbox policy for the current execution context.
    pub effective_policy: EffectiveSandboxPolicy,
    /// Explicit denials observed in the current turn or session.
    pub explicit_denials: Vec<String>,
}

/// Asynchronous permission policy contract used by the runtime.
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    /// Produces the permission decision for one request within one policy snapshot.
    async fn decide(
        &self,
        snapshot: &PolicySnapshot,
        request: &PermissionRequest,
    ) -> Result<PermissionDecision, PermissionError>;
}

/// Merges declared policies and approval-granted permissions into an effective policy.
pub trait SandboxPolicyTransformer: Send + Sync {
    /// Produces the effective sandbox policy for one execution attempt.
    fn effective_permissions(
        &self,
        sandbox_policy: &SandboxPolicyRecord,
        file_system_policy: &FileSystemPolicyRecord,
        network_policy: NetworkPolicy,
        additional_permissions: Option<&PermissionProfile>,
    ) -> Result<EffectiveSandboxPolicy, PermissionError>;
}

/// Default deterministic transformer that merges approval-granted permissions into policy records.
pub struct DefaultSandboxPolicyTransformer;

impl SandboxPolicyTransformer for DefaultSandboxPolicyTransformer {
    fn effective_permissions(
        &self,
        sandbox_policy: &SandboxPolicyRecord,
        file_system_policy: &FileSystemPolicyRecord,
        network_policy: NetworkPolicy,
        additional_permissions: Option<&PermissionProfile>,
    ) -> Result<EffectiveSandboxPolicy, PermissionError> {
        let mut readable_roots = canonicalized_set(&file_system_policy.readable_roots)?;
        let mut writable_roots = canonicalized_set(&file_system_policy.writable_roots)?;
        let denied_roots = canonicalized_set(&file_system_policy.denied_roots)?;

        if let Some(profile) = additional_permissions {
            readable_roots.extend(canonicalized_set(&profile.readable_roots)?);
            writable_roots.extend(canonicalized_set(&profile.writable_roots)?);
        }

        let network = match (network_policy, additional_permissions) {
            (NetworkPolicy::AllowAll, _) => NetworkPolicy::AllowAll,
            (NetworkPolicy::DenyAll, Some(profile)) if !profile.allowed_hosts.is_empty() => {
                NetworkPolicy::AllowHosts {
                    hosts: profile.allowed_hosts.clone(),
                }
            }
            (NetworkPolicy::AllowHosts { mut hosts }, Some(profile)) => {
                hosts.extend(profile.allowed_hosts.iter().cloned());
                NetworkPolicy::AllowHosts { hosts }
            }
            (policy, _) => policy,
        };

        Ok(EffectiveSandboxPolicy {
            mode: sandbox_policy.mode.clone(),
            readable_roots,
            writable_roots,
            denied_roots,
            network,
        })
    }
}

/// Simple deterministic permission policy that honors effective policy and asks when needed.
pub struct StaticPermissionPolicy;

#[async_trait]
impl PermissionPolicy for StaticPermissionPolicy {
    async fn decide(
        &self,
        snapshot: &PolicySnapshot,
        request: &PermissionRequest,
    ) -> Result<PermissionDecision, PermissionError> {
        match request.resource {
            ResourceKind::FileWrite => {
                let Some(path) = request.path.as_ref() else {
                    return Err(PermissionError::InvalidRequest {
                        message: "file write request missing path".into(),
                    });
                };

                let path = canonicalize_path(path)?;
                if snapshot
                    .effective_policy
                    .denied_roots
                    .iter()
                    .any(|denied| path.starts_with(denied))
                {
                    return Ok(PermissionDecision::Deny {
                        reason: format!("path denied by policy: {}", path.display()),
                    });
                }

                if snapshot
                    .effective_policy
                    .writable_roots
                    .iter()
                    .any(|root| path.starts_with(root))
                {
                    return Ok(PermissionDecision::Allow);
                }

                Ok(PermissionDecision::Ask {
                    approval_id: format!("approval-{}", request.tool_name).into(),
                    message: format!(
                        "{} needs write access to {}",
                        request.tool_name,
                        path.display()
                    ),
                    available_scopes: vec![
                        ApprovalScope::Once,
                        ApprovalScope::Turn,
                        ApprovalScope::Session,
                        ApprovalScope::PathPrefix { path },
                    ],
                })
            }
            ResourceKind::Network => {
                let Some(host) = request.host.as_ref() else {
                    return Err(PermissionError::InvalidRequest {
                        message: "network request missing host".into(),
                    });
                };

                let allowed = match &snapshot.effective_policy.network {
                    NetworkPolicy::AllowAll => true,
                    NetworkPolicy::DenyAll => false,
                    NetworkPolicy::AllowHosts { hosts } => hosts.contains(host),
                };

                if allowed {
                    Ok(PermissionDecision::Allow)
                } else {
                    Ok(PermissionDecision::Ask {
                        approval_id: format!("approval-{}", request.tool_name).into(),
                        message: format!("{} needs network access to {}", request.tool_name, host),
                        available_scopes: vec![
                            ApprovalScope::Once,
                            ApprovalScope::Turn,
                            ApprovalScope::Session,
                            ApprovalScope::Host { host: host.clone() },
                        ],
                    })
                }
            }
            _ => Ok(PermissionDecision::Allow),
        }
    }
}

/// Enumerates failures in permission evaluation, path normalization, and sandbox transforms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum PermissionError {
    /// The request payload was structurally invalid.
    #[error("invalid request: {message}")]
    InvalidRequest {
        /// The human-readable validation message.
        message: String,
    },
    /// Path normalization failed.
    #[error("path normalization failed: {path}: {message}")]
    PathNormalizationFailed {
        /// The path that failed normalization.
        path: PathBuf,
        /// The human-readable error message.
        message: String,
    },
    /// No policy implementation was available.
    #[error("policy unavailable")]
    PolicyUnavailable,
    /// The approval channel was closed while waiting for a result.
    #[error("approval channel closed")]
    ApprovalChannelClosed,
    /// Declared and effective policies could not be reconciled.
    #[error("sandbox policy conflict: {message}")]
    SandboxPolicyConflict {
        /// The human-readable conflict message.
        message: String,
    },
    /// The secret backend was unavailable.
    #[error("secret backend unavailable: {message}")]
    SecretBackendUnavailable {
        /// The human-readable backend message.
        message: String,
    },
    /// No sandbox backend was available for the requested platform.
    #[error("sandbox backend unavailable: {message}")]
    SandboxBackendUnavailable {
        /// The human-readable backend message.
        message: String,
    },
    /// The sandbox policy transform failed.
    #[error("sandbox transform failed: {message}")]
    SandboxTransformFailed {
        /// The human-readable transform message.
        message: String,
    },
}

/// Renders the model-visible safety summary from a policy snapshot.
pub fn render_safety_summary(snapshot: &PolicySnapshot) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("Sandbox mode: {:?}.", snapshot.sandbox_policy.mode));

    if snapshot.effective_policy.writable_roots.is_empty() {
        lines.push("You may not write to the filesystem unless approved.".into());
    } else {
        let writable = snapshot
            .effective_policy
            .writable_roots
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("You may only write under: {writable}."));
    }

    match &snapshot.effective_policy.network {
        NetworkPolicy::AllowAll => lines.push("Network access is enabled.".into()),
        NetworkPolicy::DenyAll => {
            lines.push("Network access is restricted unless approved.".into())
        }
        NetworkPolicy::AllowHosts { hosts } => {
            let hosts = hosts.iter().cloned().collect::<Vec<_>>().join(", ");
            lines.push(format!("Network access is limited to: {hosts}."));
        }
    }

    if !snapshot.explicit_denials.is_empty() {
        lines.extend(
            snapshot
                .explicit_denials
                .iter()
                .map(|denial| format!("The user denied: {denial}.")),
        );
    }

    lines
}

fn canonicalize_path(path: &Path) -> Result<PathBuf, PermissionError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    Err(PermissionError::PathNormalizationFailed {
        path: path.to_path_buf(),
        message: "path must be absolute".into(),
    })
}

fn canonicalized_set(paths: &BTreeSet<PathBuf>) -> Result<BTreeSet<PathBuf>, PermissionError> {
    paths.iter().map(|path| canonicalize_path(path)).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use regex::Regex;

    use super::{
        ApprovalCache, DefaultSandboxPolicyTransformer, EffectiveSandboxPolicy,
        FileSystemPolicyRecord, InMemorySecretDetectorRegistry, NetworkPolicy, PermissionDecision,
        PermissionProfile, PermissionRequest, PolicyModelSelection, PolicySnapshot,
        RegexSecretDetector, ResourceKind, SafetyPolicyMode, SandboxMode, SandboxPolicyRecord,
        SecretDetectorRegistry, SecretMatchConfidence, SecretRedactor, StaticPermissionPolicy,
        REDACTED_SECRET_PLACEHOLDER,
    };
    use crate::{PermissionPolicy, SandboxPolicyTransformer};

    #[test]
    fn default_redactor_detects_and_redacts_openai_keys() {
        let registry = InMemorySecretDetectorRegistry::with_default_detectors();
        let redactor = SecretRedactor::new(std::sync::Arc::new(registry));
        let result = redactor.redact("token sk-123456789012345678901234");

        assert!(result.redacted_text.contains(REDACTED_SECRET_PLACEHOLDER));
        assert_eq!(result.report.matches.len(), 1);
        assert_eq!(
            result.report.matches[0].matched.confidence,
            SecretMatchConfidence::High
        );
    }

    #[test]
    fn overlapping_matches_choose_longest_then_highest_confidence() {
        struct TestRegistry {
            detectors: Vec<std::sync::Arc<dyn super::SecretDetector>>,
        }

        impl SecretDetectorRegistry for TestRegistry {
            fn all(&self) -> Vec<std::sync::Arc<dyn super::SecretDetector>> {
                self.detectors.clone()
            }
        }

        let registry = TestRegistry {
            detectors: vec![
                std::sync::Arc::new(RegexSecretDetector {
                    detector_id_value: "short",
                    regex: Regex::new("abcdef").expect("regex"),
                    placeholder: REDACTED_SECRET_PLACEHOLDER,
                    confidence: SecretMatchConfidence::Low,
                }),
                std::sync::Arc::new(RegexSecretDetector {
                    detector_id_value: "long",
                    regex: Regex::new("abcdefgh").expect("regex"),
                    placeholder: REDACTED_SECRET_PLACEHOLDER,
                    confidence: SecretMatchConfidence::Medium,
                }),
            ],
        };

        let redactor = SecretRedactor::new(std::sync::Arc::new(registry));
        let result = redactor.redact("zzabcdefghyy");

        assert_eq!(result.report.matches.len(), 1);
        assert_eq!(result.report.matches[0].detector_id, "long");
    }

    #[test]
    fn transformer_merges_additional_permissions() {
        let transformer = DefaultSandboxPolicyTransformer;
        let mut readable = BTreeSet::new();
        readable.insert(PathBuf::from("C:\\repo"));
        let mut writable = BTreeSet::new();
        writable.insert(PathBuf::from("C:\\repo"));
        let fs_policy = FileSystemPolicyRecord {
            readable_roots: readable,
            writable_roots: writable,
            denied_roots: BTreeSet::new(),
        };
        let mut extra_writable = BTreeSet::new();
        extra_writable.insert(PathBuf::from("C:\\tmp"));
        let mut hosts = BTreeSet::new();
        hosts.insert("example.com".to_string());
        let profile = PermissionProfile {
            readable_roots: BTreeSet::new(),
            writable_roots: extra_writable,
            allowed_hosts: hosts.clone(),
        };

        let effective = transformer
            .effective_permissions(
                &SandboxPolicyRecord {
                    mode: SandboxMode::Restricted,
                    workspace_write: true,
                },
                &fs_policy,
                NetworkPolicy::DenyAll,
                Some(&profile),
            )
            .expect("effective policy");

        assert!(effective.writable_roots.contains(&PathBuf::from("C:\\tmp")));
        assert_eq!(effective.network, NetworkPolicy::AllowHosts { hosts });
    }

    #[tokio::test]
    async fn static_policy_asks_for_write_outside_allowed_roots() {
        let snapshot = PolicySnapshot {
            mode: SafetyPolicyMode::StaticPolicy,
            policy_model: PolicyModelSelection::UseTurnModel,
            sandbox_policy: SandboxPolicyRecord {
                mode: SandboxMode::Restricted,
                workspace_write: true,
            },
            file_system_policy: FileSystemPolicyRecord::default(),
            network_policy: NetworkPolicy::DenyAll,
            approval_cache: ApprovalCache::default(),
            effective_policy: EffectiveSandboxPolicy {
                mode: SandboxMode::Restricted,
                readable_roots: BTreeSet::new(),
                writable_roots: BTreeSet::new(),
                denied_roots: BTreeSet::new(),
                network: NetworkPolicy::DenyAll,
            },
            explicit_denials: Vec::new(),
        };

        let request = PermissionRequest {
            tool_name: "shell_command".into(),
            resource: ResourceKind::FileWrite,
            action_summary: "write file".into(),
            justification: "need to edit file".into(),
            path: Some(PathBuf::from("C:\\repo\\file.rs")),
            host: None,
            target: None,
        };

        let decision = StaticPermissionPolicy
            .decide(&snapshot, &request)
            .await
            .expect("decision");

        assert!(matches!(decision, PermissionDecision::Ask { .. }));
    }

    #[test]
    fn safety_summary_renders_constraints() {
        let mut writable = BTreeSet::new();
        writable.insert(PathBuf::from("C:\\repo"));
        let lines = super::render_safety_summary(&PolicySnapshot {
            mode: SafetyPolicyMode::StaticPolicy,
            policy_model: PolicyModelSelection::UseTurnModel,
            sandbox_policy: SandboxPolicyRecord {
                mode: SandboxMode::Restricted,
                workspace_write: true,
            },
            file_system_policy: FileSystemPolicyRecord::default(),
            network_policy: NetworkPolicy::DenyAll,
            approval_cache: ApprovalCache::default(),
            effective_policy: EffectiveSandboxPolicy {
                mode: SandboxMode::Restricted,
                readable_roots: BTreeSet::new(),
                writable_roots: writable,
                denied_roots: BTreeSet::new(),
                network: NetworkPolicy::DenyAll,
            },
            explicit_denials: vec!["writes outside workspace".into()],
        });

        assert!(lines.iter().any(|line| line.contains("write under")));
        assert!(lines
            .iter()
            .any(|line| line.contains("restricted unless approved")));
        assert!(lines.iter().any(|line| line.contains("denied")));
    }
}
