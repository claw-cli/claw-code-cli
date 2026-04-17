use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use clawcr_core::{
    AppConfig, AppConfigLoader, FileSystemAppConfigLoader, LoggingBootstrap, LoggingRuntime,
    ModelCatalog, PresetModelCatalog, load_config, resolve_provider_settings,
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

    /// Override the model used for this session.
    #[arg(long, global = true)]
    model: Option<String>,

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
            run_agent(
                true,
                cli.no_alt_screen,
                cli.log_level.map(LogLevel::as_str),
                cli.model.as_deref(),
            )
            .await
        }
        Some(Command::Prompt { input }) => {
            run_prompt(
                &input,
                cli.model.as_deref(),
                cli.log_level.map(LogLevel::as_str),
            )
            .await
        }
        Some(Command::Doctor) => run_doctor().await,
        None => {
            run_agent(
                false,
                cli.no_alt_screen,
                cli.log_level.map(LogLevel::as_str),
                cli.model.as_deref(),
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
    /// Send a single prompt to the model and print the response (non-interactive).
    Prompt {
        /// The prompt text to send to the model.
        input: String,
    },
    /// Diagnose configuration, provider connectivity, and system health.
    Doctor,
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
        Some(Command::Prompt { .. }) => "prompt",
        Some(Command::Doctor) => "doctor",
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

async fn run_prompt(
    input: &str,
    model_override: Option<&str>,
    _log_level: Option<&str>,
) -> Result<()> {
    use clawcr_core::{SessionConfig, SessionState, default_base_instructions};
    use clawcr_tools::{ToolOrchestrator, ToolRegistry};

    let cwd = std::env::current_dir()?;
    let _stored_config = load_config().unwrap_or_default();
    let mut resolved = resolve_provider_settings()
        .map_err(|e| anyhow::anyhow!("failed to resolve provider: {e}"))?;

    if let Some(model) = model_override {
        resolved.model = model.to_string();
    }

    let home_dir = find_clawcr_home()?;
    let provider =
        clawcr_server::load_server_provider(&home_dir.join("config.toml"), Some(&resolved.model))?;

    let mut session_state = SessionState::new(SessionConfig::default(), cwd.clone());
    session_state.push_message(clawcr_core::Message::user(input.to_string()));

    let registry = {
        let mut reg = ToolRegistry::new();
        clawcr_tools::register_builtin_tools(&mut reg);
        std::sync::Arc::new(reg)
    };
    let orchestrator = ToolOrchestrator::new(std::sync::Arc::clone(&registry));
    let model_catalog = PresetModelCatalog::load()?;

    let turn_config = clawcr_core::TurnConfig {
        model: model_catalog
            .get(&resolved.model)
            .cloned()
            .unwrap_or_else(|| clawcr_core::Model {
                slug: resolved.model.clone(),
                base_instructions: default_base_instructions().to_string(),
                ..Default::default()
            }),
        thinking_selection: None,
    };

    eprintln!("clawcr [prompt] model={} sending...", resolved.model);

    let result = clawcr_core::query(
        &mut session_state,
        &turn_config,
        provider.provider.as_ref(),
        registry,
        &orchestrator,
        None,
    )
    .await;

    match result {
        Ok(()) => {
            let reply = session_state.messages.iter().rev().find_map(|m| {
                if m.role != clawcr_core::Role::Assistant {
                    return None;
                }
                m.content
                    .iter()
                    .filter_map(|block| match block {
                        clawcr_core::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .next()
            });
            match reply {
                Some(text) => println!("{}", text),
                None => eprintln!("clawcr [prompt] empty response"),
            }
        }
        Err(e) => {
            anyhow::bail!("prompt failed: {e}");
        }
    }

    Ok(())
}

async fn run_doctor() -> Result<()> {
    use colored::Colorize;
    use std::process::Command;

    println!("{}", "=== Claw CR Doctor ===".bold());
    println!();

    let mut all_ok = true;

    println!("{} Rust toolchain:", "✓".green().bold());
    let rustc = Command::new("rustc").arg("--version").output();
    match rustc {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  {}", version);
        }
        Err(e) => {
            println!("  {} rustc not found: {}", "✗".red(), e);
            all_ok = false;
        }
    }
    println!();

    println!("{} Config home (CLAWCR_HOME):", "✓".green().bold());
    match find_clawcr_home() {
        Ok(home) => {
            println!("  {}", home.display());
        }
        Err(e) => {
            println!("  {} {}", "✗".red(), e);
            all_ok = false;
        }
    }
    println!();

    println!("{} Config file:", "✓".green().bold());
    if let Ok(home) = find_clawcr_home() {
        let config_path = home.join("config.toml");
        if config_path.exists() {
            println!("  {} {}", "found".green(), config_path.display());
            let content = std::fs::read_to_string(&config_path).unwrap_or_default();
            if content.contains("api_key") && content.contains("base_url") {
                println!("  {} api_key and base_url configured", "✓".green());
            } else {
                println!("  {} api_key or base_url missing", "!".yellow());
                all_ok = false;
            }
            let model_line = content.lines().find(|l| l.starts_with("model"));
            if let Some(line) = model_line {
                println!("  default model: {}", line.trim());
            } else {
                println!("  {} no default model set", "!".yellow());
            }
        } else {
            println!(
                "  {} not found at {}",
                "missing".yellow(),
                config_path.display()
            );
            println!("  Run `clawcr onboard` to create it.");
            all_ok = false;
        }
    }
    println!();

    println!("{} Provider resolution:", "✓".green().bold());
    match resolve_provider_settings() {
        Ok(resolved) => {
            println!("  provider:   {}", resolved.provider_id);
            println!("  model:      {}", resolved.model);
            println!(
                "  base_url:   {}",
                resolved.base_url.unwrap_or("default".into())
            );
            println!("  wire_api:   {:?}", resolved.wire_api);
            if resolved.api_key.is_some() {
                println!("  api_key:    {} (set)", "✓".green());
            } else {
                println!("  api_key:    {} (not set)", "✗".red());
                all_ok = false;
            }
        }
        Err(e) => {
            println!("  {} {}", "✗".red(), e);
            all_ok = false;
        }
    }
    println!();

    println!("{} Model catalog:", "✓".green().bold());
    match clawcr_core::PresetModelCatalog::load() {
        Ok(catalog) => {
            let count = catalog.into_inner().len();
            println!("  {} builtin models loaded", count);
        }
        Err(e) => {
            println!("  {} failed to load: {}", "✗".red(), e);
            all_ok = false;
        }
    }
    println!();

    if all_ok {
        println!("{}", "All checks passed. Ready to use!".green().bold());
    } else {
        println!(
            "{}",
            "Some checks failed. See above for details.".yellow().bold()
        );
        std::process::exit(1);
    }

    Ok(())
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
            model: None,
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
            model: None,
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
