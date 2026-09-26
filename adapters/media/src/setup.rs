//! Guided setup for the media helpers.
//!
//! Saving video or audio needs two third-party programs, `yt-dlp` and
//! `ffmpeg`, which Fetchpath does not redistribute. Before this module the only
//! way to supply them was an environment variable, so the first user who chose
//! "Video or audio" met a bare `helper_unavailable` and no way forward.
//!
//! ## What this module will and will not do
//!
//! It will detect helpers the user already has, accept a folder they point at,
//! and install a **pinned** artifact whose SHA-256 is recorded in
//! [`media-tools.json`] beside this source file.
//!
//! It will not install anything whose digest is absent or does not match. That
//! is the whole point of the pin: the recorded digest is the maintainer's
//! statement about a specific artifact, checked at `tools/media-tools/pin.mjs`
//! time against what the publisher served. A digest computed here from whatever
//! arrived would only prove the bytes survived the network, which is not an
//! authenticity claim and must never be presented as one.
//!
//! An entry with no recorded digest is reported to the interface as
//! `pinnedDigestMissing`, and the guided download for it is refused. The manual
//! path stays available so the feature still works.
//!
//! The desktop's Settings and the terminal's `fetchpath tools` both use it,
//! so there is one installer and one pin list.
//!
//! [`media-tools.json`]: ../media-tools.json

use crate::MediaTools;
use fetchpath_core::CancellationToken;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The pinned source list, compiled in so a tampered file beside the executable
/// cannot redirect an install.
const MANIFEST: &str = include_str!("../media-tools.json");

/// Upper bound on a helper download. Both helpers are far below this; the cap
/// stops an unexpected redirect from filling the user's disk.
const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    tools: Vec<ManifestEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    /// `yt-dlp` or `ffmpeg`.
    name: String,
    /// The exact published version this entry pins.
    version: String,
    /// Where the artifact is published.
    url: String,
    /// The publisher's own checksum page or file, for a human to confirm.
    checksum_source: String,
    /// SHA-256 of the artifact, recorded by `tools/media-tools/pin.mjs`.
    /// Empty means unpinned, and an unpinned entry is never installed.
    #[serde(default)]
    sha256: String,
    /// `executable` installs the file directly; `zip` extracts it first.
    kind: ArtifactKind,
    /// License the artifact is distributed under, shown before installing.
    license: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum ArtifactKind {
    Executable,
    Zip,
}

/// What the interface needs to explain the current state to the user.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsStatus {
    /// True when both helpers resolved and answered `--version`.
    pub ready: bool,
    pub yt_dlp_path: Option<String>,
    pub ffmpeg_dir: Option<String>,
    /// Reported by the helpers themselves, never inferred from a file name.
    pub yt_dlp_version: Option<String>,
    pub ffmpeg_version: Option<String>,
    /// Where a guided install would put them.
    pub install_dir: String,
    /// One entry per pinned artifact, so the interface can name versions,
    /// licenses and whether a guided install is possible at all.
    pub available: Vec<AvailableTool>,
    /// Present when the helpers resolved but could not be run.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableTool {
    pub name: String,
    pub version: String,
    pub url: String,
    pub checksum_source: String,
    pub license: String,
    /// False when this build carries no recorded digest for the artifact, in
    /// which case the guided download is refused and the manual path is the
    /// only one offered.
    pub pinned: bool,
}

fn manifest() -> Manifest {
    serde_json::from_str(MANIFEST).expect("media-tools.json is compiled in and must parse")
}

/// Reports what is installed, where, and what a guided install would fetch.
pub fn status(configured_dir: Option<&str>, install_dir: &Path) -> ToolsStatus {
    let available = pinned_tools();

    let mut status = ToolsStatus {
        install_dir: install_dir.display().to_string(),
        available,
        ..ToolsStatus::default()
    };

    let resolved = configured_dir
        .map(PathBuf::from)
        .and_then(|root| MediaTools::discover_in(&root).ok())
        .or_else(|| MediaTools::discover_in(install_dir).ok())
        .or_else(|| MediaTools::discover().ok());

    let Some(tools) = resolved else {
        return status;
    };
    status.yt_dlp_path = Some(tools.yt_dlp.display().to_string());
    status.ffmpeg_dir = Some(tools.ffmpeg_dir.display().to_string());

    // Presence on disk is not readiness. A helper that will not run is a
    // clearer thing to say now than a failed download later.
    match tools.versions() {
        Ok((yt_dlp, ffmpeg)) => {
            status.yt_dlp_version = Some(yt_dlp);
            status.ffmpeg_version = Some(ffmpeg);
            status.ready = true;
        }
        Err(error) => status.problem = Some(error.to_string()),
    }
    status
}

/// Validates a folder the user picked, returning it only if both helpers are
/// actually usable from it.
pub fn use_directory(root: &Path) -> Result<String, String> {
    let tools = MediaTools::discover_in(root).map_err(|_| {
        format!(
            "{} does not contain yt-dlp and ffmpeg. Choose the folder that holds both programs.",
            root.display()
        )
    })?;
    tools
        .versions()
        .map_err(|error| format!("Those programs could not be run: {error}"))?;
    Ok(root.display().to_string())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub installed: Vec<String>,
    pub install_dir: String,
}

/// One helper about to be downloaded, for a progress display. `token`
/// reports its bytes received and stated size, and cancels it.
pub struct Step<'a> {
    pub tool: &'a AvailableTool,
    /// From 1.
    pub number: usize,
    pub count: usize,
    pub token: CancellationToken,
}

/// The pinned helpers a guided install would fetch, with versions, sources
/// and licences, for showing before anything is downloaded.
pub fn pinned_tools() -> Vec<AvailableTool> {
    manifest()
        .tools
        .iter()
        .map(|entry| AvailableTool {
            name: entry.name.clone(),
            version: entry.version.clone(),
            url: entry.url.clone(),
            checksum_source: entry.checksum_source.clone(),
            license: entry.license.clone(),
            pinned: !entry.sha256.is_empty(),
        })
        .collect()
}

/// Downloads and installs every pinned helper that is not already present.
///
/// Each artifact is fetched through the same verified transfer path the rest of
/// Fetchpath uses, checked against its recorded digest, and only then moved
/// into place. A digest mismatch removes the download and stops.
pub fn install(install_dir: &Path) -> Result<InstallOutcome, String> {
    install_with(install_dir, |_| {})
}

/// [`install`], calling `on_step` as each helper's download begins.
pub fn install_with(
    install_dir: &Path,
    mut on_step: impl FnMut(Step<'_>),
) -> Result<InstallOutcome, String> {
    let manifest = manifest();
    let tools = pinned_tools();
    fs::create_dir_all(install_dir)
        .map_err(|error| format!("Could not create {}: {error}", install_dir.display()))?;

    let mut installed = Vec::new();
    let count = manifest.tools.len();
    for (index, entry) in manifest.tools.iter().enumerate() {
        if entry.sha256.is_empty() {
            return Err(format!(
                "This build has no recorded checksum for {} {}, so it will not download it. \
                 Install it yourself and point Fetchpath at the folder, or use a build where \
                 `node tools/media-tools/pin.mjs` has recorded the digest.",
                entry.name, entry.version
            ));
        }
        if is_present(entry, install_dir) {
            continue;
        }
        let token = CancellationToken::default();
        on_step(Step {
            tool: &tools[index],
            number: index + 1,
            count,
            token: token.clone(),
        });
        install_entry(entry, install_dir, token)?;
        installed.push(format!("{} {}", entry.name, entry.version));
    }

    // Prove the result rather than assuming it: the helpers have to resolve and
    // run from the directory before this reports success.
    let tools = MediaTools::discover_in(install_dir).map_err(|_| {
        "The helpers were downloaded but could not be found afterwards.".to_string()
    })?;
    tools
        .versions()
        .map_err(|error| format!("The helpers were installed but would not run: {error}"))?;

    Ok(InstallOutcome {
        installed,
        install_dir: install_dir.display().to_string(),
    })
}

fn is_present(entry: &ManifestEntry, install_dir: &Path) -> bool {
    match entry.kind {
        ArtifactKind::Executable => install_dir
            .join(MediaTools::executable_name(&entry.name))
            .is_file(),
        ArtifactKind::Zip => {
            // An ffmpeg release brings ffmpeg and ffprobe together, in either
            // the folder root or a `bin` subdirectory.
            let candidates = [install_dir.to_path_buf(), install_dir.join("bin")];
            candidates.iter().any(|dir| {
                dir.join(MediaTools::executable_name("ffmpeg")).is_file()
                    && dir.join(MediaTools::executable_name("ffprobe")).is_file()
            })
        }
    }
}

fn install_entry(
    entry: &ManifestEntry,
    install_dir: &Path,
    token: CancellationToken,
) -> Result<(), String> {
    let staging = install_dir.join(format!(".{}-download", entry.name));
    let _ = fs::remove_file(&staging);
    let _ = fs::remove_dir_all(&staging);

    let downloaded = fetch(&entry.url, &staging, &entry.sha256, token)?;
    let digest = sha256_file(&downloaded)
        .map_err(|error| format!("Could not read the downloaded file: {error}"))?;
    if !digest.eq_ignore_ascii_case(&entry.sha256) {
        let _ = fs::remove_file(&downloaded);
        return Err(format!(
            "The download of {} {} did not match its recorded checksum and was discarded. \
             Expected {}, received {}.",
            entry.name, entry.version, entry.sha256, digest
        ));
    }

    match entry.kind {
        ArtifactKind::Executable => {
            let target = install_dir.join(MediaTools::executable_name(&entry.name));
            fs::rename(&downloaded, &target)
                .map_err(|error| format!("Could not place {}: {error}", target.display()))?;
        }
        ArtifactKind::Zip => {
            // Unpacked apart from the install folder: that folder may already
            // hold another helper, and the wrapper directory can only be
            // recognised as the sole entry of a folder of its own.
            let unpack = install_dir.join(format!(".{}-unpack", entry.name));
            let _ = fs::remove_dir_all(&unpack);
            fs::create_dir_all(&unpack)
                .map_err(|error| format!("Could not create {}: {error}", unpack.display()))?;
            let extracted = extract_zip(&downloaded, &unpack)
                .and_then(|()| move_unpacked_into(&unpack, install_dir));
            let _ = fs::remove_file(&downloaded);
            let _ = fs::remove_dir_all(&unpack);
            extracted?;
        }
    }
    Ok(())
}

/// Fetches one artifact through the verified core download path.
/// The engine refuses to publish bytes that do not match `sha256`; the caller
/// still checks again before installing anything.
fn fetch(
    url: &str,
    staging: &Path,
    sha256: &str,
    cancellation: CancellationToken,
) -> Result<PathBuf, String> {
    use fetchpath_core::{CancelCleanup, DownloadRequest, download};

    let request = DownloadRequest {
        url: url.to_string(),
        destination: staging.to_path_buf(),
        cancellation,
        cancel_cleanup: CancelCleanup::RemoveStaging,
        context: fetchpath_core::RequestContext::default(),
        expected_sha256: fetchpath_core::normalize_sha256(sha256),
    };
    let done = download(request).map_err(|error| format!("Could not download {url}: {error}"))?;
    let length = fs::metadata(&done.destination)
        .map(|meta| meta.len())
        .unwrap_or_default();
    if length > MAX_ARTIFACT_BYTES {
        let _ = fs::remove_file(&done.destination);
        return Err(format!(
            "{url} returned more than the {MAX_ARTIFACT_BYTES} byte limit."
        ));
    }
    Ok(done.destination)
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// Extracts a zip using the `tar` that ships with Windows 10 1803 and later.
///
/// Fetchpath is a Windows application and bsdtar is part of the operating
/// system, so this avoids adding an archive library to the dependency graph for
/// one setup step. The archive's digest was already checked against the pin
/// before this runs.
fn extract_zip(archive: &Path, into: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let mut command = Command::new("tar");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flash during guided setup.
        command.creation_flags(0x0800_0000);
    }
    let status = command
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("Could not run the system archive extractor: {error}"))?;
    if !status.success() {
        return Err("The downloaded archive could not be extracted.".into());
    }
    Ok(())
}

/// Lifts the contents of a single wrapper directory up one level.
///
/// Published ffmpeg archives contain one top-level folder named for the build.
/// Left in place, the helpers sit two directories below where discovery looks.
fn flatten_single_root(install_dir: &Path) -> Result<(), String> {
    let entries: Vec<_> = fs::read_dir(install_dir)
        .map_err(|error| format!("Could not read {}: {error}", install_dir.display()))?
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .collect();
    let [wrapper] = entries.as_slice() else {
        return Ok(());
    };
    if !wrapper.path().is_dir() {
        return Ok(());
    }
    let inner: Vec<_> = fs::read_dir(wrapper.path())
        .map_err(|error| format!("Could not read {}: {error}", wrapper.path().display()))?
        .filter_map(Result::ok)
        .collect();
    for item in inner {
        let target = install_dir.join(item.file_name());
        if target.exists() {
            continue;
        }
        fs::rename(item.path(), &target)
            .map_err(|error| format!("Could not move {}: {error}", target.display()))?;
    }
    let _ = fs::remove_dir_all(wrapper.path());
    Ok(())
}

/// Flattens an unpacked archive and moves its contents into the install
/// folder. A same-named leftover from an earlier incomplete install is
/// replaced: `is_present` already found it unusable.
fn move_unpacked_into(unpack: &Path, install_dir: &Path) -> Result<(), String> {
    flatten_single_root(unpack)?;
    let items: Vec<_> = fs::read_dir(unpack)
        .map_err(|error| format!("Could not read {}: {error}", unpack.display()))?
        .filter_map(Result::ok)
        .collect();
    for item in items {
        let target = install_dir.join(item.file_name());
        if target.is_dir() {
            let _ = fs::remove_dir_all(&target);
        } else if target.exists() {
            let _ = fs::remove_file(&target);
        }
        fs::rename(item.path(), &target)
            .map_err(|error| format!("Could not move {}: {error}", target.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fetchpath-media-setup-{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_compiled_manifest_parses_and_names_both_helpers() {
        let manifest = manifest();
        let names: Vec<_> = manifest
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        assert!(names.contains(&"yt-dlp"), "manifest names: {names:?}");
        assert!(names.contains(&"ffmpeg"), "manifest names: {names:?}");
        for tool in &manifest.tools {
            assert!(
                tool.url.starts_with("https://"),
                "{} must be fetched over TLS",
                tool.name
            );
            assert!(!tool.license.is_empty(), "{} needs a license", tool.name);
            assert!(
                !tool.checksum_source.is_empty(),
                "{} needs a checksum source a person can check",
                tool.name
            );
        }
    }

    #[test]
    fn an_unpinned_entry_is_refused_rather_than_downloaded() {
        let dir = temp_dir("unpinned");
        let unpinned: Vec<_> = manifest()
            .tools
            .into_iter()
            .filter(|tool| tool.sha256.is_empty())
            .collect();
        if unpinned.is_empty() {
            // Every entry is pinned in this build, so there is nothing to refuse.
            fs::remove_dir_all(&dir).unwrap();
            return;
        }
        let error = install(&dir).unwrap_err();
        assert!(
            error.contains("no recorded checksum"),
            "an unpinned artifact must be refused by name, got: {error}"
        );
        // Nothing may be left behind by a refused install.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn status_reports_not_ready_for_an_empty_directory() {
        let dir = temp_dir("empty");
        let status = status(Some(&dir.display().to_string()), &dir);
        assert!(!status.ready);
        assert_eq!(status.install_dir, dir.display().to_string());
        assert_eq!(status.available.len(), manifest().tools.len());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_without_the_helpers_is_rejected_with_the_path_named() {
        let dir = temp_dir("wrong-folder");
        let error = use_directory(&dir).unwrap_err();
        assert!(error.contains(&dir.display().to_string()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_single_wrapper_directory_is_flattened_into_place() {
        let dir = temp_dir("flatten");
        let wrapper = dir.join("ffmpeg-7.1-essentials_build");
        fs::create_dir_all(wrapper.join("bin")).unwrap();
        fs::write(wrapper.join("bin").join("ffmpeg.exe"), b"binary").unwrap();
        fs::write(wrapper.join("LICENSE"), b"license").unwrap();

        flatten_single_root(&dir).unwrap();

        assert!(dir.join("bin").join("ffmpeg.exe").is_file());
        assert!(dir.join("LICENSE").is_file());
        assert!(!wrapper.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn flattening_leaves_an_already_flat_directory_alone() {
        let dir = temp_dir("already-flat");
        fs::write(dir.join("yt-dlp.exe"), b"binary").unwrap();
        fs::write(dir.join("ffmpeg.exe"), b"binary").unwrap();

        flatten_single_root(&dir).unwrap();

        assert!(dir.join("yt-dlp.exe").is_file());
        assert!(dir.join("ffmpeg.exe").is_file());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Regression: yt-dlp is installed first, so the ffmpeg archive used to be
    /// unpacked beside it, the wrapper was never the folder's only entry, and
    /// ffmpeg stayed two levels below where discovery looks.
    #[test]
    fn an_archive_is_flattened_even_when_another_helper_is_already_installed() {
        let install = temp_dir("beside-yt-dlp");
        fs::write(install.join("yt-dlp.exe"), b"binary").unwrap();
        let unpack = install.join(".ffmpeg-unpack");
        let wrapper = unpack.join("ffmpeg-9.0.2-essentials_build");
        fs::create_dir_all(wrapper.join("bin")).unwrap();
        fs::write(wrapper.join("bin").join("ffmpeg.exe"), b"binary").unwrap();
        fs::write(wrapper.join("bin").join("ffprobe.exe"), b"binary").unwrap();

        move_unpacked_into(&unpack, &install).unwrap();

        assert!(install.join("yt-dlp.exe").is_file());
        assert!(install.join("bin").join("ffmpeg.exe").is_file());
        assert!(install.join("bin").join("ffprobe.exe").is_file());
        fs::remove_dir_all(&install).unwrap();
    }

    /// The real guided path: fetch every pinned helper from its publisher,
    /// verify it against the recorded digest, install it, and prove it runs.
    /// Needs network access, so it is run by hand:
    /// `cargo test -p fetchpath-desktop --locked -- --ignored guided_install`
    #[test]
    #[ignore = "downloads the pinned helpers from their publishers"]
    fn guided_install_fetches_verifies_and_runs_the_pinned_helpers() {
        let dir = temp_dir("guided");
        let outcome = install(&dir).expect("the pinned helpers install and run");
        assert_eq!(outcome.installed.len(), manifest().tools.len());

        let status = status(None, &dir);
        assert!(
            status.ready,
            "installed helpers must report ready: {:?}",
            status.problem
        );
        assert!(status.available.iter().all(|tool| tool.pinned));

        // A second run finds everything present and fetches nothing.
        assert!(install(&dir).unwrap().installed.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
