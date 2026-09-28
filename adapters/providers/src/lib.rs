//! Model and dataset repositories as downloads (FP-022).
//!
//! A repository link resolves to one exact commit and the files in it, each
//! with the identity the provider states: the SHA-256 of a large (LFS or Xet)
//! file, the size of every file. The files are then ordinary downloads
//! pinned to that commit, so a branch moving during the download cannot mix
//! two versions, and every large file is published only if it matches.
//!
//! Hugging Face is the one provider. Public repositories only: a gated or
//! private one is refused in plain words, because Fetchpath holds no
//! Hugging Face sign-in.

use serde::Deserialize;
use std::time::Duration;

/// The most files one repository may list.
pub const MAX_FILES: usize = 10_000;
/// The most bytes a listing may take.
pub const MAX_LISTING_BYTES: usize = 32 * 1024 * 1024;
const HOST: &str = "https://huggingface.co";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepoKind {
    Model,
    Dataset,
    Space,
}

impl RepoKind {
    fn api(self) -> &'static str {
        match self {
            Self::Model => "models",
            Self::Dataset => "datasets",
            Self::Space => "spaces",
        }
    }

    /// The prefix in a file's link: none for a model.
    fn prefix(self) -> &'static str {
        match self {
            Self::Model => "",
            Self::Dataset => "datasets/",
            Self::Space => "spaces/",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Dataset => "dataset",
            Self::Space => "space",
        }
    }
}

/// A repository link, parsed but not yet resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoRef {
    pub kind: RepoKind,
    /// `owner/name`, or a bare legacy name such as `gpt2`.
    pub repo: String,
    /// A branch, tag or commit; `main` when the link names none.
    pub revision: String,
    /// Only files at or under this path, when the link names one.
    pub path: Option<String>,
}

/// Recognizes `hf://[datasets/|spaces/]OWNER/NAME[@REVISION][/PATH]` and
/// `https://huggingface.co/[datasets/|spaces/]OWNER/NAME[/tree|blob|resolve/REVISION[/PATH]]`.
/// Anything else, including other hosts, is not a repository link.
pub fn parse(link: &str) -> Option<RepoRef> {
    let link = link.trim();
    let (rest, url_form) = if let Some(rest) = link.strip_prefix("hf://") {
        (rest, false)
    } else {
        let rest = link
            .strip_prefix("https://huggingface.co/")
            .or_else(|| link.strip_prefix("https://www.huggingface.co/"))?;
        (rest, true)
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let mut segments: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    let kind = match segments.first() {
        Some(&"datasets") => RepoKind::Dataset,
        Some(&"spaces") => RepoKind::Space,
        _ => RepoKind::Model,
    };
    if kind != RepoKind::Model {
        segments.remove(0);
    }
    if segments.is_empty() {
        return None;
    }
    // A reserved first segment is a page of the site, not a repository.
    const SITE: [&str; 9] = [
        "api", "docs", "blog", "login", "join", "settings", "pricing", "models", "tasks",
    ];
    if url_form && kind == RepoKind::Model && SITE.contains(&segments[0]) {
        return None;
    }
    let owner_and_name = |segments: &[&str]| -> Option<(String, usize)> {
        match segments {
            [one] => Some((strip_revision(one).0.to_owned(), 1)),
            [owner, name, ..] if !matches!(*name, "tree" | "blob" | "resolve") => {
                Some((format!("{owner}/{}", strip_revision(name).0), 2))
            }
            [one, ..] => Some((one.to_string(), 1)),
            [] => None,
        }
    };
    let (repo, used) = owner_and_name(&segments)?;
    if !repo.split('/').all(valid_repo_part) {
        return None;
    }
    let tail = &segments[used..];
    let (revision, path) = if url_form {
        match tail {
            [] => ("main".to_owned(), None),
            ["tree" | "blob" | "resolve", revision, path @ ..] => (
                decode(revision)?,
                match path {
                    [] => None,
                    path => Some(decode(&path.join("/"))?),
                },
            ),
            _ => return None,
        }
    } else {
        let revision = strip_revision(segments[used - 1])
            .1
            .unwrap_or("main")
            .to_owned();
        (revision, (!tail.is_empty()).then(|| tail.join("/")))
    };
    if revision.is_empty() || revision.contains("..") {
        return None;
    }
    Some(RepoRef {
        kind,
        repo,
        revision,
        path,
    })
}

fn strip_revision(segment: &str) -> (&str, Option<&str>) {
    match segment.split_once('@') {
        Some((name, revision)) => (name, Some(revision)),
        None => (segment, None),
    }
}

fn valid_repo_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 96
        && part != "."
        && part != ".."
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// One file of a resolved repository.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoFile {
    /// Its path in the repository, `/`-separated, checked to stay inside.
    pub path: String,
    pub size: Option<u64>,
    /// Stated by the provider for large files; lowercase hex.
    pub sha256: Option<String>,
    /// Pinned to the commit.
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Listing {
    pub kind: RepoKind,
    pub repo: String,
    pub revision: String,
    /// The 40-hex commit every file is pinned to.
    pub commit: String,
    pub files: Vec<RepoFile>,
    /// Paths refused because they could not be saved safely.
    pub skipped: Vec<String>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ProviderError {
    /// The repository or revision does not exist.
    NotFound,
    /// Gated or private.
    SignInNeeded,
    /// The answer could not be read, or broke a limit.
    Malformed(String),
    Transport(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str(
                "Hugging Face has no such repository or revision. Check the name and the branch, tag or commit.",
            ),
            Self::SignInNeeded => f.write_str(
                "This repository is gated or private. It needs a Hugging Face sign-in, which Fetchpath does not support yet.",
            ),
            Self::Malformed(why) => write!(f, "Hugging Face answered with a listing that cannot be used: {why}."),
            Self::Transport(why) => write!(f, "Hugging Face could not be reached: {why}."),
        }
    }
}

impl std::error::Error for ProviderError {}

#[derive(Deserialize)]
struct Answer {
    sha: String,
    #[serde(default)]
    siblings: Vec<Sibling>,
}

#[derive(Deserialize)]
struct Sibling {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<Lfs>,
}

#[derive(Deserialize)]
struct Lfs {
    sha256: String,
    #[serde(default)]
    size: Option<u64>,
}

/// Resolves a repository over the network.
pub fn resolve(reference: &RepoRef) -> Result<Listing, ProviderError> {
    resolve_with(reference, |url| {
        fetchpath_http::fetch_small(url, MAX_LISTING_BYTES, Duration::from_secs(30))
            .map_err(|error| ProviderError::Transport(error.to_string()))
    })
}

/// As [`resolve`], with the listing fetched by `fetch`, which answers a
/// status and a body.
pub fn resolve_with(
    reference: &RepoRef,
    fetch: impl FnOnce(&str) -> Result<(u32, Vec<u8>), ProviderError>,
) -> Result<Listing, ProviderError> {
    let url = format!(
        "{HOST}/api/{}/{}/revision/{}?blobs=true",
        reference.kind.api(),
        reference.repo,
        encode(&reference.revision)
    );
    let (status, body) = fetch(&url)?;
    match status {
        200 => {}
        401 | 403 => return Err(ProviderError::SignInNeeded),
        404 => return Err(ProviderError::NotFound),
        other => return Err(ProviderError::Transport(format!("HTTP status {other}"))),
    }
    let answer: Answer = serde_json::from_slice(&body)
        .map_err(|error| ProviderError::Malformed(error.to_string()))?;
    if answer.sha.len() != 40 || !answer.sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ProviderError::Malformed(
            "the commit is not a 40-digit hash".into(),
        ));
    }
    if answer.siblings.len() > MAX_FILES {
        return Err(ProviderError::Malformed(format!(
            "it lists more than {MAX_FILES} files"
        )));
    }
    let commit = answer.sha.to_ascii_lowercase();
    let wanted = reference.path.as_deref().map(|path| path.trim_matches('/'));
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for sibling in answer.siblings {
        let path = sibling.rfilename;
        if let Some(wanted) = wanted
            && path != wanted
            && !path.starts_with(&format!("{wanted}/"))
        {
            continue;
        }
        if !safe_path(&path) {
            skipped.push(path);
            continue;
        }
        let sha256 = match sibling.lfs {
            Some(lfs) => {
                let hex = lfs.sha256.to_ascii_lowercase();
                if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                    skipped.push(path);
                    continue;
                }
                files.push(RepoFile {
                    url: file_url(reference, &commit, &path),
                    size: lfs.size.or(sibling.size),
                    sha256: Some(hex),
                    path,
                });
                continue;
            }
            None => None,
        };
        files.push(RepoFile {
            url: file_url(reference, &commit, &path),
            size: sibling.size,
            sha256,
            path,
        });
    }
    Ok(Listing {
        kind: reference.kind,
        repo: reference.repo.clone(),
        revision: reference.revision.clone(),
        commit,
        files,
        skipped,
    })
}

fn file_url(reference: &RepoRef, commit: &str, path: &str) -> String {
    let encoded: Vec<String> = path.split('/').map(encode).collect();
    format!(
        "{HOST}/{}{}/resolve/{commit}/{}",
        reference.kind.prefix(),
        reference.repo,
        encoded.join("/")
    )
}

/// A path that can be saved under a folder on Windows without leaving it.
fn safe_path(path: &str) -> bool {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    path.len() <= 1024
        && !path.starts_with('/')
        && path.split('/').all(|part| {
            let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !RESERVED.contains(&stem.as_str())
                && !part.chars().any(|c| {
                    c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                })
        })
}

/// Undoes percent-encoding from a link; `None` for a malformed escape.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-encodes one path segment or a revision.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(link: &str) -> RepoRef {
        parse(link).unwrap_or_else(|| panic!("{link} did not parse"))
    }

    #[test]
    fn repository_links_parse_in_both_forms() {
        let gpt2 = reference("hf://openai-community/gpt2");
        assert_eq!(
            (
                gpt2.kind,
                gpt2.repo.as_str(),
                gpt2.revision.as_str(),
                gpt2.path
            ),
            (RepoKind::Model, "openai-community/gpt2", "main", None)
        );
        let pinned = reference("hf://datasets/owner/data@v1.0/train");
        assert_eq!(pinned.kind, RepoKind::Dataset);
        assert_eq!(pinned.revision, "v1.0");
        assert_eq!(pinned.path.as_deref(), Some("train"));
        assert_eq!(reference("hf://gpt2").repo, "gpt2");

        let tree = reference("https://huggingface.co/owner/name/tree/refs%2Fpr%2F3/onnx");
        assert_eq!(tree.revision, "refs/pr/3");
        assert_eq!(tree.path.as_deref(), Some("onnx"));
        let file =
            reference("https://huggingface.co/owner/name/resolve/main/config.json?download=true");
        assert_eq!(file.path.as_deref(), Some("config.json"));
        assert_eq!(
            reference("https://huggingface.co/spaces/owner/app").kind,
            RepoKind::Space
        );

        for not_a_repo in [
            "https://example.com/owner/name",
            "https://huggingface.co/docs/hub",
            "https://huggingface.co/owner/name/discussions",
            "hf://owner/../etc",
            "hf://",
            "hf://owner/name@",
        ] {
            assert_eq!(parse(not_a_repo), None, "{not_a_repo}");
        }
    }

    const LISTING: &str = r#"{
        "sha": "607A30D783DFA663CAF39E06633721C8D4CFCD7E",
        "siblings": [
            {"rfilename": "config.json", "size": 665},
            {"rfilename": "model.safetensors", "size": 548105171,
             "lfs": {"sha256": "248DFC3911869EC493C76E65BF2FCF7F615828B0254C12B473182F0F81D3A707", "size": 548105171}},
            {"rfilename": "onnx/decoder model.onnx", "size": 10,
             "lfs": {"sha256": "e3fc9615868ff8f5e0429b892a0f6ca692784ba6c4ca31c4e9ee8218e7cce34f"}},
            {"rfilename": "../escape.bin", "size": 1},
            {"rfilename": "nested/con.txt", "size": 1},
            {"rfilename": "bad.bin", "lfs": {"sha256": "not-a-hash"}}
        ]
    }"#;

    fn resolved(link: &str, status: u32, body: &str) -> Result<Listing, ProviderError> {
        let body = body.as_bytes().to_vec();
        resolve_with(&reference(link), move |url| {
            assert!(url.starts_with("https://huggingface.co/api/"), "{url}");
            Ok((status, body))
        })
    }

    #[test]
    fn a_listing_pins_every_file_to_the_commit_with_its_stated_identity() {
        let listing = resolved("hf://openai-community/gpt2", 200, LISTING).unwrap();
        assert_eq!(listing.commit, "607a30d783dfa663caf39e06633721c8d4cfcd7e");
        let paths: Vec<&str> = listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "config.json",
                "model.safetensors",
                "onnx/decoder model.onnx"
            ]
        );
        assert_eq!(
            listing.skipped,
            ["../escape.bin", "nested/con.txt", "bad.bin"]
        );
        let weights = &listing.files[1];
        assert_eq!(
            weights.url,
            "https://huggingface.co/openai-community/gpt2/resolve/607a30d783dfa663caf39e06633721c8d4cfcd7e/model.safetensors"
        );
        assert_eq!(
            weights.sha256.as_deref(),
            Some("248dfc3911869ec493c76e65bf2fcf7f615828b0254c12b473182f0f81d3a707")
        );
        assert_eq!(
            listing.files[0].sha256, None,
            "a small file states no SHA-256"
        );
        assert!(listing.files[2].url.ends_with("/onnx/decoder%20model.onnx"));

        let folder = resolved("hf://openai-community/gpt2/onnx", 200, LISTING).unwrap();
        assert_eq!(folder.files.len(), 1);
    }

    #[test]
    fn refusals_and_broken_answers_are_plain() {
        assert_eq!(
            resolved("hf://a/b", 401, "").unwrap_err(),
            ProviderError::SignInNeeded
        );
        assert_eq!(
            resolved("hf://a/b", 403, "").unwrap_err(),
            ProviderError::SignInNeeded
        );
        assert_eq!(
            resolved("hf://a/b", 404, "").unwrap_err(),
            ProviderError::NotFound
        );
        for body in ["", "[]", r#"{"sha": "main", "siblings": []}"#] {
            assert!(matches!(
                resolved("hf://a/b", 200, body).unwrap_err(),
                ProviderError::Malformed(_)
            ));
        }
        let many = format!(
            r#"{{"sha": "{}", "siblings": [{}]}}"#,
            "a".repeat(40),
            vec![r#"{"rfilename": "x"}"#; MAX_FILES + 1].join(",")
        );
        assert!(matches!(
            resolved("hf://a/b", 200, &many).unwrap_err(),
            ProviderError::Malformed(_)
        ));
    }
}
