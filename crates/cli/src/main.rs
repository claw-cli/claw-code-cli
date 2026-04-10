use anyhow::Result;
use clap::{Parser, Subcommand};
use clawcr_core::{
    AppConfig, AppConfigLoader, FileSystemAppConfigLoader, LoggingBootstrap, LoggingRuntime,
};
use clawcr_server::{run_server_process, ServerProcessArgs};
use clawcr_utils::find_clawcr_home;

mod agent;
mod config;

use agent::run_agent;

/// Top-level `clawcr` command that dispatches to interactive agent mode or one
/// of the supporting runtime subcommands.
#[derive(Debug, Parser)]
#[command(name = "clawcr", version, about = "ClawCR CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _logging = install_logging(&cli)?;

    match cli.command {
        Some(Command::Server(args)) => run_server_process(args).await,
        None => run_agent(false).await,
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the transport-facing server runtime.
    Server(ServerProcessArgs),
}

fn install_logging(cli: &Cli) -> Result<LoggingRuntime> {
    let home_dir = find_clawcr_home()?;
    let loader = FileSystemAppConfigLoader::new(home_dir.clone());
    let current_dir = std::env::current_dir()?;
    let workspace_root = match &cli.command {
        Some(Command::Server(args)) => args.workspace_root.as_deref(),
        _ => Some(current_dir.as_path()),
    };
    let app_config = loader.load(workspace_root).unwrap_or_else(|err| {
        eprintln!("warning: failed to load app config for logging: {err}");
        AppConfig::default()
    });
    LoggingBootstrap {
        process_name: "cli",
        config: app_config.logging,
        home_dir,
    }
    .install()
    .map_err(Into::into)
}
