//! Metalink 4 ([RFC 5854]) metadata and trusted piece verification.
//!
//! The document reader is a small, hand-written, bounded XML reader. It refuses
//! DTDs and entity declarations outright, so no entity expansion or external
//! entity resolution is possible, and it caps input size, nesting depth,
//! element count, attribute count, and text length. Every malformed or hostile
//! input returns a typed [`MetalinkError`] instead of panicking.
//!
//! Honesty contract: a digest computed by this crate is an *observed* local
//! digest compared against a digest supplied by whoever wrote the metadata. It
//! is never publisher authenticity evidence. Trusted piece hashes only permit
//! per-piece rejection and selective repair; a whole-file hash alone localizes
//! nothing, so a mismatch means a conservative restart and never a claim that
//! some piece was "repaired".
//!
//! [RFC 5854]: https://www.rfc-editor.org/rfc/rfc5854

mod parse;
mod pieces;
mod xml;

pub use parse::{FileHash, Metalink, MetalinkFile, MetalinkUrl, parse_metalink, parse_with_limits};
pub use pieces::{PieceMap, PieceVerification, StreamingPieceVerifier};

/// Bounds applied to every document. Defaults are deliberately small; a
/// caller that genuinely needs more must raise them explicitly.
#[derive(Clone, Copy, Debug)]
pub struct ParseLimits {
    pub max_bytes: usize,
    pub max_depth: usize,
    pub max_elements: usize,
    pub max_attributes: usize,
    pub max_name_bytes: usize,
    pub max_text_bytes: usize,
    pub max_files: usize,
    pub max_urls_per_file: usize,
    pub max_hashes_per_file: usize,
    pub max_pieces: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_bytes: 4 * 1024 * 1024,
            max_depth: 32,
            max_elements: 100_000,
            max_attributes: 32,
            max_name_bytes: 128,
            max_text_bytes: 8 * 1024,
            max_files: 1024,
            max_urls_per_file: 256,
            max_hashes_per_file: 32,
            max_pieces: 1 << 20,
        }
    }
}

/// Largest piece length accepted in a piece map (1 GiB).
pub const MAX_PIECE_LENGTH: u64 = 1 << 30;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetalinkError {
    /// The document exceeded the configured byte budget.
    DocumentTooLarge { bytes: usize, limit: usize },
    /// The document was not valid UTF-8.
    NotUtf8,
    /// A DTD (`<!DOCTYPE ...>`) was present. Never processed.
    DoctypeForbidden,
    /// An entity declaration (`<!ENTITY ...>`) was present. Never processed.
    EntityDeclarationForbidden,
    /// A reference other than the five predefined entities or a numeric
    /// character reference was used. Never expanded.
    EntityReferenceForbidden { reference: String },
    /// Nesting exceeded the configured depth budget.
    DepthExceeded { limit: usize },
    /// The element, attribute, URL, hash, or file count exceeded its budget.
    TooManyNodes { kind: &'static str, limit: usize },
    /// The text or name length exceeded its budget.
    ValueTooLong { kind: &'static str, limit: usize },
    /// The document is not well formed, or is not a Metalink 4 document.
    Malformed { detail: &'static str },
    /// A `<file name=...>` value is not a safe relative path.
    UnsafeFileName { detail: &'static str },
    /// A hash value was not lowercase-normalizable hexadecimal of the right
    /// length for its declared type.
    InvalidHash { detail: &'static str },
    /// The `<pieces>` map cannot describe the declared size.
    InconsistentPieces {
        declared_size: Option<u64>,
        piece_length: u64,
        pieces: usize,
    },
    /// `<pieces>` declared a digest type this crate cannot verify. The map is
    /// refused rather than silently downgraded to final-hash-only behavior.
    UnsupportedPieceHash { declared: String },
}

impl std::fmt::Display for MetalinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DocumentTooLarge { bytes, limit } => write!(
                f,
                "metalink.too_large: {bytes} bytes exceeds the {limit} byte budget"
            ),
            Self::NotUtf8 => write!(f, "metalink.not_utf8: the document is not UTF-8"),
            Self::DoctypeForbidden => write!(
                f,
                "metalink.doctype_forbidden: document type declarations are never processed"
            ),
            Self::EntityDeclarationForbidden => write!(
                f,
                "metalink.entity_declaration_forbidden: entity declarations are never processed"
            ),
            Self::EntityReferenceForbidden { reference } => write!(
                f,
                "metalink.entity_reference_forbidden: &{reference}; is not a predefined entity"
            ),
            Self::DepthExceeded { limit } => write!(
                f,
                "metalink.depth_exceeded: nesting deeper than {limit} elements"
            ),
            Self::TooManyNodes { kind, limit } => {
                write!(f, "metalink.too_many_nodes: more than {limit} {kind}")
            }
            Self::ValueTooLong { kind, limit } => {
                write!(f, "metalink.value_too_long: {kind} exceeds {limit} bytes")
            }
            Self::Malformed { detail } => write!(f, "metalink.malformed: {detail}"),
            Self::UnsafeFileName { detail } => write!(f, "metalink.unsafe_file_name: {detail}"),
            Self::InvalidHash { detail } => write!(f, "metalink.invalid_hash: {detail}"),
            Self::InconsistentPieces {
                declared_size,
                piece_length,
                pieces,
            } => write!(
                f,
                "metalink.inconsistent_pieces: {pieces} pieces of {piece_length} bytes cannot describe {}",
                declared_size.map_or_else(
                    || "an undeclared size".to_owned(),
                    |size| format!("{size} bytes")
                )
            ),
            Self::UnsupportedPieceHash { declared } => write!(
                f,
                "metalink.unsupported_piece_hash: {declared} piece hashes cannot be verified here"
            ),
        }
    }
}

impl std::error::Error for MetalinkError {}

/// Normalizes a hexadecimal digest to lowercase, rejecting anything that is not
/// exactly `expected_nibbles` hexadecimal characters.
pub(crate) fn normalize_hex(
    value: &str,
    expected_nibbles: Option<usize>,
) -> Result<String, MetalinkError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 512 {
        return Err(MetalinkError::InvalidHash {
            detail: "digest length is out of range",
        });
    }
    if let Some(expected) = expected_nibbles
        && trimmed.len() != expected
    {
        return Err(MetalinkError::InvalidHash {
            detail: "digest length does not match its declared type",
        });
    }
    if !trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MetalinkError::InvalidHash {
            detail: "digest is not hexadecimal",
        });
    }
    Ok(trimmed.to_ascii_lowercase())
}
