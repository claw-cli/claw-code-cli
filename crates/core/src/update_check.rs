use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use crate::UpdatesConfig;

const RELEASES_LATEST_URL: &str = "https://api.github.com/repos/7df-lab/devo/releases/latest";
const UPDATE_CACHE_FILE_NAME: &str = "update-check.json";
const WINDOWS_UPGRADE_COMMAND: &str = "curl.exe -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1 | powershell -NoProfile -ExecutionPolicy Bypass -Command -";
const UNIX_UPGRADE_COMMAND: &str =
    "curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckOutcome {
    UpToDate,
    UpdateAvailable(UpdateNotification),
    Skipped(UpdateCheckSkipReason),
    Failed(UpdateCheckFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateNotification {
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
    pub published_at: Option<DateTime<Utc>>,
    pub upgrade_command: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckSkipReason {
    Disabled,
    StartupCheckDisabled,
    CacheFresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheckFailure {
    pub stage: UpdateCheckStage,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckStage {
    CacheRead,
    HttpRequest,
    HttpStatus,
    ResponseParse,
    CacheWrite,
}

#[derive(Debug, Clone)]
pub struct UpdateChecker {
    home_dir: PathBuf,
    config: UpdatesConfig,
    client: reqwest::Client,
}

impl UpdateChecker {
    pub fn new(home_dir: PathBuf, config: UpdatesConfig) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .user_agent(format!("devo/{}", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            home_dir,
            config,
            client,
        })
    }

    pub async fn check_for_startup_update(&self) -> UpdateCheckOutcome {
        if !self.config.enabled {
            return UpdateCheckOutcome::Skipped(UpdateCheckSkipReason::Disabled);
        }
        if !self.config.check_on_startup {
            return UpdateCheckOutcome::Skipped(UpdateCheckSkipReason::StartupCheckDisabled);
        }

        let current_version = env!("CARGO_PKG_VERSION").to_string();
        let cache_path = self.home_dir.join(UPDATE_CACHE_FILE_NAME);
        let cache = match read_update_cache(&cache_path) {
            Ok(cache) => cache,
            Err(err) => return UpdateCheckOutcome::Failed(err),
        };

        if let Some(cache) = cache
            && !cache.is_expired(self.config.check_interval_hours)
        {
            return cached_outcome(&current_version, cache).unwrap_or(UpdateCheckOutcome::Skipped(
                UpdateCheckSkipReason::CacheFresh,
            ));
        }

        let response = match self.client.get(RELEASES_LATEST_URL).send().await {
            Ok(response) => response,
            Err(err) => {
                return UpdateCheckOutcome::Failed(UpdateCheckFailure {
                    stage: UpdateCheckStage::HttpRequest,
                    message: err.to_string(),
                });
            }
        };

        if !response.status().is_success() {
            return UpdateCheckOutcome::Failed(UpdateCheckFailure {
                stage: UpdateCheckStage::HttpStatus,
                message: format!("unexpected status {}", response.status()),
            });
        }

        let release = match response.json::<GitHubRelease>().await {
            Ok(release) => release,
            Err(err) => {
                return UpdateCheckOutcome::Failed(UpdateCheckFailure {
                    stage: UpdateCheckStage::ResponseParse,
                    message: err.to_string(),
                });
            }
        };

        let cache_entry = UpdateCheckCache {
            last_checked_at: Utc::now(),
            latest_version: release.tag_name,
            release_url: release.html_url,
            published_at: release.published_at,
            last_error: None,
        };

        let outcome = cached_outcome(&current_version, cache_entry.clone())
            .unwrap_or(UpdateCheckOutcome::UpToDate);

        if let Err(err) = write_update_cache(&cache_path, &cache_entry) {
            return UpdateCheckOutcome::Failed(err);
        }

        outcome
    }
}

pub fn format_update_notification(notification: &UpdateNotification) -> String {
    format!(
        "A new devo version is available: {} (current: {})\nRelease: {}\nUpdate: {}",
        notification.latest_version,
        notification.current_version,
        notification.release_url,
        notification.upgrade_command
    )
}

fn cached_outcome(current_version: &str, cache: UpdateCheckCache) -> Option<UpdateCheckOutcome> {
    let latest = normalize_version(&cache.latest_version)?;
    let current = normalize_version(current_version)?;
    if latest > current {
        Some(UpdateCheckOutcome::UpdateAvailable(UpdateNotification {
            current_version: current_version.to_string(),
            latest_version: cache.latest_version,
            release_url: cache.release_url,
            published_at: cache.published_at,
            upgrade_command: upgrade_command_for_current_platform(),
        }))
    } else {
        Some(UpdateCheckOutcome::UpToDate)
    }
}

fn upgrade_command_for_current_platform() -> &'static str {
    if cfg!(windows) {
        WINDOWS_UPGRADE_COMMAND
    } else {
        UNIX_UPGRADE_COMMAND
    }
}

fn normalize_version(version: &str) -> Option<Vec<u64>> {
    let trimmed = version.trim().trim_start_matches('v');
    let mut parts = Vec::new();
    for part in trimmed.split('.') {
        if part.is_empty() {
            return None;
        }
        parts.push(part.parse().ok()?);
    }
    Some(parts)
}

fn read_update_cache(path: &Path) -> Result<Option<UpdateCheckCache>, UpdateCheckFailure> {
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(path).map_err(|err| UpdateCheckFailure {
        stage: UpdateCheckStage::CacheRead,
        message: err.to_string(),
    })?;
    serde_json::from_str(&contents).or(Ok(None))
}

fn write_update_cache(path: &Path, cache: &UpdateCheckCache) -> Result<(), UpdateCheckFailure> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| UpdateCheckFailure {
            stage: UpdateCheckStage::CacheWrite,
            message: err.to_string(),
        })?;
    }
    let contents = serde_json::to_string_pretty(cache).map_err(|err| UpdateCheckFailure {
        stage: UpdateCheckStage::CacheWrite,
        message: err.to_string(),
    })?;
    fs::write(path, contents).map_err(|err| UpdateCheckFailure {
        stage: UpdateCheckStage::CacheWrite,
        message: err.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct UpdateCheckCache {
    last_checked_at: DateTime<Utc>,
    latest_version: String,
    release_url: String,
    published_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

impl UpdateCheckCache {
    fn is_expired(&self, interval_hours: u64) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.last_checked_at);
        elapsed.num_hours() >= interval_hours as i64
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    published_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Duration;
    use pretty_assertions::assert_eq;

    use super::UpdateCheckCache;
    use super::UpdateCheckFailure;
    use super::UpdateCheckOutcome;
    use super::UpdateCheckSkipReason;
    use super::UpdateCheckStage;
    use super::UpdatesConfig;
    use super::cached_outcome;
    use super::format_update_notification;
    use super::normalize_version;
    use super::read_update_cache;

    #[test]
    fn normalize_version_accepts_plain_and_v_prefixed_versions() {
        assert_eq!(normalize_version("0.1.3"), Some(vec![0, 1, 3]));
        assert_eq!(normalize_version("v0.1.4"), Some(vec![0, 1, 4]));
        assert_eq!(normalize_version(" v1.2.0 "), Some(vec![1, 2, 0]));
    }

    #[test]
    fn normalize_version_rejects_malformed_versions() {
        assert_eq!(normalize_version(""), None);
        assert_eq!(normalize_version("vx.y.z"), None);
        assert_eq!(normalize_version("1..3"), None);
    }

    #[test]
    fn cached_outcome_reports_available_update_for_newer_release() {
        let outcome = cached_outcome(
            "0.1.3",
            UpdateCheckCache {
                last_checked_at: chrono::Utc::now(),
                latest_version: "v0.1.4".into(),
                release_url: "https://github.com/7df-lab/devo/releases/tag/v0.1.4".into(),
                published_at: None,
                last_error: None,
            },
        )
        .expect("cached outcome");

        assert!(matches!(outcome, UpdateCheckOutcome::UpdateAvailable(_)));
    }

    #[test]
    fn cached_outcome_reports_up_to_date_for_equal_versions() {
        let outcome = cached_outcome(
            "0.1.3",
            UpdateCheckCache {
                last_checked_at: chrono::Utc::now(),
                latest_version: "v0.1.3".into(),
                release_url: "https://github.com/7df-lab/devo/releases/tag/v0.1.3".into(),
                published_at: None,
                last_error: None,
            },
        )
        .expect("cached outcome");

        assert_eq!(outcome, UpdateCheckOutcome::UpToDate);
    }

    #[test]
    fn cached_outcome_ignores_malformed_remote_tags() {
        assert_eq!(
            cached_outcome(
                "0.1.3",
                UpdateCheckCache {
                    last_checked_at: chrono::Utc::now(),
                    latest_version: "latest".into(),
                    release_url: "https://github.com/7df-lab/devo/releases/latest".into(),
                    published_at: None,
                    last_error: None,
                },
            ),
            None
        );
    }

    #[test]
    fn cache_entry_uses_ttl_to_decide_expiration() {
        let fresh = UpdateCheckCache {
            last_checked_at: chrono::Utc::now() - Duration::hours(23),
            latest_version: "v0.1.4".into(),
            release_url: "https://github.com/7df-lab/devo/releases/tag/v0.1.4".into(),
            published_at: None,
            last_error: None,
        };
        let expired = UpdateCheckCache {
            last_checked_at: chrono::Utc::now() - Duration::hours(24),
            ..fresh.clone()
        };

        assert_eq!(fresh.is_expired(24), false);
        assert_eq!(expired.is_expired(24), true);
    }

    #[test]
    fn read_update_cache_ignores_corrupt_cache_file() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("update-check.json");
        std::fs::write(&path, "{not-json").expect("write corrupt cache");

        assert_eq!(read_update_cache(&path).expect("read cache"), None);
    }

    #[test]
    fn format_update_notification_includes_release_and_command() {
        let text = format_update_notification(&super::UpdateNotification {
            current_version: "0.1.3".into(),
            latest_version: "v0.1.4".into(),
            release_url: "https://github.com/7df-lab/devo/releases/tag/v0.1.4".into(),
            published_at: None,
            upgrade_command: "install-command",
        });

        assert_eq!(
            text,
            "A new devo version is available: v0.1.4 (current: 0.1.3)\nRelease: https://github.com/7df-lab/devo/releases/tag/v0.1.4\nUpdate: install-command"
        );
    }

    #[test]
    fn cache_read_failure_is_reported() {
        let root = tempfile::tempdir().expect("temp dir");
        let path = root.path().join("update-check.json");
        std::fs::create_dir(&path).expect("create directory in cache path");
        let err = read_update_cache(&path).expect_err("cache read failure");

        assert_eq!(
            err,
            UpdateCheckFailure {
                stage: UpdateCheckStage::CacheRead,
                message: err.message.clone(),
            }
        );
    }

    #[test]
    fn skip_reason_variants_are_stable() {
        let reasons = [
            UpdateCheckSkipReason::Disabled,
            UpdateCheckSkipReason::StartupCheckDisabled,
            UpdateCheckSkipReason::CacheFresh,
        ];

        assert_eq!(reasons.len(), 3);
        assert_eq!(
            UpdatesConfig {
                enabled: true,
                check_on_startup: true,
                check_interval_hours: 24,
            }
            .check_interval_hours,
            24
        );
        let _ = PathBuf::from("unused");
    }
}
