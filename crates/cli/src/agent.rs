use anyhow::{Context, Result};
use clawcr_core::{
    ModelCatalog, PresetModelCatalog, ProviderConfigFile, ProviderWireApi,
    ResolvedProviderSettings, load_config, resolve_provider_settings,
};
use clawcr_protocol::ProviderFamily;
use clawcr_tui::{InteractiveTuiConfig, SavedModelEntry, TerminalMode, run_interactive_tui};

/// Runs the interactive coding-agent entrypoint.
pub async fn run_agent(
    force_onboarding: bool,
    no_alt_screen: bool,
    log_level: Option<&str>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let model_catalog = PresetModelCatalog::load()?;
    let stored_config = load_config().unwrap_or_default();
    let (onboarding_mode, resolved) =
        resolve_initial_provider_settings(force_onboarding, &stored_config, &model_catalog)?;
    let saved_models = stored_config
        .model_providers
        .values()
        .flat_map(|provider_config| {
            let provider = provider_config
                .wire_api
                .unwrap_or(ProviderWireApi::OpenAIChatCompletions)
                .provider_family();
            provider_config
                .models
                .iter()
                .map(move |model| SavedModelEntry {
                    model: model.model.clone(),
                    provider,
                    base_url: model
                        .base_url
                        .clone()
                        .or_else(|| provider_config.base_url.clone()),
                    api_key: model
                        .api_key
                        .clone()
                        .or_else(|| provider_config.api_key.clone()),
                })
        })
        .collect();

    let server_env = server_env_overrides(&resolved);
    let clawcr_core::ResolvedProviderSettings {
        provider,
        model,
        base_url: _,
        api_key: _,
        ..
    } = resolved;

    run_interactive_tui(InteractiveTuiConfig {
        model,
        provider,
        cwd,
        server_env,
        server_log_level: log_level.map(ToOwned::to_owned),
        model_catalog,
        saved_models,
        show_model_onboarding: onboarding_mode,
        terminal_mode: if no_alt_screen {
            TerminalMode::Never
        } else {
            TerminalMode::Auto
        },
    })
    .await
    .map(|_| ())
}

fn resolve_initial_provider_settings(
    force_onboarding: bool,
    stored_config: &ProviderConfigFile,
    model_catalog: &PresetModelCatalog,
) -> Result<(bool, ResolvedProviderSettings)> {
    let onboarding_mode = force_onboarding || stored_config.model_providers.is_empty();
    let resolved = if onboarding_mode {
        let fallback_model = model_catalog
            .resolve_for_turn(None)
            .context("builtin model catalog does not contain a visible onboarding model")?;
        ResolvedProviderSettings {
            provider_id: fallback_model.provider.as_str().to_string(),
            provider: fallback_model.provider,
            wire_api: match &fallback_model.provider {
                ProviderFamily::Anthropic { .. } => ProviderWireApi::AnthropicMessages,
                ProviderFamily::Openai { .. } => ProviderWireApi::OpenAIChatCompletions,
            },
            model: fallback_model.slug.clone(),
            base_url: None,
            api_key: None,
            model_auto_compact_token_limit: None,
            model_context_window: None,
            model_reasoning_effort: None,
            disable_response_storage: false,
            preferred_auth_method: None,
        }
    } else {
        resolve_provider_settings()
            .with_context(|| "failed to resolve provider settings outside onboarding mode")?
    };
    Ok((onboarding_mode, resolved))
}

fn server_env_overrides(resolved: &clawcr_core::ResolvedProviderSettings) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "CLAWCR_PROVIDER".to_string(),
            resolved.provider.as_str().to_string(),
        ),
        (
            "CLAWCR_WIRE_API".to_string(),
            match resolved.wire_api {
                ProviderWireApi::OpenAIChatCompletions => "openai_chat_completions".to_string(),
                ProviderWireApi::OpenAIResponses => "openai_responses".to_string(),
                ProviderWireApi::AnthropicMessages => "anthropic_messages".to_string(),
            },
        ),
        ("CLAWCR_MODEL".to_string(), resolved.model.clone()),
    ];
    if let Some(base_url) = &resolved.base_url {
        env.push(("CLAWCR_BASE_URL".to_string(), base_url.clone()));
    }
    if let Some(api_key) = &resolved.api_key {
        env.push(("CLAWCR_API_KEY".to_string(), api_key.clone()));
    }
    env
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::resolve_initial_provider_settings;
    use clawcr_core::{Model, PresetModelCatalog, ProviderConfigFile};
    use clawcr_protocol::ProviderFamily;

    fn test_catalog() -> PresetModelCatalog {
        PresetModelCatalog::new(vec![Model {
            slug: "test-onboard-model".to_string(),
            provider: ProviderFamily::openai(),
            ..Model::default()
        }])
    }

    #[test]
    fn resolve_initial_provider_settings_uses_catalog_fallback_during_onboarding() {
        let (onboarding_mode, resolved) = resolve_initial_provider_settings(
            false,
            &ProviderConfigFile::default(),
            &test_catalog(),
        )
        .expect("resolve initial provider settings");

        assert!(onboarding_mode);
        assert_eq!(resolved.provider_id, "openai");
        assert_eq!(resolved.provider, ProviderFamily::openai());
        assert_eq!(resolved.model, "test-onboard-model");
        assert_eq!(resolved.base_url, None);
        assert_eq!(resolved.api_key, None);
    }
}
