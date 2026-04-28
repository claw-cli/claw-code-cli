use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::{Duration, timeout};

use devo_core::{FileSystemSkillCatalog, PresetModelCatalog, SkillsConfig};
use devo_protocol::{
    ModelRequest, ModelResponse, ResponseContent, ResponseMetadata, StopReason, StreamEvent, Usage,
};
use devo_provider::ModelProviderSDK;
use devo_server::{ClientTransportKind, ServerRuntime, ServerRuntimeDependencies};
use devo_tools::ToolRegistry;

struct SingleReplyProvider;

#[async_trait]
impl ModelProviderSDK for SingleReplyProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
        Ok(ModelResponse {
            id: "title-1".into(),
            content: vec![ResponseContent::Text("Generated rollout title".to_string())],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
            metadata: ResponseMetadata::default(),
        })
    }

    async fn completion_stream(
        &self,
        _request: ModelRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: "Hello from persistence test.".into(),
            }),
            Ok(StreamEvent::MessageDone {
                response: ModelResponse {
                    id: "resp-1".into(),
                    content: vec![ResponseContent::Text("Hello from persistence test.".into())],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Usage::default(),
                    metadata: ResponseMetadata::default(),
                },
            }),
        ])))
    }

    fn name(&self) -> &str {
        "single-reply-test-provider"
    }
}

#[tokio::test]
async fn runtime_rebuilds_sessions_from_rollout_and_resume_works() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 1,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": "Persistent session",
                    "model": "test-model"
                }
            }),
        )
        .await
        .context("session/start response")?;
    let session_id = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionStartResult>,
    >(start_response)?
    .result
    .session
    .session_id;

    let turn_start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 2,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "persist this session" }],
                    "model": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start response")?;
    let _: devo_server::SuccessResponse<devo_server::TurnStartResult> =
        serde_json::from_value(turn_start_response)?;

    wait_for_turn_completed(&mut notifications_rx).await?;

    let rebuilt_runtime = build_runtime(data_root.path())?;
    rebuilt_runtime.load_persisted_sessions().await?;
    let (rebuilt_connection_id, _rebuilt_notifications_rx) =
        initialize_connection(&rebuilt_runtime).await?;

    let list_response = rebuilt_runtime
        .handle_incoming(
            rebuilt_connection_id,
            serde_json::json!({
                "id": 3,
                "method": "session/list",
                "params": {}
            }),
        )
        .await
        .context("session/list response")?;
    let list_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionListResult>,
    >(list_response)?
    .result;
    assert_eq!(list_result.sessions.len(), 1);
    assert_eq!(list_result.sessions[0].session_id, session_id);
    assert_eq!(
        list_result.sessions[0].title.as_deref(),
        Some("Persistent session")
    );

    let resume_response = rebuilt_runtime
        .handle_incoming(
            rebuilt_connection_id,
            serde_json::json!({
                "id": 4,
                "method": "session/resume",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/resume response")?;
    let resume_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionResumeResult>,
    >(resume_response)?
    .result;

    assert_eq!(resume_result.session.session_id, session_id);
    assert_eq!(
        resume_result.session.title.as_deref(),
        Some("Persistent session")
    );
    assert!(resume_result.loaded_item_count >= 2);
    assert!(resume_result.latest_turn.is_some());
    Ok(())
}

#[tokio::test]
async fn runtime_generates_final_title_and_persists_explicit_rename() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 11,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": null,
                    "model": "test-model"
                }
            }),
        )
        .await
        .context("session/start response")?;
    let session_id = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionStartResult>,
    >(start_response)?
    .result
    .session
    .session_id;

    let _ = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 12,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "implement rollout persistence for the rust server" }],
                    "model": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start response")?;

    wait_for_turn_completed(&mut notifications_rx).await?;
    wait_for_title_update(&mut notifications_rx, "Generated rollout title").await?;

    let resume_after_completion = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 13,
                "method": "session/resume",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/resume response after completion")?;
    let completed_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionResumeResult>,
    >(resume_after_completion)?
    .result;
    assert_eq!(
        completed_result.session.title.as_deref(),
        Some("Generated rollout title")
    );
    assert_eq!(
        completed_result.session.title_state,
        devo_core::SessionTitleState::Final(devo_core::SessionTitleFinalSource::ModelGenerated)
    );

    let rename_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 14,
                "method": "session/title/update",
                "params": {
                    "session_id": session_id,
                    "title": "Rollout persistence follow-up"
                }
            }),
        )
        .await
        .context("session/title/update response")?;
    let rename_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionTitleUpdateResult>,
    >(rename_response)?
    .result;
    assert_eq!(
        rename_result.session.title.as_deref(),
        Some("Rollout persistence follow-up")
    );
    assert_eq!(
        rename_result.session.title_state,
        devo_core::SessionTitleState::Final(devo_core::SessionTitleFinalSource::UserRename)
    );

    let rebuilt_runtime = build_runtime(data_root.path())?;
    rebuilt_runtime.load_persisted_sessions().await?;
    let (rebuilt_connection_id, _notifications_rx) =
        initialize_connection(&rebuilt_runtime).await?;
    let resume_after_rebuild = rebuilt_runtime
        .handle_incoming(
            rebuilt_connection_id,
            serde_json::json!({
                "id": 15,
                "method": "session/resume",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/resume response after rebuild")?;
    let rebuilt_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionResumeResult>,
    >(resume_after_rebuild)?
    .result;
    assert_eq!(
        rebuilt_result.session.title.as_deref(),
        Some("Rollout persistence follow-up")
    );
    assert_eq!(
        rebuilt_result.session.title_state,
        devo_core::SessionTitleState::Final(devo_core::SessionTitleFinalSource::UserRename)
    );
    Ok(())
}

#[tokio::test]
async fn runtime_assigns_provisional_title_after_first_prompt() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 21,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": null,
                    "model": "test-model"
                }
            }),
        )
        .await
        .context("session/start response")?;
    let session_id = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionStartResult>,
    >(start_response)?
    .result
    .session
    .session_id;

    let _ = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 22,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "investigate why the current session title stays null" }],
                    "model": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start response")?;

    let provisional_title = wait_for_any_title_update(&mut notifications_rx).await?;
    assert_eq!(
        provisional_title,
        "Investigate why the current session title stays null"
    );

    let list_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 23,
                "method": "session/list",
                "params": {}
            }),
        )
        .await
        .context("session/list response")?;
    let list_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionListResult>,
    >(list_response)?
    .result;
    assert_eq!(
        list_result.sessions[0].title.as_deref(),
        Some("Investigate why the current session title stays null")
    );
    assert_eq!(
        list_result.sessions[0].title_state,
        devo_core::SessionTitleState::Provisional
    );
    Ok(())
}

fn build_runtime(data_root: &std::path::Path) -> Result<Arc<ServerRuntime>> {
    Ok(ServerRuntime::new(
        data_root.to_path_buf(),
        ServerRuntimeDependencies::new(
            Arc::new(SingleReplyProvider),
            Arc::new(ToolRegistry::new()),
            "test-model".to_string(),
            Arc::new(PresetModelCatalog::default()),
            None,
            Box::new(FileSystemSkillCatalog::new(SkillsConfig::default())),
            devo_core::AgentsMdConfig::default(),
        ),
    ))
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
                "id": 10,
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
    let response: devo_server::SuccessResponse<devo_server::InitializeResult> =
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

async fn wait_for_title_update(
    notifications_rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
    expected_title: &str,
) -> Result<()> {
    timeout(Duration::from_secs(5), async {
        while let Some(value) = notifications_rx.recv().await {
            if value.get("method") != Some(&serde_json::json!("session/title/updated")) {
                continue;
            }
            if value["params"]["session"]["title"] == serde_json::json!(expected_title) {
                return Ok(());
            }
        }
        anyhow::bail!("notification channel closed before expected session/title/updated")
    })
    .await
    .context("timed out waiting for session/title/updated")??;
    Ok(())
}

async fn wait_for_any_title_update(
    notifications_rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
) -> Result<String> {
    timeout(Duration::from_secs(5), async {
        while let Some(value) = notifications_rx.recv().await {
            if value.get("method") != Some(&serde_json::json!("session/title/updated")) {
                continue;
            }
            if let Some(title) = value["params"]["session"]["title"].as_str() {
                return Ok(title.to_string());
            }
        }
        anyhow::bail!("notification channel closed before any session/title/updated")
    })
    .await
    .context("timed out waiting for session/title/updated")?
}
