//! Multi-source verified downloads with selective, trusted-piece repair.
//!
//! What this path may and may not claim:
//!
//! * A digest computed here is an **observed local digest** compared against a
//!   digest supplied by whoever wrote the metadata. It is never publisher
//!   authenticity evidence, and no type or field in this module says otherwise.
//! * Trusted piece hashes localize damage to exact byte ranges, so only the
//!   failing ranges are re-fetched and a repair count is real.
//! * With only a whole-file digest there is **no fault localization**. A
//!   mismatch discards the whole staged file and restarts conservatively from
//!   another mirror, and [`VerifiedDownload::repaired_pieces`] stays empty.
//! * Nothing reaches the destination unless the required verification passed.
//!   Publication still goes through the existing create-only fence and the
//!   cancellation race gate.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use curl::easy::Easy;
use fetchpath_cache::{
    Acquired, CachedVerification, ContentCache, ContentId, Provenance, TrustedCheck,
};
use fetchpath_http::TransferError;
use fetchpath_metalink::{MetalinkFile, PieceMap};
use fetchpath_storage::{
    CheckpointPhase, CheckpointRecord, CheckpointStore, FaultInjector, NoFaults, sha256_file,
};
use sha2::{Digest, Sha256};

use crate::checkpoint::{ResponseHeaders, source_key_with_context};
use crate::transfer;
use crate::{
    BUFFER_BYTES, CancelCleanup, CancellationToken, DownloadError, DownloadRequest, RequestContext,
};

/// Mirror list length this path will consider. Everything past it is ignored so
/// no loop is unbounded in the number of sources.
pub const MAX_MIRRORS: usize = 32;
/// Whole-file attempts across all mirrors.
pub const MAX_WHOLE_FILE_ATTEMPTS: u32 = 6;
/// Verify/repair passes over one staged file.
pub const MAX_REPAIR_ROUNDS: u32 = 3;
/// Mirrors tried for one damaged piece.
pub const MAX_REPAIR_MIRRORS_PER_PIECE: u32 = 3;
/// This path issues mirror requests one at a time. Parallel mirror fan-out is
/// not implemented; the process-wide HTTP budget gates every request regardless.
pub const MAX_CONCURRENT_MIRROR_ATTEMPTS: usize = 1;
/// Default wall-clock ceiling for one mirror request. A mirror that exceeds it
/// is abandoned and de-prioritised rather than allowed to stall the transfer.
pub const DEFAULT_MIRROR_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);
/// Lowest advisory priority, matching RFC 5854.
pub const LOWEST_PRIORITY: u32 = 999_999;

const READ_BUFFER_BYTES: usize = 64 * 1024;

/// One mirror for the same representation. `priority` is advisory; lower is
/// preferred, and measured behavior overrides it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MirrorSource {
    pub url: String,
    pub priority: u32,
    pub location: Option<String>,
}

impl MirrorSource {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            priority: LOWEST_PRIORITY,
            location: None,
        }
    }

    pub fn with_priority(url: impl Into<String>, priority: u32) -> Self {
        Self {
            priority,
            ..Self::new(url)
        }
    }
}

/// How strongly the published bytes were checked. Callers must be able to tell
/// these apart, because they are not the same promise.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationLevel {
    /// Every piece matched a trusted piece hash, the staged size matched the
    /// piece map, and a whole-file digest, if supplied, also matched.
    PieceHashes,
    /// Only a whole-file digest was available and it matched. Nothing about
    /// which bytes were wrong could have been known if it had not.
    FinalHashOnly,
    /// No trusted digest was supplied. The bytes are unverified.
    Unverified,
}

/// Where the published bytes actually came from.
///
/// This is not a performance label. Completion from a local source is reuse,
/// not throughput, so a benchmark must exclude or flag any completion whose
/// source is not [`DeliverySource::Network`], and an interface must say the
/// file came from cache rather than report an implausible transfer rate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliverySource {
    Network,
    LocalCache,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorOutcome {
    /// Never attempted.
    Unused,
    /// Delivered the bytes that were finally published.
    Delivered,
    /// Contributed at least one verified repaired piece.
    Repaired,
    /// Delivered bytes that failed a trusted digest.
    Corrupt,
    /// Exceeded the per-attempt time ceiling.
    Slow,
    /// Refused the connection, was unreachable, or never began a response.
    Offline,
    /// Answered, but not with a usable representation.
    ProtocolFailure,
    /// Not an `http://` or `https://` mirror, so this path never used it.
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MirrorReport {
    pub mirror_index: usize,
    /// The mirror URL with any query string removed, so a signed URL never
    /// reaches a report, a log, or evidence.
    pub redacted_url: String,
    pub priority: u32,
    pub attempts: u32,
    pub bytes_delivered: u64,
    pub corrupt_observations: u32,
    pub slow_observations: u32,
    pub offline_observations: u32,
    pub protocol_failures: u32,
    /// True once measured behavior pushed this mirror behind its advisory
    /// priority, or removed it from consideration entirely.
    pub deprioritised: bool,
    pub outcome: MirrorOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedDownload {
    pub destination: PathBuf,
    pub bytes: u64,
    /// An observed local digest, not publisher-authenticity evidence.
    pub observed_sha256: String,
    pub verification: VerificationLevel,
    /// Where the bytes came from. Reuse is not throughput.
    pub source: DeliverySource,
    /// Piece indices that trusted piece hashes localized, re-fetched, and
    /// re-verified. Only ever non-empty for [`VerificationLevel::PieceHashes`].
    pub repaired_pieces: Vec<usize>,
    /// Whole-file restarts forced by a digest that localized nothing.
    pub conservative_restarts: u32,
    pub mirrors: Vec<MirrorReport>,
    pub staging_cleanup_pending: Option<PathBuf>,
}

#[derive(Debug)]
pub enum VerifiedDownloadError {
    /// No `http://` or `https://` mirror was supplied.
    NoUsableMirror,
    InvalidDestination(PathBuf),
    DestinationExists {
        destination: PathBuf,
        staging: Option<PathBuf>,
    },
    Cancelled {
        staging: Option<PathBuf>,
    },
    /// Every mirror failed to deliver a usable representation.
    MirrorsExhausted {
        detail: String,
        mirrors: Vec<MirrorReport>,
        staging: Option<PathBuf>,
    },
    /// Bytes arrived but the required verification never passed, so nothing was
    /// published.
    VerificationFailed {
        detail: String,
        mirrors: Vec<MirrorReport>,
        staging: Option<PathBuf>,
    },
    Storage {
        path: PathBuf,
        detail: String,
        staging: Option<PathBuf>,
    },
}

impl std::fmt::Display for VerifiedDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoUsableMirror => write!(
                f,
                "input.no_usable_mirror: no http:// or https:// mirror was supplied"
            ),
            Self::InvalidDestination(path) => write!(
                f,
                "input.invalid_destination: {} needs a file name",
                path.display()
            ),
            Self::DestinationExists { destination, .. } => write!(
                f,
                "storage.destination_conflict: {} already exists",
                destination.display()
            ),
            Self::Cancelled {
                staging: Some(path),
            } => {
                write!(f, "cancelled; retained staging file {}", path.display())
            }
            Self::Cancelled { staging: None } => write!(f, "cancelled; staging file removed"),
            Self::MirrorsExhausted { detail, .. } => {
                write!(f, "source.mirrors_exhausted: {detail}")
            }
            Self::VerificationFailed { detail, .. } => {
                write!(f, "verification.failed: {detail}; nothing was published")
            }
            Self::Storage { path, detail, .. } => {
                write!(f, "storage.failed at {}: {detail}", path.display())
            }
        }
    }
}

impl std::error::Error for VerifiedDownloadError {}

#[derive(Clone)]
pub struct VerifiedDownloadRequest {
    pub mirrors: Vec<MirrorSource>,
    pub destination: PathBuf,
    pub cancellation: CancellationToken,
    pub cancel_cleanup: CancelCleanup,
    pub context: RequestContext,
    /// The size the metadata declares, when there is no piece map to declare it.
    pub expected_bytes: Option<u64>,
    /// A whole-file SHA-256 supplied by the metadata author, lowercase hex.
    pub expected_sha256: Option<String>,
    /// Trusted piece hashes. Their presence is what makes selective repair, and
    /// therefore any repair claim, possible.
    pub pieces: Option<PieceMap>,
    pub mirror_attempt_timeout: Duration,
}

impl VerifiedDownloadRequest {
    pub fn new(mirrors: Vec<MirrorSource>, destination: PathBuf) -> Self {
        Self {
            mirrors,
            destination,
            cancellation: CancellationToken::default(),
            cancel_cleanup: CancelCleanup::RemoveStaging,
            context: RequestContext::default(),
            expected_bytes: None,
            expected_sha256: None,
            pieces: None,
            mirror_attempt_timeout: DEFAULT_MIRROR_ATTEMPT_TIMEOUT,
        }
    }

    /// Builds a request from one parsed Metalink 4 file entry. The metalink
    /// file name is deliberately *not* joined onto the destination here: the
    /// caller decides where bytes land.
    pub fn from_metalink_file(file: &MetalinkFile, destination: PathBuf) -> Self {
        let mirrors = file
            .mirrors_by_priority()
            .into_iter()
            .map(|url| MirrorSource {
                url: url.url.clone(),
                priority: url.priority.unwrap_or(LOWEST_PRIORITY),
                location: url.location.clone(),
            })
            .collect();
        Self {
            expected_bytes: file.size,
            expected_sha256: file.expected_sha256().map(str::to_owned),
            pieces: file.pieces.clone(),
            ..Self::new(mirrors, destination)
        }
    }
}

impl VerifiedDownloadRequest {
    /// The trusted identity of this request's content, if it has one.
    ///
    /// Piece hashes take precedence over a whole-file digest because they are
    /// the stronger construction: they localize damage, and a request carrying
    /// both is still fundamentally piece-identified.
    pub fn content_id(&self) -> Option<ContentId> {
        if let Some(pieces) = &self.pieces {
            return Some(ContentId::from_piece_map(pieces));
        }
        self.expected_sha256
            .as_deref()
            .and_then(ContentId::from_expected_sha256)
    }
}

/// The trusted check the cache runs before reuse: exactly the digests this
/// request carries, applied to the cached file.
struct RequestCheck<'a> {
    pieces: Option<&'a PieceMap>,
    expected_sha256: Option<&'a str>,
}

impl TrustedCheck for RequestCheck<'_> {
    fn verify(&self, path: &Path) -> io::Result<bool> {
        if let Some(pieces) = self.pieces
            && !pieces.verify_file(path)?.is_complete()
        {
            return Ok(false);
        }
        if let Some(expected) = self.expected_sha256
            && !sha256_file(path)?.eq_ignore_ascii_case(expected)
        {
            return Ok(false);
        }
        Ok(true)
    }
}

/// A verified download that may complete from the bounded cache instead of the
/// network.
///
/// A cache hit is reuse, not throughput. The result's [`VerifiedDownload::source`]
/// says where the bytes came from, and callers that report rates must honour it.
///
/// The cache is optional at every point and fatal at none: a miss, a failed
/// check, an unreadable store or a refused insertion all fall through to the
/// unchanged mirror path.
pub fn download_verified_cached(
    request: VerifiedDownloadRequest,
    cache: &mut ContentCache,
) -> Result<VerifiedDownload, VerifiedDownloadError> {
    if let Some(id) = request.content_id()
        && let Some(result) = reuse_from_cache(&request, cache, &id)?
    {
        return Ok(result);
    }

    let result = download_verified(request.clone())?;
    insert_into_cache(&request, cache, &result);
    Ok(result)
}

fn reuse_from_cache(
    request: &VerifiedDownloadRequest,
    cache: &mut ContentCache,
    id: &ContentId,
) -> Result<Option<VerifiedDownload>, VerifiedDownloadError> {
    if request.destination.exists() {
        return Err(VerifiedDownloadError::DestinationExists {
            destination: request.destination.clone(),
            staging: None,
        });
    }
    let check = RequestCheck {
        pieces: request.pieces.as_ref(),
        expected_sha256: request.expected_sha256.as_deref(),
    };
    // A cache that cannot be read is not a download failure.
    let Ok(acquired) = cache.acquire_verified(id, &check) else {
        return Ok(None);
    };
    let Acquired::Hit(cached) = acquired else {
        return Ok(None);
    };

    let published = publish_from_cache(request, &cached);
    cache.release(id);
    published
}

fn publish_from_cache(
    request: &VerifiedDownloadRequest,
    cached: &Path,
) -> Result<Option<VerifiedDownload>, VerifiedDownloadError> {
    let key = source_key_with_context("fetchpath:cache", &request.context.fingerprint());
    let Ok(store) = CheckpointStore::new(&request.destination, &key) else {
        return Ok(None);
    };
    // Stage a copy on the destination volume, then publish through the same
    // create-only fence the network path uses.
    match store.reset() {
        Ok(file) => drop(file),
        Err(_) => return Ok(None),
    }
    if std::fs::copy(cached, store.staging()).is_err() {
        let _ = store.remove_all();
        return Ok(None);
    }
    let (Ok(observed), Ok(total)) = (
        sha256_file(store.staging()),
        std::fs::metadata(store.staging()).map(|meta| meta.len()),
    ) else {
        let _ = store.remove_all();
        return Ok(None);
    };

    let verification = match (&request.pieces, &request.expected_sha256) {
        (Some(_), _) => VerificationLevel::PieceHashes,
        (None, Some(_)) => VerificationLevel::FinalHashOnly,
        (None, None) => VerificationLevel::Unverified,
    };

    let published = publish(
        request,
        &store,
        "fetchpath:cache",
        key,
        total,
        observed,
        &NoFaults,
    )?;

    Ok(Some(VerifiedDownload {
        destination: published.destination,
        bytes: published.bytes,
        observed_sha256: published.observed_sha256,
        verification,
        source: DeliverySource::LocalCache,
        repaired_pieces: Vec::new(),
        conservative_restarts: 0,
        mirrors: reports(&build_mirrors(&request.mirrors)),
        staging_cleanup_pending: published.staging_cleanup_pending,
    }))
}

/// Inserts a completed download. Failing to cache is never a download failure.
fn insert_into_cache(
    request: &VerifiedDownloadRequest,
    cache: &mut ContentCache,
    result: &VerifiedDownload,
) {
    if result.source != DeliverySource::Network {
        return;
    }
    let verification = match result.verification {
        VerificationLevel::PieceHashes => CachedVerification::PieceHashes,
        VerificationLevel::FinalHashOnly => CachedVerification::FinalHashOnly,
        // Not eligible: nothing trusted identifies these bytes.
        VerificationLevel::Unverified => return,
    };
    let Some(id) = request.content_id() else {
        return;
    };
    // Provenance is read from the original mirror URL, not from the report's
    // redacted copy: `redact` has already stripped the query string, so the
    // report cannot tell a signed URL from a plain one.
    let delivered_from_public_url = result
        .mirrors
        .iter()
        .find(|report| report.outcome == MirrorOutcome::Delivered)
        .and_then(|report| request.mirrors.get(report.mirror_index))
        .is_some_and(|mirror| !mirror.url.contains(['?', '#']));
    let provenance = if request.context.is_credential_free() && delivered_from_public_url {
        Provenance::Public
    } else {
        Provenance::Credentialed
    };
    let _ = cache.insert(&id, &result.destination, verification, provenance);
}

/// Downloads one representation from a mirror list, verifying it against
/// trusted digests and repairing selectively where piece hashes allow it.
pub fn download_verified(
    request: VerifiedDownloadRequest,
) -> Result<VerifiedDownload, VerifiedDownloadError> {
    download_verified_with_faults(request, &NoFaults)
}

pub fn download_verified_with_faults(
    request: VerifiedDownloadRequest,
    faults: &dyn FaultInjector,
) -> Result<VerifiedDownload, VerifiedDownloadError> {
    let mut mirrors = build_mirrors(&request.mirrors);
    if mirrors.iter().all(|mirror| mirror.disabled) {
        return Err(VerifiedDownloadError::NoUsableMirror);
    }
    if request.destination.exists() {
        return Err(VerifiedDownloadError::DestinationExists {
            destination: request.destination.clone(),
            staging: None,
        });
    }

    let fingerprint = request.context.fingerprint();
    let anchor = mirrors
        .iter()
        .find(|mirror| !mirror.disabled)
        .expect("at least one usable mirror");
    let key = source_key_with_context(&anchor.url, &fingerprint);
    let store = CheckpointStore::new(&request.destination, &key).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidInput {
            VerifiedDownloadError::InvalidDestination(request.destination.clone())
        } else {
            VerifiedDownloadError::Storage {
                path: request.destination.clone(),
                detail: error.to_string(),
                staging: None,
            }
        }
    })?;

    let expected_total = request
        .pieces
        .as_ref()
        .map(PieceMap::total_size)
        .or(request.expected_bytes);
    let mut conservative_restarts = 0_u32;
    let mut detail = "no mirror was attempted".to_owned();

    for _attempt in 0..MAX_WHOLE_FILE_ATTEMPTS {
        if request.cancellation.is_cancelled() {
            return Err(cancelled(&request, &store));
        }
        let Some(selected) = best_mirror(&mirrors) else {
            break;
        };
        let mut file = store.reset().map_err(|error| storage(&store, error))?;
        request.cancellation.set_received(0);
        mirrors[selected].attempts += 1;

        let delivered =
            match fetch_whole_file(&request, &store, &mut file, &mirrors[selected], faults) {
                Err(failure) => {
                    detail = failure.to_string();
                    if let MirrorFailure::Cancelled = failure {
                        return Err(cancelled(&request, &store));
                    }
                    if let MirrorFailure::Storage(error) = failure {
                        return Err(storage(&store, error));
                    }
                    mirrors[selected].record(&failure);
                    continue;
                }
                Ok(received) => {
                    mirrors[selected].bytes_delivered = received;
                    if expected_total.is_some_and(|total| total != received) {
                        detail = format!(
                            "mirror delivered {received} bytes where {} were declared",
                            expected_total.unwrap_or_default()
                        );
                        mirrors[selected].record(&MirrorFailure::Protocol(detail.clone()));
                        continue;
                    }
                    received
                }
            };

        store
            .sync_payload(&mut file, faults)
            .map_err(|error| storage(&store, error))?;

        // Trusted piece hashes: localize the damage and repair exactly it.
        let mut repaired: Vec<usize> = Vec::new();
        if let Some(map) = request.pieces.as_ref() {
            match repair_with_pieces(
                &request,
                &store,
                &mut file,
                map,
                &mut mirrors,
                selected,
                faults,
                &mut repaired,
            )? {
                Ok(()) => {}
                Err(reason) => {
                    detail = reason;
                    mirrors[selected].record(&MirrorFailure::Corrupt(detail.clone()));
                    continue;
                }
            }
            store
                .sync_payload(&mut file, faults)
                .map_err(|error| storage(&store, error))?;
        }

        drop(file);
        let observed = sha256_file(store.staging()).map_err(|error| storage(&store, error))?;

        // A whole-file digest is still checked when one exists, but on its own
        // it localizes nothing: a mismatch is a conservative restart, never a
        // repair.
        if let Some(expected) = request.expected_sha256.as_deref()
            && !expected.eq_ignore_ascii_case(&observed)
        {
            detail = if request.pieces.is_some() {
                "every piece matched but the whole-file digest did not".to_owned()
            } else {
                "the whole-file digest did not match and no piece map localizes the damage"
                    .to_owned()
            };
            conservative_restarts += 1;
            mirrors[selected].record(&MirrorFailure::Corrupt(detail.clone()));
            continue;
        }

        let verification = match (&request.pieces, &request.expected_sha256) {
            (Some(_), _) => VerificationLevel::PieceHashes,
            (None, Some(_)) => VerificationLevel::FinalHashOnly,
            (None, None) => VerificationLevel::Unverified,
        };
        repaired.sort_unstable();
        repaired.dedup();
        debug_assert!(repaired.is_empty() || verification == VerificationLevel::PieceHashes);

        let total = expected_total.unwrap_or(delivered);
        let published = publish(
            &request,
            &store,
            &mirrors[selected].url.clone(),
            key,
            total,
            observed,
            faults,
        )?;
        if mirrors[selected].corrupt_observations == 0 {
            mirrors[selected].outcome = MirrorOutcome::Delivered;
        }
        return Ok(VerifiedDownload {
            destination: published.destination,
            bytes: published.bytes,
            observed_sha256: published.observed_sha256,
            verification,
            source: DeliverySource::Network,
            repaired_pieces: repaired,
            conservative_restarts,
            mirrors: reports(&mirrors),
            staging_cleanup_pending: published.staging_cleanup_pending,
        });
    }

    let _ = store.remove_all();
    let staging = store
        .staging()
        .exists()
        .then(|| store.staging().to_path_buf());
    let verified_bytes_arrived = mirrors
        .iter()
        .any(|mirror| mirror.corrupt_observations > 0 || mirror.bytes_delivered > 0);
    Err(if verified_bytes_arrived {
        VerifiedDownloadError::VerificationFailed {
            detail,
            mirrors: reports(&mirrors),
            staging,
        }
    } else {
        VerifiedDownloadError::MirrorsExhausted {
            detail,
            mirrors: reports(&mirrors),
            staging,
        }
    })
}

// ---------------------------------------------------------------------------
// Mirror health
// ---------------------------------------------------------------------------

struct Mirror {
    index: usize,
    url: String,
    redacted_url: String,
    priority: u32,
    attempts: u32,
    bytes_delivered: u64,
    corrupt_observations: u32,
    slow_observations: u32,
    offline_observations: u32,
    protocol_failures: u32,
    disabled: bool,
    outcome: MirrorOutcome,
}

impl Mirror {
    /// Measured behavior outranks advisory priority: a corrupt mirror sinks
    /// furthest, then offline, then slow, then merely unhelpful.
    fn penalty(&self) -> u32 {
        self.corrupt_observations * 8
            + self.offline_observations * 4
            + self.slow_observations * 2
            + self.protocol_failures
    }

    fn record(&mut self, failure: &MirrorFailure) {
        match failure {
            MirrorFailure::Corrupt(_) => {
                self.corrupt_observations += 1;
                self.outcome = MirrorOutcome::Corrupt;
            }
            MirrorFailure::Slow(_) => {
                self.slow_observations += 1;
                self.outcome = MirrorOutcome::Slow;
            }
            MirrorFailure::Offline(_) => {
                self.offline_observations += 1;
                self.outcome = MirrorOutcome::Offline;
            }
            MirrorFailure::Protocol(_) => {
                self.protocol_failures += 1;
                self.outcome = MirrorOutcome::ProtocolFailure;
            }
            MirrorFailure::Cancelled | MirrorFailure::Storage(_) => return,
        }
        // Two strikes of the same kind, or any two strikes at all, take a
        // mirror out of rotation so no loop can spin on it.
        if self.penalty() >= 8 || self.attempts >= 3 {
            self.disabled = true;
        }
    }

    fn report(&self) -> MirrorReport {
        MirrorReport {
            mirror_index: self.index,
            redacted_url: self.redacted_url.clone(),
            priority: self.priority,
            attempts: self.attempts,
            bytes_delivered: self.bytes_delivered,
            corrupt_observations: self.corrupt_observations,
            slow_observations: self.slow_observations,
            offline_observations: self.offline_observations,
            protocol_failures: self.protocol_failures,
            deprioritised: self.penalty() > 0,
            outcome: self.outcome,
        }
    }
}

fn build_mirrors(sources: &[MirrorSource]) -> Vec<Mirror> {
    let mut mirrors: Vec<Mirror> = sources
        .iter()
        .take(MAX_MIRRORS)
        .enumerate()
        .map(|(index, source)| {
            let supported = source.url.starts_with("http://") || source.url.starts_with("https://");
            Mirror {
                index,
                url: source.url.clone(),
                redacted_url: redact(&source.url),
                priority: source.priority,
                attempts: 0,
                bytes_delivered: 0,
                corrupt_observations: 0,
                slow_observations: 0,
                offline_observations: 0,
                protocol_failures: 0,
                disabled: !supported,
                outcome: if supported {
                    MirrorOutcome::Unused
                } else {
                    MirrorOutcome::Unsupported
                },
            }
        })
        .collect();
    mirrors.sort_by_key(|mirror| (mirror.priority, mirror.index));
    mirrors
}

/// Strips any query or fragment, which is where signed-URL credentials live.
fn redact(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_owned()
}

fn best_mirror(mirrors: &[Mirror]) -> Option<usize> {
    mirrors
        .iter()
        .enumerate()
        .filter(|(_, mirror)| !mirror.disabled)
        .min_by_key(|(position, mirror)| (mirror.penalty(), mirror.priority, *position))
        .map(|(position, _)| position)
}

/// Ranks candidates for repairing one piece, preferring any mirror other than
/// the one that produced the bad bytes.
fn repair_candidates(mirrors: &[Mirror], avoid: usize) -> Vec<usize> {
    let mut candidates: Vec<usize> = (0..mirrors.len())
        .filter(|position| !mirrors[*position].disabled && *position != avoid)
        .collect();
    candidates.sort_by_key(|position| {
        (
            mirrors[*position].penalty(),
            mirrors[*position].priority,
            *position,
        )
    });
    // Only fall back to the suspect mirror when nothing else is left.
    if !mirrors[avoid].disabled {
        candidates.push(avoid);
    }
    candidates.truncate(MAX_REPAIR_MIRRORS_PER_PIECE as usize);
    candidates
}

fn reports(mirrors: &[Mirror]) -> Vec<MirrorReport> {
    let mut reports: Vec<MirrorReport> = mirrors.iter().map(Mirror::report).collect();
    reports.sort_by_key(|report| report.mirror_index);
    reports
}

// ---------------------------------------------------------------------------
// Selective repair
// ---------------------------------------------------------------------------

/// Verifies the staged file against the piece map and re-fetches exactly the
/// failing ranges. The outer `Result` is a fatal error; the inner one says the
/// staged file could not be brought into agreement with the trusted pieces.
#[allow(clippy::too_many_arguments)]
fn repair_with_pieces(
    request: &VerifiedDownloadRequest,
    store: &CheckpointStore,
    file: &mut File,
    map: &PieceMap,
    mirrors: &mut [Mirror],
    source: usize,
    faults: &dyn FaultInjector,
    repaired: &mut Vec<usize>,
) -> Result<Result<(), String>, VerifiedDownloadError> {
    let mut verification = map
        .verify_file(store.staging())
        .map_err(|error| storage(store, error))?;
    if verification.observed_size != verification.expected_size {
        return Ok(Err(format!(
            "staged {} bytes where the piece map declares {}",
            verification.observed_size, verification.expected_size
        )));
    }

    if !verification.failed_pieces.is_empty() {
        // The mirror that streamed these bytes served at least one piece that
        // failed a trusted hash. That is measured evidence about this mirror,
        // recorded before any repair is attributed to anyone else.
        mirrors[source].record(&MirrorFailure::Corrupt(format!(
            "{} of {} pieces failed their trusted hash",
            verification.failed_pieces.len(),
            map.piece_count()
        )));
    }

    for _round in 0..MAX_REPAIR_ROUNDS {
        if verification.is_complete() {
            return Ok(Ok(()));
        }
        for piece in verification.failed_pieces.clone() {
            if request.cancellation.is_cancelled() {
                return Err(cancelled(request, store));
            }
            let Some((start, length)) = map.piece_range(piece) else {
                return Ok(Err(format!("piece {piece} is outside the piece map")));
            };
            let mut healed = false;
            for candidate in repair_candidates(mirrors, source) {
                mirrors[candidate].attempts += 1;
                let outcome = fetch_range(
                    request,
                    store,
                    file,
                    &mirrors[candidate],
                    start,
                    length,
                    faults,
                );
                match outcome {
                    Err(MirrorFailure::Cancelled) => return Err(cancelled(request, store)),
                    Err(MirrorFailure::Storage(error)) => {
                        return Err(storage(store, error));
                    }
                    Err(failure) => {
                        mirrors[candidate].record(&failure);
                        continue;
                    }
                    Ok(()) => {}
                }
                store
                    .sync_payload(file, faults)
                    .map_err(|error| storage(store, error))?;
                let matches = staged_piece_matches(map, piece, store.staging())
                    .map_err(|error| storage(store, error))?;
                if matches {
                    mirrors[candidate].bytes_delivered += length;
                    if mirrors[candidate].outcome != MirrorOutcome::Delivered {
                        mirrors[candidate].outcome = MirrorOutcome::Repaired;
                    }
                    repaired.push(piece);
                    healed = true;
                    break;
                }
                mirrors[candidate].record(&MirrorFailure::Corrupt(format!(
                    "piece {piece} failed its trusted hash"
                )));
            }
            if !healed {
                return Ok(Err(format!(
                    "piece {piece} could not be repaired from any mirror"
                )));
            }
        }
        store
            .sync_payload(file, faults)
            .map_err(|error| storage(store, error))?;
        verification = map
            .verify_file(store.staging())
            .map_err(|error| storage(store, error))?;
    }

    if verification.is_complete() {
        Ok(Ok(()))
    } else {
        Ok(Err(format!(
            "{} pieces still fail after {MAX_REPAIR_ROUNDS} repair rounds",
            verification.failed_pieces.len()
        )))
    }
}

/// Hashes one staged piece in place, so repair never has to buffer a whole
/// piece in memory.
fn staged_piece_matches(map: &PieceMap, index: usize, path: &Path) -> io::Result<bool> {
    let Some(((start, length), expected)) = map.piece_range(index).zip(map.piece_hash(index))
    else {
        return Ok(false);
    };
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES];
    let mut remaining = length;
    while remaining > 0 {
        let want = buffer.len().min(remaining as usize);
        let read = file.read(&mut buffer[..want])?;
        if read == 0 {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    let digest: [u8; 32] = hasher.finalize().into();
    Ok(&digest == expected)
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

enum MirrorFailure {
    Offline(String),
    Slow(String),
    Protocol(String),
    Corrupt(String),
    Cancelled,
    Storage(io::Error),
}

impl std::fmt::Display for MirrorFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline(detail) => write!(f, "mirror offline: {detail}"),
            Self::Slow(detail) => write!(f, "mirror too slow: {detail}"),
            Self::Protocol(detail) => write!(f, "mirror protocol failure: {detail}"),
            Self::Corrupt(detail) => write!(f, "mirror served unverifiable bytes: {detail}"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Storage(error) => write!(f, "storage failure: {error}"),
        }
    }
}

fn fetch_whole_file(
    request: &VerifiedDownloadRequest,
    store: &CheckpointStore,
    file: &mut File,
    mirror: &Mirror,
    faults: &dyn FaultInjector,
) -> Result<u64, MirrorFailure> {
    // Piece hashes let a hopeless mirror be abandoned before the whole file
    // arrives, rather than after.
    let map = request.pieces.as_ref();
    let mut verifier = map.map(PieceMap::streaming_verifier);
    let abort_after = map.map_or(usize::MAX, |map| map.piece_count().div_ceil(4).max(2));
    let mut corrupt = None;
    let mut received = 0_u64;

    let result = perform(request, mirror, None, &mut |offset, bytes| {
        store.write_payload(file, offset, bytes, faults)?;
        received = offset + bytes.len() as u64;
        if let Some(verifier) = verifier.as_mut() {
            verifier.update(bytes);
            if verifier.failed_so_far().len() >= abort_after {
                corrupt = Some(format!(
                    "{} of {} pieces already failed their trusted hash",
                    verifier.failed_so_far().len(),
                    map.map_or(0, PieceMap::piece_count)
                ));
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "abandoning a mirror that is failing trusted piece hashes",
                ));
            }
        }
        Ok(())
    });
    match result {
        Ok(()) => Ok(received),
        Err(failure) => Err(match (corrupt, failure) {
            (Some(detail), _) => MirrorFailure::Corrupt(detail),
            (None, failure) => failure,
        }),
    }
}

fn fetch_range(
    request: &VerifiedDownloadRequest,
    store: &CheckpointStore,
    file: &mut File,
    mirror: &Mirror,
    start: u64,
    length: u64,
    faults: &dyn FaultInjector,
) -> Result<(), MirrorFailure> {
    perform(
        request,
        mirror,
        Some((start, length)),
        &mut |offset, bytes| store.write_payload(file, offset, bytes, faults),
    )
}

/// One bounded HTTP request against one mirror, gated by the process-wide
/// request budget and by a wall-clock ceiling.
fn perform(
    request: &VerifiedDownloadRequest,
    mirror: &Mirror,
    range: Option<(u64, u64)>,
    sink: &mut dyn FnMut(u64, &[u8]) -> io::Result<()>,
) -> Result<(), MirrorFailure> {
    use std::cell::{Cell, RefCell};

    let _permit = transfer::http_budget()
        .reserve(0, &|| request.cancellation.is_cancelled())
        .map_err(|error| match error {
            TransferError::Cancelled => MirrorFailure::Cancelled,
            other => MirrorFailure::Protocol(other.to_string()),
        })?;

    let mut easy = Easy::new();
    transfer::configure(&mut easy, &mirror.url, &request.context)
        .map_err(|error| MirrorFailure::Protocol(error.to_string()))?;
    easy.buffer_size(BUFFER_BYTES)
        .map_err(|error| MirrorFailure::Protocol(error.to_string()))?;
    // A hard ceiling inside libcurl, so a mirror that stalls cannot hold the
    // transfer open while the progress callback waits to be called again.
    easy.timeout(request.mirror_attempt_timeout)
        .map_err(|error| MirrorFailure::Protocol(error.to_string()))?;
    if let Some((start, length)) = range {
        easy.range(&format!("{start}-{}", start + length - 1))
            .map_err(|error| MirrorFailure::Protocol(error.to_string()))?;
    }

    let started = Instant::now();
    let deadline = request.mirror_attempt_timeout;
    let headers = RefCell::new(ResponseHeaders::default());
    let offset = Cell::new(range.map_or(0, |(start, _)| start));
    let body_bytes = Cell::new(0_u64);
    let failure: RefCell<Option<MirrorFailure>> = RefCell::new(None);
    let timed_out = Cell::new(false);

    let transfer_result = {
        let mut stream = easy.transfer();
        stream
            .header_function(|line| {
                headers.borrow_mut().ingest(line);
                true
            })
            .map_err(|error| MirrorFailure::Protocol(error.to_string()))?;
        stream
            .write_function(|data| {
                if body_bytes.get() == 0
                    && let Err(reason) = check_headers(&headers.borrow(), range)
                {
                    *failure.borrow_mut() = Some(MirrorFailure::Protocol(reason));
                    return Ok(0);
                }
                if let Err(error) = sink(offset.get(), data) {
                    *failure.borrow_mut() = Some(MirrorFailure::Storage(error));
                    return Ok(0);
                }
                offset.set(offset.get() + data.len() as u64);
                body_bytes.set(body_bytes.get() + data.len() as u64);
                request.cancellation.set_received(offset.get());
                Ok(data.len())
            })
            .map_err(|error| MirrorFailure::Protocol(error.to_string()))?;
        stream
            .progress_function(|_, _, _, _| {
                if request.cancellation.is_cancelled() {
                    return false;
                }
                if started.elapsed() > deadline {
                    timed_out.set(true);
                    return false;
                }
                true
            })
            .map_err(|error| MirrorFailure::Protocol(error.to_string()))?;
        stream.perform()
    };

    if let Some(failure) = failure.into_inner() {
        return Err(failure);
    }
    if request.cancellation.is_cancelled() {
        return Err(MirrorFailure::Cancelled);
    }
    // A mirror that never began a response is unreachable, however that
    // failure surfaced: some stacks refuse a dead port outright, others let the
    // connection attempt run until the ceiling expires.
    let responded = headers.borrow().status.is_some() || body_bytes.get() > 0;
    if let Err(error) = transfer_result {
        if error.is_couldnt_connect()
            || error.is_couldnt_resolve_host()
            || error.is_couldnt_resolve_proxy()
            || error.is_recv_error()
        {
            return Err(MirrorFailure::Offline(error.to_string()));
        }
        if timed_out.get() || error.is_operation_timedout() {
            let detail = format!("nothing completed within {deadline:?}");
            return Err(if responded {
                MirrorFailure::Slow(detail)
            } else {
                MirrorFailure::Offline(detail)
            });
        }
        return Err(MirrorFailure::Protocol(match easy.response_code() {
            Ok(status) if status >= 400 => format!("HTTP status {status}"),
            _ => error.to_string(),
        }));
    }
    if started.elapsed() > deadline {
        return Err(MirrorFailure::Slow(format!(
            "no complete response within {deadline:?}"
        )));
    }

    let headers = headers.into_inner();
    if let Err(reason) = check_headers(&headers, range) {
        return Err(MirrorFailure::Protocol(reason));
    }
    let declared = expected_body_len(&headers, range);
    if let Some(expected) = declared
        && expected != body_bytes.get()
    {
        return Err(MirrorFailure::Protocol(format!(
            "mirror declared {expected} bytes and delivered {}",
            body_bytes.get()
        )));
    }
    if let Some((_, length)) = range
        && body_bytes.get() != length
    {
        return Err(MirrorFailure::Protocol(format!(
            "mirror delivered {} bytes for a {length} byte range",
            body_bytes.get()
        )));
    }
    Ok(())
}

fn check_headers(headers: &ResponseHeaders, range: Option<(u64, u64)>) -> Result<(), String> {
    match range {
        None => {
            if headers.status == Some(200) {
                Ok(())
            } else {
                Err(format!(
                    "HTTP status {}",
                    headers.status.unwrap_or_default()
                ))
            }
        }
        Some((start, length)) => {
            if headers.status != Some(206) {
                return Err(format!(
                    "a byte range needs 206, got {}",
                    headers.status.unwrap_or_default()
                ));
            }
            match headers.content_range {
                Some(content_range)
                    if content_range.start == start && content_range.end == start + length - 1 =>
                {
                    Ok(())
                }
                _ => Err("the mirror answered a different byte range".to_owned()),
            }
        }
    }
}

fn expected_body_len(headers: &ResponseHeaders, range: Option<(u64, u64)>) -> Option<u64> {
    if range.is_some() {
        headers
            .content_range
            .map(|content_range| content_range.end - content_range.start + 1)
    } else {
        headers.content_length
    }
}

// ---------------------------------------------------------------------------
// Publication and error mapping
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn publish(
    request: &VerifiedDownloadRequest,
    store: &CheckpointStore,
    url: &str,
    key: String,
    total: u64,
    observed: String,
    faults: &dyn FaultInjector,
) -> Result<crate::DownloadedFile, VerifiedDownloadError> {
    let mut intent = CheckpointRecord::downloading(key, total, observed, None, Some(total));
    intent.phase = CheckpointPhase::PublicationIntent;
    let intent = store
        .commit(intent, faults)
        .map_err(|error| storage(store, error))?;
    let download_request = as_download_request(request, url);
    transfer::publish(&download_request, store, &intent, faults).map_err(from_download_error)
}

fn as_download_request(request: &VerifiedDownloadRequest, url: &str) -> DownloadRequest {
    DownloadRequest {
        url: url.to_owned(),
        destination: request.destination.clone(),
        cancellation: request.cancellation.clone(),
        cancel_cleanup: request.cancel_cleanup,
        context: request.context.clone(),
    }
}

fn from_download_error(error: DownloadError) -> VerifiedDownloadError {
    match error {
        DownloadError::InvalidUrl => VerifiedDownloadError::NoUsableMirror,
        DownloadError::InvalidDestination(path) => VerifiedDownloadError::InvalidDestination(path),
        DownloadError::DestinationExists {
            destination,
            staging,
        } => VerifiedDownloadError::DestinationExists {
            destination,
            staging,
        },
        DownloadError::Cancelled { staging } => VerifiedDownloadError::Cancelled { staging },
        DownloadError::Transport { detail, staging } => VerifiedDownloadError::MirrorsExhausted {
            detail,
            mirrors: Vec::new(),
            staging,
        },
        DownloadError::Storage {
            path,
            detail,
            staging,
        } => VerifiedDownloadError::Storage {
            path,
            detail,
            staging,
        },
    }
}

fn cancelled(request: &VerifiedDownloadRequest, store: &CheckpointStore) -> VerifiedDownloadError {
    let download_request = as_download_request(request, "");
    from_download_error(transfer::cancelled(&download_request, store))
}

fn storage(store: &CheckpointStore, error: io::Error) -> VerifiedDownloadError {
    from_download_error(transfer::storage_detail(store, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use fetchpath_metalink::ParseLimits;
    use std::fs;
    use std::io::Write;
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::{SystemTime, UNIX_EPOCH};

    const PIECE_LENGTH: u64 = 1024;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fetchpath-metalink-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn payload() -> Vec<u8> {
        (0..2500_u32).map(|index| (index % 251) as u8).collect()
    }

    fn hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn piece_map(body: &[u8]) -> PieceMap {
        let hashes = body
            .chunks(PIECE_LENGTH as usize)
            .map(|chunk| -> [u8; 32] { Sha256::digest(chunk).into() })
            .collect();
        PieceMap::new(
            PIECE_LENGTH,
            body.len() as u64,
            hashes,
            &ParseLimits::default(),
        )
        .unwrap()
    }

    /// Damages one byte inside one piece, leaving every other piece intact.
    fn damage(body: &[u8], piece: usize, seed: u8) -> Vec<u8> {
        let mut damaged = body.to_vec();
        let offset = piece * PIECE_LENGTH as usize + 7;
        damaged[offset] ^= seed;
        damaged
    }

    struct MirrorServer {
        url: String,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl Drop for MirrorServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    /// A loopback mirror that answers 200 and 206 and can be made slow.
    fn mirror(body: Vec<u8>, chunk: usize, delay: Option<Duration>) -> MirrorServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/payload.bin", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &body, chunk, delay),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        MirrorServer {
            url,
            stop,
            worker: Some(worker),
        }
    }

    /// A URL nothing is listening on, so a connection is refused outright.
    fn offline_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{address}/payload.bin")
    }

    fn serve(mut stream: TcpStream, body: &[u8], chunk: usize, delay: Option<Duration>) {
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
        }
        let request = String::from_utf8_lossy(&request).into_owned();
        let range = request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("range") {
                return None;
            }
            let (start, end) = value.trim().strip_prefix("bytes=")?.split_once('-')?;
            Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
        });
        let (status, selected, content_range) = match range {
            Some((start, end)) if end < body.len() && start <= end => (
                206,
                &body[start..=end],
                Some(format!("bytes {start}-{end}/{}", body.len())),
            ),
            Some(_) => (416, &body[..0], None),
            None => (200, body, None),
        };
        let mut headers = format!(
            "HTTP/1.1 {status} Test\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nConnection: close\r\n",
            selected.len()
        );
        if let Some(content_range) = content_range {
            headers.push_str(&format!("Content-Range: {content_range}\r\n"));
        }
        headers.push_str("\r\n");
        if stream.write_all(headers.as_bytes()).is_err() {
            return;
        }
        for piece in selected.chunks(chunk.max(1)) {
            if let Some(delay) = delay {
                thread::sleep(delay);
            }
            if stream.write_all(piece).is_err() {
                return;
            }
            let _ = stream.flush();
        }
    }

    fn request(
        mirrors: Vec<MirrorSource>,
        destination: PathBuf,
        timeout: Duration,
    ) -> VerifiedDownloadRequest {
        VerifiedDownloadRequest {
            mirror_attempt_timeout: timeout,
            ..VerifiedDownloadRequest::new(mirrors, destination)
        }
    }

    #[test]
    fn trusted_piece_hashes_repair_exactly_the_damaged_piece_from_another_mirror() {
        let dir = temp_dir("selective-repair");
        let destination = dir.join("payload.bin");
        let body = payload();
        let bad = mirror(damage(&body, 1, 0xff), 4096, None);
        let good = mirror(body.clone(), 4096, None);

        let done = download_verified(VerifiedDownloadRequest {
            expected_bytes: Some(body.len() as u64),
            expected_sha256: Some(hex(&body)),
            pieces: Some(piece_map(&body)),
            ..request(
                vec![
                    MirrorSource::with_priority(bad.url.clone(), 1),
                    MirrorSource::with_priority(good.url.clone(), 2),
                ],
                destination.clone(),
                Duration::from_secs(10),
            )
        })
        .unwrap();

        assert_eq!(done.verification, VerificationLevel::PieceHashes);
        assert_eq!(done.repaired_pieces, vec![1]);
        assert_eq!(done.conservative_restarts, 0);
        assert_eq!(done.bytes, body.len() as u64);
        assert_eq!(done.observed_sha256, hex(&body));
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(done.mirrors[0].outcome, MirrorOutcome::Corrupt);
        assert_eq!(done.mirrors[0].corrupt_observations, 1);
        assert!(done.mirrors[0].deprioritised);
        assert_eq!(done.mirrors[1].outcome, MirrorOutcome::Repaired);
        assert_eq!(done.mirrors[1].bytes_delivered, PIECE_LENGTH);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_offline_mirror_fails_over_to_a_healthy_one() {
        let dir = temp_dir("offline-failover");
        let destination = dir.join("payload.bin");
        let body = payload();
        let good = mirror(body.clone(), 4096, None);

        let done = download_verified(VerifiedDownloadRequest {
            expected_bytes: Some(body.len() as u64),
            expected_sha256: Some(hex(&body)),
            pieces: Some(piece_map(&body)),
            ..request(
                vec![
                    MirrorSource::with_priority(offline_url(), 1),
                    MirrorSource::with_priority(good.url.clone(), 2),
                ],
                destination.clone(),
                Duration::from_secs(10),
            )
        })
        .unwrap();

        assert_eq!(done.verification, VerificationLevel::PieceHashes);
        assert!(done.repaired_pieces.is_empty());
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(done.mirrors[0].outcome, MirrorOutcome::Offline);
        assert_eq!(done.mirrors[0].offline_observations, 1);
        assert_eq!(done.mirrors[0].bytes_delivered, 0);
        assert!(done.mirrors[0].deprioritised);
        assert_eq!(done.mirrors[1].outcome, MirrorOutcome::Delivered);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_slow_mirror_is_deprioritised_without_hanging_the_transfer() {
        let dir = temp_dir("slow-deprioritised");
        let destination = dir.join("payload.bin");
        let body = payload();
        let slow = mirror(body.clone(), 128, Some(Duration::from_millis(250)));
        let good = mirror(body.clone(), 4096, None);

        let started = Instant::now();
        let done = download_verified(VerifiedDownloadRequest {
            expected_bytes: Some(body.len() as u64),
            expected_sha256: Some(hex(&body)),
            pieces: Some(piece_map(&body)),
            ..request(
                vec![
                    MirrorSource::with_priority(slow.url.clone(), 1),
                    MirrorSource::with_priority(good.url.clone(), 2),
                ],
                destination.clone(),
                Duration::from_millis(600),
            )
        })
        .unwrap();
        let elapsed = started.elapsed();

        assert_eq!(done.verification, VerificationLevel::PieceHashes);
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(done.mirrors[0].outcome, MirrorOutcome::Slow);
        assert_eq!(done.mirrors[0].slow_observations, 1);
        assert!(done.mirrors[0].deprioritised);
        assert_eq!(done.mirrors[1].outcome, MirrorOutcome::Delivered);
        assert!(
            elapsed < Duration::from_secs(10),
            "the slow mirror stalled the transfer for {elapsed:?}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_final_hash_only_mismatch_restarts_conservatively_and_claims_no_repair() {
        let dir = temp_dir("final-hash-only");
        let destination = dir.join("payload.bin");
        let body = payload();
        let bad = mirror(damage(&body, 1, 0x5a), 4096, None);
        let good = mirror(body.clone(), 4096, None);

        let done = download_verified(VerifiedDownloadRequest {
            expected_bytes: Some(body.len() as u64),
            expected_sha256: Some(hex(&body)),
            pieces: None,
            ..request(
                vec![
                    MirrorSource::with_priority(bad.url.clone(), 1),
                    MirrorSource::with_priority(good.url.clone(), 2),
                ],
                destination.clone(),
                Duration::from_secs(10),
            )
        })
        .unwrap();

        assert_eq!(done.verification, VerificationLevel::FinalHashOnly);
        // No piece map means no fault localization, so no repair may be claimed.
        assert!(done.repaired_pieces.is_empty());
        assert_eq!(done.conservative_restarts, 1);
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert_eq!(done.mirrors[0].outcome, MirrorOutcome::Corrupt);
        assert_eq!(done.mirrors[0].corrupt_observations, 1);
        assert_eq!(done.mirrors[1].outcome, MirrorOutcome::Delivered);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn nothing_is_published_when_every_mirror_fails_verification() {
        let dir = temp_dir("all-corrupt");
        let destination = dir.join("payload.bin");
        let body = payload();
        let first = mirror(damage(&body, 1, 0xff), 4096, None);
        let second = mirror(damage(&body, 1, 0x0f), 4096, None);

        let error = download_verified(VerifiedDownloadRequest {
            expected_bytes: Some(body.len() as u64),
            expected_sha256: Some(hex(&body)),
            pieces: Some(piece_map(&body)),
            ..request(
                vec![
                    MirrorSource::with_priority(first.url.clone(), 1),
                    MirrorSource::with_priority(second.url.clone(), 2),
                ],
                destination.clone(),
                Duration::from_secs(10),
            )
        })
        .unwrap_err();

        assert!(
            matches!(error, VerifiedDownloadError::VerificationFailed { .. }),
            "expected a verification failure, got {error:?}"
        );
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_whole_file_hash_mismatch_is_refused_even_when_every_piece_matches() {
        let dir = temp_dir("piece-vs-final");
        let destination = dir.join("payload.bin");
        let body = payload();
        let only = mirror(body.clone(), 4096, None);

        let error = download_verified(VerifiedDownloadRequest {
            expected_bytes: Some(body.len() as u64),
            expected_sha256: Some(hex(b"a different representation")),
            pieces: Some(piece_map(&body)),
            ..request(
                vec![MirrorSource::with_priority(only.url.clone(), 1)],
                destination.clone(),
                Duration::from_secs(10),
            )
        })
        .unwrap_err();

        assert!(matches!(
            error,
            VerifiedDownloadError::VerificationFailed { .. }
        ));
        assert!(!destination.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bytes_without_a_trusted_digest_are_reported_as_unverified() {
        let dir = temp_dir("unverified");
        let destination = dir.join("payload.bin");
        let body = payload();
        let only = mirror(body.clone(), 4096, None);

        let done = download_verified(request(
            vec![MirrorSource::new(format!(
                "{}?token=private-signed-value",
                only.url
            ))],
            destination.clone(),
            Duration::from_secs(10),
        ))
        .unwrap();

        assert_eq!(done.verification, VerificationLevel::Unverified);
        assert!(done.repaired_pieces.is_empty());
        assert_eq!(done.observed_sha256, hex(&body));
        assert_eq!(fs::read(&destination).unwrap(), body);
        // A signed mirror URL never reaches a report.
        assert!(
            !done.mirrors[0]
                .redacted_url
                .contains("private-signed-value")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_mirror_offline_reports_exhaustion_and_publishes_nothing() {
        let dir = temp_dir("all-offline");
        let destination = dir.join("payload.bin");
        let error = download_verified(request(
            vec![
                MirrorSource::with_priority(offline_url(), 1),
                MirrorSource::with_priority(offline_url(), 2),
            ],
            destination.clone(),
            Duration::from_secs(5),
        ))
        .unwrap_err();

        assert!(
            matches!(error, VerifiedDownloadError::MirrorsExhausted { .. }),
            "expected exhaustion, got {error:?}"
        );
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_non_http_mirror_list_is_refused_before_anything_is_created() {
        let dir = temp_dir("unsupported");
        let destination = dir.join("payload.bin");
        assert!(matches!(
            download_verified(request(
                vec![MirrorSource::new("ftp://example.test/payload.bin")],
                destination.clone(),
                Duration::from_secs(5),
            )),
            Err(VerifiedDownloadError::NoUsableMirror)
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_existing_destination_is_never_replaced() {
        let dir = temp_dir("destination-conflict");
        let destination = dir.join("payload.bin");
        fs::write(&destination, b"keep me").unwrap();
        assert!(matches!(
            download_verified(request(
                vec![MirrorSource::new("http://127.0.0.1:9/payload.bin")],
                destination.clone(),
                Duration::from_secs(5),
            )),
            Err(VerifiedDownloadError::DestinationExists { .. })
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"keep me");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_mirror_delivered_download_reports_a_network_source() {
        let dir = temp_dir("delivery-source");
        let body = payload();
        let server = mirror(body.clone(), 4096, None);
        let destination = dir.join("payload.bin");

        let done = download_verified(VerifiedDownloadRequest {
            expected_bytes: Some(body.len() as u64),
            expected_sha256: Some(hex(&body)),
            ..request(
                vec![MirrorSource::new(server.url.clone())],
                destination,
                Duration::from_secs(5),
            )
        })
        .expect("download succeeds");

        assert_eq!(done.source, DeliverySource::Network);
        assert_eq!(done.verification, VerificationLevel::FinalHashOnly);
        fs::remove_dir_all(dir).unwrap();
    }

    fn open_cache(dir: &Path) -> fetchpath_cache::ContentCache {
        fetchpath_cache::ContentCache::open(
            &dir.join("cache"),
            fetchpath_cache::CacheConfig::new(1 << 20, 1 << 20),
        )
        .expect("cache opens")
    }

    #[test]
    fn a_populated_cache_completes_the_download_with_every_mirror_unreachable() {
        let dir = temp_dir("offline-reuse");
        let body = payload();
        let digest = hex(&body);

        // Seed the cache from a file that is not the destination.
        let seed = dir.join("seed.bin");
        fs::write(&seed, &body).unwrap();
        let mut cache = open_cache(&dir);
        let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid digest");
        cache
            .insert(
                &id,
                &seed,
                fetchpath_cache::CachedVerification::FinalHashOnly,
                fetchpath_cache::Provenance::Public,
            )
            .expect("seeded");

        let destination = dir.join("payload.bin");
        let done = download_verified_cached(
            VerifiedDownloadRequest {
                expected_bytes: Some(body.len() as u64),
                expected_sha256: Some(digest.clone()),
                ..request(
                    vec![MirrorSource::new(offline_url())],
                    destination.clone(),
                    Duration::from_secs(5),
                )
            },
            &mut cache,
        )
        .expect("completes from cache");

        assert_eq!(done.source, DeliverySource::LocalCache);
        assert_eq!(done.verification, VerificationLevel::FinalHashOnly);
        assert_eq!(done.observed_sha256, digest);
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert!(
            done.mirrors.iter().all(|report| report.attempts == 0),
            "no mirror was contacted"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_verified_network_download_populates_the_cache_for_next_time() {
        let dir = temp_dir("cache-populate");
        let body = payload();
        let digest = hex(&body);
        let server = mirror(body.clone(), 4096, None);
        let mut cache = open_cache(&dir);

        let done = download_verified_cached(
            VerifiedDownloadRequest {
                expected_bytes: Some(body.len() as u64),
                expected_sha256: Some(digest.clone()),
                ..request(
                    vec![MirrorSource::new(server.url.clone())],
                    dir.join("payload.bin"),
                    Duration::from_secs(5),
                )
            },
            &mut cache,
        )
        .expect("downloads");
        assert_eq!(done.source, DeliverySource::Network);

        let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid");
        let entry = cache
            .lookup(&id)
            .expect("cached after a verified completion");
        assert_eq!(entry.bytes, body.len() as u64);
        assert_eq!(entry.provenance, fetchpath_cache::Provenance::Public);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_download_carrying_credentials_is_cached_as_credentialed() {
        let dir = temp_dir("cache-credentialed");
        let body = payload();
        let digest = hex(&body);
        let server = mirror(body.clone(), 4096, None);
        let mut cache = open_cache(&dir);

        download_verified_cached(
            VerifiedDownloadRequest {
                expected_bytes: Some(body.len() as u64),
                expected_sha256: Some(digest.clone()),
                context: RequestContext::new(vec!["session=secret".to_owned()], None)
                    .expect("valid context"),
                ..request(
                    vec![MirrorSource::new(server.url.clone())],
                    dir.join("payload.bin"),
                    Duration::from_secs(5),
                )
            },
            &mut cache,
        )
        .expect("downloads");

        let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid");
        let entry = cache.lookup(&id).expect("cached");
        assert_eq!(entry.provenance, fetchpath_cache::Provenance::Credentialed);
        assert!(
            !entry.is_shareable(),
            "credentialed bytes never become shareable"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_download_from_a_signed_url_is_cached_as_credentialed() {
        let dir = temp_dir("cache-signed-url");
        let body = payload();
        let digest = hex(&body);
        let server = mirror(body.clone(), 4096, None);
        let mut cache = open_cache(&dir);

        // No cookies, but the mirror URL carries a query string. The report's
        // redacted URL cannot show this, so provenance must be read from the
        // original source list instead.
        let signed = format!("{}?token=secret", server.url);
        download_verified_cached(
            VerifiedDownloadRequest {
                expected_bytes: Some(body.len() as u64),
                expected_sha256: Some(digest.clone()),
                ..request(
                    vec![MirrorSource::new(signed)],
                    dir.join("payload.bin"),
                    Duration::from_secs(5),
                )
            },
            &mut cache,
        )
        .expect("downloads");

        let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid");
        let entry = cache.lookup(&id).expect("cached");
        assert_eq!(entry.provenance, fetchpath_cache::Provenance::Credentialed);
        assert!(!entry.is_shareable());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_unverified_download_is_never_cached() {
        let dir = temp_dir("cache-unverified");
        let body = payload();
        let server = mirror(body.clone(), 4096, None);
        let mut cache = open_cache(&dir);

        // No expected digest and no piece map: nothing trusted to key on.
        let done = download_verified_cached(
            request(
                vec![MirrorSource::new(server.url.clone())],
                dir.join("payload.bin"),
                Duration::from_secs(5),
            ),
            &mut cache,
        )
        .expect("downloads");

        assert_eq!(done.verification, VerificationLevel::Unverified);
        assert_eq!(done.source, DeliverySource::Network);
        assert_eq!(
            cache.entries().len(),
            0,
            "unverified bytes are not eligible"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_tampered_cache_entry_falls_through_to_the_network_rather_than_failing() {
        let dir = temp_dir("cache-tampered-fallthrough");
        let body = payload();
        let digest = hex(&body);
        let server = mirror(body.clone(), 4096, None);

        let seed = dir.join("seed.bin");
        fs::write(&seed, &body).unwrap();
        let mut cache = open_cache(&dir);
        let id = fetchpath_cache::ContentId::from_expected_sha256(&digest).expect("valid");
        cache
            .insert(
                &id,
                &seed,
                fetchpath_cache::CachedVerification::FinalHashOnly,
                fetchpath_cache::Provenance::Public,
            )
            .expect("seeded");

        fs::write(cache.path_for(&id), b"not the promised bytes").unwrap();

        let destination = dir.join("payload.bin");
        let done = download_verified_cached(
            VerifiedDownloadRequest {
                expected_bytes: Some(body.len() as u64),
                expected_sha256: Some(digest.clone()),
                ..request(
                    vec![MirrorSource::new(server.url.clone())],
                    destination.clone(),
                    Duration::from_secs(5),
                )
            },
            &mut cache,
        )
        .expect("falls through and succeeds");

        assert_eq!(done.source, DeliverySource::Network);
        assert_eq!(fs::read(&destination).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_piece_identified_download_round_trips_through_the_cache() {
        let dir = temp_dir("cache-pieces");
        let body = payload();
        let map = piece_map(&body);

        let seed = dir.join("seed.bin");
        fs::write(&seed, &body).unwrap();
        let mut cache = open_cache(&dir);
        let id = fetchpath_cache::ContentId::from_piece_map(&map);
        cache
            .insert(
                &id,
                &seed,
                fetchpath_cache::CachedVerification::PieceHashes,
                fetchpath_cache::Provenance::Public,
            )
            .expect("seeded");

        let destination = dir.join("payload.bin");
        let done = download_verified_cached(
            VerifiedDownloadRequest {
                pieces: Some(map),
                ..request(
                    vec![MirrorSource::new(offline_url())],
                    destination.clone(),
                    Duration::from_secs(5),
                )
            },
            &mut cache,
        )
        .expect("completes from cache");

        assert_eq!(done.source, DeliverySource::LocalCache);
        assert_eq!(done.verification, VerificationLevel::PieceHashes);
        assert!(done.repaired_pieces.is_empty(), "nothing was repaired");
        assert_eq!(fs::read(&destination).unwrap(), body);
        fs::remove_dir_all(dir).unwrap();
    }
}
