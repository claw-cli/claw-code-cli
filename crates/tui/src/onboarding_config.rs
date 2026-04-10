use anyhow::{Context, Result};
use clawcr_core::ProviderKind;
use clawcr_utils::find_clawcr_home;
use toml::Value;

/// Persists the onboarding choice into the user's `config.toml`.
pub(crate) fn save_onboarding_config(
    provider: ProviderKind,
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

pub(crate) fn save_last_used_model(provider: ProviderKind, model: &str) -> Result<()> {
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
    provider: ProviderKind,
    model: &str,
    base_url: Option<&str>,
    api_key: Option<&str>,
) -> Result<Value> {
    // Preserve unrelated config keys while updating only the onboarding-selected
    // provider profile.
    let table = root
        .as_table_mut()
        .context("config root must be a TOML table")?;
    table.insert(
        "default_provider".to_string(),
        Value::String(provider.as_str().to_string()),
    );

    let profile = table
        .entry(provider.as_str().to_string())
        .or_insert_with(|| Value::Table(Default::default()));
    let profile_table = profile
        .as_table_mut()
        .context("provider config must be a TOML table")?;

    profile_table.insert("last_model".to_string(), Value::String(model.to_string()));
    profile_table.insert(
        "default_model".to_string(),
        Value::String(model.to_string()),
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

fn merge_last_used_model(mut root: Value, provider: ProviderKind, model: &str) -> Result<Value> {
    let table = root
        .as_table_mut()
        .context("config root must be a TOML table")?;
    table.insert(
        "default_provider".to_string(),
        Value::String(provider.as_str().to_string()),
    );

    let profile = table
        .entry(provider.as_str().to_string())
        .or_insert_with(|| Value::Table(Default::default()));
    let profile_table = profile
        .as_table_mut()
        .context("provider config must be a TOML table")?;
    profile_table.insert("last_model".to_string(), Value::String(model.to_string()));
    Ok(root)
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
            ProviderKind::Openai,
            "qwen3-coder-next",
            Some("https://example.com/v1"),
            Some("secret"),
        )
        .expect("merge");

        let table = merged.as_table().expect("table");
        assert_eq!(
            table.get("default_provider").and_then(Value::as_str),
            Some("openai")
        );

        let profile = table
            .get("openai")
            .and_then(Value::as_table)
            .expect("provider profile");
        assert_eq!(
            profile.get("default_model").and_then(Value::as_str),
            Some("qwen3-coder-next")
        );
        assert_eq!(
            profile.get("last_model").and_then(Value::as_str),
            Some("qwen3-coder-next")
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
            table.insert("openai".to_string(), Value::Table(profile));
        }

        let merged = merge_onboarding_config(
            root,
            ProviderKind::Openai,
            "qwen3-coder-next",
            Some("https://new.example/v1"),
            Some("new-secret"),
        )
        .expect("merge");

        let models = merged
            .as_table()
            .and_then(|table| table.get("openai"))
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
