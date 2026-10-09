//! Requests to the web UI listener (FP-104).
//!
//! Every request is checked against the host allowlist first, so a page on
//! another name (DNS rebinding) learns nothing. Pages carry no inline script
//! or style, because the policy header forbids them.

use super::sessions::{COOKIE_NAME, now_ms};
use super::{Shared, assets, lock, socket};
use crate::engine::Connected;
use fetchpath_protocol::principal::DeviceId;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::header::{
    ALLOW, CACHE_CONTROL, CONNECTION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, COOKIE, HOST,
    HeaderName, HeaderValue, LOCATION, ORIGIN, REFERRER_POLICY, SET_COOKIE, UPGRADE,
    X_CONTENT_TYPE_OPTIONS,
};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

/// The attributes of the session cookie, in one place. It has no `Max-Age`:
/// it ends with the browser session, as the engine's sessions end with it.
/// `Secure` is left off
/// because the page is plain http on loopback, where a browser that honors
/// it would not send the cookie back.
const COOKIE_ATTRIBUTES: &str = "HttpOnly; SameSite=Strict; Path=/";

const POLICY: &str = "default-src 'self'; frame-ancestors 'none'";

const SIGNED_OUT: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>Fetchpath</title></head><body><h1>Fetchpath</h1>\
<p>Open Fetchpath from the tray or desktop to sign in.</p></body></html>";

type Reply = Response<Full<Bytes>>;

fn value(text: &str) -> HeaderValue {
    HeaderValue::from_str(text).expect("a valid header value")
}

fn reply(status: StatusCode, content_type: Option<&str>, body: &'static [u8]) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from_static(body)));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, value(POLICY));
    headers.insert(REFERRER_POLICY, value("no-referrer"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, value("nosniff"));
    if let Some(content_type) = content_type {
        headers.insert(CONTENT_TYPE, value(content_type));
        if content_type.starts_with("text/html") {
            headers.insert(CACHE_CONTROL, value("no-store"));
        }
    }
    response
}

fn status(status: StatusCode) -> Reply {
    let mut response = reply(status, None, b"");
    // A refused client does not get to hold the connection open.
    if status.is_client_error() {
        response.headers_mut().insert(CONNECTION, value("close"));
    }
    response
}

fn signed_out() -> Reply {
    reply(
        StatusCode::OK,
        Some("text/html; charset=utf-8"),
        SIGNED_OUT.as_bytes(),
    )
}

/// The request's `Host`, when there is exactly one, it names this listener
/// and an absolute-form target agrees with it.
fn allowed_host(shared: &Shared, request: &Request<Incoming>) -> Option<String> {
    let mut values = request.headers().get_all(HOST).iter();
    let host = values.next()?.to_str().ok()?.to_ascii_lowercase();
    if values.next().is_some() || !shared.host_allowed(&host) {
        return None;
    }
    if let Some(authority) = request.uri().authority()
        && authority.as_str().to_ascii_lowercase() != host
    {
        return None;
    }
    Some(host)
}

/// The device of the request's session cookie, when it is a live session.
fn signed_in(shared: &Shared, request: &Request<Incoming>) -> Option<DeviceId> {
    // Every `fp_session` value is tried: another local server on a sibling
    // name can plant its own ahead of the real one.
    request
        .headers()
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .filter(|(name, _)| *name == COOKIE_NAME)
        .find_map(|(_, value)| lock(&shared.sessions).check(value, now_ms()))
}

pub(super) fn respond(shared: &Arc<Shared>, mut request: Request<Incoming>) -> Reply {
    let Some(host) = allowed_host(shared, &request) else {
        return status(StatusCode::MISDIRECTED_REQUEST);
    };
    // A page on another site names itself in `Origin`; anything but this
    // listener's own page is turned away before a route sees it.
    let mut origins = request.headers().get_all(ORIGIN).iter();
    if let Some(origin) = origins.next()
        && (origins.next().is_some()
            || origin.to_str().map(str::to_ascii_lowercase).ok() != Some(format!("http://{host}")))
    {
        return status(StatusCode::FORBIDDEN);
    }
    if request.method() != Method::GET {
        let mut response = status(StatusCode::METHOD_NOT_ALLOWED);
        response.headers_mut().insert(ALLOW, value("GET"));
        return response;
    }
    match request.uri().path() {
        "/open" => open(shared, &request, &host),
        "/ui/socket" => upgrade(shared, &mut request, &host),
        path => {
            if signed_in(shared, &request).is_none() {
                return if path == "/" {
                    signed_out()
                } else {
                    status(StatusCode::NOT_FOUND)
                };
            }
            match assets::lookup(path) {
                Some((body, content_type)) => reply(StatusCode::OK, Some(content_type), body),
                None => status(StatusCode::NOT_FOUND),
            }
        }
    }
}

/// Trades a launch ticket for a session. The cookie is set only on the
/// canonical host, which keeps it from other local servers; anywhere else
/// the ticket is left unused.
fn open(shared: &Shared, request: &Request<Incoming>, host: &str) -> Reply {
    let ticket = request.uri().query().and_then(|query| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(name, _)| name == "ticket")
            .map(|(_, value)| value.into_owned())
    });
    let created = (host == shared.canonical_host())
        .then_some(ticket)
        .flatten()
        .and_then(|ticket| shared.sign_in(&ticket));
    let Some(created) = created else {
        return signed_out();
    };
    shared.close_devices(&created.evicted);
    let mut response = status(StatusCode::SEE_OTHER);
    let headers = response.headers_mut();
    headers.insert(LOCATION, value("/"));
    headers.insert(CACHE_CONTROL, value("no-store"));
    headers.insert(
        SET_COOKIE,
        value(&format!(
            "{COOKIE_NAME}={}; {COOKIE_ATTRIBUTES}",
            created.cookie
        )),
    );
    response
}

/// Whether a header carries `token` among its comma-separated values.
fn has_token(request: &Request<Incoming>, name: HeaderName, token: &str) -> bool {
    request
        .headers()
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|value| value.trim().eq_ignore_ascii_case(token))
}

/// The WebSocket upgrade: a live session, and an `Origin` that is this very
/// page's own, or a page on another site could drive the queue with the
/// cookie the browser attaches.
fn upgrade(shared: &Arc<Shared>, request: &mut Request<Incoming>, host: &str) -> Reply {
    let Some(device) = signed_in(shared, request) else {
        return status(StatusCode::FORBIDDEN);
    };
    let mut origins = request.headers().get_all(ORIGIN).iter();
    let same_origin = origins
        .next()
        .and_then(|origin| origin.to_str().ok())
        .is_some_and(|origin| origin.to_ascii_lowercase() == format!("http://{host}"));
    if !same_origin || origins.next().is_some() {
        return status(StatusCode::FORBIDDEN);
    }
    let accept = request
        .headers()
        .get("sec-websocket-key")
        .map(|key| derive_accept_key(key.as_bytes()));
    let version_13 = request
        .headers()
        .get("sec-websocket-version")
        .is_some_and(|version| version.as_bytes() == b"13");
    let websocket = request
        .headers()
        .get(UPGRADE)
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"));
    let Some(accept) =
        accept.filter(|_| version_13 && websocket && has_token(request, CONNECTION, "upgrade"))
    else {
        return status(StatusCode::BAD_REQUEST);
    };
    let Some((id, token)) = shared.register(&device) else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    // Counted from the answer on, like a pipe connection from acceptance.
    let counted = Connected::open(&shared.connections);
    let upgraded = hyper::upgrade::on(request);
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        if let Ok(upgraded) = upgraded.await {
            socket::run(&shared, &device, &token, TokioIo::new(upgraded)).await;
        }
        shared.unregister(id);
        drop(counted);
    });
    let mut response = status(StatusCode::SWITCHING_PROTOCOLS);
    let headers = response.headers_mut();
    headers.insert(UPGRADE, value("websocket"));
    headers.insert(CONNECTION, value("Upgrade"));
    headers.insert("sec-websocket-accept", value(&accept));
    response
}
