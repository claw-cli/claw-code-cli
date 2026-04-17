//! Builtin model catalog loading and resolution for core.
//!
//! Main focus:
//! - load the bundled preset list from disk-independent embedded assets
//! - convert raw `ModelPreset` values into runtime `Model` values
//! - provide the concrete builtin implementation of the shared `ModelCatalog` trait
//!
//! Design:
//! - catalog loading stays in `clawcr-core` because the embedded assets live here
//! - this module is the bridge between raw preset/config data and runtime model consumers
//! - models are sorted and materialized here so downstream code can work only with resolved `Model`
//!
//! Boundary:
//! - this module should not define the runtime model shape itself; that lives in `clawcr-protocol`
//! - serde compatibility for the raw preset file belongs in `model_preset.rs`
//! - execution logic should depend on `ModelCatalog` and `Model`, not on how this module reads JSON
//!
use crate::{Model, ModelCatalog, ModelError, ModelPreset};

const DEFAULT_BASE_INSTRUCTIONS: &str = include_str!("../default_base_instructions.txt");

/// Filesystem-independent loader for the built-in model catalog bundled with the binary.
#[derive(Debug, Clone, Default)]
pub struct PresetModelCatalog {
    models: Vec<Model>,
}

impl PresetModelCatalog {
    /// Loads the built-in catalog from `crates/core/models.json`.
    pub fn load() -> Result<Self, PresetModelCatalogError> {
        Ok(Self {
            models: load_builtin_models()?,
        })
    }

    /// Creates a catalog from an already-loaded model list.
    pub fn new(models: Vec<Model>) -> Self {
        Self { models }
    }

    /// Returns the loaded models by value.
    pub fn into_inner(self) -> Vec<Model> {
        self.models
    }
}

impl ModelCatalog for PresetModelCatalog {
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

/// Loads the built-in raw model preset list bundled with the crate.
pub fn load_builtin_model_presets() -> Result<Vec<ModelPreset>, PresetModelCatalogError> {
    serde_json::from_str(include_str!("../models.json")).map_err(Into::into)
}

/// Loads the built-in model list bundled with the crate.
pub fn load_builtin_models() -> Result<Vec<Model>, PresetModelCatalogError> {
    let mut presets = load_builtin_model_presets()?;
    presets.sort_by_key(|right| std::cmp::Reverse(right.priority));
    Ok(presets.into_iter().map(Model::from).collect())
}

/// Returns the shared fallback base instructions used when a model has no catalog entry.
pub fn default_base_instructions() -> &'static str {
    DEFAULT_BASE_INSTRUCTIONS
}

/// Errors produced while loading the builtin catalog.
#[derive(Debug, thiserror::Error)]
pub enum PresetModelCatalogError {
    /// Parsing the bundled JSON file failed.
    #[error("failed to parse builtin model catalog: {0}")]
    Parse(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::{
        PresetModelCatalog, default_base_instructions, load_builtin_model_presets,
        load_builtin_models,
    };
    use crate::ModelCatalog;

    #[test]
    fn builtin_model_presets_load_from_bundled_json() {
        let presets = load_builtin_model_presets().expect("load builtin model presets");
        assert!(!presets.is_empty());
        assert_eq!(presets[0].slug, "qwen3-coder-next");
        assert!(!presets[0].base_instructions.is_empty());
    }

    #[test]
    fn builtin_models_load_from_bundled_json() {
        let models = load_builtin_models().expect("load builtin models");
        assert!(!models.is_empty());
        assert_eq!(models[0].slug, "qwen3-coder-next");
        assert!(!models[0].base_instructions.is_empty());
    }

    #[test]
    fn builtin_catalog_resolves_visible_defaults() {
        let catalog = PresetModelCatalog::load().expect("load catalog");
        let model = catalog.resolve_for_turn(None).expect("resolve default");
        assert!(!model.slug.is_empty());
    }

    #[test]
    fn default_base_instructions_are_available() {
        assert!(!default_base_instructions().trim().is_empty());
    }
}
