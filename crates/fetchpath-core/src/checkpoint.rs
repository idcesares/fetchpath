use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResponseHeaders {
    pub status: Option<u32>,
    pub etag: Option<String>,
    pub content_length: Option<u64>,
    pub content_range: Option<ContentRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: Option<u64>,
}

impl ResponseHeaders {
    pub fn ingest(&mut self, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            return;
        };
        let line = line.trim();
        if line.starts_with("HTTP/") {
            *self = Self::default();
            self.status = line
                .split_ascii_whitespace()
                .nth(1)
                .and_then(|value| value.parse().ok());
            return;
        }
        let Some((name, value)) = line.split_once(':') else {
            return;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("etag") {
            self.etag = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            self.content_length = value.parse().ok();
        } else if name.eq_ignore_ascii_case("content-range") {
            self.content_range = parse_content_range(value);
        }
    }
}

#[cfg(test)]
pub(crate) fn source_key(url: &str) -> String {
    source_key_with_context(url, "")
}

pub(crate) fn source_key_with_context(url: &str, context_fingerprint: &str) -> String {
    // Query values can contain signed URLs or credentials. They never enter
    // persistent checkpoint identity; the strong response validator is the
    // authority for reusing retained bytes.
    let redacted = url.split(['?', '#']).next().unwrap_or(url);
    let mut hasher = Sha256::new();
    hasher.update(redacted.as_bytes());
    hasher.update([0]);
    hasher.update(context_fingerprint.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    digest[..32].to_owned()
}

pub(crate) fn strong_etag(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.starts_with("W/") || value.starts_with("w/") {
        return None;
    }
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        Some(value.to_owned())
    } else {
        None
    }
}

pub(crate) fn resume_headers_match(
    headers: &ResponseHeaders,
    offset: u64,
    expected_etag: &str,
) -> bool {
    headers.status == Some(206)
        && headers.etag.as_deref() == Some(expected_etag)
        && headers
            .content_range
            .is_some_and(|range| range.start == offset && range.end >= range.start)
}

fn parse_content_range(value: &str) -> Option<ContentRange> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse().ok()?;
    let end = end.parse().ok()?;
    if end < start {
        return None;
    }
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    if total.is_some_and(|total| end >= total) {
        return None;
    }
    Some(ContentRange { start, end, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_strong_quoted_etags_authorize_resume() {
        assert_eq!(strong_etag(Some("\"v1\"")), Some("\"v1\"".into()));
        assert_eq!(strong_etag(Some("W/\"v1\"")), None);
        assert_eq!(strong_etag(Some("v1")), None);
        assert_eq!(strong_etag(None), None);
    }

    #[test]
    fn parses_satisfiable_content_ranges() {
        assert_eq!(
            parse_content_range("bytes 10-19/100"),
            Some(ContentRange {
                start: 10,
                end: 19,
                total: Some(100)
            })
        );
        assert_eq!(parse_content_range("bytes 10-9/100"), None);
        assert_eq!(parse_content_range("bytes 10-100/100"), None);
    }

    #[test]
    fn persistent_source_keys_exclude_query_secrets() {
        assert_eq!(
            source_key("https://example.test/file?token=secret-one"),
            source_key("https://example.test/file?token=secret-two")
        );
        assert_ne!(
            source_key("https://example.test/file"),
            source_key("https://example.test/other")
        );
        assert_ne!(
            source_key_with_context("https://example.test/file", "context-a"),
            source_key_with_context("https://example.test/file", "context-b")
        );
    }
}
