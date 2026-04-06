use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use clawcr_core::{AppConfigLoader, FileSystemAppConfigLoader};
use clawcr_utils::FileSystemConfigPathResolver;

use crate::{resolve_listen_targets, run_listeners, ListenTarget, ServerRuntime};

/// Command-line arguments accepted by the standalone server process entrypoint.
#[derive(Debug, Clone, Parser)]
#[command(name = "clawcr-server", version, about)]
pub struct ServerProcessArgs {
    /// Optional workspace root used for project-level config resolution.
    #[arg(long)]
    pub workspace_root: Option<PathBuf>,
}

/// Starts the transport-facing server runtime using the resolved application
/// configuration and listener set.
pub async fn run_server_process(args: ServerProcessArgs) -> Result<()> {
    let resolver = FileSystemConfigPathResolver::from_env()?;
    let loader = FileSystemAppConfigLoader::new(
        resolver
            .user_config_dir()
            .parent()
            .expect("config dir should have a parent home directory")
            .to_path_buf(),
    );
    let config = loader.load(args.workspace_root.as_deref())?;
    let listen_targets = resolve_listen_targets(&config.server.listen)?;
    let effective_listen = listen_targets
        .iter()
        .map(|target| match target {
            ListenTarget::Stdio => "stdio://".to_string(),
            ListenTarget::WebSocket { bind_address } => format!("ws://{bind_address}"),
        })
        .collect::<Vec<_>>();

    tracing::info!(
        user_config = %resolver.user_config_file().display(),
        project_config = args
            .workspace_root
            .as_deref()
            .map(|root| resolver.project_config_file(root).display().to_string())
            .unwrap_or_else(|| "<none>".into()),
        configured_listen = ?config.server.listen,
        effective_listen = ?effective_listen,
        max_connections = config.server.max_connections,
        "loaded server config"
    );

    let runtime = ServerRuntime::new(resolver.user_config_dir());
    tracing::info!("server bootstrap completed; starting listeners");
    tokio::select! {
        result = run_listeners(runtime, &config.server.listen) => {
            result?;
        }
        result = tokio::signal::ctrl_c() => {
            result?;
            tracing::info!("server shutdown requested");
        }
    }
    Ok(())
}
