use anyhow::Result;
use clap::Parser;
use clawcr_server::{run_server_process, ServerProcessArgs};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    run_server_process(ServerProcessArgs::parse()).await
}
