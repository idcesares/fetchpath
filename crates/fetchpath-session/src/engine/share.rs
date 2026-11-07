//! Public share links (FP-117): the person's setup, the test link and the
//! links themselves. Tickets, keys and frozen files live in memory in the
//! [`ShareStore`]; only the stored address and port reach the disk.

use super::inbox::{
    Boundary, Unreadable, canonical_name, hash_file, inside_staging, is_share, open_source,
    still_at,
};
use super::{Engine, error, input, persistence};
use crate::provenance::Marker;
use crate::share::{
    DEFAULT_EXPIRY, NewShare, Phase, RelayKeyStore, ShareError, ShareInfo, ShareKind, ShareStore,
    Source,
};
use fetchpath_protocol::command::Command;
use fetchpath_protocol::error::{Action, ErrorScope, ProtocolError};
use fetchpath_protocol::message::CommandResult;
use fetchpath_protocol::model::{RelayKey, SensitiveUrl, ShareItem, ShareState, ShareView};
use fetchpath_protocol::principal::Principal;
use fetchpath_protocol::send::OfferSource;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FILE: &str = "share-v1.json";
/// A port of its own, away from the web UI's and the LAN's.
pub const DEFAULT_PORT: u16 = 47650;
const TEST_NAME: &str = "fetchpath-test.txt";
const TEST_SIZE: usize = 100_000;
const TEST_EXPIRY: Duration = Duration::from_secs(15 * 60);

/// Where to dial in relay mode, and which setup the dial is for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelayPlan {
    pub host: String,
    pub port: u16,
    pub generation: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Persisted {
    version: u32,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default = "default_port")]
    port: u16,
    /// Relay mode (FP-118). Old files have none and mean a tunnel.
    #[serde(default)]
    relay: bool,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

struct State {
    /// `https://host[:port]`, as validated.
    base_url: Option<String>,
    /// The Host a request through the tunnel carries: `host[:port]`.
    authority: Option<String>,
    port: u16,
    /// Links go through the relay at `base_url` instead of a local listener.
    relay: bool,
    /// Counts every change of address, port, mode or key, so that a report
    /// from a connection made for an earlier setup is recognised as stale.
    generation: u64,
    /// Memory only: sharing is off after every engine start.
    enabled: bool,
    /// What the listener reported for `port`: where it is bound, or why not.
    listener: Option<Result<String, String>>,
}

pub(super) struct Sharing {
    store: Arc<ShareStore>,
    state: Mutex<State>,
    path: Option<PathBuf>,
    keys: Mutex<Option<Arc<dyn RelayKeyStore>>>,
}

impl Sharing {
    pub(super) fn load(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|dir| dir.join(FILE));
        let saved = path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .filter(|bytes| bytes.len() <= 4096)
            .and_then(|bytes| serde_json::from_slice::<Persisted>(&bytes).ok());
        let (base_url, port, relay) = saved
            .map(|saved| (saved.base_url, saved.port, saved.relay))
            .unwrap_or((None, DEFAULT_PORT, false));
        // A stored address that no longer validates is dropped, not served.
        let parsed = base_url.as_deref().and_then(|url| parse_base(url).ok());
        let has_address = parsed.is_some();
        let store = ShareStore::new();
        store.configure(
            false,
            parsed.as_ref().map(|(_, authority)| authority.clone()),
        );
        Self {
            store,
            state: Mutex::new(State {
                base_url: parsed.as_ref().map(|(url, _)| url.clone()),
                authority: parsed.map(|(_, authority)| authority),
                port: if valid_port(port) { port } else { DEFAULT_PORT },
                relay: relay && has_address,
                generation: 0,
                enabled: false,
                listener: None,
            }),
            path,
            keys: Mutex::new(None),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn save(&self, base_url: Option<&str>, port: u16, relay: bool) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&Persisted {
            version: 1,
            base_url: base_url.map(str::to_owned),
            port,
            relay,
        })
        .map_err(|failure| failure.to_string())?;
        let temporary = path.with_extension("tmp");
        let write = std::fs::File::create(&temporary).and_then(|mut file| {
            file.write_all(&bytes)?;
            file.sync_all()
        });
        write
            .and_then(|()| std::fs::rename(&temporary, path))
            .map_err(|failure| format!("The sharing setup could not be saved: {failure}"))
    }
}

fn valid_port(port: u16) -> bool {
    port >= 1024
}

/// A tunnel's public address: `https://` and a name, nothing else. A path,
/// query, user info, an IP address or a local name is refused, because a
/// base URL that points back at this machine or network would let the setup
/// test pass without the internet ever being involved.
/// Returns the stored URL and the Host a request through it carries.
fn parse_base(text: &str) -> Result<(String, String), String> {
    let text = text.trim();
    let rest = text
        .get(..8)
        .filter(|scheme| scheme.eq_ignore_ascii_case("https://"))
        .map(|_| &text[8..])
        .ok_or("The address must start with https://")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty()
        || rest
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')))
    {
        return Err("Use only the address, such as https://share.example.com".into());
    }
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (
            host,
            Some(
                port.parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0 && !port.to_string().starts_with('0'))
                    .ok_or("The port in the address is not valid")?,
            ),
        ),
        None => (rest, None),
    };
    let host = host.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let local = host == "localhost"
        || [
            "localhost",
            "local",
            "internal",
            "lan",
            "arpa",
            "home",
            "corp",
            "intranet",
        ]
        .iter()
        .any(|suffix| labels.last() == Some(suffix));
    let numeric_tail = labels
        .last()
        .is_some_and(|label| label.chars().all(|c| c.is_ascii_digit()));
    if host.len() > 253
        || labels.len() < 2
        || local
        || numeric_tail
        || labels.iter().any(|label| {
            label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-')
        })
    {
        return Err(
            "The address must be the public name of your tunnel, not an IP address \
                    or a name on your own network"
                .into(),
        );
    }
    let authority = match port {
        Some(port) if port != 443 => format!("{host}:{port}"),
        _ => host.clone(),
    };
    Ok((format!("https://{authority}"), authority))
}

fn refuse(code: &'static str, message: &str, action: Action) -> ProtocolError {
    error(code, ErrorScope::Command, message).with_action(action)
}

/// A canonical path on another computer (`\\?\UNC\host\share\...`).
fn is_remote(canonical: &std::path::Path) -> bool {
    canonical
        .to_string_lossy()
        .to_ascii_uppercase()
        .starts_with(r"\\?\UNC\")
}

fn not_shareable() -> ProtocolError {
    refuse(
        "share.not_shareable",
        "That file cannot be shared by link.",
        Action::CorrectInput,
    )
}

fn credentialed() -> ProtocolError {
    refuse(
        "policy.credentialed_not_allowed",
        "This file came from a download that used a login, cookies or a referer, so it is not \
         shared by link.",
        Action::ChangePolicy,
    )
}

fn not_ready(message: &str) -> ProtocolError {
    refuse("share.not_ready", message, Action::CorrectInput)
}

fn item(info: &ShareInfo) -> ShareItem {
    ShareItem {
        handle: info.handle.clone(),
        test: info.kind == ShareKind::Test,
        name: info.name.clone(),
        size: info.size,
        state: match info.phase {
            Phase::Live => ShareState::Live,
            Phase::Completed => ShareState::Completed,
            Phase::Revoked => ShareState::Revoked,
            Phase::Expired => ShareState::Expired,
            Phase::Changed => ShareState::Changed,
            Phase::OverBudget => ShareState::OverBudget,
        },
        expires_in_seconds: info.expires_in.as_secs(),
        opened: info.opened,
        delivered: info.delivered,
        total: info.cipher_len,
    }
}

impl Engine {
    /// The tickets the listener serves from.
    pub fn share_store(&self) -> Arc<ShareStore> {
        self.sharing.store.clone()
    }

    /// The loopback port to listen on, once an address is stored. Nothing
    /// listens before that, so a fresh install opens no port. In relay mode
    /// nothing listens either: the engine dials the relay instead.
    pub fn share_port(&self) -> Option<u16> {
        let state = self.sharing.state();
        state
            .base_url
            .as_ref()
            .filter(|_| !state.relay)
            .map(|_| state.port)
    }

    /// Where to enroll, in relay mode with an address stored.
    pub fn share_relay(&self) -> Option<RelayPlan> {
        let state = self.sharing.state();
        let url = state.base_url.as_deref().filter(|_| state.relay)?;
        let authority = url.strip_prefix("https://")?;
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (authority, 443),
        };
        Some(RelayPlan {
            host: host.to_owned(),
            port,
            generation: state.generation,
        })
    }

    /// Gives the engine the host's sealed key file. Without one, relay keys
    /// are refused.
    pub fn set_relay_keys(&self, keys: Arc<dyn RelayKeyStore>) {
        *self.sharing.keys.lock().unwrap_or_else(|p| p.into_inner()) = Some(keys);
    }

    /// The relay connection's report for the setup it was made for. A report
    /// for an earlier setup (`generation` no longer current) is ignored.
    pub fn set_share_relay(&self, generation: u64, result: Result<String, String>) {
        let mut state = self.sharing.state();
        if state.relay && state.base_url.is_some() && state.generation == generation {
            state.listener = Some(result);
        }
    }

    /// The listener's report for `port`: where it is bound, or why it is not.
    /// A report for a port that is no longer the stored one is ignored.
    pub fn set_share_listener(&self, port: u16, result: Result<String, String>) {
        let mut state = self.sharing.state();
        if state.base_url.is_some() && !state.relay && state.port == port {
            state.listener = Some(result);
        }
    }

    /// True while a link can still be used, so the engine stays up for it.
    pub fn has_live_shares(&self) -> bool {
        self.sharing.store.has_live()
    }

    pub(super) fn sweep_shares(&self) {
        self.sharing.store.sweep();
        // The ticket events carry handles only. There is no audit sink to
        // write them to yet; the store caps what it keeps.
        let _ = self.sharing.store.drain_events();
    }

    fn share_view(&self) -> ShareView {
        let (enabled, base_url, port, authority, listener, relay) = {
            let state = self.sharing.state();
            (
                state.enabled,
                state.base_url.clone(),
                state.port,
                state.authority.clone(),
                state.listener.clone(),
                state.relay,
            )
        };
        let relay_key_stored = self.relay_keys().is_some_and(|keys| keys.present());
        let tested = authority.is_some() && self.sharing.store.test_passed() == authority;
        let (serving, problem) = match (&base_url, listener) {
            (Some(_), Some(Ok(address))) => (Some(address), None),
            (Some(_), Some(Err(why))) => (None, Some(why)),
            _ => (None, None),
        };
        ShareView {
            enabled,
            base_url,
            port,
            tested,
            serving,
            problem,
            relay,
            relay_key_stored,
            shares: self.sharing.store.list().iter().map(item).collect(),
        }
    }

    fn configure_sharing(
        &self,
        base_url: Option<&str>,
        port: Option<u16>,
    ) -> Result<(), ProtocolError> {
        let (new_url, new_authority) = match base_url.map(str::trim) {
            None => {
                let state = self.sharing.state();
                (state.base_url.clone(), state.authority.clone())
            }
            Some("") => (None, None),
            Some(text) => {
                let (url, authority) = parse_base(text).map_err(input)?;
                (Some(url), Some(authority))
            }
        };
        let new_port = match port {
            None => self.sharing.state().port,
            Some(port) if valid_port(port) => port,
            Some(_) => return Err(input("Use a port from 1024 to 65535.".into())),
        };
        let mut state = self.sharing.state();
        if state.base_url == new_url && state.port == new_port {
            return Ok(());
        }
        // Without an address there is nothing to relay through.
        let relay = state.relay && new_url.is_some();
        // On disk first: a failed save leaves the old setup running.
        self.sharing
            .save(new_url.as_deref(), new_port, relay)
            .map_err(persistence)?;
        // A different address or port is a different tunnel: whatever was
        // tested, enabled or issued for the old one no longer holds.
        state.base_url = new_url;
        state.authority = new_authority.clone();
        state.port = new_port;
        state.relay = relay;
        self.reset_setup(&mut state);
        Ok(())
    }

    /// Whatever was tested, enabled or issued for the old setup no longer
    /// holds. One path for every change of address, port, mode or key.
    fn reset_setup(&self, state: &mut State) {
        state.generation += 1;
        state.enabled = false;
        state.listener = None;
        self.sharing.store.configure(false, state.authority.clone());
        self.sharing.store.clear_test();
        self.sharing.store.revoke_all();
    }

    fn relay_keys(&self) -> Option<Arc<dyn RelayKeyStore>> {
        self.sharing
            .keys
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn configure_relay(&self, relay: bool, key: Option<&RelayKey>) -> Result<(), ProtocolError> {
        let mut state = self.sharing.state();
        if relay && state.base_url.is_none() {
            return Err(not_ready("Set the relay's address first."));
        }
        let keys = self.relay_keys();
        if key.is_some() && keys.is_none() {
            return Err(refuse(
                "share.unsupported",
                "This engine cannot keep a relay key.",
                Action::CorrectInput,
            ));
        }
        if relay && key.is_none() && !keys.as_ref().is_some_and(|keys| keys.present()) {
            return Err(not_ready("Enroll with the relay's key first."));
        }
        if key.is_none() && state.relay == relay {
            return Ok(());
        }
        // The key on disk first, the setup file after it. A key that was
        // replaced ends the old setup even if the file then fails to save.
        if let (Some(key), Some(keys)) = (key, &keys) {
            keys.store(key.expose())
                .map_err(|why| persistence(format!("The relay key could not be saved: {why}")))?;
        }
        if state.relay != relay {
            if let Err(why) = self
                .sharing
                .save(state.base_url.as_deref(), state.port, relay)
            {
                if key.is_some() {
                    self.reset_setup(&mut state);
                }
                return Err(persistence(why));
            }
            state.relay = relay;
        }
        self.reset_setup(&mut state);
        Ok(())
    }

    fn set_sharing_enabled(&self, enabled: bool) -> Result<(), ProtocolError> {
        let mut state = self.sharing.state();
        if !enabled {
            state.enabled = false;
            self.sharing.store.configure(false, state.authority.clone());
            // Stopping ends every link and releases the files they hold.
            self.sharing.store.revoke_all();
            return Ok(());
        }
        let Some(authority) = state.authority.clone() else {
            return Err(not_ready("Set the tunnel address first."));
        };
        if !matches!(state.listener, Some(Ok(_))) {
            return Err(not_ready(
                "The share listener is not running. Check the port, then try again.",
            ));
        }
        if self.sharing.store.test_passed().as_deref() != Some(authority.as_str()) {
            return Err(not_ready(
                "Open the test link from a phone on mobile data first. Sharing turns on only \
                 after the whole test file was delivered through your tunnel.",
            ));
        }
        state.enabled = true;
        self.sharing.store.configure(true, Some(authority));
        Ok(())
    }

    fn start_share_test(&self) -> Result<CommandResult, ProtocolError> {
        let (base_url, listener) = {
            let state = self.sharing.state();
            (state.base_url.clone(), state.listener.clone())
        };
        let Some(base_url) = base_url else {
            return Err(not_ready("Set the tunnel address first."));
        };
        match listener {
            Some(Ok(_)) => {}
            Some(Err(why)) => return Err(not_ready(&why)),
            None => return Err(not_ready("The share listener is not running yet.")),
        }
        self.sharing.store.clear_test();
        let line = b"Fetchpath share test. If you can read this, your tunnel works.\n";
        let body: Vec<u8> = line.iter().copied().cycle().take(TEST_SIZE).collect();
        let created = self
            .sharing
            .store
            .create(NewShare {
                kind: ShareKind::Test,
                name: TEST_NAME.into(),
                source: Source::Memory(body),
                size: TEST_SIZE as u64,
                expiry: TEST_EXPIRY,
            })
            .map_err(share_error)?;
        self.link_result(&base_url, &created.handle, &created.link_path)
    }

    fn link_result(
        &self,
        base_url: &str,
        handle: &str,
        link_path: &str,
    ) -> Result<CommandResult, ProtocolError> {
        let share = self
            .sharing
            .store
            .list()
            .iter()
            .find(|info| info.handle == handle)
            .map(item)
            .ok_or_else(not_shareable)?;
        let link = SensitiveUrl::try_from(format!("{base_url}{link_path}")).map_err(input)?;
        Ok(CommandResult::ShareLink { link, share })
    }

    fn create_share(
        &self,
        principal: &Principal,
        source: &OfferSource,
        expires_in_seconds: Option<u64>,
    ) -> Result<CommandResult, ProtocolError> {
        // Agents and signed-in devices do not make public links in this
        // step; the policy already refuses the command, and this does too.
        if !matches!(principal, Principal::User) {
            return Err(refuse(
                "policy.denied",
                "Only the person makes share links.",
                Action::ChangePolicy,
            ));
        }
        if !cfg!(windows) {
            return Err(refuse(
                "share.unsupported",
                "Share links need a file lock this system cannot give yet.",
                Action::CorrectInput,
            ));
        }
        let base_url = {
            let state = self.sharing.state();
            if !state.enabled {
                return Err(not_ready(
                    "Sharing is off. Finish the setup and turn it on first.",
                ));
            }
            state.base_url.clone().ok_or_else(not_shareable)?
        };
        let expiry = match expires_in_seconds {
            None => DEFAULT_EXPIRY,
            Some(0) => return Err(input("A link must last at least one second.".into())),
            Some(seconds) => Duration::from_secs(seconds),
        };

        // Which file, and where it came from (the same rules as an offer).
        let (path, from_job) = match source {
            OfferSource::Job { job_id } => {
                let output = self
                    .session
                    .published_output(job_id.as_str())
                    .ok_or_else(not_shareable)?;
                if !output.public_origin {
                    return Err(credentialed());
                }
                if is_share(&output.path) {
                    return Err(not_shareable());
                }
                (output.path, true)
            }
            OfferSource::Path { path } => {
                let path = PathBuf::from(path.trim());
                if !path.is_absolute() || is_share(&path) || crate::plain_path(&path).is_err() {
                    return Err(not_shareable());
                }
                (path, false)
            }
        };
        if inside_staging(&path) {
            return Err(not_shareable());
        }
        let boundary = Boundary {
            data: self
                .session
                .data_dir()
                .and_then(|dir| std::fs::canonicalize(dir).ok()),
            grants: None,
        };
        // Opened so that nothing else can write, rename or delete it while
        // the handle is held: the bytes served are the bytes of this handle.
        let (mut file, canonical) =
            open_source(&path, &boundary, true).map_err(|why| match why {
                Unreadable::Missing | Unreadable::Outside => not_shareable(),
            })?;
        if !from_job {
            let remembered = self
                .inbox
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_remembered_credentialed(&canonical);
            if self.session.is_credentialed_output(&canonical) || remembered {
                return Err(credentialed());
            }
            if self.session.file_marker(&canonical) == Marker::Credentialed {
                return Err(credentialed());
            }
        }
        // A mapped network drive resolves to a UNC path: the bytes would come
        // from another computer, and the handle lock does not bind it.
        if is_remote(&canonical) {
            return Err(not_shareable());
        }
        let name = canonical_name(&canonical).ok_or_else(not_shareable)?;
        // A job's output is public by origin; only a chosen path is hashed, to
        // match it against content that came from a sign-in.
        let size = if from_job {
            file.metadata().map_err(|_| not_shareable())?.len()
        } else {
            let Ok(Some((size, sha256))) = hash_file(&mut file, None) else {
                return Err(not_shareable());
            };
            if self.session.is_credentialed_content(&sha256) {
                return Err(credentialed());
            }
            size
        };
        if !still_at(&file, &canonical) {
            return Err(not_shareable());
        }
        let created = self
            .sharing
            .store
            .create(NewShare {
                kind: ShareKind::File,
                name,
                source: Source::File(file),
                size,
                expiry,
            })
            .map_err(share_error)?;
        self.link_result(&base_url, &created.handle, &created.link_path)
    }

    /// The seven sharing commands. They are the person's alone (the policy
    /// refuses every other principal) and none is written to the ledger: a
    /// link is never stored, so a resend makes a new one.
    pub(super) fn query_share(
        &self,
        principal: &Principal,
        command: &Command,
    ) -> Result<CommandResult, ProtocolError> {
        self.sweep_shares();
        match command {
            Command::ShareStatus => {}
            Command::ConfigureSharing { base_url, port } => {
                self.configure_sharing(base_url.as_deref(), *port)?;
            }
            Command::ConfigureRelay { relay, key } => {
                self.configure_relay(*relay, key.as_ref())?;
            }
            Command::SetSharingEnabled { enabled } => self.set_sharing_enabled(*enabled)?,
            Command::StartShareTest => return self.start_share_test(),
            Command::CreateShare {
                source,
                expires_in_seconds,
            } => return self.create_share(principal, source, *expires_in_seconds),
            Command::RevokeShare { handle } => {
                if !self.sharing.store.revoke(handle) {
                    return Err(refuse(
                        "share.unknown_link",
                        "That link is gone already.",
                        Action::RefreshClient,
                    ));
                }
            }
            Command::RevokeAllShares => self.sharing.store.revoke_all(),
            _ => unreachable!("only sharing commands reach the share handler"),
        }
        Ok(CommandResult::Sharing {
            sharing: self.share_view(),
        })
    }
}

fn share_error(failure: ShareError) -> ProtocolError {
    match failure {
        ShareError::TooMany => refuse(
            "share.too_many",
            "Too many links are live. Revoke one first.",
            Action::CorrectInput,
        ),
        ShareError::BadName => not_shareable(),
        _ => refuse(
            "share.unavailable",
            "The link could not be made. The file may have changed while it was read.",
            Action::Retry,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_remote, parse_base};
    use std::path::Path;

    #[test]
    fn only_a_unc_canonical_path_is_remote() {
        assert!(is_remote(Path::new(r"\\?\UNC\host\share\a.txt")));
        assert!(is_remote(Path::new(r"\\?\unc\host\share\a.txt")));
        assert!(!is_remote(Path::new(r"\\?\C:\Users\a.txt")));
    }

    #[test]
    fn a_base_url_is_a_public_https_name_and_nothing_else() {
        assert_eq!(
            parse_base(" HTTPS://Share.Example.com/ ").unwrap(),
            (
                "https://share.example.com".into(),
                "share.example.com".into()
            )
        );
        assert_eq!(
            parse_base("https://share.example.com:443").unwrap().1,
            "share.example.com"
        );
        assert_eq!(
            parse_base("https://share.example.com:8443").unwrap().1,
            "share.example.com:8443"
        );
        for bad in [
            "http://share.example.com",
            "share.example.com",
            "https://",
            "https://user@share.example.com",
            "https://share.example.com/path",
            "https://share.example.com?x=1",
            "https://share.example.com#f",
            "https://127.0.0.1",
            "https://192.168.1.4:8080",
            "https://[::1]",
            "https://localhost",
            "https://fetchpath.localhost",
            "https://nas.local",
            "https://host",
            "https://-a.example.com",
            "https://a..example.com",
            "https://share.example.com:0",
            "https://share.example.com:99999",
            "https://shäre.example.com",
        ] {
            assert!(parse_base(bad).is_err(), "{bad}");
        }
    }
}
