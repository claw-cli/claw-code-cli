use anyhow::{Context, Result};
use clawcr_core::ProviderKind;
use clawcr_utils::current_user_config_file;
use serde::{Deserialize, Serialize};

/// One model entry stored under a provider section in `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfiguredModel {
    /// The model slug or custom model name.
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// One provider-specific configuration block that can store many model entries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ConfiguredModel>,
}

impl ProviderProfile {
    pub(crate) fn is_empty(&self) -> bool {
        self.last_model.is_none()
            && self.default_model.is_none()
            && self.base_url.is_none()
            && self.api_key.is_none()
            && self.models.is_empty()
    }
}

/// Persisted provider configuration grouped by provider family.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default, skip_serializing_if = "ProviderProfile::is_empty")]
    pub anthropic: ProviderProfile,
    #[serde(default, skip_serializing_if = "ProviderProfile::is_empty")]
    pub openai: ProviderProfile,
    #[serde(default, skip_serializing_if = "ProviderProfile::is_empty")]
    pub ollama: ProviderProfile,
}

/// The fully-resolved provider settings that can be forwarded to a server process.
pub struct ResolvedProviderSettings {
    /// Normalized provider name.
    pub provider: ProviderKind,
    /// Final model identifier.
    pub model: String,
    /// Optional provider base URL override.
    pub base_url: Option<String>,
    /// Optional provider API key override.
    pub api_key: Option<String>,
}

// ---------------------------------------------------------------------------
// Config file I/O
// ---------------------------------------------------------------------------

pub fn load_config() -> Result<AppConfig> {
    let path = current_user_config_file().context("could not determine user config path")?;
    if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: AppConfig =
            toml::from_str(&data).with_context(|| format!("failed to parse {}", path.display()))?;
        return Ok(config);
    }

    Ok(AppConfig::default())
}

// ---------------------------------------------------------------------------
// Provider resolution: config file > onboarding
// ---------------------------------------------------------------------------

/// Resolves provider settings without constructing a local provider instance.
pub fn resolve_provider_settings() -> Result<ResolvedProviderSettings> {
    let file = load_config().unwrap_or_default();
    let provider_name = if !file.anthropic.is_empty() {
        ProviderKind::Anthropic
    } else if !file.openai.is_empty() {
        ProviderKind::Openai
    } else if !file.ollama.is_empty() {
        ProviderKind::Ollama
    } else {
        anyhow::bail!("No provider configured. Run `clawcr onboard` to complete setup.");
    };

    let selected_profile = match provider_name {
        ProviderKind::Anthropic => &file.anthropic,
        ProviderKind::Openai => &file.openai,
        ProviderKind::Ollama => &file.ollama,
    };
    let Some(model) = selected_profile
        .last_model
        .clone()
        .or_else(|| {
            selected_profile
                .models
                .first()
                .map(|model| model.model.clone())
        })
        .or_else(|| selected_profile.default_model.clone())
    else {
        anyhow::bail!(
            "No model configured for {:?}. Run `clawcr onboard` to complete setup.",
            provider_name
        );
    };

    Ok(ResolvedProviderSettings {
        model,
        provider: provider_name,
        base_url: selected_profile.base_url.clone(),
        api_key: selected_profile.api_key.clone(),
    })
}
