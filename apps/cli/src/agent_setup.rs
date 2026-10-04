//! Explicit user-scope MCP registration. This never talks to the engine or grants access.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const NAME: &str = "fetchpath";
const USAGE: &str = "usage: fetchpath agent-setup status [--json] | add codex|claude-code [--adopt|--replace] | remove codex|claude-code [--keep-config] | check codex|claude-code | cleanup | reconcile\nGuided setup uses user scope only. Restart the host after changing registration. --keep-config explicitly relinquishes ownership without editing host settings. Registration grants no download access; use Fetchpath approvals or explicitly grant folders separately.";
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Host {
    Codex,
    ClaudeCode,
}
impl Host {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "codex" => Ok(Self::Codex),
            "claude-code" => Ok(Self::ClaudeCode),
            _ => Err(USAGE.into()),
        }
    }
    fn agent(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude-code",
        }
    }
    fn config(self) -> Result<PathBuf> {
        let home = || {
            env::var_os("USERPROFILE")
                .or_else(|| env::var_os("HOME"))
                .map(PathBuf::from)
                .ok_or("Cannot locate your home directory".to_string())
        };
        let path = match self {
            Self::Codex => env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or(home()?.join(".codex"))
                .join("config.toml"),
            Self::ClaudeCode => match env::var_os("CLAUDE_CONFIG_DIR") {
                Some(dir) => PathBuf::from(dir).join(".claude.json"),
                None => home()?.join(".claude.json"),
            },
        };
        std::path::absolute(path).map_err(|_| "Cannot resolve host configuration path".into())
    }
    fn available(self) -> bool {
        let name = match self {
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
        };
        env::var_os("PATH")
            .map(|path| {
                env::split_paths(&path).any(|dir| {
                    if dir.join(name).is_file() {
                        return true;
                    }
                    #[cfg(windows)]
                    {
                        for ext in ["exe", "cmd", "bat"] {
                            if dir.join(format!("{name}.{ext}")).is_file() {
                                return true;
                            }
                        }
                    }
                    false
                })
            })
            .unwrap_or(false)
    }
    fn desired(self) -> Result<Value> {
        let command =
            env::current_exe().map_err(|_| "Cannot locate the installed Fetchpath executable")?;
        let command = command
            .to_str()
            .ok_or("The executable path is not valid Unicode")?;
        let mut value = json!({"command": command, "args": ["mcp", "--agent", self.agent()]});
        if self == Self::ClaudeCode {
            value["type"] = "stdio".into();
        }
        Ok(value)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    host: Host,
    config: PathBuf,
    expected: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    previous: Option<Record>,
    next: Option<Record>,
    before_digest: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    version: u32,
    records: Vec<Record>,
    pending: Option<Pending>,
}

struct Store {
    dir: PathBuf,
    inventory: Inventory,
    _lock: File,
}
impl Store {
    fn open() -> Result<Self> {
        let dir = env::var_os("FETCHPATH_AGENT_SETUP_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("FetchpathAgentSetup"))
            })
            .ok_or("Cannot locate local agent-setup inventory")?;
        fs::create_dir_all(&dir).map_err(|_| "Cannot create agent-setup inventory directory")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))
            .map_err(|_| "Cannot open agent-setup lock")?;
        lock.try_lock()
            .map_err(|_| "Another agent-setup command is running; retry when it finishes")?;
        let inventory = match read(&dir.join("inventory.json"), 1024 * 1024)? {
            Some(bytes) => serde_json::from_slice::<Inventory>(&bytes)
                .map_err(|_| "Agent-setup inventory is malformed; preserve it for recovery")?,
            None => Inventory {
                version: 1,
                ..Default::default()
            },
        };
        if inventory.version != 1 {
            return Err(
                "Unsupported agent-setup inventory version; preserve it for recovery".into(),
            );
        }
        for record in inventory.records.iter().chain(
            inventory
                .pending
                .iter()
                .flat_map(|p| p.previous.iter().chain(p.next.iter())),
        ) {
            if !record.config.is_absolute() || !valid_expected(record) {
                return Err(
                    "Invalid agent-setup ownership record; preserve inventory for recovery".into(),
                );
            }
        }
        Ok(Self {
            dir,
            inventory,
            _lock: lock,
        })
    }
    fn save(&self) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&self.inventory)
            .map_err(|_| "Cannot encode ownership inventory")?;
        atomic_write(&self.dir.join("inventory.json"), &bytes)
    }
    fn finish(&mut self, pending: &Pending) -> Result<()> {
        let record = pending
            .next
            .as_ref()
            .or(pending.previous.as_ref())
            .ok_or("Invalid pending ownership operation")?;
        self.inventory
            .records
            .retain(|r| !(r.host == record.host && r.config == record.config));
        if let Some(next) = &pending.next {
            self.inventory.records.push(next.clone());
        }
        self.inventory.pending = None;
        self.save()
    }
    fn recover(&mut self) -> Result<()> {
        let Some(pending) = self.inventory.pending.clone() else {
            return Ok(());
        };
        let record = pending
            .next
            .as_ref()
            .or(pending.previous.as_ref())
            .ok_or("Invalid pending ownership operation")?;
        let config = Config::load(record.host, &record.config)?;
        let entry = config.entry()?;
        if entry == pending.next.as_ref().map(|r| r.expected.clone()) {
            self.finish(&pending)
        } else if digest(config.before.as_deref()) == pending.before_digest
            || pending
                .previous
                .as_ref()
                .is_some_and(|r| entry.as_ref() == Some(&r.expected))
        {
            self.inventory.pending = None;
            self.save()
        } else {
            Err(format!(
                "Interrupted {} registration conflicts with current configuration. Preserve inventory and resolve the host's fetchpath entry before retrying; no configuration was changed",
                record.host.agent()
            ))
        }
    }
    fn change(
        &mut self,
        mut config: Config,
        previous: Option<Record>,
        next: Option<Record>,
    ) -> Result<()> {
        let pending = Pending {
            previous,
            next,
            before_digest: digest(config.before.as_deref()),
        };
        config.set(pending.next.as_ref().map(|r| &r.expected))?;
        self.inventory.pending = Some(pending.clone());
        self.save()?;
        config.save()?;
        self.finish(&pending)
    }
}

// Inventory contains only the bounded stdio entry we create, never host secrets/settings.
fn valid_expected(record: &Record) -> bool {
    let Some(map) = record.expected.as_object() else {
        return false;
    };
    let expected_len = if record.host == Host::Codex { 2 } else { 3 };
    map.len() == expected_len
        && map
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|p| Path::new(p).is_absolute())
        && map.get("args") == Some(&json!(["mcp", "--agent", record.host.agent()]))
        && (record.host != Host::ClaudeCode || map.get("type") == Some(&json!("stdio")))
}

enum Document {
    Toml(toml_edit::DocumentMut),
    Json(Value),
}
struct Config {
    host: Host,
    path: PathBuf,
    before: Option<Vec<u8>>,
    doc: Document,
    _lock: File,
}
impl Config {
    fn load(host: Host, path: &Path) -> Result<Self> {
        let parent = path.parent().ok_or("Invalid configuration path")?;
        fs::create_dir_all(parent).map_err(|_| "Cannot access host configuration directory")?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join(match host {
                Host::Codex => ".fetchpath-codex.lock",
                Host::ClaudeCode => ".fetchpath-claude.lock",
            }))
            .map_err(|_| "Cannot open host configuration lock")?;
        lock.try_lock()
            .map_err(|_| "Another registration command is changing this host; retry")?;
        let before = read(path, 16 * 1024 * 1024)?;
        let bytes = before.as_deref().unwrap_or(match host {
            Host::Codex => b"",
            Host::ClaudeCode => b"{}",
        });
        let invalid = || {
            format!(
                "{} user configuration is malformed or unsupported; fix it in the host before retrying",
                host.agent()
            )
        };
        let doc = match host {
            Host::Codex => Document::Toml(
                std::str::from_utf8(bytes)
                    .map_err(|_| invalid())?
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|_| invalid())?,
            ),
            Host::ClaudeCode => Document::Json(parse_json(bytes).map_err(|_| invalid())?),
        };
        let config = Self {
            host,
            path: path.to_owned(),
            before,
            doc,
            _lock: lock,
        };
        config.entry()?;
        Ok(config)
    }
    fn entry(&self) -> Result<Option<Value>> {
        let value = match &self.doc {
            Document::Toml(doc) => toml_edit::de::from_str::<Value>(&doc.to_string())
                .map_err(|_| "Unsupported Codex configuration")?,
            Document::Json(value) => value.clone(),
        };
        let root = value
            .as_object()
            .ok_or("Host configuration must be an object/table")?;
        let key = match self.host {
            Host::Codex => "mcp_servers",
            Host::ClaudeCode => "mcpServers",
        };
        match root.get(key) {
            None => Ok(None),
            Some(servers) => Ok(servers
                .as_object()
                .ok_or("Host MCP server configuration must be an object/table")?
                .get(NAME)
                .cloned()),
        }
    }
    fn set(&mut self, entry: Option<&Value>) -> Result<()> {
        match &mut self.doc {
            Document::Toml(doc) => {
                if doc.get("mcp_servers").is_none() {
                    if entry.is_none() {
                        return Ok(());
                    }
                    doc["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let servers = doc["mcp_servers"]
                    .as_table_like_mut()
                    .ok_or("Codex MCP server configuration must be a table")?;
                if let Some(entry) = entry {
                    let mut table = toml_edit::Table::new();
                    table["command"] =
                        toml_edit::value(entry["command"].as_str().ok_or("Invalid command")?);
                    let mut args = toml_edit::Array::new();
                    for arg in entry["args"].as_array().ok_or("Invalid arguments")? {
                        args.push(arg.as_str().ok_or("Invalid argument")?);
                    }
                    table["args"] = toml_edit::value(args);
                    servers.insert(NAME, toml_edit::Item::Table(table));
                } else {
                    servers.remove(NAME);
                }
            }
            Document::Json(doc) => {
                let root = doc
                    .as_object_mut()
                    .ok_or("Claude user configuration must be an object")?;
                if !root.contains_key("mcpServers") {
                    if entry.is_none() {
                        return Ok(());
                    }
                    root.insert("mcpServers".into(), json!({}));
                }
                let servers = root
                    .get_mut("mcpServers")
                    .and_then(Value::as_object_mut)
                    .ok_or("Claude MCP server configuration must be an object")?;
                if let Some(entry) = entry {
                    servers.insert(NAME.into(), entry.clone());
                } else {
                    servers.remove(NAME);
                }
            }
        }
        Ok(())
    }
    fn save(&self) -> Result<()> {
        let bytes = match &self.doc {
            Document::Toml(doc) => doc.to_string().into_bytes(),
            Document::Json(doc) => {
                serde_json::to_vec_pretty(doc).map_err(|_| "Cannot encode Claude configuration")?
            }
        };
        if read(&self.path, 16 * 1024 * 1024)? != self.before {
            return Err("Host configuration changed during setup; inventory retained. Retry after closing the host".into());
        }
        atomic_write(&self.path, &bytes)
    }
}

fn read(path: &Path, limit: u64) -> Result<Option<Vec<u8>>> {
    let file =
        match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(
                "Cannot read configuration/inventory; close the host and check file permissions"
                    .into(),
            ),
        };
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read configuration/inventory")?;
    if bytes.len() as u64 > limit {
        return Err("Configuration/inventory exceeds the supported size; no changes made".into());
    }
    Ok(Some(bytes))
}

fn digest(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(|bytes| format!("{:x}", Sha256::digest(bytes)))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or("Invalid configuration path")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "Cannot stage configuration/inventory; check file permissions")?;
    temp.write_all(bytes)
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| "Cannot flush configuration/inventory; ownership retained for recovery")?;
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
        };
        let from: Vec<u16> = temp
            .path()
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let staged_path = temp.into_temp_path();
        // Preserve existing ACLs and metadata. A new file uses no-overwrite
        // rename so a concurrent creator wins safely.
        let published = if path.exists() {
            unsafe {
                ReplaceFileW(
                    to.as_ptr(),
                    from.as_ptr(),
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            }
        } else {
            unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) }
        };
        if published == 0 {
            return Err(format!(
                "Cannot publish configuration/inventory (OS code {}); close the host and check permissions. Ownership retained for recovery",
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
            ));
        }
        OpenOptions::new().write(true).open(path).and_then(|file| file.sync_all()).map_err(
            |_| "Cannot flush published configuration/inventory; ownership retained for recovery",
        )?;
        drop(staged_path);
    }
    #[cfg(not(windows))]
    {
        temp.persist(path).map_err(
            |_| "Cannot publish configuration/inventory; ownership retained for recovery",
        )?;
        File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|_| "Cannot flush configuration directory; ownership retained for recovery")?;
    }
    Ok(())
}

// Reject duplicate JSON keys: choosing one silently could discard host settings or
// turn an ambiguous registration into an apparently owned one.
fn parse_json(bytes: &[u8]) -> std::result::Result<Value, serde_json::Error> {
    struct Unique(Value);
    impl<'de> Deserialize<'de> for Unique {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Unique;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("unambiguous JSON")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Unique, A::Error> {
                    let mut map = serde_json::Map::new();
                    while let Some((key, value)) = a.next_entry::<String, Unique>()? {
                        if map.insert(key, value.0).is_some() {
                            return Err(serde::de::Error::custom("duplicate key"));
                        }
                    }
                    Ok(Unique(Value::Object(map)))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Unique, A::Error> {
                    let mut values = Vec::new();
                    while let Some(value) = a.next_element::<Unique>()? {
                        values.push(value.0);
                    }
                    Ok(Unique(Value::Array(values)))
                }
                fn visit_bool<E: serde::de::Error>(
                    self,
                    v: bool,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                    Ok(Unique(json!(v)))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_string<E: serde::de::Error>(
                    self,
                    v: String,
                ) -> std::result::Result<Unique, E> {
                    Ok(Unique(v.into()))
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                    Ok(Unique(Value::Null))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    serde_json::from_slice::<Unique>(bytes).map(|v| v.0)
}

fn add(store: &mut Store, host: Host, mode: Option<&str>) -> Result<()> {
    if !host.available() {
        return Err(format!(
            "{} is not available on PATH. Install the host or use manual setup",
            host.agent()
        ));
    }
    let path = host.config()?;
    let config = Config::load(host, &path)?;
    let entry = config.entry()?;
    let previous = store
        .inventory
        .records
        .iter()
        .find(|r| r.host == host && r.config == path)
        .cloned();
    let desired = host.desired()?;
    if let Some(current) = &entry {
        let owned = previous.as_ref().is_some_and(|r| r.expected == *current);
        if !owned && mode != Some("--replace") && !(mode == Some("--adopt") && *current == desired)
        {
            return Err(format!(
                "{} already has an unowned or edited fetchpath entry. Use --adopt only for the exact installed command, or explicitly --replace to discard that entry",
                host.agent()
            ));
        }
    } else if mode == Some("--adopt") {
        return Err("No matching registration to adopt; run add without --adopt".into());
    }
    let next = Record {
        host,
        config: path,
        expected: desired,
    };
    if entry == Some(next.expected.clone()) {
        // Ownership-only adoption: no host configuration write is needed.
        store
            .inventory
            .records
            .retain(|r| !(r.host == host && r.config == next.config));
        store.inventory.records.push(next);
        store.save()
    } else {
        store.change(config, previous, Some(next))
    }
}

fn process_owned(store: &mut Store, host: Option<Host>, reconcile: bool) -> Result<()> {
    let records: Vec<_> = store
        .inventory
        .records
        .iter()
        .filter(|r| host.is_none_or(|h| r.host == h))
        .cloned()
        .collect();
    if records.is_empty() {
        eprintln!(
            "No owned registrations found. Any manually configured fetchpath entries are preserved; remove those through the host's MCP settings."
        );
    }
    let mut failures = Vec::new();
    for record in records {
        let result = (|| {
            let config = Config::load(record.host, &record.config)?;
            let entry = config.entry()?;
            if entry.is_none() {
                store
                    .inventory
                    .records
                    .retain(|r| !(r.host == record.host && r.config == record.config));
                return store.save();
            }
            if entry != Some(record.expected.clone()) {
                return Err(format!(
                    "{} fetchpath entry was edited; preserve it and the inventory. Restore the owned entry, explicitly add --replace, or run remove {} --keep-config to relinquish ownership and keep your settings",
                    record.host.agent(),
                    record.host.agent()
                ));
            }
            let next = if reconcile {
                Some(Record {
                    expected: record.host.desired()?,
                    ..record.clone()
                })
            } else {
                None
            };
            if next
                .as_ref()
                .is_some_and(|next| next.expected == record.expected)
            {
                return Ok(());
            }
            store.change(config, Some(record), next)
        })();
        if let Err(error) = result {
            failures.push(error);
            if store.inventory.pending.is_some() {
                break;
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn status(store: &Store, host: Host) -> Result<Value> {
    let path = host.config()?;
    let (registered, owned, state) = match read(&path, 16 * 1024 * 1024)? {
        None => (false, false, "not-registered"),
        Some(_) => {
            let config = Config::load(host, &path)?;
            let entry = config.entry()?;
            let owned =
                store.inventory.records.iter().any(|r| {
                    r.host == host && r.config == path && Some(&r.expected) == entry.as_ref()
                });
            (
                entry.is_some(),
                owned,
                if owned {
                    "registered-restart-host-to-connect"
                } else if entry.is_some() {
                    "unowned-or-edited"
                } else {
                    "not-registered"
                },
            )
        }
    };
    Ok(
        json!({"available": host.available(), "scope": "user", "config": path, "registered": registered, "owned": owned, "connection": state, "restart_required": registered}),
    )
}

fn check(store: &Store, host: Host) -> Result<()> {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    let path = host.config()?;
    let config = Config::load(host, &path)?;
    let record = store
        .inventory
        .records
        .iter()
        .find(|r| {
            r.host == host
                && r.config == path
                && config.entry().ok().flatten().as_ref() == Some(&r.expected)
        })
        .ok_or(
            "Connection check requires an unchanged owned registration; run explicit setup first",
        )?;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(Command::new(record.expected["command"].as_str().ok_or("Invalid owned command")?).args(["mcp", "--agent", host.agent()]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|_| "Cannot start owned Fetchpath command; run agent-setup reconcile from the installed executable")?);
    let stdout = child.0.stdout.take().ok_or("Cannot read MCP output")?;
    let mut stdin = child.0.stdin.take().ok_or("Cannot write MCP input")?;
    let (sender, receiver) = mpsc::sync_channel(4);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        for _ in 0..128 {
            let mut line = Vec::new();
            if (&mut reader)
                .take(1024 * 1024 + 1)
                .read_until(b'\n', &mut line)
                .is_err()
                || line.is_empty()
                || line.len() > 1024 * 1024
            {
                break;
            }
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut write = |message: Value| -> Result<()> {
        writeln!(stdin, "{message}")
            .and_then(|_| stdin.flush())
            .map_err(|_| "MCP connection closed during handshake".into())
    };
    let response = |id: u64| -> Result<Value> {
        loop {
            let line = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(
                    |_| "MCP connection check timed out or closed; restart the host and retry",
                )?;
            let value: Value = serde_json::from_slice(&line)
                .map_err(|_| "MCP connection returned malformed output")?;
            if value["id"] == json!(id) {
                if value.get("error").is_some() || value.get("result").is_none() {
                    return Err("MCP connection rejected the handshake/tool discovery".into());
                }
                return Ok(value["result"].clone());
            }
        }
    };
    let result = (|| {
        write(
            json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{"protocolVersion":"2024-11-05", "capabilities":{}, "clientInfo":{"name":"fetchpath-agent-setup", "version":crate::VERSION}}}),
        )?;
        let initialized = response(1)?;
        if initialized
            .get("protocolVersion")
            .and_then(Value::as_str)
            .is_none()
            || initialized
                .get("capabilities")
                .and_then(|v| v.get("tools"))
                .is_none()
        {
            return Err("MCP server did not advertise a tools connection".into());
        }
        write(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))?;
        write(json!({"jsonrpc":"2.0", "id":2, "method":"tools/list", "params":{}}))?;
        let tools = response(2)?;
        let count = tools["tools"]
            .as_array()
            .ok_or("MCP server returned an invalid tool list")?
            .len();
        if count == 0 {
            return Err("MCP server advertised no tools".into());
        }
        println!(
            "{}: MCP handshake and tool discovery passed ({count} tools). Restart the host and check /mcp; host policy may restrict loading. No download access was granted.",
            host.agent()
        );
        Ok(())
    })();
    drop(stdin);
    drop(receiver);
    drop(child);
    let _ = reader.join();
    result
}

pub(crate) fn run(args: &[String]) -> i32 {
    let result: Result<()> = (|| {
        // Parse the entire request before opening or recovering any inventory.
        let command = args.first().map(String::as_str).ok_or(USAGE)?;
        let host = match command {
            "add"
                if args.len() == 2
                    || (args.len() == 3 && matches!(args[2].as_str(), "--adopt" | "--replace")) =>
            {
                Some(Host::parse(&args[1])?)
            }
            "remove" if args.len() == 2 || (args.len() == 3 && args[2] == "--keep-config") => {
                Some(Host::parse(&args[1])?)
            }
            "check" if args.len() == 2 => Some(Host::parse(&args[1])?),
            "status" if args.len() == 1 || (args.len() == 2 && args[1] == "--json") => None,
            "cleanup" | "reconcile" if args.len() == 1 => None,
            _ => return Err(USAGE.into()),
        };
        let mut store = Store::open()?;
        if command == "remove" && args.get(2).is_some_and(|arg| arg == "--keep-config") {
            let host = host.unwrap();
            store.inventory.records.retain(|r| r.host != host);
            if store.inventory.pending.as_ref().is_some_and(|p| {
                p.previous
                    .as_ref()
                    .or(p.next.as_ref())
                    .is_some_and(|r| r.host == host)
            }) {
                store.inventory.pending = None;
            }
            store.save()?;
            println!(
                "Relinquished {} registration ownership. Host configuration was preserved; remove its fetchpath entry manually if desired.",
                host.agent()
            );
            return Ok(());
        }
        store.recover()?;
        match command {
            "status" => {
                let report = |host| {
                    status(&store, host).unwrap_or_else(|error| json!({"available": host.available(), "scope":"user", "connection":"configuration-error", "error":error}))
                };
                let value = json!({"codex": report(Host::Codex), "claude_code": report(Host::ClaudeCode), "owned_registrations": store.inventory.records.len(), "access_granted_by_setup": false});
                if args.len() == 2 {
                    println!("{value}");
                } else {
                    for key in ["codex", "claude_code"] {
                        println!(
                            "{key}: available={}, registration={}, scope=user",
                            value[key]["available"], value[key]["connection"]
                        );
                    }
                    println!(
                        "Restart registered hosts. Connection is checked in Codex /mcp or Claude Code /mcp, not by this command. Setup grants no access."
                    );
                }
            }
            "add" => {
                add(&mut store, host.unwrap(), args.get(2).map(String::as_str))?;
                println!(
                    "Registered Fetchpath in user scope. Restart the selected host and check /mcp. No folder grants or automatic approvals were added."
                );
            }
            "check" => check(&store, host.unwrap())?,
            "remove" | "cleanup" => {
                process_owned(&mut store, host, false)?;
                println!("Removed matching owned registrations. Restart affected hosts.");
            }
            "reconcile" => {
                process_owned(&mut store, None, true)?;
                println!(
                    "Updated matching owned registrations to this executable. Restart affected hosts."
                );
            }
            _ => unreachable!(),
        }
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("fetchpath agent-setup: {error}");
            if error.starts_with("usage:") { 2 } else { 6 }
        }
    }
}
