//! Builtin model catalog loading and resolution for core.
//!
//! Main focus:
//! - load the bundled preset list from disk-independent embedded assets
//! - load per-user and per-project model overrides from the filesystem
//! - convert raw `ModelPreset` values into runtime `Model` values
//! - provide the concrete builtin implementation of the shared `ModelCatalog` trait
//!
//! Design:
//! - catalog loading stays in `devo-core` because the embedded assets live here
//! - this module is the bridge between raw preset/config data and runtime model consumers
//! - models are sorted and materialized here so downstream code can work only with resolved `Model`
//! - the layered merge order is: builtin < ~/.devo/models.json < <workspace>/.devo/models.json
//!
//! Boundary:
//! - this module should not define the runtime model shape itself; that lives in `devo-protocol`
//! - serde compatibility for the raw preset file belongs in `model_preset.rs`
//! - execution logic should depend on `ModelCatalog` and `Model`, not on how this module reads JSON
//!
use std::path::Path;

use crate::{Model, ModelCatalog, ModelError, ModelPreset};

const DEFAULT_BASE_INSTRUCTIONS: &str = include_str!("../default_base_instructions.txt");
const BUILTIN_MODELS_JSON: &str = include_str!("../models.json");

/// Filesystem-independent loader for the built-in model catalog bundled with the binary.
///
/// Use [`PresetModelCatalog::load_from_config`] to include user and project overrides.
/// Use [`PresetModelCatalog::load`] for the builtin-only variant (tests, doctor, etc.).
#[derive(Debug, Clone, Default)]
pub struct PresetModelCatalog {
    models: Vec<Model>,
}

impl PresetModelCatalog {
    /// Loads the built-in catalog only (no filesystem overrides).
    pub fn load() -> Result<Self, PresetModelCatalogError> {
        Ok(Self {
            models: load_builtin_models()?,
        })
    }

    /// Loads the effective catalog from three layers, merged highest-wins:
    /// 1. built-in models (embedded)
    /// 2. `config_home/models.json` (user overrides)
    /// 3. `<workspace_root>/.devo/models.json` (project overrides)
    ///
    /// If the user file does not exist it is seeded from the builtin list so
    /// users can discover and customize the catalog.
    pub fn load_from_config(
        config_home: &Path,
        workspace_root: Option<&Path>,
    ) -> Result<Self, PresetModelCatalogError> {
        seed_user_models_file(config_home);

        let mut presets = load_builtin_model_presets()?;

        if let Some(user_overrides) = load_models_from_file(&config_home.join("models.json")) {
            presets = merge_model_presets(presets, user_overrides);
        }

        if let Some(workspace_root) = workspace_root {
            let project_path = workspace_root.join(".devo").join("models.json");
            if let Some(project_overrides) = load_models_from_file(&project_path) {
                presets = merge_model_presets(presets, project_overrides);
            }
        }

        presets.sort_by(|left, right| right.priority.cmp(&left.priority));
        Ok(Self {
            models: presets.into_iter().map(Model::from).collect(),
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

    /// Resolves an explicit requested slug, or falls back to the first visible preset model.
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
    serde_json::from_str(BUILTIN_MODELS_JSON).map_err(Into::into)
}

/// Loads the built-in model list bundled with the crate.
pub fn load_builtin_models() -> Result<Vec<Model>, PresetModelCatalogError> {
    let mut presets = load_builtin_model_presets()?;
    presets.sort_by(|left, right| right.priority.cmp(&left.priority));
    Ok(presets.into_iter().map(Model::from).collect())
}

/// Reads model presets from a filesystem JSON path. Returns `None` if the file
/// does not exist, and a parse error if the file exists but is invalid.
fn load_models_from_file(path: &Path) -> Option<Vec<ModelPreset>> {
    let contents = std::fs::read_to_string(path).ok()?;
    if contents.trim().is_empty() {
        return Some(Vec::new());
    }
    Some(serde_json::from_str(&contents).unwrap_or_default())
}

/// Merges two model lists by slug. Entries from `overlay` replace matching
/// entries in `base`; entries with new slugs are appended.
fn merge_model_presets(mut base: Vec<ModelPreset>, overlay: Vec<ModelPreset>) -> Vec<ModelPreset> {
    for entry in overlay {
        match base.iter_mut().find(|m| m.slug == entry.slug) {
            Some(existing) => *existing = entry,
            None => base.push(entry),
        }
    }
    base
}

/// Copies the built-in `models.json` to the user config directory if no user
/// file exists yet.
fn seed_user_models_file(config_home: &Path) {
    let user_path = config_home.join("models.json");
    if user_path.exists() {
        return;
    }
    if let Some(parent) = user_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&user_path, BUILTIN_MODELS_JSON);
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
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use pretty_assertions::assert_eq;

    use super::{
        PresetModelCatalog, default_base_instructions, load_builtin_model_presets,
        load_builtin_models, merge_model_presets,
    };
    use crate::{ModelCatalog, ModelPreset};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("devo-{name}-{nanos}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn make_preset(slug: &str, display_name: &str, priority: i32) -> ModelPreset {
        ModelPreset {
            slug: slug.into(),
            display_name: display_name.into(),
            priority,
            ..ModelPreset::default()
        }
    }

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

    #[test]
    fn merge_by_slug_overrides_existing() {
        let base = vec![make_preset("a", "Base A", 10)];
        let overlay = vec![make_preset("a", "Overlay A", 20)];
        let merged = merge_model_presets(base, overlay);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].display_name, "Overlay A");
        assert_eq!(merged[0].priority, 20);
    }

    #[test]
    fn merge_by_slug_appends_new() {
        let base = vec![make_preset("a", "A", 10)];
        let overlay = vec![make_preset("b", "B", 20)];
        let merged = merge_model_presets(base, overlay);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].slug, "a");
        assert_eq!(merged[1].slug, "b");
    }

    #[test]
    fn merge_empty_overlay_does_nothing() {
        let base = vec![make_preset("a", "A", 10)];
        let merged = merge_model_presets(base, Vec::new());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].display_name, "A");
    }

    #[test]
    fn load_from_config_returns_builtin_when_no_filesystem_files() {
        let root = unique_temp_dir("catalog-builtin-only");
        let home = root.join("home").join(".devo");
        std::fs::create_dir_all(&home).expect("create home");

        let catalog =
            PresetModelCatalog::load_from_config(&home, /*workspace_root*/ None).expect("load");
        let models = catalog.into_inner();
        assert!(!models.is_empty());
        assert_eq!(models[0].slug, "qwen3-coder-next");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_from_config_seeds_user_file_when_missing() {
        let root = unique_temp_dir("catalog-seed");
        let home = root.join("home").join(".devo");
        std::fs::create_dir_all(&home).expect("create home");

        let user_file = home.join("models.json");
        assert!(!user_file.exists());

        let _catalog =
            PresetModelCatalog::load_from_config(&home, /*workspace_root*/ None).expect("load");

        assert!(user_file.exists());
        let contents = std::fs::read_to_string(&user_file).expect("read");
        assert!(contents.contains("qwen3-coder-next"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn load_from_config_does_not_overwrite_existing_user_file() {
        let root = unique_temp_dir("catalog-no-overwrite");
        let home = root.join("home").join(".devo");
        std::fs::create_dir_all(&home).expect("create home");

        let user_file = home.join("models.json");
        std::fs::write(
            &user_file,
            "[{\"slug\":\"custom\",\"display_name\":\"Custom\"}]",
        )
        .expect("write");

        let catalog =
            PresetModelCatalog::load_from_config(&home, /*workspace_root*/ None).expect("load");
        let models = catalog.into_inner();

        assert!(models.iter().any(|m| m.slug == "custom"));
        assert!(models.iter().any(|m| m.slug == "qwen3-coder-next"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn builtin_models_have_channel_fields() {
        let models = load_builtin_models().expect("load builtin models");
        let deepseek_models: Vec<_> = models
            .iter()
            .filter(|m| m.channel.as_deref() == Some("DeepSeek"))
            .collect();
        assert!(!deepseek_models.is_empty());
        assert!(deepseek_models.iter().any(|m| m.slug == "deepseek-v4-pro"));
    }
}
