use anyhow::Context;
use anyhow::Result;
use devo_core::provider_id_for_endpoint;
use devo_core::provider_name_for_endpoint;
use devo_protocol::ProviderWireApi;
use devo_utils::find_devo_home;
use toml::Value;

/// Persists the onboarding choice into the user's `config.toml`.
pub(crate) fn save_onboarding_config(
    provider: ProviderWireApi,
    model: &str,
    base_url: Option<&str>,
    api_key: Option<&str>,
) -> Result<()> {
    let path = find_devo_home()
        .context("could not determine user config path")?
        .join("config.toml");

    let mut root = if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        data.parse::<Value>()
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        Value::Table(Default::default())
    };

    root = merge_onboarding_config(root, provider, model, base_url, api_key)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let rendered = toml::to_string_pretty(&root)?;

    std::fs::write(&path, rendered)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub(crate) fn save_last_used_model(
    wire_api: Option<ProviderWireApi>,
    provider: ProviderWireApi,
    model: &str,
) -> Result<()> {
    let path = find_devo_home()
        .context("could not determine user config path")?
        .join("config.toml");
    let mut root = if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        data.parse::<Value>()
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        Value::Table(Default::default())
    };
    root = merge_last_used_model(root, wire_api, provider, model)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(&root)?;

    std::fs::write(&path, rendered)
        .with_context(|| format!("failed to write {}", path.display()))?;

    Ok(())
}

#[allow(dead_code)]
pub(crate) fn save_thinking_selection(selection: Option<&str>) -> Result<()> {
    let path = find_devo_home()
        .context("could not determine user config path")?
        .join("config.toml");
    let mut root = if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        data.parse::<Value>()
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        Value::Table(Default::default())
    };
    root = merge_thinking_selection(root, selection)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(&root)?;

    std::fs::write(&path, rendered)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub(crate) fn save_theme_selection(name: &str) -> Result<()> {
    let path = find_devo_home()
        .context("could not determine user config path")?
        .join("config.toml");
    let mut root = if path.exists() {
        let data = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        data.parse::<Value>()
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        Value::Table(Default::default())
    };
    root = merge_theme_selection(root, name)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(&root)?;

    std::fs::write(&path, rendered)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub(crate) fn load_theme_selection() -> Option<String> {
    let path = find_devo_home().ok()?.join("config.toml");
    let data = std::fs::read_to_string(&path).ok()?;
    let root: Value = data.parse().ok()?;
    root.get("theme")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn merge_theme_selection(mut root: Value, name: &str) -> Result<Value> {
    let table = root
        .as_table_mut()
        .context("config root must be a TOML table")?;
    table.insert("theme".to_string(), Value::String(name.to_string()));
    Ok(root)
}

#[allow(dead_code)]
fn merge_thinking_selection(mut root: Value, selection: Option<&str>) -> Result<Value> {
    let table = root
        .as_table_mut()
        .context("config root must be a TOML table")?;
    match normalized_optional(selection) {
        Some(value) => {
            table.insert(
                "model_thinking_selection".to_string(),
                Value::String(value.to_string()),
            );
        }
        None => {
            table.remove("model_thinking_selection");
        }
    }
    Ok(root)
}

fn merge_onboarding_config(
    mut root: Value,
    provider: ProviderWireApi,
    model: &str,
    base_url: Option<&str>,
    api_key: Option<&str>,
) -> Result<Value> {
    // Preserve unrelated config keys while updating only the onboarding-selected
    // provider profile.
    let table = root
        .as_table_mut()
        .context("config root must be a TOML table")?;
    let provider_id = provider_id_for_endpoint(&provider, normalized_optional(base_url));
    table.insert(
        "model_provider".to_string(),
        Value::String(provider_id.clone()),
    );
    table.insert("model".to_string(), Value::String(model.to_string()));

    let providers = table
        .entry("model_providers".to_string())
        .or_insert_with(|| Value::Table(Default::default()));
    let providers_table = providers
        .as_table_mut()
        .context("model_providers must be a TOML table")?;
    let profile = providers_table
        .entry(provider_id.clone())
        .or_insert_with(|| Value::Table(Default::default()));
    let profile_table = profile
        .as_table_mut()
        .context("provider config must be a TOML table")?;
    profile_table.insert(
        "name".to_string(),
        Value::String(provider_name_for_endpoint(
            &provider,
            normalized_optional(base_url),
        )),
    );
    profile_table.insert(
        "wire_api".to_string(),
        Value::String(provider.as_str().to_string()),
    );

    match normalized_optional(base_url) {
        Some(value) => {
            profile_table.insert("base_url".to_string(), Value::String(value.to_string()));
        }
        None => {
            profile_table.remove("base_url");
        }
    }

    match normalized_optional(api_key) {
        Some(value) => {
            profile_table.insert("api_key".to_string(), Value::String(value.to_string()));
        }
        None => {
            profile_table.remove("api_key");
        }
    }

    let models = profile_table
        .entry("models")
        .or_insert_with(|| Value::Array(Vec::new()));
    let models_array = models
        .as_array_mut()
        .context("provider models must be a TOML array")?;

    upsert_model_entry(
        models_array,
        model,
        normalized_optional(base_url),
        normalized_optional(api_key),
    );

    Ok(root)
}

fn merge_last_used_model(
    mut root: Value,
    wire_api: Option<ProviderWireApi>,
    provider: ProviderWireApi,
    model: &str,
) -> Result<Value> {
    let table = root
        .as_table_mut()
        .context("config root must be a TOML table")?;
    let provider_id = current_provider_id(table, &provider, model);
    table.insert(
        "model_provider".to_string(),
        Value::String(provider_id.clone()),
    );
    table.insert("model".to_string(), Value::String(model.to_string()));

    let providers = table
        .entry("model_providers".to_string())
        .or_insert_with(|| Value::Table(Default::default()));
    let providers_table = providers
        .as_table_mut()
        .context("model_providers must be a TOML table")?;
    let profile = providers_table
        .entry(provider_id)
        .or_insert_with(|| Value::Table(Default::default()));
    let profile_table = profile
        .as_table_mut()
        .context("provider config must be a TOML table")?;
    if let Some(wire_api) = wire_api.or_else(|| {
        profile_table
            .get("wire_api")
            .and_then(Value::as_str)
            .and_then(provider_wire_api_from_str)
    }) {
        profile_table.insert(
            "wire_api".to_string(),
            Value::String(wire_api_to_string(wire_api).to_string()),
        );
    }
    Ok(root)
}

fn current_provider_id(
    table: &toml::map::Map<String, Value>,
    provider: &ProviderWireApi,
    model: &str,
) -> String {
    table
        .get("model_providers")
        .and_then(Value::as_table)
        .and_then(|providers| {
            providers.iter().find_map(|(provider_id, value)| {
                let profile = value.as_table()?;
                let contains_model =
                    profile
                        .get("models")
                        .and_then(Value::as_array)
                        .is_some_and(|models| {
                            models.iter().any(|entry| {
                                entry
                                    .as_table()
                                    .and_then(|model_entry| model_entry.get("model"))
                                    .and_then(Value::as_str)
                                    == Some(model)
                            })
                        });
                let matches_last_model =
                    profile.get("last_model").and_then(Value::as_str) == Some(model);
                let matches_default_model =
                    profile.get("default_model").and_then(Value::as_str) == Some(model);
                (contains_model || matches_last_model || matches_default_model)
                    .then(|| provider_id.clone())
            })
        })
        .or_else(|| {
            table
                .get("model_provider")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            table
                .get("model_providers")
                .and_then(Value::as_table)
                .and_then(|providers| {
                    providers.iter().find_map(|(provider_id, value)| {
                        let profile = value.as_table()?;
                        let wire_api = profile.get("wire_api")?.as_str()?;
                        let matches_provider = match provider {
                            ProviderWireApi::AnthropicMessages => {
                                wire_api == ProviderWireApi::AnthropicMessages.as_str()
                            }
                            ProviderWireApi::OpenAIResponses => {
                                wire_api == ProviderWireApi::OpenAIResponses.as_str()
                            }
                            ProviderWireApi::OpenAIChatCompletions => {
                                wire_api == ProviderWireApi::OpenAIChatCompletions.as_str()
                            }
                        };
                        matches_provider.then(|| provider_id.clone())
                    })
                })
        })
        .unwrap_or_else(|| provider_id_for_endpoint(provider, None))
}

fn normalized_optional(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn provider_wire_api_from_str(value: &str) -> Option<ProviderWireApi> {
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

fn wire_api_to_string(wire_api: ProviderWireApi) -> &'static str {
    wire_api.as_str()
}

fn upsert_model_entry(
    models: &mut Vec<Value>,
    model: &str,
    base_url: Option<&str>,
    api_key: Option<&str>,
) {
    // Keep exactly one entry per model slug so repeated onboarding runs replace
    // the existing profile instead of appending duplicates.
    let mut entry = toml::map::Map::new();
    entry.insert("model".to_string(), Value::String(model.to_string()));
    if let Some(base_url) = base_url {
        entry.insert("base_url".to_string(), Value::String(base_url.to_string()));
    }
    if let Some(api_key) = api_key {
        entry.insert("api_key".to_string(), Value::String(api_key.to_string()));
    }

    if let Some(existing) = models.iter_mut().find(|value| {
        value
            .as_table()
            .and_then(|table| table.get("model"))
            .and_then(Value::as_str)
            == Some(model)
    }) {
        *existing = Value::Table(entry);
    } else {
        models.push(Value::Table(entry));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_optional_trims_and_drops_empty_values() {
        assert_eq!(
            normalized_optional(Some("  https://example.com  ")),
            Some("https://example.com")
        );
        assert_eq!(normalized_optional(Some("   ")), None);
        assert_eq!(normalized_optional(None), None);
    }

    #[test]
    fn merge_onboarding_config_creates_provider_profile_and_model_entry() {
        let root = Value::Table(Default::default());
        let merged = merge_onboarding_config(
            root,
            ProviderWireApi::OpenAIChatCompletions,
            "qwen3-coder-next",
            Some("https://example.com/v1"),
            Some("secret"),
        )
        .expect("merge");

        let table = merged.as_table().expect("table");
        assert_eq!(
            table.get("model_provider").and_then(Value::as_str),
            Some("example.com")
        );
        assert_eq!(
            table.get("model").and_then(Value::as_str),
            Some("qwen3-coder-next")
        );

        let profile = table
            .get("model_providers")
            .and_then(Value::as_table)
            .and_then(|providers| providers.get("example.com"))
            .and_then(Value::as_table)
            .expect("provider profile");
        assert_eq!(
            profile.get("name").and_then(Value::as_str),
            Some("example.com")
        );
        assert_eq!(
            profile.get("wire_api").and_then(Value::as_str),
            Some("openai_chat_completions")
        );
        assert_eq!(
            profile.get("base_url").and_then(Value::as_str),
            Some("https://example.com/v1")
        );
        assert_eq!(
            profile.get("api_key").and_then(Value::as_str),
            Some("secret")
        );

        let models = profile
            .get("models")
            .and_then(Value::as_array)
            .expect("models array");
        assert_eq!(models.len(), 1);
        assert_eq!(
            models[0]
                .as_table()
                .and_then(|entry| entry.get("model"))
                .and_then(Value::as_str),
            Some("qwen3-coder-next")
        );
    }

    #[test]
    fn merge_onboarding_config_upserts_existing_model_entry() {
        let mut root = Value::Table(Default::default());
        {
            let table = root.as_table_mut().expect("table");
            let mut profile = toml::map::Map::new();
            profile.insert(
                "models".to_string(),
                Value::Array(vec![Value::Table({
                    let mut entry = toml::map::Map::new();
                    entry.insert(
                        "model".to_string(),
                        Value::String("qwen3-coder-next".to_string()),
                    );
                    entry.insert(
                        "base_url".to_string(),
                        Value::String("http://old".to_string()),
                    );
                    entry.insert("api_key".to_string(), Value::String("old".to_string()));
                    entry
                })]),
            );
            let mut providers = toml::map::Map::new();
            providers.insert("old-host".to_string(), Value::Table(profile));
            table.insert("model_providers".to_string(), Value::Table(providers));
        }

        let merged = merge_onboarding_config(
            root,
            ProviderWireApi::OpenAIChatCompletions,
            "qwen3-coder-next",
            Some("https://new.example/v1"),
            Some("new-secret"),
        )
        .expect("merge");

        let models = merged
            .as_table()
            .and_then(|table| table.get("model_providers"))
            .and_then(Value::as_table)
            .and_then(|providers| providers.get("new.example"))
            .and_then(Value::as_table)
            .and_then(|profile| profile.get("models"))
            .and_then(Value::as_array)
            .expect("models array");
        assert_eq!(models.len(), 1);
        let entry = models[0].as_table().expect("model entry");
        assert_eq!(
            entry.get("base_url").and_then(Value::as_str),
            Some("https://new.example/v1")
        );
        assert_eq!(
            entry.get("api_key").and_then(Value::as_str),
            Some("new-secret")
        );
    }

    #[test]
    fn merge_last_used_model_prefers_profile_that_contains_model() {
        let root: Value = r#"
model_provider = "anthropic"

[model_providers.anthropic]
wire_api = "anthropic_messages"

[[model_providers.anthropic.models]]
model = "claude-sonnet-4"

[model_providers.openai]
wire_api = "openai_chat_completions"

[[model_providers.openai.models]]
model = "gpt-5.4"
"#
        .parse()
        .expect("parse");

        let merged =
            merge_last_used_model(root, None, ProviderWireApi::AnthropicMessages, "gpt-5.4")
                .expect("merge");

        let table = merged.as_table().expect("table");
        assert_eq!(
            table.get("model_provider").and_then(Value::as_str),
            Some("openai")
        );
        assert_eq!(
            table
                .get("model_providers")
                .and_then(Value::as_table)
                .and_then(|providers| providers.get("openai"))
                .and_then(Value::as_table)
                .and_then(|profile| profile.get("wire_api"))
                .and_then(Value::as_str),
            Some("openai_chat_completions")
        );
    }

    #[test]
    fn merge_last_used_model_preserves_existing_wire_api_when_not_provided() {
        let root: Value = r#"
[model_providers.openai]
wire_api = "openai_responses"

[[model_providers.openai.models]]
model = "gpt-5.4"
"#
        .parse()
        .expect("parse");

        let merged = merge_last_used_model(root, None, ProviderWireApi::OpenAIResponses, "gpt-5.4")
            .expect("merge");

        assert_eq!(
            merged
                .as_table()
                .and_then(|table| table.get("model_providers"))
                .and_then(Value::as_table)
                .and_then(|providers| providers.get("openai"))
                .and_then(Value::as_table)
                .and_then(|profile| profile.get("wire_api"))
                .and_then(Value::as_str),
            Some("openai_responses")
        );
    }

    #[test]
    fn merge_thinking_selection_updates_and_removes_value() {
        let merged = merge_thinking_selection(Value::Table(Default::default()), Some("medium"))
            .expect("merge");
        assert_eq!(
            merged
                .as_table()
                .and_then(|table| table.get("model_thinking_selection"))
                .and_then(Value::as_str),
            Some("medium")
        );

        let removed = merge_thinking_selection(merged, None).expect("remove");
        assert_eq!(
            removed
                .as_table()
                .and_then(|table| table.get("model_thinking_selection")),
            None
        );
    }
}
