use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::{self, Stream};
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, timeout};

use devo_core::{FileSystemSkillCatalog, PresetModelCatalog, SkillsConfig};
use devo_protocol::{
    ModelRequest, ModelResponse, ResponseContent, ResponseMetadata, SessionHistoryItemKind,
    StopReason, StreamEvent, TurnStatus, Usage,
};
use devo_provider::ModelProviderSDK;
use devo_server::{ClientTransportKind, ServerRuntime, ServerRuntimeDependencies};
use devo_tools::ToolRegistry;

struct SingleReplyProvider;

#[derive(Default)]
struct CapturingProvider {
    requests: Mutex<Vec<ModelRequest>>,
}

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

#[async_trait]
impl ModelProviderSDK for CapturingProvider {
    async fn completion(&self, request: ModelRequest) -> Result<ModelResponse> {
        self.requests.lock().expect("lock requests").push(request);
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
        request: ModelRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>> {
        self.requests.lock().expect("lock requests").push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(StreamEvent::TextDelta {
                index: 0,
                text: "Captured request reply.".into(),
            }),
            Ok(StreamEvent::MessageDone {
                response: ModelResponse {
                    id: "resp-capture".into(),
                    content: vec![ResponseContent::Text("Captured request reply.".into())],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Usage::default(),
                    metadata: ResponseMetadata::default(),
                },
            }),
        ])))
    }

    fn name(&self) -> &str {
        "capturing-provider"
    }
}

/// A stream that yields one TextDelta, then blocks on a oneshot until unblocked or
/// cancelled, then yields MessageDone.  Used by tests that need to interrupt a turn
/// mid-stream to exercise the deferred-item completion race.
struct GatedStream {
    block_rx: oneshot::Receiver<()>,
    state: u8,
}

impl GatedStream {
    fn new(block_rx: oneshot::Receiver<()>) -> Self {
        Self { block_rx, state: 0 }
    }
}

impl Stream for GatedStream {
    type Item = Result<StreamEvent>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Option<Self::Item>> {
        match self.state {
            0 => {
                self.state = 1;
                task::Poll::Ready(Some(Ok(StreamEvent::TextDelta {
                    index: 0,
                    text: "mid-interrupt content".into(),
                })))
            }
            1 => match Pin::new(&mut self.block_rx).poll(cx) {
                task::Poll::Ready(Ok(())) => {
                    self.state = 2;
                    task::Poll::Ready(Some(Ok(StreamEvent::MessageDone {
                        response: ModelResponse {
                            id: "resp-gated".into(),
                            content: vec![ResponseContent::Text("mid-interrupt content".into())],
                            stop_reason: Some(StopReason::EndTurn),
                            usage: Usage::default(),
                            metadata: ResponseMetadata::default(),
                        },
                    })))
                }
                task::Poll::Ready(Err(_)) => task::Poll::Ready(None),
                task::Poll::Pending => task::Poll::Pending,
            },
            2 => {
                self.state = 3;
                task::Poll::Ready(None)
            }
            _ => task::Poll::Ready(None),
        }
    }
}

/// Provider whose stream blocks mid-way, letting the test send an interrupt while
/// the assistant item is still in-progress.
struct GatedProvider {
    /// Kept alive so the oneshot receiver in GatedStream blocks forever
    /// (or until the task is aborted, dropping the receiver).
    _block_tx: Mutex<Option<oneshot::Sender<()>>>,
    /// Receiver taken by the first completion_stream call.
    block_rx: Mutex<Option<oneshot::Receiver<()>>>,
}

impl GatedProvider {
    fn new() -> Self {
        let (tx, rx) = oneshot::channel();
        Self {
            _block_tx: Mutex::new(Some(tx)),
            block_rx: Mutex::new(Some(rx)),
        }
    }
}

#[async_trait]
impl ModelProviderSDK for GatedProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse> {
        Ok(ModelResponse {
            id: "title-gated".into(),
            content: vec![ResponseContent::Text("Gated title".to_string())],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
            metadata: ResponseMetadata::default(),
        })
    }

    async fn completion_stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>> {
        let rx = self
            .block_rx
            .lock()
            .expect("lock block_rx")
            .take()
            .expect("completion_stream called more than once");
        Ok(Box::pin(GatedStream::new(rx)))
    }

    fn name(&self) -> &str {
        "gated-provider"
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

    wait_for_title_update(&mut notifications_rx, "Generated rollout title").await?;
    wait_for_turn_completed(&mut notifications_rx).await?;

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

#[tokio::test]
async fn runtime_skips_invalid_rollout_files_when_loading_sessions() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 31,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": "Valid session",
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
                "id": 32,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "persist the valid session" }],
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

    let bad_rollout_dir = data_root.path().join("sessions/2026/04/28");
    std::fs::create_dir_all(&bad_rollout_dir)?;
    let bad_rollout_path =
        bad_rollout_dir.join("rollout-2026-04-28T15-12-34Z-legacy-invalid.jsonl");
    std::fs::write(
        &bad_rollout_path,
        "{ definitely not valid json\n{\"still\":\"broken\"}\n",
    )?;

    let rebuilt_runtime = build_runtime(data_root.path())?;
    rebuilt_runtime.load_persisted_sessions().await?;
    let (rebuilt_connection_id, _notifications_rx) =
        initialize_connection(&rebuilt_runtime).await?;

    let list_response = rebuilt_runtime
        .handle_incoming(
            rebuilt_connection_id,
            serde_json::json!({
                "id": 33,
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
        Some("Valid session")
    );
    Ok(())
}

#[tokio::test]
async fn runtime_recovers_session_when_middle_rollout_line_is_corrupted() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 41,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": "Recoverable session",
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
                "id": 42,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "persist this session before corruption" }],
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

    let sessions_root = data_root.path().join("sessions");
    let rollout_path = std::fs::read_dir(&sessions_root)?
        .next()
        .context("expected year partition")??
        .path();
    let rollout_path = std::fs::read_dir(rollout_path)?
        .next()
        .context("expected month partition")??
        .path();
    let rollout_path = std::fs::read_dir(rollout_path)?
        .next()
        .context("expected day partition")??
        .path();
    let rollout_path = std::fs::read_dir(rollout_path)?
        .next()
        .context("expected rollout file")??
        .path();

    let mut lines = std::fs::read_to_string(&rollout_path)?
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert!(lines.len() >= 4);
    lines[2] = "{\"Turn\":{\"timestamp\":\"broken\"".to_string();
    std::fs::write(&rollout_path, format!("{}\n", lines.join("\n")))?;

    let rebuilt_runtime = build_runtime(data_root.path())?;
    rebuilt_runtime.load_persisted_sessions().await?;
    let (rebuilt_connection_id, _notifications_rx) =
        initialize_connection(&rebuilt_runtime).await?;

    let resume_response = rebuilt_runtime
        .handle_incoming(
            rebuilt_connection_id,
            serde_json::json!({
                "id": 43,
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
        Some("Recoverable session")
    );
    assert!(resume_result.loaded_item_count >= 1);
    Ok(())
}

#[tokio::test]
async fn session_compact_runs_asynchronously_and_emits_lifecycle_events() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 51,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": "Compaction session",
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
                "id": 52,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "create some history first" }],
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

    let compact_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 53,
                "method": "session/compact",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/compact response")?;
    let compact_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionCompactResult>,
    >(compact_response)?
    .result;
    assert_eq!(compact_result.session.session_id, session_id);

    wait_for_notification_method(&mut notifications_rx, "session/compaction/started").await?;
    wait_for_notification_method(&mut notifications_rx, "session/compaction/completed").await?;
    Ok(())
}

#[tokio::test]
async fn compacted_session_resume_keeps_full_transcript_after_restart() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 61,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": "Persist compacted session",
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

    for request_id in 0..3 {
        let large_prompt = "x".repeat(30_000);
        let _ = runtime
            .handle_incoming(
                connection_id,
                serde_json::json!({
                    "id": 62 + request_id,
                    "method": "turn/start",
                    "params": {
                        "session_id": session_id,
                        "input": [{ "type": "text", "text": large_prompt }],
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
    }

    let _ = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 70,
                "method": "session/compact",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/compact response")?;
    wait_for_notification_method(&mut notifications_rx, "session/compaction/completed").await?;

    let rebuilt_runtime = build_runtime(data_root.path())?;
    rebuilt_runtime.load_persisted_sessions().await?;
    let (rebuilt_connection_id, _notifications_rx) =
        initialize_connection(&rebuilt_runtime).await?;

    let resume_response = rebuilt_runtime
        .handle_incoming(
            rebuilt_connection_id,
            serde_json::json!({
                "id": 71,
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

    assert!(
        resume_result.history_items.len() >= 6,
        "expected full transcript to survive compaction, got {:?}",
        resume_result.history_items
    );
    assert!(
        resume_result
            .history_items
            .iter()
            .all(|item| !item.body.contains("<compaction_summary>")),
        "compaction summary must not appear in user-visible transcript"
    );
    assert!(
        resume_result
            .history_items
            .iter()
            .any(|item| item.body.contains("Hello from persistence test.")),
        "expected assistant transcript entries to remain visible"
    );
    Ok(())
}

#[tokio::test]
async fn compacted_session_next_query_uses_compaction_summary_after_restart() -> Result<()> {
    let data_root = TempDir::new()?;
    let runtime = build_runtime(data_root.path())?;
    let (connection_id, mut notifications_rx) = initialize_connection(&runtime).await?;

    let start_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 81,
                "method": "session/start",
                "params": {
                    "cwd": data_root.path(),
                    "ephemeral": false,
                    "title": "Prompt snapshot session",
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

    for request_id in 0..3 {
        let large_prompt = "x".repeat(30_000);
        let _ = runtime
            .handle_incoming(
                connection_id,
                serde_json::json!({
                    "id": 82 + request_id,
                    "method": "turn/start",
                    "params": {
                        "session_id": session_id,
                        "input": [{ "type": "text", "text": large_prompt }],
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
    }

    let _ = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 90,
                "method": "session/compact",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/compact response")?;
    wait_for_notification_method(&mut notifications_rx, "session/compaction/completed").await?;

    let capturing_provider = Arc::new(CapturingProvider::default());
    let rebuilt_runtime =
        build_runtime_with_provider(data_root.path(), capturing_provider.clone())?;
    rebuilt_runtime.load_persisted_sessions().await?;
    let (rebuilt_connection_id, mut rebuilt_notifications_rx) =
        initialize_connection(&rebuilt_runtime).await?;

    let _ = rebuilt_runtime
        .handle_incoming(
            rebuilt_connection_id,
            serde_json::json!({
                "id": 91,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "go on" }],
                    "model": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start response after restart")?;
    wait_for_turn_completed(&mut rebuilt_notifications_rx).await?;

    let requests = capturing_provider.requests.lock().expect("lock requests");
    let request = requests
        .last()
        .context("expected captured model request after restart")?;

    assert!(
        request.messages.iter().any(|message| {
            message.content.iter().any(|content| match content {
                devo_protocol::RequestContent::Text { text }
                | devo_protocol::RequestContent::Reasoning { text } => {
                    text.contains("<compaction_summary>")
                }
                devo_protocol::RequestContent::ToolUse { .. }
                | devo_protocol::RequestContent::ToolResult { .. } => false,
            })
        }),
        "expected prompt request to include compaction summary after restart"
    );
    Ok(())
}

fn build_runtime(data_root: &std::path::Path) -> Result<Arc<ServerRuntime>> {
    build_runtime_with_provider(data_root, Arc::new(SingleReplyProvider))
}

fn build_runtime_with_provider(
    data_root: &std::path::Path,
    provider: Arc<dyn ModelProviderSDK>,
) -> Result<Arc<ServerRuntime>> {
    let db_path = data_root.join("test_persistence.db");
    let db = Arc::new(devo_server::db::Database::open(db_path).expect("open test database"));
    Ok(ServerRuntime::new(
        data_root.to_path_buf(),
        ServerRuntimeDependencies::new(
            provider,
            Arc::new(ToolRegistry::new()),
            "test-model".to_string(),
            Arc::new(PresetModelCatalog::default()),
            None,
            Box::new(FileSystemSkillCatalog::new(SkillsConfig::default())),
            devo_core::AgentsMdConfig::default(),
            db,
            data_root.join("config.toml"),
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

async fn wait_for_notification_method(
    notifications_rx: &mut mpsc::UnboundedReceiver<serde_json::Value>,
    method: &str,
) -> Result<()> {
    let wanted = serde_json::json!(method);
    timeout(Duration::from_secs(5), async {
        while let Some(value) = notifications_rx.recv().await {
            if value.get("method") == Some(&wanted) {
                return Ok(());
            }
        }
        anyhow::bail!("notification channel closed before {method}")
    })
    .await
    .with_context(|| format!("timed out waiting for {method}"))??;
    Ok(())
}

#[tokio::test]
async fn interrupt_mid_stream_does_not_duplicate_last_item_on_resume() -> Result<()> {
    let data_root = TempDir::new()?;
    let gated = Arc::new(GatedProvider::new());
    let runtime = build_runtime_with_provider(data_root.path(), Arc::clone(&gated) as _)?;
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
                    "title": null,
                    "model": "test-model"
                }
            }),
        )
        .await
        .context("session/start")?;
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
                    "input": [{ "type": "text", "text": "interrupt me" }],
                    "model": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            }),
        )
        .await
        .context("turn/start")?;
    let turn_id = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::TurnStartResult>,
    >(turn_start_response)?
    .result
    .turn_id;

    // Wait until the assistant item has started streaming.  The provider yields
    // one TextDelta, then blocks, so once we see the delta notification we know
    // deferred_assistant has been stored in the session.
    wait_for_notification_method(&mut notifications_rx, "item/agentMessage/delta").await?;

    // Now interrupt the turn while it is still in-progress.
    let interrupt_response = runtime
        .handle_incoming(
            connection_id,
            serde_json::json!({
                "id": 3,
                "method": "turn/interrupt",
                "params": {
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "reason": "test duplicate bug"
                }
            }),
        )
        .await
        .context("turn/interrupt")?;
    let interrupt_result: devo_server::SuccessResponse<devo_server::TurnInterruptResult> =
        serde_json::from_value(interrupt_response)?;
    assert_eq!(interrupt_result.result.status, TurnStatus::Interrupted);

    // The server broadcasts both turn/interrupted and turn/completed.
    wait_for_notification_method(&mut notifications_rx, "turn/interrupted").await?;
    wait_for_notification_method(&mut notifications_rx, "turn/completed").await?;

    // Rebuild runtime (simulates restart) and resume the session.
    let gated2 = Arc::new(GatedProvider::new());
    let rebuilt = build_runtime_with_provider(data_root.path(), Arc::clone(&gated2) as _)?;
    rebuilt.load_persisted_sessions().await?;
    let (rebuilt_cid, _) = initialize_connection(&rebuilt).await?;

    let resume_response = rebuilt
        .handle_incoming(
            rebuilt_cid,
            serde_json::json!({
                "id": 4,
                "method": "session/resume",
                "params": {
                    "session_id": session_id
                }
            }),
        )
        .await
        .context("session/resume")?;
    let resume_result = serde_json::from_value::<
        devo_server::SuccessResponse<devo_server::SessionResumeResult>,
    >(resume_response)?
    .result;

    // The crucial assertion: no two consecutive items should have the same
    // kind if they are Assistant or Reasoning — those are the types that
    // were being duplicated by the event_task post-loop cleanup race.
    let kinds: Vec<_> = resume_result
        .history_items
        .iter()
        .map(|i| &i.kind)
        .collect();
    for window in kinds.windows(2) {
        if window[0] == window[1] {
            match window[0] {
                SessionHistoryItemKind::Assistant | SessionHistoryItemKind::Reasoning => {
                    anyhow::bail!(
                        "duplicate consecutive {:?} items detected: indices {:?}",
                        window[0],
                        kinds
                            .iter()
                            .enumerate()
                            .filter_map(|(idx, k)| {
                                if *k == window[0] { Some(idx) } else { None }
                            })
                            .collect::<Vec<_>>()
                    );
                }
                _ => {}
            }
        }
    }

    // Sanity: there should be exactly one User and one Assistant item.
    let user_count = kinds
        .iter()
        .filter(|k| matches!(k, SessionHistoryItemKind::User))
        .count();
    let assistant_count = kinds
        .iter()
        .filter(|k| matches!(k, SessionHistoryItemKind::Assistant))
        .count();
    assert_eq!(user_count, 1, "expected exactly one User item");
    assert_eq!(
        assistant_count, 1,
        "expected exactly one Assistant item, got history: {kinds:?}"
    );

    Ok(())
}
