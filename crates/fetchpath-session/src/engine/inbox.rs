//! The same-machine inbox (contract D8, FP-115).
//!
//! An offer hands a file that already exists on this machine to another
//! principal of this engine. Offers live in their own small store beside the
//! queue: nothing here is a download, so the queue, its scheduler and its
//! states are untouched. The engine builds every manifest from the file
//! itself; a caller names only a source and a recipient.

use super::{Engine, Outcome, error, input, persistence, unknown_job};
use crate::policy;
use crate::provenance::Marker;
use fetchpath_protocol::command::Command;
use fetchpath_protocol::error::{Action, ErrorScope, ProtocolError};
use fetchpath_protocol::principal::Principal;
use fetchpath_protocol::send::{HashSource, Manifest, OfferSource, OfferState, OfferView};
use fetchpath_protocol::{JobId, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const FILE: &str = "inbox-v1.json";

/// The most the inbox file may hold. Offers are capped per sender and old
/// finished ones are dropped, so a real file stays far below this; a larger
/// one is not read.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// How many quarantined copies stay, so a file that keeps breaking cannot
/// fill the disk.
const KEEP_QUARANTINED: usize = 3;

/// How long an offer waits for its recipient (contract D8, as D6).
pub(crate) const OFFER_EXPIRY_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// A finished offer is dropped from the store after this long, so the file
/// does not grow without bound.
const KEEP_FINISHED_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    id: String,
    state: OfferState,
    manifest: Manifest,
    sender: Principal,
    recipient: Principal,
    /// Never shown to any client.
    source: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// When the offer last changed, for dropping old finished ones.
    #[serde(default)]
    changed_ms: i64,
    /// The commands (client, id and request) that changed this offer, oldest
    /// first. Saved with the offer, so a resend after a crash between this
    /// file and the engine ledger finds its own effect instead of applying it
    /// twice, even when another party has acted on the offer since.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    commands: Vec<String>,
}

/// Keys kept per offer: its creation and the few answers an offer can take.
const KEEP_COMMANDS: usize = 8;

impl Stored {
    fn remember(&mut self, key: &str) {
        self.commands.push(key.to_owned());
        if self.commands.len() > KEEP_COMMANDS {
            // The creating command stays: it is the one a crash can orphan.
            self.commands.remove(1);
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    version: u32,
    offers: Vec<Stored>,
    /// Outputs of downloads that were not credential-free, kept after their
    /// row is removed so an unknown origin never turns public (D8).
    #[serde(default)]
    credentialed: Vec<PathBuf>,
}

#[derive(Default)]
pub(crate) struct Inbox {
    path: Option<PathBuf>,
    offers: Vec<Stored>,
    credentialed: Vec<PathBuf>,
    /// Set when the file could not be used and the inbox started empty.
    notice: Option<String>,
}

/// Reads at most `MAX_BYTES`; a larger file is refused without being read.
fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the inbox file is too large",
        ));
    }
    Ok(bytes)
}

fn parse(bytes: &[u8]) -> Result<Persisted, String> {
    let saved: Persisted = serde_json::from_slice(bytes).map_err(|failure| failure.to_string())?;
    // A later build's file is not this build's to read or to overwrite.
    if saved.version != 1 {
        return Err(format!("unsupported version {}", saved.version));
    }
    Ok(saved)
}

/// Moves a file this build cannot use aside, so the next save does not
/// replace it, and drops the oldest copies beyond `KEEP_QUARANTINED`.
fn quarantine(path: &Path) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let mut target = path.with_extension(format!("json.corrupt-{stamp}"));
    let mut attempt = 0;
    while target.exists() {
        attempt += 1;
        target = path.with_extension(format!("json.corrupt-{stamp}-{attempt}"));
    }
    std::fs::rename(path, &target)?;
    if let Some((dir, name)) = path.parent().zip(path.file_name()) {
        let prefix = format!("{}.corrupt-", name.to_string_lossy());
        let mut copies: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|found| {
                found
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
            })
            .collect();
        // Oldest first by the stamp in the name (the file's own modified
        // time survives a rename), and never the copy just made.
        let stamp_of = |found: &PathBuf| {
            let tail = found.file_name().map(|n| n.to_string_lossy().into_owned());
            let tail = tail.as_deref().and_then(|n| n.strip_prefix(&prefix));
            let mut parts = tail.unwrap_or_default().splitn(2, '-');
            let number = |part: Option<&str>| part.and_then(|p| p.parse::<u128>().ok());
            (number(parts.next()), number(parts.next()))
        };
        copies.sort_by_key(stamp_of);
        let surplus = copies.len().saturating_sub(KEEP_QUARANTINED);
        for old in copies
            .into_iter()
            .take(surplus)
            .filter(|old| *old != target)
        {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(target)
}

impl Inbox {
    /// A missing file is an empty inbox. A file that cannot be read, is too
    /// large or does not parse is quarantined, never deleted, and the inbox
    /// starts empty with a notice: a damaged file must not block startup.
    pub(crate) fn load(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|dir| dir.join(FILE));
        let mut notice = None;
        let mut path = path;
        let saved = match path.as_deref().map(|path| (path, read_bounded(path))) {
            None => Persisted::default(),
            Some((_, Ok(bytes))) => match parse(&bytes) {
                Ok(saved) => saved,
                Err(reason) => {
                    let (message, kept) = Self::set_aside(path.as_deref().unwrap(), &reason);
                    notice = Some(message);
                    if !kept {
                        path = None;
                    }
                    Persisted::default()
                }
            },
            Some((_, Err(failure))) if failure.kind() == std::io::ErrorKind::NotFound => {
                Persisted::default()
            }
            Some((file, Err(failure))) if failure.kind() == std::io::ErrorKind::InvalidData => {
                let (message, kept) = Self::set_aside(file, &failure.to_string());
                notice = Some(message);
                if !kept {
                    path = None;
                }
                Persisted::default()
            }
            // The file may be fine and only busy: it is neither moved nor
            // replaced, and this run keeps the inbox in memory.
            Some((_, Err(failure))) => {
                notice = Some(format!(
                    "The inbox file could not be read ({failure}). The inbox started empty and will not be saved this run; the file was left as it is. Offers and the credentialed-output list it held are not in effect."
                ));
                path = None;
                Persisted::default()
            }
        };
        let (mut offers, credentialed) = (saved.offers, saved.credentialed);
        // A copy that a restart cut short did not happen.
        for offer in &mut offers {
            if offer.state == OfferState::Copying {
                offer.state = OfferState::Offered;
                // Its accept did not happen either: a resend applies it.
                offer.commands.pop();
            }
        }
        Self {
            path,
            offers,
            credentialed,
            notice,
        }
    }

    /// Moves an unusable file aside. The bool is whether saving may go on:
    /// when the file cannot be moved it is never replaced.
    fn set_aside(path: &Path, reason: &str) -> (String, bool) {
        match quarantine(path) {
            Ok(moved) => (
                format!(
                    "The inbox file could not be used ({reason}). It was moved to {} and the inbox started empty. Offers and the credentialed-output list it held are not in effect.",
                    moved.display()
                ),
                true,
            ),
            Err(failure) => (
                format!(
                    "The inbox file could not be used ({reason}) and could not be moved aside ({failure}). The inbox started empty and will not be saved this run; the file was left as it is."
                ),
                false,
            ),
        }
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&Persisted {
            version: 1,
            offers: self.offers.clone(),
            credentialed: self.credentialed.clone(),
        })
        .map_err(|failure| failure.to_string())?;
        let temporary = path.with_extension("tmp");
        // Flushed before the rename, so a crash leaves the old file or the
        // whole new one, never a renamed empty one.
        let write = std::fs::File::create(&temporary).and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        });
        write
            .and_then(|()| std::fs::rename(&temporary, path))
            .map_err(|failure| format!("The inbox could not be saved: {failure}"))
    }

    /// Keeps refusing a removed download's output (and a torrent's folder).
    pub(crate) fn remember_credentialed(&mut self, output: PathBuf) -> Result<(), String> {
        if !self.credentialed.contains(&output) {
            self.credentialed.push(output);
            // Memory never holds what the disk does not, or a retry would
            // find it already there and skip the save.
            if let Err(failure) = self.save() {
                self.credentialed.pop();
                return Err(failure);
            }
        }
        Ok(())
    }

    fn is_remembered_credentialed(&self, canonical: &Path) -> bool {
        self.credentialed
            .iter()
            .any(|made| canonical.starts_with(made))
    }

    fn open_from(&self, sender: &Principal) -> usize {
        self.offers
            .iter()
            .filter(|offer| &offer.sender == sender && offer.state.is_open())
            .count()
    }

    /// Marks offers nobody answered in time, and forgets old finished ones.
    fn expire(&mut self, now_ms: i64) -> bool {
        let mut changed = false;
        for offer in &mut self.offers {
            if offer.state == OfferState::Offered
                && now_ms - offer.manifest.offered_at.unix_ms() > OFFER_EXPIRY_MS
            {
                offer.state = OfferState::Expired;
                offer.reason = Some("send.offer_expired".into());
                offer.changed_ms = now_ms;
                changed = true;
            }
        }
        let before = self.offers.len();
        self.offers.retain(|offer| {
            offer.state.is_open() || now_ms - offer.changed_ms.max(0) < KEEP_FINISHED_MS
        });
        changed || before != self.offers.len()
    }

    fn can_see(offer: &Stored, viewer: &Principal) -> bool {
        viewer.is_person() || &offer.sender == viewer || &offer.recipient == viewer
    }

    fn view(offer: &Stored, viewer: &Principal) -> OfferView {
        // Where the copy went is the recipient's business, or the person's.
        let saved_to = (viewer.is_person() || &offer.recipient == viewer)
            .then(|| offer.saved_to.clone())
            .flatten();
        OfferView {
            job_id: JobId::try_from(offer.id.as_str()).expect("an offer id is a UUID"),
            state: offer.state,
            manifest: offer.manifest.clone(),
            sender: offer.sender.clone(),
            recipient: offer.recipient.clone(),
            saved_to,
            reason: offer.reason.clone(),
        }
    }

    /// Saves the offers; if that fails the memory returns to `before`, so it
    /// never holds a change the disk lacks.
    fn save_or_restore(&mut self, before: Vec<Stored>) -> Result<(), String> {
        let saved = self.save();
        if saved.is_err() {
            self.offers = before;
        }
        saved
    }

    /// The offer this exact command already produced or changed, if any.
    fn done_by(&self, key: &str) -> Option<&Stored> {
        self.offers
            .iter()
            .find(|offer| offer.commands.iter().any(|done| done == key))
    }

    fn find_mut(&mut self, id: &str, viewer: &Principal) -> Option<&mut Stored> {
        self.offers
            .iter_mut()
            .find(|offer| offer.id == id && Self::can_see(offer, viewer))
    }
}

fn refusal(code: &'static str, message: &str, action: Action) -> ProtocolError {
    error(code, ErrorScope::Command, message).with_action(action)
}

/// One answer for every path that is not offerable, so a refusal never says
/// whether the path exists.
fn not_offerable() -> ProtocolError {
    refusal(
        "send.not_offerable",
        "That file cannot be offered. An agent can offer a file it downloaded or a file inside \
         a folder it was given.",
        Action::CorrectInput,
    )
}

fn recipient_not_allowed() -> ProtocolError {
    refusal(
        "send.recipient_not_allowed",
        "An agent can offer a file to the person only, and the person to one agent.",
        Action::CorrectInput,
    )
}

fn credentialed() -> ProtocolError {
    refusal(
        "policy.credentialed_not_allowed",
        "This file came from a download that used a login, cookies or a referer, so it is not \
         offered to anyone else.",
        Action::ChangePolicy,
    )
}

fn offer_closed() -> ProtocolError {
    refusal(
        "contract.invalid_transition",
        "This offer is no longer open.",
        Action::RefreshClient,
    )
}

fn unknown_offer() -> ProtocolError {
    let mut failure = unknown_job();
    failure.message = "This offer is no longer available.".into();
    failure
}

fn expired() -> ProtocolError {
    refusal(
        "send.offer_expired",
        "This offer expired. Ask the sender to offer the file again.",
        Action::RefreshClient,
    )
}

/// Engine files and staging files are never offered (contract D8).
fn staged(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.starts_with(".fetchpath-") || name.ends_with(".part") || name.contains(".checkpoint")
}

/// A network share path. Resolving one makes Windows connect to, and sign in
/// at, another computer, so it is refused before the filesystem is touched.
fn is_share(path: &Path) -> bool {
    // Windows reads `/` as `\`, so `/\host\share` is a share too.
    let text = path.to_string_lossy().replace('/', "\\");
    text.starts_with("\\\\")
}

/// A name or folder of an unfinished download (contract D8: never offered).
fn inside_staging(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        name.starts_with(".fetchpath-") || name.ends_with(".part")
    })
}

/// Hashes the file, reading at most `limit` bytes: a file that grows past it
/// is refused instead of holding the engine's command lock.
fn hash_file(
    file: &mut std::fs::File,
    limit: Option<u64>,
) -> std::io::Result<Option<(u64, String)>> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if limit.is_some_and(|limit| size > limit) {
            return Ok(None);
        }
        digest.update(&buffer[..read]);
    }
    Ok(Some((size, format!("{:x}", digest.finalize()))))
}

/// Where a source file may be read from. The path is only a request: what is
/// read is the file the opened handle names, checked after it is open.
struct Boundary {
    /// The engine's own data folder, canonical. Never readable.
    data: Option<PathBuf>,
    /// The folders an agent may read from; `None` for the person.
    grants: Option<Vec<String>>,
}

impl Boundary {
    fn admits(&self, canonical: &Path) -> bool {
        !(staged(canonical)
            || inside_staging(canonical)
            || self
                .data
                .as_ref()
                .is_some_and(|data| canonical.starts_with(data))
            || self
                .grants
                .as_ref()
                .is_some_and(|folders| !policy::inside_grants(canonical, folders)))
    }
}

#[derive(Debug)]
enum Unreadable {
    Missing,
    Outside,
}

/// The canonical path of what an open handle names, with links and junctions
/// resolved. `None` when the system cannot say, which refuses the file.
#[cfg(windows)]
fn handle_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let handle = file.as_raw_handle();
    let mut buffer = vec![0u16; 1024];
    loop {
        // SAFETY: `handle` stays open for the call and `buffer` is writable
        // for its stated length.
        let length = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        match length {
            0 => return None,
            n if n < buffer.len() => {
                return Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..n])));
            }
            n => buffer.resize(n + 1, 0),
        }
    }
}

#[cfg(unix)]
fn handle_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
}

/// True while the handle still names `canonical`. A handle follows a rename,
/// so a file moved away and replaced at its path by a decoy is caught here.
fn still_at(file: &std::fs::File, canonical: &Path) -> bool {
    handle_path(file).is_some_and(|now| now == canonical)
}

/// Opens `path` once and checks the handle it got: a regular file whose
/// canonical path is inside the boundary. Everything after reads this handle,
/// so a path swapped for a link or another file after this point changes
/// nothing, and one swapped before it is seen here.
fn open_source(path: &Path, boundary: &Boundary) -> Result<(std::fs::File, PathBuf), Unreadable> {
    let unreadable = |failure: std::io::Error| match failure.kind() {
        std::io::ErrorKind::NotFound => Unreadable::Missing,
        _ => Unreadable::Outside,
    };
    // Before opening, so a pipe or device cannot block the open itself.
    if !std::fs::metadata(path).map_err(unreadable)?.is_file() {
        return Err(Unreadable::Outside);
    }
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let canonical = handle_path(&file).ok_or(Unreadable::Outside)?;
    if !file.metadata().is_ok_and(|meta| meta.is_file()) || !boundary.admits(&canonical) {
        return Err(Unreadable::Outside);
    }
    Ok((file, canonical))
}

impl Engine {
    /// Called on every tick and before the inbox is listed.
    pub(super) fn expire_offers(&self) {
        let mut inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
        if inbox.expire(Timestamp::now().unix_ms()) {
            let _ = inbox.save();
        }
    }

    /// Who may offer, to whom, and what.
    pub(super) fn apply_inbox(
        &self,
        principal: &Principal,
        command: &Command,
        key: &str,
    ) -> Result<Outcome, ProtocolError> {
        self.expire_offers();
        if !matches!(command, Command::ListInbox) {
            // Saved before the ledger entry: a resend after a crash between
            // the two answers with the offer already on disk.
            let inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(offer) = inbox.done_by(key) {
                return Ok(Outcome::Offer(Box::new(Inbox::view(offer, principal))));
            }
        }
        match command {
            Command::OfferFile { source, recipient } => {
                self.offer_file(principal, source, recipient, key)
            }
            Command::ListInbox => {
                let inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
                let mut offers: Vec<OfferView> = inbox
                    .offers
                    .iter()
                    .filter(|offer| Inbox::can_see(offer, principal))
                    .map(|offer| Inbox::view(offer, principal))
                    .collect();
                offers.reverse();
                Ok(Outcome::Inbox(offers))
            }
            Command::AcceptOffer {
                offer_id,
                destination,
            } => self.accept_offer(principal, offer_id.as_str(), destination.as_deref(), key),
            Command::DeclineOffer { offer_id } => {
                self.answer_offer(principal, offer_id.as_str(), true, key)
            }
            Command::WithdrawOffer { offer_id } => {
                self.answer_offer(principal, offer_id.as_str(), false, key)
            }
            _ => unreachable!("only inbox commands reach the inbox"),
        }
    }

    fn offer_file(
        &self,
        principal: &Principal,
        source: &OfferSource,
        recipient: &Principal,
        key: &str,
    ) -> Result<Outcome, ProtocolError> {
        let allowed = match principal {
            Principal::User => matches!(recipient, Principal::Agent(_)),
            Principal::Agent(_) => matches!(recipient, Principal::User),
            _ => false,
        };
        if !allowed {
            return Err(recipient_not_allowed());
        }
        let agent = principal.agent();
        let access = agent.map(|agent| self.session.agent_policy(agent));

        // Which file, and where it came from.
        let (path, origin, from_job) = match source {
            OfferSource::Job { job_id } => {
                let output = self
                    .session
                    .published_output(job_id.as_str())
                    .ok_or_else(not_offerable)?;
                if agent.is_some() && &output.owner != principal {
                    return Err(not_offerable());
                }
                if !output.public_origin {
                    return Err(credentialed());
                }
                if is_share(&output.path) {
                    return Err(not_offerable());
                }
                (output.path, output.origin, true)
            }
            OfferSource::Path { path } => {
                let path = PathBuf::from(path.trim());
                if !path.is_absolute() || is_share(&path) || crate::plain_path(&path).is_err() {
                    return Err(not_offerable());
                }
                (path, None, false)
            }
        };
        if inside_staging(&path) {
            return Err(not_offerable());
        }
        let boundary = Boundary {
            data: self
                .session
                .data_dir()
                .and_then(|dir| std::fs::canonicalize(dir).ok()),
            grants: access.as_ref().map(|access| access.folders.clone()),
        };
        let (mut file, canonical) = open_source(&path, &boundary).map_err(|why| match why {
            Unreadable::Missing if from_job => refusal(
                "send.source_missing",
                "The downloaded file is no longer there.",
                Action::RefreshClient,
            ),
            _ => not_offerable(),
        })?;
        // A file picked by path may be the output of a credentialed download:
        // by its row or remembered path, by the stream it carries, or (below,
        // once the bytes are hashed) by its content.
        if !from_job {
            let remembered = self
                .inbox
                .lock()
                .expect("inbox poisoned")
                .is_remembered_credentialed(&canonical);
            if self.session.is_credentialed_output(&canonical) || remembered {
                return Err(credentialed());
            }
            match self.session.file_marker(&canonical) {
                Marker::Credentialed => return Err(credentialed()),
                // Unknown for an agent fails closed; the person picked the
                // file themselves.
                Marker::Unknown if agent.is_some() => return Err(not_offerable()),
                Marker::Unknown | Marker::Absent => {}
            }
        }
        let file_name = canonical
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .filter(|name| !name.chars().any(|c| c.is_control() || spoofs_direction(c)))
            // The name that goes in the manifest is the canonical one: an
            // 8.3 short name expands to it, so the caller's spelling proves
            // nothing.
            .filter(|name| crate::plain_path(&Path::new("C:\\").join(name)).is_ok())
            .ok_or_else(not_offerable)?;

        // The agent's limits (contract D1).
        if let (Some(agent), Some(access)) = (agent, &access) {
            let open = self
                .inbox
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .open_from(principal);
            let rate_exceeded = self
                .rates
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .exceeded(agent, access.max_new_jobs_per_hour, super::now_ms());
            if open >= policy::MAX_PENDING_APPROVALS || rate_exceeded {
                return Err(policy::too_many_pending());
            }
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            if size > access.max_bytes {
                return Err(policy::size_limit());
            }
        }

        let limit = access.as_ref().map(|access| access.max_bytes);
        let (size, sha256) = match hash_file(&mut file, limit) {
            Ok(Some(hashed)) => hashed,
            Ok(None) => return Err(policy::size_limit()),
            Err(_) => return Err(not_offerable()),
        };
        // The marker above was read by path: it counts only if the opened
        // file was still there while it was read and hashed.
        if !still_at(&file, &canonical) {
            return Err(not_offerable());
        }
        if !from_job {
            if self.session.is_credentialed_content(&sha256) {
                return Err(credentialed());
            }
            if agent.is_some() && !self.session.provenance_readable() {
                return Err(not_offerable());
            }
        }
        let offered_at = Timestamp::now();
        let stored = Stored {
            id: uuid::Uuid::new_v4().to_string(),
            state: OfferState::Offered,
            manifest: Manifest {
                file_name,
                size,
                sha256,
                hash_source: HashSource::SenderEngine,
                origin,
                sender_label: String::from(principal.clone()),
                offered_at,
            },
            sender: principal.clone(),
            recipient: recipient.clone(),
            source: canonical,
            saved_to: None,
            reason: None,
            changed_ms: offered_at.unix_ms(),
            commands: vec![key.to_owned()],
        };
        let view = Inbox::view(&stored, principal);
        let mut inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
        let before = inbox.offers.clone();
        inbox.offers.push(stored);
        inbox.save_or_restore(before).map_err(persistence)?;
        drop(inbox);
        self.created(principal);
        Ok(Outcome::Offer(Box::new(view)))
    }

    /// Decline (the recipient) or withdraw (the sender) an open offer.
    fn answer_offer(
        &self,
        principal: &Principal,
        id: &str,
        decline: bool,
        key: &str,
    ) -> Result<Outcome, ProtocolError> {
        let mut inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
        let before = inbox.offers.clone();
        let offer = inbox.find_mut(id, principal).ok_or_else(unknown_offer)?;
        let party = if decline {
            &offer.recipient
        } else {
            &offer.sender
        };
        if party != principal {
            return Err(unknown_offer());
        }
        if offer.state != OfferState::Offered {
            return Err(offer_closed());
        }
        if decline {
            offer.state = OfferState::Declined;
            offer.reason = Some("send.offer_declined".into());
        } else {
            offer.state = OfferState::Withdrawn;
        }
        offer.changed_ms = Timestamp::now().unix_ms();
        offer.remember(key);
        let view = Inbox::view(offer, principal);
        inbox.save_or_restore(before).map_err(persistence)?;
        Ok(Outcome::Offer(Box::new(view)))
    }

    fn accept_offer(
        &self,
        principal: &Principal,
        id: &str,
        destination: Option<&str>,
        key: &str,
    ) -> Result<Outcome, ProtocolError> {
        let mut inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
        let before = inbox.offers.clone();
        let offer = inbox.find_mut(id, principal).ok_or_else(unknown_offer)?;
        if &offer.recipient != principal {
            return Err(unknown_offer());
        }
        if offer.state == OfferState::Expired {
            return Err(expired());
        }
        if offer.state != OfferState::Offered {
            return Err(offer_closed());
        }

        // A folder, never a replacement.
        let access = principal
            .agent()
            .map(|agent| self.session.agent_policy(agent));
        // The recipient's limit as it is now, not when the offer was made.
        if let Some(access) = &access
            && offer.manifest.size > access.max_bytes
        {
            return Err(policy::size_limit());
        }
        let limit = access.as_ref().map(|access| access.max_bytes);
        let folder = match destination.map(str::trim).filter(|text| !text.is_empty()) {
            Some(text) => PathBuf::from(text),
            None => match &access {
                Some(access) => access.folders.first().map(PathBuf::from).ok_or_else(|| {
                    input("Choose a folder inside the folders you were given.".into())
                })?,
                None => self
                    .session
                    .settings()
                    .default_destination_dir
                    .map(PathBuf::from)
                    .ok_or_else(|| input("Choose a folder to save the file in.".into()))?,
            },
        };
        if access.is_some() && is_share(&folder) {
            return Err(policy::outside_grants());
        }
        let target = folder.join(&offer.manifest.file_name);
        crate::validated_destination(&target.to_string_lossy()).map_err(input)?;
        if let Some(access) = &access
            && !policy::inside_grants(&target, &access.folders)
        {
            return Err(policy::outside_grants());
        }
        if !folder.is_dir() {
            return Err(input("That folder does not exist.".into()));
        }
        if target.exists() {
            return Err(refusal(
                "send.destination_exists",
                "A file with that name is already in that folder. Choose another folder.",
                Action::CorrectInput,
            ));
        }

        offer.state = OfferState::Copying;
        offer.changed_ms = Timestamp::now().unix_ms();
        offer.remember(key);
        let view = Inbox::view(offer, principal);
        let job = (
            offer.id.clone(),
            offer.source.clone(),
            offer.manifest.clone(),
        );
        // The sender's reach as it is when the copy starts, not when offered.
        let boundary = Boundary {
            data: self
                .session
                .data_dir()
                .and_then(|dir| std::fs::canonicalize(dir).ok()),
            grants: offer
                .sender
                .agent()
                .map(|agent| self.session.agent_policy(agent).folders),
        };
        inbox.save_or_restore(before).map_err(persistence)?;
        drop(inbox);

        let shared = Arc::clone(&self.inbox);
        std::thread::spawn(move || copy_offer(&shared, job, &boundary, &target, limit));
        Ok(Outcome::Offer(Box::new(view)))
    }
}

/// Copies the offered file beside its destination, checks it still matches
/// the manifest, and publishes it without replacing anything.
fn copy_offer(
    shared: &Arc<Mutex<Inbox>>,
    (id, source, manifest): (String, PathBuf, Manifest),
    boundary: &Boundary,
    target: &Path,
    limit: Option<u64>,
) {
    let outcome = copy_checked(&source, &manifest, boundary, target, limit);
    let mut inbox = shared.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(offer) = inbox.offers.iter_mut().find(|offer| offer.id == id) {
        offer.changed_ms = Timestamp::now().unix_ms();
        match outcome {
            Ok(()) => {
                offer.state = OfferState::Completed;
                offer.saved_to = Some(target.display().to_string());
            }
            Err(reason) => {
                offer.state = OfferState::Failed;
                offer.reason = Some(reason.into());
            }
        }
    }
    let _ = inbox.save();
}

fn copy_checked(
    source: &Path,
    manifest: &Manifest,
    boundary: &Boundary,
    target: &Path,
    limit: Option<u64>,
) -> Result<(), &'static str> {
    let folder = target.parent().ok_or("send.destination_unsupported")?;
    let (mut input, opened) = open_source(source, boundary).map_err(|why| match why {
        Unreadable::Missing => "send.source_missing",
        Unreadable::Outside => "send.changed",
    })?;
    // The offer named one file; a path that now leads elsewhere is another.
    if opened != source {
        return Err("send.changed");
    }
    let staging = folder.join(format!(
        ".fetchpath-send-{}.part",
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|_| "send.destination_unsupported")?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0u8; 256 * 1024];
        let mut size = 0u64;
        loop {
            let read = input.read(&mut buffer).map_err(|_| "send.source_missing")?;
            if read == 0 {
                break;
            }
            size += read as u64;
            // A source that grew past the recipient's limit since the offer.
            if limit.is_some_and(|limit| size > limit) {
                return Err("policy.size_limit");
            }
            digest.update(&buffer[..read]);
            output
                .write_all(&buffer[..read])
                .map_err(|_| "send.destination_unsupported")?;
        }
        output
            .sync_all()
            .map_err(|_| "send.destination_unsupported")?;
        if size != manifest.size || format!("{:x}", digest.finalize()) != manifest.sha256 {
            return Err("send.changed");
        }
        // Publication cannot replace a file that appeared meanwhile.
        std::fs::hard_link(&staging, target).map_err(|_| "send.destination_exists")
    })();
    let _ = std::fs::remove_file(&staging);
    result
}

/// Bidirectional controls and invisible marks that make a file name read as
/// something else (`invoice\u{202E}fdp.exe`).
fn spoofs_direction(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200B}' | '\u{200E}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(dir: &Path) -> PathBuf {
        let mut inbox = Inbox::load(Some(dir.to_path_buf()));
        inbox.remember_credentialed(PathBuf::from("kept")).unwrap();
        dir.join(FILE)
    }

    fn quarantined(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(".corrupt-"))
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn a_valid_file_loads_without_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        saved(dir.path());
        let inbox = Inbox::load(Some(dir.path().to_path_buf()));
        assert_eq!(inbox.credentialed, [PathBuf::from("kept")]);
        assert!(inbox.notice.is_none());
        assert!(quarantined(dir.path()).is_empty());
    }

    #[test]
    fn a_missing_file_starts_empty_without_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::load(Some(dir.path().to_path_buf()));
        assert!(inbox.notice.is_none() && inbox.offers.is_empty());
    }

    fn damaged_file_is_quarantined(bytes: Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE);
        std::fs::write(&path, &bytes).unwrap();
        let mut inbox = Inbox::load(Some(dir.path().to_path_buf()));
        assert!(inbox.offers.is_empty() && inbox.credentialed.is_empty());
        assert!(
            inbox
                .notice
                .as_deref()
                .is_some_and(|n| n.contains("corrupt-"))
        );
        let kept = quarantined(dir.path());
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read(&kept[0]).unwrap(), bytes, "bytes preserved");
        assert!(!path.exists());
        // The engine still works and the next save does not touch the copy.
        inbox.remember_credentialed(PathBuf::from("new")).unwrap();
        assert_eq!(std::fs::read(&kept[0]).unwrap(), bytes);
    }

    #[test]
    fn a_truncated_file_is_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = std::fs::read(saved(dir.path())).unwrap();
        damaged_file_is_quarantined(bytes[..bytes.len() / 2].to_vec());
    }

    #[test]
    fn a_non_utf8_file_is_quarantined() {
        damaged_file_is_quarantined(vec![0xff, 0xfe, 0x00, 0x80, 0xc3, 0x28]);
    }

    #[test]
    fn an_oversized_file_is_quarantined_unread() {
        damaged_file_is_quarantined(vec![b' '; MAX_BYTES as usize + 1]);
    }

    #[test]
    fn a_file_from_a_later_build_is_kept_not_replaced() {
        damaged_file_is_quarantined(br#"{"version":2,"offers":[],"credentialed":[]}"#.to_vec());
    }

    #[test]
    fn only_the_newest_quarantined_copies_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        for round in 0..(KEEP_QUARANTINED + 2) {
            std::fs::write(dir.path().join(FILE), format!("bad {round}")).unwrap();
            Inbox::load(Some(dir.path().to_path_buf()));
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        let kept = quarantined(dir.path());
        assert_eq!(kept.len(), KEEP_QUARANTINED);
        let newest = std::fs::read_to_string(kept.last().unwrap()).unwrap();
        assert_eq!(newest, format!("bad {}", KEEP_QUARANTINED + 1));
    }

    #[test]
    fn an_interrupted_write_leaves_the_previous_inbox_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = saved(dir.path());
        let before = std::fs::read(&path).unwrap();
        // A crash after a partial temporary write, before the rename.
        std::fs::write(path.with_extension("tmp"), b"{\"version\":1,\"off").unwrap();
        let inbox = Inbox::load(Some(dir.path().to_path_buf()));
        assert_eq!(inbox.credentialed, [PathBuf::from("kept")]);
        assert!(inbox.notice.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_failed_replace_leaves_the_previous_inbox_and_the_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = saved(dir.path());
        let before = std::fs::read(&path).unwrap();
        let mut inbox = Inbox::load(Some(dir.path().to_path_buf()));
        // A directory where the temporary file belongs makes the write fail.
        std::fs::create_dir(path.with_extension("tmp")).unwrap();
        assert!(inbox.remember_credentialed(PathBuf::from("lost")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(inbox.credentialed, [PathBuf::from("kept")]);
    }

    #[test]
    fn an_unreadable_file_is_neither_moved_nor_replaced() {
        let dir = tempfile::tempdir().unwrap();
        // A directory cannot be read as a file: a read failure, not a bad file.
        std::fs::create_dir(dir.path().join(FILE)).unwrap();
        let mut inbox = Inbox::load(Some(dir.path().to_path_buf()));
        assert!(inbox.notice.is_some());
        assert!(quarantined(dir.path()).is_empty());
        inbox.remember_credentialed(PathBuf::from("x")).unwrap();
        assert!(dir.path().join(FILE).is_dir(), "never replaced");
    }

    #[test]
    fn the_copy_just_made_survives_pruning_whatever_its_modified_time() {
        let dir = tempfile::tempdir().unwrap();
        for stamp in 1..=KEEP_QUARANTINED {
            std::fs::write(dir.path().join(format!("{FILE}.corrupt-{stamp}")), "old").unwrap();
        }
        let path = dir.path().join(FILE);
        std::fs::write(&path, "bad").unwrap();
        let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let inbox = Inbox::load(Some(dir.path().to_path_buf()));
        let notice = inbox.notice().unwrap();
        let moved = quarantined(dir.path())
            .into_iter()
            .find(|found| notice.contains(&*found.to_string_lossy()))
            .expect("the new copy is kept");
        assert_eq!(std::fs::read_to_string(moved).unwrap(), "bad");
        assert_eq!(quarantined(dir.path()).len(), KEEP_QUARANTINED);
    }

    #[test]
    fn the_opened_handle_not_the_path_is_checked_against_the_grant() {
        let root = tempfile::tempdir().unwrap();
        let granted = root.path().join("granted");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"x").unwrap();
        std::fs::write(granted.join("a.txt"), b"x").unwrap();
        let boundary = Boundary {
            data: None,
            grants: Some(vec![granted.display().to_string()]),
        };
        let inside = open_source(&granted.join("a.txt"), &boundary).unwrap().1;
        assert!(inside.starts_with(std::fs::canonicalize(&granted).unwrap()));

        // The path was fine a moment ago; now its folder leads outside.
        let swapped = granted.join("swapped");
        #[cfg(windows)]
        assert!(
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&swapped)
                .arg(&outside)
                .output()
                .unwrap()
                .status
                .success()
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &swapped).unwrap();
        assert!(matches!(
            open_source(&swapped.join("secret.txt"), &boundary),
            Err(Unreadable::Outside)
        ));
        assert!(matches!(
            open_source(&granted.join("gone.txt"), &boundary),
            Err(Unreadable::Missing)
        ));
    }
}
