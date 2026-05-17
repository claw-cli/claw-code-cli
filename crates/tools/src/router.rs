use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use devo_safety::ResourceKind;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::invocation::{ToolCallId, ToolContent, ToolInvocation, ToolName};
use crate::registry::ToolRegistry;
use crate::tool_spec::ToolCapabilityTag;

type ProgressCallback = dyn Fn(&str, &str) + Send + Sync;
type ProgressCallbackArc = Arc<ProgressCallback>;
type CompletionCallback = dyn Fn(&ToolCallResult) + Send + Sync;
type CompletionCallbackArc = Arc<CompletionCallback>;
type PermissionFuture = futures::future::BoxFuture<'static, Result<(), String>>;
type PermissionCheckFn = dyn Fn(ToolPermissionRequest) -> PermissionFuture + Send + Sync;
const PROGRESS_DRAIN_GRACE_MS: u64 = 50;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ToolCallResult {
    pub tool_use_id: String,
    pub content: ToolContent,
    pub is_error: bool,
    pub display_content: Option<String>,
}

impl ToolCallResult {
    pub fn success(tool_use_id: &str, content: ToolContent) -> Self {
        ToolCallResult {
            tool_use_id: tool_use_id.to_string(),
            content,
            is_error: false,
            display_content: None,
        }
    }

    pub fn error(tool_use_id: &str, message: &str) -> Self {
        ToolCallResult {
            tool_use_id: tool_use_id.to_string(),
            content: ToolContent::Text(message.to_string()),
            is_error: true,
            display_content: None,
        }
    }
}

pub struct ToolRuntime {
    registry: Arc<ToolRegistry>,
    permission: PermissionChecker,
    gate: RwLock<()>,
    context: ToolRuntimeContext,
}

impl ToolRuntime {
    pub fn new(registry: Arc<ToolRegistry>, permission: PermissionChecker) -> Self {
        ToolRuntime {
            registry,
            permission,
            gate: RwLock::new(()),
            context: ToolRuntimeContext::default(),
        }
    }

    pub fn new_with_context(
        registry: Arc<ToolRegistry>,
        permission: PermissionChecker,
        context: ToolRuntimeContext,
    ) -> Self {
        ToolRuntime {
            registry,
            permission,
            gate: RwLock::new(()),
            context,
        }
    }

    pub fn new_without_permissions(registry: Arc<ToolRegistry>) -> Self {
        ToolRuntime {
            registry,
            permission: PermissionChecker::always_allow(),
            gate: RwLock::new(()),
            context: ToolRuntimeContext::default(),
        }
    }

    pub async fn execute_batch(&self, calls: &[ToolCall]) -> Vec<ToolCallResult> {
        self.execute_batch_inner(
            calls, /*on_progress*/ None, /*on_completion*/ None,
        )
        .await
    }

    pub async fn execute_batch_streaming(
        &self,
        calls: &[ToolCall],
        on_progress: impl Fn(&str, &str) + Send + Sync + 'static,
    ) -> Vec<ToolCallResult> {
        self.execute_batch_inner(
            calls,
            Some(Box::new(on_progress)),
            /*on_completion*/ None,
        )
        .await
    }

    pub async fn execute_batch_streaming_with_completion(
        &self,
        calls: &[ToolCall],
        on_progress: impl Fn(&str, &str) + Send + Sync + 'static,
        on_completion: impl Fn(&ToolCallResult) + Send + Sync + 'static,
    ) -> Vec<ToolCallResult> {
        self.execute_batch_inner(
            calls,
            Some(Box::new(on_progress)),
            Some(Box::new(on_completion)),
        )
        .await
    }

    async fn execute_batch_inner(
        &self,
        calls: &[ToolCall],
        on_progress: Option<Box<ProgressCallback>>,
        on_completion: Option<Box<CompletionCallback>>,
    ) -> Vec<ToolCallResult> {
        // Wrap the Box in an Arc so it can be shared across spawned tasks
        let on_progress: Option<ProgressCallbackArc> = on_progress.map(Arc::from);
        let on_completion: Option<CompletionCallbackArc> = on_completion.map(Arc::from);

        let mut indexed_results = Vec::with_capacity(calls.len());

        let (parallel, exclusive): (Vec<_>, Vec<_>) = calls
            .iter()
            .enumerate()
            .partition(|(_, call)| self.registry.supports_parallel(&call.name));

        if !parallel.is_empty() {
            let _guard = self.gate.read().await;
            let mut futures: FuturesUnordered<_> = parallel
                .iter()
                .map(|(index, call)| {
                    let on_progress = on_progress.clone();
                    async move { (*index, self.execute_single(call, &on_progress).await) }
                })
                .collect();
            while let Some((index, result)) = futures.next().await {
                if let Some(callback) = &on_completion {
                    callback(&result);
                }
                indexed_results.push((index, result));
            }
        }

        for (index, call) in exclusive {
            let _guard = self.gate.write().await;
            let result = self.execute_single(call, &on_progress).await;
            if let Some(callback) = &on_completion {
                callback(&result);
            }
            indexed_results.push((index, result));
        }

        indexed_results.sort_by_key(|(index, _)| *index);
        indexed_results
            .into_iter()
            .map(|(_, result)| result)
            .collect()
    }

    pub(crate) async fn execute_single(
        &self,
        call: &ToolCall,
        on_progress: &Option<ProgressCallbackArc>,
    ) -> ToolCallResult {
        let tool = match self.registry.get(&call.name) {
            Some(t) => t.clone(),
            None => {
                warn!(tool = %call.name, "tool not found");
                return ToolCallResult::error(&call.id, &format!("unknown tool: {}", call.name));
            }
        };

        if let Some(request) = self.permission_request_for_call(call) {
            match self.permission.check(request).await {
                Ok(()) => {}
                Err(reason) => {
                    return ToolCallResult::error(
                        &call.id,
                        &format!("permission denied: {reason}"),
                    );
                }
            }
        }

        info!(tool = %call.name, id = %call.id, "executing tool");

        let invocation = ToolInvocation {
            call_id: ToolCallId(call.id.clone()),
            tool_name: ToolName(call.name.clone().into()),
            session_id: self.context.session_id.clone(),
            cwd: self.context.cwd.clone(),
            input: call.input.clone(),
        };

        let (progress_sender, mut progress_task) = if let Some(cb) = on_progress.as_ref() {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let call_id = call.id.clone();
            let cb = Arc::clone(cb);
            let task = tokio::spawn(async move {
                while let Some(chunk) = rx.recv().await {
                    cb(&call_id, &chunk);
                }
            });
            (Some(tx), Some(task))
        } else {
            (None, None)
        };

        let result = tool.handle(invocation, progress_sender).await;
        if let Some(task) = progress_task.as_mut() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(PROGRESS_DRAIN_GRACE_MS),
                task,
            )
            .await;
        }

        match result {
            Ok(output) => {
                let is_error = output.is_error();
                let display_content = output.display_content().map(str::to_string);
                let content = output.to_content();
                ToolCallResult {
                    tool_use_id: call.id.clone(),
                    content,
                    is_error,
                    display_content,
                }
            }
            Err(e) => ToolCallResult::error(&call.id, &e.to_string()),
        }
    }

    fn permission_request_for_call(&self, call: &ToolCall) -> Option<ToolPermissionRequest> {
        let spec = self.registry.spec(&call.name)?;
        let needs_permission = spec.execution_mode == crate::tool_spec::ToolExecutionMode::Mutating
            || spec
                .capability_tags
                .iter()
                .any(|tag| matches!(tag, ToolCapabilityTag::NetworkAccess));
        if !needs_permission {
            return None;
        }

        let resource = resource_kind_for_tool(&call.name, &spec.capability_tags);
        let path = path_for_tool_input(&call.name, &call.input, &self.context.cwd);
        let host = host_for_tool_input(&call.name, &call.input);
        let target = target_for_tool_input(&call.name, &call.input);
        let command_prefix = command_prefix_for_tool_input(&call.name, &call.input);
        Some(ToolPermissionRequest {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            input: call.input.clone(),
            cwd: self.context.cwd.clone(),
            session_id: self.context.session_id.clone(),
            turn_id: self.context.turn_id.clone(),
            resource,
            action_summary: crate::tool_summary::tool_summary(
                &call.name,
                &call.input,
                &self.context.cwd,
            ),
            justification: justification_for_tool_input(&call.input),
            path,
            host,
            target,
            command_prefix,
            requests_escalation: requests_explicit_escalation(&call.input),
        })
    }
}

#[derive(Clone)]
pub struct PermissionChecker {
    inner: Arc<PermissionCheckFn>,
}

impl PermissionChecker {
    pub fn new<F>(check: F) -> Self
    where
        F: Fn(ToolPermissionRequest) -> PermissionFuture + Send + Sync + 'static,
    {
        PermissionChecker {
            inner: Arc::new(check),
        }
    }

    pub fn always_allow() -> Self {
        PermissionChecker::new(|_| Box::pin(async { Ok(()) }))
    }

    pub async fn check(&self, request: ToolPermissionRequest) -> Result<(), String> {
        (self.inner)(request).await
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolRuntimeContext {
    pub session_id: String,
    pub turn_id: Option<String>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ToolPermissionRequest {
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub cwd: PathBuf,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub resource: ResourceKind,
    pub action_summary: String,
    pub justification: Option<String>,
    pub path: Option<PathBuf>,
    pub host: Option<String>,
    pub target: Option<String>,
    pub command_prefix: Option<Vec<String>>,
    pub requests_escalation: bool,
}

fn resource_kind_for_tool(tool_name: &str, tags: &[ToolCapabilityTag]) -> ResourceKind {
    if tags
        .iter()
        .any(|tag| matches!(tag, ToolCapabilityTag::NetworkAccess))
    {
        return ResourceKind::Network;
    }
    if tags
        .iter()
        .any(|tag| matches!(tag, ToolCapabilityTag::ExecuteProcess))
    {
        return ResourceKind::ShellExec;
    }
    if tags
        .iter()
        .any(|tag| matches!(tag, ToolCapabilityTag::WriteFiles))
    {
        return ResourceKind::FileWrite;
    }
    if tags.iter().any(|tag| {
        matches!(
            tag,
            ToolCapabilityTag::ReadFiles | ToolCapabilityTag::SearchWorkspace
        )
    }) {
        return ResourceKind::FileRead;
    }
    ResourceKind::Custom(tool_name.to_string())
}

fn path_for_tool_input(tool_name: &str, input: &serde_json::Value, cwd: &Path) -> Option<PathBuf> {
    let raw = match tool_name {
        "read" | "write" | "lsp" => input
            .get("filePath")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("path").and_then(serde_json::Value::as_str)),
        "grep" | "glob" => input.get("path").and_then(serde_json::Value::as_str),
        _ => None,
    }?;
    let path = PathBuf::from(raw);
    Some(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

fn host_for_tool_input(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "webfetch" => input
            .get("url")
            .and_then(serde_json::Value::as_str)
            .and_then(host_from_url),
        "websearch" => input
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(|_| "websearch".to_string()),
        _ => None,
    }
}

fn host_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    after_scheme
        .split('/')
        .next()
        .and_then(|host| (!host.is_empty()).then(|| host.to_string()))
}

fn target_for_tool_input(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    match tool_name {
        "bash" | "shell_command" => input
            .get("command")
            .or_else(|| input.get("cmd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        "exec_command" => input
            .get("cmd")
            .or_else(|| input.get("command"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        "webfetch" => input
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        "websearch" => input
            .get("query")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn command_prefix_for_tool_input(
    tool_name: &str,
    input: &serde_json::Value,
) -> Option<Vec<String>> {
    if tool_name == "exec_command"
        && let Some(prefix_rule) = input.get("prefix_rule").and_then(prefix_rule_from_value)
    {
        return Some(prefix_rule);
    }

    let command = match tool_name {
        "bash" | "shell_command" => input
            .get("command")
            .or_else(|| input.get("cmd"))
            .and_then(serde_json::Value::as_str),
        "exec_command" => input
            .get("cmd")
            .or_else(|| input.get("command"))
            .and_then(serde_json::Value::as_str),
        _ => None,
    }?;
    command_prefix(command)
}

fn prefix_rule_from_value(value: &serde_json::Value) -> Option<Vec<String>> {
    let prefix = value
        .as_array()?
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    (!prefix.is_empty()).then(|| prefix.into_iter().map(str::to_string).collect())
}

fn requests_explicit_escalation(input: &serde_json::Value) -> bool {
    matches!(
        input
            .get("sandbox_permissions")
            .and_then(serde_json::Value::as_str),
        Some("require_escalated" | "with_additional_permissions")
    ) || input.get("additional_permissions").is_some()
}

fn command_prefix(command: &str) -> Option<Vec<String>> {
    let argv = shlex::split(command)?;
    if argv
        .iter()
        .any(|token| shell_token_requires_user_scope(command, token))
        || argv
            .first()
            .is_some_and(|token| looks_like_env_assignment(token))
    {
        return None;
    }
    prefix_from_argv(&argv)
}

fn shell_token_requires_user_scope(command: &str, token: &str) -> bool {
    token.contains(['|', ';', '>', '<', '*', '?', '$', '(', ')'])
        || token.contains("$(")
        || command.contains("&&")
        || command.contains("||")
        || command.contains("$(")
        || command.contains('`')
}

fn looks_like_env_assignment(token: &str) -> bool {
    let Some((name, value)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !value.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
}

fn prefix_from_argv(argv: &[String]) -> Option<Vec<String>> {
    let executable = argv.first()?.clone();
    let second = argv
        .iter()
        .skip(1)
        .find(|token| !token.starts_with('-'))
        .cloned();
    Some(
        second
            .map(|token| vec![executable.clone(), token])
            .unwrap_or_else(|| vec![executable]),
    )
}

fn justification_for_tool_input(input: &serde_json::Value) -> Option<String> {
    input
        .get("justification")
        .or_else(|| input.get("description"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ToolExecutionError;
    use crate::events::ToolProgressSender;
    use crate::handler_kind::ToolHandlerKind;
    use crate::invocation::{FunctionToolOutput, ToolOutput};
    use crate::json_schema::JsonSchema;
    use crate::registry::ToolRegistryBuilder;
    use crate::tool_handler::ToolHandler;
    use crate::tool_spec::{ToolExecutionMode, ToolOutputMode, ToolSpec};
    use async_trait::async_trait;
    use pretty_assertions::assert_eq;

    struct ReadOnlyTool;

    #[async_trait]
    impl ToolHandler for ReadOnlyTool {
        fn tool_kind(&self) -> ToolHandlerKind {
            ToolHandlerKind::Read
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
            _progress: Option<ToolProgressSender>,
        ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
            Ok(Box::new(FunctionToolOutput::success("read ok")))
        }
    }

    struct WriteTool;

    #[async_trait]
    impl ToolHandler for WriteTool {
        fn tool_kind(&self) -> ToolHandlerKind {
            ToolHandlerKind::Write
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
            _progress: Option<ToolProgressSender>,
        ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
            Ok(Box::new(FunctionToolOutput::success("write ok")))
        }
    }

    struct DelayedReadTool;

    #[async_trait]
    impl ToolHandler for DelayedReadTool {
        fn tool_kind(&self) -> ToolHandlerKind {
            ToolHandlerKind::Read
        }

        async fn handle(
            &self,
            invocation: ToolInvocation,
            _progress: Option<ToolProgressSender>,
        ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
            let delay_ms = invocation
                .input
                .get("delay_ms")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            let output = invocation
                .input
                .get("output")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(Box::new(FunctionToolOutput::success(output)))
        }
    }

    fn make_registry() -> Arc<ToolRegistry> {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler("read_tool", Arc::new(ReadOnlyTool));
        builder.push_spec(ToolSpec {
            name: "read_tool".into(),
            description: String::new(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        });
        builder.register_handler("write_tool", Arc::new(WriteTool));
        builder.push_spec(ToolSpec {
            name: "write_tool".into(),
            description: String::new(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![ToolCapabilityTag::WriteFiles],
            supports_parallel: false,
        });
        builder.register_handler("delayed_read_tool", Arc::new(DelayedReadTool));
        builder.push_spec(ToolSpec {
            name: "delayed_read_tool".into(),
            description: String::new(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::ReadOnly,
            capability_tags: vec![],
            supports_parallel: true,
        });
        Arc::new(builder.build())
    }

    #[tokio::test]
    async fn unknown_tool_returns_error() {
        let registry = make_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let call = ToolCall {
            id: "c1".into(),
            name: "nonexistent".into(),
            input: serde_json::json!({}),
        };
        let result = runtime.execute_single(&call, &None).await;
        assert!(result.is_error);
        assert!(result.content.into_string().contains("unknown tool"));
    }

    #[tokio::test]
    async fn read_only_tool_succeeds() {
        let registry = make_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let call = ToolCall {
            id: "c1".into(),
            name: "read_tool".into(),
            input: serde_json::json!({}),
        };
        let result = runtime.execute_single(&call, &None).await;
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn execute_batch_runs_all_tools() {
        let registry = make_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "read_tool".into(),
                input: serde_json::json!({}),
            },
            ToolCall {
                id: "c2".into(),
                name: "write_tool".into(),
                input: serde_json::json!({}),
            },
        ];
        let results = runtime.execute_batch(&calls).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| !r.is_error));
    }

    #[tokio::test]
    async fn permission_checker_allow() {
        let checker = PermissionChecker::always_allow();
        assert!(
            checker
                .check(test_permission_request("any_tool"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn permission_checker_deny() {
        let checker = PermissionChecker::new(|request| {
            let n = request.tool_name;
            Box::pin(async move {
                if n == "blocked" {
                    Err("blocked".into())
                } else {
                    Ok(())
                }
            })
        });
        assert!(
            checker
                .check(test_permission_request("allowed"))
                .await
                .is_ok()
        );
        assert!(
            checker
                .check(test_permission_request("blocked"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn runtime_denies_mutating_with_deny_checker() {
        let registry = make_registry();
        let checker = PermissionChecker::new(|request| {
            let n = request.tool_name;
            Box::pin(async move { Err(format!("{n} denied")) })
        });
        let runtime = ToolRuntime::new(registry, checker);
        // Read-only tool should succeed (no permission check)
        let read_call = ToolCall {
            id: "c1".into(),
            name: "read_tool".into(),
            input: serde_json::json!({}),
        };
        let read_result = runtime.execute_single(&read_call, &None).await;
        assert!(
            !read_result.is_error,
            "read-only tool should bypass permission check"
        );

        // Mutating tool should be denied
        let write_call = ToolCall {
            id: "c2".into(),
            name: "write_tool".into(),
            input: serde_json::json!({}),
        };
        let write_result = runtime.execute_single(&write_call, &None).await;
        assert!(write_result.is_error, "mutating tool should be denied");
        assert!(
            write_result
                .content
                .into_string()
                .contains("permission denied")
        );
    }

    #[tokio::test]
    async fn mutating_tool_permission_request_carries_context_and_summary() {
        let registry = make_registry();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = std::sync::Mutex::new(Some(tx));
        let checker = PermissionChecker::new(move |request| {
            tx.lock()
                .expect("lock sender")
                .take()
                .expect("send once")
                .send(request)
                .expect("receiver still alive");
            Box::pin(async { Ok(()) })
        });
        let runtime = ToolRuntime::new_with_context(
            registry,
            checker,
            ToolRuntimeContext {
                session_id: "session-1".into(),
                turn_id: Some("turn-1".into()),
                cwd: PathBuf::from("C:/workspace"),
            },
        );
        let call = ToolCall {
            id: "call-1".into(),
            name: "write_tool".into(),
            input: serde_json::json!({ "filePath": "src/main.rs" }),
        };

        let result = runtime.execute_single(&call, &None).await;
        let request = rx.await.expect("permission request");

        assert!(!result.is_error);
        assert_eq!(request.tool_call_id, "call-1");
        assert_eq!(request.tool_name, "write_tool");
        assert_eq!(request.session_id, "session-1");
        assert_eq!(request.turn_id, Some("turn-1".into()));
        assert_eq!(request.resource, devo_safety::ResourceKind::FileWrite);
    }

    #[test]
    fn path_for_tool_input_resolves_relative_paths_against_cwd() {
        let path = path_for_tool_input(
            "write",
            &serde_json::json!({ "filePath": "src/lib.rs" }),
            Path::new("C:/workspace"),
        );

        assert_eq!(path, Some(PathBuf::from("C:/workspace").join("src/lib.rs")));
    }

    #[test]
    fn host_from_url_ignores_scheme_and_path() {
        assert_eq!(
            host_from_url("https://example.com/docs/index.html"),
            Some("example.com".into())
        );
    }

    #[test]
    fn command_prefix_uses_first_command_tokens() {
        assert_eq!(
            command_prefix("git add -A"),
            Some(vec!["git".to_string(), "add".to_string()])
        );
        assert_eq!(
            command_prefix("'cargo' test --all"),
            Some(vec!["cargo".to_string(), "test".to_string()])
        );
    }

    #[test]
    fn command_prefix_rejects_complex_shell_features() {
        assert_eq!(command_prefix("git add -A | tee out.txt"), None);
        assert_eq!(command_prefix("npm test > output.txt"), None);
        assert_eq!(command_prefix("echo $(pwd)"), None);
        assert_eq!(command_prefix("echo $HOME"), None);
        assert_eq!(command_prefix("FOO=bar cargo test"), None);
        assert_eq!(command_prefix("(pwd)"), None);
        assert_eq!(command_prefix("rg *.rs"), None);
        assert_eq!(command_prefix("cargo fmt && cargo test"), None);
    }

    #[test]
    fn exec_command_prefix_rule_overrides_derived_prefix() {
        assert_eq!(
            command_prefix_for_tool_input(
                "exec_command",
                &serde_json::json!({
                    "cmd": "git add -A",
                    "prefix_rule": ["cargo", "test"]
                })
            ),
            Some(vec!["cargo".to_string(), "test".to_string()])
        );
    }

    #[test]
    fn explicit_sandbox_permissions_request_escalation() {
        assert!(requests_explicit_escalation(&serde_json::json!({
            "sandbox_permissions": "require_escalated"
        })));
        assert!(requests_explicit_escalation(&serde_json::json!({
            "additional_permissions": {"network": true}
        })));
        assert!(!requests_explicit_escalation(&serde_json::json!({
            "sandbox_permissions": "use_default"
        })));
    }

    fn test_permission_request(tool_name: &str) -> ToolPermissionRequest {
        ToolPermissionRequest {
            tool_call_id: "call".into(),
            tool_name: tool_name.into(),
            input: serde_json::json!({}),
            cwd: std::path::PathBuf::new(),
            session_id: "session".into(),
            turn_id: Some("turn".into()),
            resource: devo_safety::ResourceKind::Custom(tool_name.into()),
            action_summary: tool_name.into(),
            justification: None,
            path: None,
            host: None,
            target: None,
            command_prefix: None,
            requests_escalation: false,
        }
    }

    #[tokio::test]
    async fn runtime_concurrent_then_sequential() {
        // Two parallel tools followed by a sequential tool should still work
        let registry = make_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let calls = vec![
            ToolCall {
                id: "r1".into(),
                name: "read_tool".into(),
                input: serde_json::json!({}),
            },
            ToolCall {
                id: "r2".into(),
                name: "read_tool".into(),
                input: serde_json::json!({}),
            },
            ToolCall {
                id: "w1".into(),
                name: "write_tool".into(),
                input: serde_json::json!({}),
            },
        ];
        let results = runtime.execute_batch(&calls).await;
        assert_eq!(results.len(), 3);
        assert!(results.iter().all(|r| !r.is_error));
        // Order should be preserved (parallel tools first, then sequential)
        assert_eq!(results[0].tool_use_id, "r1".to_string());
        assert_eq!(results[1].tool_use_id, "r2".to_string());
    }

    #[tokio::test]
    async fn parallel_completion_callback_streams_before_batch_is_done_but_results_stay_ordered() {
        let registry = make_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let calls = vec![
            ToolCall {
                id: "slow".into(),
                name: "delayed_read_tool".into(),
                input: serde_json::json!({
                    "delay_ms": 50,
                    "output": "slow output",
                }),
            },
            ToolCall {
                id: "fast".into(),
                name: "delayed_read_tool".into(),
                input: serde_json::json!({
                    "delay_ms": 5,
                    "output": "fast output",
                }),
            },
        ];
        let completions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let completions_clone = Arc::clone(&completions);

        let results = runtime
            .execute_batch_streaming_with_completion(
                &calls,
                |_tool_use_id, _content| {},
                move |result| {
                    completions_clone
                        .lock()
                        .expect("lock completions")
                        .push(result.tool_use_id.clone());
                },
            )
            .await;

        assert_eq!(
            completions.lock().expect("lock completions").as_slice(),
            &["fast".to_string(), "slow".to_string()]
        );
        assert_eq!(
            results
                .iter()
                .map(|result| result.tool_use_id.as_str())
                .collect::<Vec<_>>(),
            vec!["slow", "fast"]
        );
    }

    #[tokio::test]
    async fn runtime_empty_batch() {
        let registry = make_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let results = runtime.execute_batch(&[]).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn runtime_single_tool() {
        let registry = make_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let call = ToolCall {
            id: "c1".into(),
            name: "read_tool".into(),
            input: serde_json::json!({}),
        };
        let result = runtime.execute_single(&call, &None).await;
        assert!(!result.is_error);
        assert_eq!(result.tool_use_id, "c1");
    }

    // --- Streaming tests ---

    struct StreamingHandler {
        chunks: Vec<String>,
    }

    #[async_trait]
    impl ToolHandler for StreamingHandler {
        fn tool_kind(&self) -> ToolHandlerKind {
            ToolHandlerKind::Write
        }

        async fn handle(
            &self,
            _invocation: ToolInvocation,
            progress: Option<ToolProgressSender>,
        ) -> Result<Box<dyn ToolOutput>, ToolExecutionError> {
            // Send chunks through progress, then return
            if let Some(sender) = progress {
                for chunk in &self.chunks {
                    let _ = sender.send(chunk.clone());
                    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
                }
            }
            Ok(Box::new(FunctionToolOutput::success(self.chunks.join(""))))
        }
    }

    fn make_streaming_registry() -> Arc<ToolRegistry> {
        let mut builder = ToolRegistryBuilder::new();
        builder.register_handler(
            "stream_tool",
            Arc::new(StreamingHandler {
                chunks: vec!["hello ".into(), "world".into()],
            }),
        );
        builder.push_spec(ToolSpec {
            name: "stream_tool".into(),
            description: String::new(),
            input_schema: JsonSchema::object(Default::default(), None, None),
            output_mode: ToolOutputMode::Text,
            execution_mode: ToolExecutionMode::Mutating,
            capability_tags: vec![],
            supports_parallel: false,
        });
        Arc::new(builder.build())
    }

    #[tokio::test]
    async fn execute_single_receives_progress() {
        let registry = make_streaming_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let call = ToolCall {
            id: "s1".into(),
            name: "stream_tool".into(),
            input: serde_json::json!({}),
        };

        let collected = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let collected_clone = Arc::clone(&collected);
        let cb: ProgressCallbackArc = Arc::new(move |_, chunk| {
            let c = collected_clone.clone();
            let chunk = chunk.to_string();
            tokio::spawn(async move {
                c.lock().await.push(chunk);
            });
        });

        let result = runtime.execute_single(&call, &Some(cb.clone())).await;
        assert!(!result.is_error);
        assert_eq!(result.content.into_string(), "hello world");

        // Give the spawned tasks time to process
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let final_chunks = collected.lock().await;
        assert_eq!(final_chunks.len(), 2, "should have received 2 chunks");
        assert!(final_chunks.iter().any(|c| c == "hello "));
        assert!(final_chunks.iter().any(|c| c == "world"));
    }

    #[tokio::test]
    async fn execute_batch_streaming_receives_progress() {
        let registry = make_streaming_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let call = ToolCall {
            id: "s1".into(),
            name: "stream_tool".into(),
            input: serde_json::json!({}),
        };

        let collected = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let collected_clone = Arc::clone(&collected);

        let results = runtime
            .execute_batch_streaming(&[call], move |_id, chunk| {
                let c = collected_clone.clone();
                let chunk = chunk.to_string();
                tokio::spawn(async move {
                    c.lock().await.push(chunk);
                });
            })
            .await;

        assert_eq!(results.len(), 1);
        assert!(!results[0].is_error);

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let final_chunks = collected.lock().await;
        assert_eq!(
            final_chunks.len(),
            2,
            "streaming callback should have 2 chunks"
        );
    }

    #[tokio::test]
    async fn execute_batch_streaming_empty() {
        let registry = make_streaming_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let results = runtime.execute_batch_streaming(&[], |_, _| {}).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn execute_batch_streaming_unknown_tool() {
        let registry = make_streaming_registry();
        let runtime = ToolRuntime::new_without_permissions(registry);
        let call = ToolCall {
            id: "x1".into(),
            name: "nonexistent".into(),
            input: serde_json::json!({}),
        };
        let results = runtime.execute_batch_streaming(&[call], |_, _| {}).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
    }
}
