use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::stream;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio::time::timeout;

use devo_core::FileSystemSkillCatalog;
use devo_core::PresetModelCatalog;
use devo_core::SkillsConfig;
use devo_protocol::ModelRequest;
use devo_protocol::ModelResponse;
use devo_protocol::RequestContent;
use devo_protocol::ResponseContent;
use devo_protocol::ResponseMetadata;
use devo_protocol::StopReason;
use devo_protocol::StreamEvent;
use devo_protocol::Usage;
use devo_provider::ModelProviderSDK;
use devo_server::ClientTransportKind;
use devo_server::ErrorResponse;
use devo_server::ProtocolErrorCode;
use devo_server::ServerRuntime;
use devo_server::ServerRuntimeDependencies;
use devo_server::SkillChangedResult;
use devo_server::SkillListResult;
use devo_server::SkillRecord;
use devo_server::SkillSource;
use devo_server::SuccessResponse;
use devo_tools::ToolRegistry;
use devo_tools::invocation::{FunctionToolOutput, ToolInvocation, ToolOutput};
use devo_tools::json_schema::JsonSchema;
use devo_tools::registry::ToolRegistryBuilder;
use devo_tools::tool_handler::ToolHandler;
use devo_tools::tool_spec::{ToolExecutionMode, ToolOutputMode, ToolSpec};

#[derive(Default)]
struct CapturingProvider {
    stream_requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl ModelProviderSDK for CapturingProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
        Ok(ModelResponse {
            id: "title-1".into(),
            content: vec![ResponseContent::Text("Generated skill title".into())],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
            metadata: ResponseMetadata::default(),
        })
    }

    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>> {
        self.stream_requests
            .lock()
            .expect("stream request lock")
            .push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: "Skill acknowledged.".into(),
            }),
            Ok(StreamEvent::MessageDone {
                response: ModelResponse {
                    id: "resp-1".into(),
                    content: vec![ResponseContent::Text("Skill acknowledged.".into())],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Usage::default(),
                    metadata: ResponseMetadata::default(),
                },
            }),
        ])))
    }

    fn name(&self) -> &str {
        "capturing-test-provider"
    }
}

fn create_skill(root: &Path, name: &str, content: &str) -> PathBuf {
    let skill_dir = root.join(name);
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    let skill_path = skill_dir.join("SKILL.md");
    std::fs::write(&skill_path, content).expect("write skill");
    skill_path
}

fn canonical_skill_path(path: &Path) -> PathBuf {
    devo_core::normalize_canonical_path(
        std::fs::canonicalize(path).expect("canonicalize skill path"),
    )
}

fn canonical_skill_base_dir(path: &Path) -> PathBuf {
    canonical_skill_path(path)
        .parent()
        .expect("canonical skill base directory")
        .to_path_buf()
}

fn build_runtime(
    data_root: &Path,
    user_skill_root: PathBuf,
    workspace_root: Option<PathBuf>,
    provider: Arc<dyn ModelProviderSDK>,
) -> Arc<ServerRuntime> {
    build_runtime_with_registry(
        data_root,
        user_skill_root,
        workspace_root,
        provider,
        Arc::new(ToolRegistry::new()),
    )
}

fn build_runtime_with_registry(
    data_root: &Path,
    user_skill_root: PathBuf,
    workspace_root: Option<PathBuf>,
    provider: Arc<dyn ModelProviderSDK>,
    registry: Arc<ToolRegistry>,
) -> Arc<ServerRuntime> {
    let workspace_skill_roots = workspace_root
        .iter()
        .map(|root| root.join(".devo").join("skills"))
        .collect::<Vec<_>>();
    let db_path = data_root.join("test_skills.db");
    let db = Arc::new(devo_server::db::Database::open(db_path).expect("open test database"));
    ServerRuntime::new(
        data_root.to_path_buf(),
        ServerRuntimeDependencies::new(
            provider,
            registry,
            "test-model".to_string(),
            Arc::new(PresetModelCatalog::default()),
            workspace_root,
            Box::new(FileSystemSkillCatalog::new(SkillsConfig {
                enabled: true,
                user_roots: vec![user_skill_root],
                workspace_roots: workspace_skill_roots,
                watch_for_changes: false,
            })),
            devo_core::AgentsMdConfig::default(),
            db,
        ),
    )
}

async fn initialize_connection(
    runtime: &Arc<ServerRuntime>,
) -> Result<(u64, mpsc::UnboundedReceiver<serde_json::Value>)> {
    let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
    let connection_id = runtime
        .register_connection(ClientTransportKind::Stdio, notifications_tx)
        .await;
    let initialize_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "client_name": "test",
                    "client_version": "1.0.0",
                    "transport": "stdio",
                    "supports_streaming": true,
                    "supports_binary_images": false,
                    "opt_out_notification_methods": []
                }
            }),
        )
        .await
        .context("initialize response")?;
    let response: SuccessResponse<devo_server::InitializeResult> =
        serde_json::from_value(initialize_response)?;
    assert_eq!(response.result.server_name, "devo-server");

    let _ = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "method": "initialized"
            }),
        )
        .await;
    Ok((connection_id, notifications_rx))
}

async fn start_session(
    runtime: &Arc<ServerRuntime>,
    connection_id: u64,
    cwd: &Path,
) -> Result<devo_core::SessionId> {
    let response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 2,
                "method": "session/start",
                "params": {
                    "cwd": cwd,
                    "ephemeral": false,
                    "title": "Skills integration",
                    "model": "test-model"
                }
            }),
        )
        .await
        .context("session/start response")?;
    let result: SuccessResponse<devo_server::SessionStartResult> =
        serde_json::from_value(response)?;
    Ok(result.result.session.session_id)
}

async fn wait_for_turn_completed(
    notifications_rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
) -> Result<()> {
    timeout(Duration::from_secs(5), async {
        while let Some(value) = notifications_rx.recv().await {
            if value.get("method") == Some(&serde_json::json!("turn/completed")) {
                return Ok(());
            }
        }
        anyhow::bail!("notification channel closed before turn/completed")
    })
    .await
    .context("timed out waiting for turn/completed")??;
    Ok(())
}

fn user_request_text(request: &ModelRequest) -> Result<String> {
    let text = all_user_request_texts(request).join("\n");
    (!text.is_empty())
        .then_some(text)
        .context("expected a user text request payload")
}

fn all_user_request_texts(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .flat_map(|message| {
            message.content.iter().filter_map(|content| match content {
                RequestContent::Reasoning { text } => Some(text.clone()),
                RequestContent::Text { text } => Some(text.clone()),
                RequestContent::ToolUse { .. } | RequestContent::ToolResult { .. } => None,
            })
        })
        .collect()
}

struct BlockingReadOnlyTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl ToolHandler for BlockingReadOnlyTool {
    fn tool_kind(&self) -> devo_tools::ToolHandlerKind {
        devo_tools::ToolHandlerKind::Read
    }

    async fn handle(
        &self,
        _invocation: ToolInvocation,
        _progress: Option<devo_tools::events::ToolProgressSender>,
    ) -> Result<Box<dyn ToolOutput>, devo_tools::ToolExecutionError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(Box::new(FunctionToolOutput::success("released")))
    }
}

#[derive(Default)]
struct SteerCapturingProvider {
    stream_requests: Mutex<Vec<ModelRequest>>,
}

#[async_trait]
impl ModelProviderSDK for SteerCapturingProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
        Ok(ModelResponse {
            id: "title-1".into(),
            content: vec![ResponseContent::Text("Generated skill title".into())],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
            metadata: ResponseMetadata::default(),
        })
    }

    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>> {
        let request_number = {
            let mut requests = self.stream_requests.lock().expect("stream request lock");
            requests.push(request);
            requests.len()
        };
        let events = if request_number == 1 {
            vec![
                Ok(StreamEvent::ToolCallStart {
                    index: 0,
                    id: "tool-1".into(),
                    name: "blocking_wait".into(),
                    input: json!({}),
                }),
                Ok(StreamEvent::ToolCallInputDelta {
                    index: 0,
                    partial_json: "{}".into(),
                }),
                Ok(StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp-1".into(),
                        content: vec![ResponseContent::ToolUse {
                            id: "tool-1".into(),
                            name: "blocking_wait".into(),
                            input: json!({}),
                        }],
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Usage::default(),
                        metadata: ResponseMetadata::default(),
                    },
                }),
            ]
        } else {
            vec![
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    text: "Steer applied.".into(),
                }),
                Ok(StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp-2".into(),
                        content: vec![ResponseContent::Text("Steer applied.".into())],
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Usage::default(),
                        metadata: ResponseMetadata::default(),
                    },
                }),
            ]
        };

        Ok(Box::pin(stream::iter(events)))
    }

    fn name(&self) -> &str {
        "steer-capturing-provider"
    }
}

#[tokio::test]
async fn skills_list_returns_user_and_workspace_skills() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let user_skill_root = temp_dir.path().join("user-skills");
    let workspace_root = temp_dir.path().join("workspace");
    let workspace_skill_root = workspace_root.join(".devo").join("skills");

    let rust_skill_path =
        create_skill(&user_skill_root, "rust-docs", "# Rust Docs\n\nUse rustdoc.");
    let team_skill_path = create_skill(
        &workspace_skill_root,
        "team-style",
        "# Team Style\n\nFollow the formatter.",
    );

    let runtime = build_runtime(
        temp_dir.path(),
        user_skill_root.clone(),
        Some(workspace_root.clone()),
        Arc::new(CapturingProvider::default()),
    );
    let (connection_id, _) = initialize_connection(&runtime).await?;

    let response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 3,
                "method": "skills/list",
                "params": {
                    "cwd": workspace_root,
                }
            }),
        )
        .await
        .context("skills/list response")?;
    let result: SuccessResponse<SkillListResult> = serde_json::from_value(response)?;
    let canonical_rust_skill_path = canonical_skill_path(&rust_skill_path);
    let canonical_team_skill_path = canonical_skill_path(&team_skill_path);

    assert_eq!(
        result.result,
        SkillListResult {
            skills: vec![
                SkillRecord {
                    id: "rust-docs".into(),
                    name: "rust-docs".into(),
                    description: format!("Skill discovered at {}", rust_skill_path.display()),
                    path: canonical_rust_skill_path,
                    enabled: true,
                    source: SkillSource::User,
                },
                SkillRecord {
                    id: "team-style".into(),
                    name: "team-style".into(),
                    description: format!("Skill discovered at {}", team_skill_path.display()),
                    path: canonical_team_skill_path,
                    enabled: true,
                    source: SkillSource::Workspace {
                        cwd: workspace_root,
                    },
                },
            ],
        }
    );
    Ok(())
}

#[tokio::test]
async fn skills_changed_rediscovers_new_workspace_skill() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let user_skill_root = temp_dir.path().join("user-skills");
    let workspace_root = temp_dir.path().join("workspace");
    let workspace_skill_root = workspace_root.join(".devo").join("skills");

    let alpha_skill_path = create_skill(&workspace_skill_root, "alpha", "# Alpha\n\nFirst skill.");

    let runtime = build_runtime(
        temp_dir.path(),
        user_skill_root,
        Some(workspace_root.clone()),
        Arc::new(CapturingProvider::default()),
    );
    let (connection_id, _) = initialize_connection(&runtime).await?;

    let first_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 4,
                "method": "skills/changed",
                "params": {
                    "cwd": workspace_root.clone(),
                }
            }),
        )
        .await
        .context("first skills/changed response")?;
    let first_result: SuccessResponse<SkillChangedResult> = serde_json::from_value(first_response)?;
    let canonical_alpha_skill_path = canonical_skill_path(&alpha_skill_path);
    assert_eq!(
        first_result.result,
        SkillChangedResult {
            skills: vec![SkillRecord {
                id: "alpha".into(),
                name: "alpha".into(),
                description: format!("Skill discovered at {}", alpha_skill_path.display()),
                path: canonical_alpha_skill_path.clone(),
                enabled: true,
                source: SkillSource::Workspace {
                    cwd: workspace_root.clone(),
                },
            }],
        }
    );

    let bravo_skill_path = create_skill(&workspace_skill_root, "bravo", "# Bravo\n\nSecond skill.");
    let second_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 5,
                "method": "skills/changed",
                "params": {
                    "cwd": workspace_root,
                }
            }),
        )
        .await
        .context("second skills/changed response")?;
    let second_result: SuccessResponse<SkillChangedResult> =
        serde_json::from_value(second_response)?;
    let canonical_bravo_skill_path = canonical_skill_path(&bravo_skill_path);
    assert_eq!(
        second_result.result,
        SkillChangedResult {
            skills: vec![
                SkillRecord {
                    id: "alpha".into(),
                    name: "alpha".into(),
                    description: format!("Skill discovered at {}", alpha_skill_path.display()),
                    path: canonical_alpha_skill_path,
                    enabled: true,
                    source: SkillSource::Workspace {
                        cwd: workspace_root.clone(),
                    },
                },
                SkillRecord {
                    id: "bravo".into(),
                    name: "bravo".into(),
                    description: format!("Skill discovered at {}", bravo_skill_path.display()),
                    path: canonical_bravo_skill_path,
                    enabled: true,
                    source: SkillSource::Workspace {
                        cwd: workspace_root,
                    },
                },
            ],
        }
    );
    Ok(())
}

#[tokio::test]
async fn turn_start_resolves_skill_content_into_model_request() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let user_skill_root = temp_dir.path().join("user-skills");
    let workspace_root = temp_dir.path().join("workspace");
    let skill_path = create_skill(
        &user_skill_root,
        "rust-docs",
        "# Rust Docs\n\nPrefer `cargo test` before `cargo fmt`.",
    );
    let provider = Arc::new(CapturingProvider::default());
    let runtime = build_runtime(
        temp_dir.path(),
        user_skill_root,
        Some(workspace_root.clone()),
        provider.clone(),
    );
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, &workspace_root).await?;

    let response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 6,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [
                        { "type": "text", "text": "Follow this skill." },
                        { "type": "skill", "id": "rust-docs" }
                    ],
                    "model": null,
                    "thinking": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start response")?;
    let start_result: SuccessResponse<devo_server::TurnStartResult> =
        serde_json::from_value(response)?;
    assert_eq!(start_result.result.status, devo_core::TurnStatus::Running);

    wait_for_turn_completed(&mut notifications_rx).await?;

    let captured_request = provider
        .stream_requests
        .lock()
        .expect("captured requests lock")
        .first()
        .cloned()
        .context("expected one streamed model request")?;
    let request_text = user_request_text(&captured_request)?;
    let skill_base_dir = canonical_skill_base_dir(&skill_path);

    assert!(request_text.contains("Follow this skill."));
    assert!(request_text.contains("<skill id=\"rust-docs\" name=\"rust-docs\">"));
    assert!(request_text.contains("Prefer `cargo test` before `cargo fmt`."));
    assert!(request_text.contains(&format!("Base directory: {}", skill_base_dir.display())));
    Ok(())
}

#[tokio::test]
async fn turn_start_rejects_missing_skill_references() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let user_skill_root = temp_dir.path().join("user-skills");
    let workspace_root = temp_dir.path().join("workspace");
    let runtime = build_runtime(
        temp_dir.path(),
        user_skill_root,
        Some(workspace_root.clone()),
        Arc::new(CapturingProvider::default()),
    );
    let (connection_id, _) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, &workspace_root).await?;

    let response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 7,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [
                        { "type": "skill", "id": "missing-skill" }
                    ],
                    "model": null,
                    "thinking": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start missing skill response")?;
    let error: ErrorResponse = serde_json::from_value(response)?;

    assert_eq!(error.error.code, ProtocolErrorCode::InvalidParams);
    assert!(error.error.message.contains("skill not found"));
    Ok(())
}

#[tokio::test]
async fn turn_steer_injects_resolved_skill_into_next_model_request() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let user_skill_root = temp_dir.path().join("user-skills");
    let workspace_root = temp_dir.path().join("workspace");
    let skill_path = create_skill(
        &user_skill_root,
        "steer-rust",
        "---\nname: steer-rust\ndescription: Rust steering\n---\nPrefer exhaustive matches and cargo tests.",
    );
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut builder = ToolRegistryBuilder::new();
    builder.register_handler(
        "blocking_wait",
        Arc::new(BlockingReadOnlyTool {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
    );
    builder.push_spec(ToolSpec {
        name: "blocking_wait".into(),
        description: "Blocks until the integration test releases it.".into(),
        input_schema: JsonSchema::object(std::collections::BTreeMap::new(), None, None),
        output_mode: ToolOutputMode::Text,
        execution_mode: ToolExecutionMode::ReadOnly,
        capability_tags: vec![],
        supports_parallel: true,
    });
    let registry = Arc::new(builder.build());
    let provider = Arc::new(SteerCapturingProvider::default());
    let runtime = build_runtime_with_registry(
        temp_dir.path(),
        user_skill_root,
        Some(workspace_root.clone()),
        provider.clone(),
        registry,
    );
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;
    let session_id = start_session(&runtime, connection_id, &workspace_root).await?;

    let response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 8,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [
                        { "type": "text", "text": "Start with the tool." }
                    ],
                    "model": null,
                    "thinking": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start response for steering test")?;
    let start_result: SuccessResponse<devo_server::TurnStartResult> =
        serde_json::from_value(response)?;

    timeout(Duration::from_secs(5), started.notified())
        .await
        .context("timed out waiting for blocking tool to start")?;

    let steer_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 9,
                "method": "turn/steer",
                "params": {
                    "session_id": session_id,
                    "expected_turn_id": start_result.result.turn_id,
                    "input": [
                        { "type": "text", "text": "Apply this steer now." },
                        { "type": "skill", "id": "steer-rust" }
                    ]
                }
            }),
        )
        .await
        .context("turn/steer response")?;
    let steer_result: SuccessResponse<devo_server::TurnSteerResult> =
        serde_json::from_value(steer_response)?;
    assert_eq!(steer_result.result.turn_id, start_result.result.turn_id);

    release.notify_one();
    wait_for_turn_completed(&mut notifications_rx).await?;

    let captured_requests = provider
        .stream_requests
        .lock()
        .expect("captured requests lock");
    assert_eq!(captured_requests.len(), 2);

    let first_user_texts = all_user_request_texts(&captured_requests[0]);
    let second_user_texts = all_user_request_texts(&captured_requests[1]);
    let skill_base_dir = canonical_skill_base_dir(&skill_path);

    assert!(
        first_user_texts
            .iter()
            .all(|text| !text.contains("Apply this steer now.")),
        "steer text should not appear before the follow-up request"
    );
    assert!(
        second_user_texts
            .iter()
            .any(|text| text.contains("Apply this steer now.")),
        "expected steer text in the follow-up request"
    );
    assert!(
        second_user_texts
            .iter()
            .any(|text| text.contains("<skill id=\"steer-rust\" name=\"steer-rust\">")),
        "expected resolved skill wrapper in the follow-up request"
    );
    assert!(
        second_user_texts
            .iter()
            .any(|text| text.contains("Prefer exhaustive matches and cargo tests.")),
        "expected skill body in the follow-up request"
    );
    assert!(
        second_user_texts
            .iter()
            .any(|text| text.contains(&format!("Base directory: {}", skill_base_dir.display()))),
        "expected skill base directory in the follow-up request"
    );
    Ok(())
}
