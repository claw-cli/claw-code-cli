use std::collections::BTreeMap;

use anyhow::{Context, Result};
use clawcr_protocol::{ProviderFamily, ReasoningEffort};
use serde::{Deserialize, Serialize};
use toml::Value;

use clawcr_utils::current_user_config_file;

/// One supported provider wire protocol exposed by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProviderWireApi {
    /// OpenAI-compatible `/v1/chat/completions`.
    #[serde(rename = "openai_chat_completions")]
    OpenAIChatCompletions,
    /// OpenAI-compatible `/v1/responses`.
    #[serde(rename = "openai_responses")]
    OpenAIResponses,
    /// Anthropic-compatible `/v1/messages`.
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
}

impl ProviderWireApi {
    /// Returns the provider family implied by this wire protocol.
    pub fn provider_family(self) -> ProviderFamily {
        match self {
            Self::OpenAIChatCompletions | Self::OpenAIResponses => ProviderFamily::openai(),
            Self::AnthropicMessages => ProviderFamily::anthropic(),
        }
    }

    pub fn default_for_provider(provider: &ProviderFamily) -> Self {
        match provider {
            ProviderFamily::Anthropic { .. } => Self::AnthropicMessages,
            ProviderFamily::Openai { .. } => Self::OpenAIChatCompletions,
        }
    }
}

pub fn provider_id_from_base_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .split_once("://")
        .map_or(trimmed, |(_, remainder)| remainder);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme)
        .trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

pub fn provider_id_for_endpoint(provider: &ProviderFamily, base_url: Option<&str>) -> String {
    base_url
        .and_then(provider_id_from_base_url)
        .unwrap_or_else(|| provider.as_str().to_string())
}

pub fn provider_name_for_endpoint(provider: &ProviderFamily, base_url: Option<&str>) -> String {
    provider_id_for_endpoint(provider, base_url)
}

impl<'de> Deserialize<'de> for ProviderWireApi {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.trim().to_ascii_lowercase().as_str() {
            "chat_completion"
            | "chat_completions"
            | "openai"
            | "openai_chat_completion"
            | "openai_chat_completions" => Ok(Self::OpenAIChatCompletions),
            "responses" | "openai_responses" => Ok(Self::OpenAIResponses),
            "anthropic" | "messages" | "anthropic_messages" => Ok(Self::AnthropicMessages),
            other => Err(serde::de::Error::custom(format!(
                "unsupported wire_api `{other}`"
            ))),
        }
    }
}

/// The preferred authentication method for the active provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PreferredAuthMethod {
    /// Use an API key or bearer token.
    Apikey,
}

impl<'de> Deserialize<'de> for PreferredAuthMethod {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.trim().to_ascii_lowercase().as_str() {
            "apikey" | "api_key" => Ok(Self::Apikey),
            other => Err(serde::de::Error::custom(format!(
                "unsupported preferred_auth_method `{other}`"
            ))),
        }
    }
}

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
pub struct ModelProviderConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_api: Option<ProviderWireApi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ConfiguredModel>,
}

impl ModelProviderConfig {
    /// Returns whether the profile has no configured values.
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.base_url.is_none()
            && self.api_key.is_none()
            && self.wire_api.is_none()
            && self.last_model.is_none()
            && self.default_model.is_none()
            && self.models.is_empty()
    }
}

/// Persisted provider and active model configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfigFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_auto_compact_token_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_response_storage: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_auth_method: Option<PreferredAuthMethod>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_providers: BTreeMap<String, ModelProviderConfig>,
}

/// The fully-resolved provider settings that can be forwarded to a server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProviderSettings {
    /// Selected provider identifier from `[model_providers.<id>]`.
    pub provider_id: String,
    /// Normalized provider family for runtime dispatch.
    pub provider: ProviderFamily,
    /// Selected provider transport implementation.
    pub wire_api: ProviderWireApi,
    /// Final model identifier.
    pub model: String,
    /// Optional provider base URL override.
    pub base_url: Option<String>,
    /// Optional provider API key override.
    pub api_key: Option<String>,
    /// Optional active model auto-compaction threshold in tokens.
    pub model_auto_compact_token_limit: Option<u32>,
    /// Optional active model context window override in tokens.
    pub model_context_window: Option<u32>,
    /// Optional active reasoning effort override.
    pub model_reasoning_effort: Option<ReasoningEffort>,
    /// Whether provider-side response storage should be disabled.
    pub disable_response_storage: bool,
    /// Preferred authentication method for the active provider.
    pub preferred_auth_method: Option<PreferredAuthMethod>,
}

/// Loads the user's provider config file from the standard config path.
pub fn load_config() -> Result<ProviderConfigFile> {
    let path = current_user_config_file().context("could not determine user config path")?;
    if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        return parse_config_str(&data)
            .with_context(|| format!("failed to parse {}", path.display()));
    }

    Ok(ProviderConfigFile::default())
}

/// Parses provider config TOML from a string and normalizes legacy layouts.
pub fn parse_config_str(data: &str) -> Result<ProviderConfigFile> {
    let value: Value = toml::from_str(data)?;
    parse_config_value(value)
}

/// Parses provider config TOML and normalizes legacy layouts.
pub fn parse_config_value(value: Value) -> Result<ProviderConfigFile> {
    let normalized = normalize_config_value(value)?;
    Ok(normalized.try_into()?)
}

/// Resolves provider settings without constructing a local provider instance.
pub fn resolve_provider_settings() -> Result<ResolvedProviderSettings> {
    resolve_provider_settings_from_config(&load_config().unwrap_or_default())
}

pub(crate) fn resolve_provider_settings_from_config(
    file: &ProviderConfigFile,
) -> Result<ResolvedProviderSettings> {
    let provider_id = file
        .model_provider
        .as_deref()
        .filter(|provider_id| file.model_providers.contains_key(*provider_id))
        .map(ToOwned::to_owned)
        .or_else(|| provider_id_for_model(file, file.model.as_deref()))
        .or_else(|| first_configured_provider_id(file))
        .context("No provider configured. Run `clawcr onboard` to complete setup.")?;
    let provider_config = file
        .model_providers
        .get(&provider_id)
        .with_context(|| format!("configured provider `{provider_id}` was not found"))?;
    let model = file
        .model
        .clone()
        .or_else(|| provider_config.last_model.clone())
        .or_else(|| provider_config.default_model.clone())
        .or_else(|| {
            provider_config
                .models
                .first()
                .map(|entry| entry.model.clone())
        })
        .or_else(|| first_configured_model(file))
        .context("No model configured. Run `clawcr onboard` to complete setup.")?;
    let matched_model = provider_config
        .models
        .iter()
        .find(|entry| entry.model == model);
    let wire_api = provider_config
        .wire_api
        .unwrap_or(ProviderWireApi::OpenAIChatCompletions);

    Ok(ResolvedProviderSettings {
        provider_id,
        provider: wire_api.provider_family(),
        wire_api,
        model,
        base_url: matched_model
            .and_then(|entry| entry.base_url.clone())
            .or_else(|| provider_config.base_url.clone()),
        api_key: matched_model
            .and_then(|entry| entry.api_key.clone())
            .or_else(|| provider_config.api_key.clone()),
        model_auto_compact_token_limit: file.model_auto_compact_token_limit,
        model_context_window: file.model_context_window,
        model_reasoning_effort: file.model_reasoning_effort,
        disable_response_storage: file.disable_response_storage.unwrap_or(false),
        preferred_auth_method: file.preferred_auth_method,
    })
}

fn normalize_config_value(value: Value) -> Result<Value> {
    let Some(table) = value.as_table() else {
        return Ok(value);
    };
    if table.contains_key("model_providers")
        || table.contains_key("model_provider")
        || table.contains_key("model_auto_compact_token_limit")
        || table.contains_key("model_context_window")
        || table.contains_key("model_reasoning_effort")
        || table.contains_key("disable_response_storage")
        || table.contains_key("preferred_auth_method")
    {
        return Ok(value);
    }

    let legacy: LegacyConfigFile = value.clone().try_into()?;
    Ok(Value::try_from(legacy.into_provider_config_file())?)
}

fn first_configured_provider_id(config: &ProviderConfigFile) -> Option<String> {
    config.model_providers.keys().next().cloned()
}

fn first_configured_model(config: &ProviderConfigFile) -> Option<String> {
    config.model_providers.values().find_map(|provider| {
        provider
            .last_model
            .clone()
            .or_else(|| provider.default_model.clone())
            .or_else(|| provider.models.first().map(|entry| entry.model.clone()))
    })
}

fn provider_id_for_model(
    config: &ProviderConfigFile,
    requested_model: Option<&str>,
) -> Option<String> {
    let requested_model = requested_model?;
    config
        .model_providers
        .iter()
        .find(|(_, provider)| {
            provider.last_model.as_deref() == Some(requested_model)
                || provider.default_model.as_deref() == Some(requested_model)
                || provider
                    .models
                    .iter()
                    .any(|entry| entry.model == requested_model)
        })
        .map(|(provider_id, _)| provider_id.clone())
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LegacyConfigFile {
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    anthropic: LegacyProviderProfile,
    #[serde(default)]
    openai: LegacyProviderProfile,
    #[serde(default)]
    ollama: LegacyProviderProfile,
}

impl LegacyConfigFile {
    fn into_provider_config_file(self) -> ProviderConfigFile {
        let mut model_providers = BTreeMap::new();
        insert_legacy_provider(
            &mut model_providers,
            ProviderFamily::anthropic(),
            self.anthropic,
        );
        insert_legacy_provider(&mut model_providers, ProviderFamily::openai(), self.openai);
        insert_legacy_provider(&mut model_providers, ProviderFamily::openai(), self.ollama);

        let fallback_provider_id = self
            .default_provider
            .as_deref()
            .and_then(parse_legacy_provider_family)
            .map(|provider| provider_id_for_legacy_profile(&provider, &model_providers))
            .or(self.provider.clone())
            .or_else(|| model_providers.keys().next().cloned());
        let fallback_model = self
            .model
            .clone()
            .or_else(|| {
                fallback_provider_id
                    .as_deref()
                    .and_then(|provider_id| model_providers.get(provider_id))
                    .and_then(legacy_provider_selected_model)
            })
            .or_else(|| {
                model_providers
                    .values()
                    .find_map(legacy_provider_selected_model)
            });

        if model_providers.is_empty()
            && (fallback_provider_id.is_some()
                || fallback_model.is_some()
                || self.base_url.is_some()
                || self.api_key.is_some())
        {
            let provider_id = fallback_provider_id
                .clone()
                .unwrap_or_else(|| provider_id_for_endpoint(&ProviderFamily::openai(), None));
            let model = fallback_model
                .clone()
                .unwrap_or_else(|| "gpt-4o".to_string());
            model_providers.insert(
                provider_id.clone(),
                ModelProviderConfig {
                    name: Some(provider_name_for_endpoint(
                        &ProviderFamily::openai(),
                        self.base_url.as_deref(),
                    )),
                    base_url: self.base_url.clone(),
                    api_key: self.api_key.clone(),
                    wire_api: Some(ProviderWireApi::default_for_provider(
                        &ProviderFamily::openai(),
                    )),
                    last_model: Some(model.clone()),
                    default_model: Some(model.clone()),
                    models: vec![ConfiguredModel {
                        model,
                        base_url: self.base_url,
                        api_key: self.api_key,
                    }],
                },
            );
        }

        ProviderConfigFile {
            model_provider: fallback_provider_id,
            model: fallback_model,
            model_auto_compact_token_limit: None,
            model_context_window: None,
            model_reasoning_effort: None,
            disable_response_storage: None,
            preferred_auth_method: None,
            model_providers,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct LegacyProviderProfile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    last_model: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    models: Vec<ConfiguredModel>,
}

fn insert_legacy_provider(
    providers: &mut BTreeMap<String, ModelProviderConfig>,
    provider: ProviderFamily,
    legacy: LegacyProviderProfile,
) {
    if legacy.name.is_none()
        && legacy.model.is_none()
        && legacy.last_model.is_none()
        && legacy.default_model.is_none()
        && legacy.base_url.is_none()
        && legacy.api_key.is_none()
        && legacy.models.is_empty()
    {
        return;
    }

    let provider_id = provider_id_for_endpoint(&provider, legacy.base_url.as_deref());
    let selected_model = legacy
        .last_model
        .clone()
        .or_else(|| legacy.default_model.clone())
        .or_else(|| legacy.model.clone())
        .or_else(|| legacy.models.first().map(|entry| entry.model.clone()));
    let mut models = legacy.models;
    if let Some(model) = selected_model.clone()
        && !models.iter().any(|entry| entry.model == model)
    {
        models.insert(
            0,
            ConfiguredModel {
                model,
                base_url: legacy.base_url.clone(),
                api_key: legacy.api_key.clone(),
            },
        );
    }

    providers.insert(
        provider_id.clone(),
        ModelProviderConfig {
            name: legacy.name.or_else(|| {
                Some(provider_name_for_endpoint(
                    &provider,
                    legacy.base_url.as_deref(),
                ))
            }),
            base_url: legacy.base_url,
            api_key: legacy.api_key,
            wire_api: Some(ProviderWireApi::default_for_provider(&provider)),
            last_model: legacy.last_model.or_else(|| legacy.model.clone()),
            default_model: legacy.default_model.or(legacy.model),
            models,
        },
    );
}

fn provider_id_for_legacy_profile(
    provider: &ProviderFamily,
    model_providers: &BTreeMap<String, ModelProviderConfig>,
) -> String {
    model_providers
        .iter()
        .find(|(_, config)| {
            config.wire_api.map(|wire_api| wire_api.provider_family()) == Some(*provider)
        })
        .map(|(provider_id, _)| provider_id.clone())
        .unwrap_or_else(|| provider_id_for_endpoint(provider, None))
}

fn legacy_provider_selected_model(provider: &ModelProviderConfig) -> Option<String> {
    provider
        .last_model
        .clone()
        .or_else(|| provider.default_model.clone())
        .or_else(|| provider.models.first().map(|entry| entry.model.clone()))
}

fn parse_legacy_provider_family(value: &str) -> Option<ProviderFamily> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" => Some(ProviderFamily::openai()),
        "anthropic" => Some(ProviderFamily::anthropic()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{
        ModelProviderConfig, PreferredAuthMethod, ProviderConfigFile, ProviderWireApi,
        ResolvedProviderSettings, parse_config_str, resolve_provider_settings_from_config,
    };
    use clawcr_protocol::{ProviderFamily, ReasoningEffort};

    #[test]
    fn resolves_new_style_provider_and_model_settings() {
        let config = parse_config_str(
            r#"
model_provider = "tvideo"
model = "gpt-5.4"
model_auto_compact_token_limit = 970000
model_context_window = 997500
model_reasoning_effort = "medium"
disable_response_storage = true
preferred_auth_method = "apikey"

[model_providers.tvideo]
name = "tvideo"
base_url = "https://qy.testvideo.site/v1"
wire_api = "responses"
"#,
        )
        .expect("parse config");

        let resolved =
            resolve_provider_settings_from_config(&config).expect("resolve provider settings");

        assert_eq!(
            resolved,
            ResolvedProviderSettings {
                provider_id: "tvideo".to_string(),
                provider: ProviderFamily::openai(),
                wire_api: ProviderWireApi::OpenAIResponses,
                model: "gpt-5.4".to_string(),
                base_url: Some("https://qy.testvideo.site/v1".to_string()),
                api_key: None,
                model_auto_compact_token_limit: Some(970000),
                model_context_window: Some(997500),
                model_reasoning_effort: Some(ReasoningEffort::Medium),
                disable_response_storage: true,
                preferred_auth_method: Some(PreferredAuthMethod::Apikey),
            }
        );
    }

    #[test]
    fn parses_legacy_section_config_into_new_shape() {
        let config = parse_config_str(
            r#"
default_provider = "openai"

[openai]
model = "qwen3-coder-next"
base_url = "https://api.example.com/v1"
api_key = "profile-key"
"#,
        )
        .expect("parse config");

        assert_eq!(config.model_provider, Some("api.example.com".to_string()));
        assert_eq!(config.model, Some("qwen3-coder-next".to_string()));
        assert_eq!(
            config.model_providers.get("api.example.com"),
            Some(&ModelProviderConfig {
                name: Some("api.example.com".to_string()),
                base_url: Some("https://api.example.com/v1".to_string()),
                api_key: Some("profile-key".to_string()),
                wire_api: Some(ProviderWireApi::OpenAIChatCompletions),
                last_model: Some("qwen3-coder-next".to_string()),
                default_model: Some("qwen3-coder-next".to_string()),
                models: vec![super::ConfiguredModel {
                    model: "qwen3-coder-next".to_string(),
                    base_url: Some("https://api.example.com/v1".to_string()),
                    api_key: Some("profile-key".to_string()),
                }],
            })
        );
    }

    #[test]
    fn resolves_provider_from_model_when_provider_id_is_stale() {
        let config = ProviderConfigFile {
            model_provider: Some("missing".to_string()),
            model: Some("qwen3-coder-next".to_string()),
            model_auto_compact_token_limit: None,
            model_context_window: None,
            model_reasoning_effort: None,
            disable_response_storage: None,
            preferred_auth_method: None,
            model_providers: [(
                "api.example.com".to_string(),
                ModelProviderConfig {
                    name: Some("api.example.com".to_string()),
                    base_url: Some("https://api.example.com".to_string()),
                    api_key: Some("profile-key".to_string()),
                    wire_api: Some(ProviderWireApi::OpenAIChatCompletions),
                    last_model: Some("qwen3-coder-next".to_string()),
                    default_model: None,
                    models: Vec::new(),
                },
            )]
            .into_iter()
            .collect(),
        };

        let resolved =
            resolve_provider_settings_from_config(&config).expect("resolve provider settings");

        assert_eq!(resolved.provider_id, "api.example.com");
        assert_eq!(resolved.provider, ProviderFamily::openai());
        assert_eq!(resolved.model, "qwen3-coder-next");
        assert_eq!(
            resolved.base_url,
            Some("https://api.example.com".to_string())
        );
        assert_eq!(resolved.api_key, Some("profile-key".to_string()));
    }

    #[test]
    fn provider_id_from_base_url_extracts_hostname() {
        assert_eq!(
            super::provider_id_from_base_url("https://open.bigmodel.cn/api/paas/v4"),
            Some("open.bigmodel.cn".to_string())
        );
        assert_eq!(
            super::provider_id_from_base_url("https://api.deepseek.com/v1"),
            Some("api.deepseek.com".to_string())
        );
    }
}
