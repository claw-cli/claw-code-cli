use std::{fs, path::Path};

use anyhow::{Context, Result};

use clawcr_core::{
    ConfiguredModel, ModelProviderConfig, ProviderConfigFile, ProviderWireApi, parse_config_str,
};
use clawcr_protocol::ProviderFamily;
use clawcr_provider::{
    ModelProviderSDK, anthropic::AnthropicProvider, openai::OpenAIProvider,
    openai::OpenAIResponsesProvider,
};

/// Resolved provider bootstrap owned by the server runtime.
pub struct ResolvedServerProvider {
    /// Concrete provider used for model requests.
    pub provider: std::sync::Arc<dyn ModelProviderSDK>,
    /// Default model slug used when a session or turn does not request one.
    pub default_model: String,
}

/// Loads the server-side provider from config and environment variables.
pub fn load_server_provider(
    config_file: &Path,
    default_model: Option<&str>,
) -> Result<ResolvedServerProvider> {
    let file_config = read_provider_config(config_file).unwrap_or_default();
    let env_provider = env_non_empty("CLAWCR_PROVIDER");
    let env_wire_api = env_non_empty("CLAWCR_WIRE_API");
    let env_model = env_non_empty("CLAWCR_MODEL");
    let env_base_url = env_non_empty("CLAWCR_BASE_URL");
    let env_api_key = env_non_empty("CLAWCR_API_KEY");

    let requested_model = env_model.as_deref().or(file_config.model.as_deref());
    let provider_id = env_provider
        .clone()
        .and_then(|provider| {
            file_config
                .model_providers
                .contains_key(&provider)
                .then_some(provider)
        })
        .or_else(|| provider_id_for_model(&file_config, requested_model))
        .or_else(|| {
            file_config
                .model_provider
                .clone()
                .filter(|provider| file_config.model_providers.contains_key(provider))
        })
        .or_else(|| file_config.model_providers.keys().next().cloned());
    let provider_config = provider_id
        .as_deref()
        .and_then(|provider_id| file_config.model_providers.get(provider_id));
    let selected_model =
        provider_config.and_then(|provider| select_configured_model(provider, requested_model));
    let wire_api = env_wire_api
        .as_deref()
        .and_then(parse_wire_api)
        .or_else(|| provider_config.and_then(|provider| provider.wire_api))
        .unwrap_or(ProviderWireApi::OpenAIChatCompletions);
    let provider_name = wire_api.provider_family();

    let model = env_model
        .or_else(|| selected_model.map(|model| model.model.clone()))
        .or(file_config.model.clone())
        .or_else(|| default_model.map(ToOwned::to_owned))
        .or_else(|| provider_config.and_then(|provider| provider.default_model.clone()))
        .or_else(|| {
            provider_config
                .and_then(|provider| provider.models.first().map(|model| model.model.clone()))
        })
        .unwrap_or_else(|| default_model_for_provider(provider_name));

    let base_url = env_base_url
        .or_else(|| selected_model.and_then(|model| model.base_url.clone()))
        .or_else(|| provider_config.and_then(|provider| provider.base_url.clone()))
        .or_else(|| env_non_empty("ANTHROPIC_BASE_URL"))
        .or_else(|| env_non_empty("OPENAI_BASE_URL"));
    let api_key = env_api_key
        .or_else(|| selected_model.and_then(|model| model.api_key.clone()))
        .or_else(|| provider_config.and_then(|provider| provider.api_key.clone()))
        .or_else(|| env_non_empty("ANTHROPIC_API_KEY"))
        .or_else(|| env_non_empty("ANTHROPIC_AUTH_TOKEN"))
        .or_else(|| env_non_empty("OPENAI_API_KEY"));

    let provider: std::sync::Arc<dyn ModelProviderSDK> = match wire_api {
        ProviderWireApi::AnthropicMessages => {
            let api_key = api_key.context("anthropic provider requires an API key")?;
            let base_url = base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string());
            std::sync::Arc::new(AnthropicProvider::new(base_url).with_api_key(api_key))
        }
        ProviderWireApi::OpenAIChatCompletions => {
            let base_url = normalize_openai_base_url(
                &base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            );
            let mut provider = OpenAIProvider::new(base_url);
            if let Some(api_key) = api_key {
                provider = provider.with_api_key(api_key);
            }
            std::sync::Arc::new(provider)
        }
        ProviderWireApi::OpenAIResponses => {
            let base_url = normalize_openai_base_url(
                &base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            );
            let mut provider = OpenAIResponsesProvider::new(base_url);
            if let Some(api_key) = api_key {
                provider = provider.with_api_key(api_key);
            }
            std::sync::Arc::new(provider)
        }
    };

    Ok(ResolvedServerProvider {
        provider,
        default_model: model,
    })
}

fn read_provider_config(config_file: &Path) -> Result<ProviderConfigFile> {
    if !config_file.exists() {
        return Ok(ProviderConfigFile::default());
    }
    let contents = fs::read_to_string(config_file)
        .with_context(|| format!("failed to read {}", config_file.display()))?;
    parse_config_str(&contents)
        .with_context(|| format!("failed to parse {}", config_file.display()))
}

fn select_configured_model<'a>(
    profile: &'a ModelProviderConfig,
    requested: Option<&str>,
) -> Option<&'a ConfiguredModel> {
    match requested {
        Some(model) => profile.models.iter().find(|entry| entry.model == model),
        None => profile
            .default_model
            .as_deref()
            .and_then(|default| profile.models.iter().find(|entry| entry.model == default))
            .or_else(|| profile.models.first()),
    }
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

fn default_model_for_provider(provider: ProviderFamily) -> String {
    match provider {
        ProviderFamily::Anthropic { .. } => "claude-sonnet-4-20250514".to_string(),
        ProviderFamily::Openai { .. } => "gpt-4o".to_string(),
    }
}

fn parse_wire_api(value: &str) -> Option<ProviderWireApi> {
    match value.trim().to_ascii_lowercase().as_str() {
        "chat_completion"
        | "chat_completions"
        | "openai"
        | "openai_chat_completion"
        | "openai_chat_completions" => Some(ProviderWireApi::OpenAIChatCompletions),
        "responses" | "openai_responses" => Some(ProviderWireApi::OpenAIResponses),
        "anthropic" | "messages" | "anthropic_messages" => Some(ProviderWireApi::AnthropicMessages),
        _ => None,
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn normalize_openai_base_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let Some(scheme_sep) = trimmed.find("://") else {
        return trimmed.to_string();
    };
    let has_explicit_path = trimmed[scheme_sep + 3..].contains('/');
    if has_explicit_path {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::normalize_openai_base_url;

    #[test]
    fn preserves_explicit_openai_compatible_paths() {
        assert_eq!(
            normalize_openai_base_url("https://open.bigmodel.cn/api/paas/v4/"),
            "https://open.bigmodel.cn/api/paas/v4"
        );
    }

    #[test]
    fn appends_v1_for_bare_openai_hosts() {
        assert_eq!(
            normalize_openai_base_url("https://api.openai.com"),
            "https://api.openai.com/v1"
        );
    }
}
