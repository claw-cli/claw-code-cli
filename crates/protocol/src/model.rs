//! Runtime model types shared across core, server, and clients.
//!
//! Main focus:
//! - represent the resolved model shape used during execution
//! - expose model capabilities needed by UI, request building, and turn resolution
//! - provide the read-only catalog trait over runtime `Model` values
//!
//! Design:
//! - `Model` is the cross-crate runtime type, not the raw config/catalog input type
//! - this module keeps behavior that belongs to the executable model itself, such as
//!   thinking resolution and effective defaults
//! - callers should be able to use this type without knowing how the model catalog was loaded
//!
//! Boundary:
//! - this module must not own bundled JSON loading or compatibility parsing for catalog files
//! - raw preset/config concerns live in `devo-core`
//! - this module describes runtime state and runtime-facing interfaces only
//!
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::fmt;

use crate::ReasoningEffort;
use crate::ReasoningEffortPreset;
use crate::ResolvedThinkingRequest;
use crate::ThinkingCapability;
use crate::ThinkingImplementation;
use crate::nearest_effort;
use crate::truncation::TruncationPolicyConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Verbosity {
    Low,
    #[default]
    Medium,
    High,
}

/// Sampling controls and model-selection hints shared across adapters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingControls {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
}

/// A message in the request to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessage {
    pub role: String,
    pub content: Vec<RequestContent>,
}

/// Full request to the model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<RequestMessage>,
    pub max_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(default)]
    pub sampling: SamplingControls,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<Value>,
}

/// A tool definition sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A content block within a message sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RequestContent {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "reasoning")]
    Reasoning { text: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Supported input types that a model can accept.
#[derive(Default)]
pub enum InputModality {
    /// Plain text input.
    #[default]
    Text,
    /// Image input.
    Image,
}

/// OpenAI-family API surfaces supported by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum OpenAiApi {
    #[default]
    ChatCompletions,
    Responses,
}

/// Anthropic-family API surfaces supported by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicApi {
    Messages,
}

/// Provider identity plus its selected wire API.
impl Default for AnthropicApi {
    fn default() -> Self {
        AnthropicApi::Messages
    }
}

/// One supported provider wire protocol exposed by the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Returns the canonical config and environment string for this wire API.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAIChatCompletions => "openai_chat_completions",
            Self::OpenAIResponses => "openai_responses",
            Self::AnthropicMessages => "anthropic_messages",
        }
    }
}

impl fmt::Display for ProviderWireApi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ProviderWireApi> for &'static str {
    fn from(value: ProviderWireApi) -> Self {
        value.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// Resolved runtime model metadata used across core, server, and clients.
pub struct Model {
    /// Stable model identifier used in config and requests. such as `claude-sonnet-20250425`
    pub slug: String,
    /// Human-readable display name shown in the UI. such as `claude-sonnet-4.6`
    pub display_name: String,
    /// Provider selection that serves this model.
    pub provider: ProviderWireApi,
    /// Optional short description of the model.
    pub description: Option<String>,
    /// Thinking control available for this model.
    pub thinking_capability: ThinkingCapability,
    /// Default reasoning effort selected for the model when no levels are exposed.
    pub default_reasoning_effort: Option<ReasoningEffort>,
    /// How the selected thinking mode should be applied to requests.
    pub thinking_implementation: Option<ThinkingImplementation>,
    /// Base system instructions bundled with the model.
    pub base_instructions: String,
    /// Maximum context window in tokens.
    pub context_window: u32,
    /// Percentage of the context window treated as effectively usable.
    pub effective_context_window_percent: Option<u8>,
    /// Policy used when truncating content for requests.
    pub truncation_policy: TruncationPolicyConfig,
    /// Input types accepted by the model.
    pub input_modalities: Vec<InputModality>,
    /// Whether the model supports original-resolution image detail.
    pub supports_image_detail_original: bool,
    /// Grouping label used to organize models by vendor or family.
    pub channel: Option<String>,
    /// Default temperature to use when the model does not override it.
    pub temperature: Option<f64>,
    /// Default nucleus sampling value to use when the model does not override it.
    pub top_p: Option<f64>,
    /// Default top-k sampling value to use when the model does not override it.
    pub top_k: Option<f64>,
    /// Default maximum token limit for responses from this model.
    pub max_tokens: Option<u32>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            slug: String::new(),
            display_name: String::new(),
            provider: ProviderWireApi::OpenAIChatCompletions,
            description: None,
            thinking_capability: ThinkingCapability::Unsupported,
            default_reasoning_effort: Some(ReasoningEffort::default()),
            thinking_implementation: None,
            base_instructions: String::new(),
            context_window: 200_000,
            effective_context_window_percent: None,
            truncation_policy: TruncationPolicyConfig::default(),
            input_modalities: vec![InputModality::default()],
            supports_image_detail_original: false,
            channel: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
        }
    }
}

impl Model {
    pub fn provider_wire_api(&self) -> ProviderWireApi {
        self.provider
    }

    pub fn reasoning_effort_options(&self) -> Vec<ReasoningEffortPreset> {
        match &self.thinking_capability {
            ThinkingCapability::Levels(levels) => levels
                .iter()
                .copied()
                .map(|effort| ReasoningEffortPreset::new(effort, effort.description()))
                .collect(),
            ThinkingCapability::ToggleWithLevels(levels) => levels
                .iter()
                .copied()
                .map(|effort| ReasoningEffortPreset::new(effort, effort.description()))
                .collect(),
            _ => self
                .default_reasoning_effort
                .iter()
                .copied()
                .map(|effort| ReasoningEffortPreset::new(effort, effort.description()))
                .collect(),
        }
    }

    pub fn effective_thinking_capability(&self) -> ThinkingCapability {
        self.thinking_capability.clone()
    }

    pub fn effective_thinking_implementation(&self) -> ThinkingImplementation {
        self.thinking_implementation.clone().unwrap_or({
            if matches!(self.thinking_capability, ThinkingCapability::Unsupported) {
                ThinkingImplementation::Disabled
            } else {
                ThinkingImplementation::RequestParameter
            }
        })
    }

    pub fn effective_context_window_percent(&self) -> u8 {
        self.effective_context_window_percent.unwrap_or(95)
    }

    pub fn default_thinking_selection(&self) -> Option<String> {
        match &self.thinking_capability {
            ThinkingCapability::Unsupported => None,
            ThinkingCapability::Toggle => Some(String::from("enabled")),
            ThinkingCapability::ToggleWithLevels(levels) => self
                .default_reasoning_effort
                .or_else(|| levels.first().copied())
                .map(|effort| effort.label().to_lowercase()),
            ThinkingCapability::Levels(levels) => self
                .default_reasoning_effort
                .or_else(|| levels.first().copied())
                .map(|effort| effort.label().to_lowercase()),
        }
    }

    pub fn nearest_supported_reasoning_effort(&self, target: ReasoningEffort) -> ReasoningEffort {
        match &self.thinking_capability {
            ThinkingCapability::Levels(levels) | ThinkingCapability::ToggleWithLevels(levels)
                if !levels.is_empty() =>
            {
                nearest_effort(target, levels)
            }
            _ => self.default_reasoning_effort.unwrap_or(target),
        }
    }

    pub fn resolve_thinking_selection(&self, selection: Option<&str>) -> ResolvedThinkingRequest {
        let normalized_selection = selection
            .map(str::trim)
            .filter(|selection| !selection.is_empty())
            .map(|selection| selection.to_ascii_lowercase())
            .or_else(|| self.default_thinking_selection());

        match self.effective_thinking_implementation() {
            ThinkingImplementation::Disabled => ResolvedThinkingRequest {
                request_model: self.slug.clone(),
                request_thinking: None,
                request_reasoning_effort: None,
                effective_reasoning_effort: None,
                extra_body: None,
            },
            ThinkingImplementation::RequestParameter => {
                let (request_thinking, request_reasoning_effort, effective_reasoning_effort) =
                    match self.effective_thinking_capability() {
                        ThinkingCapability::Unsupported => (None, None, None),
                        ThinkingCapability::Toggle => {
                            let request_thinking = normalized_selection
                                .filter(|selection| {
                                    selection == "enabled" || selection == "disabled"
                                })
                                .or_else(|| self.default_thinking_selection());
                            let effective_reasoning_effort = self.default_reasoning_effort;
                            (request_thinking, None, effective_reasoning_effort)
                        }
                        ThinkingCapability::Levels(_) => {
                            let request_reasoning_effort = normalized_selection
                                .as_deref()
                                .and_then(|selection| selection.parse::<ReasoningEffort>().ok())
                                .map(|effort| self.nearest_supported_reasoning_effort(effort))
                                .or(self.default_reasoning_effort);
                            (
                                request_reasoning_effort
                                    .map(|effort| effort.label().to_lowercase()),
                                request_reasoning_effort,
                                request_reasoning_effort,
                            )
                        }
                        ThinkingCapability::ToggleWithLevels(_) => {
                            let request_reasoning_effort = normalized_selection
                                .as_deref()
                                .and_then(|selection| match selection {
                                    "enabled" => self.default_reasoning_effort,
                                    "disabled" => None,
                                    _ => selection.parse::<ReasoningEffort>().ok(),
                                })
                                .map(|effort| self.nearest_supported_reasoning_effort(effort))
                                .or_else(|| {
                                    normalized_selection
                                        .as_deref()
                                        .filter(|selection| *selection == "enabled")
                                        .and(self.default_reasoning_effort)
                                });
                            let request_thinking = normalized_selection.as_deref().map_or_else(
                                || {
                                    request_reasoning_effort
                                        .map(|_| String::from("enabled"))
                                        .or_else(|| Some(String::from("disabled")))
                                },
                                |selection| {
                                    if selection == "disabled" {
                                        Some(String::from("disabled"))
                                    } else {
                                        Some(String::from("enabled"))
                                    }
                                },
                            );
                            (
                                request_thinking,
                                request_reasoning_effort,
                                request_reasoning_effort,
                            )
                        }
                    };
                ResolvedThinkingRequest {
                    request_model: self.slug.clone(),
                    request_thinking,
                    request_reasoning_effort,
                    effective_reasoning_effort,
                    extra_body: None,
                }
            }
            ThinkingImplementation::ModelVariant(config) => {
                let selected_variant = normalized_selection
                    .as_deref()
                    .and_then(|selection| {
                        config
                            .variants
                            .iter()
                            .find(|variant| variant.selection_value.eq_ignore_ascii_case(selection))
                    })
                    .or_else(|| {
                        self.default_thinking_selection()
                            .as_deref()
                            .and_then(|selection| {
                                config.variants.iter().find(|variant| {
                                    variant.selection_value.eq_ignore_ascii_case(selection)
                                })
                            })
                    })
                    .or_else(|| config.variants.first());
                if let Some(variant) = selected_variant {
                    ResolvedThinkingRequest {
                        request_model: variant.model_slug.clone(),
                        request_thinking: None,
                        request_reasoning_effort: variant.reasoning_effort,
                        effective_reasoning_effort: variant.reasoning_effort,
                        extra_body: None,
                    }
                } else {
                    ResolvedThinkingRequest {
                        request_model: self.slug.clone(),
                        request_thinking: None,
                        request_reasoning_effort: self.default_reasoning_effort,
                        effective_reasoning_effort: self.default_reasoning_effort,
                        extra_body: None,
                    }
                }
            }
        }
    }
}

/// Provides read-only access to resolved runtime model definitions.
pub trait ModelCatalog: Send + Sync {
    /// Lists all models that are available for user-facing selection.
    fn list_visible(&self) -> Vec<&Model>;

    /// Returns the model whose slug exactly matches `slug`.
    fn get(&self, slug: &str) -> Option<&Model>;

    /// Resolves the model that should be used for a turn.
    ///
    /// `requested` is an optional model slug supplied by the caller. When it is
    /// present, implementations must return that exact model or
    /// [`ModelError::ModelNotFound`]. When it is absent, implementations should
    /// return their highest-priority visible model or [`ModelError::NoVisibleModels`]
    /// if no selectable model exists.
    fn resolve_for_turn(&self, requested: Option<&str>) -> Result<&Model, ModelError>;
}

#[derive(Debug, Clone)]
pub struct InMemoryModelCatalog {
    models: Vec<Model>,
}

impl InMemoryModelCatalog {
    pub fn new(models: Vec<Model>) -> Self {
        Self { models }
    }
}

impl ModelCatalog for InMemoryModelCatalog {
    fn list_visible(&self) -> Vec<&Model> {
        self.models.iter().collect()
    }

    fn get(&self, slug: &str) -> Option<&Model> {
        self.models.iter().find(|model| model.slug == slug)
    }

    fn resolve_for_turn(&self, requested: Option<&str>) -> Result<&Model, ModelError> {
        if let Some(slug) = requested {
            return self.get(slug).ok_or_else(|| ModelError::ModelNotFound {
                slug: slug.to_string(),
            });
        }

        self.list_visible()
            .into_iter()
            .next()
            .ok_or(ModelError::NoVisibleModels)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("model not found: {slug}")]
    ModelNotFound { slug: String },
    #[error("no visible models available")]
    NoVisibleModels,
}

// ── model/catalog API ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelCatalogParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogResult {
    pub models: Vec<ModelCatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogEntry {
    pub slug: String,
    pub display_name: String,
    pub channel: Option<String>,
    pub description: Option<String>,
    pub provider: ProviderWireApi,
    pub context_window: u32,
    pub thinking_capability: ThinkingCapability,
    pub input_modalities: Vec<InputModality>,
    pub max_tokens: Option<u32>,
}

impl From<&Model> for ModelCatalogEntry {
    fn from(m: &Model) -> Self {
        Self {
            slug: m.slug.clone(),
            display_name: m.display_name.clone(),
            channel: m.channel.clone(),
            description: m.description.clone(),
            provider: m.provider,
            context_window: m.context_window,
            thinking_capability: m.thinking_capability.clone(),
            input_modalities: m.input_modalities.clone(),
            max_tokens: m.max_tokens,
        }
    }
}

// ── model/saved API ─────────────────────────────────────────────────

/// Lists models that have been configured with credentials in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ModelSavedParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSavedResult {
    pub models: Vec<ModelSavedEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSavedEntry {
    pub slug: String,
    pub display_name: String,
    pub channel: Option<String>,
    pub description: Option<String>,
    pub provider_id: String,
    pub wire_api: ProviderWireApi,
    pub context_window: u32,
}

#[cfg(test)]
mod tests {
    use crate::RequestRole;
    use crate::ThinkingVariant;
    use crate::ThinkingVariantConfig;
    use pretty_assertions::assert_eq;

    use super::InMemoryModelCatalog;
    use super::InputModality;
    use super::Model;
    use super::ModelCatalog;
    use super::ProviderWireApi;
    use super::ReasoningEffort;
    use super::ThinkingCapability;
    use super::ThinkingImplementation;
    use super::TruncationPolicyConfig;

    fn model(slug: &str) -> Model {
        Model {
            slug: slug.into(),
            display_name: slug.into(),
            provider: ProviderWireApi::OpenAIChatCompletions,
            description: None,
            thinking_capability: ThinkingCapability::Unsupported,
            default_reasoning_effort: Some(ReasoningEffort::Medium),
            thinking_implementation: None,
            base_instructions: String::new(),
            context_window: 200_000,
            effective_context_window_percent: None,
            truncation_policy: TruncationPolicyConfig {
                mode: crate::TruncationMode::Tokens,
                limit: 10000,
            },
            input_modalities: vec![InputModality::Text],
            supports_image_detail_original: false,
            channel: None,
            temperature: None,
            top_p: None,
            top_k: None,
            max_tokens: None,
        }
    }

    #[test]
    fn resolve_for_turn_honors_requested_slug() {
        let catalog = InMemoryModelCatalog::new(vec![model("test")]);
        let resolved = catalog
            .resolve_for_turn(Some("test"))
            .expect("resolve explicit");
        assert_eq!(resolved.slug, "test");
    }

    #[test]
    fn provider_wire_api_as_str_returns_canonical_values() {
        for (wire_api, expected) in [
            (
                ProviderWireApi::OpenAIChatCompletions,
                "openai_chat_completions",
            ),
            (ProviderWireApi::OpenAIResponses, "openai_responses"),
            (ProviderWireApi::AnthropicMessages, "anthropic_messages"),
        ] {
            assert_eq!(wire_api.as_str(), expected);
        }
    }

    #[test]
    fn provider_wire_api_as_str_matches_serialized_values() {
        for wire_api in [
            ProviderWireApi::OpenAIChatCompletions,
            ProviderWireApi::OpenAIResponses,
            ProviderWireApi::AnthropicMessages,
        ] {
            assert_eq!(
                serde_json::to_value(wire_api).expect("serialize wire api"),
                serde_json::Value::String(wire_api.as_str().to_string())
            );
        }
    }

    #[test]
    fn provider_wire_api_display_and_from_use_canonical_values() {
        for (wire_api, expected) in [
            (
                ProviderWireApi::OpenAIChatCompletions,
                "openai_chat_completions",
            ),
            (ProviderWireApi::OpenAIResponses, "openai_responses"),
            (ProviderWireApi::AnthropicMessages, "anthropic_messages"),
        ] {
            let converted: &'static str = wire_api.into();

            assert_eq!(wire_api.to_string(), expected);
            assert_eq!(converted, expected);
        }
    }

    #[test]
    fn resolve_thinking_selection_disables_request_thinking_when_capability_is_disabled() {
        let preset = model("test");

        let resolved = preset.resolve_thinking_selection(Some("enabled"));

        assert_eq!(resolved.request_model, "test");
        assert_eq!(resolved.request_thinking, None);
        assert_eq!(resolved.effective_reasoning_effort, None);
    }

    #[test]
    fn resolve_thinking_selection_uses_request_parameter_for_toggle_models() {
        let mut preset = model("glm-5.1");
        preset.thinking_capability = ThinkingCapability::Toggle;

        let resolved = preset.resolve_thinking_selection(Some("disabled"));

        assert_eq!(resolved.request_model, "glm-5.1");
        assert_eq!(resolved.request_thinking, Some(String::from("disabled")));
        assert_eq!(
            resolved.effective_reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn resolve_thinking_selection_snaps_effort_for_level_models() {
        let mut preset = model("o-model");
        preset.thinking_capability =
            ThinkingCapability::Levels(vec![ReasoningEffort::Low, ReasoningEffort::High]);
        preset.default_reasoning_effort = Some(ReasoningEffort::Low);

        let resolved = preset.resolve_thinking_selection(Some("medium"));

        assert_eq!(resolved.request_model, "o-model");
        assert_eq!(resolved.request_thinking, Some(String::from("low")));
        assert_eq!(
            resolved.effective_reasoning_effort,
            Some(ReasoningEffort::Low)
        );
    }

    #[test]
    fn resolve_thinking_selection_supports_toggle_with_levels() {
        let mut preset = model("deepseek-v4");
        preset.thinking_capability =
            ThinkingCapability::ToggleWithLevels(vec![ReasoningEffort::High, ReasoningEffort::Max]);
        preset.default_reasoning_effort = Some(ReasoningEffort::High);

        let enabled = preset.resolve_thinking_selection(Some("enabled"));
        assert_eq!(enabled.request_thinking, Some(String::from("enabled")));
        assert_eq!(
            enabled.request_reasoning_effort,
            Some(ReasoningEffort::High)
        );
        assert_eq!(
            enabled.effective_reasoning_effort,
            Some(ReasoningEffort::High)
        );

        let max = preset.resolve_thinking_selection(Some("max"));
        assert_eq!(max.request_thinking, Some(String::from("enabled")));
        assert_eq!(max.request_reasoning_effort, Some(ReasoningEffort::Max));
        assert_eq!(max.effective_reasoning_effort, Some(ReasoningEffort::Max));

        let disabled = preset.resolve_thinking_selection(Some("disabled"));
        assert_eq!(disabled.request_thinking, Some(String::from("disabled")));
        assert_eq!(disabled.request_reasoning_effort, None);
        assert_eq!(disabled.effective_reasoning_effort, None);
    }

    #[test]
    fn resolve_thinking_selection_uses_model_variants_when_configured() {
        let mut preset = model("kimi-k2.5");
        preset.thinking_capability = ThinkingCapability::Toggle;
        preset.thinking_implementation = Some(ThinkingImplementation::ModelVariant(
            ThinkingVariantConfig {
                variants: vec![
                    ThinkingVariant {
                        selection_value: String::from("disabled"),
                        model_slug: String::from("kimi-k2.5"),
                        reasoning_effort: None,
                        label: String::from("Off"),
                        description: String::from("Use the standard model"),
                    },
                    ThinkingVariant {
                        selection_value: String::from("enabled"),
                        model_slug: String::from("kimi-k2.5-thinking"),
                        reasoning_effort: Some(ReasoningEffort::Medium),
                        label: String::from("On"),
                        description: String::from("Use the thinking model"),
                    },
                ],
            },
        ));

        let resolved = preset.resolve_thinking_selection(Some("enabled"));

        assert_eq!(resolved.request_model, "kimi-k2.5-thinking");
        assert_eq!(resolved.request_thinking, None);
        assert_eq!(
            resolved.effective_reasoning_effort,
            Some(ReasoningEffort::Medium)
        );
    }

    #[test]
    fn resolve_thinking_selection_falls_back_to_first_variant_when_selection_is_invalid() {
        let mut preset = model("deepseek-chat");
        preset.thinking_capability = ThinkingCapability::Toggle;
        preset.thinking_implementation = Some(ThinkingImplementation::ModelVariant(
            ThinkingVariantConfig {
                variants: vec![ThinkingVariant {
                    selection_value: String::from("disabled"),
                    model_slug: String::from("deepseek-chat"),
                    reasoning_effort: None,
                    label: String::from("Off"),
                    description: String::from("Use the standard model"),
                }],
            },
        ));

        let resolved = preset.resolve_thinking_selection(Some("invalid"));

        assert_eq!(resolved.request_model, "deepseek-chat");
        assert_eq!(resolved.request_thinking, None);
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn tool_definition_serde_roundtrip() {
        let def = ToolDefinition {
            name: "bash".into(),
            description: "run commands".into(),
            input_schema: json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "bash");
        assert_eq!(deserialized.description, "run commands");
    }

    #[test]
    fn request_content_text_serde() {
        let content = RequestContent::Text {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"text""#));
        let deserialized: RequestContent = serde_json::from_str(&json).unwrap();
        match deserialized {
            RequestContent::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected Text"),
        }
    }

    #[test]
    fn request_content_reasoning_serde() {
        let content = RequestContent::Reasoning {
            text: "hello".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains(r#""type":"reasoning""#));
        let deserialized: RequestContent = serde_json::from_str(&json).unwrap();
        match deserialized {
            RequestContent::Reasoning { text } => assert_eq!(text, "hello"),
            _ => panic!("expected Reasoning"),
        }
    }

    #[test]
    fn request_content_tool_result_skips_none_error() {
        let content = RequestContent::ToolResult {
            tool_use_id: "t1".into(),
            content: "ok".into(),
            is_error: None,
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(!json.contains("is_error"));
    }

    #[test]
    fn request_content_tool_result_includes_error() {
        let content = RequestContent::ToolResult {
            tool_use_id: "t1".into(),
            content: "failed".into(),
            is_error: Some(true),
        };
        let json = serde_json::to_string(&content).unwrap();
        assert!(json.contains("is_error"));
    }

    #[test]
    fn model_request_serde() {
        let req = ModelRequest {
            model: "claude-sonnet-4-20250514".into(),
            system: Some("You are helpful.".into()),
            messages: vec![RequestMessage {
                role: "user".into(),
                content: vec![RequestContent::Text { text: "hi".into() }],
            }],
            max_tokens: 4096,
            tools: None,
            sampling: SamplingControls::default(),
            thinking: None,
            reasoning_effort: None,
            extra_body: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("tools"));
        assert!(!json.contains("temperature"));
        let deserialized: ModelRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.model, "claude-sonnet-4-20250514");
        assert_eq!(deserialized.messages.len(), 1);
    }

    #[test]
    fn request_role_roundtrip() {
        for role in [
            RequestRole::System,
            RequestRole::Developer,
            RequestRole::User,
            RequestRole::Assistant,
            RequestRole::Tool,
            RequestRole::Function,
        ] {
            let rendered = role.as_str();
            let parsed: RequestRole = rendered.parse().unwrap();
            assert_eq!(parsed, role);
        }
    }
}
