//! The download session: the queue, its persistence, scheduling, retry
//! policy, rate estimates, history, settings, media orchestration, agent
//! policy, and public sharing.
//!
//! Hosted solely by the headless engine (`fetchpath engine`). Holds no UI or
//! transport types; every client reaches it through `fetchpath-protocol`.

pub use fetchpath_browser_inbox as browser_inbox;
mod durable;
pub mod engine;
pub mod lan;
mod persist;
pub mod policy;
mod provenance;
mod rate;
mod record;
pub mod rules;
pub mod settings;
pub mod share;
pub mod space;
mod types;
mod validation;
mod wire;

pub(crate) use record::{error_code, failure_code};
pub use types::{
    APPROVAL_EXPIRY_MS, CancelResponse, JobDetails, JobDraft, JobSnapshot, MAX_CACHE_QUOTA_BYTES,
    MIN_CACHE_QUOTA_BYTES, MediaDraft, QueueStats, TorrentDraft, TorrentPolicy,
};
pub(crate) use types::{Origin, PublishedOutput, SegmentView};
pub(crate) use validation::plain_path;

use browser_inbox::BridgeStore;
#[cfg(test)]
use durable::RecordDurable;
use durable::{Durable, RemovedJob};
use fetchpath_core::{RequestContext, normalize_sha256};
use fetchpath_media::{MediaInspection, MediaJob, MediaTools};
use fetchpath_protocol::principal::{AgentName, AgentPolicy, ApprovalReason, Principal};
#[cfg(test)]
use fetchpath_torrent::JobState as TorrentJobState;
use persist::{
    ENGINE_FILE, Loaded, PersistedQueue, load_engine, load_persisted, newer_queue_explanation,
    write_json_atomically, write_queue,
};
#[cfg(test)]
use persist::{PersistedRecord, save_persisted};
#[cfg(test)]
use rate::RateEstimate;
use record::{
    JobHandle, QueueRecord, UNREADABLE_CHECKSUM, checked_checksum, discover_media_tools, file_job,
    find_record, find_record_mut, has_checksum, now_ms, refresh_record, remaining_bytes,
    retried_automatically, sample_rate, show_waiting_for_space, torrent_job,
};
#[cfg(test)]
use record::{action_for_error, is_terminal, recovery_action};
use settings::Settings;
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
#[cfg(test)]
use validation::published_inside;
use validation::{
    display_url, is_media_page, redact_userinfo, restartable_url, torrent_metadata_path,
    unique_browser_destination, validated_destination, validated_source,
    validated_torrent_destination, validated_torrent_source,
};

const QUEUE_SCHEMA_VERSION: u32 = 2;
const ENGINE_SCHEMA_VERSION: u32 = 1;
const MAX_TORRENT_METADATA_BYTES: u64 = 4 * 1024 * 1024;
pub const DEFAULT_MAX_ACTIVE: usize = 3;
/// Upper bound for an inter-process address. Long enough for real signed links,
/// short enough that a malformed renderer message cannot force unbounded work.
const MAX_SOURCE_LENGTH: usize = 8_192;
/// Upper bound for a destination path. Windows long paths stop well below this.
pub const MAX_DESTINATION_LENGTH: usize = 4_096;

pub struct Session {
    inner: Mutex<QueueState>,
    /// Coalesced wakeups for commands and settled transfers. No queue or
    /// durable locks are taken by a worker's completion callback.
    wake: Arc<ReconcileWake>,
    state_path: Option<PathBuf>,
    /// Hashes of credentialed outputs (contract D8, FP-121). Locked after
    /// `inner`, never before it.
    provenance: Mutex<provenance::Provenance>,
    settings_path: Option<PathBuf>,
    /// Guarded separately from the queue so reading a setting never waits on a
    /// transfer, and so a settings write cannot deadlock against the poll.
    settings: Mutex<Settings>,
    /// True when the stored settings file had to be repaired on load.
    settings_repaired: bool,
    browser_store: Option<BridgeStore>,
    browser_download_dir: Option<PathBuf>,
    media_tools: Mutex<Option<MediaTools>>,
    /// The ledger, the event log and the subscribers (FP-051). Locked after
    /// `inner`, never before it.
    durable: Mutex<Durable>,
    /// Metadata copies retired by a source change or removal. Delete only
    /// after the queue rename commits, including deferred Engine commands.
    retired_torrent_metadata: Mutex<Vec<PathBuf>>,
    /// Set when the engine stops: no job starts any more (FP-053).
    halted: std::sync::atomic::AtomicBool,
    /// When running downloads' drives were last measured (FP-101).
    space_checked_ms: std::sync::atomic::AtomicU64,
    /// Drive measurements, taken off the queue lock.
    space: Arc<space::SpaceCache>,
    /// Set when the queue was written by a newer Fetchpath (FP-070): the
    /// saved list is shown as it is, nothing starts, and nothing is written
    /// over it. Holds the explanation.
    read_only: Option<String>,
    /// What each configured agent may do without asking (contract D1).
    agents: Mutex<BTreeMap<AgentName, AgentPolicy>>,
    agents_path: Option<PathBuf>,
    /// The content cache, when the host gave one (FP-032). The person's and
    /// the browser's checksum-verified downloads complete from it and fill
    /// it; an agent's never touch it, so an agent cannot learn what the
    /// person has downloaded or obtain it by naming its checksum.
    cache_root: Mutex<Option<PathBuf>>,
    /// Paired devices and sharing, when the host gave a place for them.
    lan: std::sync::OnceLock<lan::Lan>,
    /// Sizes of links looked at lately, so a size rule decides the same way
    /// when the job is created as it did on the card.
    inspected_sizes: Mutex<std::collections::VecDeque<(String, u64)>>,
}

#[derive(Default)]
struct ReconcileWake {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl ReconcileWake {
    fn notify(&self) {
        *self.pending.lock().expect("reconcile wake poisoned") = true;
        self.ready.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let pending = self.pending.lock().expect("reconcile wake poisoned");
        let (mut pending, _) = self
            .ready
            .wait_timeout_while(pending, timeout, |pending| !*pending)
            .expect("reconcile wake poisoned");
        std::mem::take(&mut *pending)
    }
}

/// How many inspected sizes are remembered.
const REMEMBERED_SIZES: usize = 64;

#[derive(Default)]
struct QueueState {
    records: Vec<QueueRecord>,
    /// Downloads stopped at their checkpoint because their drive reached
    /// the disk reserve (FP-101). They go back to the queue once stopped
    /// and start again when there is room. Not saved: after a restart the
    /// queue simply checks the room again.
    space_stopped: std::collections::HashSet<String>,
    /// Pages captured from the browser that are media rather than files. They
    /// open in Add download, where the quality is chosen, instead of being
    /// saved as a web page. Their captures stay pending in the browser inbox
    /// until a client takes them (FP-056), so an engine that leaves before
    /// any window opens loses none.
    link_reviews: Vec<LinkReview>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkReview {
    capture_id: String,
    credential_ref: String,
    url: String,
}

impl Session {
    #[cfg(test)]
    fn load(state_path: PathBuf, max_active: usize) -> io::Result<Self> {
        Self::load_with_browser(state_path, max_active, None)
    }

    pub fn load_with_browser(
        state_path: PathBuf,
        max_active: usize,
        browser_download_dir: Option<PathBuf>,
    ) -> io::Result<Self> {
        let (persisted, newer) = match load_persisted(&state_path)? {
            Loaded::Current(queue) => (Some(queue), None),
            Loaded::Newer { version, queue } => (None, Some((version, queue))),
            Loaded::Missing => (None, None),
        };
        let (generation, cursor) = persisted.as_ref().map_or((0, 0), |queue| {
            (queue.engine_generation, queue.engine_cursor)
        });
        let engine_path = state_path.with_file_name(ENGINE_FILE);
        let (engine, discarded, seen) = load_engine(&engine_path).committed(generation, cursor);
        let now = now_ms();
        let browser_store = state_path
            .parent()
            .map(|parent| BridgeStore::new(parent.to_path_buf()));
        let settings_path = state_path.with_file_name("settings-v1.json");
        let agents_path = state_path.with_file_name(policy::AGENTS_FILE);
        let loaded = settings::load(&settings_path);
        let mut stored = loaded.settings;
        // A caller-supplied concurrency (the tests, and the launch default) only
        // applies when the user has never chosen one themselves.
        if !settings_path.exists() {
            stored.max_active_downloads = max_active;
            stored.clamp();
        }
        let media_tools = discover_media_tools(stored.media_tools_dir.as_deref());
        let records: Vec<QueueRecord> = persisted
            .map(|queue| {
                queue
                    .records
                    .into_iter()
                    .map(|saved| {
                        QueueRecord::restore(
                            saved,
                            now,
                            browser_store.as_ref(),
                            media_tools.as_ref(),
                            &state_path,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut records = records;
        let read_only = newer.map(|(version, queue)| {
            // Shown as the newer build saved them: no download is prepared
            // and no browser secret is opened for them.
            records = queue
                .map(|queue| queue.records.into_iter().map(QueueRecord::shown).collect())
                .unwrap_or_default();
            newer_queue_explanation(version)
        });
        for record in &mut records {
            seen.advance(record);
        }
        Ok(Self {
            wake: Arc::default(),
            inner: Mutex::new(QueueState {
                records,
                ..QueueState::default()
            }),
            provenance: Mutex::new(provenance::Provenance::load(state_path.parent())),
            state_path: Some(state_path),
            settings_path: Some(settings_path),
            settings: Mutex::new(stored),
            settings_repaired: loaded.repaired,
            browser_store,
            browser_download_dir,
            media_tools: Mutex::new(media_tools),
            durable: Mutex::new(Durable {
                engine,
                // Rewrite the engine file once without what never committed.
                engine_changed: discarded,
                committed: generation,
                ..Durable::default()
            }),
            retired_torrent_metadata: Mutex::new(Vec::new()),
            halted: std::sync::atomic::AtomicBool::new(read_only.is_some()),
            space_checked_ms: std::sync::atomic::AtomicU64::new(0),
            space: space::SpaceCache::new(space::probe),
            read_only,
            agents: Mutex::new(policy::AgentsFile::load(&agents_path)),
            agents_path: Some(agents_path),
            cache_root: Mutex::default(),
            lan: std::sync::OnceLock::new(),
            inspected_sizes: Mutex::default(),
        })
    }

    #[cfg(test)]
    fn in_memory(max_active: usize) -> Self {
        let mut stored = Settings {
            max_active_downloads: max_active,
            // Tiny fixtures must not depend on the host drive's free-space ratio.
            disk_reserve_bytes: 1,
            ..Settings::default()
        };
        stored.clamp();
        Self {
            wake: Arc::default(),
            inner: Mutex::new(QueueState::default()),
            provenance: Mutex::default(),
            state_path: None,
            settings_path: None,
            settings: Mutex::new(stored),
            settings_repaired: false,
            browser_store: None,
            browser_download_dir: None,
            media_tools: Mutex::new(None),
            durable: Mutex::new(Durable::default()),
            retired_torrent_metadata: Mutex::new(Vec::new()),
            halted: std::sync::atomic::AtomicBool::new(false),
            space_checked_ms: std::sync::atomic::AtomicU64::new(0),
            space: space::SpaceCache::new(space::probe),
            read_only: None,
            agents: Mutex::new(BTreeMap::new()),
            agents_path: None,
            cache_root: Mutex::default(),
            lan: std::sync::OnceLock::new(),
            inspected_sizes: Mutex::default(),
        }
    }

    #[cfg(test)]
    fn in_memory_with_media(max_active: usize, media_tools: MediaTools) -> Self {
        let jobs = Self::in_memory(max_active);
        *jobs.media_tools.lock().expect("media tools poisoned") = Some(media_tools);
        jobs
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().expect("settings poisoned").clone()
    }

    /// Whether the engine is kept running in the background (FP-101).
    pub fn hub_mode(&self) -> bool {
        self.settings.lock().expect("settings poisoned").hub_mode
    }

    /// Downloads transferring now, as opposed to queued or scheduled.
    pub fn running_count(&self) -> usize {
        if self.read_only.is_some() {
            return 0;
        }
        // Read as it stands: reconciling belongs to the engine's own pass.
        let state = self.inner.lock().expect("desktop jobs poisoned");
        state
            .records
            .iter()
            .filter(|record| record.view.state == "running")
            .count()
    }

    /// The name clients show for this engine (contract D6).
    pub fn instance_name(&self) -> String {
        self.settings
            .lock()
            .expect("settings poisoned")
            .display_instance_name()
    }

    /// True when the stored settings file was unusable on load and defaults
    /// were substituted.
    pub fn settings_repaired(&self) -> bool {
        self.settings_repaired
    }

    /// Why the queue cannot be changed, when it was written by a newer
    /// Fetchpath (FP-070).
    pub fn read_only(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// Media pages sent from the browser, each handed out once. Only now is
    /// the capture marked done and its protected context, which a media page
    /// does not use, deleted. Both happen under the queue lock, so the next
    /// intake cannot offer the page again.
    pub fn take_link_reviews(&self) -> Vec<String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let reviews = std::mem::take(&mut state.link_reviews);
        let mut urls = Vec::with_capacity(reviews.len());
        for review in reviews {
            if let Some(store) = self.browser_store.as_ref() {
                // A receipt that cannot be written offers the page again
                // after a restart, which is the safe way round.
                let _ = store.mark_processed(&review.capture_id, "link-review");
                let _ = store.remove_secret(&review.credential_ref);
            }
            if !urls.contains(&review.url) {
                urls.push(review.url);
            }
        }
        urls
    }

    fn max_active(&self) -> usize {
        self.settings().max_active_downloads
    }

    fn media_tools(&self) -> Option<MediaTools> {
        self.media_tools
            .lock()
            .expect("media tools poisoned")
            .clone()
    }

    /// Keeps the content cache at `root`. Without this call nothing is cached.
    pub fn use_cache(&self, root: PathBuf) {
        *self.cache_root.lock().expect("cache poisoned") = Some(root);
    }

    /// Resolves a model or dataset repository link to one commit and its
    /// files (FP-022). Nothing is queued.
    pub fn inspect_repository(
        &self,
        link: &str,
    ) -> Result<fetchpath_protocol::model::RepositoryView, String> {
        let reference = fetchpath_providers::parse(link).ok_or_else(|| {
            "That is not a Hugging Face repository link, such as hf://owner/name or https://huggingface.co/owner/name.".to_string()
        })?;
        let listing =
            fetchpath_providers::resolve(&reference).map_err(|error| error.to_string())?;
        Ok(fetchpath_protocol::model::RepositoryView {
            provider: "huggingface".into(),
            kind: listing.kind.name().into(),
            total_bytes: listing.files.iter().filter_map(|file| file.size).sum(),
            files: listing
                .files
                .into_iter()
                .map(|file| fetchpath_protocol::model::RepositoryFile {
                    path: file.path,
                    size: file.size,
                    sha256: file.sha256,
                    url: file.url,
                })
                .collect(),
            repo: listing.repo,
            revision: listing.revision,
            commit: listing.commit,
            skipped: listing.skipped,
        })
    }

    /// Keeps paired devices and the sharing switch in `dir`, sharing from
    /// the cache at `cache_root`, and resumes sharing if it was left on.
    pub fn use_lan(&self, dir: PathBuf, cache_root: PathBuf) {
        if self.lan.set(lan::Lan::new(dir, cache_root)).is_ok() {
            self.lan().expect("just set").resume();
        }
    }

    pub fn lan(&self) -> Result<&lan::Lan, String> {
        self.lan
            .get()
            .ok_or_else(|| "Paired devices are not available in this Fetchpath.".to_string())
    }

    fn cache_config(settings: &Settings) -> fetchpath_core::fetchpath_cache::CacheConfig {
        let quota = settings.cache_quota_bytes;
        fetchpath_core::fetchpath_cache::CacheConfig::new(quota, quota)
    }

    fn open_cache(&self) -> Result<fetchpath_core::fetchpath_cache::ContentCache, String> {
        let root = self
            .cache_root
            .lock()
            .expect("cache poisoned")
            .clone()
            .ok_or_else(|| "This Fetchpath keeps no cache.".to_string())?;
        fetchpath_core::fetchpath_cache::ContentCache::open(
            &root,
            Self::cache_config(&self.settings()),
        )
        .map_err(|error| format!("The cache cannot be read: {error}"))
    }

    /// How much the cache holds, and its bounds.
    pub fn cache_status(&self) -> Result<fetchpath_protocol::model::CacheView, String> {
        let cache = self.open_cache()?;
        Ok(fetchpath_protocol::model::CacheView {
            bytes: cache.total_bytes(),
            entries: cache.entries().len() as u64,
            quota_bytes: cache.config().quota_bytes,
            min_quota_bytes: MIN_CACHE_QUOTA_BYTES,
            max_quota_bytes: MAX_CACHE_QUOTA_BYTES,
        })
    }

    /// Empties the cache. Saved downloads are separate files and stay.
    pub fn clear_cache(&self) -> Result<fetchpath_protocol::model::CacheView, String> {
        self.open_cache()?
            .clear()
            .map_err(|error| format!("The cache could not be cleared: {error}"))?;
        self.cache_status()
    }

    fn media_tools_install_dir(&self) -> Result<PathBuf, String> {
        self.data_dir()
            .map(|dir| dir.join("media-tools"))
            .ok_or_else(|| "This session has no data folder for media tools.".to_owned())
    }

    fn map_tools_status(
        status: fetchpath_media::setup::ToolsStatus,
    ) -> fetchpath_protocol::model::MediaToolsStatus {
        fetchpath_protocol::model::MediaToolsStatus {
            ready: status.ready,
            yt_dlp_path: status.yt_dlp_path,
            ffmpeg_dir: status.ffmpeg_dir,
            yt_dlp_version: status.yt_dlp_version,
            ffmpeg_version: status.ffmpeg_version,
            install_dir: status.install_dir,
            available: status
                .available
                .into_iter()
                .map(|tool| fetchpath_protocol::model::AvailableTool {
                    name: tool.name,
                    version: tool.version,
                    url: tool.url,
                    checksum_source: tool.checksum_source,
                    license: tool.license,
                    pinned: tool.pinned,
                })
                .collect(),
            problem: status.problem,
        }
    }

    /// Readiness and install location of `yt-dlp` and `ffmpeg`.
    pub fn media_tools_status(
        &self,
    ) -> Result<fetchpath_protocol::model::MediaToolsStatus, String> {
        let install_dir = self.media_tools_install_dir()?;
        let configured = self.settings().media_tools_dir;
        Ok(Self::map_tools_status(fetchpath_media::setup::status(
            configured.as_deref(),
            &install_dir,
        )))
    }

    /// Downloads and verifies the pinned media helpers into the engine's data
    /// folder, saves the folder in settings, and returns the updated status.
    pub fn install_media_tools(
        &self,
    ) -> Result<fetchpath_protocol::model::MediaToolsStatus, String> {
        let install_dir = self.media_tools_install_dir()?;
        fetchpath_media::setup::install(&install_dir)?;
        let mut next = self.settings();
        next.media_tools_dir = Some(install_dir.display().to_string());
        self.update_settings(next)?;
        self.media_tools_status()
    }

    /// Validates a user-chosen folder holding `yt-dlp` and `ffmpeg`, saves it
    /// in settings, and returns the updated status.
    pub fn use_media_tools_dir(
        &self,
        directory: &str,
    ) -> Result<fetchpath_protocol::model::MediaToolsStatus, String> {
        if directory.len() > MAX_DESTINATION_LENGTH {
            return Err("That folder path is too long.".into());
        }
        let accepted = fetchpath_media::setup::use_directory(&PathBuf::from(directory))?;
        let mut next = self.settings();
        next.media_tools_dir = Some(accepted);
        self.update_settings(next)?;
        self.media_tools_status()
    }

    /// Applies a settings change, clamping it first and re-resolving anything
    /// the change affects.
    pub fn update_settings(&self, mut next: Settings) -> Result<Settings, String> {
        next.clamp();
        let tools_dir_changed = {
            let current = self.settings.lock().expect("settings poisoned");
            current.media_tools_dir != next.media_tools_dir
        };
        if tools_dir_changed {
            *self.media_tools.lock().expect("media tools poisoned") =
                discover_media_tools(next.media_tools_dir.as_deref());
        }
        let quota_lowered = next.cache_quota_bytes < self.settings().cache_quota_bytes;
        *self.settings.lock().expect("settings poisoned") = next.clone();
        if quota_lowered && let Ok(mut cache) = self.open_cache() {
            let _ = cache.trim();
        }
        if let Some(path) = self.settings_path.as_ref() {
            settings::save(path, &next).map_err(|error| {
                format!("Could not save settings at {}: {error}", path.display())
            })?;
        }
        // A higher concurrency takes effect immediately rather than at the next
        // poll, so raising the limit visibly starts the next waiting download.
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let _ = self.save_locked(&mut state);
        Ok(next)
    }

    pub fn enqueue(&self, drafts: Vec<JobDraft>) -> Result<Vec<JobSnapshot>, String> {
        self.enqueue_for(drafts, &Origin::default())
    }

    pub(crate) fn enqueue_for(
        &self,
        drafts: Vec<JobDraft>,
        origin: &Origin,
    ) -> Result<Vec<JobSnapshot>, String> {
        if drafts.is_empty() {
            return Err("Add at least one download address.".into());
        }
        if drafts.len() > 100 {
            return Err("A batch can contain at most 100 downloads.".into());
        }
        // A checksum describes exactly one file.
        if drafts.len() > 1
            && drafts
                .iter()
                .any(|draft| has_checksum(draft.checksum.as_deref()))
        {
            return Err(
                "A checksum describes one file. Add links with a checksum one at a time.".into(),
            );
        }
        // Every link is checked before any is queued, so a batch that fails
        // on one link queues none of them (finding F4).
        let mut destinations = HashSet::new();
        let mut batch_directory: Option<PathBuf> = None;
        let mut accepted = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let url = validated_source(&draft.url)?;
            let destination = validated_destination(&draft.destination)?;
            let directory = destination
                .parent()
                .ok_or_else(|| "Choose a destination folder for this download.".to_string())?
                .to_path_buf();
            match batch_directory.as_ref() {
                None => batch_directory = Some(directory),
                Some(expected) if expected == &directory => {}
                Some(_) => {
                    return Err(
                        "Every download in one batch has to be saved in the same folder.".into(),
                    );
                }
            }
            if !destinations.insert(destination.clone()) {
                return Err(format!(
                    "The batch contains the destination {} more than once.",
                    destination.display()
                ));
            }
            let checksum = match draft.checksum.as_deref() {
                Some(text) => checked_checksum(text)?,
                None => None,
            };
            accepted.push((url, destination, draft.not_before_ms, checksum));
        }

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let mut created_ids = Vec::with_capacity(accepted.len());
        for (url, destination, not_before_ms, checksum) in accepted {
            let mut record = QueueRecord::new_checked(url, destination, not_before_ms, checksum);
            record.apply(origin);
            created_ids.push(record.id.clone());
            state.records.push(record);
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(state
            .records
            .iter()
            .filter(|record| created_ids.contains(&record.id))
            .map(|record| record.view.clone())
            .collect())
    }

    pub fn inspect_media(&self, source: &str) -> Result<MediaInspection, String> {
        let tools = self
            .media_tools()
            .ok_or_else(|| TOOLS_MISSING.to_string())?;
        let source = validated_source(source)?;
        tools.inspect(&source).map_err(|error| error.to_string())
    }

    /// Whether a link is a file, a media page or a web page, and what its
    /// server says about it. Known media sites are recognized without a
    /// request; anything else is asked for its headers.
    pub fn inspect_link(
        &self,
        source: &str,
    ) -> Result<fetchpath_protocol::model::LinkInspection, String> {
        use fetchpath_protocol::model::{LinkInspection, LinkKind};
        let source = validated_source(source)?;
        if is_media_page(&source) {
            return Ok(LinkInspection {
                kind: LinkKind::MediaPage,
                file_name: None,
                content_type: None,
                size_bytes: None,
                resumable: false,
                rules: Some(self.decide_rules(&source, None, None)),
            });
        }
        let facts = fetchpath_core::inspect_link(&source)?;
        if let Some(size) = facts.size {
            let mut sizes = self.inspected_sizes.lock().expect("sizes poisoned");
            sizes.retain(|(url, _)| *url != source);
            if sizes.len() == REMEMBERED_SIZES {
                sizes.pop_front();
            }
            sizes.push_back((source.clone(), size));
        }
        let rules = self.decide_rules(&source, facts.file_name.as_deref(), facts.size);
        Ok(LinkInspection {
            kind: if facts.is_web_page() {
                LinkKind::WebPage
            } else {
                LinkKind::File
            },
            file_name: facts.file_name,
            content_type: facts.content_type,
            size_bytes: facts.size,
            resumable: facts.resumable,
            rules: Some(rules),
        })
    }

    /// Which rule decides for a link, and why.
    pub fn decide_rules(
        &self,
        url: &str,
        file_name: Option<&str>,
        size: Option<u64>,
    ) -> fetchpath_protocol::model::RulesVerdict {
        let rules = self
            .settings
            .lock()
            .expect("settings poisoned")
            .rules
            .clone();
        rules::decide(&rules, &rules::Facts::new(url, file_name, size))
    }

    /// The size an inspection found for this exact link, if one did lately.
    fn inspected_size(&self, url: &str) -> Option<u64> {
        self.inspected_sizes
            .lock()
            .expect("sizes poisoned")
            .iter()
            .find(|(seen, _)| seen == url)
            .map(|(_, size)| *size)
    }

    /// Applies the rules to a new job before it is queued, for every
    /// principal: a `destination` that is only a file name goes into the
    /// matching rule's folder, or the default folder; a rule may require a
    /// checksum of a file download. Returns the full destination.
    pub fn apply_rules(
        &self,
        url: &str,
        destination: &str,
        checksum: Option<&str>,
        media: bool,
    ) -> Result<String, String> {
        let path = Path::new(destination.trim());
        let bare = path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)));
        let name = path.file_name().and_then(|name| name.to_str());
        let verdict = self.decide_rules(url, name, self.inspected_size(url));
        let matched = verdict.matched.as_ref();
        if let Some(rule) = matched
            && rule.spec.then.require_checksum
            && !media
            && !has_checksum(checksum)
        {
            return Err(format!(
                "integrity.checksum_required: {} requires a SHA-256 for this download. \
                 Add the checksum and try again.",
                rules::label(rule)
            ));
        }
        if !bare {
            return Ok(destination.to_owned());
        }
        let folder = matched
            .and_then(|rule| rule.spec.then.folder.clone())
            .map(PathBuf::from)
            .or_else(|| self.default_folder())
            .ok_or_else(|| "Choose a full destination path, including its drive.".to_owned())?;
        Ok(folder.join(path).display().to_string())
    }

    /// The destination of a new torrent after the person's rules, for every
    /// principal. A full path stays as it is. A bare folder name goes into the
    /// matching rule's folder or the default folder, like a file name does. An
    /// empty one means the engine names the folder from the torrent: the
    /// returned path is then the root and the flag is set. A checksum rule does
    /// not apply, since a torrent verifies itself against its metadata.
    pub fn resolve_torrent_destination(
        &self,
        source: Option<&str>,
        destination: &str,
    ) -> Result<(String, bool), String> {
        let trimmed = destination.trim();
        let path = Path::new(trimmed);
        let bare = path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)));
        if !trimmed.is_empty() && !bare {
            return Ok((destination.to_owned(), false));
        }
        let verdict = self.decide_rules(source.unwrap_or(""), None, None);
        let root = verdict
            .matched
            .and_then(|rule| rule.spec.then.folder)
            .map(PathBuf::from)
            .or_else(|| self.default_folder())
            .ok_or_else(|| "Choose a folder for this torrent.".to_owned())?;
        if trimmed.is_empty() {
            Ok((root.display().to_string(), true))
        } else {
            Ok((root.join(path).display().to_string(), false))
        }
    }

    /// Where a download goes when nothing else says: the setting, or the
    /// Downloads folder this engine was started with.
    pub fn default_folder(&self) -> Option<PathBuf> {
        self.settings
            .lock()
            .expect("settings poisoned")
            .default_destination_dir
            .clone()
            .map(PathBuf::from)
            .or_else(|| self.browser_download_dir.clone())
    }

    /// Where a signed-in device may save (contract D6): the default folder
    /// first, then each rule's folder once, labelled by the folder's name.
    pub fn folder_choices(&self) -> Vec<fetchpath_protocol::model::FolderChoice> {
        let mut choices: Vec<fetchpath_protocol::model::FolderChoice> = Vec::new();
        let folders = self.default_folder().into_iter().chain(
            self.rules()
                .into_iter()
                .filter_map(|rule| rule.spec.then.folder.map(PathBuf::from)),
        );
        for folder in folders {
            let path = folder.display().to_string();
            if choices.iter().any(|choice| choice.path == path) {
                continue;
            }
            let label = folder
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&path)
                .to_owned();
            choices.push(fetchpath_protocol::model::FolderChoice { label, path });
        }
        choices
    }

    pub fn rules(&self) -> Vec<fetchpath_protocol::model::Rule> {
        self.settings
            .lock()
            .expect("settings poisoned")
            .rules
            .clone()
    }

    /// Adds a rule last, or at `position` from 1, and saves it.
    pub fn add_rule(
        &self,
        spec: fetchpath_protocol::model::RuleSpec,
        position: Option<u32>,
    ) -> Result<Vec<fetchpath_protocol::model::Rule>, String> {
        let spec = rules::validate(spec)?;
        let mut next = self.settings();
        if next.rules.len() >= rules::MAX_RULES {
            return Err(format!("There can be at most {} rules.", rules::MAX_RULES));
        }
        let id = next.rules.iter().map(|rule| rule.id).max().unwrap_or(0) + 1;
        let at = position
            .map(|position| (position.max(1) as usize - 1).min(next.rules.len()))
            .unwrap_or(next.rules.len());
        next.rules
            .insert(at, fetchpath_protocol::model::Rule { id, spec });
        Ok(self.update_settings(next)?.rules)
    }

    pub fn remove_rule(&self, id: u32) -> Result<Vec<fetchpath_protocol::model::Rule>, String> {
        let mut next = self.settings();
        let before = next.rules.len();
        next.rules.retain(|rule| rule.id != id);
        if next.rules.len() == before {
            return Err(format!("There is no rule {id}."));
        }
        Ok(self.update_settings(next)?.rules)
    }

    pub fn enqueue_media(&self, draft: MediaDraft) -> Result<JobSnapshot, String> {
        self.enqueue_media_for(draft, &Origin::default())
    }

    pub(crate) fn enqueue_torrent_for(
        &self,
        draft: TorrentDraft,
        origin: &Origin,
    ) -> Result<JobSnapshot, String> {
        let source = validated_torrent_source(&draft.source)?;
        let destination = validated_torrent_destination(&draft.destination, draft.auto)?;
        if !draft.policy.discover_peers {
            return Err(
                "policy.discovery_off: a torrent cannot find peers without discovery.".into(),
            );
        }
        let mut record = QueueRecord::new_torrent(
            source,
            destination,
            draft.auto,
            draft.not_before_ms,
            draft.policy,
        );
        record.apply(origin);
        let id = record.id.clone();
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        state.records.push(record);
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, &id)?.view.clone())
    }

    pub(crate) fn enqueue_torrent_file_for(
        &self,
        source: &Path,
        destination: &str,
        auto: bool,
        not_before_ms: Option<u64>,
        policy: TorrentPolicy,
        origin: &Origin,
    ) -> Result<JobSnapshot, String> {
        if !policy.discover_peers {
            return Err(
                "policy.discovery_off: a torrent cannot find peers without discovery.".into(),
            );
        }
        if !source.is_absolute() {
            return Err("Choose a .torrent file by its full path.".into());
        }
        let state_path = self
            .state_path
            .as_ref()
            .ok_or("Local torrents need a saved engine queue.")?;
        let attributes =
            fs::symlink_metadata(source).map_err(|_| "That .torrent file could not be read.")?;
        if !attributes.file_type().is_file()
            || attributes.len() > MAX_TORRENT_METADATA_BYTES
            || !source
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().ends_with(".torrent"))
        {
            return Err("Choose a regular .torrent file of at most 4 MiB.".into());
        }
        let bytes = fs::read(source).map_err(|_| "That .torrent file could not be read.")?;
        if bytes.len() as u64 > MAX_TORRENT_METADATA_BYTES {
            return Err("Choose a .torrent file of at most 4 MiB.".into());
        }
        use sha2::{Digest, Sha256};
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let destination = validated_torrent_destination(destination, auto)?;
        let source_name = source.file_name().unwrap().to_string_lossy().into_owned();
        let synthetic_source = format!("local-torrent:{hash}");
        let mut record = QueueRecord::new_torrent(
            synthetic_source.clone(),
            destination,
            auto,
            not_before_ms,
            policy,
        );
        record.display_url = source_name.clone();
        record.view.source = source_name;
        record.restart_url = Some(synthetic_source.clone());
        let snapshot = torrent_metadata_path(state_path, &record.id)
            .ok_or("The torrent metadata path could not be made.")?;
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        fs::create_dir_all(snapshot.parent().unwrap()).map_err(|error| error.to_string())?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&snapshot)
            .map_err(|error| format!("Could not save torrent metadata: {error}"))?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&snapshot);
            return Err(format!("Could not save torrent metadata: {error}"));
        }
        drop(file);
        record.torrent_metadata_sha256 = Some(hash);
        record.torrent_metadata_path = Some(snapshot);
        if !record.destination_taken() {
            record.job = Some(torrent_job(&record, synthetic_source, policy));
        }
        record.apply(origin);
        let id = record.id.clone();
        state.records.push(record);
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, &id)?.view.clone())
    }

    pub(crate) fn enqueue_media_for(
        &self,
        draft: MediaDraft,
        origin: &Origin,
    ) -> Result<JobSnapshot, String> {
        let tools = self
            .media_tools()
            .ok_or_else(|| TOOLS_MISSING.to_string())?;
        let url = validated_source(&draft.url)?;
        let destination = validated_destination(&draft.destination)?;
        if draft.variant_id.len() > 512 || draft.quality_label.len() > 512 {
            return Err(
                "That quality selection could not be read. Inspect the source again.".into(),
            );
        }
        let inspection = tools.inspect(&url).map_err(|error| error.to_string())?;
        let selected = inspection
            .variants
            .iter()
            .find(|variant| variant.id == draft.variant_id)
            .ok_or_else(|| {
                "That quality is no longer available. Inspect the source again.".to_string()
            })?;
        if selected.label != draft.quality_label {
            return Err("That quality changed. Inspect the source again.".into());
        }
        let mut record = QueueRecord::new_media(
            url,
            destination,
            draft.not_before_ms,
            draft.variant_id,
            draft.quality_label,
            tools,
        );
        record.apply(origin);
        let id = record.id.clone();
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        state.records.push(record);
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, &id)?.view.clone())
    }

    pub fn list(&self) -> Result<Vec<JobSnapshot>, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let processed = self.ingest_browser_locked(&mut state)?;
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        let snapshots = state
            .records
            .iter()
            .rev()
            .map(|record| record.view.clone())
            .collect();
        drop(state);
        if let Some(store) = self.browser_store.as_ref() {
            for (capture_id, job_id) in processed {
                store
                    .mark_processed(&capture_id, &job_id)
                    .map_err(|error| format!("Browser capture was queued but its receipt could not be updated: {error}"))?;
            }
        }
        Ok(snapshots)
    }

    pub fn snapshot(&self, job_id: &str) -> Result<JobSnapshot, String> {
        self.list()?
            .into_iter()
            .find(|job| job.job_id == job_id)
            .ok_or_else(|| "This download is no longer available.".to_string())
    }

    pub fn details(&self, job_id: &str) -> Result<JobDetails, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let record = find_record(&state, job_id)?;
        // Only a running file download has ranges in flight; media reports none.
        let segments = match (&record.job, record.view.state.as_str()) {
            (Some(JobHandle::File(job)), "running") => job
                .segments()
                .into_iter()
                .map(|segment| SegmentView {
                    start: segment.start,
                    end: segment.end,
                    received: segment.received,
                })
                .collect(),
            _ => Vec::new(),
        };
        Ok(JobDetails {
            job: record.view.clone(),
            segments,
        })
    }

    pub fn cancel(&self, job_id: &str) -> Result<CancelResponse, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        // Cancelled while stopping at the disk reserve: it is not queued
        // again; it reads as what its worker reports.
        let stopping = state.space_stopped.remove(job_id);
        let record = find_record_mut(&mut state, job_id)?;
        // Its worker was already taken to be joined (or it was requeued
        // without one), so there is nothing to cancel but the wait itself:
        // end it here, or the requeue would start the file again.
        let held_for_space = record.job.is_none()
            && record.view.state == "queued"
            && (stopping || record.view.waiting_for_space);
        if stopping {
            record.view.waiting_for_space = false;
            refresh_record(record);
        }
        if held_for_space {
            record.view.waiting_for_space = false;
            record.view.state = "cancelled".into();
            record.view.error = None;
            record.view.action = None;
            record.view.retryable = false;
            record.finished_at_ms = Some(now_ms());
            record.view.finished_at_ms = record.finished_at_ms;
        }
        // Cancelling a job that waits for approval ends it; the person no
        // longer has anything to decide.
        let awaiting = record.awaiting_approval();
        if awaiting {
            if let Some(approval) = record.approval.as_mut() {
                approval.withdrawn = true;
            }
            if record.job.is_none() {
                record.view.state = "cancelled".into();
                record.finished_at_ms = Some(now_ms());
                record.view.finished_at_ms = record.finished_at_ms;
            }
        }
        let outcome = if let Some(job) = record.job.as_ref() {
            match job.cancel() {
                // A size stop already cancelled it; this cancel is the one
                // that counts.
                "already_terminal" if awaiting => "accepted",
                outcome => outcome,
            }
        } else if awaiting || held_for_space {
            "accepted"
        } else {
            "already_terminal"
        };
        refresh_record(record);
        self.save_locked(&mut state)?;
        Ok(CancelResponse {
            outcome,
            job: find_record(&state, job_id)?.view.clone(),
        })
    }

    pub fn start_now(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.view.state != "scheduled" && record.view.state != "queued" {
            return Err("Only queued or scheduled downloads can start now.".into());
        }
        record.not_before_ms = None;
        record.view.not_before_ms = None;
        record.view.state = "queued".into();
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    /// Moves a queued or scheduled download to a new start time.
    pub fn reschedule(&self, job_id: &str, not_before_ms: u64) -> Result<JobSnapshot, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.view.state != "scheduled" && record.view.state != "queued" {
            return Err("Only queued or scheduled downloads can be rescheduled.".into());
        }
        record.not_before_ms = Some(not_before_ms);
        record.view.not_before_ms = Some(not_before_ms);
        record.view.state = if not_before_ms > now_ms() {
            "scheduled".into()
        } else {
            "queued".into()
        };
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    pub fn retry(
        &self,
        job_id: &str,
        url: Option<String>,
        destination: Option<String>,
        checksum: Option<String>,
    ) -> Result<JobSnapshot, String> {
        self.retry_as(
            job_id,
            url,
            destination,
            checksum,
            &Principal::User,
            Vec::new(),
        )
    }

    /// Retries on behalf of `by`. Only the person may retry a download they
    /// declined; `hold` puts the retried job back in front of them.
    pub(crate) fn retry_as(
        &self,
        job_id: &str,
        url: Option<String>,
        destination: Option<String>,
        checksum: Option<String>,
        by: &Principal,
        hold: Vec<ApprovalReason>,
    ) -> Result<JobSnapshot, String> {
        let checksum = checksum.map(|text| checked_checksum(&text)).transpose()?;
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.awaiting_approval() {
            return Err("This download is waiting for the person to approve it.".into());
        }
        // Nothing about the approval changes until every check below has
        // passed, so a refused retry cannot shed a hold.
        let mut hold = hold;
        if let Some(approval) = record.approval.as_ref() {
            // A browser could be driven by an agent, so it never reopens a
            // request the person declined or left undecided (FP-104).
            if by.device().is_some() && (approval.denied || approval.withdrawn || approval.expired)
            {
                return Err(
                    "Retry this download in Fetchpath on this PC: it waited for approval.".into(),
                );
            }
            if approval.denied && !by.is_user() {
                return Err("The person declined this download.".into());
            }
            // An agent retrying what it withdrew, or what nobody decided on
            // in time, asks the person again.
            if (approval.withdrawn || approval.expired) && !by.is_user() {
                for reason in &approval.reasons {
                    if !hold.contains(reason) {
                        hold.push(*reason);
                    }
                }
            }
        }
        let url = url
            .map(|url| {
                if record.torrent_policy.is_some() {
                    validated_torrent_source(&url)
                } else {
                    validated_source(&url)
                }
            })
            .transpose()?;
        if url.is_some()
            && !by.is_user()
            && let Some(policy) = record.torrent_policy
        {
            let automatic =
                matches!(by, Principal::Agent(agent) if self.agent_policy(agent).automatic);
            if policy.discover_peers && !automatic && !hold.contains(&ApprovalReason::PeerDiscovery)
            {
                hold.push(ApprovalReason::PeerDiscovery);
            }
            if policy.upload && !hold.contains(&ApprovalReason::PeerUpload) {
                hold.push(ApprovalReason::PeerUpload);
            }
        }
        let destination = destination
            .map(|destination| validated_destination(&destination))
            .transpose()?;
        if url.is_some() && !by.is_user() {
            // A size approval was for the link the person saw. Dropped even
            // if the retry fails later, which only tightens the limit.
            record.size_approved = false;
        }
        if !matches!(
            record.view.state.as_str(),
            "failed" | "cancelled" | "needs_source"
        ) {
            return Err(
                "Only failed, cancelled, or source-expired downloads can be retried.".into(),
            );
        }
        if let Some(job) = record.job.take() {
            job.cancel();
            job.join();
        }
        let mut retired_metadata = None;
        if let Some(url) = url {
            if record.torrent_metadata_sha256.take().is_some() {
                retired_metadata = record.torrent_metadata_path.take().or_else(|| {
                    self.state_path
                        .as_ref()
                        .and_then(|path| torrent_metadata_path(path, &record.id))
                });
            }
            // A refreshed link may carry credentials the first one did not;
            // provenance only ever gets narrower (D8).
            record.public_origin = record.public_origin && !policy::has_userinfo(&url);
            record.display_url = display_url(&url);
            record.view.source = record.display_url.clone();
            record.restart_url = restartable_url(&url);
            record.live_url = Some(url);
            record.live_context = RequestContext::default();
            if let Some(credential_ref) = record.credential_ref.take()
                && let Some(store) = self.browser_store.as_ref()
            {
                let _ = store.remove_secret(&credential_ref);
            }
        }
        // An unchanged destination keeps an automatic torrent automatic.
        if let Some(destination) = destination
            && destination != record.destination
        {
            record.destination = destination;
            record.torrent_auto = false;
            // An approval outside the grants was for the place the person saw.
            record.folder_approved = false;
        }
        // An empty field clears the checksum; anything else replaces it.
        if let Some(checksum) = checksum {
            record.view.expected_sha256 = checksum;
        }
        let live_url = record.live_url.clone().ok_or_else(|| {
            "This source included private query values. Paste a refreshed link to continue."
                .to_string()
        })?;
        if record.destination_taken() {
            record.view.state = "failed".into();
            record.view.error = Some("A file already exists at this destination.".into());
            record.view.action = Some("choose_new_path".into());
            record.view.retryable = true;
        } else {
            record.job = Some(if let Some(policy) = record.torrent_policy {
                torrent_job(record, live_url, policy)
            } else if let Some(variant_id) = record.media_variant_id.clone() {
                let tools = self
                    .media_tools()
                    .ok_or_else(|| TOOLS_MISSING.to_string())?;
                JobHandle::Media(MediaJob::create(
                    live_url,
                    variant_id,
                    record.destination.clone(),
                    tools,
                ))
            } else {
                JobHandle::File(file_job(
                    live_url,
                    record.destination.clone(),
                    record.live_context.clone(),
                    record.view.expected_sha256.as_deref(),
                )?)
            });
            record.not_before_ms = None;
            record.finished_at_ms = None;
            record.view.state = "queued".into();
            record.view.bytes_received = 0;
            record.view.destination = Some(record.destination.display().to_string());
            record.view.observed_sha256 = None;
            record.view.cleanup_pending = false;
            record.view.error = None;
            record.view.action = None;
            record.view.retryable = false;
            record.view.not_before_ms = None;
            record.view.finished_at_ms = None;
        }
        // The retry went through: settle the approval.
        record.approval = None;
        if !hold.is_empty() {
            record.hold(hold);
        }
        if let Some(path) = retired_metadata {
            self.retired_torrent_metadata
                .lock()
                .expect("torrent cleanup poisoned")
                .push(path);
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    /// Stops a running download at its last checkpoint so it can continue
    /// later from that offset.
    ///
    /// Pausing races publication. Cancellation is refused once the engine has
    /// committed to publishing, and when that happens the download really did
    /// finish: this reports the completion instead of claiming a paused state
    /// for a file that is already on disk.
    pub fn pause(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let handle = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            self.reconcile_locked(&mut state);
            // Paused while stopping at the disk reserve: it is paused like a
            // running download, once its worker has stopped.
            let stopping = state.space_stopped.remove(job_id);
            let record = find_record_mut(&mut state, job_id)?;
            if stopping {
                record.view.waiting_for_space = false;
                // Its worker, if this pass has not taken it to join, is
                // stopped as a running one is; otherwise it pauses as queued.
                if record.job.is_some() {
                    record.view.state = "running".into();
                }
            }
            if record.view.kind != "file" {
                return Err(
                    "Only file downloads can be paused. Video and audio downloads have to be cancelled and started again."
                        .into(),
                );
            }
            match record.view.state.as_str() {
                // Not started yet: hold it out of the queue without disturbing
                // the prepared job, so resuming costs nothing.
                "queued" | "scheduled" => {
                    record.view.state = "paused".into();
                    record.view.error = None;
                    record.view.action = None;
                    record.view.retryable = false;
                    sample_rate(record);
                    let view = record.view.clone();
                    self.save_locked(&mut state)?;
                    return Ok(view);
                }
                "running" => record.job.clone(),
                "paused" => return Ok(record.view.clone()),
                other => {
                    return Err(format!("A download that is {other} cannot be paused."));
                }
            }
        };

        // Joining the worker must happen with the queue lock released; it waits
        // for a network read to unwind and would otherwise freeze every other
        // command, including the poll that draws the interface.
        let Some(handle) = handle else {
            return Err("This download is no longer running.".into());
        };
        handle.cancel();
        handle.join();

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        refresh_record(record);
        if record.view.state == "cancelled" {
            // The checkpoint was retained, so the bytes already verified stay
            // on disk and the next start resumes from that offset.
            record.job = None;
            record.view.state = "paused".into();
            record.view.error = None;
            record.view.action = None;
            record.view.retryable = false;
            record.finished_at_ms = None;
            record.view.finished_at_ms = None;
            sample_rate(record);
        }
        let view = record.view.clone();
        self.save_locked(&mut state)?;
        Ok(view)
    }

    /// Returns a paused download to the queue, continuing from its checkpoint.
    pub fn resume(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if record.view.state != "paused" {
            return Err("Only a paused download can be resumed.".into());
        }
        if record.job.is_none() {
            let url = record.live_url.clone().ok_or_else(|| {
                "This source included private query values. Paste a refreshed link to continue."
                    .to_string()
            })?;
            // A recoverable job validates the retained checkpoint against the
            // source before reusing a single byte of it, so a source that
            // changed while paused restarts instead of splicing.
            record.job = Some(JobHandle::File(file_job(
                url,
                record.destination.clone(),
                record.live_context.clone(),
                record.view.expected_sha256.as_deref(),
            )?));
        }
        record.view.state = "queued".into();
        record.view.error = None;
        record.view.action = None;
        record.view.retryable = false;
        record.retry_at_ms = None;
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    pub fn remove(&self, job_id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let index = state
            .records
            .iter()
            .position(|record| record.id == job_id)
            .ok_or_else(|| "This download is no longer available.".to_string())?;
        let record = state.records.remove(index);
        if record.durable.reported.is_some() {
            self.durable
                .lock()
                .expect("engine state poisoned")
                .removed
                .push(RemovedJob {
                    job_id: record.id.clone(),
                    job_revision: record.durable.job_revision,
                    last_seq: record.durable.last_seq,
                });
        }
        if let Some(credential_ref) = record.credential_ref.as_deref()
            && let Some(store) = self.browser_store.as_ref()
        {
            let _ = store.remove_secret(credential_ref);
        }
        if let Some(job) = record.job {
            job.cancel();
            job.join();
        }
        if record.torrent_metadata_sha256.is_some()
            && let Some(path) = self
                .state_path
                .as_ref()
                .and_then(|state_path| torrent_metadata_path(state_path, &record.id))
        {
            self.retired_torrent_metadata
                .lock()
                .expect("torrent cleanup poisoned")
                .push(path);
        }
        self.save_locked(&mut state)?;
        Ok(())
    }

    /// Lets a job that waits for approval run (contract D1). A size stop is
    /// joined first, so its checkpoint is quiet before anything reuses it,
    /// and the job then continues from that checkpoint.
    pub fn approve(&self, job_id: &str) -> Result<JobSnapshot, String> {
        let stopped = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            let record = find_record_mut(&mut state, job_id)?;
            if !record.awaiting_approval() {
                return Err("This download is not waiting for approval.".into());
            }
            record.job.take()
        };
        // Joining waits for a network read to unwind; never under the lock.
        let finished = self.join_stopped(stopped);

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let media_tools = self.media_tools();
        let record = find_record_mut(&mut state, job_id)?;
        if !record.awaiting_approval() {
            return Err("This download is not waiting for approval.".into());
        }
        if let Some(job) = finished {
            // The size stop came after publication: the download finished.
            record.job = Some(job);
            refresh_record(record);
            let view = record.view.clone();
            self.save_locked(&mut state)?;
            return Ok(view);
        }
        let reasons = record
            .approval
            .take()
            .map(|approval| approval.reasons)
            .unwrap_or_default();
        if reasons.contains(&ApprovalReason::SizeLimit) {
            record.size_approved = true;
        }
        if reasons.contains(&ApprovalReason::OutsideGrantedFolders) {
            record.folder_approved = true;
        }
        record.finished_at_ms = None;
        record.view.finished_at_ms = None;
        let prepared = match record.live_url.clone() {
            None => Err((
                "needs_source",
                "Paste a refreshed link because private query values were not saved.",
                "edit_link",
            )),
            Some(_) if record.destination_taken() => Err((
                "failed",
                "A file already exists at this destination.",
                "choose_new_path",
            )),
            Some(url) if record.torrent_policy.is_some() => Ok(torrent_job(
                record,
                url,
                record.torrent_policy.expect("checked above"),
            )),
            Some(url) => match record.media_variant_id.clone() {
                Some(variant_id) => media_tools
                    .map(|tools| {
                        JobHandle::Media(MediaJob::create(
                            url,
                            variant_id,
                            record.destination.clone(),
                            tools,
                        ))
                    })
                    .ok_or((
                        "failed",
                        "Media tools are unavailable. Configure them to retry this download.",
                        "configure_media_tools",
                    )),
                // A recoverable job validates any retained checkpoint against
                // the source before reusing a byte of it.
                None => file_job(
                    url,
                    record.destination.clone(),
                    record.live_context.clone(),
                    record.view.expected_sha256.as_deref(),
                )
                .map(JobHandle::File)
                .map_err(|_| ("failed", UNREADABLE_CHECKSUM, "check_checksum")),
            },
        };
        match prepared {
            Ok(job) => {
                record.job = Some(job);
                record.view.state = if record.not_before_ms.is_some_and(|due| due > now_ms()) {
                    "scheduled".into()
                } else {
                    "queued".into()
                };
                record.view.error = None;
                record.view.action = None;
                record.view.retryable = false;
            }
            Err((state_name, error, action)) => {
                record.view.state = state_name.into();
                record.view.error = Some(error.into());
                record.view.action = Some(action.into());
                record.view.retryable = true;
                if state_name == "failed" {
                    record.finished_at_ms = Some(now_ms());
                    record.view.finished_at_ms = record.finished_at_ms;
                }
            }
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)?;
        Ok(find_record(&state, job_id)?.view.clone())
    }

    /// Refuses a job that waits for approval. It ends cancelled, and its
    /// agent is told the person declined it.
    pub fn deny(&self, job_id: &str) -> Result<JobSnapshot, String> {
        self.decline(job_id, false)
    }

    /// Ends every request nobody decided on within the expiry period
    /// (contract D6), as a denial does. Run by the engine's own pass.
    pub fn expire_approvals(&self) {
        if self.read_only.is_some() {
            return;
        }
        let now = now_ms();
        let expired: Vec<String> = {
            let state = self.inner.lock().expect("desktop jobs poisoned");
            state
                .records
                .iter()
                .filter(|record| record.approval_expired_at(now))
                .map(|record| record.id.clone())
                .collect()
        };
        for job_id in expired {
            let _ = self.decline(&job_id, true);
        }
    }

    /// Ends a request: the person declined it, or nobody decided in time.
    fn decline(&self, job_id: &str, expired: bool) -> Result<JobSnapshot, String> {
        let stopped = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            let record = find_record_mut(&mut state, job_id)?;
            if !record.awaiting_approval() {
                return Err("This download is not waiting for approval.".into());
            }
            record.job.take()
        };
        let finished = self.join_stopped(stopped);
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record_mut(&mut state, job_id)?;
        if !record.awaiting_approval() {
            return Err("This download is not waiting for approval.".into());
        }
        if let Some(job) = finished {
            // Too late to decline: the size stop came after publication.
            record.job = Some(job);
            refresh_record(record);
            let view = record.view.clone();
            self.save_locked(&mut state)?;
            return Ok(view);
        }
        {
            if let Some(approval) = record.approval.as_mut() {
                if expired {
                    approval.expired = true;
                } else {
                    approval.denied = true;
                }
            }
            let now = now_ms();
            record.view.state = "cancelled".into();
            record.view.error = Some(if expired {
                "Nobody approved this download within 7 days.".into()
            } else {
                "The person declined this download.".into()
            });
            record.view.action = None;
            record.view.retryable = false;
            record.finished_at_ms = Some(now);
            record.view.finished_at_ms = Some(now);
            sample_rate(record);
        }
        let view = record.view.clone();
        self.save_locked(&mut state)?;
        Ok(view)
    }

    /// Cancels and joins a job taken from a record, returning it only when
    /// it had already published, so the caller reports that completion.
    fn join_stopped(&self, stopped: Option<JobHandle>) -> Option<JobHandle> {
        let job = stopped?;
        job.cancel();
        job.join();
        job.completed().then_some(job)
    }

    /// Where a job saves, for re-checking an agent's grants.
    pub(crate) fn destination_of(&self, job_id: &str) -> Option<PathBuf> {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        find_record(&state, job_id)
            .ok()
            .map(QueueRecord::grant_path)
    }

    /// Asks running file downloads on a drive below its disk reserve to stop
    /// at their checkpoint (FP-101). Only the stop is requested here; the
    /// engine's own pass joins it and returns the download to the queue
    /// ([`Session::requeue_space_stopped`]). Drives are measured at most
    /// every two seconds.
    fn stop_below_reserve(&self, state: &mut QueueState, settings: &Settings) {
        let now = now_ms();
        let last = self
            .space_checked_ms
            .load(std::sync::atomic::Ordering::SeqCst);
        if now.saturating_sub(last) < 2_000 {
            return;
        }
        self.space_checked_ms
            .store(now, std::sync::atomic::Ordering::SeqCst);
        let mut budget = self.space.budget(settings.disk_reserve_bytes);
        let QueueState {
            records,
            space_stopped,
            ..
        } = state;
        for record in records.iter_mut() {
            if record.view.state != "running" || space_stopped.contains(&record.id) {
                continue;
            }
            let Some(JobHandle::File(file)) = record.job.as_ref() else {
                continue;
            };
            if budget.below_reserve(&record.destination) {
                file.cancel();
                space_stopped.insert(record.id.clone());
                show_waiting_for_space(record);
            }
        }
    }

    /// Returns downloads stopped at the disk reserve to the queue once
    /// their worker has stopped, joining it outside the queue lock as a
    /// pause does. A download that finished first keeps its outcome. Run by
    /// the engine's own pass.
    pub fn requeue_space_stopped(&self) {
        let stopped: Vec<(String, JobHandle)> = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            let ids: Vec<String> = state.space_stopped.iter().cloned().collect();
            let mut taken = Vec::new();
            for id in ids {
                let Ok(record) = find_record_mut(&mut state, &id) else {
                    state.space_stopped.remove(&id);
                    continue;
                };
                refresh_record(record);
                let outcome = record.view.state.clone();
                match outcome.as_str() {
                    "running" | "cancelling" => show_waiting_for_space(record),
                    "cancelled" => {
                        show_waiting_for_space(record);
                        if let Some(job) = record.job.take() {
                            taken.push((id, job));
                        }
                    }
                    // Completed or failed before the stop took effect.
                    _ => {
                        record.view.waiting_for_space = false;
                        state.space_stopped.remove(&id);
                    }
                }
            }
            taken
        };
        if stopped.is_empty() {
            return;
        }
        for (_, job) in &stopped {
            job.join();
        }
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        for (id, job) in stopped {
            state.space_stopped.remove(&id);
            let Ok(record) = find_record_mut(&mut state, &id) else {
                continue;
            };
            if job.completed() {
                // Published before the stop reached it.
                record.job = Some(job);
                refresh_record(record);
                record.view.waiting_for_space = false;
                continue;
            }
            // The person paused or cancelled it meanwhile: that stands.
            if record.view.state != "queued" {
                record.view.waiting_for_space = false;
                continue;
            }
            // The checkpoint was retained; the next start resumes from it.
            record.view.state = "queued".into();
            record.view.waiting_for_space = true;
            record.view.error = None;
            record.view.action = None;
            record.view.retryable = false;
            record.finished_at_ms = None;
            record.view.finished_at_ms = None;
            sample_rate(record);
        }
        let _ = self.save_locked(&mut state);
    }

    /// Stops a running agent download whose stated or received size passed
    /// its agent's limit and holds it for approval. The stop is requested
    /// here and joined on approval, so the queue lock never waits on it.
    fn stop_oversize_agent_jobs(&self, state: &mut QueueState) {
        let agents = self.agents.lock().expect("agents poisoned").clone();
        for record in &mut state.records {
            let Principal::Agent(agent) = &record.principal else {
                continue;
            };
            // Automatic mode lifts size stops, including an adapter failure
            // sampled just after the person changed the policy (contract D7).
            if agents.get(agent).is_some_and(|policy| policy.automatic) {
                continue;
            }
            // A media download stopped by its byte cap has failed in the
            // adapter; for the person it is a size stop like any other.
            if record.approval.is_none()
                && record.view.state == "failed"
                && record
                    .view
                    .error
                    .as_deref()
                    .is_some_and(|error| error.starts_with("size_limit"))
            {
                record.retry_at_ms = None;
                record.hold(vec![ApprovalReason::SizeLimit]);
                continue;
            }
            if record.size_approved || record.approval.is_some() || record.view.state != "running" {
                continue;
            }
            let limit = agents
                .get(agent)
                .map_or(AgentPolicy::DEFAULT_MAX_BYTES, |policy| policy.max_bytes);
            let over = record.view.total_bytes.is_some_and(|total| total > limit)
                || record.view.bytes_received > limit;
            if !over {
                continue;
            }
            if let Some(job) = record.job.as_ref() {
                job.cancel();
            }
            record.hold(vec![ApprovalReason::SizeLimit]);
        }
    }

    /// Jobs of `principal` waiting for the person.
    pub(crate) fn pending_approvals(&self, principal: &Principal) -> usize {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        state
            .records
            .iter()
            .filter(|record| &record.principal == principal && record.awaiting_approval())
            .count()
    }

    /// The finished file of a completed download and what may be done with
    /// it (contract D8). `None` while the job is anything but completed.
    pub(crate) fn published_output(&self, job_id: &str) -> Option<PublishedOutput> {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record(&state, job_id).ok()?;
        // A stored address with userinfo is never public, even in a queue
        // saved before the refresh path narrowed `public_origin`, and the
        // origin shown never carries it.
        let userinfo = record
            .restart_url
            .as_deref()
            .is_some_and(policy::has_userinfo);
        (record.view.state == "completed").then(|| PublishedOutput {
            path: record.destination.clone(),
            owner: record.principal.clone(),
            public_origin: record.public_origin && !userinfo,
            origin: record
                .restart_url
                .as_deref()
                .map(redact_userinfo)
                .or_else(|| record.display_url.split('?').next().map(str::to_owned)),
        })
    }

    /// Marks the output of a finished credentialed job, so the marking
    /// travels with the bytes (contract D8, FP-121). Order: the hash set is
    /// saved, then the stream is written (also when the save failed, so the
    /// file still carries a marker), and only a saved hash, or a torrent's
    /// best-effort pass, sets the row's flag. Until then the live row still
    /// refuses by path. Returns whether any marker holds for the output.
    fn mark_credentialed_output(&self, record: &mut QueueRecord) -> bool {
        let destination = std::fs::canonicalize(&record.destination)
            .unwrap_or_else(|_| record.destination.clone());
        let Ok(files) = provenance::files_under(&destination) else {
            return false;
        };
        if files.len() == 1 && files[0] == destination {
            let saved = record
                .view
                .observed_sha256
                .as_deref()
                .and_then(normalize_sha256)
                .or_else(|| provenance::hash_file(&destination).ok())
                .is_some_and(|hash| {
                    self.provenance
                        .lock()
                        .expect("provenance poisoned")
                        .remember(&hash)
                        .is_ok()
                });
            let streamed = provenance::mark_if_needed(&destination);
            record.provenance_marked = saved;
            return saved || streamed;
        }
        // A torrent's folder has no single hash, so each file carries the
        // stream. One pass: a volume that refuses streams would otherwise be
        // retried at every reconcile. The path stays refused through the row.
        // Every file is tried, not just until one succeeds.
        let marked: Vec<bool> = files
            .iter()
            .map(|file| provenance::mark_if_needed(file))
            .collect();
        let streamed = marked.contains(&true);
        record.provenance_marked = true;
        streamed
    }

    /// Makes sure a finished credentialed job's output carries a marker
    /// before its row is removed (contract D8, FP-121). A job that is public,
    /// not finished, or already marked needs nothing.
    pub(crate) fn ensure_output_marked(&self, job_id: &str) -> Result<(), String> {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let Some(record) = state.records.iter_mut().find(|record| record.id == job_id) else {
            return Ok(());
        };
        if record.view.state != "completed"
            || record.provenance_marked
            || (record.public_origin
                && !record
                    .restart_url
                    .as_deref()
                    .is_some_and(policy::has_userinfo))
        {
            return Ok(());
        }
        if self.mark_credentialed_output(record) {
            Ok(())
        } else {
            Err("This download's file could not be marked as coming from a login, so it was                  not removed from the list. Try again."
                .into())
        }
    }

    /// True when these bytes are those of a credentialed output (contract D8,
    /// FP-121), whatever the file is now called or where it sits.
    pub(crate) fn is_credentialed_content(&self, sha256: &str) -> bool {
        self.provenance
            .lock()
            .expect("provenance poisoned")
            .contains(sha256)
    }

    /// False when the record of credentialed content could not be read, so
    /// the absence of a match proves nothing.
    pub(crate) fn provenance_readable(&self) -> bool {
        !self
            .provenance
            .lock()
            .expect("provenance poisoned")
            .unreadable()
    }

    /// The stream on the file, checked before the file is read.
    pub(crate) fn file_marker(&self, path: &Path) -> provenance::Marker {
        provenance::read_marker(path)
    }

    /// True when a download whose output is `path`, or the folder holding it
    /// (a torrent), was not credential-free, or is of a kind whose origin was
    /// never recorded as public. A file no download of this engine made is not
    /// credentialed by this test.
    pub(crate) fn is_credentialed_output(&self, path: &Path) -> bool {
        let Ok(path) = std::fs::canonicalize(path) else {
            return false;
        };
        let state = self.inner.lock().expect("desktop jobs poisoned");
        state.records.iter().any(|record| {
            !record.public_origin
                && std::fs::canonicalize(&record.destination)
                    .is_ok_and(|made| path.starts_with(made))
        })
    }

    /// Where a finished job that was not public wrote, so the engine can keep
    /// refusing that output after the job's row is removed. A job that did not
    /// finish wrote nothing there.
    pub(crate) fn credentialed_destination(&self, job_id: &str) -> Option<PathBuf> {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        let record = find_record(&state, job_id).ok()?;
        (!record.public_origin && record.view.state == "completed").then(|| {
            std::fs::canonicalize(&record.destination)
                .unwrap_or_else(|_| record.destination.clone())
        })
    }

    /// The folder holding the queue and the other engine files.
    pub(crate) fn data_dir(&self) -> Option<PathBuf> {
        self.state_path
            .as_ref()
            .and_then(|path| path.parent())
            .map(Path::to_path_buf)
    }

    /// Who created a job, if it exists.
    pub(crate) fn principal_of(&self, job_id: &str) -> Option<Principal> {
        let state = self.inner.lock().expect("desktop jobs poisoned");
        find_record(&state, job_id)
            .ok()
            .map(|record| record.principal.clone())
    }

    /// An agent's policy, or the default for an agent the person has not
    /// configured.
    pub fn agent_policy(&self, agent: &AgentName) -> AgentPolicy {
        self.agents
            .lock()
            .expect("agents poisoned")
            .get(agent)
            .cloned()
            .unwrap_or_default()
    }

    pub fn agent_policies(&self) -> Vec<fetchpath_protocol::principal::AgentAccess> {
        self.agents
            .lock()
            .expect("agents poisoned")
            .iter()
            .map(
                |(agent, policy)| fetchpath_protocol::principal::AgentAccess {
                    agent: agent.clone(),
                    policy: policy.clone(),
                },
            )
            .collect()
    }

    /// Sets or removes one agent's access and saves every agent's. Its
    /// unfinished downloads that now fall outside its folders wait for the
    /// person again, a running one stopped at its checkpoint; so revoking an
    /// agent stops everything it started from continuing unapproved.
    pub fn set_agent_policy(
        &self,
        agent: AgentName,
        next: Option<AgentPolicy>,
    ) -> Result<(), String> {
        let next = next.map(policy::validated).transpose()?;
        let previous = self.agent_policy(&agent);
        let next_policy = next.clone().unwrap_or_default();
        let cap_changed = previous.automatic != next_policy.automatic
            || previous.max_bytes != next_policy.max_bytes;
        let folders = next
            .as_ref()
            .map(|policy| policy.folders.clone())
            .unwrap_or_default();
        {
            let mut agents = self.agents.lock().expect("agents poisoned");
            let mut updated = agents.clone();
            match next {
                Some(policy) => updated.insert(agent.clone(), policy),
                None => updated.remove(&agent),
            };
            if let Some(path) = self.agents_path.as_ref() {
                write_json_atomically(path, &policy::AgentsFile::new(updated.clone())).map_err(
                    |error| format!("Could not save agent access at {}: {error}", path.display()),
                )?;
            }
            *agents = updated;
        }
        self.hold_outside_grants(&agent, &folders)?;
        if cap_changed {
            self.restart_agent_helpers(&agent)?;
        }
        Ok(())
    }

    /// A torrent helper copies its cap at start. Stop and rebuild helpers
    /// when that cap changes; media can also have crossed its previous cap
    /// before the policy update. Joining happens outside the queue lock.
    fn restart_agent_helpers(&self, agent: &AgentName) -> Result<(), String> {
        let access = self.agent_policy(agent);
        let stopped = {
            let mut state = self.inner.lock().expect("desktop jobs poisoned");
            let mut stopped = Vec::new();
            for record in &mut state.records {
                // A sampler may have held the old cap just before the policy
                // update. Lift that size reason, retaining every other hold.
                if access.automatic
                    && policy::inside_grants(&record.grant_path(), &access.folders)
                    && record.principal == Principal::Agent(agent.clone())
                    && matches!(
                        record.job,
                        Some(JobHandle::Media(_) | JobHandle::Torrent(_))
                    )
                    && let Some(approval) = record.approval.as_mut()
                {
                    approval
                        .reasons
                        .retain(|reason| *reason != ApprovalReason::SizeLimit);
                    if approval.reasons.is_empty() {
                        record.approval = None;
                        record.view.state = "running".into();
                    }
                }
                if record.principal != Principal::Agent(agent.clone())
                    || record.size_approved
                    || record.approval.is_some()
                    || !matches!(
                        record.job,
                        Some(JobHandle::Media(_) | JobHandle::Torrent(_))
                    )
                {
                    continue;
                }
                // Include a helper which failed at the old cap but has not
                // yet been sampled into an approval hold.
                let cap_failure = record.job.as_ref().is_some_and(|job| {
                    match job {
                        JobHandle::Media(job) => job.snapshot().error,
                        JobHandle::Torrent(job) => job.snapshot().error,
                        JobHandle::File(_) => None,
                    }
                    .is_some_and(|error| error.starts_with("size_limit"))
                });
                if record.view.state != "running" && !cap_failure {
                    continue;
                }
                let job = record.job.take().expect("helper present");
                job.cancel();
                record.view.state = "queued".into();
                record.view.error = None;
                record.view.action = None;
                record.view.retryable = false;
                record.retry_at_ms = None;
                stopped.push((record.id.clone(), job));
            }
            stopped
        };
        for (_, job) in &stopped {
            job.join();
        }
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        for (id, job) in stopped {
            let Ok(record) = find_record_mut(&mut state, &id) else {
                continue;
            };
            if job.completed() {
                record.job = Some(job);
                refresh_record(record);
                continue;
            }
            // Another command may have cancelled, held or removed the row
            // while its helper stopped. Preserve that command's decision.
            if record.view.state != "queued" || record.job.is_some() || record.approval.is_some() {
                continue;
            }
            let Some(url) = record.live_url.clone() else {
                record.view.state = "needs_source".into();
                continue;
            };
            record.job = if let Some(policy) = record.torrent_policy {
                Some(torrent_job(record, url, policy))
            } else if let Some(variant) = record.media_variant_id.clone() {
                self.media_tools().map(|tools| {
                    JobHandle::Media(MediaJob::create(
                        url,
                        variant,
                        record.destination.clone(),
                        tools,
                    ))
                })
            } else {
                None
            };
            record.finished_at_ms = None;
            record.view.finished_at_ms = None;
        }
        self.reconcile_locked(&mut state);
        self.save_locked(&mut state)
    }

    /// Holds `agent`'s unfinished downloads whose destination is outside
    /// `folders`. The stop of a running one is requested here and joined on
    /// approval, as a size stop is, so the queue lock never waits on it; a
    /// download that published first reports its completion then.
    fn hold_outside_grants(&self, agent: &AgentName, folders: &[String]) -> Result<(), String> {
        let principal = Principal::Agent(agent.clone());
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        let mut held = false;
        for record in &mut state.records {
            // A failed one counts while an automatic retry would start it.
            let unfinished = match record.view.state.as_str() {
                "queued" | "scheduled" | "running" | "paused" | "needs_source" => true,
                "failed" => record.retry_at_ms.is_some(),
                _ => false,
            };
            if record.principal != principal
                || record.approval.is_some()
                || !unfinished
                || policy::inside_grants(&record.grant_path(), folders)
            {
                continue;
            }
            if let Some(job) = record.job.as_ref() {
                job.cancel();
            }
            record.retry_at_ms = None;
            record.hold(vec![ApprovalReason::OutsideGrantedFolders]);
            held = true;
        }
        if held {
            self.save_locked(&mut state)?;
        }
        Ok(())
    }

    pub fn cancel_all_and_join(&self) {
        let jobs: Vec<(String, JobHandle)> = {
            let state = self.inner.lock().expect("desktop jobs poisoned");
            state
                .records
                .iter()
                .filter(|record| {
                    matches!(record.view.state.as_str(), "running" | "cancelling")
                        // A size stop may still be writing its checkpoint.
                        || (record.awaiting_approval() && record.job.is_some())
                })
                .filter_map(|record| {
                    record
                        .job
                        .as_ref()
                        .cloned()
                        .map(|job| (record.id.clone(), job))
                })
                .collect()
        };
        for (_, job) in &jobs {
            job.cancel();
        }
        for (_, job) in &jobs {
            job.join();
        }

        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        for (id, _) in jobs {
            let Ok(record) = find_record_mut(&mut state, &id) else {
                continue;
            };
            if record.awaiting_approval() {
                // Quiet now; approval prepares a fresh job from the checkpoint.
                if !record.job.as_ref().is_some_and(JobHandle::completed) {
                    record.job = None;
                    continue;
                }
            }
            refresh_record(record);
            if record.view.state == "cancelled"
                && let Some(url) = record.live_url.clone()
            {
                record.job = if let Some(policy) = record.torrent_policy {
                    Some(torrent_job(record, url, policy))
                } else if let Some(variant_id) = record.media_variant_id.clone() {
                    self.media_tools().map(|tools| {
                        JobHandle::Media(MediaJob::create(
                            url,
                            variant_id,
                            record.destination.clone(),
                            tools,
                        ))
                    })
                } else {
                    file_job(
                        url,
                        record.destination.clone(),
                        record.live_context.clone(),
                        record.view.expected_sha256.as_deref(),
                    )
                    .ok()
                    .map(JobHandle::File)
                };
                record.view.state = if record.not_before_ms.is_some_and(|due| due > now_ms()) {
                    "scheduled".into()
                } else {
                    "queued".into()
                };
                record.view.error = Some("Ready to recover after Fetchpath restarts.".into());
                record.view.action = None;
                record.view.retryable = false;
                record.finished_at_ms = None;
                record.view.finished_at_ms = None;
            }
        }
        let _ = self.save_locked(&mut state);
    }

    fn reconcile_locked(&self, state: &mut QueueState) {
        if self.read_only.is_some() {
            // Nothing runs, is retried, or has its secrets removed.
            return;
        }
        let settings = self.settings();
        let max_active = settings.max_active_downloads;
        for record in &mut state.records {
            refresh_record(record);
            if record.view.state == "completed"
                && !record.provenance_marked
                && !(record.public_origin
                    && !record
                        .restart_url
                        .as_deref()
                        .is_some_and(policy::has_userinfo))
            {
                self.mark_credentialed_output(record);
            }
            if record.view.state == "completed"
                && let Some(credential_ref) = record.credential_ref.take()
                && let Some(store) = self.browser_store.as_ref()
            {
                let _ = store.remove_secret(&credential_ref);
            }
        }
        // A download being stopped at the disk reserve reads as waiting for
        // space from the moment it is asked to stop, never as cancelled.
        for record in &mut state.records {
            if state.space_stopped.contains(&record.id) {
                show_waiting_for_space(record);
            }
        }
        self.stop_below_reserve(state, &settings);
        self.stop_oversize_agent_jobs(state);
        if settings.auto_retry {
            self.schedule_automatic_retries(state, &settings);
        }
        let mut active = state
            .records
            .iter()
            .filter(|record| matches!(record.view.state.as_str(), "running" | "cancelling"))
            .count();
        let now = now_ms();
        let halted = self.halted.load(std::sync::atomic::Ordering::SeqCst);
        // What running downloads still have to write, by destination, for
        // the disk reserve; drives are measured only if something starts.
        let running: Vec<(PathBuf, u64)> = state
            .records
            .iter()
            .filter(|record| record.view.state == "running")
            .map(|record| (record.destination.clone(), remaining_bytes(&record.view)))
            .collect();
        let mut budget: Option<space::Budget> = None;
        let QueueState {
            records,
            space_stopped,
            ..
        } = state;
        for record in records.iter_mut() {
            if active >= max_active || halted {
                break;
            }
            // Its worker is still stopping; it is queued again once joined.
            if space_stopped.contains(&record.id) {
                continue;
            }
            if record.view.state == "scheduled" && record.not_before_ms.is_none_or(|due| due <= now)
            {
                record.view.state = "queued".into();
            }
            if record.view.state != "queued" || record.not_before_ms.is_some_and(|due| due > now) {
                continue;
            }
            if record.job.is_none() && !record.view.waiting_for_space {
                continue;
            }
            // Folder grants are checked again as the job starts: a junction
            // can be pointed elsewhere between the request and now, and the
            // agent's grants may have changed. The person's approval stands.
            if let Principal::Agent(agent) = &record.principal
                && !record.folder_approved
                && !policy::inside_grants(&record.grant_path(), &self.agent_policy(agent).folders)
            {
                // Kept, cancelled: approving joins it before the job is rebuilt.
                if let Some(job) = record.job.as_ref() {
                    job.cancel();
                }
                record.view.waiting_for_space = false;
                record.hold(vec![ApprovalReason::OutsideGrantedFolders]);
                continue;
            }
            // The disk reserve (FP-101): a download starts only when its
            // drive has room for what it still needs above the reserve,
            // after what running downloads there still need. Otherwise it
            // waits in the queue; it never fails for it.
            let needed = record
                .view
                .total_bytes
                .or_else(|| {
                    let url = record.live_url.as_deref().unwrap_or(&record.display_url);
                    self.inspected_size(url)
                })
                .map(|total| total.saturating_sub(record.view.bytes_received));
            let budget = budget.get_or_insert_with(|| {
                let mut budget = self.space.budget(settings.disk_reserve_bytes);
                for (destination, remaining) in &running {
                    budget.running(destination, *remaining);
                }
                budget
            });
            if !budget.admit(&record.destination, needed, record.view.waiting_for_space) {
                record.view.waiting_for_space = true;
                continue;
            }
            // Stopped at the reserve: continue from the checkpoint, as a
            // resume does, now that there is room.
            if record.job.is_none() {
                let Some(url) = record.live_url.clone() else {
                    record.view.waiting_for_space = false;
                    record.view.state = "needs_source".into();
                    record.view.error = Some(
                        "Paste a refreshed link because private query values were not saved."
                            .into(),
                    );
                    record.view.action = Some("edit_link".into());
                    continue;
                };
                match file_job(
                    url,
                    record.destination.clone(),
                    record.live_context.clone(),
                    record.view.expected_sha256.as_deref(),
                ) {
                    Ok(job) => record.job = Some(JobHandle::File(job)),
                    Err(message) => {
                        record.view.waiting_for_space = false;
                        record.view.state = "failed".into();
                        record.view.error = Some(message);
                        record.view.action = Some("retry".into());
                        record.view.retryable = true;
                        continue;
                    }
                }
            }
            record.view.waiting_for_space = false;
            let Some(job) = record.job.as_ref() else {
                continue;
            };
            // An agent's video or audio download is capped at its size limit
            // inside the adapter, since a helper can write faster than the
            // engine samples (FP-067). The person's approval lifts it.
            // Automatic mode lifts the cap (contract D7).
            let cap = match &record.principal {
                Principal::Agent(agent) if !record.size_approved => {
                    let agents = self.agents.lock().expect("agents poisoned");
                    match agents.get(agent) {
                        Some(policy) if policy.automatic => None,
                        policy => Some(
                            policy
                                .map_or(AgentPolicy::DEFAULT_MAX_BYTES, |policy| policy.max_bytes),
                        ),
                    }
                }
                _ => None,
            };
            if let (JobHandle::Media(media), Some(limit)) = (job, cap) {
                media.limit_bytes(limit);
            }
            if let (JobHandle::Torrent(torrent), Some(limit)) = (job, cap) {
                torrent.limit_bytes(limit);
            }
            if let JobHandle::File(file) = job {
                // A rule may cap the connections, decided as the job starts.
                let url = record.live_url.as_deref().unwrap_or(&record.display_url);
                let name = record
                    .destination
                    .file_name()
                    .and_then(|name| name.to_str());
                let size = record.view.total_bytes.or_else(|| self.inspected_size(url));
                let verdict = rules::decide(&settings.rules, &rules::Facts::new(url, name, size));
                if let Some(connections) = verdict
                    .matched
                    .and_then(|rule| rule.spec.then.max_connections)
                {
                    let _ = file.limit_connections(connections as usize);
                }
                // A download into a folder that does not exist yet, such as
                // a repository's subfolder, gets it as it starts: after any
                // approval, never while it waits for one.
                if let Some(parent) = record.destination.parent()
                    && !parent.as_os_str().is_empty()
                {
                    let _ = fs::create_dir_all(parent);
                }
                if !matches!(record.principal, Principal::Agent(_))
                    && let Some(root) = self.cache_root.lock().expect("cache poisoned").clone()
                {
                    let _ = file.use_cache(root, Self::cache_config(&settings));
                }
                // Paired devices are asked for the person's and the browser's
                // checksum-verified downloads only, like the cache.
                if !matches!(record.principal, Principal::Agent(_))
                    && let Some(lan) = self.lan.get()
                {
                    let peers = lan.peer_sources();
                    if !peers.is_empty() {
                        let _ = file.use_peers(peers);
                    }
                }
            }
            if let Err(code) = job.start(Arc::clone(&self.wake)) {
                record.view.state = "failed".into();
                record.view.error = Some(format!("Could not start this download ({code})."));
                record.view.action = Some("retry".into());
                record.view.retryable = true;
                record.finished_at_ms = Some(now);
                record.view.finished_at_ms = record.finished_at_ms;
                continue;
            }
            refresh_record(record);
            active += 1;
        }
        for record in &mut state.records {
            record.view.error_code = failure_code(&record.view);
        }
    }

    /// Re-queues transport failures on a widening backoff.
    ///
    /// Only failures the engine classified as plain transport trouble qualify.
    /// A destination conflict, an invalid link, an expired private source or a
    /// missing helper all need a person to decide something, and retrying them
    /// on a timer would bury that decision under repeated identical failures.
    fn schedule_automatic_retries(&self, state: &mut QueueState, settings: &Settings) {
        let now = now_ms();
        for record in &mut state.records {
            let due = record.retry_at_ms.is_some_and(|due| due <= now);
            if due && record.view.state == "scheduled" {
                record.retry_at_ms = None;
                continue;
            }
            if record.view.state != "failed"
                || record.attempt >= settings.auto_retry_max_attempts
                || record.retry_at_ms.is_some()
            {
                continue;
            }
            // `recovery_action` returns "retry" only for failures with no
            // specific user action attached. Of those, only recognized codes
            // are retried by the queue itself (finding F2).
            if record.view.action.as_deref() != Some("retry")
                || !failure_code(&record.view).is_some_and(|code| retried_automatically(&code))
            {
                continue;
            }
            let Some(url) = record.live_url.clone() else {
                continue;
            };
            if record.destination_taken() {
                continue;
            }
            record.attempt += 1;
            let delay = settings.retry_delay_seconds(record.attempt);
            let due_at = now.saturating_add(delay.saturating_mul(1_000));
            record.job = if let Some(policy) = record.torrent_policy {
                Some(torrent_job(record, url, policy))
            } else if let Some(variant_id) = record.media_variant_id.clone() {
                let Some(tools) = self.media_tools() else {
                    record.attempt -= 1;
                    continue;
                };
                Some(JobHandle::Media(MediaJob::create(
                    url,
                    variant_id,
                    record.destination.clone(),
                    tools,
                )))
            } else {
                file_job(
                    url,
                    record.destination.clone(),
                    record.live_context.clone(),
                    record.view.expected_sha256.as_deref(),
                )
                .ok()
                .map(JobHandle::File)
            };
            record.not_before_ms = Some(due_at);
            record.retry_at_ms = Some(due_at);
            record.finished_at_ms = None;
            record.view.state = "scheduled".into();
            record.view.not_before_ms = Some(due_at);
            record.view.finished_at_ms = None;
            record.view.attempt = record.attempt;
            record.view.action = None;
            record.view.retryable = false;
            record.view.error = Some(format!(
                "Retrying automatically (attempt {} of {}).",
                record.attempt, settings.auto_retry_max_attempts
            ));
        }
    }

    /// Stops starting jobs, for an engine that is shutting down. Running
    /// ones are then cancelled to their checkpoints by `cancel_all_and_join`
    /// and nothing takes their place.
    pub fn halt(&self) {
        self.halted.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(lan) = self.lan.get() {
            lan.cancel_pairing();
            lan.stop_serving();
            lan.stop_listening();
        }
    }

    /// True while a job is queued, scheduled (including an automatic retry)
    /// or running: work that happens without a person. The engine stays up
    /// for it (FP-053).
    pub fn has_own_work(&self) -> bool {
        // A newer build's queue is only shown; nothing in it is waiting on us.
        if self.read_only.is_some() {
            return false;
        }
        let stats = self.stats();
        stats.running + stats.queued + stats.scheduled > 0
            || self.lan.get().is_some_and(lan::Lan::busy)
    }

    /// Aggregate figures for the statistics panel.
    pub fn stats(&self) -> QueueStats {
        let mut state = self.inner.lock().expect("desktop jobs poisoned");
        self.reconcile_locked(&mut state);
        let mut stats = QueueStats::default();
        for record in &state.records {
            let view = &record.view;
            match view.state.as_str() {
                "running" => stats.running += 1,
                "queued" => stats.queued += 1,
                "scheduled" => stats.scheduled += 1,
                "paused" => stats.paused += 1,
                "completed" => {
                    stats.completed += 1;
                    stats.completed_bytes =
                        stats.completed_bytes.saturating_add(view.bytes_received);
                }
                "failed" | "needs_source" => stats.failed += 1,
                "awaiting_approval" => stats.awaiting_approval += 1,
                _ => {}
            }
            if view.state == "running" {
                stats.active_bytes = stats.active_bytes.saturating_add(view.bytes_received);
                if let Some(rate) = view.bytes_per_second {
                    stats.combined_bytes_per_second =
                        stats.combined_bytes_per_second.saturating_add(rate);
                }
            }
        }
        stats.max_active_downloads = self.max_active();
        stats
    }

    /// Commits the queue: derives the durable events of every change since
    /// the last commit and writes records, events and ledger in one save.
    /// While a ledgered command runs it only marks the state dirty, and the
    /// command's own commit writes everything (see [`engine`]).
    fn save_locked(&self, state: &mut QueueState) -> Result<(), String> {
        if self.read_only.is_some() {
            // Nothing changed that is ours to save.
            return Ok(());
        }
        let mut durable = self.durable.lock().expect("engine state poisoned");
        if durable.defer {
            durable.dirty = true;
            return Ok(());
        }
        durable.derive_events(&mut state.records, now_ms());
        durable.prune_ledger(now_ms());
        self.write_locked(state, &mut durable)
    }

    /// Writes the queue and engine state, then hands the committed events to
    /// subscribers. Events are never shown before they are on disk.
    ///
    /// Ordering: the engine file (ledger and events, tagged with the new
    /// generation) is written first and the queue file, which names that
    /// generation, second. The queue file's rename is the commit point: until
    /// it happens, a load discards the new generation's entries.
    fn write_locked(&self, state: &QueueState, durable: &mut Durable) -> Result<(), String> {
        // The last line of defense: a newer build's queue is never written
        // over, whatever path got here.
        if let Some(reason) = &self.read_only {
            return Err(reason.clone());
        }
        if let Some(path) = self.state_path.as_ref() {
            if durable.engine_changed {
                let mut engine = durable.engine.clone();
                engine.schema_version = ENGINE_SCHEMA_VERSION;
                engine.generation = durable.pending_generation();
                let engine_path = path.with_file_name(ENGINE_FILE);
                write_json_atomically(&engine_path, &engine).map_err(|error| {
                    format!(
                        "Could not save the engine journal at {}: {error}",
                        engine_path.display()
                    )
                })?;
                durable.engine.generation = engine.generation;
                durable.engine_changed = false;
            }
            write_queue(
                path,
                state,
                durable.engine.generation,
                durable.engine.cursor,
            )
            .map_err(|error| {
                format!(
                    "Could not save download history at {}: {error}",
                    path.display()
                )
            })?;
            let mut retired = self
                .retired_torrent_metadata
                .lock()
                .expect("torrent cleanup poisoned");
            if !retired.is_empty() {
                // The previous queue remains as a recovery backup. Do not
                // remove metadata it still names; the next successful save
                // rotates that reference away.
                let backup_refs = match fs::read(path.with_extension("json.bak")) {
                    Ok(bytes) => {
                        serde_json::from_slice::<PersistedQueue>(&bytes)
                            .ok()
                            .map(|queue| {
                                queue
                                    .records
                                    .into_iter()
                                    .filter(|record| record.torrent_metadata_sha256.is_some())
                                    .map(|record| record.id)
                                    .collect::<HashSet<_>>()
                            })
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Some(HashSet::new()),
                    Err(_) => None,
                };
                if let Some(backup_refs) = backup_refs {
                    retired.retain(|snapshot| {
                        let id = snapshot
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .unwrap_or_default();
                        if backup_refs.contains(id)
                            || state.records.iter().any(|record| {
                                record.id == id && record.torrent_metadata_sha256.is_some()
                            })
                        {
                            return true;
                        }
                        match fs::remove_file(snapshot) {
                            Ok(()) => false,
                            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                            Err(_) => true,
                        }
                    });
                }
            }
        } else if durable.engine_changed {
            // Nothing to write to: everything in memory counts as committed.
            durable.engine.generation = durable.pending_generation();
            durable.engine_changed = false;
        }
        // Publication recovery evidence outlives the helper. Only the queue
        // commit above makes the completed row and its final path durable.
        if self.state_path.is_some() {
            for record in &state.records {
                if record.view.state == "completed"
                    && let Some(JobHandle::Torrent(job)) = &record.job
                {
                    let _ = job.acknowledge_publication();
                }
            }
        }
        durable.committed = durable.engine.generation;
        durable.broadcast();
        Ok(())
    }

    fn ingest_browser_locked(
        &self,
        state: &mut QueueState,
    ) -> Result<Vec<(String, String)>, String> {
        let (Some(store), Some(download_dir), None) = (
            self.browser_store.as_ref(),
            self.browser_download_dir.as_ref(),
            self.read_only.as_ref(),
        ) else {
            // Read-only: captures stay in the inbox for the newer build.
            return Ok(Vec::new());
        };
        let pending = store
            .pending()
            .map_err(|error| format!("Could not read browser captures: {error}"))?;
        let mut processed = Vec::with_capacity(pending.len());
        for capture in pending {
            if let Some(existing) = state
                .records
                .iter()
                .find(|record| record.credential_ref.as_deref() == Some(&capture.credential_ref))
            {
                processed.push((capture.capture_id, existing.id.clone()));
                continue;
            }
            let secret = match store.load_secret(&capture.credential_ref) {
                Ok(secret) => secret,
                Err(_) => {
                    // One missing or unreadable secret must never poison
                    // Session::list() for the entire queue: quarantine it and
                    // continue ingesting the remaining captures.
                    let _ = store.mark_processed(&capture.capture_id, "quarantined");
                    let _ = store.remove_secret(&capture.credential_ref);
                    continue;
                }
            };
            // The person's rules decide as for any new job (FP-075).
            let verdict = self.decide_rules(&secret.url, Some(&capture.suggested_filename), None);
            let rule = verdict.matched.as_ref();
            // A page on a media site saved as a file would be its HTML, and a
            // file a rule requires a checksum for cannot start without one.
            // Both go to Add download instead, where the video is found or
            // the checksum is asked for. Their cookies are not carried over.
            if is_media_page(&secret.url)
                || rule.is_some_and(|rule| rule.spec.then.require_checksum)
            {
                if !state
                    .link_reviews
                    .iter()
                    .any(|review| review.capture_id == capture.capture_id)
                {
                    state.link_reviews.push(LinkReview {
                        capture_id: capture.capture_id,
                        credential_ref: capture.credential_ref,
                        url: secret.url,
                    });
                }
                continue;
            }
            let Ok(context) = RequestContext::new(secret.cookie_lines, secret.referer) else {
                let _ = store.mark_processed(&capture.capture_id, "quarantined");
                let _ = store.remove_secret(&capture.credential_ref);
                continue;
            };
            let folder = rule
                .and_then(|rule| rule.spec.then.folder.clone())
                .map(PathBuf::from)
                .or_else(|| self.default_folder())
                .unwrap_or_else(|| download_dir.clone());
            let destination =
                unique_browser_destination(&folder, &capture.suggested_filename, &state.records);
            let record = QueueRecord::new_with_context(
                secret.url,
                destination,
                None,
                Some(capture.credential_ref),
                context,
                None,
            );
            let job_id = record.id.clone();
            state.records.push(record);
            processed.push((capture.capture_id, job_id));
        }
        Ok(processed)
    }
}

/// Why a video or audio request cannot run yet. The code lets every client
/// offer its own way to set the helpers up.
const TOOLS_MISSING: &str = "media.helper_unavailable: Video and audio need yt-dlp and ffmpeg, \
     which are not set up. Set them up in Settings, Video and audio, or run `fetchpath tools install`.";

#[cfg(test)]
mod characterization;
#[cfg(test)]
mod tests;
