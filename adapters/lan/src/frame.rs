//! Length-prefixed frames: `kind: u8`, `len: u32` big-endian, then payload.
//!
//! The length is checked against [`MAX_FRAME_BYTES`] before anything is
//! allocated, so a hostile length costs the receiver nothing.

use std::io::{self, Read, Write};

/// Largest plaintext chunk of content in one frame.
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
/// Largest payload on the wire: one chunk plus the AEAD tag.
pub const MAX_FRAME_BYTES: usize = MAX_CHUNK_BYTES + 16;

pub(crate) mod kind {
    pub const HELLO: u8 = 1;
    pub const CONFIRM: u8 = 2;
    pub const AUTH: u8 = 3;
    pub const REFUSE: u8 = 4;
    pub const GET: u8 = 10;
    pub const OFFER: u8 = 11;
    pub const DATA: u8 = 12;
    pub const END: u8 = 13;
    pub const BYE: u8 = 14;
}

pub fn write_frame(writer: &mut impl Write, kind: u8, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload too large",
        ));
    }
    let mut header = [0_u8; 5];
    header[0] = kind;
    header[1..].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    writer.write_all(&header)?;
    writer.write_all(payload)?;
    writer.flush()
}

pub fn read_frame(reader: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0_u8; 5];
    reader.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame length exceeds the limit",
        ));
    }
    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload)?;
    Ok((header[0], payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_frame_round_trips() {
        let mut wire = Vec::new();
        write_frame(&mut wire, kind::GET, b"sha256:ab").expect("write");
        let (kind, payload) = read_frame(&mut Cursor::new(wire)).expect("read");
        assert_eq!(kind, kind::GET);
        assert_eq!(payload, b"sha256:ab");
    }

    #[test]
    fn a_hostile_length_is_refused_before_allocation() {
        // Declares 4 GiB and supplies nothing. Allocating first would abort.
        let wire = [kind::DATA, 0xff, 0xff, 0xff, 0xff];
        let error = read_frame(&mut Cursor::new(wire)).expect_err("refused");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_truncated_frame_is_an_error_not_a_short_read() {
        let wire = [kind::DATA, 0, 0, 0, 8, 1, 2, 3];
        let error = read_frame(&mut Cursor::new(wire)).expect_err("truncated");
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn an_oversize_payload_is_never_written() {
        let mut wire = Vec::new();
        let big = vec![0_u8; MAX_FRAME_BYTES + 1];
        assert!(write_frame(&mut wire, kind::DATA, &big).is_err());
        assert!(wire.is_empty());
    }
}
