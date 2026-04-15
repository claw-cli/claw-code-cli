use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
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

    /// Override the logging level for this process.
    #[arg(long = "log-level", global = true, value_enum)]
    log_level: Option<LogLevel>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _logging = install_logging(&cli)?;

    match cli.command {
        Some(Command::Server(args)) => run_server_process(args).await,
        Some(Command::Onboard) => {
            run_agent(true, cli.no_alt_screen, cli.log_level.map(LogLevel::as_str)).await
        }
        None => {
            run_agent(
                false,
                cli.no_alt_screen,
                cli.log_level.map(LogLevel::as_str),
            )
            .await
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Launch the interactive onboarding flow to configure a model provider.
    Onboard,
    /// Start the transport-facing server runtime.
    Server(ServerProcessArgs),
}

fn install_logging(cli: &Cli) -> Result<LoggingRuntime> {
    let home_dir = find_clawcr_home()?;
    let loader = FileSystemAppConfigLoader::new(home_dir.clone())
        .with_cli_overrides(cli_logging_overrides(cli));
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

fn cli_logging_overrides(cli: &Cli) -> toml::Value {
    let Some(log_level) = cli.log_level else {
        return toml::Value::Table(Default::default());
    };

    toml::Value::Table(toml::map::Map::from_iter([(
        "logging".to_string(),
        toml::Value::Table(toml::map::Map::from_iter([(
            "level".to_string(),
            toml::Value::String(log_level.as_str().to_string()),
        )])),
    )]))
}

fn logging_process_name(command: &Option<Command>) -> &'static str {
    match command {
        Some(Command::Onboard) => "onboard",
        Some(Command::Server(_)) => "server",
        None => "cli",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{
        Cli, Command, LogLevel, ServerProcessArgs, cli_logging_overrides, logging_process_name,
    };

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

    #[test]
    fn logging_process_name_uses_onboard_for_onboard_subcommand() {
        assert_eq!(logging_process_name(&Some(Command::Onboard)), "onboard");
    }

    #[test]
    fn cli_logging_overrides_is_empty_without_log_level() {
        let cli = Cli {
            command: None,
            no_alt_screen: false,
            log_level: None,
        };

        assert_eq!(
            cli_logging_overrides(&cli),
            toml::Value::Table(Default::default())
        );
    }

    #[test]
    fn cli_logging_overrides_sets_logging_level() {
        let cli = Cli {
            command: None,
            no_alt_screen: false,
            log_level: Some(LogLevel::Debug),
        };

        assert_eq!(
            cli_logging_overrides(&cli),
            toml::Value::Table(toml::map::Map::from_iter([(
                "logging".to_string(),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "level".to_string(),
                    toml::Value::String("debug".to_string()),
                )])),
            )]))
        );
    }
}
