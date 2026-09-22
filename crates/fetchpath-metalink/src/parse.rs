//! The Metalink 4 subset this project needs: file name, size, mirror `<url>`
//! entries with `priority`/`location`, whole-file `<hash type=...>`, and a
//! `<pieces length=N type=sha-256>` map of ordered `<hash>` children.
//!
//! Anything outside that subset is ignored, except for inputs that would make a
//! dishonest result possible: an unsafe file name, a digest that is not
//! hexadecimal, a piece map whose count cannot describe the declared size, and
//! a piece digest type this crate cannot verify are all typed errors.

use crate::pieces::PieceMap;
use crate::xml::{Event, read_events};
use crate::{MetalinkError, ParseLimits, normalize_hex};

/// The highest `priority` RFC 5854 allows. Lower numbers are preferred.
const MAX_PRIORITY: u32 = 999_999;
const MAX_URL_BYTES: usize = 4096;
const MAX_LOCATION_BYTES: usize = 64;
const MAX_FILE_NAME_BYTES: usize = 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metalink {
    pub files: Vec<MetalinkFile>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetalinkFile {
    /// A validated relative path. Never absolute, never a drive-qualified
    /// path, never containing `..`, and never containing control characters.
    pub name: String,
    pub size: Option<u64>,
    /// Mirrors in document order. `priority` is advisory and lower is better.
    pub urls: Vec<MetalinkUrl>,
    /// Whole-file digests exactly as declared, with lowercase hex values.
    pub hashes: Vec<FileHash>,
    /// A consistent map of trusted SHA-256 piece digests, when the document
    /// carries one.
    pub pieces: Option<PieceMap>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetalinkUrl {
    pub url: String,
    pub priority: Option<u32>,
    pub location: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileHash {
    /// The digest type as declared, lowercased (for example `sha-256`).
    pub hash_type: String,
    pub value: String,
}

impl MetalinkFile {
    /// The declared whole-file SHA-256, if any. This is metadata-author
    /// supplied; matching it is not publisher authenticity.
    pub fn expected_sha256(&self) -> Option<&str> {
        self.hashes
            .iter()
            .find(|hash| is_sha256_type(&hash.hash_type))
            .map(|hash| hash.value.as_str())
    }

    /// Mirrors ordered by advisory priority, then document order.
    pub fn mirrors_by_priority(&self) -> Vec<&MetalinkUrl> {
        let mut ordered: Vec<&MetalinkUrl> = self.urls.iter().collect();
        ordered.sort_by_key(|url| url.priority.unwrap_or(MAX_PRIORITY));
        ordered
    }
}

fn is_sha256_type(value: &str) -> bool {
    matches!(value, "sha-256" | "sha256")
}

/// Parses a Metalink 4 document under the default bounds.
pub fn parse_metalink(input: &[u8]) -> Result<Metalink, MetalinkError> {
    parse_with_limits(input, &ParseLimits::default())
}

/// Parses a Metalink 4 document under explicit bounds.
pub fn parse_with_limits(input: &[u8], limits: &ParseLimits) -> Result<Metalink, MetalinkError> {
    let events = read_events(input, limits)?;
    let mut path: Vec<&str> = Vec::new();
    let mut files: Vec<MetalinkFile> = Vec::new();
    let mut current: Option<Builder> = None;
    let mut text = String::new();

    for event in &events {
        match event {
            Event::Text(value) => text.push_str(value),
            Event::Start { name, attributes } => {
                text.clear();
                let local = local_name(name);
                if path.is_empty() {
                    if local != "metalink" {
                        return Err(MetalinkError::Malformed {
                            detail: "the root element is not <metalink>",
                        });
                    }
                } else if path.as_slice() == ["metalink"] && local == "file" {
                    if files.len() == limits.max_files {
                        return Err(MetalinkError::TooManyNodes {
                            kind: "files",
                            limit: limits.max_files,
                        });
                    }
                    current = Some(Builder::begin(attributes)?);
                } else if let Some(builder) = current.as_mut() {
                    builder.start(&path, local, attributes, limits)?;
                }
                path.push(local);
            }
            Event::End { name } => {
                let local = local_name(name);
                path.pop();
                if path.as_slice() == ["metalink"] && local == "file" {
                    if let Some(builder) = current.take() {
                        files.push(builder.finish(limits)?);
                    }
                } else if let Some(builder) = current.as_mut() {
                    builder.end(&path, local, text.trim())?;
                }
                text.clear();
            }
        }
    }
    Ok(Metalink { files })
}

fn local_name(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn attribute<'a>(attributes: &'a [(String, String)], wanted: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(name, _)| local_name(name) == wanted)
        .map(|(_, value)| value.as_str())
}

struct Builder {
    name: String,
    size: Option<u64>,
    urls: Vec<MetalinkUrl>,
    hashes: Vec<FileHash>,
    piece_length: Option<u64>,
    piece_hashes: Vec<[u8; 32]>,
    in_pieces: bool,
    seen_pieces: bool,
    pending_url: Option<MetalinkUrl>,
    pending_hash_type: Option<String>,
}

impl Builder {
    fn begin(attributes: &[(String, String)]) -> Result<Self, MetalinkError> {
        let name = attribute(attributes, "name").ok_or(MetalinkError::Malformed {
            detail: "<file> has no name attribute",
        })?;
        Ok(Self {
            name: validate_file_name(name)?,
            size: None,
            urls: Vec::new(),
            hashes: Vec::new(),
            piece_length: None,
            piece_hashes: Vec::new(),
            in_pieces: false,
            seen_pieces: false,
            pending_url: None,
            pending_hash_type: None,
        })
    }

    fn start(
        &mut self,
        path: &[&str],
        local: &str,
        attributes: &[(String, String)],
        limits: &ParseLimits,
    ) -> Result<(), MetalinkError> {
        let inside_file = path.last() == Some(&"file");
        let inside_pieces = path.last() == Some(&"pieces");
        match (local, inside_file, inside_pieces) {
            ("url", true, _) => {
                if self.urls.len() == limits.max_urls_per_file {
                    return Err(MetalinkError::TooManyNodes {
                        kind: "mirror urls on one file",
                        limit: limits.max_urls_per_file,
                    });
                }
                self.pending_url = Some(MetalinkUrl {
                    url: String::new(),
                    priority: parse_priority(attribute(attributes, "priority"))?,
                    location: parse_location(attribute(attributes, "location"))?,
                });
            }
            ("hash", true, _) => {
                if self.hashes.len() == limits.max_hashes_per_file {
                    return Err(MetalinkError::TooManyNodes {
                        kind: "whole-file hashes on one file",
                        limit: limits.max_hashes_per_file,
                    });
                }
                self.pending_hash_type = Some(
                    attribute(attributes, "type")
                        .unwrap_or_default()
                        .trim()
                        .to_ascii_lowercase(),
                );
            }
            ("pieces", true, _) => {
                if self.seen_pieces {
                    return Err(MetalinkError::Malformed {
                        detail: "a file carries more than one <pieces> map",
                    });
                }
                self.seen_pieces = true;
                self.in_pieces = true;
                let declared = attribute(attributes, "type")
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase();
                if !is_sha256_type(&declared) {
                    return Err(MetalinkError::UnsupportedPieceHash { declared });
                }
                let length = attribute(attributes, "length").ok_or(MetalinkError::Malformed {
                    detail: "<pieces> has no length attribute",
                })?;
                self.piece_length =
                    Some(
                        length
                            .trim()
                            .parse()
                            .map_err(|_| MetalinkError::Malformed {
                                detail: "<pieces length> is not a byte count",
                            })?,
                    );
            }
            ("hash", _, true) if self.piece_hashes.len() == limits.max_pieces => {
                return Err(MetalinkError::TooManyNodes {
                    kind: "piece hashes",
                    limit: limits.max_pieces,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn end(&mut self, path: &[&str], local: &str, text: &str) -> Result<(), MetalinkError> {
        let inside_file = path.last() == Some(&"file");
        match (local, inside_file, self.in_pieces) {
            ("size", true, _) => {
                self.size = Some(text.parse().map_err(|_| MetalinkError::Malformed {
                    detail: "<size> is not a byte count",
                })?);
            }
            ("url", true, _) => {
                if let Some(mut url) = self.pending_url.take() {
                    url.url = validate_url(text)?;
                    self.urls.push(url);
                }
            }
            ("hash", true, false) => {
                if let Some(hash_type) = self.pending_hash_type.take() {
                    let nibbles = is_sha256_type(&hash_type).then_some(64);
                    self.hashes.push(FileHash {
                        value: normalize_hex(text, nibbles)?,
                        hash_type,
                    });
                }
            }
            ("hash", _, true) => {
                let normalized = normalize_hex(text, Some(64))?;
                let mut digest = [0_u8; 32];
                for (index, byte) in digest.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&normalized[index * 2..index * 2 + 2], 16).map_err(
                        |_| MetalinkError::InvalidHash {
                            detail: "a piece digest is not hexadecimal",
                        },
                    )?;
                }
                self.piece_hashes.push(digest);
            }
            ("pieces", true, _) => self.in_pieces = false,
            _ => {}
        }
        Ok(())
    }

    fn finish(self, limits: &ParseLimits) -> Result<MetalinkFile, MetalinkError> {
        let pieces = match self.piece_length {
            None => None,
            Some(piece_length) => {
                // Without a declared size there is nothing to check the piece
                // count against, so the map is refused rather than trusted.
                let size = self.size.ok_or(MetalinkError::InconsistentPieces {
                    declared_size: None,
                    piece_length,
                    pieces: self.piece_hashes.len(),
                })?;
                Some(PieceMap::new(
                    piece_length,
                    size,
                    self.piece_hashes,
                    limits,
                )?)
            }
        };
        Ok(MetalinkFile {
            name: self.name,
            size: self.size,
            urls: self.urls,
            hashes: self.hashes,
            pieces,
        })
    }
}

fn parse_priority(value: Option<&str>) -> Result<Option<u32>, MetalinkError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let priority: u32 = value.trim().parse().map_err(|_| MetalinkError::Malformed {
        detail: "<url priority> is not a number",
    })?;
    if priority == 0 || priority > MAX_PRIORITY {
        return Err(MetalinkError::Malformed {
            detail: "<url priority> is outside 1..=999999",
        });
    }
    Ok(Some(priority))
}

fn parse_location(value: Option<&str>) -> Result<Option<String>, MetalinkError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_LOCATION_BYTES {
        return Err(MetalinkError::Malformed {
            detail: "<url location> length is out of range",
        });
    }
    Ok(Some(trimmed.to_ascii_lowercase()))
}

fn validate_url(value: &str) -> Result<String, MetalinkError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_URL_BYTES {
        return Err(MetalinkError::Malformed {
            detail: "<url> length is out of range",
        });
    }
    if trimmed
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(MetalinkError::Malformed {
            detail: "<url> contains whitespace or a control character",
        });
    }
    if !trimmed.contains("://") {
        return Err(MetalinkError::Malformed {
            detail: "<url> has no scheme",
        });
    }
    Ok(trimmed.to_owned())
}

/// Validates the one field that decides where bytes land on disk.
fn validate_file_name(value: &str) -> Result<String, MetalinkError> {
    let name = value.trim();
    if name.is_empty() || name.len() > MAX_FILE_NAME_BYTES {
        return Err(MetalinkError::UnsafeFileName {
            detail: "the name is empty or too long",
        });
    }
    if name.chars().any(|character| character.is_control()) {
        return Err(MetalinkError::UnsafeFileName {
            detail: "the name contains a control character",
        });
    }
    if name.contains('\\') {
        return Err(MetalinkError::UnsafeFileName {
            detail: "the name contains a backslash",
        });
    }
    if name.starts_with('/') {
        return Err(MetalinkError::UnsafeFileName {
            detail: "the name is an absolute path",
        });
    }
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(MetalinkError::UnsafeFileName {
            detail: "the name is drive qualified",
        });
    }
    for component in name.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(MetalinkError::UnsafeFileName {
                detail: "the name contains an empty or traversing path component",
            });
        }
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn document(body: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <metalink xmlns=\"urn:ietf:params:xml:ns:metalink\">{body}</metalink>"
        )
    }

    fn parse(body: &str) -> Result<Metalink, MetalinkError> {
        parse_metalink(document(body).as_bytes())
    }

    fn hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn parses_the_supported_subset() {
        let body: Vec<u8> = (0..2500_u32).map(|index| (index % 251) as u8).collect();
        let pieces: String = body
            .chunks(1024)
            .map(|chunk| format!("<hash>{}</hash>", hex(chunk)))
            .collect();
        let parsed = parse(&format!(
            "<file name=\"nested/dir/payload.bin\">\
               <size>2500</size>\
               <hash type=\"sha-256\">{}</hash>\
               <hash type=\"md5\">d41d8cd98f00b204e9800998ecf8427e</hash>\
               <url priority=\"2\" location=\"DE\">http://b.test/payload.bin</url>\
               <url priority=\"1\">http://a.test/payload.bin</url>\
               <url>ftp://c.test/payload.bin</url>\
               <pieces length=\"1024\" type=\"sha-256\">{pieces}</pieces>\
             </file>",
            hex(&body)
        ))
        .unwrap();

        assert_eq!(parsed.files.len(), 1);
        let file = &parsed.files[0];
        assert_eq!(file.name, "nested/dir/payload.bin");
        assert_eq!(file.size, Some(2500));
        assert_eq!(file.expected_sha256(), Some(hex(&body).as_str()));
        assert_eq!(file.hashes.len(), 2);
        assert_eq!(file.urls.len(), 3);
        assert_eq!(file.urls[0].priority, Some(2));
        assert_eq!(file.urls[0].location.as_deref(), Some("de"));
        assert_eq!(file.urls[2].priority, None);
        assert_eq!(
            file.mirrors_by_priority()
                .iter()
                .map(|url| url.url.as_str())
                .collect::<Vec<_>>(),
            [
                "http://a.test/payload.bin",
                "http://b.test/payload.bin",
                "ftp://c.test/payload.bin"
            ]
        );
        let map = file.pieces.as_ref().unwrap();
        assert_eq!(map.piece_count(), 3);
        assert_eq!(map.piece_length(), 1024);
        assert!(map.verify_piece(0, &body[..1024]));
    }

    #[test]
    fn parses_a_file_without_pieces_or_hashes() {
        let parsed = parse("<file name=\"a.bin\"><url>http://a.test/a.bin</url></file>").unwrap();
        let file = &parsed.files[0];
        assert!(file.pieces.is_none());
        assert_eq!(file.expected_sha256(), None);
        assert_eq!(file.size, None);
    }

    #[test]
    fn handles_prefixed_namespaces_and_multiple_files() {
        let parsed = parse_metalink(
            b"<ml:metalink xmlns:ml=\"urn:ietf:params:xml:ns:metalink\">\
                <ml:file ml:name=\"a.bin\"><ml:url>http://a.test/a</ml:url></ml:file>\
                <ml:file ml:name=\"b.bin\"><ml:url>http://b.test/b</ml:url></ml:file>\
              </ml:metalink>",
        )
        .unwrap();
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[1].name, "b.bin");
    }

    #[test]
    fn rejects_unsafe_file_names() {
        for name in [
            "/etc/passwd",
            "C:\\\\Windows\\\\system32",
            "c:relative",
            "../escape.bin",
            "nested/../escape.bin",
            "nested/./x.bin",
            "windows\\\\path.bin",
            "",
            "   ",
            "a//b.bin",
        ] {
            let parsed = parse(&format!("<file name=\"{name}\"/>"));
            assert!(
                matches!(parsed, Err(MetalinkError::UnsafeFileName { .. })),
                "expected {name:?} to be refused, got {parsed:?}"
            );
        }
        assert!(
            parse("<file name=\"a&#0;b.bin\"/>").is_err(),
            "a NUL in a file name must be refused"
        );
    }

    #[test]
    fn rejects_an_inconsistent_piece_map() {
        let body = "<file name=\"a.bin\"><size>2500</size>\
                    <pieces length=\"1024\" type=\"sha-256\">\
                      <hash>{h}</hash><hash>{h}</hash>\
                    </pieces></file>"
            .replace("{h}", &hex(b"x"));
        assert!(matches!(
            parse(&body),
            Err(MetalinkError::InconsistentPieces { .. })
        ));
    }

    #[test]
    fn rejects_a_piece_map_without_a_declared_size() {
        let body = format!(
            "<file name=\"a.bin\"><pieces length=\"1024\" type=\"sha-256\">\
               <hash>{}</hash></pieces></file>",
            hex(b"x")
        );
        assert!(matches!(
            parse(&body),
            Err(MetalinkError::InconsistentPieces {
                declared_size: None,
                ..
            })
        ));
    }

    #[test]
    fn refuses_an_unverifiable_piece_digest_type_instead_of_downgrading() {
        let body = "<file name=\"a.bin\"><size>10</size>\
                    <pieces length=\"10\" type=\"sha-1\"><hash>aa</hash></pieces></file>";
        assert_eq!(
            parse(body),
            Err(MetalinkError::UnsupportedPieceHash {
                declared: "sha-1".into()
            })
        );
    }

    #[test]
    fn rejects_bad_hashes_priorities_and_urls() {
        assert!(matches!(
            parse("<file name=\"a.bin\"><hash type=\"sha-256\">nothex</hash></file>"),
            Err(MetalinkError::InvalidHash { .. })
        ));
        assert!(matches!(
            parse("<file name=\"a.bin\"><hash type=\"sha-256\">abcd</hash></file>"),
            Err(MetalinkError::InvalidHash { .. })
        ));
        assert!(matches!(
            parse("<file name=\"a.bin\"><url priority=\"0\">http://a.test/a</url></file>"),
            Err(MetalinkError::Malformed { .. })
        ));
        assert!(matches!(
            parse("<file name=\"a.bin\"><url priority=\"1000000\">http://a.test/a</url></file>"),
            Err(MetalinkError::Malformed { .. })
        ));
        assert!(matches!(
            parse("<file name=\"a.bin\"><url>not-a-url</url></file>"),
            Err(MetalinkError::Malformed { .. })
        ));
        assert!(matches!(
            parse("<file name=\"a.bin\"><size>nine</size></file>"),
            Err(MetalinkError::Malformed { .. })
        ));
        assert!(matches!(
            parse("<file/>"),
            Err(MetalinkError::Malformed {
                detail: "<file> has no name attribute"
            })
        ));
    }

    #[test]
    fn rejects_a_non_metalink_root() {
        assert!(matches!(
            parse_metalink(b"<rss><file name=\"a\"/></rss>"),
            Err(MetalinkError::Malformed {
                detail: "the root element is not <metalink>"
            })
        ));
    }

    #[test]
    fn refuses_hostile_documents_without_panicking() {
        let billion_laughs = b"<?xml version=\"1.0\"?>\
            <!DOCTYPE metalink [\
              <!ENTITY lol \"lol\">\
              <!ENTITY lol2 \"&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;\">\
              <!ENTITY lol3 \"&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;\">\
            ]>\
            <metalink><file name=\"&lol3;\"/></metalink>";
        assert_eq!(
            parse_metalink(billion_laughs),
            Err(MetalinkError::DoctypeForbidden)
        );

        let external = b"<?xml version=\"1.0\"?>\
            <!DOCTYPE metalink [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]>\
            <metalink><file name=\"&xxe;\"/></metalink>";
        assert_eq!(
            parse_metalink(external),
            Err(MetalinkError::DoctypeForbidden)
        );

        assert_eq!(
            parse_metalink(b"<metalink><file name=\"&xxe;\"/></metalink>"),
            Err(MetalinkError::EntityReferenceForbidden {
                reference: "xxe".into()
            })
        );
    }

    #[test]
    fn refuses_an_oversized_document() {
        let limits = ParseLimits {
            max_bytes: 64,
            ..ParseLimits::default()
        };
        let oversized = document("<file name=\"a.bin\"><url>http://a.test/a</url></file>");
        assert!(matches!(
            parse_with_limits(oversized.as_bytes(), &limits),
            Err(MetalinkError::DocumentTooLarge { .. })
        ));
    }

    #[test]
    fn refuses_more_files_urls_and_hashes_than_the_budget() {
        let limits = ParseLimits {
            max_files: 1,
            max_urls_per_file: 1,
            max_hashes_per_file: 1,
            ..ParseLimits::default()
        };
        let two_files = document("<file name=\"a\"/><file name=\"b\"/>");
        assert_eq!(
            parse_with_limits(two_files.as_bytes(), &limits),
            Err(MetalinkError::TooManyNodes {
                kind: "files",
                limit: 1
            })
        );
        let two_urls = document(
            "<file name=\"a\"><url>http://a.test/a</url><url>http://b.test/b</url></file>",
        );
        assert_eq!(
            parse_with_limits(two_urls.as_bytes(), &limits),
            Err(MetalinkError::TooManyNodes {
                kind: "mirror urls on one file",
                limit: 1
            })
        );
    }
}
