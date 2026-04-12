use anyhow::Result;
use clap::{Parser, Subcommand};
use clawcr_core::{
    AppConfig, AppConfigLoader, FileSystemAppConfigLoader, LoggingBootstrap, LoggingRuntime,
};
use clawcr_server::{ServerProcessArgs, run_server_process};
use clawcr_utils::find_clawcr_home;

mod agent;

use agent::run_agent;

/// Top-level `clawcr` command that dispatches to interactive agent mode or one
/// of the supporting runtime subcommands.
#[derive(Debug, Parser)]
#[command(name = "clawcr", version, about = "ClawCR CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Keep the UI in the main terminal buffer instead of switching to the alternate screen.
    #[arg(long = "no-alt-screen", default_value_t = false)]
    no_alt_screen: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _logging = install_logging(&cli)?;

    match cli.command {
        Some(Command::Server(args)) => run_server_process(args).await,
        None => run_agent(false, cli.no_alt_screen).await,
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
        Some(Command::Server(args)) => args.working_root.as_deref(),
        _ => Some(current_dir.as_path()),
    };
    let app_config = loader.load(workspace_root).unwrap_or_else(|err| {
        eprintln!("warning: failed to load app config for logging: {err}");
        AppConfig::default()
    });
    LoggingBootstrap {
        process_name: logging_process_name(&cli.command),
        config: app_config.logging,
        home_dir,
    }
    .install()
    .map_err(Into::into)
}

fn logging_process_name(command: &Option<Command>) -> &'static str {
    match command {
        Some(Command::Server(_)) => "server",
        None => "cli",
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{Command, ServerProcessArgs, logging_process_name};

    #[test]
    fn logging_process_name_defaults_to_cli() {
        assert_eq!(logging_process_name(&None), "cli");
    }

    #[test]
    fn logging_process_name_uses_server_for_server_subcommand() {
        assert_eq!(
            logging_process_name(&Some(Command::Server(ServerProcessArgs {
                working_root: None,
            }))),
            "server"
        );
    }
}
