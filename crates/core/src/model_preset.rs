//! Raw model preset types used to load the builtin catalog.
//!
//! Main focus:
//! - deserialize bundled model definitions from `models.json`
//! - preserve JSON compatibility and catalog-only metadata such as priority and API-config flags
//! - convert raw presets into runtime `clawcr_protocol::Model` values
//!
//! Design:
//! - `ModelPreset` is intentionally a core-only type because it exists to support catalog loading
//! - serde adapters and legacy field aliases live here so they do not leak into the runtime model
//! - conversion into `Model` is the handoff point from config data to executable runtime data
//!
//! Boundary:
//! - this module should not act as the runtime model API seen by server, client, or query code
//! - turn execution should consume `Model`, not `ModelPreset`
//! - loading policy and catalog access live in `model_catalog.rs`; this file only defines the raw shape
//!
use clawcr_protocol::{
    InputModality, Model, ProviderFamily, ReasoningEffort, ThinkingCapability,
    ThinkingImplementation, TruncationPolicyConfig,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// Raw catalog preset loaded from the bundled model JSON.
pub struct ModelPreset {
    /// Stable model identifier used in config and requests. such as `claude-sonnet-20250425`
    pub slug: String,
    /// Human-readable display name shown in the UI. such as `claude-sonnet-4.6`
    pub display_name: String,
    /// Provider selection that serves this model.
    #[serde(
        default,
        alias = "provider_family",
        deserialize_with = "clawcr_protocol::deserialize_provider"
    )]
    pub provider: ProviderFamily,
    /// Optional short description of the model.
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub description: Option<String>,
    /// Thinking control available for this model.
    #[serde(
        default = "default_thinking_capability",
        deserialize_with = "deserialize_thinking_capability"
    )]
    pub thinking_capability: ThinkingCapability,
    /// Default reasoning effort selected for the model when no levels are exposed.
    #[serde(
        default = "default_reasoning_effort",
        alias = "default_reasoning_level",
        deserialize_with = "deserialize_reasoning_effort_option"
    )]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    /// How the selected thinking mode should be applied to requests.
    #[serde(default)]
    pub thinking_implementation: Option<ThinkingImplementation>,
    /// Base system instructions bundled with the model.
    pub base_instructions: String,
    /// Maximum context window in tokens.
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    /// Percentage of the context window treated as effectively usable.
    pub effective_context_window_percent: Option<u8>,
    /// Policy used when truncating content for requests.
    #[serde(
        default,
        deserialize_with = "clawcr_protocol::deserialize_truncation_policy_config"
    )]
    pub truncation_policy: TruncationPolicyConfig,
    /// Input types accepted by the model.
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
    /// Whether the model supports original-resolution image detail.
    pub supports_image_detail_original: bool,
    /// Whether the user configured API access for this model.
    #[serde(rename = "supported_in_api")]
    pub api_configured: bool,
    /// Default temperature to use when the model does not override it.
    pub temperature: Option<f64>,
    /// Default nucleus sampling value to use when the model does not override it.
    pub top_p: Option<f64>,
    /// Default top-k sampling value to use when the model does not override it.
    pub top_k: Option<f64>,
    /// Default maximum token limit for responses from this model.
    pub max_tokens: Option<u32>,
    /// Relative priority used when choosing a default visible model.
    pub priority: i32,
}

impl Default for ModelPreset {
    fn default() -> Self {
        Self {
            slug: String::new(),
            display_name: String::new(),
            provider: ProviderFamily::openai(),
            description: None,
            thinking_capability: ThinkingCapability::Disabled,
            default_reasoning_effort: Some(ReasoningEffort::default()),
            thinking_implementation: None,
            base_instructions: String::new(),
            context_window: 200_000,
            effective_context_window_percent: None,
            truncation_policy: TruncationPolicyConfig::default(),
            input_modalities: vec![InputModality::default()],
            supports_image_detail_original: false,
            api_configured: false,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
            priority: 0,
        }
    }
}

impl From<ModelPreset> for Model {
    fn from(value: ModelPreset) -> Self {
        Self {
            slug: value.slug,
            display_name: value.display_name,
            provider: value.provider,
            description: value.description,
            thinking_capability: value.thinking_capability,
            default_reasoning_effort: value.default_reasoning_effort,
            thinking_implementation: value.thinking_implementation,
            base_instructions: value.base_instructions,
            context_window: value.context_window,
            effective_context_window_percent: value.effective_context_window_percent,
            truncation_policy: value.truncation_policy,
            input_modalities: value.input_modalities,
            supports_image_detail_original: value.supports_image_detail_original,
            temperature: value.temperature,
            top_p: value.top_p,
            top_k: value.top_k,
            max_tokens: value.max_tokens,
        }
    }
}

fn default_reasoning_effort() -> Option<ReasoningEffort> {
    Some(ReasoningEffort::default())
}

fn default_context_window() -> u32 {
    200_000
}

fn default_input_modalities() -> Vec<InputModality> {
    vec![InputModality::Text, InputModality::Image]
}

fn default_thinking_capability() -> ThinkingCapability {
    ThinkingCapability::Disabled
}

fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(value)
        }
    }))
}

fn deserialize_reasoning_effort_option<'de, D>(
    deserializer: D,
) -> Result<Option<ReasoningEffort>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(default_reasoning_effort()),
        serde_json::Value::String(text) if text.trim().is_empty() => Ok(default_reasoning_effort()),
        other => serde_json::from_value(other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn deserialize_thinking_capability<'de, D>(deserializer: D) -> Result<ThinkingCapability, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null => Ok(default_thinking_capability()),
        serde_json::Value::String(text) if text.trim().is_empty() => {
            Ok(default_thinking_capability())
        }
        other => serde_json::from_value(other).map_err(serde::de::Error::custom),
    }
}
