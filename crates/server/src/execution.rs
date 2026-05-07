use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use devo_core::AgentsMdConfig;
use devo_core::Model;
use devo_core::ModelCatalog;
use devo_core::ResolvedSkill;
use devo_core::SessionConfig;
use devo_core::SessionId;
use devo_core::SessionRecord;
use devo_core::SessionState;
use devo_core::SkillCatalog;
use devo_core::SkillError;
use devo_core::SkillId;
use devo_core::TurnConfig;
use devo_core::TurnId;
use devo_core::default_base_instructions;
use devo_core::normalize_canonical_path;
use devo_protocol::PendingInputItem;
use devo_provider::ModelProviderSDK;
use devo_tools::ToolRegistry;

use crate::InputItem;
use crate::SkillRecord;
use crate::db::Database;
use crate::session::SessionHistoryItem;
use crate::session::SessionMetadata;
use crate::turn::TurnMetadata;

#[derive(Debug, Clone)]
pub(crate) struct PersistedTurnItem {
    pub(crate) turn_id: TurnId,
    pub(crate) item_id: devo_core::ItemId,
    pub(crate) turn_item: devo_core::TurnItem,
}

/// Shared server-owned runtime dependencies used by live turn execution.
pub struct ServerRuntimeDependencies {
    /// Provider used for all model requests.
    pub(crate) provider: Arc<dyn ModelProviderSDK>,
    /// Shared built-in tool registry used by turn execution.
    pub(crate) registry: Arc<ToolRegistry>,
    /// Default model applied when no model override is present.
    pub(crate) default_model: String,
    /// Model catalog used to resolve builtin prompt metadata.
    pub(crate) model_catalog: Arc<dyn ModelCatalog>,
    /// Default workspace root used for workspace-scoped skill discovery.
    pub(crate) skill_workspace_root: Option<PathBuf>,
    /// Skill catalog for discovering and loading skills.
    pub(crate) skill_catalog: StdMutex<Box<dyn SkillCatalog + Send>>,
    /// AGENTS.md discovery configuration applied to new sessions.
    pub(crate) agents_md: AgentsMdConfig,
    /// SQLite database for session metadata, token stats, and pending queues.
    pub(crate) db: Arc<Database>,
    /// Path to user config.toml used for listing configured models.
    pub(crate) config_file: PathBuf,
}

impl ServerRuntimeDependencies {
    /// Creates a new bundle of runtime dependencies for the transport server.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: Arc<dyn ModelProviderSDK>,
        registry: Arc<ToolRegistry>,
        default_model: String,
        model_catalog: Arc<dyn ModelCatalog>,
        skill_workspace_root: Option<PathBuf>,
        skill_catalog: Box<dyn SkillCatalog + Send>,
        agents_md: AgentsMdConfig,
        db: Arc<Database>,
        config_file: PathBuf,
    ) -> Self {
        Self {
            provider,
            registry,
            default_model,
            model_catalog,
            skill_workspace_root,
            skill_catalog: StdMutex::new(skill_catalog),
            agents_md,
            db,
            config_file,
        }
    }

    /// Creates an initial core session state for a newly created server session.
    pub(crate) fn new_session_state(&self, session_id: SessionId, cwd: PathBuf) -> SessionState {
        let mut state = SessionState::new(
            SessionConfig {
                agents_md: self.agents_md.clone(),
                ..SessionConfig::default()
            },
            cwd,
        );
        state.id = session_id.to_string();
        state
    }

    /// Resolves one runtime model for a turn, applying the server default when needed.
    pub(crate) fn resolve_turn_model(&self, requested_model: Option<&str>) -> Model {
        if let Some(model) = requested_model.and_then(|requested| self.model_catalog.get(requested))
        {
            return model.clone();
        }

        self.model_catalog
            .resolve_for_turn(Some(&self.default_model))
            .or_else(|_| self.model_catalog.resolve_for_turn(None))
            .cloned()
            .unwrap_or_else(|_| Model {
                slug: self.default_model.clone(),
                base_instructions: default_base_instructions().to_string(),
                ..Model::default()
            })
    }

    /// Resolves the full turn configuration used by the core query loop.
    pub(crate) fn resolve_turn_config(
        &self,
        requested_model: Option<&str>,
        thinking_selection: Option<String>,
    ) -> TurnConfig {
        TurnConfig {
            model: self.resolve_turn_model(requested_model),
            thinking_selection,
        }
    }

    /// Returns the current skill catalog snapshot for one optional workspace root.
    pub(crate) fn discover_skills(
        &self,
        workspace_root: Option<&Path>,
    ) -> Result<Vec<SkillRecord>, SkillError> {
        let workspace_root = workspace_root.or(self.skill_workspace_root.as_deref());
        let mut skill_catalog = self
            .skill_catalog
            .lock()
            .expect("skill catalog mutex should not be poisoned");
        skill_catalog.discover(workspace_root).map(|skills| {
            skills
                .into_iter()
                .map(|record| SkillRecord {
                    id: record.id.0.to_string(),
                    name: record.name,
                    description: record.description,
                    path: normalize_canonical_path(record.path),
                    enabled: record.enabled,
                    source: serde_json::from_value(
                        serde_json::to_value(record.source)
                            .expect("core skill source should serialize"),
                    )
                    .expect("protocol skill source should deserialize"),
                })
                .collect()
        })
    }

    /// Renders turn input items and resolves any referenced skills into prompt-visible text.
    pub(crate) fn resolve_input_items(
        &self,
        input: &[InputItem],
        workspace_root: Option<&Path>,
    ) -> Result<Option<String>, SkillError> {
        let workspace_root = workspace_root.or(self.skill_workspace_root.as_deref());
        let mut skill_catalog = self
            .skill_catalog
            .lock()
            .expect("skill catalog mutex should not be poisoned");
        if input
            .iter()
            .any(|item| matches!(item, InputItem::Skill { .. }))
        {
            skill_catalog.discover(workspace_root)?;
        }

        let parts = input
            .iter()
            .map(|item| match item {
                InputItem::Text { text } => Ok(text.trim().to_string()),
                InputItem::Skill { id } => skill_catalog
                    .load(&SkillId(id.clone().into()))
                    .map(|skill| render_resolved_skill(&skill)),
                InputItem::LocalImage { path } => Ok(format!("[image:{}]", path.display())),
                InputItem::Mention { path, name } => Ok(format!(
                    "[mention:{}]",
                    name.as_deref().unwrap_or(path.as_str())
                )),
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        Ok((!parts.is_empty()).then(|| parts.join("\n")))
    }
}

fn render_resolved_skill(skill: &ResolvedSkill) -> String {
    let base_dir = normalize_canonical_path(
        skill
            .record
            .path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf(),
    );
    format!(
        "<skill id=\"{}\" name=\"{}\">\n{}\n\nBase directory: {}\n</skill>",
        skill.record.id.0,
        skill.record.name,
        skill.content.trim_end(),
        base_dir.display()
    )
}

/// Mutable per-session runtime state owned by the server.
pub(crate) struct RuntimeSession {
    /// Canonical persisted session metadata when the session is durable.
    pub(crate) record: Option<SessionRecord>,
    /// Transport-facing metadata exposed over the API.
    pub(crate) summary: SessionMetadata,
    /// Canonical core session state used by the query loop.
    pub(crate) core_session: Arc<Mutex<SessionState>>,
    /// Currently active turn, if any.
    pub(crate) active_turn: Option<TurnMetadata>,
    /// Latest terminal turn metadata for the session.
    pub(crate) latest_turn: Option<TurnMetadata>,
    /// Number of items loaded or appended for the session.
    pub(crate) loaded_item_count: u64,
    /// Replay-friendly ordered history used by interactive clients during session resume.
    pub(crate) history_items: Vec<SessionHistoryItem>,
    /// Canonical persisted turn items in prompt order for replay/compaction bookkeeping.
    pub(crate) persisted_turn_items: Vec<PersistedTurnItem>,
    /// Latest compaction snapshot used to rebuild the model-facing prompt view.
    pub(crate) latest_compaction_snapshot: Option<devo_core::CompactionSnapshotLine>,
    /// Pending turn inputs (from turn/start while a turn is active).
    /// Preserved across turns; unconsumed items are pushed back when the turn ends.
    pub(crate) pending_turn_queue: Arc<StdMutex<VecDeque<PendingInputItem>>>,
    /// /btw steer inputs scoped to the current turn.
    /// Drained by the query loop at each iteration and never carried to the next turn.
    pub(crate) btw_input_queue: Arc<StdMutex<VecDeque<PendingInputItem>>>,
    /// Live query task for the active turn.
    pub(crate) active_task: Option<JoinHandle<()>>,
    /// Deferred completion info for in-progress assistant text item.
    /// Cleared when the item is completed; used for crash/interrupt recovery.
    pub(crate) deferred_assistant: Option<(devo_core::ItemId, u64, String)>,
    /// Deferred completion info for in-progress reasoning text item.
    pub(crate) deferred_reasoning: Option<(devo_core::ItemId, u64, String)>,
    /// Monotonic session-scoped item sequence counter.
    pub(crate) next_item_seq: u64,
    /// First user input captured from the session's first turn, used for title generation.
    pub(crate) first_user_input: Option<String>,
}

impl RuntimeSession {
    /// Wraps a new runtime session in an async mutex for storage in the session map.
    pub(crate) fn shared(self) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(self))
    }
}
