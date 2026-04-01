use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use agent_provider::ModelProvider;

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

/// `~/.claude/config.json`
pub fn config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".claude").join("config.json"))
}

pub fn load_config() -> Result<AppConfig> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let cfg: AppConfig = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(cfg)
}

pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(config)?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write {}", path.display()))?;
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
    let api_key = env_non_empty("ANTHROPIC_API_KEY")
        .or_else(|| env_non_empty("ANTHROPIC_AUTH_TOKEN"));
    let base_url = env_non_empty("ANTHROPIC_BASE_URL");

    // If any Anthropic auth is present, provider is anthropic
    let provider = if api_key.is_some() {
        Some("anthropic".to_string())
    } else if env_non_empty("OPENAI_API_KEY").is_some() || env_non_empty("OPENAI_BASE_URL").is_some() {
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
    let model_override = cli_model.map(|s| s.to_string());

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
            let key = api_key.context(
                "Anthropic provider requires ANTHROPIC_API_KEY or ANTHROPIC_AUTH_TOKEN",
            )?;
            let default_model = "claude-sonnet-4-20250514".to_string();
            let model = model.unwrap_or(default_model);
            eprintln!("Using Anthropic API (model: {})", model);

            let mut p = agent_provider::anthropic::AnthropicProvider::new(key);
            if let Some(url) = base_url {
                eprintln!("  base_url: {}", url);
                p = p.with_base_url(url);
            }
            Ok(ResolvedProvider {
                provider: Box::new(p),
                model,
            })
        }
        "ollama" => {
            let model = model.unwrap_or_else(|| "qwen3.5:9b".into());
            let url = base_url.as_deref().unwrap_or(ollama_url);
            eprintln!("Using Ollama (url: {}, model: {})", url, model);
            let mut p = agent_provider::openai_compat::OpenAICompatProvider::new(url);
            if let Some(ref key) = api_key {
                p = p.with_api_key(key);
            }
            Ok(ResolvedProvider {
                provider: Box::new(p),
                model,
            })
        }
        "openai" => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com".into());
            let model = model.unwrap_or_else(|| "gpt-4o".into());
            eprintln!("Using OpenAI-compat (url: {}, model: {})", url, model);
            let mut p = agent_provider::openai_compat::OpenAICompatProvider::new(&url);
            if let Some(key) = api_key {
                p = p.with_api_key(key);
            }
            Ok(ResolvedProvider {
                provider: Box::new(p),
                model,
            })
        }
        other => {
            anyhow::bail!("Unknown provider '{}'. Use: anthropic, ollama, openai", other);
        }
    }
}
