//! Requests to the share listener (FP-117).
//!
//! Exactly one route exists: `GET /s/<id>`. The bare path answers with the
//! page (which does not use the ticket), `?d=1` answers with the encrypted
//! stream. Everything else is a 404 or 405 that says nothing. Responses carry
//! no file name, no hash and no `Content-Disposition`: the name rides inside
//! the ciphertext, and the key never leaves the URL fragment.

use super::Shared;
use super::body::Body;
use super::page;
use fetchpath_session::share::ShareError;
use hyper::body::{Bytes, Incoming};
use hyper::header::{
    ACCEPT_RANGES, ALLOW, CACHE_CONTROL, CONNECTION, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, HeaderName, HeaderValue, RANGE, REFERRER_POLICY,
    X_CONTENT_TYPE_OPTIONS,
};
use hyper::{Method, Request, Response, StatusCode};

type Reply = Response<Body>;

const ROBOTS: HeaderName = HeaderName::from_static("x-robots-tag");
const RESOURCE_POLICY: HeaderName = HeaderName::from_static("cross-origin-resource-policy");
/// Characters of a ticket id: 32 random bytes as base64url.
const ID_LEN: usize = 43;

fn value(text: &str) -> HeaderValue {
    HeaderValue::from_str(text).expect("a valid header value")
}

fn reply(status: StatusCode, content_type: Option<&str>, policy: &str, body: Body) -> Reply {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, value(policy));
    headers.insert(REFERRER_POLICY, value("no-referrer"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, value("nosniff"));
    headers.insert(CACHE_CONTROL, value("no-store"));
    headers.insert(ROBOTS, value("noindex, nofollow"));
    headers.insert(RESOURCE_POLICY, value("same-origin"));
    if let Some(content_type) = content_type {
        headers.insert(CONTENT_TYPE, value(content_type));
    }
    response
}

fn refuse(status: StatusCode) -> Reply {
    let mut response = reply(status, None, page::PLAIN_POLICY, Body::empty());
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
    // A refused client does not get to hold the connection open.
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

fn ended() -> Reply {
    let ended = page::ended();
    let html = Bytes::from_static(ended.html.as_bytes());
    let length = html.len();
    let mut response = reply(
        StatusCode::GONE,
        Some("text/html; charset=utf-8"),
        &ended.policy,
        Body::page(html),
    );
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, value(&length.to_string()));
    response
}

fn from_error(error: ShareError) -> Reply {
    match error {
        ShareError::WrongHost => refuse(StatusCode::MISDIRECTED_REQUEST),
        ShareError::Gone => ended(),
        ShareError::Range => refuse(StatusCode::RANGE_NOT_SATISFIABLE),
        ShareError::TooMany => refuse(StatusCode::TOO_MANY_REQUESTS),
        ShareError::Off | ShareError::Unknown => refuse(StatusCode::NOT_FOUND),
        ShareError::BadName | ShareError::Unavailable => refuse(StatusCode::SERVICE_UNAVAILABLE),
    }
}

/// The request's `Host` when there is exactly one and an absolute-form target
/// agrees with it.
fn host_of(request: &Request<Incoming>) -> Option<String> {
    let mut values = request.headers().get_all(HOST).iter();
    let host = values.next()?.to_str().ok()?.to_ascii_lowercase();
    if values.next().is_some() {
        return None;
    }
    if let Some(authority) = request.uri().authority()
        && !authority.as_str().eq_ignore_ascii_case(&host)
    {
        return None;
    }
    Some(host)
}

/// `/s/<id>` and nothing else.
fn id_of(path: &str) -> Option<&str> {
    let id = path.strip_prefix("/s/")?;
    (id.len() == ID_LEN
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
    .then_some(id)
}

/// What a `Range` header asks for, within a stream of `total` bytes.
#[derive(Debug, Eq, PartialEq)]
enum Asked {
    Whole,
    /// `start..end`, end exclusive.
    Part(u64, u64),
    Unsatisfiable,
}

/// Only `bytes=a-` and `bytes=a-b` are served: that is all a resuming client
/// needs. Suffix and multiple ranges are refused rather than guessed at.
fn parse_range(header: Option<&HeaderValue>, total: u64) -> Asked {
    let Some(header) = header else {
        return Asked::Whole;
    };
    let Some(spec) = header
        .to_str()
        .ok()
        .and_then(|text| text.strip_prefix("bytes="))
    else {
        return Asked::Unsatisfiable;
    };
    let Some((first, last)) = spec.split_once('-') else {
        return Asked::Unsatisfiable;
    };
    let digits = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u64>().ok())
            .flatten()
    };
    let Some(start) = digits(first) else {
        return Asked::Unsatisfiable;
    };
    let end = if last.is_empty() {
        total
    } else {
        match digits(last) {
            Some(last) if last >= start => last.saturating_add(1).min(total),
            _ => return Asked::Unsatisfiable,
        }
    };
    if start >= total || end <= start {
        return Asked::Unsatisfiable;
    }
    Asked::Part(start, end)
}

pub(super) fn respond(shared: &Shared, request: Request<Incoming>) -> Reply {
    if request.method() != Method::GET {
        let mut response = refuse(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET"));
        return response;
    }
    let Some(id) = id_of(request.uri().path()) else {
        return refuse(StatusCode::NOT_FOUND);
    };
    let Some(host) = host_of(&request) else {
        return refuse(StatusCode::MISDIRECTED_REQUEST);
    };
    match request.uri().query() {
        None => page_for(shared, id, &host),
        Some("d=1") => stream_for(shared, id, &host, request.headers().get(RANGE)),
        Some(_) => refuse(StatusCode::NOT_FOUND),
    }
}

fn page_for(shared: &Shared, id: &str, host: &str) -> Reply {
    if let Err(error) = shared.store.probe(id, host) {
        return from_error(error);
    }
    let html = Bytes::from(shared.page.html.clone());
    let length = html.len();
    let mut response = reply(
        StatusCode::OK,
        Some("text/html; charset=utf-8"),
        &shared.page.policy,
        Body::page(html),
    );
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, value(&length.to_string()));
    response
}

fn stream_for(shared: &Shared, id: &str, host: &str, range: Option<&HeaderValue>) -> Reply {
    let transfer = match shared.store.open(id, host) {
        Ok(transfer) => transfer,
        Err(error) => return from_error(error),
    };
    let total = transfer.cipher_len();
    let (status, start, end) = match parse_range(range, total) {
        Asked::Whole => (StatusCode::OK, 0, total),
        Asked::Part(start, end) => (StatusCode::PARTIAL_CONTENT, start, end),
        Asked::Unsatisfiable => {
            let mut response = refuse(StatusCode::RANGE_NOT_SATISFIABLE);
            response
                .headers_mut()
                .insert(CONTENT_RANGE, value(&format!("bytes */{total}")));
            return response;
        }
    };
    let mut response = reply(
        status,
        Some("application/octet-stream"),
        page::PLAIN_POLICY,
        Body::stream(transfer, start, end),
    );
    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CONTENT_LENGTH, value(&(end - start).to_string()));
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(
            CONTENT_RANGE,
            value(&format!("bytes {start}-{}/{total}", end - 1)),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(text: &str, total: u64) -> Asked {
        parse_range(Some(&HeaderValue::from_str(text).unwrap()), total)
    }

    #[test]
    fn only_open_and_closed_ranges_are_served() {
        assert_eq!(parse_range(None, 100), Asked::Whole);
        assert_eq!(asked("bytes=0-", 100), Asked::Part(0, 100));
        assert_eq!(asked("bytes=22-", 100), Asked::Part(22, 100));
        assert_eq!(asked("bytes=10-19", 100), Asked::Part(10, 20));
        assert_eq!(asked("bytes=10-5000", 100), Asked::Part(10, 100));
        assert_eq!(asked("bytes=99-", 100), Asked::Part(99, 100));
        for refused in [
            "bytes=100-",
            "bytes=-5",
            "bytes=5-4",
            "bytes=0-1,5-6",
            "bytes=a-",
            "bytes=1-b",
            "items=0-",
            "bytes=",
            "bytes=-",
            "0-5",
            "bytes=+1-",
        ] {
            assert_eq!(asked(refused, 100), Asked::Unsatisfiable, "{refused}");
        }
        assert_eq!(asked("bytes=0-", 0), Asked::Unsatisfiable);
    }

    #[test]
    fn only_a_full_length_url_safe_id_is_a_route() {
        let id = "A".repeat(ID_LEN);
        assert_eq!(id_of(&format!("/s/{id}")), Some(id.as_str()));
        for path in [
            "/",
            "/s/",
            "/s",
            "/index.html",
            "/api/ws",
            &format!("/s/{id}/"),
            &format!("/s/{id}x"),
            &format!("/s/{}", &id[1..]),
            &format!("/s/{}.", &id[1..]),
            &format!("/s/..%2f{}", &id[5..]),
        ] {
            assert_eq!(id_of(path), None, "{path}");
        }
    }
}
