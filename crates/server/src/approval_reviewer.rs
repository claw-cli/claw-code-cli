use devo_protocol::{
    ModelRequest, RequestContent, RequestMessage, ResponseContent, SamplingControls,
};
use devo_tools::ToolPermissionRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewerDecision {
    Approve { rationale: String },
    Deny { rationale: String },
    Uncertain { rationale: String },
}

pub(crate) fn build_approval_review_request(
    model: String,
    request: &ToolPermissionRequest,
) -> ModelRequest {
    ModelRequest {
        model,
        system: Some(
            "You are Devo's automatic approval reviewer. Decide whether a tool approval request is safe under the user's active policy. Respond with exactly one compact JSON object and no markdown: {\"decision\":\"approve|deny|uncertain\",\"rationale\":\"short reason\"}. Approve only when the action is clearly low risk and scoped to the stated target. Deny destructive, credential, privilege escalation, or ambiguous high-impact actions. Use uncertain when more context or user intent is needed."
                .to_string(),
        ),
        messages: vec![RequestMessage {
            role: "user".to_string(),
            content: vec![RequestContent::Text {
                text: review_prompt_for_request(request),
            }],
        }],
        max_tokens: 128,
        tools: None,
        sampling: SamplingControls {
            temperature: Some(0.0),
            ..SamplingControls::default()
        },
        thinking: None,
        reasoning_effort: None,
        extra_body: None,
    }
}

pub(crate) fn parse_reviewer_decision(content: &[ResponseContent]) -> Option<ReviewerDecision> {
    let raw = content.iter().find_map(|block| match block {
        ResponseContent::Text(text) => Some(text.as_str()),
        ResponseContent::ToolUse { .. } => None,
    })?;
    parse_reviewer_text(raw)
}

fn parse_reviewer_text(raw: &str) -> Option<ReviewerDecision> {
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let rationale = value
        .get("rationale")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    match value.get("decision").and_then(serde_json::Value::as_str)? {
        "approve" => Some(ReviewerDecision::Approve { rationale }),
        "deny" => Some(ReviewerDecision::Deny { rationale }),
        "uncertain" => Some(ReviewerDecision::Uncertain { rationale }),
        _ => None,
    }
}

fn review_prompt_for_request(request: &ToolPermissionRequest) -> String {
    let mut details = vec![
        format!("tool_name: {}", request.tool_name),
        format!("resource: {:?}", request.resource),
        format!("cwd: {}", request.cwd.display()),
        format!("action_summary: {}", request.action_summary),
    ];
    if let Some(justification) = &request.justification {
        details.push(format!("justification: {justification}"));
    }
    if let Some(path) = &request.path {
        details.push(format!("path: {}", path.display()));
    }
    if let Some(host) = &request.host {
        details.push(format!("host: {host}"));
    }
    if let Some(target) = &request.target {
        details.push(format!("target: {target}"));
    }
    if let Some(command_prefix) = &request.command_prefix {
        details.push(format!("command_prefix: {}", command_prefix.join(" ")));
    }
    details.push(format!("input_json: {}", request.input));
    details.join("\n")
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_approval_reviewer_json_decision() {
        assert_eq!(
            parse_reviewer_text(r#"{"decision":"approve","rationale":"scoped command"}"#),
            Some(ReviewerDecision::Approve {
                rationale: "scoped command".to_string(),
            })
        );
        assert_eq!(
            parse_reviewer_text(r#"{"decision":"deny","rationale":"destructive"}"#),
            Some(ReviewerDecision::Deny {
                rationale: "destructive".to_string(),
            })
        );
        assert_eq!(
            parse_reviewer_text(r#"{"decision":"uncertain","rationale":"needs user"}"#),
            Some(ReviewerDecision::Uncertain {
                rationale: "needs user".to_string(),
            })
        );
    }

    #[test]
    fn builds_review_prompt_with_command_prefix() {
        let request = ToolPermissionRequest {
            tool_call_id: "call".to_string(),
            tool_name: "shell_command".to_string(),
            input: json!({ "command": "git add -A" }),
            cwd: std::path::PathBuf::from("C:\\repo"),
            session_id: "session".to_string(),
            turn_id: Some("turn".to_string()),
            resource: devo_safety::ResourceKind::ShellExec,
            action_summary: "Run git add -A".to_string(),
            justification: Some("stage files".to_string()),
            path: None,
            host: None,
            target: Some("git add -A".to_string()),
            command_prefix: Some(vec!["git".to_string(), "add".to_string()]),
            requests_escalation: false,
        };

        let model_request = build_approval_review_request("model".to_string(), &request);
        let RequestContent::Text { text } = &model_request.messages[0].content[0] else {
            panic!("review request should contain text content");
        };
        assert!(text.contains("command_prefix: git add"));
        assert!(text.contains("target: git add -A"));
    }
}
