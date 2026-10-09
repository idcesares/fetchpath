//! Persisted user settings.
//!
//! Two rules shape this module.
//!
//! First, **a settings file never stops Fetchpath from starting.** The file is
//! ordinary JSON in the user's app data directory; it can be truncated by a
//! power loss, hand-edited, or written by an older build. Any value that cannot
//! be read falls back to the documented default for that one field, and a file
//! that cannot be parsed at all falls back to the whole default set. Load
//! reports what it had to repair so the interface can say so plainly rather
//! than silently changing what the user asked for.
//!
//! Second, **every bound is enforced here, not at the call site.** Concurrency,
//! retry counts and delays all clamp to ranges the engine can actually honour,
//! so no other module has to re-check them.

use fetchpath_protocol::model::Rule;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MIN_ACTIVE_DOWNLOADS: usize = 1;
pub const MAX_ACTIVE_DOWNLOADS: usize = 8;
pub const DEFAULT_ACTIVE_DOWNLOADS: usize = 3;

pub const MAX_RETRY_ATTEMPTS: u32 = 10;
pub const DEFAULT_RETRY_ATTEMPTS: u32 = 3;

pub const MIN_RETRY_DELAY_SECONDS: u64 = 5;
pub const MAX_RETRY_DELAY_SECONDS: u64 = 3_600;
pub const DEFAULT_RETRY_DELAY_SECONDS: u64 = 15;

pub const MIN_CACHE_QUOTA_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_CACHE_QUOTA_BYTES: u64 = 256 * 1024 * 1024 * 1024;
pub const DEFAULT_CACHE_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Upper bound for any stored path, matching the queue's own limit.
const MAX_PATH_LENGTH: usize = 4_096;

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the Windows light/dark setting. The default, and the only value
    /// that keeps working when the user changes their system theme.
    #[default]
    System,
    Light,
    Dark,
    /// Pure black ground and white lines (FP-095).
    #[serde(rename = "high-contrast")]
    HighContrast,
}

/// Row and control density (FP-095).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

impl Density {
    fn is_default(&self) -> bool {
        *self == Self::Comfortable
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// How many downloads may run at once. More is not faster past the point
    /// where the link saturates, so this is presented as a choice rather than
    /// a performance setting.
    pub max_active_downloads: usize,
    /// Where the destination picker opens, and what a browser capture uses.
    /// `None` means the Windows Downloads folder.
    pub default_destination_dir: Option<String>,
    /// Retry transport failures automatically. Never applies to failures that
    /// need a human decision, such as a destination conflict or an expired
    /// private source.
    pub auto_retry: bool,
    pub auto_retry_max_attempts: u32,
    /// First backoff step. Each further attempt doubles it, up to the maximum.
    pub auto_retry_base_delay_seconds: u64,
    /// Closing the window hides Fetchpath to the notification area instead of
    /// quitting, so a running queue is not lost to a stray click.
    pub close_to_tray: bool,
    /// Show transfer diagnostics inline. Additive only: nothing ordinary is
    /// removed or moved when this is on.
    pub power_mode: bool,
    /// Where `yt-dlp` and `ffmpeg` live, when the user supplied them.
    pub media_tools_dir: Option<String>,
    /// Ask before removing a completed download from the list.
    pub confirm_remove_completed: bool,
    pub theme: Theme,
    /// Set once the first-run walkthrough has been seen or dismissed.
    pub onboarding_completed: bool,
    /// Start the engine when the person signs in to Windows (FP-053). Off by
    /// default, and written only when on, so a settings file from before it
    /// writes back unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub start_engine_at_sign_in: bool,
    /// Smart rules, in the order they are tried (FP-064). Written only when
    /// there are some; an unreadable rule is dropped alone, never the file.
    #[serde(
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "readable_rules"
    )]
    pub rules: Vec<Rule>,
    /// The most the content cache may hold (FP-032). Written only when
    /// changed, so a settings file from before it writes back unchanged.
    #[serde(skip_serializing_if = "is_default_quota")]
    pub cache_quota_bytes: u64,
    /// Row and control density (FP-095). Written only when compact, so a
    /// settings file from before it writes back unchanged.
    #[serde(skip_serializing_if = "Density::is_default")]
    pub density: Density,
    /// The name every client shows for this engine (contract D6). `None`
    /// means the computer's name; written only when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_name: Option<String>,
    /// Keep the engine running in the background (FP-101, design §10): it
    /// does not stop when idle, starts at sign-in, and keeps the computer
    /// awake while downloads run. Off by default and written only when on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hub_mode: bool,
    /// Serve the local web UI on loopback (FP-104). Off by default and
    /// written only when on.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub web_ui: bool,
    /// Free space left on every drive Fetchpath saves to (FP-101). 0, the
    /// default, is automatic: the larger of 5 GiB and 5 % of the drive.
    /// Written only when chosen.
    #[serde(skip_serializing_if = "is_zero")]
    pub disk_reserve_bytes: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_default_quota(bytes: &u64) -> bool {
    *bytes == DEFAULT_CACHE_QUOTA_BYTES
}

fn readable_rules<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Rule>, D::Error> {
    let values = Vec::<serde_json::Value>::deserialize(deserializer).unwrap_or_default();
    Ok(values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_active_downloads: DEFAULT_ACTIVE_DOWNLOADS,
            default_destination_dir: None,
            auto_retry: true,
            auto_retry_max_attempts: DEFAULT_RETRY_ATTEMPTS,
            auto_retry_base_delay_seconds: DEFAULT_RETRY_DELAY_SECONDS,
            close_to_tray: true,
            power_mode: false,
            media_tools_dir: None,
            confirm_remove_completed: true,
            theme: Theme::System,
            onboarding_completed: false,
            start_engine_at_sign_in: false,
            rules: Vec::new(),
            cache_quota_bytes: DEFAULT_CACHE_QUOTA_BYTES,
            density: Density::Comfortable,
            instance_name: None,
            hub_mode: false,
            web_ui: false,
            disk_reserve_bytes: 0,
        }
    }
}

impl Settings {
    /// Forces every field into a range the engine can honour.
    ///
    /// Called on load and on every update, so a value written by an older
    /// build, a hand edit, or a renderer message cannot reach the queue out of
    /// range.
    pub fn clamp(&mut self) {
        self.max_active_downloads = self
            .max_active_downloads
            .clamp(MIN_ACTIVE_DOWNLOADS, MAX_ACTIVE_DOWNLOADS);
        self.auto_retry_max_attempts = self.auto_retry_max_attempts.min(MAX_RETRY_ATTEMPTS);
        self.auto_retry_base_delay_seconds = self
            .auto_retry_base_delay_seconds
            .clamp(MIN_RETRY_DELAY_SECONDS, MAX_RETRY_DELAY_SECONDS);
        self.default_destination_dir = clamp_directory(self.default_destination_dir.take());
        self.media_tools_dir = clamp_directory(self.media_tools_dir.take());
        self.rules = crate::rules::sanitized(std::mem::take(&mut self.rules));
        self.cache_quota_bytes = self
            .cache_quota_bytes
            .clamp(MIN_CACHE_QUOTA_BYTES, MAX_CACHE_QUOTA_BYTES);
        self.instance_name = clamp_instance_name(self.instance_name.take());
        // Always on means back after a restart too, so it implies the
        // sign-in start; turning it off leaves the sign-in start as it is.
        if self.hub_mode {
            self.start_engine_at_sign_in = true;
        }
    }

    /// The name clients show for this engine: the one the person chose, or
    /// the computer's.
    pub fn display_instance_name(&self) -> String {
        self.instance_name.clone().unwrap_or_else(|| {
            std::env::var("COMPUTERNAME")
                .ok()
                .and_then(|name| clamp_instance_name(Some(name)))
                .unwrap_or_else(|| "This computer".to_owned())
        })
    }

    /// The delay before attempt number `attempt` (1-based), doubling each time
    /// and never exceeding the configured ceiling.
    pub fn retry_delay_seconds(&self, attempt: u32) -> u64 {
        let steps = attempt.saturating_sub(1).min(16);
        self.auto_retry_base_delay_seconds
            .saturating_mul(1_u64 << steps)
            .min(MAX_RETRY_DELAY_SECONDS)
    }
}

/// The longest instance name kept, in characters.
pub const MAX_INSTANCE_NAME_CHARS: usize = 64;

/// An instance name is shown wherever a consequential action names its
/// target, so it keeps only printable characters: no control, line-breaking,
/// zero-width or text-reordering characters. Blank means the computer's name.
fn clamp_instance_name(value: Option<String>) -> Option<String> {
    let cleaned: String = value?
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(
                    *c,
                    '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
                )
        })
        .take(MAX_INSTANCE_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// A stored directory is only kept if it is an absolute path of sane length.
/// A relative path would resolve against whatever directory Fetchpath happened
/// to be launched from, which is not a location the user chose.
fn clamp_directory(value: Option<String>) -> Option<String> {
    let value = value?;
    if value.is_empty() || value.len() > MAX_PATH_LENGTH {
        return None;
    }
    let path = PathBuf::from(&value);
    // A folder downloads could not be saved in is no default at all.
    (path.is_absolute() && crate::plain_path(&path).is_ok()).then_some(value)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsFile {
    #[serde(default)]
    schema_version: u32,
    #[serde(flatten)]
    settings: Settings,
}

/// The outcome of reading the settings file.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// True when the stored file was missing, unreadable or out of range and
    /// defaults were substituted. The interface tells the user rather than
    /// quietly presenting defaults as their own choices.
    pub repaired: bool,
}

/// Reads settings, falling back to defaults for anything unusable.
pub fn load(path: &Path) -> Loaded {
    let Ok(raw) = fs::read_to_string(path) else {
        // No file yet is the ordinary first-run case, not a repair.
        return Loaded {
            settings: Settings::default(),
            repaired: false,
        };
    };
    match serde_json::from_str::<SettingsFile>(&raw) {
        Ok(file) => {
            let mut settings = file.settings;
            let before = settings.clone();
            settings.clamp();
            let stored_rules = serde_json::from_str::<serde_json::Value>(&raw)
                .ok()
                .and_then(|value| value.get("rules")?.as_array().map(Vec::len))
                .unwrap_or_default();
            Loaded {
                repaired: file.schema_version != SCHEMA_VERSION
                    || settings != before
                    || stored_rules != settings.rules.len(),
                settings,
            }
        }
        Err(_) => Loaded {
            settings: Settings::default(),
            repaired: true,
        },
    }
}

/// Writes settings through a temporary file and a rename.
///
/// The same create-then-rename shape the queue uses: an interrupted write
/// leaves the previous settings intact instead of a half-written file that
/// would load as defaults on the next launch.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = SettingsFile {
        schema_version: SCHEMA_VERSION,
        settings: settings.clone(),
    };
    let body = serde_json::to_vec_pretty(&file)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, &body)?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fetchpath-settings-{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("settings-v1.json")
    }

    #[test]
    fn a_missing_file_yields_defaults_without_reporting_a_repair() {
        let loaded = load(&temp_path("missing"));
        assert_eq!(loaded.settings, Settings::default());
        assert!(!loaded.repaired);
    }

    #[test]
    fn settings_survive_a_save_and_load_round_trip() {
        let path = temp_path("roundtrip");
        let settings = Settings {
            max_active_downloads: 6,
            power_mode: true,
            theme: Theme::Dark,
            default_destination_dir: Some(r"C:\Users\example\Downloads".into()),
            ..Settings::default()
        };
        save(&path, &settings).unwrap();

        let loaded = load(&path);
        assert_eq!(loaded.settings, settings);
        assert!(!loaded.repaired);
    }

    #[test]
    fn a_truncated_file_falls_back_to_defaults_and_reports_the_repair() {
        let path = temp_path("truncated");
        fs::write(&path, br#"{"schemaVersion":1,"maxActiveDownl"#).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.repaired);
    }

    #[test]
    fn out_of_range_values_are_clamped_and_reported() {
        let path = temp_path("range");
        fs::write(
            &path,
            br#"{"schemaVersion":1,"maxActiveDownloads":9999,"defaultDestinationDir":null,
                 "autoRetry":true,"autoRetryMaxAttempts":500,"autoRetryBaseDelaySeconds":0,
                 "closeToTray":true,"powerMode":false,"mediaToolsDir":null,
                 "confirmRemoveCompleted":true,"theme":"system","onboardingCompleted":false}"#,
        )
        .unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.settings.max_active_downloads, MAX_ACTIVE_DOWNLOADS);
        assert_eq!(loaded.settings.auto_retry_max_attempts, MAX_RETRY_ATTEMPTS);
        assert_eq!(
            loaded.settings.auto_retry_base_delay_seconds,
            MIN_RETRY_DELAY_SECONDS
        );
        assert!(loaded.repaired);
    }

    #[test]
    fn a_relative_destination_is_rejected_rather_than_resolved() {
        let mut settings = Settings {
            default_destination_dir: Some("downloads".into()),
            media_tools_dir: Some(r"..\tools".into()),
            ..Settings::default()
        };
        settings.clamp();
        // Both would otherwise resolve against whatever directory Fetchpath was
        // launched from, which is never a folder the user picked.
        assert_eq!(settings.default_destination_dir, None);
        assert_eq!(settings.media_tools_dir, None);
    }

    #[test]
    fn rules_round_trip_and_an_unreadable_one_is_dropped_alone() {
        let path = temp_path("rules");
        fs::write(
            &path,
            br#"{"schemaVersion":1,"maxActiveDownloads":5,"rules":[
                {"id":1,"when":{"file_types":["iso"]},"then":{"folder":"D:\\ISOs"}},
                {"id":2,"when":"not a rule"}]}"#,
        )
        .unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.settings.max_active_downloads, 5);
        assert_eq!(loaded.settings.rules.len(), 1);
        assert_eq!(
            loaded.settings.rules[0].spec.then.folder.as_deref(),
            Some(r"D:\ISOs")
        );
        assert!(loaded.repaired);

        save(&path, &loaded.settings).unwrap();
        let again = load(&path);
        assert_eq!(again.settings, loaded.settings);
        assert!(!again.repaired);
    }

    #[test]
    fn retry_delay_doubles_and_then_holds_at_the_ceiling() {
        let settings = Settings::default();
        assert_eq!(settings.retry_delay_seconds(1), 15);
        assert_eq!(settings.retry_delay_seconds(2), 30);
        assert_eq!(settings.retry_delay_seconds(3), 60);
        // Far beyond any configured attempt count, the delay is still bounded.
        assert_eq!(settings.retry_delay_seconds(60), MAX_RETRY_DELAY_SECONDS);
    }

    #[test]
    fn always_on_implies_the_sign_in_start_and_off_leaves_it() {
        let mut settings = Settings {
            hub_mode: true,
            ..Settings::default()
        };
        settings.clamp();
        assert!(settings.start_engine_at_sign_in);

        settings.hub_mode = false;
        settings.clamp();
        assert!(
            settings.start_engine_at_sign_in,
            "turning it off keeps the person's sign-in start"
        );

        // Written only when on, so a file from before it writes back unchanged.
        let off = serde_json::to_value(Settings::default()).unwrap();
        assert!(off.get("hubMode").is_none());
    }
}
