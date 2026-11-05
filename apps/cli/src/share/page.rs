//! The one HTML page a link shows, with a policy that allows exactly its own
//! script and style (FP-117). The page holds nothing about the file: the name
//! and the key come in the encrypted stream and the URL fragment.

use sha2::{Digest, Sha256};

const TEMPLATE: &str = include_str!("page.html");
const STYLE: &str = include_str!("page.css");
const SCRIPT: &str = include_str!("page.cjs");
const ENDED: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<meta name=\"robots\" content=\"noindex\"><title>Fetchpath</title></head><body>\
<h1>Fetchpath</h1><p>This link has already been used, has expired or was withdrawn. \
Ask the sender for a new one.</p></body></html>";

pub(super) struct Page {
    pub(super) html: String,
    /// Allows the page's script and style by hash and nothing else.
    pub(super) policy: String,
}

impl Page {
    pub(super) fn build() -> Self {
        let html = TEMPLATE
            .replace("<!--STYLE-->", STYLE)
            .replace("<!--SCRIPT-->", SCRIPT);
        let policy = format!(
            "default-src 'none'; script-src 'sha256-{}'; style-src 'sha256-{}'; \
             connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
            base64(&Sha256::digest(SCRIPT.as_bytes())),
            base64(&Sha256::digest(STYLE.as_bytes())),
        );
        Self { html, policy }
    }
}

/// The page of a link that cannot be used any more: no script, no policy
/// exceptions.
pub(super) const fn ended() -> &'static str {
    ENDED
}

/// The policy for pages without any script or style of their own.
pub(super) const PLAIN_POLICY: &str =
    "default-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for group in bytes.chunks(3) {
        let n = group
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, byte)| n | (u32::from(*byte) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= group.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_with_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_page_names_nothing_about_a_file() {
        let page = Page::build();
        assert!(!page.html.contains("<!--"));
        assert!(page.policy.contains("script-src 'sha256-"));
        assert!(!page.policy.contains("unsafe-inline"));
    }

    #[test]
    fn the_policy_allows_the_scripts_and_styles_the_page_really_has() {
        let page = Page::build();
        let inner = |open: &str, close: &str| {
            let from = page.html.find(open).unwrap() + open.len();
            let to = page.html[from..].find(close).unwrap() + from;
            page.html[from..to].to_owned()
        };
        for (what, body) in [
            ("script-src", inner("<script>", "</script>")),
            ("style-src", inner("<style>", "</style>")),
        ] {
            let allowed = format!(
                "{what} 'sha256-{}'",
                base64(&Sha256::digest(body.as_bytes()))
            );
            assert!(
                page.policy.contains(&allowed),
                "{allowed} in {}",
                page.policy
            );
        }
        assert_eq!(page.html.matches("<script").count(), 1);
    }
}
