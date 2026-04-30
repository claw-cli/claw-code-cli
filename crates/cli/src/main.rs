use anyhow::Result;
use clap::Parser;
use clap::Subcommand;
use clap::builder::PossibleValuesParser;
use clap::builder::TypedValueParser as _;
use devo_core::AppConfig;
use devo_core::AppConfigLoader;
use devo_core::FileSystemAppConfigLoader;
use devo_core::LoggingBootstrap;
use devo_core::LoggingRuntime;
use devo_core::UpdateCheckOutcome;
use devo_core::UpdateChecker;
use devo_core::format_update_notification;
use devo_server::ServerProcessArgs;
use devo_server::ServerTransportMode;
use devo_server::run_server_process;
use devo_utils::find_devo_home;
use tracing_subscriber::filter::LevelFilter;

mod agent_command;
mod doctor_command;
mod prompt_command;

use agent_command::run_agent;
use doctor_command::run_doctor;
use prompt_command::run_prompt;

/// Top-level `devo` command that dispatches to interactive agent mode or one
/// of the supporting runtime subcommands.
///
#[derive(Debug, Parser)]
#[command(name = "devo", version, about = "Devo CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Override the model used for this session.
    #[arg(long, global = true)]
    model: Option<String>,

    /// Override the logging level for this process.
    #[arg(
        long = "log-level",
        global = true,
        value_parser = PossibleValuesParser::new(["trace", "debug", "info", "warn", "error"])
            .try_map(|level| level.parse::<LevelFilter>())
    )]
    log_level: Option<LevelFilter>,
}

fn main() -> Result<()> {
    devo_arg0::run_as(|_paths| async { run_cli().await })
}

async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    let log_level = cli.log_level.map(|level| level.to_string());

    match &cli.command {
        Some(Command::Onboard) => {
            maybe_print_startup_update(&cli).await;
            // Resolve logging config early, install the process-wide file subscriber,
            // and keep its non-blocking writer guard alive for the command lifetime.
            let _logging = install_logging(&cli)?;
            run_agent(/*force_onboarding*/ true, log_level.as_deref()).await
        }
        Some(Command::Prompt { input }) => {
            maybe_print_startup_update(&cli).await;
            let _logging = install_logging(&cli)?;
            run_prompt(input, cli.model.as_deref(), log_level.as_deref()).await
        }
        Some(Command::Doctor) => {
            let _logging = install_logging(&cli)?;
            run_doctor().await
        }
        Some(Command::Server {
            working_root,
            transport,
        }) => {
            let args = ServerProcessArgs {
                working_root: working_root.clone(),
                transport: *transport,
            };
            let _logging = install_server_logging(&args, &cli)?;
            run_server_process(args).await
        }
        None => {
            maybe_print_startup_update(&cli).await;
            let _logging = install_logging(&cli)?;
            run_agent(/*force_onboarding*/ false, log_level.as_deref()).await
        }
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Launch the interactive onboarding flow to configure a model provider.
    Onboard,
    /// Send a single prompt to the model and print the response (non-interactive).
    Prompt {
        /// The prompt text to send to the model.
        input: String,
    },
    /// Diagnose configuration, provider connectivity, and system health.
    Doctor,
    /// Start the runtime server process.
    #[command(hide = true)]
    Server {
        /// Optional workspace root used for project-level config resolution.
        #[arg(long)]
        working_root: Option<std::path::PathBuf>,
        /// Override the transport mode used by this server process.
        #[arg(long, value_enum, hide = true, default_value_t = ServerTransportMode::Config)]
        transport: ServerTransportMode,
    },
}

async fn maybe_print_startup_update(cli: &Cli) {
    let Ok(home_dir) = find_devo_home() else {
        return;
    };
    let app_config = FileSystemAppConfigLoader::new(home_dir.clone())
        .with_cli_overrides(cli_logging_overrides(cli))
        .load(Some(
            std::env::current_dir()
                .ok()
                .as_deref()
                .unwrap_or_else(|| std::path::Path::new(".")),
        ))
        .unwrap_or_else(|_| AppConfig::default());
    let Ok(checker) = UpdateChecker::new(home_dir, app_config.updates) else {
        return;
    };

    if let UpdateCheckOutcome::UpdateAvailable(notification) =
        checker.check_for_startup_update().await
    {
        eprintln!("{}", format_update_notification(&notification));
    }
}

fn install_logging(cli: &Cli) -> Result<LoggingRuntime> {
    let home_dir = find_devo_home()?;
    let app_config = devo_core::FileSystemAppConfigLoader::new(home_dir.clone())
        .with_cli_overrides(cli_logging_overrides(cli))
        .load(Some(std::env::current_dir()?.as_path()))
        .unwrap_or_else(|err| {
            eprintln!("warning: failed to load app config for logging: {err}");
            devo_core::AppConfig::default()
        });
    LoggingBootstrap {
        process_name: "cli",
        config: app_config.logging,
        home_dir,
    }
    .install()
    .map_err(Into::into)
}

fn install_server_logging(args: &ServerProcessArgs, cli: &Cli) -> Result<LoggingRuntime> {
    let home_dir = find_devo_home()?;
    let loader = devo_core::FileSystemAppConfigLoader::new(home_dir.clone())
        .with_cli_overrides(cli_logging_overrides(cli));
    let app_config = loader
        .load(args.working_root.as_deref())
        .unwrap_or_else(|err| {
            eprintln!("warning: failed to load app config for logging: {err}");
            devo_core::AppConfig::default()
        });
    LoggingBootstrap {
        process_name: "server",
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
            toml::Value::String(log_level.to_string()),
        )])),
    )]))
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use pretty_assertions::assert_eq;
    use tracing_subscriber::filter::LevelFilter;

    use super::Cli;
    use super::Command;
    use super::cli_logging_overrides;

    #[test]
    fn cli_parses_supported_log_levels() {
        for (level, expected) in [
            ("trace", LevelFilter::TRACE),
            ("debug", LevelFilter::DEBUG),
            ("info", LevelFilter::INFO),
            ("warn", LevelFilter::WARN),
            ("error", LevelFilter::ERROR),
        ] {
            let cli = Cli::try_parse_from(["devo", "--log-level", level]).expect("parse log level");

            assert!(cli.command.is_none());
            assert_eq!(cli.model, None);
            assert_eq!(cli.log_level, Some(expected));
        }
    }

    #[test]
    fn cli_rejects_unsupported_log_levels() {
        let err = Cli::try_parse_from(["devo", "--log-level", "off"]).expect_err("reject off");

        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn cli_logging_overrides_is_empty_without_log_level() {
        let cli = Cli {
            command: None,
            model: None,
            log_level: None,
        };

        assert_eq!(
            cli_logging_overrides(&cli),
            toml::Value::Table(Default::default())
        );
    }

    #[test]
    fn cli_logging_overrides_sets_logging_level() {
        for (level, expected) in [
            (LevelFilter::TRACE, "trace"),
            (LevelFilter::DEBUG, "debug"),
            (LevelFilter::INFO, "info"),
            (LevelFilter::WARN, "warn"),
            (LevelFilter::ERROR, "error"),
        ] {
            let cli = Cli {
                command: None,
                model: None,
                log_level: Some(level),
            };

            assert_eq!(
                cli_logging_overrides(&cli),
                toml::Value::Table(toml::map::Map::from_iter([(
                    "logging".to_string(),
                    toml::Value::Table(toml::map::Map::from_iter([(
                        "level".to_string(),
                        toml::Value::String(expected.to_string()),
                    )])),
                )]))
            );
        }
    }

    #[test]
    fn startup_update_check_scope_covers_expected_user_facing_commands() {
        for cli in [
            Cli {
                command: None,
                model: None,
                log_level: None,
            },
            Cli {
                command: Some(Command::Onboard),
                model: None,
                log_level: None,
            },
            Cli {
                command: Some(Command::Prompt {
                    input: "hello".to_string(),
                }),
                model: None,
                log_level: None,
            },
        ] {
            assert_eq!(
                matches!(
                    cli.command,
                    None | Some(Command::Onboard) | Some(Command::Prompt { .. })
                ),
                true
            );
        }
    }

    #[test]
    fn startup_update_check_scope_skips_server_and_doctor() {
        let doctor = Cli {
            command: Some(Command::Doctor),
            model: None,
            log_level: None,
        };
        let server = Cli {
            command: Some(Command::Server {
                working_root: None,
                transport: devo_server::ServerTransportMode::Config,
            }),
            model: None,
            log_level: None,
        };

        assert_eq!(
            matches!(
                doctor.command,
                None | Some(Command::Onboard) | Some(Command::Prompt { .. })
            ),
            false
        );
        assert_eq!(
            matches!(
                server.command,
                None | Some(Command::Onboard) | Some(Command::Prompt { .. })
            ),
            false
        );
    }
}
