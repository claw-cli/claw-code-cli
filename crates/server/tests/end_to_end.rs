use std::net::TcpListener as StdTcpListener;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::process::Command;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use clawcr_core::BuiltinModelCatalog;
use clawcr_provider::{ModelProvider, ModelRequest, ModelResponse, StreamEvent};
use clawcr_server::{ServerRuntime, ServerRuntimeDependencies};
use clawcr_tools::ToolRegistry;
use futures::stream;

fn write_test_config(home_dir: &TempDir, listen: &[&str]) -> Result<()> {
    #[cfg(windows)]
    let config_dir = home_dir.path().join(".clawcr");

    #[cfg(unix)]
    let config_dir = home_dir.path().join(".config").join("clawcr");

    std::fs::create_dir_all(&config_dir)?;
    let listen_entries = listen
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let config = format!(
        "[server]\nlisten = [{listen_entries}]\nmax_connections = 32\nevent_buffer_size = 128\nidle_session_timeout_secs = 300\npersist_ephemeral_sessions = false\n"
    );
    std::fs::write(config_dir.join("config.toml"), config)?;
    Ok(())
}

fn initialize_request(transport: &str) -> serde_json::Value {
    serde_json::json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "client_name": "e2e-test",
            "client_version": "1.0.0",
            "transport": transport,
            "supports_streaming": true,
            "supports_binary_images": false,
            "opt_out_notification_methods": [],
        }
    })
}

struct PendingProvider;

#[async_trait]
impl ModelProvider for PendingProvider {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse> {
        anyhow::bail!("test provider does not support complete")
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamEvent>> + Send>>> {
        Ok(Box::pin(stream::pending()))
    }

    fn name(&self) -> &str {
        "pending-test-provider"
    }
}

#[tokio::test]
async fn stdio_server_process_supports_handshake_and_session_start() -> Result<()> {
    let home_dir = TempDir::new()?;
    write_test_config(&home_dir, &["stdio://"])?;

    let test_cwd = home_dir.path().to_string_lossy().into_owned();

    let mut child = Command::new(env!("CARGO_BIN_EXE_clawcr-server"))
        .env("USERPROFILE", home_dir.path())
        .env("HOME", home_dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("spawn clawcr-server child process")?;

    let mut stdin = child.stdin.take().context("capture child stdin")?;
    let stdout = child.stdout.take().context("capture child stdout")?;
    let stderr = child.stderr.take().context("capture child stderr")?;
    let mut stdout_reader = AsyncBufReader::new(stdout).lines();
    let mut stderr_reader = AsyncBufReader::new(stderr);

    stdin
        .write_all(format!("{}\n", initialize_request("stdio")).as_bytes())
        .await?;
    stdin.flush().await?;

    let line = read_stdio_line(&mut stdout_reader, "initialize response").await?;
    let initialize_response: serde_json::Value =
        parse_stdio_json_line(&mut child, &mut stderr_reader, "initialize response", &line).await?;
    assert_eq!(initialize_response["id"], serde_json::json!(1));
    assert_eq!(
        initialize_response["result"]["server_name"],
        serde_json::json!("clawcr-server")
    );

    stdin.write_all(b"{\"method\":\"initialized\"}\n").await?;
    stdin
        .write_all(
            format!(
                "{}\n",
                serde_json::json!({
                    "id": 2,
                    "method": "session/start",
                    "params": {
                        "cwd": test_cwd,
                        "ephemeral": false,
                        "title": "End To End",
                        "model": "test-model"
                    }
                })
            )
            .as_bytes(),
        )
        .await?;
    stdin.flush().await?;

    let first_message =
        read_stdio_line(&mut stdout_reader, "first post-session/start message").await?;
    let second_message =
        read_stdio_line(&mut stdout_reader, "second post-session/start message").await?;

    let first_value = parse_stdio_json_line(
        &mut child,
        &mut stderr_reader,
        "first post-session/start message",
        &first_message,
    )
    .await?;
    let second_value = parse_stdio_json_line(
        &mut child,
        &mut stderr_reader,
        "second post-session/start message",
        &second_message,
    )
    .await?;
    let messages = [first_value, second_value];

    let session_started = messages
        .iter()
        .find(|value| value.get("method") == Some(&serde_json::json!("session/started")))
        .context("find session/started notification")?;
    let session_start_response = messages
        .iter()
        .find(|value| value.get("id") == Some(&serde_json::json!(2)))
        .context("find session/start response")?;

    assert_eq!(
        session_started["params"]["session"]["cwd"],
        serde_json::json!(test_cwd)
    );
    assert_eq!(
        session_start_response["result"]["resolved_model"],
        serde_json::json!("test-model")
    );

    drop(stdin);
    child.kill().await.ok();
    let _ = child.wait().await;
    Ok(())
}

#[tokio::test]
async fn websocket_listener_supports_handshake_subscription_and_turn_lifecycle() -> Result<()> {
    let port = {
        let listener = StdTcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        port
    };
    let bind_address = format!("127.0.0.1:{port}");
    let runtime = ServerRuntime::new(
        std::env::temp_dir(),
        ServerRuntimeDependencies::new(
            Arc::new(PendingProvider),
            Arc::new(ToolRegistry::new()),
            "test-model".to_string(),
            Arc::new(BuiltinModelCatalog::default()),
        ),
    );
    let listen = vec![format!("ws://{bind_address}")];
    let listener_task =
        tokio::spawn(
            async move { clawcr_server::run_listeners(Arc::clone(&runtime), &listen).await },
        );

    tokio::time::sleep(Duration::from_millis(200)).await;

    let (mut socket, _) = connect_async(format!("ws://{bind_address}")).await?;
    socket
        .send(Message::Text(
            serde_json::to_string(&initialize_request("web_socket"))?.into(),
        ))
        .await?;

    let initialize_response = read_websocket_json(&mut socket).await?;
    assert_eq!(initialize_response["id"], serde_json::json!(1));
    assert_eq!(
        initialize_response["result"]["server_name"],
        serde_json::json!("clawcr-server")
    );

    socket
        .send(Message::Text(
            serde_json::json!({ "method": "initialized" })
                .to_string()
                .into(),
        ))
        .await?;

    socket
        .send(Message::Text(
            serde_json::json!({
                "id": 2,
                "method": "session/start",
                "params": {
                    "cwd": "C:/repo",
                    "ephemeral": false,
                    "title": null,
                    "model": "test-model"
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    let session_start_messages = read_n_websocket_json(&mut socket, 2).await?;
    let session_started = session_start_messages
        .iter()
        .find(|value| value.get("method") == Some(&serde_json::json!("session/started")))
        .context("find session/started notification")?;
    let session_response = session_start_messages
        .iter()
        .find(|value| value.get("id") == Some(&serde_json::json!(2)))
        .context("find session/start response")?;
    let session_id = session_response["result"]["session_id"]
        .as_str()
        .context("extract session id")?
        .to_string();
    assert_eq!(
        session_started["params"]["session"]["session_id"],
        serde_json::json!(session_id)
    );

    socket
        .send(Message::Text(
            serde_json::json!({
                "id": 3,
                "method": "turn/start",
                "params": {
                    "session_id": session_id,
                    "input": [{ "type": "text", "text": "hello" }],
                    "model": null,
                    "sandbox": null,
                    "approval_policy": null,
                    "cwd": null
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    let turn_start_messages = read_until_websocket_json(
        &mut socket,
        |messages| {
            messages
                .iter()
                .any(|value| value.get("method") == Some(&serde_json::json!("turn/started")))
                && messages
                    .iter()
                    .any(|value| value.get("id") == Some(&serde_json::json!(3)))
        },
        4,
    )
    .await
    .context("read turn/start websocket messages")?;
    let turn_started = turn_start_messages
        .iter()
        .find(|value| value.get("method") == Some(&serde_json::json!("turn/started")))
        .context("find turn/started notification")?;
    let turn_start_response = turn_start_messages
        .iter()
        .find(|value| value.get("id") == Some(&serde_json::json!(3)))
        .context("find turn/start response")?;
    let turn_id = turn_start_response["result"]["turn_id"]
        .as_str()
        .context("extract turn id")?
        .to_string();
    assert_eq!(
        turn_started["params"]["turn"]["turn_id"],
        serde_json::json!(turn_id)
    );

    socket
        .send(Message::Text(
            serde_json::json!({
                "id": 4,
                "method": "turn/interrupt",
                "params": {
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "reason": "e2e test"
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    let interrupt_messages = read_until_websocket_json(
        &mut socket,
        |messages| {
            messages
                .iter()
                .any(|value| value.get("id") == Some(&serde_json::json!(4)))
                && messages.iter().any(|value| {
                    value.get("method") == Some(&serde_json::json!("turn/interrupted"))
                })
                && messages
                    .iter()
                    .any(|value| value.get("method") == Some(&serde_json::json!("turn/completed")))
        },
        8,
    )
    .await
    .context("read turn/interrupt websocket messages")?;
    let interrupt_response = interrupt_messages
        .iter()
        .find(|value| value.get("id") == Some(&serde_json::json!(4)))
        .context("find turn/interrupt response")?;
    let interrupted_event = interrupt_messages
        .iter()
        .find(|value| value.get("method") == Some(&serde_json::json!("turn/interrupted")))
        .context("find turn/interrupted notification")?;
    let completed_event = interrupt_messages
        .iter()
        .find(|value| value.get("method") == Some(&serde_json::json!("turn/completed")))
        .context("find turn/completed notification")?;

    assert_eq!(
        interrupt_response["result"]["status"],
        serde_json::json!("Interrupted")
    );
    assert_eq!(
        interrupted_event["params"]["turn"]["status"],
        serde_json::json!("Interrupted")
    );
    assert_eq!(
        completed_event["params"]["turn"]["status"],
        serde_json::json!("Interrupted")
    );

    listener_task.abort();
    let _ = listener_task.await;
    Ok(())
}

async fn read_websocket_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Result<serde_json::Value> {
    timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await.context("websocket closed")?? {
                Message::Text(text) => {
                    return serde_json::from_str(text.as_str()).map_err(Into::into)
                }
                _ => continue,
            }
        }
    })
    .await
    .context("timed out waiting for websocket message")?
}

async fn read_n_websocket_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    count: usize,
) -> Result<Vec<serde_json::Value>> {
    let mut values = Vec::with_capacity(count);
    while values.len() < count {
        values.push(read_websocket_json(socket).await?);
    }
    Ok(values)
}

async fn read_until_websocket_json<F>(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    predicate: F,
    max_messages: usize,
) -> Result<Vec<serde_json::Value>>
where
    F: Fn(&[serde_json::Value]) -> bool,
{
    let mut values = Vec::new();
    while values.len() < max_messages {
        values.push(read_websocket_json(socket).await?);
        if predicate(&values) {
            return Ok(values);
        }
    }
    anyhow::bail!("did not observe expected websocket messages within {max_messages} frames")
}

async fn parse_stdio_json_line(
    child: &mut tokio::process::Child,
    stderr_reader: &mut AsyncBufReader<tokio::process::ChildStderr>,
    context: &str,
    line: &str,
) -> Result<serde_json::Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        let mut stderr_output = String::new();
        stderr_reader.read_to_string(&mut stderr_output).await?;
        let exit_status = child.try_wait()?;
        anyhow::bail!(
            "{context} was empty; child_exit_status={exit_status:?}; child_stderr={stderr_output:?}"
        );
    }

    serde_json::from_str(trimmed).with_context(|| {
        let stderr_output = String::new();
        let _ = stderr_output;
        let exit_status = child.try_wait().ok().flatten();
        format!(
            "{context} was not valid JSON; raw_stdout_line={trimmed:?}; child_exit_status={exit_status:?}"
        )
    })
}

async fn read_stdio_line(
    reader: &mut tokio::io::Lines<AsyncBufReader<tokio::process::ChildStdout>>,
    context: &str,
) -> Result<String> {
    timeout(Duration::from_secs(5), reader.next_line())
        .await
        .with_context(|| format!("timed out waiting for {context}"))?
        .with_context(|| format!("failed reading {context} from child stdout"))?
        .with_context(|| format!("{context} reached EOF before a line was produced"))
}
