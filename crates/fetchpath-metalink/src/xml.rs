//! A bounded, hand-written XML reader.
//!
//! It deliberately supports only what a Metalink 4 document needs: elements,
//! attributes, character data, CDATA sections, comments, and processing
//! instructions. Document type declarations and entity declarations are a hard
//! error, so there is no entity expansion and no external entity resolution.
//! Every limit in [`ParseLimits`] is enforced while scanning, and every failure
//! is a typed [`MetalinkError`].

use crate::{MetalinkError, ParseLimits};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Event {
    Start {
        name: String,
        attributes: Vec<(String, String)>,
    },
    End {
        name: String,
    },
    Text(String),
}

/// Reads a whole document into a bounded event list.
pub(crate) fn read_events(input: &[u8], limits: &ParseLimits) -> Result<Vec<Event>, MetalinkError> {
    if input.len() > limits.max_bytes {
        return Err(MetalinkError::DocumentTooLarge {
            bytes: input.len(),
            limit: limits.max_bytes,
        });
    }
    let text = std::str::from_utf8(input).map_err(|_| MetalinkError::NotUtf8)?;
    Reader {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        limits,
        events: Vec::new(),
        stack: Vec::new(),
        elements: 0,
        roots: 0,
    }
    .run()
}

struct Reader<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    limits: &'a ParseLimits,
    events: Vec<Event>,
    stack: Vec<String>,
    elements: usize,
    roots: usize,
}

const MALFORMED_EOF: MetalinkError = MetalinkError::Malformed {
    detail: "the document ended inside markup",
};

impl<'a> Reader<'a> {
    fn run(mut self) -> Result<Vec<Event>, MetalinkError> {
        while self.pos < self.bytes.len() {
            if self.bytes[self.pos] == b'<' {
                self.markup()?;
            } else {
                self.character_data()?;
            }
        }
        if !self.stack.is_empty() {
            return Err(MetalinkError::Malformed {
                detail: "an element was never closed",
            });
        }
        if self.roots != 1 {
            return Err(MetalinkError::Malformed {
                detail: "a document needs exactly one root element",
            });
        }
        Ok(self.events)
    }

    fn slice(&self, start: usize, end: usize) -> Result<&'a str, MetalinkError> {
        self.text.get(start..end).ok_or(MetalinkError::Malformed {
            detail: "markup split a UTF-8 character",
        })
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.bytes[self.pos..].starts_with(prefix.as_bytes())
    }

    fn starts_with_ignore_case(&self, prefix: &str) -> bool {
        let rest = &self.bytes[self.pos..];
        rest.len() >= prefix.len() && rest[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    }

    fn find(&self, needle: &str) -> Option<usize> {
        self.text
            .get(self.pos..)
            .and_then(|rest| rest.find(needle))
            .map(|offset| self.pos + offset)
    }

    fn character_data(&mut self) -> Result<(), MetalinkError> {
        let start = self.pos;
        let end = self.find("<").unwrap_or(self.bytes.len());
        self.pos = end;
        let raw = self.slice(start, end)?;
        let decoded = decode(raw, self.limits)?;
        if decoded.trim().is_empty() {
            return Ok(());
        }
        if self.stack.is_empty() {
            return Err(MetalinkError::Malformed {
                detail: "text is not allowed outside the root element",
            });
        }
        self.push_text(decoded);
        Ok(())
    }

    fn push_text(&mut self, decoded: String) {
        if let Some(Event::Text(existing)) = self.events.last_mut() {
            existing.push_str(&decoded);
        } else {
            self.events.push(Event::Text(decoded));
        }
    }

    fn markup(&mut self) -> Result<(), MetalinkError> {
        if self.starts_with("<!--") {
            self.pos += 4;
            let end = self.find("-->").ok_or(MALFORMED_EOF)?;
            self.pos = end + 3;
            return Ok(());
        }
        if self.starts_with("<![CDATA[") {
            self.pos += 9;
            let start = self.pos;
            let end = self.find("]]>").ok_or(MALFORMED_EOF)?;
            let raw = self.slice(start, end)?;
            self.pos = end + 3;
            if raw.len() > self.limits.max_text_bytes {
                return Err(MetalinkError::ValueTooLong {
                    kind: "a CDATA section",
                    limit: self.limits.max_text_bytes,
                });
            }
            reject_control_characters(raw)?;
            if !raw.trim().is_empty() {
                if self.stack.is_empty() {
                    return Err(MetalinkError::Malformed {
                        detail: "text is not allowed outside the root element",
                    });
                }
                self.push_text(raw.to_owned());
            }
            return Ok(());
        }
        if self.starts_with("<!") {
            // Nothing below this line is ever processed: a DTD is the entry
            // point for entity expansion and external entity resolution, so the
            // whole document is refused instead.
            return Err(if self.starts_with_ignore_case("<!DOCTYPE") {
                MetalinkError::DoctypeForbidden
            } else if self.starts_with_ignore_case("<!ENTITY") {
                MetalinkError::EntityDeclarationForbidden
            } else {
                MetalinkError::Malformed {
                    detail: "markup declarations are not supported",
                }
            });
        }
        if self.starts_with("<?") {
            self.pos += 2;
            let end = self.find("?>").ok_or(MALFORMED_EOF)?;
            self.pos = end + 2;
            return Ok(());
        }
        if self.starts_with("</") {
            self.pos += 2;
            return self.end_tag();
        }
        self.pos += 1;
        self.start_tag()
    }

    fn end_tag(&mut self) -> Result<(), MetalinkError> {
        let name = self.read_name()?;
        self.skip_whitespace();
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'>' {
            return Err(MetalinkError::Malformed {
                detail: "an end tag is not terminated",
            });
        }
        self.pos += 1;
        match self.stack.pop() {
            Some(open) if open == name => {
                self.events.push(Event::End { name });
                Ok(())
            }
            _ => Err(MetalinkError::Malformed {
                detail: "an end tag does not match its start tag",
            }),
        }
    }

    fn start_tag(&mut self) -> Result<(), MetalinkError> {
        let name = self.read_name()?;
        let mut attributes: Vec<(String, String)> = Vec::new();
        let self_closing;
        loop {
            let had_space = self.skip_whitespace();
            if self.pos >= self.bytes.len() {
                return Err(MALFORMED_EOF);
            }
            match self.bytes[self.pos] {
                b'>' => {
                    self.pos += 1;
                    self_closing = false;
                    break;
                }
                b'/' => {
                    if self.bytes.get(self.pos + 1) != Some(&b'>') {
                        return Err(MetalinkError::Malformed {
                            detail: "a start tag is not terminated",
                        });
                    }
                    self.pos += 2;
                    self_closing = true;
                    break;
                }
                _ => {
                    if !had_space {
                        return Err(MetalinkError::Malformed {
                            detail: "attributes must be separated by whitespace",
                        });
                    }
                    if attributes.len() == self.limits.max_attributes {
                        return Err(MetalinkError::TooManyNodes {
                            kind: "attributes on one element",
                            limit: self.limits.max_attributes,
                        });
                    }
                    let attribute = self.read_attribute()?;
                    if attributes.iter().any(|(name, _)| name == &attribute.0) {
                        return Err(MetalinkError::Malformed {
                            detail: "an attribute is declared twice",
                        });
                    }
                    attributes.push(attribute);
                }
            }
        }

        self.elements += 1;
        if self.elements > self.limits.max_elements {
            return Err(MetalinkError::TooManyNodes {
                kind: "elements",
                limit: self.limits.max_elements,
            });
        }
        if self.stack.is_empty() {
            self.roots += 1;
            if self.roots > 1 {
                return Err(MetalinkError::Malformed {
                    detail: "a document needs exactly one root element",
                });
            }
        }
        if self.stack.len() + 1 > self.limits.max_depth {
            return Err(MetalinkError::DepthExceeded {
                limit: self.limits.max_depth,
            });
        }

        self.events.push(Event::Start {
            name: name.clone(),
            attributes,
        });
        if self_closing {
            self.events.push(Event::End { name });
        } else {
            self.stack.push(name);
        }
        Ok(())
    }

    fn read_attribute(&mut self) -> Result<(String, String), MetalinkError> {
        let name = self.read_name()?;
        self.skip_whitespace();
        if self.pos >= self.bytes.len() || self.bytes[self.pos] != b'=' {
            return Err(MetalinkError::Malformed {
                detail: "an attribute has no value",
            });
        }
        self.pos += 1;
        self.skip_whitespace();
        let quote = match self.bytes.get(self.pos) {
            Some(&byte @ (b'"' | b'\'')) => byte,
            _ => {
                return Err(MetalinkError::Malformed {
                    detail: "attribute values must be quoted",
                });
            }
        };
        self.pos += 1;
        let start = self.pos;
        let end = self.bytes[start..]
            .iter()
            .position(|byte| *byte == quote)
            .map(|offset| start + offset)
            .ok_or(MALFORMED_EOF)?;
        let raw = self.slice(start, end)?;
        self.pos = end + 1;
        if raw.len() > self.limits.max_text_bytes {
            return Err(MetalinkError::ValueTooLong {
                kind: "an attribute value",
                limit: self.limits.max_text_bytes,
            });
        }
        if raw.contains('<') {
            return Err(MetalinkError::Malformed {
                detail: "an attribute value contains a raw '<'",
            });
        }
        Ok((name, decode(raw, self.limits)?))
    }

    fn read_name(&mut self) -> Result<String, MetalinkError> {
        let start = self.pos;
        while self.pos < self.bytes.len() && is_name_byte(self.bytes[self.pos]) {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(MetalinkError::Malformed {
                detail: "an element or attribute has no name",
            });
        }
        if self.pos - start > self.limits.max_name_bytes {
            return Err(MetalinkError::ValueTooLong {
                kind: "an element or attribute name",
                limit: self.limits.max_name_bytes,
            });
        }
        Ok(self.slice(start, self.pos)?.to_owned())
    }

    fn skip_whitespace(&mut self) -> bool {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
        self.pos != start
    }
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':')
}

fn reject_control_characters(value: &str) -> Result<(), MetalinkError> {
    if value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\t' | '\n' | '\r'))
    {
        return Err(MetalinkError::Malformed {
            detail: "a value contains a control character",
        });
    }
    Ok(())
}

/// Resolves the five predefined entities and numeric character references. Any
/// other reference is refused, so a document can never name a declared entity.
fn decode(raw: &str, limits: &ParseLimits) -> Result<String, MetalinkError> {
    reject_control_characters(raw)?;
    if raw.len() > limits.max_text_bytes {
        return Err(MetalinkError::ValueTooLong {
            kind: "a decoded value",
            limit: limits.max_text_bytes,
        });
    }
    if !raw.contains('&') {
        return Ok(raw.to_owned());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(position) = rest.find('&') {
        out.push_str(&rest[..position]);
        let tail = &rest[position + 1..];
        let end = tail.find(';').ok_or(MetalinkError::Malformed {
            detail: "a reference is not terminated",
        })?;
        if end > 12 {
            return Err(MetalinkError::EntityReferenceForbidden {
                reference: tail.get(..12).unwrap_or("oversized").to_owned(),
            });
        }
        let reference = &tail[..end];
        match reference {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            numeric if numeric.starts_with('#') => {
                out.push(numeric_character(&numeric[1..])?);
            }
            other => {
                return Err(MetalinkError::EntityReferenceForbidden {
                    reference: other.to_owned(),
                });
            }
        }
        if out.len() > limits.max_text_bytes {
            return Err(MetalinkError::ValueTooLong {
                kind: "a decoded value",
                limit: limits.max_text_bytes,
            });
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    if out.len() > limits.max_text_bytes {
        return Err(MetalinkError::ValueTooLong {
            kind: "a decoded value",
            limit: limits.max_text_bytes,
        });
    }
    reject_control_characters(&out)?;
    Ok(out)
}

fn numeric_character(digits: &str) -> Result<char, MetalinkError> {
    let malformed = MetalinkError::Malformed {
        detail: "a numeric character reference is out of range",
    };
    let value = if let Some(hex) = digits.strip_prefix(['x', 'X']) {
        if hex.is_empty() || hex.len() > 8 {
            return Err(malformed);
        }
        u32::from_str_radix(hex, 16).map_err(|_| malformed.clone())?
    } else {
        if digits.is_empty() || digits.len() > 8 {
            return Err(malformed);
        }
        digits.parse::<u32>().map_err(|_| malformed.clone())?
    };
    char::from_u32(value).ok_or(malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(input: &str) -> Result<Vec<Event>, MetalinkError> {
        read_events(input.as_bytes(), &ParseLimits::default())
    }

    #[test]
    fn reads_elements_attributes_and_text() {
        let parsed = events("<?xml version=\"1.0\"?><a k='v'><b>hi</b><c/></a>").unwrap();
        assert_eq!(
            parsed,
            vec![
                Event::Start {
                    name: "a".into(),
                    attributes: vec![("k".into(), "v".into())],
                },
                Event::Start {
                    name: "b".into(),
                    attributes: Vec::new(),
                },
                Event::Text("hi".into()),
                Event::End { name: "b".into() },
                Event::Start {
                    name: "c".into(),
                    attributes: Vec::new(),
                },
                Event::End { name: "c".into() },
                Event::End { name: "a".into() },
            ]
        );
    }

    #[test]
    fn comments_processing_instructions_and_cdata_survive() {
        let parsed = events("<a><!-- <!DOCTYPE x> --><![CDATA[a<b&c]]><?pi ?></a>").unwrap();
        assert_eq!(parsed[1], Event::Text("a<b&c".into()));
    }

    #[test]
    fn refuses_every_doctype_and_entity_declaration() {
        assert_eq!(
            events("<!DOCTYPE a [<!ENTITY x \"y\">]><a/>"),
            Err(MetalinkError::DoctypeForbidden)
        );
        assert_eq!(
            events("<a><!ENTITY x \"y\"></a>"),
            Err(MetalinkError::EntityDeclarationForbidden)
        );
        assert_eq!(
            events("<!doctype a><a/>"),
            Err(MetalinkError::DoctypeForbidden)
        );
    }

    #[test]
    fn refuses_undeclared_entity_references_instead_of_expanding_them() {
        assert_eq!(
            events("<a>&lol1;</a>"),
            Err(MetalinkError::EntityReferenceForbidden {
                reference: "lol1".into()
            })
        );
        assert_eq!(
            events("<a k='&xxe;'/>"),
            Err(MetalinkError::EntityReferenceForbidden {
                reference: "xxe".into()
            })
        );
    }

    #[test]
    fn decodes_predefined_and_numeric_references_only() {
        let parsed = events("<a>&amp;&lt;&gt;&quot;&apos;&#65;&#x42;</a>").unwrap();
        assert_eq!(parsed[1], Event::Text("&<>\"'AB".into()));
    }

    #[test]
    fn enforces_size_depth_and_element_budgets() {
        let limits = ParseLimits {
            max_bytes: 8,
            ..ParseLimits::default()
        };
        assert_eq!(
            read_events(b"<a>0123456789</a>", &limits),
            Err(MetalinkError::DocumentTooLarge {
                bytes: 17,
                limit: 8
            })
        );

        let deep = "<a>".repeat(40) + &"</a>".repeat(40);
        assert_eq!(
            events(&deep),
            Err(MetalinkError::DepthExceeded { limit: 32 })
        );

        let limits = ParseLimits {
            max_elements: 2,
            ..ParseLimits::default()
        };
        assert_eq!(
            read_events(b"<a><b/><c/></a>", &limits),
            Err(MetalinkError::TooManyNodes {
                kind: "elements",
                limit: 2
            })
        );
    }

    #[test]
    fn enforces_attribute_name_and_text_budgets() {
        let limits = ParseLimits {
            max_attributes: 1,
            ..ParseLimits::default()
        };
        assert_eq!(
            read_events(b"<a x='1' y='2'/>", &limits),
            Err(MetalinkError::TooManyNodes {
                kind: "attributes on one element",
                limit: 1
            })
        );
        let limits = ParseLimits {
            max_text_bytes: 4,
            ..ParseLimits::default()
        };
        assert_eq!(
            read_events(b"<a>0123456789</a>", &limits),
            Err(MetalinkError::ValueTooLong {
                kind: "a decoded value",
                limit: 4
            })
        );
        let limits = ParseLimits {
            max_name_bytes: 2,
            ..ParseLimits::default()
        };
        assert_eq!(
            read_events(b"<abcdef/>", &limits),
            Err(MetalinkError::ValueTooLong {
                kind: "an element or attribute name",
                limit: 2
            })
        );
    }

    #[test]
    fn rejects_malformed_documents_without_panicking() {
        for hostile in [
            "<a>",
            "</a>",
            "<a></b>",
            "<a/><b/>",
            "text",
            "<a k=v/>",
            "<a k/>",
            "<a",
            "<a>&amp",
            "<!-- unterminated",
            "<![CDATA[unterminated",
            "<?pi",
            "<a k='1' k='2'/>",
            "<a k='1'b='2'/>",
            "<>",
            "<a>\u{0}</a>",
            "<a k='\u{0}'/>",
            "<a>&#xZZ;</a>",
            "<a>&#999999999;</a>",
        ] {
            assert!(
                events(hostile).is_err(),
                "expected a typed error for {hostile:?}"
            );
        }
    }

    #[test]
    fn rejects_non_utf8_input() {
        assert_eq!(
            read_events(&[0xff, 0xfe], &ParseLimits::default()),
            Err(MetalinkError::NotUtf8)
        );
    }
}
