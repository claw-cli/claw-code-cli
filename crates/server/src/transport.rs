use std::sync::Arc;

use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::{ClientTransportKind, ServerRuntime};

/// Default bind address used when the WebSocket transport is selected without
/// an explicit host-and-port suffix.
pub const DEFAULT_WEBSOCKET_BIND_ADDRESS: &str = "127.0.0.1:3210";

/// Enumerates the supported listener targets parsed from server config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenTarget {
    /// Start the process-scoped stdio transport.
    Stdio,
    /// Start a WebSocket listener at one host and port pair.
    WebSocket {
        /// The socket address host and port, without the `ws://` prefix.
        bind_address: String,
    },
}

/// Parses one configured listen-address string into a typed transport target.
pub fn parse_listen_target(value: &str) -> Result<ListenTarget> {
    if value.eq_ignore_ascii_case("stdio://") || value.eq_ignore_ascii_case("stdio") {
        return Ok(ListenTarget::Stdio);
    }
    if let Some(bind_address) = value.strip_prefix("ws://") {
        return Ok(ListenTarget::WebSocket {
            bind_address: if bind_address.is_empty() {
                DEFAULT_WEBSOCKET_BIND_ADDRESS.to_string()
            } else {
                bind_address.to_string()
            },
        });
    }
    Err(anyhow!("unsupported listen target: {value}"))
}

/// Resolves the configured listen-address strings into the concrete listener
/// targets the process will start.
pub fn resolve_listen_targets(listen: &[String]) -> Result<Vec<ListenTarget>> {
    if listen.is_empty() {
        Ok(vec![
            ListenTarget::Stdio,
            ListenTarget::WebSocket {
                bind_address: DEFAULT_WEBSOCKET_BIND_ADDRESS.to_string(),
            },
        ])
    } else {
        listen
            .iter()
            .map(|value| parse_listen_target(value))
            .collect::<Result<Vec<_>>>()
    }
}

/// Runs every configured listener target until shutdown.
pub async fn run_listeners(runtime: Arc<ServerRuntime>, listen: &[String]) -> Result<()> {
    let targets = resolve_listen_targets(listen)?;

    let mut tasks = Vec::new();
    for target in targets {
        let runtime = Arc::clone(&runtime);
        tasks.push(tokio::spawn(async move {
            match target {
                ListenTarget::Stdio => {
                    tracing::info!("stdio listener active on stdin/stdout");
                    run_stdio(runtime).await
                }
                ListenTarget::WebSocket { bind_address } => {
                    tracing::info!(bind_address = %bind_address, "websocket listener starting");
                    run_websocket(runtime, &bind_address).await
                }
            }
        }));
    }

    for task in tasks {
        task.await??;
    }
    Ok(())
}

async fn run_stdio(runtime: Arc<ServerRuntime>) -> Result<()> {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let sender_clone = sender.clone();
    let connection_id = runtime
        .register_connection(ClientTransportKind::Stdio, sender)
        .await;
    tracing::info!(connection_id, "stdio connection established");

    let stdout_task = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = receiver.recv().await {
            let line = serde_json::to_vec(&message).expect("serialize stdio response");
            stdout.write_all(&line).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
        Result::<()>::Ok(())
    });

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if let Some(response) = runtime.handle_incoming(connection_id, value).await {
            let _ = sender_clone.send(response);
        }
    }

    runtime.unregister_connection(connection_id).await;
    tracing::info!(connection_id, "stdio connection closed");
    stdout_task.abort();
    Ok(())
}

async fn run_websocket(runtime: Arc<ServerRuntime>, bind_address: &str) -> Result<()> {
    let listener = TcpListener::bind(bind_address).await?;
    tracing::info!(bind_address = %bind_address, "websocket listener bound");
    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            tracing::info!(remote_addr = %remote_addr, "websocket client connected");
            if let Err(error) = handle_websocket_connection(runtime, stream).await {
                tracing::warn!(remote_addr = %remote_addr, error = %error, "websocket connection closed with error");
            }
            tracing::info!(remote_addr = %remote_addr, "websocket client disconnected");
        });
    }
}

async fn handle_websocket_connection(
    runtime: Arc<ServerRuntime>,
    stream: tokio::net::TcpStream,
) -> Result<()> {
    let websocket = accept_async(stream).await?;
    let (mut writer, mut reader) = websocket.split();
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let sender_clone = sender.clone();
    let connection_id = runtime
        .register_connection(ClientTransportKind::WebSocket, sender)
        .await;
    tracing::info!(connection_id, "websocket connection established");

    let writer_task = tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            writer
                .send(Message::Text(
                    serde_json::to_string(&message)
                        .expect("serialize websocket response")
                        .into(),
                ))
                .await?;
        }
        Result::<()>::Ok(())
    });

    while let Some(frame) = reader.next().await {
        let frame = frame?;
        match frame {
            Message::Text(text) => {
                let value: serde_json::Value = serde_json::from_str(&text)?;
                if let Some(response) = runtime.handle_incoming(connection_id, value).await {
                    let _ = sender_clone.send(response);
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    runtime.unregister_connection(connection_id).await;
    tracing::info!(connection_id, "websocket connection closed");
    writer_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_listen_target, resolve_listen_targets, ListenTarget, DEFAULT_WEBSOCKET_BIND_ADDRESS,
    };

    #[test]
    fn parse_stdio_target() {
        assert_eq!(
            parse_listen_target("stdio://").expect("stdio"),
            ListenTarget::Stdio
        );
    }

    #[test]
    fn parse_ws_target() {
        assert_eq!(
            parse_listen_target("ws://127.0.0.1:9000").expect("ws"),
            ListenTarget::WebSocket {
                bind_address: "127.0.0.1:9000".into(),
            }
        );
    }

    #[test]
    fn parse_ws_target_without_bind_address_uses_default() {
        assert_eq!(
            parse_listen_target("ws://").expect("ws"),
            ListenTarget::WebSocket {
                bind_address: DEFAULT_WEBSOCKET_BIND_ADDRESS.into(),
            }
        );
    }

    #[test]
    fn resolve_empty_listener_list_defaults_to_stdio() {
        assert_eq!(
            resolve_listen_targets(&[]).expect("targets"),
            vec![
                ListenTarget::Stdio,
                ListenTarget::WebSocket {
                    bind_address: DEFAULT_WEBSOCKET_BIND_ADDRESS.into(),
                },
            ]
        );
    }
}
