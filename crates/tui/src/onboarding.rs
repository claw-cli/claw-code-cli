use anyhow::{Context, Result};
use clawcr_core::{provider_id_for_endpoint, provider_name_for_endpoint};
use clawcr_protocol::ProviderFamily;
use clawcr_utils::find_clawcr_home;
use toml::Value;

/// Persists the onboarding choice into the user's `config.toml`.
pub(crate) fn save_onboarding_config(
    provider: ProviderFamily,
    model: &str,
    base_url: Option<&str>,
    api_key: Option<&str>,
) -> Result<()> {
    let path = find_clawcr_home()
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

pub(crate) fn save_last_used_model(provider: ProviderFamily, model: &str) -> Result<()> {
    let path = find_clawcr_home()
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
    root = merge_last_used_model(root, provider, model)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(&root)?;

    std::fs::write(&path, rendered)
        .with_context(|| format!("failed to write {}", path.display()))?;

    Ok(())
}

fn merge_onboarding_config(
    mut root: Value,
    provider: ProviderFamily,
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
        Value::String(match provider {
            ProviderFamily::Anthropic { .. } => "anthropic_messages".to_string(),
            ProviderFamily::Openai { .. } => "openai_chat_completions".to_string(),
        }),
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

fn merge_last_used_model(mut root: Value, provider: ProviderFamily, model: &str) -> Result<Value> {
    let table = root
        .as_table_mut()
        .context("config root must be a TOML table")?;
    let provider_id = current_provider_id(table, &provider);
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
    profile_table.insert(
        "wire_api".to_string(),
        Value::String(match provider {
            ProviderFamily::Anthropic { .. } => "anthropic_messages".to_string(),
            ProviderFamily::Openai { .. } => "openai_chat_completions".to_string(),
        }),
    );
    Ok(root)
}

fn current_provider_id(table: &toml::map::Map<String, Value>, provider: &ProviderFamily) -> String {
    table
        .get("model_provider")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            table
                .get("model_providers")
                .and_then(Value::as_table)
                .and_then(|providers| {
                    providers.iter().find_map(|(provider_id, value)| {
                        let profile = value.as_table()?;
                        let wire_api = profile.get("wire_api")?.as_str()?;
                        let matches_provider = match provider {
                            ProviderFamily::Anthropic { .. } => wire_api == "anthropic_messages",
                            ProviderFamily::Openai { .. } => {
                                wire_api == "openai_chat_completions"
                                    || wire_api == "openai_responses"
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
            ProviderFamily::openai(),
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
            ProviderFamily::openai(),
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
}
