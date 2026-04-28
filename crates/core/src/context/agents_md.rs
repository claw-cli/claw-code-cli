use std::fs;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

const DEFAULT_PROJECT_DOC_MAX_BYTES: usize = 32 * 1024;
const HIERARCHICAL_AGENTS_MESSAGE: &str =
    include_str!("../../prompts/hierarchical_agents_message.md");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsMdConfig {
    pub project_root_markers: Vec<String>,
    pub equivalent_project_doc_filenames: Vec<String>,
    pub project_doc_max_bytes: usize,
    pub include_hierarchical_message: bool,
}

impl Default for AgentsMdConfig {
    fn default() -> Self {
        Self {
            project_root_markers: vec![".git".into()],
            equivalent_project_doc_filenames: vec![
                "AGENTS.md".into(),
                "CLAUDE.md".into(),
                "PROMPT.md".into(),
            ],
            project_doc_max_bytes: DEFAULT_PROJECT_DOC_MAX_BYTES,
            include_hierarchical_message: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentsDocumentKind {
    Override,
    Equivalent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsDocumentSnapshot {
    pub path: PathBuf,
    pub scope_dir: PathBuf,
    pub kind: AgentsDocumentKind,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsMdSnapshot {
    pub cwd: PathBuf,
    pub project_root: PathBuf,
    pub documents: Vec<AgentsDocumentSnapshot>,
    pub rendered_instructions: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsMdDiff {
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub changed: Vec<PathBuf>,
}

impl AgentsMdDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentsMdManager {
    config: AgentsMdConfig,
}

impl AgentsMdManager {
    pub fn new(config: AgentsMdConfig) -> Self {
        Self { config }
    }

    pub fn load(&self, cwd: &Path) -> Option<AgentsMdSnapshot> {
        let project_root = self.find_project_root(cwd);
        let directories = self.directory_chain(&project_root, cwd);
        let documents = directories
            .into_iter()
            .flat_map(|directory| self.read_directory_documents(&directory))
            .collect::<Vec<_>>();

        if documents.is_empty() {
            return None;
        }

        let rendered_instructions = self.render_documents(&documents);
        Some(AgentsMdSnapshot {
            cwd: cwd.to_path_buf(),
            project_root,
            documents,
            rendered_instructions,
        })
    }

    pub fn diff(
        previous: Option<&AgentsMdSnapshot>,
        current: Option<&AgentsMdSnapshot>,
    ) -> Option<AgentsMdDiff> {
        let previous_documents = previous.map(|snapshot| &snapshot.documents);
        let current_documents = current.map(|snapshot| &snapshot.documents);
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();

        let empty = Vec::new();
        let previous_documents = previous_documents.unwrap_or(&empty);
        let current_documents = current_documents.unwrap_or(&empty);

        for current_document in current_documents {
            match previous_documents
                .iter()
                .find(|previous_document| previous_document.path == current_document.path)
            {
                None => added.push(current_document.path.clone()),
                Some(previous_document) if previous_document != current_document => {
                    changed.push(current_document.path.clone());
                }
                Some(_) => {}
            }
        }

        for previous_document in previous_documents {
            if current_documents
                .iter()
                .all(|current_document| current_document.path != previous_document.path)
            {
                removed.push(previous_document.path.clone());
            }
        }

        let diff = AgentsMdDiff {
            added,
            removed,
            changed,
        };
        (!diff.is_empty()).then_some(diff)
    }

    fn render_documents(&self, documents: &[AgentsDocumentSnapshot]) -> String {
        let mut rendered = documents
            .iter()
            .map(|document| document.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        truncate_to_bytes(&mut rendered, self.config.project_doc_max_bytes);
        if self.config.include_hierarchical_message {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(HIERARCHICAL_AGENTS_MESSAGE.trim());
        }
        rendered
    }

    fn read_directory_documents(&self, directory: &Path) -> Vec<AgentsDocumentSnapshot> {
        let mut documents = Vec::new();

        for (file_name, kind) in
            std::iter::once(("AGENTS.override.md", AgentsDocumentKind::Override)).chain(
                self.config
                    .equivalent_project_doc_filenames
                    .iter()
                    .map(|name| (name.as_str(), AgentsDocumentKind::Equivalent)),
            )
        {
            let path = directory.join(file_name);
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if !metadata.is_file() {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let content = content.trim().to_string();
            if content.is_empty() {
                continue;
            }
            documents.push(AgentsDocumentSnapshot {
                path,
                scope_dir: directory.to_path_buf(),
                kind,
                content,
            });
        }

        documents
    }

    fn find_project_root(&self, cwd: &Path) -> PathBuf {
        if self.config.project_root_markers.is_empty() {
            return cwd.to_path_buf();
        }

        let mut current = Some(cwd);
        let mut discovered_root = None;
        while let Some(directory) = current {
            if self
                .config
                .project_root_markers
                .iter()
                .any(|marker| directory.join(marker).exists())
            {
                discovered_root = Some(directory.to_path_buf());
            }
            current = directory.parent();
        }

        discovered_root.unwrap_or_else(|| cwd.to_path_buf())
    }

    fn directory_chain(&self, project_root: &Path, cwd: &Path) -> Vec<PathBuf> {
        if project_root == cwd {
            return vec![cwd.to_path_buf()];
        }

        let mut directories = Vec::new();
        let mut current = cwd;
        loop {
            directories.push(current.to_path_buf());
            if current == project_root {
                break;
            }
            let Some(parent) = current.parent() else {
                return vec![cwd.to_path_buf()];
            };
            current = parent;
        }
        directories.reverse();
        directories
    }
}

fn truncate_to_bytes(text: &mut String, max_bytes: usize) {
    if text.len() <= max_bytes {
        return;
    }

    let mut boundary = max_bytes.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;
    use std::time::UNIX_EPOCH;

    use pretty_assertions::assert_eq;

    use super::AgentsDocumentKind;
    use super::AgentsMdConfig;
    use super::AgentsMdManager;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("devo-agents-{name}-{nanos}"));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn manager_loads_documents_from_root_to_cwd() {
        let root = unique_temp_dir("hierarchy");
        let nested = root.join("a").join("b");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(root.join(".git"), "").expect("write marker");
        std::fs::write(root.join("AGENTS.md"), "root").expect("write root agents");
        std::fs::write(root.join("CLAUDE.md"), "claude").expect("write root claude");
        std::fs::write(root.join("a").join("AGENTS.override.md"), "override")
            .expect("write nested override");

        let snapshot = AgentsMdManager::new(AgentsMdConfig::default())
            .load(&nested)
            .expect("load snapshot");

        assert_eq!(snapshot.documents.len(), 3);
        assert_eq!(snapshot.documents[0].content, "root");
        assert_eq!(snapshot.documents[0].kind, AgentsDocumentKind::Equivalent);
        assert_eq!(snapshot.documents[1].content, "claude");
        assert_eq!(snapshot.documents[1].kind, AgentsDocumentKind::Equivalent);
        assert_eq!(snapshot.documents[2].kind, AgentsDocumentKind::Override);
        assert!(
            snapshot
                .rendered_instructions
                .contains("root\n\nclaude\n\noverride")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manager_uses_cwd_only_when_markers_are_disabled() {
        let root = unique_temp_dir("disabled-markers");
        let nested = root.join("a");
        std::fs::create_dir_all(&nested).expect("create nested");
        std::fs::write(root.join("AGENTS.md"), "root").expect("write root agents");
        std::fs::write(nested.join("AGENTS.md"), "nested").expect("write nested agents");

        let snapshot = AgentsMdManager::new(AgentsMdConfig {
            project_root_markers: Vec::new(),
            ..AgentsMdConfig::default()
        })
        .load(&nested)
        .expect("load snapshot");

        assert_eq!(snapshot.documents.len(), 1);
        assert_eq!(snapshot.documents[0].content, "nested");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manager_diff_reports_added_removed_and_changed_documents() {
        let root = unique_temp_dir("diff");
        std::fs::write(root.join(".git"), "").expect("write marker");
        std::fs::write(root.join("AGENTS.md"), "one").expect("write agents");

        let manager = AgentsMdManager::new(AgentsMdConfig::default());
        let previous = manager.load(&root).expect("load previous");

        std::fs::write(root.join("AGENTS.md"), "two").expect("rewrite agents");
        std::fs::write(root.join("AGENTS.override.md"), "override").expect("write override");

        let current = manager.load(&root).expect("load current");
        let diff = AgentsMdManager::diff(Some(&previous), Some(&current)).expect("compute diff");

        assert_eq!(diff.added, vec![root.join("AGENTS.override.md")]);
        assert!(diff.removed.is_empty());
        assert_eq!(diff.changed, vec![root.join("AGENTS.md")]);

        let _ = std::fs::remove_dir_all(root);
    }
}
