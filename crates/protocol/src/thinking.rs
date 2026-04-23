//! Thinking and reasoning metadata shared across the catalog, runtime, and UI.
//!
//! This module exists to keep the model schema focused while making the
//! "thinking" design explicit in one place.
//!
//! The motivation is that a user's logical thinking choice is not always
//! transported the same way to every provider or model family:
//!
//! - Some models do not expose thinking at all.
//! - Some models expose thinking as a request parameter such as `thinking`.
//! - Some models expose "thinking" by publishing separate model variants, for
//!   example "deepseek-chat" vs "deepseek-reasoner".
//!
//! Because of that, the runtime should not treat the request `thinking` field
//! as the only representation of thinking mode. Instead, the system uses a
//! two-step design:
//!
//! 1. The user or session stores a logical thinking selection such as
//!    `disabled`, `enabled`, or `medium`.
//! 2. The runtime resolves that logical selection into concrete provider
//!    request fields:
//!    - the final request model slug
//!    - the final optional `thinking` parameter
//!    - the effective reasoning effort
//!    - optional provider-specific extra request JSON
//!
//! This split is represented by two separate concepts:
//!
//! - `ThinkingCapability` describes what choices the UI should present.
//! - `ThinkingImplementation` describes how that choice should be applied to a
//!   request.
//!
//! Keeping those concerns separate lets the UI remain stable while the runtime
//! adapts request construction for very different provider behaviors. Provider
//! adapters then consume already-resolved request fields instead of embedding
//! model-variant logic themselves.
//!
//! `ResolvedThinkingRequest` is the boundary type produced by resolution. It is
//! the normalized transport-ready result of combining:
//!
//! - a logical model preset
//! - a logical thinking selection
//! - model-specific thinking implementation rules
//!
//! That makes model-variant thinking a catalog/runtime concern rather than a
//! provider-transport concern.

use std::str::FromStr;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use strum_macros::Display;
use strum_macros::EnumIter;

/// Describes how a logical thinking selection should be applied to a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingImplementation {
    /// Thinking is not exposed for this model.
    Disabled,
    /// Thinking is sent via the provider request payload for the same model slug.
    RequestParameter,
    /// Thinking selects a different wire-model variant instead of a request parameter.
    ModelVariant(ThinkingVariantConfig),
}

/// Groups the available model variants used to realize thinking selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingVariantConfig {
    pub variants: Vec<ThinkingVariant>,
}

/// Maps one logical thinking selection to a concrete request model and defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingVariant {
    /// Logical thinking selection value, such as `enabled` or `disabled`.
    pub selection_value: String,
    /// Concrete wire-model slug to send to the provider for this selection.
    pub model_slug: String,
    /// Effective reasoning effort implied by this variant, when one exists.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// User-facing label shown for this selection in pickers.
    pub label: String,
    /// User-facing description shown alongside the label.
    pub description: String,
}

/// Fully resolved request settings derived from a logical model plus thinking selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ResolvedThinkingRequest {
    /// Final model slug that should be sent to the provider.
    pub request_model: String,
    /// Final `thinking` request parameter, when the provider expects one.
    pub request_thinking: Option<String>,
    /// Effective reasoning effort chosen after normalizing the selection.
    pub effective_reasoning_effort: Option<ReasoningEffort>,
    /// Provider-specific extra request JSON to merge into the outbound payload.
    pub extra_body: Option<Value>,
}

/// OpenAI models support reasoning effort.
/// See <https://platform.openai.com/docs/guides/reasoning?api-mode=responses#get-started-with-reasoning>
#[derive(
    Debug,
    Serialize,
    Deserialize,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    JsonSchema,
    EnumIter,
    Hash,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(s.to_string()))
            .map_err(|_| format!("invalid reasoning_effort: {s}"))
    }
}

impl ReasoningEffort {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::XHigh => "XHigh",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::None => "Disable extra reasoning effort",
            Self::Minimal => "Use the lightest supported reasoning effort",
            Self::Low => "Fastest, cheapest, least deliberative",
            Self::Medium => "Balanced speed and deliberation",
            Self::High => "More deliberate for harder tasks",
            Self::XHigh => "Most deliberate, highest effort",
        }
    }
}

/// Maps reasoning efforts onto a stable numeric scale for comparison.
fn effort_rank(effort: ReasoningEffort) -> i32 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
    }
}

/// Picks the supported effort closest to the requested one.
pub(crate) fn nearest_effort(
    target: ReasoningEffort,
    supported: &[ReasoningEffort],
) -> ReasoningEffort {
    let target_rank = effort_rank(target);
    supported
        .iter()
        .copied()
        .min_by_key(|candidate| (effort_rank(*candidate) - target_rank).abs())
        .unwrap_or(target)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One selectable reasoning-effort option presented to the UI or protocol client.
pub struct ReasoningEffortPreset {
    pub effort: ReasoningEffort,
    pub description: String,
}

impl ReasoningEffortPreset {
    pub fn new(effort: ReasoningEffort, description: impl Into<String>) -> Self {
        Self {
            effort,
            description: description.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One selectable thinking option presented to the UI or protocol client.
pub struct ThinkingPreset {
    pub label: String,
    pub description: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingCapability {
    /// Model thinking cannot be controlled.
    Unsupported,
    /// Model thinking can be toggled on and off.
    Toggle,
    /// Multiple effort levels can be selected for thinking.
    Levels(Vec<ReasoningEffort>),
}

impl ThinkingCapability {
    pub fn options(&self) -> Vec<ThinkingPreset> {
        match self {
            ThinkingCapability::Unsupported => Vec::new(),
            ThinkingCapability::Toggle => vec![
                ThinkingPreset {
                    label: "Off".to_string(),
                    description: "Disable thinking for this turn".to_string(),
                    value: "disabled".to_string(),
                },
                ThinkingPreset {
                    label: "On".to_string(),
                    description: "Enable the model's thinking mode".to_string(),
                    value: "enabled".to_string(),
                },
            ],
            ThinkingCapability::Levels(levels) => levels
                .iter()
                .copied()
                .map(|effort| ThinkingPreset {
                    label: effort.label().to_string(),
                    description: effort.description().to_string(),
                    value: effort.label().to_lowercase(),
                })
                .collect(),
        }
    }
}
