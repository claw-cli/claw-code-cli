use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The legacy permission mode controlling how the current runtime handles permission checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    /// Approve every request without asking.
    AutoApprove,
    /// Ask the user for confirmation on each request.
    Interactive,
    /// Deny all requests that require permission.
    Deny,
}

/// The legacy resource kind used by the current tool runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResourceKind {
    /// A file-read request.
    FileRead,
    /// A file-write request.
    FileWrite,
    /// A shell-execution request.
    ShellExec,
    /// A network-access request.
    Network,
    /// A tool-specific custom resource kind.
    Custom(String),
}

/// The legacy permission request emitted by the current tool system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// The originating tool name.
    pub tool_name: String,
    /// The kind of resource being accessed.
    pub resource: ResourceKind,
    /// The free-form human-readable description of the action.
    pub description: String,
    /// The optional target path, host, or command string.
    pub target: Option<String>,
}

/// The legacy result of one permission check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// Allow the request immediately.
    Allow,
    /// Deny the request with a reason.
    Deny {
        /// The human-readable denial reason.
        reason: String,
    },
    /// Ask the user to approve the request.
    Ask {
        /// The human-readable approval prompt.
        message: String,
    },
}

/// The legacy pluggable permission-policy trait used by the current runtime.
#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    /// Returns the legacy permission decision for one request.
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision;
}

/// One legacy rule-based permission entry persisted in configuration or tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// The resource kind matched by the rule.
    pub resource: ResourceKind,
    /// The glob-like pattern matched against the target.
    pub pattern: String,
    /// Whether the rule allows or denies matching requests.
    pub allow: bool,
}

/// The legacy rule-based permission policy used by the current query loop and tools.
pub struct RuleBasedPolicy {
    /// The fallback permission mode used when no explicit rule matches.
    pub mode: PermissionMode,
    /// The explicit resource rules evaluated before the fallback mode.
    pub rules: Vec<PermissionRule>,
}

impl RuleBasedPolicy {
    /// Creates a rule-based policy with no explicit rules.
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode,
            rules: Vec::new(),
        }
    }

    /// Creates a rule-based policy with an explicit rule list.
    pub fn with_rules(mode: PermissionMode, rules: Vec<PermissionRule>) -> Self {
        Self { mode, rules }
    }

    fn match_rule(&self, request: &PermissionRequest) -> Option<&PermissionRule> {
        let target = request.target.as_deref().unwrap_or("");
        self.rules.iter().find(|rule| {
            rule.resource == request.resource && Self::pattern_matches(&rule.pattern, target)
        })
    }

    fn pattern_matches(pattern: &str, target: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        if pattern.ends_with('*') {
            return target.starts_with(pattern.trim_end_matches('*'));
        }
        target == pattern
    }
}

#[async_trait]
impl PermissionPolicy for RuleBasedPolicy {
    async fn check(&self, request: &PermissionRequest) -> PermissionDecision {
        if let Some(rule) = self.match_rule(request) {
            return if rule.allow {
                PermissionDecision::Allow
            } else {
                PermissionDecision::Deny {
                    reason: format!("blocked by rule: {}", rule.pattern),
                }
            };
        }

        match self.mode {
            PermissionMode::AutoApprove => PermissionDecision::Allow,
            PermissionMode::Deny => PermissionDecision::Deny {
                reason: "permission mode is Deny".into(),
            },
            PermissionMode::Interactive => PermissionDecision::Ask {
                message: format!(
                    "{} wants to access {:?}: {}",
                    request.tool_name, request.resource, request.description
                ),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PermissionDecision, PermissionMode, PermissionPolicy, PermissionRequest, PermissionRule,
        ResourceKind, RuleBasedPolicy,
    };

    fn file_write_request(target: Option<&str>) -> PermissionRequest {
        PermissionRequest {
            tool_name: "file_write".into(),
            resource: ResourceKind::FileWrite,
            description: "write a file".into(),
            target: target.map(|value| value.into()),
        }
    }

    #[test]
    fn permission_mode_serde_roundtrip() {
        for mode in [
            PermissionMode::AutoApprove,
            PermissionMode::Interactive,
            PermissionMode::Deny,
        ] {
            let json = serde_json::to_string(&mode).expect("serialize");
            let restored: PermissionMode = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(restored, mode);
        }
    }

    #[test]
    fn pattern_matches_prefix_and_exact() {
        assert!(RuleBasedPolicy::pattern_matches("/tmp/*", "/tmp/file.txt"));
        assert!(RuleBasedPolicy::pattern_matches(
            "/etc/passwd",
            "/etc/passwd"
        ));
        assert!(!RuleBasedPolicy::pattern_matches(
            "/tmp/*",
            "/var/tmp/file.txt"
        ));
    }

    #[tokio::test]
    async fn explicit_allow_rule_overrides_deny_mode() {
        let policy = RuleBasedPolicy::with_rules(
            PermissionMode::Deny,
            vec![PermissionRule {
                resource: ResourceKind::FileWrite,
                pattern: "/tmp/*".into(),
                allow: true,
            }],
        );

        assert!(matches!(
            policy.check(&file_write_request(Some("/tmp/file"))).await,
            PermissionDecision::Allow
        ));
    }

    #[tokio::test]
    async fn interactive_mode_asks() {
        let policy = RuleBasedPolicy::new(PermissionMode::Interactive);
        assert!(matches!(
            policy.check(&file_write_request(Some("/tmp/file"))).await,
            PermissionDecision::Ask { .. }
        ));
    }
}
