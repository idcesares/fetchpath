//! What the signed-in browser is served (FP-104): the web bundle that
//! `apps/desktop` builds (`pnpm build:web`), embedded by build.rs. Without the
//! bundle one fixed page stands in.

const HTML: &str = "text/html; charset=utf-8";

const NOT_BUILT: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>Fetchpath</title></head><body><h1>Fetchpath</h1>\
<p>The web UI is not built into this version of Fetchpath.</p></body></html>";

/// Every embedded file as (path, bytes, content type), sorted by path. Empty
/// when the bundle was not built.
static FILES: &[(&str, &[u8], &str)] = include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// The body and content type served at `path`, or `None` for a 404.
pub(super) fn lookup(path: &str) -> Option<(&'static [u8], &'static str)> {
    find(FILES, path)
}

fn find(
    files: &'static [(&'static str, &'static [u8], &'static str)],
    path: &str,
) -> Option<(&'static [u8], &'static str)> {
    if files.is_empty() {
        return (path == "/").then_some((NOT_BUILT.as_bytes(), HTML));
    }
    let path = if path == "/" { "/index.html" } else { path };
    files
        .binary_search_by(|(name, _, _)| (*name).cmp(path))
        .ok()
        .map(|index| (files[index].1, files[index].2))
}

#[cfg(test)]
mod tests {
    use super::*;

    static BUNDLE: &[(&str, &[u8], &str)] = &[
        ("/assets/app.js", b"js", "text/javascript"),
        ("/index.html", b"page", HTML),
    ];

    #[test]
    fn root_is_the_index_and_unknown_paths_are_absent() {
        assert_eq!(find(BUNDLE, "/"), Some((&b"page"[..], HTML)));
        assert_eq!(
            find(BUNDLE, "/assets/app.js").map(|found| found.0),
            Some(&b"js"[..])
        );
        assert!(find(BUNDLE, "/missing").is_none());
    }

    #[test]
    fn without_a_bundle_only_the_root_shows_the_placeholder() {
        assert_eq!(find(&[], "/").map(|found| found.1), Some(HTML));
        assert!(find(&[], "/index.html").is_none());
    }
}
