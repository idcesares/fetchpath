//! URL, path, filename, media-page, and torrent metadata validation helpers.

use crate::record::QueueRecord;
use crate::{MAX_DESTINATION_LENGTH, MAX_SOURCE_LENGTH, MAX_TORRENT_METADATA_BYTES};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf, Prefix};
/// Sites whose pages are video or audio rather than files. Kept in step with
/// `MEDIA_HOSTS` in the desktop interface, which uses it for pasted links.
pub(crate) const MEDIA_HOSTS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "vimeo.com",
    "dailymotion.com",
    "dai.ly",
    "twitch.tv",
    "tiktok.com",
    "instagram.com",
    "facebook.com",
    "fb.watch",
    "x.com",
    "twitter.com",
    "soundcloud.com",
    "bandcamp.com",
    "bilibili.com",
    "rumble.com",
    "odysee.com",
    "streamable.com",
    "ted.com",
    "reddit.com",
    "v.redd.it",
    "mixcloud.com",
    "nicovideo.jp",
    "archive.org",
];

/// File extensions that always indicate a direct file rather than a media page.
/// Kept in step with `FILE_EXTENSIONS` in `apps/desktop/src/main.ts`.
pub(crate) const MEDIA_DIRECT_FILE_EXTENSIONS: &[&str] = &[
    "7z", "aab", "apk", "appx", "bin", "bz2", "cab", "csv", "deb", "dmg", "doc", "docx", "epub",
    "exe", "flac", "gz", "img", "iso", "jar", "json", "m4a", "mkv", "mobi", "mov", "mp3", "mp4",
    "msi", "msix", "ogg", "pdf", "pkg", "ppt", "pptx", "rar", "rpm", "svg", "tar", "tgz", "txt",
    "wav", "webm", "xls", "xlsx", "xml", "xz", "zip", "zst", "png", "jpg", "jpeg", "gif", "webp",
];

/// A page on a known media site, not a direct file on it (an `.mp4` on
/// archive.org is still a file, while dotted handles like `@john.doe` or
/// `ubuntu-24.04` remain media pages).
pub(crate) fn is_media_page(raw: &str) -> bool {
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let host = ["www.", "m.", "music."]
        .iter()
        .find_map(|prefix| host.strip_prefix(prefix))
        .unwrap_or(&host)
        .to_owned();
    let on_media_site = MEDIA_HOSTS
        .iter()
        .any(|media| host == *media || host.ends_with(&format!(".{media}")));
    let last = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("");
    let has_file_extension = last.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && MEDIA_DIRECT_FILE_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
    });
    on_media_site && !has_file_extension
}

pub(crate) fn unique_browser_destination(
    directory: &Path,
    filename: &str,
    records: &[QueueRecord],
) -> PathBuf {
    let candidate = directory.join(filename);
    if !candidate.exists() && !records.iter().any(|record| record.destination == candidate) {
        return candidate;
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("download");
    let extension = path.extension().and_then(|value| value.to_str());
    for index in 2..10_000 {
        let name = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        let candidate = directory.join(name);
        if !candidate.exists() && !records.iter().any(|record| record.destination == candidate) {
            return candidate;
        }
    }
    directory.join(format!("{}-{}", uuid::Uuid::new_v4(), filename))
}

pub(crate) fn display_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or(url);
    let redacted = redact_userinfo(base);
    if redacted == url {
        redacted
    } else {
        format!("{redacted}?…")
    }
}

/// Replaces any `user:password@` section of an address with a fixed marker so a
/// credential can never reach queue JSON, an event payload, or an error string.
pub(crate) fn redact_userinfo(base: &str) -> String {
    let Some(scheme_end) = base.find("://") else {
        return base.to_owned();
    };
    let authority_start = scheme_end + 3;
    let authority_end = base[authority_start..]
        .find('/')
        .map_or(base.len(), |offset| authority_start + offset);
    let authority = &base[authority_start..authority_end];
    match authority.rfind('@') {
        None => base.to_owned(),
        Some(at) => format!(
            "{}…@{}{}",
            &base[..authority_start],
            &authority[at + 1..],
            &base[authority_end..]
        ),
    }
}

/// Validates an address arriving over IPC. Every rejection is a user-facing
/// sentence and never echoes the raw value, which may carry a signed query.
pub(crate) fn validated_source(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("Add at least one download address.".into());
    }
    if url.len() > MAX_SOURCE_LENGTH {
        return Err("That download address is too long to queue safely.".into());
    }
    if url.chars().any(char::is_control) {
        return Err("That download address contains characters Fetchpath cannot use.".into());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!(
            "{} is not an HTTP or HTTPS address.",
            display_url(url)
        ));
    }
    let parsed = url::Url::parse(url)
        .map_err(|_| "That download address could not be read as a web address.".to_string())?;
    if parsed.host_str().is_none() {
        return Err("That download address is missing a host name.".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(
            "Addresses that embed a user name or password are not accepted. Remove the credentials from the link."
                .into(),
        );
    }
    Ok(url.to_owned())
}

pub(crate) fn validated_torrent_source(raw: &str) -> Result<String, String> {
    let source = raw.trim();
    if source.is_empty() || source.len() > MAX_SOURCE_LENGTH || source.chars().any(char::is_control)
    {
        return Err("That torrent or magnet link cannot be queued safely.".into());
    }
    if source.starts_with("magnet:?") {
        let parsed = url::Url::parse(source)
            .map_err(|_| "That magnet link could not be read.".to_string())?;
        let has_hash = parsed.query_pairs().any(|(key, value)| {
            key == "xt" && (value.starts_with("urn:btih:") || value.starts_with("urn:btmh:"))
        });
        if !has_hash {
            return Err("That magnet link has no supported torrent hash.".into());
        }
        return Ok(source.to_owned());
    }
    if source.starts_with("https://") {
        let parsed = url::Url::parse(source)
            .map_err(|_| "That torrent link could not be read.".to_string())?;
        if parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err("That torrent link has an invalid host or embedded credentials.".into());
        }
        return Ok(source.to_owned());
    }
    Err("Use a magnet link or HTTPS torrent link.".into())
}

/// Validates a destination arriving over IPC. The path must be absolute and free
/// of traversal, so a renderer cannot steer a write outside the folder the user
/// actually chose, and the leaf must be a name Windows can really create.
/// Whether the helper's reported folder is a single new name directly in the root.
pub(crate) fn published_inside(root: &Path, published: &Path) -> bool {
    published.parent() == Some(root)
        && published
            .components()
            .next_back()
            .is_some_and(|last| matches!(last, Component::Normal(_)))
}

/// A torrent's destination: a new folder, or for an automatic torrent the
/// existing root folder it is published into, which may be a drive root.
pub(crate) fn validated_torrent_destination(raw: &str, auto: bool) -> Result<PathBuf, String> {
    if !auto {
        return validated_destination(raw);
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_DESTINATION_LENGTH {
        return Err("Choose a folder for this torrent.".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("That destination path contains characters Windows cannot use.".into());
    }
    let path = PathBuf::from(trimmed);
    plain_path(&path)?;
    Ok(path)
}

pub(crate) fn validated_destination(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Every queued download needs a destination filename.".into());
    }
    if trimmed.len() > MAX_DESTINATION_LENGTH {
        return Err("That destination path is too long.".into());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("That destination path contains characters Windows cannot use.".into());
    }
    let path = PathBuf::from(trimmed);
    if path.file_name().and_then(|name| name.to_str()).is_none() {
        return Err("Every queued download needs a destination filename.".into());
    }
    plain_path(&path)?;
    Ok(path)
}

/// Whether `path` is an ordinary full path Windows applies its name rules
/// to, with every folder and name a plain one. Shared by destinations, rule
/// folders and the default folder, so a folder accepted there is never one a
/// download into it would be refused for (FP-067).
pub(crate) fn plain_path(path: &Path) -> Result<(), String> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("A destination path cannot contain \"..\".".into());
    }
    // A drive path or a network share. Device (`\\.\`) and verbatim
    // (`\\?\`) paths skip the rules Windows applies to ordinary names, and a
    // folder part such as `CON` or `a:stream` would then reach a device or a
    // hidden stream (FP-067).
    let ordinary = match path.components().next() {
        Some(Component::Prefix(prefix)) => {
            matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(..))
        }
        _ => false,
    };
    if !path.is_absolute() || !ordinary {
        return Err("Choose a full destination path, including its drive.".into());
    }
    // Every folder and the file name must be a plain name. A colon names an
    // NTFS alternate data stream, so `notes.txt:hidden` would attach bytes to
    // a file the person never chose while still satisfying the create-only
    // publication fence. The drive letter's colon lives in the prefix.
    for component in path.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        let Some(part) = part.to_str() else {
            return Err("That destination path contains characters Windows cannot use.".into());
        };
        if part != part.trim_end_matches(['.', ' ']) {
            return Err(format!(
                "{part:?}: a destination filename or folder cannot end with a dot or a space."
            ));
        }
        if let Some(offending) = part.chars().find(|character| {
            matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
        }) {
            return Err(format!(
                "A destination filename or folder cannot contain {offending}."
            ));
        }
        // A name must read as what it is on the person's approval card:
        // embeddings, overrides and isolates reorder what follows (a name
        // ending `\u{202E}txt.exe` looks like a text file), and line or
        // paragraph separators break the line. Joiners and marks that do
        // neither stay allowed: Persian, Indic and emoji names need them
        // (FP-067).
        if part.chars().any(|character| {
            matches!(
                character,
                '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
            )
        }) {
            return Err(
                "A destination name cannot contain characters that reorder or break its text."
                    .into(),
            );
        }
        if is_reserved_device_name(part) {
            return Err(format!(
                "{part:?} is reserved by Windows; choose another name."
            ));
        }
    }
    Ok(())
}

/// Names Windows keeps for devices, with or without an extension: `CON`,
/// `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, and `COM` or `LPT` followed by
/// 1 to 9 or a superscript 1 to 3.
pub(crate) fn is_reserved_device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = stem.chars();
    let head: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (head == "COM" || head == "LPT")
        && matches!(
            rest.as_slice(),
            ['1'..='9'] | ['\u{b9}' | '\u{b2}' | '\u{b3}']
        )
}

pub(crate) fn restartable_url(url: &str) -> Option<String> {
    (!url.contains(['?', '#'])).then(|| url.to_owned())
}

pub(crate) fn torrent_metadata_path(state_path: &Path, id: &str) -> Option<PathBuf> {
    uuid::Uuid::parse_str(id).ok()?;
    Some(
        state_path
            .parent()?
            .join("torrent-metadata")
            .join(format!("{id}.torrent")),
    )
}

pub(crate) fn verify_torrent_metadata(path: &Path, expected: &str) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid metadata hash",
        ));
    }
    let attributes = fs::symlink_metadata(path)?;
    if !attributes.file_type().is_file() || attributes.len() > MAX_TORRENT_METADATA_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid metadata file",
        ));
    }
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > MAX_TORRENT_METADATA_BYTES
        || format!("{:x}", Sha256::digest(bytes)) != expected.to_ascii_lowercase()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "metadata hash changed",
        ));
    }
    Ok(())
}
