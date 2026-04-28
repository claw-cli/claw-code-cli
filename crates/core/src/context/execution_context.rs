use std::env;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use devo_protocol::{Message, Model, ReasoningEffort, UserInput};

use crate::context::AgentsMdDiff;
use crate::context::AgentsMdManager;
use crate::context::AgentsMdSnapshot;
use crate::context::ContextualUserFragment;
use crate::context::user_instructions::UserInstructions;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Persona {
    #[default]
    Default,
}

impl Persona {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentContext {
    pub cwd: PathBuf,
    pub shell: String,
    pub current_date: String,
    pub timezone: String,
}

impl EnvironmentContext {
    pub fn capture(cwd: &Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            shell: shell_basename(),
            current_date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            timezone: iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string()),
        }
    }

    pub fn render(&self) -> String {
        format!(
            "<environment_context>\n  <cwd>{}</cwd>\n  <shell>{}</shell>\n  <current_date>{}</current_date>\n  <timezone>{}</timezone>\n</environment_context>",
            self.cwd.display(),
            self.shell,
            self.current_date,
            self.timezone,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionContext {
    pub base_instructions: String,
    pub workspace_instructions: Option<String>,
    pub locked_agents_snapshot: Option<AgentsMdSnapshot>,
    pub environment: EnvironmentContext,
    pub persona: Persona,
    pub model: Model,
    pub thinking_selection: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl SessionContext {
    pub fn capture(
        model: &Model,
        thinking_selection: Option<&str>,
        cwd: &Path,
        locked_agents_snapshot: Option<AgentsMdSnapshot>,
    ) -> Self {
        let normalized_thinking_selection = normalize_thinking_selection(thinking_selection);
        let resolved = model.resolve_thinking_selection(normalized_thinking_selection.as_deref());
        let workspace_instructions = locked_agents_snapshot
            .as_ref()
            .map(|snapshot| snapshot.rendered_instructions.clone());
        Self {
            base_instructions: model.base_instructions.clone(),
            workspace_instructions,
            locked_agents_snapshot,
            environment: EnvironmentContext::capture(cwd),
            persona: Persona::Default,
            model: model.clone(),
            thinking_selection: normalized_thinking_selection,
            reasoning_effort: resolved.effective_reasoning_effort,
        }
    }

    pub fn build_system_prompt(&self) -> String {
        self.base_instructions.trim().to_string()
    }

    pub fn prefix_user_inputs(&self) -> Vec<UserInput> {
        let mut inputs = Vec::new();
        if let Some(text) = self
            .workspace_instructions
            .as_ref()
            .filter(|text| !text.trim().is_empty())
        {
            inputs.push(UserInput::Text {
                text: UserInstructions {
                    directory: self.environment.cwd.display().to_string(),
                    text: text.clone(),
                }
                .render(),
                text_elements: Vec::new(),
            });
        }
        inputs.push(UserInput::Text {
            text: self.environment.render(),
            text_elements: Vec::new(),
        });
        inputs
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnContext {
    pub environment: EnvironmentContext,
    pub persona: Persona,
    pub model: Model,
    pub thinking_selection: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub observed_agents_snapshot: Option<AgentsMdSnapshot>,
}

impl TurnContext {
    pub fn capture(
        model: &Model,
        thinking_selection: Option<&str>,
        cwd: &Path,
        observed_agents_snapshot: Option<AgentsMdSnapshot>,
    ) -> Self {
        let normalized_thinking_selection = normalize_thinking_selection(thinking_selection);
        let resolved = model.resolve_thinking_selection(normalized_thinking_selection.as_deref());
        Self {
            environment: EnvironmentContext::capture(cwd),
            persona: Persona::Default,
            model: model.clone(),
            thinking_selection: normalized_thinking_selection,
            reasoning_effort: resolved.effective_reasoning_effort,
            observed_agents_snapshot,
        }
    }

    pub fn diff_since(&self, previous: &TurnContext) -> Option<ContextDiffFragment> {
        let mut changes = Vec::new();

        if self.environment.cwd != previous.environment.cwd {
            changes.push(format!(
                "cwd: {} -> {}",
                previous.environment.cwd.display(),
                self.environment.cwd.display()
            ));
        }
        if self.environment.shell != previous.environment.shell {
            changes.push(format!(
                "shell: {} -> {}",
                previous.environment.shell, self.environment.shell
            ));
        }
        if self.environment.current_date != previous.environment.current_date {
            changes.push(format!(
                "current_date: {} -> {}",
                previous.environment.current_date, self.environment.current_date
            ));
        }
        if self.environment.timezone != previous.environment.timezone {
            changes.push(format!(
                "timezone: {} -> {}",
                previous.environment.timezone, self.environment.timezone
            ));
        }
        if self.persona != previous.persona {
            changes.push(format!(
                "persona: {} -> {}",
                previous.persona.as_str(),
                self.persona.as_str()
            ));
        }
        if self.model.slug != previous.model.slug {
            changes.push(format!(
                "model: {} -> {}",
                previous.model.slug, self.model.slug
            ));
        }
        if self.thinking_selection != previous.thinking_selection {
            changes.push(format!(
                "thinking_selection: {:?} -> {:?}",
                previous.thinking_selection, self.thinking_selection
            ));
        }
        if self.reasoning_effort != previous.reasoning_effort {
            changes.push(format!(
                "reasoning_effort: {:?} -> {:?}",
                previous.reasoning_effort, self.reasoning_effort
            ));
        }

        if changes.is_empty() {
            return None;
        }

        Some(ContextDiffFragment { changes })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDiffFragment {
    changes: Vec<String>,
}

impl ContextDiffFragment {
    pub fn to_message(&self) -> Message {
        Message::user(self.render())
    }
}

impl ContextualUserFragment for ContextDiffFragment {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = "<context_changes>";
    const END_MARKER: &'static str = "</context_changes>";

    fn body(&self) -> String {
        format!("\n{}\n", self.changes.join("\n"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsMdDiffFragment {
    diff: AgentsMdDiff,
}

impl AgentsMdDiffFragment {
    pub fn new(diff: AgentsMdDiff) -> Self {
        Self { diff }
    }

    pub fn to_message(&self) -> Message {
        Message::user(self.render())
    }
}

impl ContextualUserFragment for AgentsMdDiffFragment {
    const ROLE: &'static str = "user";
    const START_MARKER: &'static str = "<agents_md_updates>";
    const END_MARKER: &'static str = "</agents_md_updates>";

    fn body(&self) -> String {
        let mut lines = Vec::new();
        for path in &self.diff.added {
            lines.push(format!("added: {}", path.display()));
        }
        for path in &self.diff.removed {
            lines.push(format!("removed: {}", path.display()));
        }
        for path in &self.diff.changed {
            lines.push(format!("changed: {}", path.display()));
        }
        format!("\n{}\n", lines.join("\n"))
    }
}

pub fn load_workspace_instructions(
    cwd: &Path,
    manager: &AgentsMdManager,
) -> Option<AgentsMdSnapshot> {
    manager.load(cwd)
}

fn normalize_thinking_selection(thinking_selection: Option<&str>) -> Option<String> {
    thinking_selection
        .map(str::trim)
        .filter(|selection| !selection.is_empty())
        .map(ToOwned::to_owned)
}

fn default_shell_name() -> String {
    #[cfg(target_os = "windows")]
    {
        return default_shell_windows();
    }

    #[cfg(target_os = "android")]
    {
        return default_shell_android();
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        return default_shell_unix();
    }

    #[allow(unreachable_code)]
    "sh".to_string()
}

#[cfg(target_os = "windows")]
fn default_shell_windows() -> String {
    if let Some(shell) = env::var_os("COMSPEC")
        && !shell.is_empty()
    {
        return shell.to_string_lossy().into_owned();
    }

    "cmd.exe".to_string()
}

#[cfg(target_os = "android")]
fn default_shell_android() -> String {
    if let Some(shell) = env::var_os("SHELL")
        && !shell.is_empty()
    {
        return shell.to_string_lossy().into_owned();
    }

    "/system/bin/sh".to_string()
}

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn default_shell_unix() -> String {
    if let Some(shell) = env::var_os("SHELL")
        && !shell.is_empty()
    {
        return shell.to_string_lossy().into_owned();
    }

    "/bin/sh".to_string()
}

fn shell_basename() -> String {
    let shell = default_shell_name();

    Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or(shell.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use devo_protocol::UserInput;
    use pretty_assertions::assert_eq;

    use super::{ContextDiffFragment, EnvironmentContext, SessionContext, TurnContext};
    use crate::{
        AgentsMdSnapshot, ContextualUserFragment, Model, ReasoningEffort, ThinkingCapability,
    };

    #[test]
    fn session_context_prefix_contains_locked_environment() {
        let context = SessionContext::capture(
            &Model {
                base_instructions: "base".into(),
                ..Model::default()
            },
            Some("enabled"),
            Path::new("/tmp/project"),
            Some(AgentsMdSnapshot {
                cwd: PathBuf::from("/tmp/project"),
                project_root: PathBuf::from("/tmp"),
                documents: Vec::new(),
                rendered_instructions: "workspace".into(),
            }),
        );

        let prefix = context.prefix_user_inputs();
        assert_eq!(prefix.len(), 2);
        let rendered = match &prefix[1] {
            UserInput::Text { text, .. } => text.clone(),
            _ => String::new(),
        };
        assert!(rendered.contains("<environment_context>"));
        assert!(rendered.contains("/tmp/project"));
    }

    #[test]
    fn turn_context_diff_reports_model_and_reasoning_changes() {
        let previous = TurnContext {
            environment: EnvironmentContext {
                cwd: PathBuf::from("/tmp/a"),
                shell: "bash".into(),
                current_date: "2026-04-27".into(),
                timezone: "UTC".into(),
            },
            persona: super::Persona::Default,
            model: Model {
                slug: "a".into(),
                ..Model::default()
            },
            thinking_selection: Some("enabled".into()),
            reasoning_effort: Some(ReasoningEffort::Medium),
            observed_agents_snapshot: None,
        };
        let current = TurnContext {
            environment: EnvironmentContext {
                cwd: PathBuf::from("/tmp/b"),
                shell: "bash".into(),
                current_date: "2026-04-28".into(),
                timezone: "UTC".into(),
            },
            persona: super::Persona::Default,
            model: Model {
                slug: "b".into(),
                thinking_capability: ThinkingCapability::Toggle,
                ..Model::default()
            },
            thinking_selection: Some("disabled".into()),
            reasoning_effort: None,
            observed_agents_snapshot: None,
        };

        let diff = current.diff_since(&previous).expect("diff should exist");
        let rendered = diff.render();
        assert!(rendered.contains("model: a -> b"));
        assert!(rendered.contains("thinking_selection"));
        assert!(rendered.contains("reasoning_effort"));
        assert!(rendered.contains("/tmp/a -> /tmp/b"));
    }

    #[test]
    fn context_diff_fragment_roundtrips_to_message() {
        let fragment = ContextDiffFragment {
            changes: vec!["model: a -> b".into()],
        };

        let message = fragment.to_message();
        assert_eq!(message.role, devo_protocol::Role::User);
        assert_eq!(message.content.len(), 1);
    }
}
