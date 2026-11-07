//! The TLS front on loopback: one port, an engine by ALPN and a visitor by
//! `http/1.1`, and an engine without the key refused. Run with
//! `--features serve`.
#![cfg(feature = "serve")]

use fetchpath_relay::client::{self, Connector, Handler, Settings, Status};
use fetchpath_relay::server::{Conn, Limits, Relay};
use fetchpath_relay::tls::{self, Certificate};
use fetchpath_relay::wire::{ALPN, EnrollmentKey};
use rustls::ClientConfig;
use rustls::RootCertStore;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::ServerName;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_rustls::TlsConnector;

const ID: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";

fn connector_for(alpn: &[u8], roots: RootCertStore) -> TlsConnector {
    let mut config = ClientConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    TlsConnector::from(Arc::new(config))
}

struct Front {
    address: std::net::SocketAddr,
    roots: RootCertStore,
    key: EnrollmentKey,
}

async fn front() -> Front {
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate = Certificate::new();
    certificate
        .set_pem(
            &generated.cert.pem(),
            &generated.signing_key.serialize_pem(),
        )
        .unwrap();
    let mut roots = RootCertStore::empty();
    roots.add(generated.cert.der().clone()).unwrap();
    let key = EnrollmentKey::generate().unwrap();
    let relay = Relay::new(key.clone(), Limits::default(), |_| {});
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(tls::serve(
        listener,
        relay,
        tls::acceptor(certificate).unwrap(),
    ));
    Front {
        address,
        roots,
        key,
    }
}

fn engine_connector(front: &Front) -> Connector {
    let tls = connector_for(ALPN, front.roots.clone());
    let address = front.address;
    Arc::new(move || {
        let tls = tls.clone();
        Box::pin(async move {
            let tcp = TcpStream::connect(address).await?;
            let stream = tls
                .connect(ServerName::try_from("localhost").unwrap(), tcp)
                .await?;
            Ok(Box::new(stream) as Conn)
        })
    })
}

fn hello() -> Handler {
    Arc::new(|mut conn| {
        Box::pin(async move {
            let mut head = Vec::new();
            let mut chunk = [0u8; 256];
            while !head.windows(4).any(|window| window == b"\r\n\r\n") {
                match conn.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => head.extend_from_slice(&chunk[..read]),
                }
            }
            let _ = conn
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                )
                .await;
            let _ = conn.shutdown().await;
        })
    })
}

#[tokio::test]
async fn an_enrolled_engine_and_a_visitor_share_one_tls_port() {
    let front = front().await;
    let (_ids, ids_rx) = watch::channel(vec![ID.to_owned()]);
    let (status_tx, mut status) = watch::channel(Status::Connecting);
    let task = tokio::spawn(client::run(
        Settings::new(front.key.clone(), "laptop"),
        engine_connector(&front),
        ids_rx,
        hello(),
        status_tx,
    ));
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|s| *s == Status::Enrolled),
    )
    .await
    .expect("enrolled over TLS")
    .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;

    let visitor = connector_for(b"http/1.1", front.roots.clone());
    let tcp = TcpStream::connect(front.address).await.unwrap();
    let mut stream = visitor
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    stream
        .write_all(format!("GET /s/{ID} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer)).await;
    let text = String::from_utf8_lossy(&answer);
    assert!(text.starts_with("HTTP/1.1 200 OK"), "{text}");
    assert!(text.ends_with("hello"), "{text}");
    task.abort();
}

#[tokio::test]
async fn an_engine_with_the_wrong_key_is_refused_over_tls() {
    let front = front().await;
    let (_ids, ids_rx) = watch::channel(vec![ID.to_owned()]);
    let (status_tx, mut status) = watch::channel(Status::Connecting);
    let task = tokio::spawn(client::run(
        Settings::new(EnrollmentKey::generate().unwrap(), "intruder"),
        engine_connector(&front),
        ids_rx,
        hello(),
        status_tx,
    ));
    tokio::time::timeout(
        Duration::from_secs(5),
        status.wait_for(|s| *s == Status::Refused),
    )
    .await
    .expect("refused over TLS")
    .unwrap();
    task.abort();
}
