use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clawcr_utils::{current_user_config_file, FileSystemConfigPathResolver};
use serde::{Deserialize, Serialize};

use clawcr_provider::ModelProvider;

/// Persisted provider configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// The fully-resolved provider ready for use.
pub struct ResolvedProvider {
    pub provider: Box<dyn ModelProvider>,
    pub model: String,
}

// ---------------------------------------------------------------------------
// Config file I/O
// ---------------------------------------------------------------------------

/// `~/.clawcr/config.toml`
pub fn config_path() -> Result<PathBuf> {
    current_user_config_file().context("could not determine user config path")
}

/// The previous JSON location under the current `.clawcr` directory.
fn legacy_json_config_path() -> Result<PathBuf> {
    let resolver = FileSystemConfigPathResolver::from_env()
        .context("could not determine home directory for legacy config path")?;
    Ok(resolver.user_config_dir().join("config.json"))
}

/// The older pre-spec JSON location used by early CLI builds.
fn legacy_cli_config_path() -> Result<PathBuf> {
    let resolver = FileSystemConfigPathResolver::from_env()
        .context("could not determine home directory for legacy config path")?;
    Ok(resolver
        .user_config_dir()
        .parent()
        .expect("config dir should have a parent home directory")
        .join(".claw-code-rust")
        .join("config.json"))
}

/// Load a JSON config file from one of the legacy locations.
fn load_legacy_json_config(path: &Path) -> Result<AppConfig> {
    let data = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&data).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn load_config() -> Result<AppConfig> {
    let path = config_path()?;
    if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let cfg: AppConfig =
            toml::from_str(&data).with_context(|| format!("failed to parse {}", path.display()))?;
        return Ok(cfg);
    }

    let legacy_json_path = legacy_json_config_path()?;
    if legacy_json_path.exists() {
        let cfg = load_legacy_json_config(&legacy_json_path)?;
        save_config(&cfg)?;
        return Ok(cfg);
    }

    let legacy_cli_path = legacy_cli_config_path()?;
    if legacy_cli_path.exists() {
        let cfg = load_legacy_json_config(&legacy_cli_path)?;
        save_config(&cfg)?;
        return Ok(cfg);
    }

    Ok(AppConfig::default())
}

pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let toml = toml::to_string_pretty(config)?;
    std::fs::write(&path, toml).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Env-var detection
// ---------------------------------------------------------------------------

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Build a partial config from environment variables.
fn env_config() -> AppConfig {
    let api_key =
        env_non_empty("ANTHROPIC_API_KEY").or_else(|| env_non_empty("ANTHROPIC_AUTH_TOKEN"));
    let base_url = env_non_empty("ANTHROPIC_BASE_URL");

    // If any Anthropic auth is present, provider is anthropic
    let provider = if api_key.is_some() {
        Some("anthropic".to_string())
    } else if env_non_empty("OPENAI_API_KEY").is_some()
        || env_non_empty("OPENAI_BASE_URL").is_some()
    {
        Some("openai".to_string())
    } else {
        None
    };

    AppConfig {
        provider,
        model: None,
        base_url,
        api_key,
    }
}

// ---------------------------------------------------------------------------
// Provider resolution: CLI flags > env vars > config file > onboarding
// ---------------------------------------------------------------------------

pub fn resolve_provider(
    cli_provider: Option<&str>,
    cli_model: Option<&str>,
    cli_ollama_url: &str,
    interactive: bool,
) -> Result<ResolvedProvider> {
    let env = env_config();
    let file = load_config().unwrap_or_default();

    // Merge layers: CLI > env > file
    let provider_name = cli_provider
        .map(|s| s.to_string())
        .or(env.provider.clone())
        .or(file.provider.clone());

    let api_key = env.api_key.clone().or(file.api_key.clone());
    let base_url = env.base_url.clone().or(file.base_url.clone());
    let model_override = cli_model
        .map(|s| s.to_string())
        .or(env.model.clone())
        .or(file.model.clone());

    // If we have a provider, build it
    if let Some(ref name) = provider_name {
        return build_provider(name, model_override, api_key, base_url, cli_ollama_url);
    }

    // Nothing resolved — try onboarding or error
    if interactive {
        eprintln!("No provider configured. Starting first-run setup...\n");
        let onboard_config = crate::onboarding::run_onboarding()?;
        save_config(&onboard_config)?;

        let name = onboard_config.provider.as_deref().unwrap_or("stub");
        return build_provider(
            name,
            model_override.or(onboard_config.model),
            onboard_config.api_key,
            onboard_config.base_url,
            cli_ollama_url,
        );
    }

    anyhow::bail!(
        "No provider configured. Set ANTHROPIC_API_KEY / ANTHROPIC_AUTH_TOKEN, \
         or run interactively to complete setup."
    )
}

fn build_provider(
    name: &str,
    model: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    ollama_url: &str,
) -> Result<ResolvedProvider> {
    match name {
        "anthropic" => {
            let key = api_key
                .context("Anthropic provider requires ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN")?;
            let default_model = "claude-sonnet-4-20250514".to_string();
            let model = model.unwrap_or(default_model);
            eprintln!("Using Anthropic API (model: {})", model);

            let p = if let Some(url) = base_url {
                eprintln!("  base_url: {}", url);
                clawcr_provider::anthropic::AnthropicProvider::new_with_url(&key, url)
            } else {
                clawcr_provider::anthropic::AnthropicProvider::new(&key)
            };
            Ok(ResolvedProvider {
                provider: Box::new(p),
                model,
            })
        }
        "ollama" => {
            let model = model.unwrap_or_else(|| "qwen3.5:9b".into());
            let raw_url = base_url.as_deref().unwrap_or(ollama_url);
            let url = ensure_openai_v1(raw_url);
            eprintln!("Using Ollama (url: {}, model: {})", url, model);
            let mut p = clawcr_provider::openai_compat::OpenAICompatProvider::new(&url);
            if let Some(ref key) = api_key {
                p = p.with_api_key(key);
            }
            Ok(ResolvedProvider {
                provider: Box::new(p),
                model,
            })
        }
        "openai" => {
            let raw_url = base_url.unwrap_or_else(|| "https://api.openai.com".into());
            let url = ensure_openai_v1(&raw_url);
            let model = model.unwrap_or_else(|| "gpt-4o".into());
            eprintln!("Using OpenAI-compat (url: {}, model: {})", url, model);
            let mut p = clawcr_provider::openai_compat::OpenAICompatProvider::new(&url);
            if let Some(key) = api_key {
                p = p.with_api_key(key);
            }
            Ok(ResolvedProvider {
                provider: Box::new(p),
                model,
            })
        }
        other => {
            anyhow::bail!(
                "Unknown provider '{}'. Use: anthropic, ollama, openai",
                other
            );
        }
    }
}

/// async-openai appends `/chat/completions` to the base URL, so Ollama/OpenAI
/// endpoints need a `/v1` suffix. Append it if missing.
fn ensure_openai_v1(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        trimmed.to_string()
    } else {
        format!("{}/v1", trimmed)
    }
}

// ---------------------------------------------------------------------------
// Ollama availability check + auto-start
// ---------------------------------------------------------------------------

/// Parse host and port from an Ollama URL (e.g. "http://localhost:11434").
fn parse_ollama_addr(url: &str) -> (String, u16) {
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);
    let without_path = without_scheme.split('/').next().unwrap_or(without_scheme);
    if let Some((host, port_str)) = without_path.rsplit_once(':') {
        let port = port_str.parse().unwrap_or(11434);
        (host.to_string(), port)
    } else {
        (without_path.to_string(), 11434)
    }
}

/// Check if Ollama is listening on the given URL.
fn is_ollama_reachable(url: &str) -> bool {
    let (host, port) = parse_ollama_addr(url);
    let addr = format!("{}:{}", host, port);
    std::net::TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        std::time::Duration::from_secs(2),
    )
    .is_ok()
}

/// Ensure Ollama is running. If not, offer to start it (interactive mode)
/// or return an error (non-interactive).
pub fn ensure_ollama(url: &str, interactive: bool) -> Result<()> {
    if is_ollama_reachable(url) {
        return Ok(());
    }

    if !interactive {
        anyhow::bail!(
            "Ollama is not running at {}. Start it with `ollama serve` and try again.",
            url
        );
    }

    eprint!(
        "Ollama is not running at {}. Start it automatically? [Y/n] ",
        url
    );
    std::io::Write::flush(&mut std::io::stderr())?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_lowercase();
    if !answer.is_empty() && answer != "y" && answer != "yes" {
        anyhow::bail!("Ollama is required. Start it with `ollama serve` and try again.");
    }

    eprintln!("Starting Ollama...");
    let child = std::process::Command::new("ollama")
        .arg("serve")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match child {
        Ok(_) => {
            // Wait for Ollama to become available (up to 15 seconds)
            for i in 0..30 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if is_ollama_reachable(url) {
                    eprintln!("Ollama is ready. (took ~{}s)", (i + 1) / 2);
                    return Ok(());
                }
            }
            anyhow::bail!("Ollama was started but did not become reachable within 15 seconds.")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!(
                "Could not find `ollama` in PATH. \
                 Install it from https://ollama.com and try again."
            )
        }
        Err(e) => {
            anyhow::bail!("Failed to start Ollama: {}", e)
        }
    }
}
