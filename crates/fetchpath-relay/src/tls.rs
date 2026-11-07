//! The relay's TLS front: one port, two roles told apart by ALPN.
//!
//! A browser offers `http/1.1` (or nothing) and is a visitor. An enrolled
//! engine offers `fetchpath-relay/1` and goes through the enrollment
//! handshake. The certificate can be replaced while the relay runs.

use crate::server::Relay;
use crate::wire::ALPN;
use rustls::ServerConfig;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::io;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

/// TLS handshakes that may be in flight at once, before any role is known.
const HANDSHAKES: usize = 256;
const TLS_TIMEOUT: Duration = Duration::from_secs(10);

/// The certificate the relay presents; `set_pem` swaps it without a restart.
#[derive(Debug, Default)]
pub struct Certificate(RwLock<Option<Arc<CertifiedKey>>>);

impl Certificate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn is_set(&self) -> bool {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    pub fn set_pem(&self, chain: &str, key: &str) -> io::Result<()> {
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_owned());
        let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(chain.as_bytes())
            .collect::<Result<_, _>>()
            .map_err(|_| invalid("the certificate chain is not valid PEM"))?;
        if chain.is_empty() {
            return Err(invalid("the certificate chain is empty"));
        }
        let key = PrivateKeyDer::from_pem_slice(key.as_bytes())
            .map_err(|_| invalid("the private key is not valid PEM"))?;
        let certified = CertifiedKey::from_der(chain, key, &aws_lc_rs::default_provider())
            .map_err(|error| invalid(&format!("the key does not fit the chain: {error}")))?;
        *self
            .0
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(certified));
        Ok(())
    }
}

impl ResolvesServerCert for Certificate {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.0
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// TLS 1.2+ with the relay's ALPN list; no client certificates.
pub fn acceptor(certificate: Arc<Certificate>) -> io::Result<TlsAcceptor> {
    let mut config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_cert_resolver(certificate);
    config.alpn_protocols = vec![ALPN.to_vec(), b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Accepts until the listener fails: engines by ALPN, everything else is a
/// visitor.
pub async fn serve(listener: TcpListener, relay: Arc<Relay>, acceptor: TlsAcceptor) {
    let handshakes = Arc::new(Semaphore::new(HANDSHAKES));
    loop {
        // A failed accept (descriptor exhaustion, an aborted connection) is
        // not the end of the relay: wait a moment and keep accepting.
        let Ok((tcp, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = Arc::clone(&handshakes).try_acquire_owned() else {
            continue;
        };
        let relay = Arc::clone(&relay);
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let secured = tokio::time::timeout(TLS_TIMEOUT, acceptor.accept(tcp)).await;
            drop(permit);
            let Ok(Ok(stream)) = secured else {
                return;
            };
            let engine = stream.get_ref().1.alpn_protocol() == Some(ALPN);
            if engine {
                relay.serve_engine(stream).await;
            } else {
                relay.serve_visitor(stream).await;
            }
        });
    }
}
