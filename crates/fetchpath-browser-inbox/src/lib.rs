//! The browser capture inbox: captures the browser host accepted, their
//! DPAPI-protected request context, and the record of which job took each.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaptureRequest {
    pub schema_version: u32,
    pub capture_id: String,
    pub method: String,
    pub url: String,
    pub suggested_filename: String,
    pub referrer: Option<String>,
    #[serde(default)]
    pub cookies: Vec<BrowserCookie>,
    pub user_initiated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub host_only: bool,
    pub expiration_date: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxRecord {
    pub schema_version: u32,
    pub capture_id: String,
    pub request_fingerprint: String,
    pub redacted_url: String,
    pub suggested_filename: String,
    pub credential_ref: String,
    pub created_at_ms: u64,
    pub processed_job_id: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct SecretEnvelope {
    pub url: String,
    pub referer: Option<String>,
    pub cookie_lines: Vec<String>,
}

#[derive(Clone)]
pub struct BridgeStore {
    root: PathBuf,
}

impl BridgeStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn default_for_user() -> io::Result<Self> {
        if let Some(path) = std::env::var_os("FETCHPATH_APP_DATA_DIR") {
            return Ok(Self::new(PathBuf::from(path)));
        }
        let roaming = std::env::var_os("APPDATA")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is unavailable"))?;
        Ok(Self::new(
            PathBuf::from(roaming).join("app.fetchpath.desktop"),
        ))
    }

    pub fn accept(&self, request: &CaptureRequest) -> Result<bool, String> {
        let prepared = validate_capture(request)?;
        fs::create_dir_all(self.inbox_dir()).map_err(storage_reason)?;
        fs::create_dir_all(self.secrets_dir()).map_err(storage_reason)?;

        let inbox_path = self.inbox_path(&request.capture_id);
        if inbox_path.exists() {
            let existing: InboxRecord = read_json(&inbox_path).map_err(storage_reason)?;
            if existing.request_fingerprint == prepared.fingerprint {
                return Ok(true);
            }
            return Err("contract.idempotency_conflict".into());
        }

        let secret_json = serde_json::to_vec(&prepared.secret)
            .map_err(|_| "bridge.invalid_secret".to_string())?;
        let protected = protect_secret(&secret_json).map_err(storage_reason)?;
        match write_once_synced(&self.secret_path(&request.capture_id), &protected) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = self
                    .load_secret(&request.capture_id)
                    .map_err(storage_reason)?;
                let existing_json = serde_json::to_vec(&existing)
                    .map_err(|_| "bridge.invalid_secret".to_string())?;
                let existing_fingerprint = format!("{:x}", Sha256::digest(existing_json));
                if existing_fingerprint != prepared.fingerprint {
                    return Err("contract.idempotency_conflict".into());
                }
            }
            Err(error) => return Err(storage_reason(error)),
        }

        let record = InboxRecord {
            schema_version: SCHEMA_VERSION,
            capture_id: request.capture_id.clone(),
            request_fingerprint: prepared.fingerprint,
            redacted_url: redact_url(&request.url),
            suggested_filename: prepared.filename,
            credential_ref: request.capture_id.clone(),
            created_at_ms: now_ms(),
            processed_job_id: None,
        };
        match write_json_once(&inbox_path, &record) {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing: InboxRecord = read_json(&inbox_path).map_err(storage_reason)?;
                if existing.request_fingerprint == record.request_fingerprint {
                    Ok(true)
                } else {
                    Err("contract.idempotency_conflict".into())
                }
            }
            Err(error) => Err(storage_reason(error)),
        }
    }

    pub fn pending(&self) -> io::Result<Vec<InboxRecord>> {
        let mut records = Vec::new();
        let entries = match fs::read_dir(self.inbox_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let record: InboxRecord = match read_json(&entry.path()) {
                Ok(record) => record,
                Err(_) => continue,
            };
            if record.schema_version == SCHEMA_VERSION && record.processed_job_id.is_none() {
                records.push(record);
            }
        }
        records.sort_by_key(|record| record.created_at_ms);
        Ok(records)
    }

    pub fn load_secret(&self, credential_ref: &str) -> io::Result<SecretEnvelope> {
        validate_id(credential_ref)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidInput, reason))?;
        let protected = fs::read(self.secret_path(credential_ref))?;
        let plaintext = unprotect_secret(&protected)?;
        serde_json::from_slice(&plaintext)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn mark_processed(&self, capture_id: &str, job_id: &str) -> io::Result<()> {
        validate_id(capture_id)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidInput, reason))?;
        let path = self.inbox_path(capture_id);
        let mut record: InboxRecord = read_json(&path)?;
        record.processed_job_id = Some(job_id.to_owned());
        write_json_replace(&path, &record)
    }

    pub fn remove_secret(&self, credential_ref: &str) -> io::Result<()> {
        validate_id(credential_ref)
            .map_err(|reason| io::Error::new(io::ErrorKind::InvalidInput, reason))?;
        match fs::remove_file(self.secret_path(credential_ref)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn inbox_dir(&self) -> PathBuf {
        self.root.join("browser-inbox")
    }

    fn secrets_dir(&self) -> PathBuf {
        self.root.join("browser-secrets")
    }

    fn inbox_path(&self, capture_id: &str) -> PathBuf {
        self.inbox_dir().join(format!("{capture_id}.json"))
    }

    fn secret_path(&self, credential_ref: &str) -> PathBuf {
        self.secrets_dir().join(format!("{credential_ref}.bin"))
    }
}

struct PreparedCapture {
    filename: String,
    fingerprint: String,
    secret: SecretEnvelope,
}

fn validate_capture(request: &CaptureRequest) -> Result<PreparedCapture, String> {
    if request.schema_version != SCHEMA_VERSION {
        return Err("contract.unsupported_version".into());
    }
    validate_id(&request.capture_id)?;
    if !request.user_initiated {
        return Err("bridge.explicit_action_required".into());
    }
    if request.method != "GET" {
        return Err("bridge.unsupported_method".into());
    }
    let url = Url::parse(&request.url).map_err(|_| "bridge.invalid_url".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("bridge.unsupported_scheme".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("bridge.url_credentials_rejected".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "bridge.invalid_url".to_string())?;
    let filename = safe_filename(&request.suggested_filename)?;
    let mut cookie_lines = Vec::with_capacity(request.cookies.len());
    if request.cookies.len() > 256 {
        return Err("bridge.too_many_cookies".into());
    }
    for cookie in &request.cookies {
        cookie_lines.push(cookie_line(cookie, &url, host)?);
    }
    let referer = request
        .referrer
        .as_deref()
        .map(Url::parse)
        .transpose()
        .map_err(|_| "bridge.invalid_referrer".to_string())?
        .filter(|value| value.origin() == url.origin())
        .map(|value| format!("{}/", value.origin().ascii_serialization()));
    let secret = SecretEnvelope {
        url: request.url.clone(),
        referer,
        cookie_lines,
    };
    let secret_json = serde_json::to_vec(&secret).map_err(|_| "bridge.invalid_secret")?;
    let fingerprint = format!("{:x}", Sha256::digest(secret_json));
    Ok(PreparedCapture {
        filename,
        fingerprint,
        secret,
    })
}

fn validate_id(value: &str) -> Result<(), String> {
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| "bridge.invalid_capture_id".into())
}

fn safe_filename(value: &str) -> Result<String, String> {
    let trimmed = value.trim().trim_end_matches(['.', ' ']);
    // The engine's list (FP-067): CONIN$, CONOUT$ and the superscript
    // COM and LPT ports too.
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or(trimmed)
        .trim_end()
        .to_uppercase();
    let mut chars = stem.chars();
    let head: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ((head == "COM" || head == "LPT")
        && matches!(
            rest.as_slice(),
            ['1'..='9'] | ['\u{b9}' | '\u{b2}' | '\u{b3}']
        ));
    if trimmed.is_empty()
        || trimmed.len() > 240
        || trimmed == "."
        || trimmed == ".."
        || reserved
        || trimmed.chars().any(|character| {
            character.is_control()
                // A page cannot name a file so it reads as another on the
                // person's screen: no overrides, isolates or line breaks.
                || matches!(character, '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
        })
    {
        return Err("bridge.invalid_filename".into());
    }
    Ok(trimmed.to_owned())
}

fn cookie_line(cookie: &BrowserCookie, url: &Url, host: &str) -> Result<String, String> {
    for value in [&cookie.name, &cookie.value, &cookie.domain, &cookie.path] {
        if value.contains(['\t', '\r', '\n', '\0']) || value.len() > 4096 {
            return Err("bridge.invalid_cookie".into());
        }
    }
    let domain = cookie.domain.trim_start_matches('.').to_ascii_lowercase();
    if cookie.name.is_empty() || domain.is_empty() || !cookie.path.starts_with('/') {
        return Err("bridge.invalid_cookie".into());
    }
    let host = host.to_ascii_lowercase();
    let domain_matches =
        host == domain || (!cookie.host_only && host.ends_with(&format!(".{domain}")));
    let request_path = url.path();
    let path_matches = request_path == cookie.path
        || (request_path.starts_with(&cookie.path)
            && (cookie.path.ends_with('/')
                || request_path.as_bytes().get(cookie.path.len()) == Some(&b'/')));
    if !domain_matches || !path_matches || (cookie.secure && url.scheme() != "https") {
        return Err("bridge.cookie_scope_mismatch".into());
    }
    let cookie_domain = if cookie.host_only {
        domain
    } else {
        format!(".{domain}")
    };
    let include_subdomains = if cookie.host_only { "FALSE" } else { "TRUE" };
    let secure = if cookie.secure { "TRUE" } else { "FALSE" };
    let expires = cookie
        .expiration_date
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(0.0) as u64;
    Ok(format!(
        "{cookie_domain}\t{include_subdomains}\t{}\t{secure}\t{expires}\t{}\t{}",
        cookie.path, cookie.name, cookie.value
    ))
}

fn redact_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut url) => {
            url.set_query(None);
            url.set_fragment(None);
            url.to_string()
        }
        Err(_) => "<invalid-url>".into(),
    }
}

fn write_json_once(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(invalid_data)?;
    write_once_synced(path, &[bytes, b"\n".to_vec()].concat())
}

fn write_once_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

fn write_json_replace(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension("json.new");
    let bytes = serde_json::to_vec_pretty(value).map_err(invalid_data)?;
    let mut file = File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    serde_json::from_reader(File::open(path)?).map_err(invalid_data)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

pub fn storage_reason(error: io::Error) -> String {
    format!("bridge.storage_unavailable:{error}")
}

#[cfg(windows)]
fn protect_secret(plaintext: &[u8]) -> io::Result<Vec<u8>> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len().try_into().map_err(invalid_data)?,
        pbData: plaintext.as_ptr().cast_mut(),
    };
    let entropy_bytes = b"fetchpath-browser-v1";
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len() as u32,
        pbData: entropy_bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptProtectData(
            &input,
            ptr::null(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    let protected =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(protected)
}

#[cfg(windows)]
fn unprotect_secret(protected: &[u8]) -> io::Result<Vec<u8>> {
    use std::ptr;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: protected.len().try_into().map_err(invalid_data)?,
        pbData: protected.as_ptr().cast_mut(),
    };
    let entropy_bytes = b"fetchpath-browser-v1";
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len() as u32,
        pbData: entropy_bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    let success = unsafe {
        CryptUnprotectData(
            &input,
            ptr::null_mut(),
            &entropy,
            ptr::null(),
            ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    let plaintext =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(output.pbData.cast()) };
    Ok(plaintext)
}

#[cfg(not(windows))]
fn protect_secret(_: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "DPAPI requires Windows",
    ))
}

#[cfg(not(windows))]
fn unprotect_secret(_: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "DPAPI requires Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str) -> CaptureRequest {
        CaptureRequest {
            schema_version: SCHEMA_VERSION,
            capture_id: id.into(),
            method: "GET".into(),
            url: "https://files.example.test/archive.zip?token=secret".into(),
            suggested_filename: "archive.zip".into(),
            referrer: Some("https://files.example.test/downloads".into()),
            cookies: vec![BrowserCookie {
                name: "session".into(),
                value: "private-value".into(),
                domain: "files.example.test".into(),
                path: "/".into(),
                secure: true,
                host_only: true,
                expiration_date: None,
            }],
            user_initiated: true,
        }
    }

    #[test]
    fn rejects_unsupported_or_unsafe_capture_inputs() {
        let id = Uuid::new_v4().to_string();
        let mut capture = request(&id);
        capture.method = "POST".into();
        assert_eq!(
            validate_capture(&capture).err().unwrap(),
            "bridge.unsupported_method"
        );
        capture.method = "GET".into();
        capture.url = "blob:https://example.test/id".into();
        assert_eq!(
            validate_capture(&capture).err().unwrap(),
            "bridge.unsupported_scheme"
        );
        capture.url = "https://user:pass@example.test/file".into();
        assert_eq!(
            validate_capture(&capture).err().unwrap(),
            "bridge.url_credentials_rejected"
        );
        capture.url = "https://files.example.test/file".into();
        capture.suggested_filename = "../escape.bin".into();
        assert_eq!(
            validate_capture(&capture).err().unwrap(),
            "bridge.invalid_filename"
        );
        capture.suggested_filename = "NUL.txt".into();
        assert_eq!(
            validate_capture(&capture).err().unwrap(),
            "bridge.invalid_filename"
        );
        capture.suggested_filename = "safe.bin".into();
        capture.cookies[0].domain = "other.example.test".into();
        assert_eq!(
            validate_capture(&capture).err().unwrap(),
            "bridge.cookie_scope_mismatch"
        );
    }

    #[test]
    fn scoped_cookie_context_is_protected_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let store = BridgeStore::new(temp.path().to_path_buf());
        let id = Uuid::new_v4().to_string();
        let capture = request(&id);
        assert!(!store.accept(&capture).unwrap());
        assert!(store.accept(&capture).unwrap());

        let public = fs::read_to_string(store.inbox_path(&id)).unwrap();
        assert!(!public.contains("secret"));
        assert!(!public.contains("private-value"));
        let protected = fs::read(store.secret_path(&id)).unwrap();
        assert!(!String::from_utf8_lossy(&protected).contains("private-value"));

        let secret = store.load_secret(&id).unwrap();
        assert!(secret.url.contains("token=secret"));
        assert!(secret.cookie_lines[0].contains("private-value"));
        let pending = store.pending().unwrap();
        assert_eq!(pending.len(), 1);
        store.mark_processed(&id, "job-id").unwrap();
        assert!(store.pending().unwrap().is_empty());
    }

    #[test]
    fn duplicate_identity_with_different_request_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let store = BridgeStore::new(temp.path().to_path_buf());
        let id = Uuid::new_v4().to_string();
        let first = request(&id);
        store.accept(&first).unwrap();
        let mut second = request(&id);
        second.url = "https://files.example.test/other.zip".into();
        assert_eq!(
            store.accept(&second).unwrap_err(),
            "contract.idempotency_conflict"
        );
    }

    #[test]
    fn retry_recovers_an_envelope_synced_before_its_inbox_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = BridgeStore::new(temp.path().to_path_buf());
        let id = Uuid::new_v4().to_string();
        let capture = request(&id);
        store.accept(&capture).unwrap();
        fs::remove_file(store.inbox_path(&id)).unwrap();

        assert!(!store.accept(&capture).unwrap());
        assert_eq!(store.pending().unwrap().len(), 1);
    }

    /// A page-chosen name cannot be a device or disguise itself (FP-067).
    #[test]
    fn page_chosen_names_cannot_be_devices_or_disguised() {
        for name in [
            "CONIN$",
            "conout$.txt",
            "COM\u{b9}.bin",
            "invoice\u{202E}txt.exe",
            "a\u{2066}b\u{2069}.exe",
            "two\u{2028}lines.bin",
        ] {
            assert!(safe_filename(name).is_err(), "{name:?}");
        }
        assert_eq!(safe_filename("COM0.txt").as_deref(), Ok("COM0.txt"));
        let persian = "\u{06AF}\u{0632}\u{0627}\u{0631}\u{0634}\u{200C}\u{0647}\u{0627}.pdf";
        assert!(safe_filename(persian).is_ok());
    }
}
