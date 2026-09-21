use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use url::Url;

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const CHROME_EXTENSION_ID: &str = "lfikhkjdpjcjaboanknaabncpkbgoele";
const FIREFOX_EXTENSION_ID: &str = "fetchpath-browser-spike@example.invalid";

#[derive(Debug, Deserialize)]
struct Request {
    #[serde(default)]
    schema_version: u64,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    capture_id: String,
    #[serde(default)]
    method: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    has_auth_context: bool,
    #[serde(default)]
    observation_complete: bool,
    #[serde(default)]
    signed_hint: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct LedgerRecord {
    capture_id: String,
    method: String,
    redacted_url: String,
    signed_hint: bool,
}

fn main() {
    let response = match run() {
        Ok(response) => response,
        Err(error) => json!({
            "schema_version": 1,
            "type": "error_ack",
            "accepted": false,
            "reason": error,
        }),
    };

    if let Err(error) = write_message(&response) {
        let _ = writeln!(io::stderr(), "native messaging response failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<Value, String> {
    validate_caller()?;
    let request: Request = serde_json::from_slice(&read_message()?)
        .map_err(|error| format!("invalid_json:{error}"))?;

    if request.schema_version != 1 {
        return Ok(rejected(&request, "unsupported_schema"));
    }

    if request.kind == "probe" {
        return Ok(json!({
            "schema_version": 1,
            "type": "probe_ack",
            "accepted": true,
            "reason": "native_host_available",
        }));
    }

    if request.kind != "capture" || request.capture_id.is_empty() {
        return Ok(rejected(&request, "invalid_capture"));
    }

    if !request.method.eq_ignore_ascii_case("GET") {
        return Ok(rejected(&request, "unsupported_method"));
    }

    let parsed = Url::parse(&request.url).map_err(|_| "invalid_url".to_owned())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Ok(rejected(&request, "unsupported_scheme"));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Ok(rejected(&request, "browser_auth_context_required"));
    }

    if !request.observation_complete {
        return Ok(rejected(&request, "observation_incomplete"));
    }

    if request.has_auth_context {
        return Ok(rejected(&request, "browser_auth_context_required"));
    }

    let ledger_path = ledger_path()?;
    if ledger_contains(&ledger_path, &request.capture_id)? {
        return Ok(accepted(&request, true));
    }

    let record = LedgerRecord {
        capture_id: request.capture_id.clone(),
        method: "GET".to_owned(),
        redacted_url: redact_url(parsed),
        signed_hint: request.signed_hint,
    };
    append_durable(&ledger_path, &record)?;

    Ok(accepted(&request, false))
}

fn validate_caller() -> Result<(), String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let chromium_origin = format!("chrome-extension://{CHROME_EXTENSION_ID}/");
    if args.iter().any(|arg| arg == &chromium_origin)
        || args.iter().any(|arg| arg == FIREFOX_EXTENSION_ID)
    {
        return Ok(());
    }
    Err("unauthorized_extension".to_owned())
}

fn ledger_path() -> Result<PathBuf, String> {
    env::var_os("FETCHPATH_BROWSER_SPIKE_LEDGER")
        .map(PathBuf::from)
        .ok_or_else(|| "ledger_path_missing".to_owned())
}

fn ledger_contains(path: &Path, capture_id: &str) -> Result<bool, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("ledger_read_failed:{error}")),
    };
    Ok(contents
        .lines()
        .filter_map(|line| serde_json::from_str::<LedgerRecord>(line).ok())
        .any(|record| record.capture_id == capture_id))
}

fn append_durable(path: &Path, record: &LedgerRecord) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("ledger_dir_failed:{error}"))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("ledger_open_failed:{error}"))?;
    serde_json::to_writer(&mut file, record)
        .map_err(|error| format!("ledger_encode_failed:{error}"))?;
    file.write_all(b"\n")
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("ledger_sync_failed:{error}"))
}

fn redact_url(mut url: Url) -> String {
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

fn accepted(request: &Request, deduplicated: bool) -> Value {
    json!({
        "schema_version": 1,
        "type": "capture_ack",
        "capture_id": request.capture_id,
        "accepted": true,
        "deduplicated": deduplicated,
    })
}

fn rejected(request: &Request, reason: &str) -> Value {
    json!({
        "schema_version": 1,
        "type": "capture_ack",
        "capture_id": request.capture_id,
        "accepted": false,
        "reason": reason,
    })
}

fn read_message() -> Result<Vec<u8>, String> {
    let mut length = [0_u8; 4];
    io::stdin()
        .read_exact(&mut length)
        .map_err(|error| format!("message_length_failed:{error}"))?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err("message_size_rejected".to_owned());
    }
    let mut payload = vec![0_u8; length];
    io::stdin()
        .read_exact(&mut payload)
        .map_err(|error| format!("message_read_failed:{error}"))?;
    Ok(payload)
}

fn write_message(value: &Value) -> Result<(), String> {
    let payload = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let length = u32::try_from(payload.len()).map_err(|_| "response_too_large".to_owned())?;
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&length.to_le_bytes())
        .and_then(|_| stdout.write_all(&payload))
        .and_then(|_| stdout.flush())
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_removes_query_and_fragment() {
        let url = Url::parse("https://example.test/file?token=secret#fragment").unwrap();
        assert_eq!(redact_url(url), "https://example.test/file");
    }

    #[test]
    fn url_credentials_are_detectable_before_redaction() {
        let url = Url::parse("https://user:secret@example.test/file").unwrap();
        assert!(!url.username().is_empty());
        assert!(url.password().is_some());
    }
}
