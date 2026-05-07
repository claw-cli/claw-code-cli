use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientTransportKind {
    Stdio,
    WebSocket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Connected,
    Initializing,
    Ready,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeParams {
    pub client_name: String,
    pub client_version: String,
    pub transport: ClientTransportKind,
    pub supports_streaming: bool,
    pub supports_binary_images: bool,
    pub opt_out_notification_methods: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitializeResult {
    pub server_name: String,
    pub server_version: String,
    pub platform_family: String,
    pub platform_os: String,
    pub server_home: PathBuf,
}
