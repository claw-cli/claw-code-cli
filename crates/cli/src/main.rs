use anyhow::Result;
use clap::{Parser, Subcommand};
use clawcr_server::{run_server_process, ServerProcessArgs};

mod agent;
mod config;
mod onboarding;

use agent::{run_agent, AgentCli};

/// Top-level `clawcr` command that dispatches to interactive agent mode or one
/// of the supporting runtime subcommands.
#[derive(Debug, Parser)]
#[command(name = "clawcr", version, about = "ClawCR CLI")]
struct Cli {
    #[command(flatten)]
    agent: AgentCli,

    #[command(subcommand)]
    command: Option<Commands>,
}

/// Subcommands exposed by the primary `clawcr` executable.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the transport-facing server runtime.
    Server(ServerProcessArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Server(args)) => run_server_process(args).await,
        None => run_agent(cli.agent).await,
    }
}
