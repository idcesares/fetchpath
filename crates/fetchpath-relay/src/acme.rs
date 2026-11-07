//! The relay's own certificate from Let's Encrypt, by HTTP-01.
//!
//! The relay answers `/.well-known/acme-challenge/<token>` on its HTTP port
//! while an order is open and nothing else. The account key and the issued
//! chain live in the state directory; the certificate key is made fresh for
//! every order.

use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PREFIX: &str = "/.well-known/acme-challenge/";

/// Open HTTP-01 challenges: token to key authorization.
#[derive(Default)]
pub struct Challenges(Mutex<HashMap<String, String>>);

impl Challenges {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn answer(&self, token: &str) -> Option<String> {
        self.map().get(token).cloned()
    }
}

/// Serves challenge answers; every other request gets a 404.
pub async fn http_challenges(listener: TcpListener, challenges: Arc<Challenges>) {
    let open = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let Ok(permit) = Arc::clone(&open).try_acquire_owned() else {
            continue;
        };
        let challenges = Arc::clone(&challenges);
        tokio::spawn(async move {
            let _permit = permit;
            let work = async {
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).await.ok()?;
                let head = std::str::from_utf8(&buffer[..read]).ok()?;
                let path = head
                    .lines()
                    .next()?
                    .strip_prefix("GET ")?
                    .split(' ')
                    .next()?;
                challenges.answer(path.strip_prefix(PREFIX)?)
            };
            let body = tokio::time::timeout(Duration::from_secs(5), work)
                .await
                .ok()
                .flatten();
            let response = match body {
                Some(body) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned(),
            };
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

pub struct Request<'a> {
    pub domain: &'a str,
    pub email: Option<&'a str>,
    pub staging: bool,
    pub state: &'a Path,
}

/// A new certificate chain and its private key, both PEM.
pub async fn obtain(
    request: &Request<'_>,
    challenges: &Challenges,
) -> Result<(String, String), String> {
    let account = account(request).await?;
    let identifiers = [Identifier::Dns(request.domain.to_owned())];
    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(|error| format!("the order was refused: {error}"))?;
    let mut tokens = Vec::new();
    let mut authorizations = order.authorizations();
    while let Some(result) = authorizations.next().await {
        let mut authorization = result.map_err(|error| format!("authorization failed: {error}"))?;
        match authorization.status {
            AuthorizationStatus::Pending => {}
            AuthorizationStatus::Valid => continue,
            other => return Err(format!("authorization is {other:?}")),
        }
        let mut challenge = authorization
            .challenge(ChallengeType::Http01)
            .ok_or("the server offered no HTTP-01 challenge")?;
        let authorization_value = challenge.key_authorization().as_str().to_owned();
        let token = authorization_value
            .split('.')
            .next()
            .unwrap_or_default()
            .to_owned();
        challenges.map().insert(token.clone(), authorization_value);
        tokens.push(token);
        challenge
            .set_ready()
            .await
            .map_err(|error| format!("could not start validation: {error}"))?;
    }
    let outcome = async {
        let status = order
            .poll_ready(&RetryPolicy::default())
            .await
            .map_err(|error| format!("validation failed: {error}"))?;
        if status != OrderStatus::Ready {
            return Err(format!("the order ended as {status:?}"));
        }
        let key = order
            .finalize()
            .await
            .map_err(|error| format!("finalizing failed: {error}"))?;
        let chain = order
            .poll_certificate(&RetryPolicy::default())
            .await
            .map_err(|error| format!("the certificate was not issued: {error}"))?;
        Ok((chain, key))
    }
    .await;
    for token in tokens {
        challenges.map().remove(&token);
    }
    outcome
}

async fn account(request: &Request<'_>) -> Result<Account, String> {
    let path = request.state.join(account_file(request.staging));
    let builder = || Account::builder().map_err(|error| format!("ACME client: {error}"));
    if let Ok(stored) = std::fs::read(&path) {
        let credentials: AccountCredentials = serde_json::from_slice(&stored).map_err(|_| {
            "the ACME account file is damaged; delete it to register again".to_owned()
        })?;
        return builder()?
            .from_credentials(credentials)
            .await
            .map_err(|error| format!("the stored ACME account was refused: {error}"));
    }
    let contact = request.email.map(|email| format!("mailto:{email}"));
    let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
    let url = if request.staging {
        LetsEncrypt::Staging.url()
    } else {
        LetsEncrypt::Production.url()
    };
    let (account, credentials) = builder()?
        .create(
            &NewAccount {
                contact: &contacts,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            url.to_owned(),
            None,
        )
        .await
        .map_err(|error| format!("could not register an ACME account: {error}"))?;
    let json = serde_json::to_vec(&credentials).map_err(|error| error.to_string())?;
    write_private(&path, &json).map_err(|error| format!("could not save the account: {error}"))?;
    Ok(account)
}

/// Writes a file only the owner can read (on Unix; elsewhere the default).
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `mode` above only applies to a file this call created.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}

/// The account belongs to one CA: a staging account must never be reused for
/// production, because its credentials carry the staging directory.
pub fn account_file(staging: bool) -> &'static str {
    if staging {
        "acme-account-staging.json"
    } else {
        "acme-account-production.json"
    }
}

/// What the stored certificate was issued for, kept beside it.
pub fn certificate_meta(domain: &str, staging: bool, issued: u64) -> String {
    let ca = if staging { "staging" } else { "production" };
    format!("domain={domain}\nca={ca}\nissued={issued}\n")
}

/// Whether a stored certificate may be served: same domain, same CA, and
/// issued less than `renew_after` seconds ago.
pub fn stored_is_usable(
    meta: &str,
    domain: &str,
    staging: bool,
    now: u64,
    renew_after: u64,
) -> bool {
    let field = |name: &str| {
        meta.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
    };
    let ca = if staging { "staging" } else { "production" };
    let issued = field("issued").and_then(|value| value.parse::<u64>().ok());
    field("domain") == Some(domain)
        && field("ca") == Some(ca)
        && issued.is_some_and(|issued| now.saturating_sub(issued) < renew_after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_certificate_is_only_for_its_domain_and_ca_and_age() {
        let day = 24 * 60 * 60;
        let meta = certificate_meta("share.example", false, 1_000);
        assert!(stored_is_usable(
            &meta,
            "share.example",
            false,
            1_000 + day,
            60 * day
        ));
        assert!(!stored_is_usable(
            &meta,
            "other.example",
            false,
            1_000 + day,
            60 * day
        ));
        assert!(!stored_is_usable(
            &meta,
            "share.example",
            true,
            1_000 + day,
            60 * day
        ));
        assert!(!stored_is_usable(
            &meta,
            "share.example",
            false,
            1_000 + 61 * day,
            60 * day
        ));
        assert!(!stored_is_usable("", "share.example", false, 0, 60 * day));
        assert!(!stored_is_usable(
            "domain=share.example\nca=production\nissued=x",
            "share.example",
            false,
            5,
            day
        ));
    }

    #[test]
    fn staging_and_production_do_not_share_an_account_file() {
        assert_ne!(account_file(true), account_file(false));
    }
}
