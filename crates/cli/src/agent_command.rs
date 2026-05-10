use anyhow::Context;
use anyhow::Result;
use devo_core::AppConfigLoader;
use devo_core::FileSystemAppConfigLoader;
use devo_core::ModelCatalog;
use devo_core::PresetModelCatalog;
use devo_core::ProviderConfigFile;
use devo_core::ResolvedProviderSettings;
use devo_core::load_config;
use devo_core::project_config_key;
use devo_core::resolve_provider_settings;
use devo_protocol::PermissionPreset;
use devo_protocol::ProviderWireApi;
use devo_tui::InitialTuiSession;
use devo_tui::InteractiveTuiConfig;
use devo_tui::SavedModelEntry;
use devo_tui::run_interactive_tui;
use devo_utils::find_devo_home;

/// Runs the interactive coding-agent entrypoint.
///
/// `force_onboarding` forces the TUI to start in provider onboarding mode even
/// when a provider config already exists. `log_level` is forwarded to the
/// background server process, and `model_override` replaces the resolved model
/// for this session without mutating the stored provider config.
pub(crate) async fn run_agent(force_onboarding: bool, log_level: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let config_home = find_devo_home().context("could not determine devo home directory")?;
    let model_catalog = PresetModelCatalog::load_from_config(&config_home, Some(&cwd))?;
    let stored_config = load_config().unwrap_or_default();
    let app_config = FileSystemAppConfigLoader::new(config_home.clone()).load(Some(&cwd))?;
    let project_key = project_config_key(&cwd);
    let permission_preset = app_config
        .projects
        .get(&project_key)
        .and_then(|config| config.permission_preset)
        .unwrap_or(PermissionPreset::Default);
    let (onboarding_mode, resolved) =
        resolve_initial_provider_settings(force_onboarding, &stored_config, &model_catalog)?;

    // convert to TUI `SavedModelEntry` type.
    // the `SaveModelEntry` seems utilized to display model at TUI.
    // TODO: Investigate  whether we could simplify it, unify model structure.
    let saved_models = saved_model_entries(&stored_config);

    let ResolvedProviderSettings {
        wire_api,
        model,
        base_url: _,
        api_key: _,
        model_thinking_selection,
        ..
    } = resolved;

    run_interactive_tui(InteractiveTuiConfig {
        // initial_session corresponding fields at top of `config.toml`.
        initial_session: InitialTuiSession {
            model,
            provider: wire_api,
            thinking_selection: model_thinking_selection,
            permission_preset,
            // TODO: why do we need cwd here, maybe remove it ?
            cwd,
        },
        server_log_level: log_level.map(ToOwned::to_owned),
        model_catalog,
        saved_models,
        show_model_onboarding: onboarding_mode,
    })
    .await
    .map(|_| ())
}

/// Resolves the initial provider settings and whether onboarding should be shown.
///
/// `force_onboarding` requests onboarding regardless of stored configuration.
/// `stored_config` is the persisted provider config used to decide whether this
/// is a first-run session. `model_catalog` supplies the fallback onboarding
/// model when no usable provider settings should be resolved yet.
fn resolve_initial_provider_settings(
    force_onboarding: bool,
    stored_config: &ProviderConfigFile,
    model_catalog: &PresetModelCatalog,
) -> Result<(bool, ResolvedProviderSettings)> {
    let onboarding_mode = force_onboarding || stored_config.model_providers.is_empty();
    let resolved = if onboarding_mode {
        // falls back to the first visible preset model.
        let fallback_model = model_catalog
            .resolve_for_turn(None)
            .context("builtin model catalog does not contain a visible onboarding model")?;

        ResolvedProviderSettings {
            provider_id: fallback_model.provider.as_str().to_string(),
            wire_api: fallback_model.provider,
            model: fallback_model.slug.clone(),
            base_url: None,
            api_key: None,
            model_auto_compact_token_limit: None,
            model_context_window: None,
            model_thinking_selection: None,
            disable_response_storage: false,
            preferred_auth_method: None,
        }
    } else {
        resolve_provider_settings()
            .with_context(|| "failed to resolve provider settings outside onboarding mode")?
    };
    Ok((onboarding_mode, resolved))
}

/// Converts persisted provider model profiles into TUI model-picker entries.
///
/// `stored_config` supplies provider-level defaults and model-specific
/// overrides. Model-level `base_url` and `api_key` values take precedence over
/// provider-level values so the picker launches each saved model with the same
/// credentials it was configured with.
fn saved_model_entries(stored_config: &ProviderConfigFile) -> Vec<SavedModelEntry> {
    stored_config
        .model_providers
        .values()
        .flat_map(|provider_config| {
            // Older config entries may not have persisted `wire_api`; keep them
            // on the historical OpenAI-compatible chat-completions default.
            let wire_api = provider_config
                .wire_api
                .unwrap_or(ProviderWireApi::OpenAIChatCompletions);
            provider_config
                .models
                .iter()
                .map(move |model| SavedModelEntry {
                    model: model.model.clone(),
                    wire_api,
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
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pretty_assertions::assert_eq;

    use super::resolve_initial_provider_settings;
    use super::saved_model_entries;
    use devo_core::ConfiguredModel;
    use devo_core::Model;
    use devo_core::ModelProviderConfig;
    use devo_core::PresetModelCatalog;
    use devo_core::ProviderConfigFile;
    use devo_core::ResolvedProviderSettings;
    use devo_protocol::ProviderWireApi;
    use devo_tui::SavedModelEntry;

    fn test_catalog() -> PresetModelCatalog {
        PresetModelCatalog::new(vec![Model {
            slug: "test-onboard-model".to_string(),
            provider: ProviderWireApi::OpenAIChatCompletions,
            ..Model::default()
        }])
    }

    #[test]
    fn resolve_initial_provider_settings_uses_catalog_fallback_during_onboarding() {
        let actual = resolve_initial_provider_settings(
            false,
            &ProviderConfigFile::default(),
            &test_catalog(),
        )
        .expect("resolve initial provider settings");

        assert_eq!(
            actual,
            (
                true,
                ResolvedProviderSettings {
                    provider_id: "openai_chat_completions".to_string(),
                    wire_api: ProviderWireApi::OpenAIChatCompletions,
                    model: "test-onboard-model".to_string(),
                    base_url: None,
                    api_key: None,
                    model_auto_compact_token_limit: None,
                    model_context_window: None,
                    model_thinking_selection: None,
                    disable_response_storage: false,
                    preferred_auth_method: None,
                }
            )
        );
    }

    #[test]
    fn resolve_initial_provider_settings_honors_forced_onboarding_with_existing_config() {
        let mut stored_config = ProviderConfigFile::default();
        stored_config.model_providers.insert(
            "openai_chat_completions".to_string(),
            ModelProviderConfig::default(),
        );

        let actual = resolve_initial_provider_settings(true, &stored_config, &test_catalog())
            .expect("resolve initial provider settings");

        assert_eq!(
            actual,
            (
                true,
                ResolvedProviderSettings {
                    provider_id: "openai_chat_completions".to_string(),
                    wire_api: ProviderWireApi::OpenAIChatCompletions,
                    model: "test-onboard-model".to_string(),
                    base_url: None,
                    api_key: None,
                    model_auto_compact_token_limit: None,
                    model_context_window: None,
                    model_thinking_selection: None,
                    disable_response_storage: false,
                    preferred_auth_method: None,
                }
            )
        );
    }

    #[test]
    fn saved_model_entries_inherit_provider_defaults_and_preserve_model_overrides() {
        let stored_config = ProviderConfigFile {
            model_providers: BTreeMap::from([(
                "openai".to_string(),
                ModelProviderConfig {
                    base_url: Some("https://provider.example".to_string()),
                    api_key: Some("provider-key".to_string()),
                    wire_api: Some(ProviderWireApi::OpenAIResponses),
                    models: vec![
                        ConfiguredModel {
                            model: "provider-defaults".to_string(),
                            ..ConfiguredModel::default()
                        },
                        ConfiguredModel {
                            model: "model-overrides".to_string(),
                            base_url: Some("https://model.example".to_string()),
                            api_key: Some("model-key".to_string()),
                        },
                    ],
                    ..ModelProviderConfig::default()
                },
            )]),
            ..ProviderConfigFile::default()
        };

        assert_eq!(
            saved_model_entries(&stored_config),
            vec![
                SavedModelEntry {
                    model: "provider-defaults".to_string(),
                    wire_api: ProviderWireApi::OpenAIResponses,
                    base_url: Some("https://provider.example".to_string()),
                    api_key: Some("provider-key".to_string()),
                },
                SavedModelEntry {
                    model: "model-overrides".to_string(),
                    wire_api: ProviderWireApi::OpenAIResponses,
                    base_url: Some("https://model.example".to_string()),
                    api_key: Some("model-key".to_string()),
                },
            ]
        );
    }

    #[test]
    fn saved_model_entries_defaults_wire_api_to_openai_chat_completions() {
        let stored_config = ProviderConfigFile {
            model_providers: BTreeMap::from([(
                "openai".to_string(),
                ModelProviderConfig {
                    models: vec![ConfiguredModel {
                        model: "default-wire-api".to_string(),
                        ..ConfiguredModel::default()
                    }],
                    ..ModelProviderConfig::default()
                },
            )]),
            ..ProviderConfigFile::default()
        };

        assert_eq!(
            saved_model_entries(&stored_config),
            vec![SavedModelEntry {
                model: "default-wire-api".to_string(),
                wire_api: ProviderWireApi::OpenAIChatCompletions,
                base_url: None,
                api_key: None,
            }]
        );
    }
}
