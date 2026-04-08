use std::path::{Path, PathBuf};

use crate::find_clawcr_home;

/// The fixed directory name used for user-level and project-level config folders.
pub const APP_CONFIG_DIR_NAME: &str = ".clawcr";

/// The fixed TOML filename used for application config.
pub const APP_CONFIG_FILE_NAME: &str = "config.toml";

/// Stores the resolved config paths visible from one workspace context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    /// The canonical user-level config file path.
    pub user_config_file: PathBuf,
    /// The canonical user-level config directory path.
    pub user_config_dir: PathBuf,
    /// The canonical project-level config file path, when a workspace is known.
    pub project_config_file: Option<PathBuf>,
    /// The canonical project-level config directory path, when a workspace is known.
    pub project_config_dir: Option<PathBuf>,
}

/// Enumerates failures that can occur while resolving config paths.
#[derive(Debug, thiserror::Error)]
pub enum ConfigPathError {
    /// The current process environment did not expose a usable home directory.
    #[error("home directory is unavailable")]
    HomeDirectoryUnavailable,
}

/// Resolves the user-level and optional project-level app-config paths.
pub trait ConfigPathResolver {
    /// Resolves config paths for an optional workspace root.
    fn resolve_paths(&self, workspace_root: Option<&Path>) -> Result<ConfigPaths, ConfigPathError>;
}

/// Resolves the current process config paths for an optional workspace root.
pub fn current_config_paths(workspace_root: Option<&Path>) -> Result<ConfigPaths, ConfigPathError> {
    FileSystemConfigPathResolver::from_env()?.resolve_paths(workspace_root)
}

/// Resolves the current process user-level config file path.
pub fn current_user_config_file() -> Result<PathBuf, ConfigPathError> {
    Ok(FileSystemConfigPathResolver::from_env()?.user_config_file())
}

/// Filesystem-backed config-path resolver for the local host process.
#[derive(Debug, Clone)]
pub struct FileSystemConfigPathResolver {
    /// The resolved user-level config directory (for example `~/.clawcr`).
    user_config_dir: PathBuf,
}

impl FileSystemConfigPathResolver {
    /// Creates a config-path resolver rooted at one explicit user config directory.
    pub fn new(user_config_dir: PathBuf) -> Self {
        Self { user_config_dir }
    }

    pub fn from_env() -> Result<Self, ConfigPathError> {
        let user_config_dir =
            find_clawcr_home().map_err(|_| ConfigPathError::HomeDirectoryUnavailable)?;
        Ok(Self::new(user_config_dir))
    }

    pub fn user_config_dir(&self) -> PathBuf {
        self.user_config_dir.clone()
    }

    pub fn user_config_file(&self) -> PathBuf {
        self.user_config_dir().join(APP_CONFIG_FILE_NAME)
    }

    /// Returns the canonical project-level config directory for one workspace root.
    pub fn project_config_dir(&self, workspace_root: &Path) -> PathBuf {
        workspace_root.join(APP_CONFIG_DIR_NAME)
    }

    /// Returns the canonical project-level config file for one workspace root.
    pub fn project_config_file(&self, workspace_root: &Path) -> PathBuf {
        self.project_config_dir(workspace_root)
            .join(APP_CONFIG_FILE_NAME)
    }
}

impl ConfigPathResolver for FileSystemConfigPathResolver {
    fn resolve_paths(&self, workspace_root: Option<&Path>) -> Result<ConfigPaths, ConfigPathError> {
        Ok(ConfigPaths {
            user_config_file: self.user_config_file(),
            user_config_dir: self.user_config_dir(),
            project_config_file: workspace_root.map(|root| self.project_config_file(root)),
            project_config_dir: workspace_root.map(|root| self.project_config_dir(root)),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{current_config_paths, ConfigPathResolver, FileSystemConfigPathResolver};

    #[test]
    fn resolver_builds_user_and_project_paths() {
        let resolver = FileSystemConfigPathResolver::new(PathBuf::from("/home/tester"));
        let paths = resolver
            .resolve_paths(Some(PathBuf::from("/repo").as_path()))
            .expect("paths");

        assert_eq!(paths.user_config_dir, PathBuf::from("/home/tester"));
        assert_eq!(
            paths.user_config_file,
            PathBuf::from("/home/tester/config.toml")
        );
        assert_eq!(
            paths.project_config_dir,
            Some(PathBuf::from("/repo/.clawcr"))
        );
        assert_eq!(
            paths.project_config_file,
            Some(PathBuf::from("/repo/.clawcr/config.toml"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolver_supports_user_only_paths_windows() {
        let resolver =
            FileSystemConfigPathResolver::new(PathBuf::from("C:\\Users\\tester\\.clawcr"));
        let paths = resolver.resolve_paths(None).expect("paths");

        assert!(paths.project_config_dir.is_none());
        assert!(paths.project_config_file.is_none());
        assert_eq!(
            paths.user_config_file,
            PathBuf::from("C:\\Users\\tester\\.clawcr\\config.toml")
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolver_supports_user_only_paths_unix() {
        let resolver = FileSystemConfigPathResolver::new(PathBuf::from("/home/tester/.clawcr"));
        let paths = resolver.resolve_paths(None).expect("paths");

        assert!(paths.project_config_dir.is_none());
        assert!(paths.project_config_file.is_none());
        assert_eq!(
            paths.user_config_file,
            PathBuf::from("/home/tester/.clawcr/config.toml")
        );
    }
}
