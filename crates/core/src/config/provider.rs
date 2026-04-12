use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use clawcr_utils::current_user_config_file;

use crate::ProviderKind;

/// One model entry stored under a provider section in `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfiguredModel {
    /// The model slug or custom model name.
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

/// One provider-specific configuration block that can store many model entries.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Returns whether the profile has no configured values.
    pub fn is_empty(&self) -> bool {
        self.last_model.is_none()
            && self.default_model.is_none()
            && self.base_url.is_none()
            && self.api_key.is_none()
            && self.models.is_empty()
    }
}

/// Persisted provider configuration grouped by provider family.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfigFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<ProviderKind>,
    #[serde(default, skip_serializing_if = "ProviderProfile::is_empty")]
    pub anthropic: ProviderProfile,
    #[serde(default, skip_serializing_if = "ProviderProfile::is_empty")]
    pub openai: ProviderProfile,
    #[serde(default, skip_serializing_if = "ProviderProfile::is_empty")]
    pub ollama: ProviderProfile,
}

/// The fully-resolved provider settings that can be forwarded to a server process.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Loads the user's provider config file from the standard config path.
pub fn load_config() -> Result<ProviderConfigFile> {
    let path = current_user_config_file().context("could not determine user config path")?;
    if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: ProviderConfigFile =
            toml::from_str(&data).with_context(|| format!("failed to parse {}", path.display()))?;
        return Ok(config);
    }

    Ok(ProviderConfigFile::default())
}

/// Resolves provider settings without constructing a local provider instance.
pub fn resolve_provider_settings() -> Result<ResolvedProviderSettings> {
    resolve_provider_settings_from_config(&load_config().unwrap_or_default())
}

fn resolve_provider_settings_from_config(
    file: &ProviderConfigFile,
) -> Result<ResolvedProviderSettings> {
    let requested_model = file
        .default_provider
        .and_then(|provider| profile_for_provider(file, provider).last_model.clone())
        .or_else(|| {
            file.default_provider.and_then(|provider| {
                profile_for_provider(file, provider)
                    .models
                    .first()
                    .map(|model| model.model.clone())
            })
        })
        .or_else(|| {
            file.default_provider
                .and_then(|provider| profile_for_provider(file, provider).default_model.clone())
        })
        .or_else(|| first_configured_model(file));

    let Some(model) = requested_model else {
        anyhow::bail!("No model configured. Run `clawcr onboard` to complete setup.");
    };

    let provider = provider_for_model(file, &model)
        .or(file.default_provider)
        .or_else(|| first_configured_provider(file))
        .context("No provider configured. Run `clawcr onboard` to complete setup.")?;
    let profile = profile_for_provider(file, provider);
    let matched_model = profile.models.iter().find(|entry| entry.model == model);

    Ok(ResolvedProviderSettings {
        model,
        provider,
        base_url: matched_model
            .and_then(|entry| entry.base_url.clone())
            .or_else(|| profile.base_url.clone()),
        api_key: matched_model
            .and_then(|entry| entry.api_key.clone())
            .or_else(|| profile.api_key.clone()),
    })
}

fn profile_for_provider(config: &ProviderConfigFile, provider: ProviderKind) -> &ProviderProfile {
    match provider {
        ProviderKind::Anthropic => &config.anthropic,
        ProviderKind::Openai => &config.openai,
        ProviderKind::Ollama => &config.ollama,
    }
}

fn first_configured_model(config: &ProviderConfigFile) -> Option<String> {
    for profile in [&config.anthropic, &config.openai, &config.ollama] {
        if let Some(model) = profile.last_model.clone() {
            return Some(model);
        }
        if let Some(model) = profile.models.first().map(|entry| entry.model.clone()) {
            return Some(model);
        }
        if let Some(model) = profile.default_model.clone() {
            return Some(model);
        }
    }
    None
}

fn first_configured_provider(config: &ProviderConfigFile) -> Option<ProviderKind> {
    if !config.anthropic.is_empty() {
        Some(ProviderKind::Anthropic)
    } else if !config.openai.is_empty() {
        Some(ProviderKind::Openai)
    } else if !config.ollama.is_empty() {
        Some(ProviderKind::Ollama)
    } else {
        None
    }
}

fn provider_for_model(config: &ProviderConfigFile, requested_model: &str) -> Option<ProviderKind> {
    for (provider, profile) in [
        (ProviderKind::Anthropic, &config.anthropic),
        (ProviderKind::Openai, &config.openai),
        (ProviderKind::Ollama, &config.ollama),
    ] {
        if profile.last_model.as_deref() == Some(requested_model)
            || profile.default_model.as_deref() == Some(requested_model)
            || profile
                .models
                .iter()
                .any(|entry| entry.model == requested_model)
        {
            return Some(provider);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{
        ProviderConfigFile, ProviderKind, ProviderProfile, resolve_provider_settings_from_config,
    };

    #[test]
    fn resolves_provider_from_model_profile_when_default_provider_is_stale() {
        let config = ProviderConfigFile {
            default_provider: Some(ProviderKind::Openai),
            anthropic: ProviderProfile::default(),
            openai: ProviderProfile {
                last_model: Some("qwen3-coder-next".to_string()),
                default_model: None,
                base_url: Some("https://api.example.com".to_string()),
                api_key: Some("profile-key".to_string()),
                models: Vec::new(),
            },
            ollama: ProviderProfile::default(),
        };

        let resolved =
            resolve_provider_settings_from_config(&config).expect("resolve provider settings");

        assert_eq!(resolved.provider, ProviderKind::Openai);
        assert_eq!(resolved.model, "qwen3-coder-next");
        assert_eq!(
            resolved.base_url,
            Some("https://api.example.com".to_string())
        );
        assert_eq!(resolved.api_key, Some("profile-key".to_string()));
    }
}
