//! `fetchpath-relay`: run a share relay on a VPS.
//!
//! ```text
//! fetchpath-relay enroll --state DIR [--rotate]
//! fetchpath-relay serve --state DIR --domain HOST --accept-acme-terms [--email ADDR]
//!                       [--staging] [--listen 0.0.0.0:443] [--http-listen 0.0.0.0:80]
//! fetchpath-relay serve --state DIR --cert CHAIN.pem --key KEY.pem [--listen ...]
//! ```
//!
//! The only secrets are the enrollment key and the ACME account and
//! certificate keys, all in the state directory. Log lines are fixed event
//! names: no path, ticket id, header, key or body byte reaches them.

use fetchpath_relay::acme::{self, Challenges};
use fetchpath_relay::server::{Limits, Relay};
use fetchpath_relay::tls::{self, Certificate};
use fetchpath_relay::wire::EnrollmentKey;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

/// Renew early: certificate lifetimes are getting shorter.
const RENEW_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

struct Args(Vec<String>);

impl Args {
    fn value(&self, name: &str) -> Option<&str> {
        let at = self.0.iter().position(|arg| arg == name)?;
        self.0.get(at + 1).map(String::as_str)
    }

    fn flag(&self, name: &str) -> bool {
        self.0.iter().any(|arg| arg == name)
    }
}

fn main() -> ExitCode {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        return usage();
    }
    let command = raw.remove(0);
    let args = Args(raw);
    let outcome = match command.as_str() {
        "enroll" => enroll(&args),
        "serve" => tokio::runtime::Runtime::new()
            .map_err(|error| error.to_string())
            .and_then(|runtime| runtime.block_on(serve(&args))),
        _ => return usage(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("fetchpath-relay: {message}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage:\n  fetchpath-relay enroll --state DIR [--rotate]\n  fetchpath-relay serve --state DIR --domain HOST --accept-acme-terms [--email ADDR] [--staging]\n                        [--listen 0.0.0.0:443] [--http-listen 0.0.0.0:80]\n  fetchpath-relay serve --state DIR --cert CHAIN.pem --key KEY.pem [--listen 0.0.0.0:443]"
    );
    ExitCode::from(2)
}

fn state_dir(args: &Args) -> Result<PathBuf, String> {
    let dir = PathBuf::from(args.value("--state").ok_or("--state DIR is required")?);
    std::fs::create_dir_all(&dir).map_err(|error| format!("state directory: {error}"))?;
    Ok(dir)
}

fn key_path(state: &Path) -> PathBuf {
    state.join("enrollment.key")
}

/// Creates the enrollment key if there is none (or with --rotate) and prints
/// it: the owner pastes it into each engine that may use this relay.
fn enroll(args: &Args) -> Result<(), String> {
    let state = state_dir(args)?;
    let path = key_path(&state);
    let existing = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| EnrollmentKey::from_hex(&text));
    let key = match existing {
        Some(key) if !args.flag("--rotate") => key,
        _ => {
            let key = EnrollmentKey::generate().map_err(|error| error.to_string())?;
            acme::write_private(&path, key.to_hex().as_bytes())
                .map_err(|error| format!("could not save the key: {error}"))?;
            key
        }
    };
    println!("{}", key.to_hex());
    Ok(())
}

fn load_key(state: &Path) -> Result<EnrollmentKey, String> {
    std::fs::read_to_string(key_path(state))
        .ok()
        .and_then(|text| EnrollmentKey::from_hex(&text))
        .ok_or_else(|| {
            "no enrollment key: run `fetchpath-relay enroll --state DIR` first".to_owned()
        })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

async fn serve(args: &Args) -> Result<(), String> {
    let state = state_dir(args)?;
    let key = load_key(&state)?;
    let listen = args.value("--listen").unwrap_or("0.0.0.0:443");
    let certificate = Certificate::new();

    let relay = Relay::new(key, Limits::default(), |event| println!("relay: {event}"));
    let manual = args.value("--cert").zip(args.value("--key"));
    if let Some((chain, private)) = manual {
        let chain = std::fs::read_to_string(chain).map_err(|error| format!("--cert: {error}"))?;
        let private =
            std::fs::read_to_string(private).map_err(|error| format!("--key: {error}"))?;
        certificate
            .set_pem(&chain, &private)
            .map_err(|error| error.to_string())?;
    } else {
        let domain = args
            .value("--domain")
            .ok_or("--domain HOST (or --cert and --key) is required")?;
        if !args.flag("--accept-acme-terms") {
            return Err(
                "--accept-acme-terms is required: it agrees to the Let's Encrypt subscriber terms"
                    .into(),
            );
        }
        let http = TcpListener::bind(args.value("--http-listen").unwrap_or("0.0.0.0:80"))
            .await
            .map_err(|error| format!("HTTP port for the certificate challenge: {error}"))?;
        let challenges = Challenges::new();
        tokio::spawn(acme::http_challenges(http, Arc::clone(&challenges)));
        let settings = AcmeSettings {
            domain: domain.to_owned(),
            email: args.value("--email").map(str::to_owned),
            staging: args.flag("--staging"),
            state: state.clone(),
        };
        // The first certificate is needed before any visitor can connect.
        refresh(&settings, &challenges, &certificate, true).await?;
        tokio::spawn(renew_forever(
            settings,
            challenges,
            Arc::clone(&certificate),
        ));
    }

    let listener = TcpListener::bind(listen)
        .await
        .map_err(|error| format!("{listen}: {error}"))?;
    println!("relay: listening");
    tokio::select! {
        () = tls::serve(listener, relay, tls::acceptor(certificate).map_err(|error| error.to_string())?) => {
            return Err("the listener stopped".to_owned());
        }
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}

struct AcmeSettings {
    domain: String,
    email: Option<String>,
    staging: bool,
    state: PathBuf,
}

/// Loads the stored certificate when it is fresh, otherwise gets a new one.
async fn refresh(
    settings: &AcmeSettings,
    challenges: &Challenges,
    certificate: &Certificate,
    first: bool,
) -> Result<(), String> {
    let chain_path = settings.state.join("cert-chain.pem");
    let key_path = settings.state.join("cert-key.pem");
    let meta_path = settings.state.join("cert-meta");
    let fresh = std::fs::read_to_string(&meta_path).is_ok_and(|meta| {
        acme::stored_is_usable(
            &meta,
            &settings.domain,
            settings.staging,
            now(),
            RENEW_AFTER.as_secs(),
        )
    });
    if fresh
        && !certificate.is_set()
        && let (Ok(chain), Ok(private)) = (
            std::fs::read_to_string(&chain_path),
            std::fs::read_to_string(&key_path),
        )
        && certificate.set_pem(&chain, &private).is_ok()
    {
        println!("relay: certificate loaded");
        return Ok(());
    }
    if fresh && certificate.is_set() {
        return Ok(());
    }
    // Past the renewal age but still the right domain and CA: keep it as a
    // fallback so a Let's Encrypt outage at restart is not fatal.
    if !certificate.is_set()
        && std::fs::read_to_string(&meta_path).is_ok_and(|meta| {
            acme::stored_is_usable(
                &meta,
                &settings.domain,
                settings.staging,
                now(),
                40 * 24 * 60 * 60,
            )
        })
        && let (Ok(chain), Ok(private)) = (
            std::fs::read_to_string(&chain_path),
            std::fs::read_to_string(&key_path),
        )
    {
        let _ = certificate.set_pem(&chain, &private);
    }
    let request = acme::Request {
        domain: &settings.domain,
        email: settings.email.as_deref(),
        staging: settings.staging,
        state: &settings.state,
    };
    match acme::obtain(&request, challenges).await {
        Ok((chain, private)) => {
            certificate
                .set_pem(&chain, &private)
                .map_err(|error| error.to_string())?;
            let saved = acme::write_private(&key_path, private.as_bytes())
                .and_then(|()| acme::write_private(&chain_path, chain.as_bytes()))
                .and_then(|()| {
                    let meta = acme::certificate_meta(&settings.domain, settings.staging, now());
                    acme::write_private(&meta_path, meta.as_bytes())
                });
            if let Err(error) = saved {
                eprintln!("fetchpath-relay: could not save the certificate: {error}");
            }
            println!("relay: certificate issued");
            Ok(())
        }
        // A failed renewal keeps serving the old certificate; only the very
        // first one is fatal.
        Err(message) if first && !certificate.is_set() => Err(message),
        Err(message) => {
            eprintln!("fetchpath-relay: renewal failed: {message}");
            Ok(())
        }
    }
}

async fn renew_forever(
    settings: AcmeSettings,
    challenges: Arc<Challenges>,
    certificate: Arc<Certificate>,
) {
    loop {
        tokio::time::sleep(Duration::from_secs(12 * 60 * 60)).await;
        let _ = refresh(&settings, &challenges, &certificate, false).await;
    }
}
