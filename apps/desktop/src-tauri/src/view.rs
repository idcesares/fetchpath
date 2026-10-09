//! The engine's protocol records in the shapes the interface already reads
//! (`JobSnapshot`, `SettingsView` and the rest in `src/main.ts`), so the
//! interface did not change when the queue moved into the engine (FP-055).

use fetchpath_protocol::command::{JobInput, JobRequest};
use fetchpath_protocol::model::{
    self, Density, EngineSettings, MediaInspection, MediaVariantKind, Theme,
};
use fetchpath_protocol::principal::AgentAccess;
pub use fetchpath_protocol::view::{
    JobDetails, JobDraft, JobView, QueueStats, Segment, at, destination, job, jobs, link,
};
use serde::{Deserialize, Serialize};

/// One rule, in the words every client uses.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    pub id: u32,
    pub label: String,
    pub when: String,
    pub then: String,
}

pub fn rules(rules: &[fetchpath_protocol::model::Rule]) -> Vec<RuleView> {
    use fetchpath_protocol::describe;
    rules
        .iter()
        .map(|rule| RuleView {
            id: rule.id,
            label: describe::label(rule),
            when: describe::conditions(&rule.spec.when),
            then: describe::actions(&rule.spec.then),
        })
        .collect()
}

/// How the rules decide for one link: for Add download and for testing a
/// link in Settings.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAdvice {
    /// "Rule 1 (Disc images): a .iso file", when one matches.
    pub matched: Option<String>,
    pub folder: Option<String>,
    pub needs_checksum: bool,
    /// Every rule tried and why, as `fetchpath rules test` prints it.
    pub lines: Vec<String>,
}

pub fn rule_advice(verdict: Option<&fetchpath_protocol::model::RulesVerdict>) -> RuleAdvice {
    let then = verdict
        .and_then(|verdict| verdict.matched.as_ref())
        .map(|rule| &rule.spec.then);
    RuleAdvice {
        matched: fetchpath_protocol::describe::matched(verdict),
        folder: then.and_then(|then| then.folder.clone()),
        needs_checksum: then.is_some_and(|then| then.require_checksum),
        lines: fetchpath_protocol::describe::verdict(verdict),
    }
}

/// One agent's access, as Settings shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub name: String,
    pub folders: Vec<String>,
    pub max_bytes: u64,
    pub max_new_jobs_per_hour: u32,
    /// Inside its folders, nothing it downloads waits for approval.
    pub automatic: bool,
}

pub fn agents(policies: &[AgentAccess]) -> Vec<AgentView> {
    policies
        .iter()
        .map(|access| AgentView {
            name: access.agent.to_string(),
            folders: access.policy.folders.clone(),
            max_bytes: access.policy.max_bytes,
            max_new_jobs_per_hour: access.policy.max_new_jobs_per_hour,
            automatic: access.policy.automatic,
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaDraft {
    pub url: String,
    pub variant_id: String,
    pub quality_label: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentDraft {
    pub url: String,
    pub destination: String,
    pub not_before_ms: Option<u64>,
    pub discover_peers: bool,
    pub upload: bool,
}

impl MediaDraft {
    pub fn request(&self) -> Result<JobRequest, String> {
        Ok(JobRequest::Media {
            input: JobInput::Url {
                url: link(&self.url)?,
            },
            destination: destination(&self.destination),
            not_before: at(self.not_before_ms),
            variant_id: self.variant_id.clone(),
            quality_label: self.quality_label.clone(),
        })
    }
}

impl TorrentDraft {
    pub fn request(&self) -> Result<JobRequest, String> {
        let source = self.url.trim();
        let input = if source.starts_with("magnet:?") || source.starts_with("https://") {
            JobInput::Url { url: link(source)? }
        } else {
            JobInput::TorrentFile {
                path: source.to_owned(),
            }
        };
        Ok(JobRequest::Torrent {
            input,
            destination: destination(&self.destination),
            not_before: at(self.not_before_ms),
            discover_peers: Some(self.discover_peers),
            upload: self.upload,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub max_active_downloads: u64,
    pub default_destination_dir: Option<String>,
    pub auto_retry: bool,
    pub auto_retry_max_attempts: u32,
    pub auto_retry_base_delay_seconds: u64,
    pub close_to_tray: bool,
    pub power_mode: bool,
    pub media_tools_dir: Option<String>,
    pub confirm_remove_completed: bool,
    pub theme: String,
    pub onboarding_completed: bool,
    #[serde(default)]
    pub cache_quota_bytes: Option<u64>,
    /// `comfortable` or `compact`.
    #[serde(default)]
    pub density: Option<String>,
    /// The name this engine shows on every client (contract D6). Absent
    /// leaves it unchanged; blank returns to the computer's name.
    #[serde(default)]
    pub instance_name: Option<String>,
    /// Keep the engine running in the background. Absent leaves it.
    #[serde(default)]
    pub hub_mode: Option<bool>,
    /// Serve the local web UI on loopback. Absent leaves it.
    #[serde(default)]
    pub web_ui: Option<bool>,
    /// Free space left on every drive; 0 is automatic. Absent leaves it.
    #[serde(default)]
    pub disk_reserve_bytes: Option<u64>,
}

impl Settings {
    pub fn from_engine(settings: &EngineSettings) -> Self {
        Self {
            max_active_downloads: settings.max_active_downloads,
            default_destination_dir: settings.default_destination_dir.clone(),
            auto_retry: settings.auto_retry,
            auto_retry_max_attempts: settings.auto_retry_max_attempts,
            auto_retry_base_delay_seconds: settings.auto_retry_base_delay_seconds,
            close_to_tray: settings.close_to_tray,
            power_mode: settings.power_mode,
            media_tools_dir: settings.media_tools_dir.clone(),
            confirm_remove_completed: settings.confirm_remove_completed,
            theme: match settings.theme {
                Theme::Light => "light",
                Theme::Dark => "dark",
                Theme::HighContrast => "high-contrast",
                Theme::System | Theme::Unknown => "system",
            }
            .into(),
            onboarding_completed: settings.onboarding_completed,
            cache_quota_bytes: settings.cache_quota_bytes,
            density: Some(
                match settings.density {
                    Some(Density::Compact) => "compact",
                    _ => "comfortable",
                }
                .into(),
            ),
            instance_name: settings.instance_name.clone(),
            hub_mode: settings.hub_mode,
            web_ui: settings.web_ui,
            disk_reserve_bytes: settings.disk_reserve_bytes,
        }
    }

    /// The settings to send. The interface has no sign-in start switch yet,
    /// so that one is left out, which the engine reads as "unchanged".
    pub fn to_engine(&self) -> EngineSettings {
        EngineSettings {
            max_active_downloads: self.max_active_downloads,
            default_destination_dir: self.default_destination_dir.clone(),
            auto_retry: self.auto_retry,
            auto_retry_max_attempts: self.auto_retry_max_attempts,
            auto_retry_base_delay_seconds: self.auto_retry_base_delay_seconds,
            close_to_tray: self.close_to_tray,
            power_mode: self.power_mode,
            media_tools_dir: self.media_tools_dir.clone(),
            confirm_remove_completed: self.confirm_remove_completed,
            theme: match self.theme.as_str() {
                "light" => Theme::Light,
                "dark" => Theme::Dark,
                "high-contrast" => Theme::HighContrast,
                _ => Theme::System,
            },
            onboarding_completed: self.onboarding_completed,
            start_engine_at_sign_in: None,
            cache_quota_bytes: self.cache_quota_bytes,
            density: match self.density.as_deref() {
                Some("compact") => Some(Density::Compact),
                Some("comfortable") => Some(Density::Comfortable),
                _ => None,
            },
            instance_name: self.instance_name.clone(),
            hub_mode: self.hub_mode,
            web_ui: self.web_ui,
            disk_reserve_bytes: self.disk_reserve_bytes,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub settings: Settings,
    pub repaired: bool,
    pub max_active_limit: u64,
    pub max_retry_attempts: u32,
    /// The folder used when no default destination is set.
    pub system_download_dir: Option<String>,
}

impl SettingsView {
    pub fn new(view: &model::SettingsView, system_download_dir: Option<String>) -> Self {
        Self {
            settings: Settings::from_engine(&view.settings),
            repaired: view.repaired,
            max_active_limit: view.max_active_limit,
            max_retry_attempts: view.max_retry_attempts,
            system_download_dir,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaVariant {
    pub id: String,
    pub label: String,
    pub kind: &'static str,
    pub extension: String,
    pub height: Option<u32>,
    pub fps: Option<u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub title: String,
    pub duration_seconds: Option<f64>,
    pub variants: Vec<MediaVariant>,
}

impl From<&MediaInspection> for Inspection {
    fn from(inspection: &MediaInspection) -> Self {
        Self {
            title: inspection.title.clone(),
            duration_seconds: inspection.duration_seconds,
            variants: inspection
                .variants
                .iter()
                .filter(|variant| variant.kind != MediaVariantKind::Unknown)
                .map(|variant| MediaVariant {
                    id: variant.id.clone(),
                    label: variant.label.clone(),
                    kind: match variant.kind {
                        MediaVariantKind::Audio => "audio",
                        _ => "video",
                    },
                    extension: variant.extension.clone(),
                    height: variant.height,
                    fps: variant.fps,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelResponse {
    pub outcome: &'static str,
    pub job: JobView,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn torrent_draft_preserves_local_file_as_a_torrent_input() {
        let draft = TorrentDraft {
            url: r"C:\Downloads\debian.torrent".into(),
            destination: r"C:\Downloads\debian".into(),
            not_before_ms: None,
            discover_peers: true,
            upload: false,
        };
        assert!(matches!(
            draft.request().unwrap(),
            JobRequest::Torrent {
                input: JobInput::TorrentFile { .. },
                ..
            }
        ));
        let url = TorrentDraft {
            url: "magnet:?xt=urn:btih:abc".into(),
            ..draft
        };
        assert!(matches!(
            url.request().unwrap(),
            JobRequest::Torrent {
                input: JobInput::Url { .. },
                ..
            }
        ));
    }

    #[test]
    fn settings_round_trip_and_leave_sign_in_start_alone() {
        let engine = EngineSettings {
            max_active_downloads: 4,
            default_destination_dir: Some("D:\\Downloads".into()),
            auto_retry: false,
            auto_retry_max_attempts: 2,
            auto_retry_base_delay_seconds: 30,
            close_to_tray: true,
            power_mode: true,
            media_tools_dir: None,
            confirm_remove_completed: false,
            theme: Theme::Dark,
            onboarding_completed: true,
            start_engine_at_sign_in: Some(true),
            cache_quota_bytes: Some(1 << 30),
            density: Some(Density::Comfortable),
            instance_name: Some("Studio PC".into()),
            hub_mode: Some(false),
            web_ui: Some(false),
            disk_reserve_bytes: Some(0),
        };
        let shown = Settings::from_engine(&engine);
        assert_eq!(serde_json::to_value(&shown).unwrap()["theme"], "dark");
        let back = shown.to_engine();
        assert_eq!(back.start_engine_at_sign_in, None);
        assert_eq!(
            EngineSettings {
                start_engine_at_sign_in: Some(true),
                ..back
            },
            engine
        );
    }
}
