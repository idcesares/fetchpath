//! The engine's TLS dial to a relay (FP-118).
//!
//! The relay is authenticated by its certificate for the configured name,
//! checked by the operating system's verifier (Windows' certificate store on
//! the engine's platform). The engine offers ALPN `fetchpath-relay/1`, which
//! is how the relay tells it from a browser.

use crate::client::Connector;
use crate::server::Conn;
use crate::wire::ALPN;
use rustls::ClientConfig;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls_platform_verifier::Verifier;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A connector that dials `address` (`host:port`) and verifies the relay as
/// `server_name`. `extra_roots` are trusted in addition to the system's; the
/// engine passes none, tests pass their own certificate.
pub fn tls_connector(
    server_name: &str,
    address: String,
    extra_roots: Vec<CertificateDer<'static>>,
) -> io::Result<Connector> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let verifier = if extra_roots.is_empty() {
        Verifier::new(Arc::clone(&provider))
    } else {
        Verifier::new_with_extra_roots(extra_roots, Arc::clone(&provider))
    }
    .map_err(io::Error::other)?;
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![ALPN.to_vec()];
    let connector = TlsConnector::from(Arc::new(config));
    let name = ServerName::try_from(server_name.to_owned())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "the relay name is not valid"))?;
    Ok(Arc::new(move || {
        let connector = connector.clone();
        let name = name.clone();
        let address = address.clone();
        Box::pin(async move {
            let secured = async {
                let tcp = TcpStream::connect(&address).await?;
                let _ = tcp.set_nodelay(true);
                connector.connect(name, tcp).await
            };
            let stream = tokio::time::timeout(TIMEOUT, secured).await.map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "the relay did not answer")
            })??;
            Ok(Box::new(stream) as Conn)
        })
    }))
}
