//! Looking at a link before downloading it: one ranged request for the
//! first byte, read for its headers only, so a person can see what the link
//! is (a file or a web page), its name and size, and whether it can resume.

use super::{ProtocolDecision, RequestContext, TransferError, configure, curl_error};
use curl::easy::{Easy, List};
use std::cell::RefCell;
use std::time::Duration;

/// What a link's headers say about it. Every field is the server's claim,
/// not a checked fact.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LinkFacts {
    /// The media type without parameters, lower case (`application/zip`).
    pub content_type: Option<String>,
    /// The name from `Content-Disposition`, unsanitized.
    pub file_name: Option<String>,
    /// The full size: the total of a ranged answer, or the length of a
    /// whole one.
    pub size: Option<u64>,
    /// The server answered the range, so an interrupted download can resume.
    pub resumable: bool,
}

impl LinkFacts {
    /// A page for reading rather than a file: HTML or XHTML.
    pub fn is_web_page(&self) -> bool {
        matches!(
            self.content_type.as_deref(),
            Some("text/html" | "application/xhtml+xml")
        )
    }
}

#[derive(Default)]
struct Headers {
    status: Option<u32>,
    content_type: Option<String>,
    disposition: Option<String>,
    length: Option<u64>,
    range_total: Option<u64>,
}

impl Headers {
    fn ingest(&mut self, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            return;
        };
        let line = line.trim();
        if line.starts_with("HTTP/") {
            // A redirect or interim answer: start again for the next one.
            *self = Self {
                status: line.split_whitespace().nth(1).and_then(|v| v.parse().ok()),
                ..Self::default()
            };
            return;
        }
        let Some((name, value)) = line.split_once(':') else {
            return;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-type") {
            let media = value.split(';').next().unwrap_or_default().trim();
            if !media.is_empty() {
                self.content_type = Some(media.to_ascii_lowercase());
            }
        } else if name.eq_ignore_ascii_case("content-disposition") {
            self.disposition = Some(value.to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            self.length = value.parse().ok();
        } else if name.eq_ignore_ascii_case("content-range") {
            self.range_total = value
                .rsplit_once('/')
                .and_then(|(_, total)| total.trim().parse().ok());
        }
    }
}

/// The file name a `Content-Disposition` value gives, preferring the
/// RFC 6266 `filename*` form.
pub fn disposition_file_name(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        let Some((key, raw)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let raw = raw.trim();
        if key == "filename*" {
            // charset'language'percent-encoded
            let encoded = raw.splitn(3, '\'').nth(2).unwrap_or(raw);
            if let Ok(name) = percent_encoding::percent_decode_str(encoded).decode_utf8() {
                let name = name.trim().to_owned();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        } else if key == "filename" {
            let name = raw.trim_matches('"').trim();
            if !name.is_empty() {
                plain = Some(name.to_owned());
            }
        }
    }
    plain
}

/// Asks for the first byte of `url` and reads the answer's headers. The body
/// is never kept. HTTP(S) only.
pub fn inspect_link(
    url: &str,
    context: &RequestContext,
    decision: &ProtocolDecision,
    timeout: Duration,
) -> Result<LinkFacts, TransferError> {
    let mut easy = Easy::new();
    configure(&mut easy, url, context, decision)?;
    easy.range("0-0").map_err(curl_error)?;
    easy.timeout(timeout).map_err(curl_error)?;
    easy.connect_timeout(timeout.min(Duration::from_secs(10)))
        .map_err(curl_error)?;
    let mut list = List::new();
    list.append("Accept-Encoding: identity")
        .map_err(curl_error)?;
    easy.http_headers(list).map_err(curl_error)?;
    let headers = RefCell::new(Headers::default());
    let result = {
        let mut transfer = easy.transfer();
        transfer
            .header_function(|line| {
                headers.borrow_mut().ingest(line);
                true
            })
            .map_err(curl_error)?;
        // The headers are all that is wanted; refusing the body ends the
        // request as soon as it starts.
        transfer.write_function(|_| Ok(0)).map_err(curl_error)?;
        transfer.perform()
    };
    let headers = headers.into_inner();
    let status = headers
        .status
        .or_else(|| easy.response_code().ok().filter(|code| *code != 0));
    match (result, status) {
        (_, Some(status)) if status >= 400 => {
            return Err(TransferError::Transport(format!("HTTP status {status}")));
        }
        (Err(error), _) if !error.is_write_error() => return Err(curl_error(error)),
        (_, None) => return Err(TransferError::Transport("no answer from the server".into())),
        _ => {}
    }
    let resumable = status == Some(206) && headers.range_total.is_some();
    Ok(LinkFacts {
        content_type: headers.content_type,
        file_name: headers
            .disposition
            .as_deref()
            .and_then(disposition_file_name),
        size: if status == Some(206) {
            headers.range_total
        } else {
            headers.length
        },
        resumable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn serve(answer: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(answer.as_bytes());
        });
        format!("http://{address}/file")
    }

    fn inspect(answer: &'static str) -> Result<LinkFacts, TransferError> {
        inspect_link(
            &serve(answer),
            &RequestContext::default(),
            &crate::decide_protocol(crate::ProtocolCapabilities::detect()),
            Duration::from_secs(5),
        )
    }

    #[test]
    fn a_ranged_answer_gives_the_full_size_name_and_resume() {
        let facts = inspect(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/zip\r\n\
             Content-Range: bytes 0-0/5000\r\nContent-Length: 1\r\n\
             Content-Disposition: attachment; filename=\"a b.zip\"\r\n\r\nx",
        )
        .unwrap();
        assert_eq!(
            facts,
            LinkFacts {
                content_type: Some("application/zip".into()),
                file_name: Some("a b.zip".into()),
                size: Some(5000),
                resumable: true,
            }
        );
        assert!(!facts.is_web_page());
    }

    #[test]
    fn a_whole_html_answer_is_a_web_page_that_cannot_resume() {
        let facts = inspect(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: 1234\r\n\r\n<html>",
        )
        .unwrap();
        assert!(facts.is_web_page());
        assert_eq!((facts.size, facts.resumable), (Some(1234), false));
    }

    #[test]
    fn an_error_status_is_reported() {
        let error = inspect("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").unwrap_err();
        assert!(error.to_string().contains("404"), "{error}");
    }

    #[test]
    fn a_redirect_to_ftp_is_not_followed() {
        let ftp = TcpListener::bind("127.0.0.1:0").unwrap();
        ftp.set_nonblocking(true).unwrap();
        let answer = format!(
            "HTTP/1.1 302 Found\r\nLocation: ftp://{}/file\r\nContent-Length: 0\r\n\r\n",
            ftp.local_addr().unwrap()
        );
        let error = inspect(Box::leak(answer.into_boxed_str())).unwrap_err();
        assert!(ftp.accept().is_err(), "the FTP address was contacted");
        assert!(!error.to_string().is_empty());
    }

    #[test]
    fn the_encoded_file_name_wins() {
        assert_eq!(
            disposition_file_name(
                "attachment; filename=\"x.bin\"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"
            )
            .as_deref(),
            Some("résumé.pdf")
        );
        assert_eq!(disposition_file_name("inline").as_deref(), None);
    }
}
