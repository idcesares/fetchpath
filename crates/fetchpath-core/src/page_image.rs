//! Bounded inspection of an HTTP(S) page's public Open Graph / Twitter image
//! metadata (and direct `image/*` responses) so photo links such as X `/photo/1`
//! posts can be resolved to their direct image download URL without cookies or
//! site-specific proxies.

use std::time::Duration;

const MAX_HTML_BYTES: usize = 512 * 1024;
const INSPECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageImage {
    pub url: String,
    pub filename: String,
    pub title: Option<String>,
    pub content_type: Option<String>,
}

/// Fetches a bounded slice of `page_url` over HTTP(S) (no cookies, TLS verified)
/// and returns a direct image download target if the response is either an
/// `image/*` resource or an HTML page advertising a public `og:image` /
/// `twitter:image` URL.
pub fn inspect_page_image(page_url: &str) -> Result<Option<PageImage>, String> {
    let mut current = page_url.trim().to_owned();
    if !is_http_url(&current) {
        return Err("Enter an HTTP or HTTPS address.".into());
    }

    for _hop in 0..=5 {
        let response = fetchpath_http::fetch_page_head(&current, MAX_HTML_BYTES, INSPECT_TIMEOUT)
            .map_err(|error| format!("Could not inspect the page: {error}"))?;

        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let Some(loc) = response.location else {
                return Ok(None);
            };
            let Some(next) = resolve_image_url(&current, &loc) else {
                return Ok(None);
            };
            current = next;
            continue;
        }
        if !(200..300).contains(&response.status) {
            return Ok(None);
        }

        if let Some(ref mime_type) = response.content_type
            && mime_type.starts_with("image/")
        {
            let filename = derive_filename(&current, Some(mime_type), None);
            return Ok(Some(PageImage {
                url: current,
                filename,
                title: None,
                content_type: Some(mime_type.clone()),
            }));
        }

        let is_html = response
            .content_type
            .as_deref()
            .is_none_or(|m| m.contains("html") || m.contains("xml"));
        if !is_html {
            return Ok(None);
        }

        let html = String::from_utf8_lossy(&response.body).into_owned();
        return Ok(extract_image_from_html(&current, &html));
    }

    Ok(None)
}

fn is_http_url(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !raw.chars().any(|c| c.is_control() || c.is_whitespace())
}

/// Pure HTML metadata parser, unit-tested offline.
pub fn extract_image_from_html(page_url: &str, html: &str) -> Option<PageImage> {
    let mut og_images: Vec<String> = Vec::new();
    let mut twitter_images: Vec<String> = Vec::new();
    let mut og_image_type: Option<String> = None;
    let mut title: Option<String> = None;

    for tag in iter_meta_tags(html) {
        let key = attr_value(tag, "property")
            .or_else(|| attr_value(tag, "name"))
            .map(|v| v.trim().to_ascii_lowercase());
        let Some(key) = key else { continue };
        let Some(raw_content) = attr_value(tag, "content") else {
            continue;
        };
        let content = decode_html_entities(raw_content.trim());
        if content.is_empty() {
            continue;
        }
        match key.as_str() {
            "og:image:secure_url" | "og:image" => {
                if let Some(resolved) = resolve_image_url(page_url, &content)
                    && !og_images.contains(&resolved)
                {
                    og_images.push(resolved);
                }
            }
            "twitter:image" | "twitter:image:src" => {
                if let Some(resolved) = resolve_image_url(page_url, &content)
                    && !twitter_images.contains(&resolved)
                {
                    twitter_images.push(resolved);
                }
            }
            "og:image:type" if og_image_type.is_none() => {
                let lower = content.to_ascii_lowercase();
                if lower.starts_with("image/") {
                    og_image_type = Some(lower);
                }
            }
            "og:title" | "twitter:title" if title.is_none() => {
                title = Some(content);
            }
            _ => {}
        }
    }

    let candidates: Vec<String> = og_images
        .into_iter()
        .chain(twitter_images)
        .filter(|candidate| !is_placeholder_image(candidate))
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // For URLs ending in `/photo/<N>` (e.g. X multi-photo posts), prefer the
    // Nth image when the page advertises multiple og:image entries.
    let chosen_index = photo_index_hint(page_url)
        .and_then(|n| n.checked_sub(1))
        .filter(|&idx| idx < candidates.len())
        .unwrap_or(0);
    let image_url = candidates[chosen_index].clone();
    let filename = derive_filename(&image_url, og_image_type.as_deref(), title.as_deref());
    Some(PageImage {
        url: image_url,
        filename,
        title,
        content_type: og_image_type,
    })
}

fn iter_meta_tags(html: &str) -> impl Iterator<Item = &str> {
    let bytes = html.as_bytes();
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        while pos + 5 < bytes.len() {
            let rel = html[pos..].find('<')?;
            let start = pos + rel;
            let rest = &html[start..];
            if rest.len() >= 5
                && rest[1..5].eq_ignore_ascii_case("meta")
                && rest
                    .as_bytes()
                    .get(5)
                    .is_some_and(|b| b.is_ascii_whitespace())
            {
                let end = find_tag_end(rest)?;
                pos = start + end + 1;
                return Some(&rest[..=end]);
            }
            pos = start + 1;
        }
        None
    })
}

fn find_tag_end(tag_start: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (idx, &b) in tag_start.as_bytes().iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(idx),
            None => {}
        }
    }
    None
}

fn attr_value<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let bytes = tag.as_bytes();
    let mut i = 5; // skip `<meta`
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' {
            break;
        }
        let key_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'>'
        {
            i += 1;
        }
        let key = &tag[key_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let (val_start, val_end) = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'')
            {
                let q = bytes[i];
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                let end = i;
                if i < bytes.len() {
                    i += 1;
                }
                (start, end)
            } else {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                (start, i)
            };
            if key.eq_ignore_ascii_case(name) {
                return Some(&tag[val_start..val_end]);
            }
        }
    }
    None
}

fn decode_html_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';')
            && semi <= 12
        {
            let entity = &after[..semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" | "#39" | "#x27" | "#X27" => Some('\''),
                "lt" => Some('<'),
                "gt" => Some('>'),
                s if s.starts_with("#x") || s.starts_with("#X") => u32::from_str_radix(&s[2..], 16)
                    .ok()
                    .and_then(char::from_u32),
                s if s.starts_with('#') => s[1..].parse::<u32>().ok().and_then(char::from_u32),
                _ => None,
            };
            if let Some(ch) = decoded {
                out.push(ch);
                rest = &after[semi + 1..];
                continue;
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn resolve_image_url(page_url: &str, raw: &str) -> Option<String> {
    let candidate = raw.trim();
    if is_http_url(candidate) {
        return Some(candidate.to_owned());
    }
    if candidate.starts_with("//") {
        let scheme = page_url.split("://").next()?;
        let combined = format!("{scheme}:{candidate}");
        return is_http_url(&combined).then_some(combined);
    }
    if candidate.starts_with('/') {
        let scheme_end = page_url.find("://")?;
        let after_scheme = &page_url[scheme_end + 3..];
        let host_end = after_scheme
            .find(['/', '?', '#'])
            .unwrap_or(after_scheme.len());
        let origin = &page_url[..scheme_end + 3 + host_end];
        let combined = format!("{origin}{candidate}");
        return is_http_url(&combined).then_some(combined);
    }
    None
}

/// Filters out known site-wide fallback logos (for instance X's default SSR
/// logo when a post has no media or is behind a login wall).
fn is_placeholder_image(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("://abs.twimg.com/") || lower.contains("/profile_images/")
}

fn photo_index_hint(page_url: &str) -> Option<usize> {
    let path = page_url.split(['?', '#']).next().unwrap_or(page_url);
    let mut parts = path.trim_end_matches('/').rsplit('/');
    let last = parts.next()?;
    let prev = parts.next()?;
    if prev.eq_ignore_ascii_case("photo") {
        last.parse::<usize>().ok().filter(|&n| n >= 1)
    } else {
        None
    }
}

fn derive_filename(image_url: &str, content_type: Option<&str>, title: Option<&str>) -> String {
    let ext = extension_from_query(image_url)
        .or_else(|| extension_from_path(image_url))
        .or_else(|| content_type.and_then(extension_from_mime))
        .unwrap_or("jpg");

    let path = image_url.split(['?', '#']).next().unwrap_or(image_url);
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let raw_stem = last
        .rsplit_once('.')
        .filter(|(_, e)| is_image_ext(e))
        .map_or(last, |(s, _)| s);

    let stem = if !raw_stem.is_empty()
        && raw_stem.len() >= 3
        && !matches!(
            raw_stem.to_ascii_lowercase().as_str(),
            "image" | "photo" | "media" | "index" | "default" | "large" | "original"
        ) {
        sanitize_stem(raw_stem)
    } else {
        title
            .map(sanitize_stem)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "photo".into())
    };
    let stem = if stem.is_empty() {
        "photo".into()
    } else {
        stem
    };
    format!("{stem}.{ext}")
}

fn extension_from_query(url: &str) -> Option<&'static str> {
    let query = url.split_once('?')?.1.split('#').next().unwrap_or("");
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k.eq_ignore_ascii_case("format")
        {
            return normalize_image_ext(v);
        }
    }
    None
}

fn extension_from_path(url: &str) -> Option<&'static str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next()?;
    let (_, ext) = last.rsplit_once('.')?;
    normalize_image_ext(ext)
}

fn extension_from_mime(mime: &str) -> Option<&'static str> {
    match mime.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/avif" => Some("avif"),
        "image/bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        _ => None,
    }
}

fn is_image_ext(ext: &str) -> bool {
    normalize_image_ext(ext).is_some()
}

fn normalize_image_ext(ext: &str) -> Option<&'static str> {
    match ext.trim().to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("jpg"),
        "png" => Some("png"),
        "webp" => Some("webp"),
        "gif" => Some("gif"),
        "avif" => Some("avif"),
        "bmp" => Some("bmp"),
        "svg" => Some("svg"),
        _ => None,
    }
}

fn sanitize_stem(raw: &str) -> String {
    let mut cleaned = String::new();
    for ch in raw.chars() {
        if cleaned.len() >= 80 {
            break;
        }
        let unsafe_char = ch.is_control()
            || matches!(
                ch,
                '<' | '>'
                    | ':'
                    | '"'
                    | '/'
                    | '\\'
                    | '|'
                    | '?'
                    | '*'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{2028}'..='\u{202E}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{FEFF}'
            );
        if unsafe_char {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }
    let trimmed = cleaned.trim().trim_matches(['.', ' ']);
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return String::new();
    }
    let head = trimmed
        .split('.')
        .next()
        .unwrap_or(trimmed)
        .to_ascii_uppercase();
    let reserved = matches!(
        head.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CONIN$"
            | "CONOUT$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_x_photo_og_image_with_format_query_extension() {
        let html = r#"<!doctype html><html><head>
            <meta property="og:title" content="user on X"/>
            <meta property="og:image" content="https://pbs.twimg.com/media/HX7d-WAWwAArhPZ?format=webp&amp;name=large"/>
            <meta property="og:image:type" content="image/webp"/>
        </head><body></body></html>"#;
        let found = extract_image_from_html("https://x.com/user/status/12345/photo/1", html)
            .expect("should find photo");
        assert_eq!(
            found.url,
            "https://pbs.twimg.com/media/HX7d-WAWwAArhPZ?format=webp&name=large"
        );
        assert_eq!(found.filename, "HX7d-WAWwAArhPZ.webp");
        assert_eq!(found.content_type.as_deref(), Some("image/webp"));
    }

    #[test]
    fn picks_indexed_photo_and_skips_placeholder_or_non_http_urls() {
        let html = r#"<html><head>
            <meta property="og:image" content="javascript:alert(1)"/>
            <meta property="og:image" content="https://abs.twimg.com/rweb/ssr/default/v2/og/image.png"/>
            <meta property="og:image" content="https://pbs.twimg.com/media/FIRST_ID?format=jpg&amp;name=large"/>
            <meta property="og:image" content="https://pbs.twimg.com/media/SECOND_ID?format=png&amp;name=large"/>
        </head></html>"#;
        let second = extract_image_from_html("https://x.com/user/status/123/photo/2", html)
            .expect("should select second photo");
        assert_eq!(
            second.url,
            "https://pbs.twimg.com/media/SECOND_ID?format=png&name=large"
        );
        assert_eq!(second.filename, "SECOND_ID.png");

        let only_placeholder = r#"<html><head>
            <meta property="og:image" content="https://abs.twimg.com/rweb/ssr/default/v2/og/image.png"/>
        </head></html>"#;
        assert!(
            extract_image_from_html("https://x.com/user/status/999", only_placeholder).is_none()
        );
    }

    #[test]
    fn sanitizes_reserved_stems_and_unsafe_characters() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://images.example.test/con.png"/>
        </head></html>"#;
        let found = extract_image_from_html("https://example.test/post", html).unwrap();
        assert_eq!(found.filename, "_con.png");
    }
}
