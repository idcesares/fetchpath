use std::fmt;
use std::io::{self, SeekFrom};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;

use curl::easy::Easy;
use percent_encoding::percent_decode_str;
use russh::client;
use russh::client::AuthResult;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate, known_hosts, load_secret_key};
use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use url::Url;

use crate::{Chunk, TransferError};

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IO_BUFFER_BYTES: usize = 64 * 1024;
const HOST_KEY_NOT_CHECKED: u8 = 0;
const HOST_KEY_REJECTED: u8 = 1;
const HOST_KEY_MATCHED: u8 = 2;

#[derive(Clone, Eq, PartialEq)]
pub struct CredentialSecret(String);

impl CredentialSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Authentication {
    Password {
        username: String,
        password: CredentialSecret,
    },
    SshKey {
        username: String,
        private_key: PathBuf,
        passphrase: Option<CredentialSecret>,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompatibilityContext {
    pub authentication: Option<Authentication>,
    /// A PEM certificate bundle used in addition to the platform trust store.
    pub tls_ca_pem: Option<Vec<u8>>,
    /// An OpenSSH known_hosts file. SFTP fails closed when this is absent.
    pub ssh_known_hosts: Option<PathBuf>,
    /// Defaults to 15 seconds when omitted. The same bound applies to blocking
    /// SFTP operations so a stalled fixture or server cannot wait forever.
    pub timeout: Option<Duration>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityProtocol {
    Ftp,
    Ftps,
    Sftp,
}

impl CompatibilityProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ftp => "ftp",
            Self::Ftps => "ftps",
            Self::Sftp => "sftp",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompatibilityCapabilities {
    pub ftp: bool,
    pub ftps: bool,
    pub sftp: bool,
}

impl CompatibilityCapabilities {
    pub fn detect() -> Self {
        let version = curl::Version::get();
        let protocols: Vec<_> = version.protocols().collect();
        Self {
            ftp: protocols.contains(&"ftp"),
            ftps: protocols.contains(&"ftps"),
            sftp: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompatibilityTransferReport {
    pub protocol: CompatibilityProtocol,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
    pub resumed_from: u64,
}

pub fn transfer_compatibility<F, C>(
    raw_url: &str,
    context: &CompatibilityContext,
    start_offset: u64,
    cancelled: C,
    mut sink: F,
) -> Result<CompatibilityTransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool,
{
    let (url, protocol) = parse_url(raw_url)?;
    validate_context(protocol, context)?;
    match protocol {
        CompatibilityProtocol::Ftp | CompatibilityProtocol::Ftps => transfer_ftp(
            raw_url,
            protocol,
            context,
            start_offset,
            cancelled,
            &mut sink,
        ),
        CompatibilityProtocol::Sftp => {
            transfer_sftp(&url, context, start_offset, cancelled, &mut sink)
        }
    }
}

fn parse_url(raw_url: &str) -> Result<(Url, CompatibilityProtocol), TransferError> {
    let url = Url::parse(raw_url)
        .map_err(|_| TransferError::InvalidUrl("the URL is malformed".into()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(TransferError::InvalidUrl(
            "credentials must be supplied through the authentication context".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(TransferError::InvalidUrl("a host is required".into()));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(TransferError::InvalidUrl(
            "queries and fragments are not valid remote file paths".into(),
        ));
    }
    let protocol = match url.scheme() {
        "ftp" => CompatibilityProtocol::Ftp,
        "ftps" => CompatibilityProtocol::Ftps,
        "sftp" => CompatibilityProtocol::Sftp,
        _ => {
            return Err(TransferError::InvalidUrl(
                "only ftp, ftps, and sftp schemes are supported".into(),
            ));
        }
    };
    Ok((url, protocol))
}

fn validate_context(
    protocol: CompatibilityProtocol,
    context: &CompatibilityContext,
) -> Result<(), TransferError> {
    match (protocol, context.authentication.as_ref()) {
        (
            CompatibilityProtocol::Ftp | CompatibilityProtocol::Ftps,
            Some(Authentication::SshKey { .. }),
        ) => {
            return Err(TransferError::Authentication(
                "SSH keys are only supported by SFTP".into(),
            ));
        }
        (CompatibilityProtocol::Sftp, None) => {
            return Err(TransferError::Authentication(
                "SFTP requires password or private-key credentials".into(),
            ));
        }
        _ => {}
    }
    if protocol == CompatibilityProtocol::Sftp && context.ssh_known_hosts.is_none() {
        return Err(TransferError::HostKey(
            "an OpenSSH known_hosts file is required".into(),
        ));
    }
    if protocol != CompatibilityProtocol::Ftps && context.tls_ca_pem.is_some() {
        return Err(TransferError::Certificate(
            "a custom CA bundle is only valid for FTPS".into(),
        ));
    }
    Ok(())
}

fn transfer_ftp<F, C>(
    raw_url: &str,
    protocol: CompatibilityProtocol,
    context: &CompatibilityContext,
    start_offset: u64,
    cancelled: C,
    sink: &mut F,
) -> Result<CompatibilityTransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool,
{
    let timeout = context.timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT);
    let mut easy = Easy::new();
    easy.url(raw_url).map_err(map_curl_error)?;
    easy.fail_on_error(true).map_err(map_curl_error)?;
    easy.follow_location(false).map_err(map_curl_error)?;
    easy.ssl_verify_peer(true).map_err(map_curl_error)?;
    easy.ssl_verify_host(true).map_err(map_curl_error)?;
    easy.connect_timeout(timeout).map_err(map_curl_error)?;
    easy.timeout(timeout).map_err(map_curl_error)?;
    easy.progress(true).map_err(map_curl_error)?;
    easy.buffer_size(IO_BUFFER_BYTES).map_err(map_curl_error)?;
    if start_offset > 0 {
        easy.resume_from(start_offset).map_err(map_curl_error)?;
    }
    if let Some(Authentication::Password { username, password }) = &context.authentication {
        easy.username(username).map_err(map_curl_error)?;
        easy.password(password.expose()).map_err(map_curl_error)?;
    }
    if let Some(ca) = &context.tls_ca_pem {
        easy.ssl_cainfo_blob(ca).map_err(map_curl_error)?;
    }

    let current_offset = std::cell::Cell::new(start_offset);
    let callback_error = std::cell::RefCell::new(None);
    let result = {
        let mut transfer = easy.transfer();
        transfer
            .write_function(|bytes| {
                let offset = current_offset.get();
                if let Err(error) = sink(Chunk {
                    offset,
                    bytes,
                    strong_etag: None,
                    total_bytes: None,
                }) {
                    *callback_error.borrow_mut() = Some(error);
                    return Ok(0);
                }
                current_offset.set(offset + bytes.len() as u64);
                Ok(bytes.len())
            })
            .map_err(map_curl_error)?;
        transfer
            .progress_function(|_, _, _, _| !cancelled())
            .map_err(map_curl_error)?;
        transfer.perform()
    };
    if let Some(error) = callback_error.into_inner() {
        return Err(TransferError::Sink(error));
    }
    if cancelled() {
        return Err(TransferError::Cancelled);
    }
    result.map_err(map_curl_error)?;
    let total_bytes = current_offset.get();
    Ok(CompatibilityTransferReport {
        protocol,
        transferred_bytes: total_bytes - start_offset,
        total_bytes,
        resumed_from: start_offset,
    })
}

fn transfer_sftp<F, C>(
    url: &Url,
    context: &CompatibilityContext,
    start_offset: u64,
    cancelled: C,
    sink: &mut F,
) -> Result<CompatibilityTransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool,
{
    let host = url.host_str().expect("parse_url requires a host");
    let port = url.port().unwrap_or(22);
    let timeout = context.timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT);
    let decoded_path = percent_decode_str(url.path())
        .decode_utf8()
        .map_err(|_| TransferError::InvalidUrl("the SFTP path is not valid UTF-8".into()))?;
    if decoded_path.is_empty() || decoded_path == "/" {
        return Err(TransferError::InvalidUrl(
            "an SFTP file path is required".into(),
        ));
    }
    let known_hosts_path = context
        .ssh_known_hosts
        .clone()
        .expect("validate_context requires known_hosts");
    if !known_hosts_path.is_file() {
        return Err(TransferError::HostKey(
            "the known_hosts file could not be read".into(),
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| TransferError::Transport("failed to initialize the SFTP runtime".into()))?;
    runtime.block_on(transfer_sftp_async(
        host,
        port,
        decoded_path.as_ref(),
        context,
        known_hosts_path,
        timeout,
        start_offset,
        &cancelled,
        sink,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn transfer_sftp_async<F, C>(
    host: &str,
    port: u16,
    remote_path: &str,
    context: &CompatibilityContext,
    known_hosts_path: PathBuf,
    timeout: Duration,
    start_offset: u64,
    cancelled: &C,
    sink: &mut F,
) -> Result<CompatibilityTransferReport, TransferError>
where
    F: FnMut(Chunk<'_>) -> io::Result<()>,
    C: Fn() -> bool,
{
    let host_key_status = Arc::new(AtomicU8::new(HOST_KEY_NOT_CHECKED));
    let handler = SftpClient {
        host: host.to_owned(),
        port,
        known_hosts_path,
        host_key_status: Arc::clone(&host_key_status),
    };
    let config = client::Config {
        inactivity_timeout: Some(timeout),
        ..client::Config::default()
    };
    let connect = client::connect(Arc::new(config), (host, port), handler);
    let mut session = tokio::time::timeout(timeout, connect)
        .await
        .map_err(|_| TransferError::Transport("the SSH connection timed out".into()))?
        .map_err(|_| {
            if host_key_status.load(Ordering::Acquire) == HOST_KEY_REJECTED {
                TransferError::HostKey(
                    "the server key is absent from or differs from known_hosts".into(),
                )
            } else {
                TransferError::Transport("SSH connection or handshake failed".into())
            }
        })?;

    let authenticated = match context
        .authentication
        .as_ref()
        .expect("validate_context requires authentication")
    {
        Authentication::Password { username, password } => session
            .authenticate_password(username, password.expose())
            .await
            .map_err(|_| TransferError::Authentication("SFTP authentication failed".into()))?,
        Authentication::SshKey {
            username,
            private_key,
            passphrase,
        } => {
            let private_key = load_secret_key(
                private_key,
                passphrase.as_ref().map(CredentialSecret::expose),
            )
            .map_err(|_| {
                TransferError::Authentication("the SSH private key could not be loaded".into())
            })?;
            let hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(|_| TransferError::Authentication("SSH key negotiation failed".into()))?
                .flatten();
            session
                .authenticate_publickey(
                    username,
                    PrivateKeyWithHashAlg::new(Arc::new(private_key), hash),
                )
                .await
                .map_err(|_| TransferError::Authentication("SFTP authentication failed".into()))?
        }
    };
    if !matches!(authenticated, AuthResult::Success) {
        return Err(TransferError::Authentication(
            "the SFTP server rejected the credentials".into(),
        ));
    }

    let channel = session
        .channel_open_session()
        .await
        .map_err(|_| TransferError::Transport("failed to open an SSH channel".into()))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|_| TransferError::Transport("failed to start the SFTP subsystem".into()))?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|_| TransferError::Transport("failed to initialize SFTP".into()))?;
    sftp.set_timeout(timeout.as_secs().max(1));
    let metadata = sftp
        .metadata(remote_path)
        .await
        .map_err(|_| TransferError::Transport("SFTP path is unavailable".into()))?;
    let total_bytes = metadata
        .size
        .ok_or_else(|| TransferError::Transport("SFTP server omitted the file size".into()))?;
    if start_offset > total_bytes {
        return Err(TransferError::ResumeRejected(format!(
            "offset {start_offset} exceeds the remote size {total_bytes}"
        )));
    }
    let mut remote = sftp
        .open(remote_path)
        .await
        .map_err(|_| TransferError::Transport("SFTP file open failed".into()))?;
    remote
        .seek(SeekFrom::Start(start_offset))
        .await
        .map_err(|_| {
            TransferError::ResumeRejected("the server rejected the requested offset".into())
        })?;

    let mut buffer = vec![0_u8; IO_BUFFER_BYTES];
    let mut offset = start_offset;
    while offset < total_bytes {
        if cancelled() {
            return Err(TransferError::Cancelled);
        }
        let remaining = (total_bytes - offset).min(buffer.len() as u64) as usize;
        let read = remote
            .read(&mut buffer[..remaining])
            .await
            .map_err(|_| TransferError::Transport("SFTP read failed".into()))?;
        if read == 0 {
            return Err(TransferError::Transport(format!(
                "SFTP response ended at {offset} of {total_bytes} bytes"
            )));
        }
        sink(Chunk {
            offset,
            bytes: &buffer[..read],
            strong_etag: None,
            total_bytes: Some(total_bytes),
        })
        .map_err(TransferError::Sink)?;
        offset += read as u64;
    }

    let _ = sftp.close().await;
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "transfer complete", "en")
        .await;
    Ok(CompatibilityTransferReport {
        protocol: CompatibilityProtocol::Sftp,
        transferred_bytes: offset - start_offset,
        total_bytes,
        resumed_from: start_offset,
    })
}

#[derive(Debug)]
struct SftpClient {
    host: String,
    port: u16,
    known_hosts_path: PathBuf,
    host_key_status: Arc<AtomicU8>,
}

impl client::Handler for SftpClient {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let matched = known_hosts::check_known_hosts_path(
            &self.host,
            self.port,
            &server_public_key.public_key(),
            &self.known_hosts_path,
        )
        .unwrap_or(false);
        self.host_key_status.store(
            if matched {
                HOST_KEY_MATCHED
            } else {
                HOST_KEY_REJECTED
            },
            Ordering::Release,
        );
        Ok(matched)
    }
}

fn map_curl_error(error: curl::Error) -> TransferError {
    if error.is_login_denied() || error.is_remote_access_denied() {
        TransferError::Authentication("the FTP server rejected the credentials".into())
    } else if error.is_peer_failed_verification()
        || error.is_ssl_certproblem()
        || error.is_ssl_cacert()
        || error.is_ssl_cacert_badfile()
        || error.is_ssl_issuer_error()
    {
        TransferError::Certificate("the FTPS server certificate is not trusted".into())
    } else {
        TransferError::Transport(error.to_string())
    }
}
