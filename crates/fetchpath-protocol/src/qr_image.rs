//! A QR code as an image file (FP-127): SVG or PNG, from the modules the
//! clients already draw on screen (`1` dark, `0` light, one string per row).
//!
//! Black on a white square with the four-module quiet zone scanners need,
//! whatever the theme. Std only: the PNG is a 1-bit grayscale image whose
//! zlib stream uses stored blocks, so no image or compression crate is added.
//! The result encodes the whole link, key included: callers write it only to
//! a path the person chose.

/// Modules of quiet zone around the code.
const QUIET: usize = 4;
/// Pixels per module in a PNG.
const SCALE: usize = 8;
/// The largest QR code (version 40) is 177 modules wide.
const MAX_MODULES: usize = 177;

/// The image formats a code can be saved as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Svg,
    Png,
}

impl Format {
    /// The format a file name asks for, by its extension (`.svg` or `.png`).
    pub fn from_path(path: &str) -> Option<Self> {
        let extension = path.rsplit_once('.')?.1.to_ascii_lowercase();
        match extension.as_str() {
            "svg" => Some(Self::Svg),
            "png" => Some(Self::Png),
            _ => None,
        }
    }
}

/// The file for `rows` in `format`, or `None` when `rows` is not a square of
/// `0` and `1` of a size a QR code can have.
pub fn render(rows: &[String], format: Format) -> Option<Vec<u8>> {
    let size = rows.len();
    let square = (1..=MAX_MODULES).contains(&size)
        && rows
            .iter()
            .all(|row| row.len() == size && row.bytes().all(|b| matches!(b, b'0' | b'1')));
    if !square {
        return None;
    }
    Some(match format {
        Format::Svg => svg(rows).into_bytes(),
        Format::Png => png(rows),
    })
}

fn svg(rows: &[String]) -> String {
    let size = rows.len();
    let full = size + 2 * QUIET;
    let mut path = String::new();
    for (y, row) in rows.iter().enumerate() {
        for (x, module) in row.bytes().enumerate() {
            if module == b'1' {
                path.push_str(&format!("M{} {}h1v1h-1z", x + QUIET, y + QUIET));
            }
        }
    }
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {full} {full}\" \
         width=\"{px}\" height=\"{px}\" shape-rendering=\"crispEdges\">\
         <title>QR code</title><rect width=\"{full}\" height=\"{full}\" fill=\"#fff\"/>\
         <path d=\"{path}\" fill=\"#000\"/></svg>\n",
        px = full * SCALE,
    )
}

fn png(rows: &[String]) -> Vec<u8> {
    let full = rows.len() + 2 * QUIET;
    let side = full * SCALE;
    let stride = side.div_ceil(8);
    let mut raw = Vec::with_capacity((stride + 1) * side);
    for pixel_row in 0..side {
        raw.push(0); // filter: none
        let module_row = (pixel_row / SCALE)
            .checked_sub(QUIET)
            .and_then(|y| rows.get(y));
        let mut packed = vec![0xff_u8; stride]; // 1 = white
        if let Some(row) = module_row {
            for (x, module) in row.bytes().enumerate() {
                if module != b'1' {
                    continue;
                }
                for pixel in (x + QUIET) * SCALE..(x + QUIET + 1) * SCALE {
                    packed[pixel / 8] &= !(0x80 >> (pixel % 8));
                }
            }
        }
        raw.extend_from_slice(&packed);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let side_bytes = u32::try_from(side).unwrap_or(u32::MAX).to_be_bytes();
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&side_bytes);
    header.extend_from_slice(&side_bytes);
    header.extend_from_slice(&[1, 0, 0, 0, 0]); // 1 bit, grayscale, no interlace
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream of stored (uncompressed) deflate blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut blocks = data.chunks(0xffff).peekable();
    if blocks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let len = u16::try_from(block.len()).unwrap_or(u16::MAX);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<String> {
        // A 3x3 pattern is enough: the writer does not care about QR validity.
        ["101", "010", "101"].map(str::to_owned).to_vec()
    }

    #[test]
    fn the_format_comes_from_the_extension() {
        assert_eq!(Format::from_path("a/code.SVG"), Some(Format::Svg));
        assert_eq!(Format::from_path("code.png"), Some(Format::Png));
        assert_eq!(Format::from_path("code.jpg"), None);
        assert_eq!(Format::from_path("code"), None);
    }

    #[test]
    fn rows_that_are_not_a_square_of_modules_are_refused() {
        assert!(render(&[], Format::Svg).is_none());
        assert!(render(&["10".to_owned()], Format::Svg).is_none());
        assert!(render(&["1x".to_owned(), "01".to_owned()], Format::Png).is_none());
    }

    #[test]
    fn the_svg_draws_every_dark_module_inside_a_quiet_zone() {
        let svg = String::from_utf8(render(&rows(), Format::Svg).unwrap()).unwrap();
        assert!(svg.contains("viewBox=\"0 0 11 11\""));
        assert_eq!(svg.matches('M').count(), 5);
        assert!(svg.contains("M4 4h1v1h-1z"));
        assert!(svg.contains("M6 6h1v1h-1z"));
    }

    /// Reads a PNG written by `png` back: every chunk checksum, the zlib
    /// stored blocks and the pixels.
    fn decode(png: &[u8]) -> (usize, Vec<Vec<bool>>) {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut at = 8;
        let mut side = 0;
        let mut idat = Vec::new();
        let mut kinds = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind = &png[at + 4..at + 8];
            let data = &png[at + 8..at + 8 + len];
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(&png[at + 4..at + 8 + len]), "crc of {kind:?}");
            kinds.push(kind.to_vec());
            match kind {
                b"IHDR" => {
                    side = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
                    assert_eq!(data[4..8], data[..4]);
                    assert_eq!(&data[8..], &[1, 0, 0, 0, 0]);
                }
                b"IDAT" => idat.extend_from_slice(data),
                _ => {}
            }
            at += 12 + len;
        }
        assert_eq!(
            kinds,
            [b"IHDR".to_vec(), b"IDAT".to_vec(), b"IEND".to_vec()]
        );
        assert_eq!(&idat[..2], &[0x78, 0x01]);
        let mut raw = Vec::new();
        let mut i = 2;
        loop {
            let last = idat[i] == 1;
            let len = u16::from_le_bytes([idat[i + 1], idat[i + 2]]) as usize;
            let not = u16::from_le_bytes([idat[i + 3], idat[i + 4]]);
            assert_eq!(not, !(len as u16));
            raw.extend_from_slice(&idat[i + 5..i + 5 + len]);
            i += 5 + len;
            if last {
                break;
            }
        }
        assert_eq!(
            u32::from_be_bytes(idat[i..i + 4].try_into().unwrap()),
            adler32(&raw)
        );
        assert_eq!(i + 4, idat.len());
        let stride = side.div_ceil(8);
        let pixels = raw
            .chunks(stride + 1)
            .map(|line| {
                assert_eq!(line[0], 0);
                (0..side)
                    .map(|x| line[1 + x / 8] & (0x80 >> (x % 8)) == 0)
                    .collect()
            })
            .collect();
        (side, pixels)
    }

    #[test]
    fn the_png_decodes_to_the_modules_on_white() {
        let png = render(&rows(), Format::Png).unwrap();
        let (side, dark) = decode(&png);
        assert_eq!(side, 11 * SCALE);
        assert_eq!(dark.len(), side);
        for (y, row) in dark.iter().enumerate() {
            for (x, pixel) in row.iter().enumerate() {
                let (mx, my) = (x / SCALE, y / SCALE);
                let expect = (QUIET..QUIET + 3).contains(&mx)
                    && (QUIET..QUIET + 3).contains(&my)
                    && rows()[my - QUIET].as_bytes()[mx - QUIET] == b'1';
                assert_eq!(*pixel, expect, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn a_full_size_code_spans_several_stored_blocks() {
        let size = MAX_MODULES;
        let rows: Vec<String> = (0..size)
            .map(|y| {
                (0..size)
                    .map(|x| if (x + y) % 3 == 0 { '1' } else { '0' })
                    .collect()
            })
            .collect();
        let (side, dark) = decode(&render(&rows, Format::Png).unwrap());
        assert_eq!(side, (size + 8) * SCALE);
        assert!(dark[(QUIET) * SCALE][QUIET * SCALE]);
    }
}
