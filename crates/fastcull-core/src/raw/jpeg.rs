//! JPEG header sniffing: signature check and SOF dimension extraction without
//! decoding. Reads at most `SNIFF_LIMIT` bytes from the stream.

use std::io::{Read, Seek, SeekFrom};

use super::tiff::TiffError;

/// JPEG headers put SOF within the first segments; 64 KB covers cameras that
/// front-load large Exif/metadata segments.
const SNIFF_LIMIT: u64 = 64 * 1024;

/// True if the bytes at `offset` start with the JPEG SOI marker.
pub(crate) fn has_jpeg_signature<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
) -> Result<bool, TiffError> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut soi = [0u8; 2];
    if reader.read_exact(&mut soi).is_err() {
        return Ok(false);
    }
    Ok(soi == [0xFF, 0xD8])
}

/// Parse (width, height) from the first SOF segment of the JPEG at `offset`,
/// or `None` if the stream is not a parseable JPEG. Never reads more than
/// `min(len, SNIFF_LIMIT)` bytes.
pub(crate) fn sniff_dimensions<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    len: u64,
) -> Result<Option<(u32, u32)>, TiffError> {
    let budget = len.min(SNIFF_LIMIT) as usize;
    if budget < 4 {
        return Ok(None);
    }
    reader.seek(SeekFrom::Start(offset))?;
    let mut head = vec![0u8; budget];
    let mut filled = 0;
    while filled < budget {
        let n = reader.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    Ok(parse_sof(&head))
}

/// Locate the Exif TIFF block inside a bare JPEG's APP1 segment (issue
/// #8): returns `(absolute_offset, len)` of the TIFF header, or `None`
/// when there is no `Exif\0\0` APP1 in the pre-SOS segments. Reads at
/// most `SNIFF_LIMIT` bytes of headers; never decodes.
pub(crate) fn app1_tiff_bounds<R: Read + Seek>(
    reader: &mut R,
) -> Result<Option<(u64, u64)>, TiffError> {
    reader.seek(SeekFrom::Start(0))?;
    let mut head = vec![0u8; SNIFF_LIMIT as usize];
    let mut filled = 0;
    while filled < head.len() {
        let n = reader.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    head.truncate(filled);
    if head.len() < 4 || head[0] != 0xFF || head[1] != 0xD8 {
        return Ok(None);
    }
    let mut pos = 2usize;
    loop {
        if pos + 4 > head.len() {
            return Ok(None);
        }
        if head[pos] != 0xFF {
            return Ok(None); // not a marker: bail, never guess
        }
        // Skip fill bytes (FF FF ... marker).
        while pos + 4 <= head.len() && head[pos + 1] == 0xFF {
            pos += 1;
        }
        let marker = head[pos + 1];
        if marker == 0xDA || marker == 0xD9 {
            return Ok(None); // SOS/EOI: image data begins, no Exif APP1
        }
        let seg_len = u16::from_be_bytes([head[pos + 2], head[pos + 3]]) as usize;
        if seg_len < 2 {
            return Ok(None);
        }
        if marker == 0xE1 {
            // APP1: payload starts after the 2-byte length.
            let payload = pos + 4;
            if payload + 6 <= head.len() && &head[payload..payload + 6] == b"Exif\0\0" {
                let tiff = payload + 6;
                let tiff_len = seg_len.saturating_sub(2 + 6);
                if tiff_len >= 8 {
                    return Ok(Some((tiff as u64, tiff_len as u64)));
                }
                return Ok(None);
            }
        }
        pos += 2 + seg_len;
    }
}

/// Scan JPEG segments for SOF0–SOF15 (excluding DHT/JPG/DAC markers) and
/// return (width, height).
fn parse_sof(data: &[u8]) -> Option<(u32, u32)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut pos = 2;
    loop {
        // Segment marker: FF xx (skip fill bytes).
        if pos + 4 > data.len() {
            return None;
        }
        if data[pos] != 0xFF {
            return None; // desynchronized
        }
        let marker = data[pos + 1];
        pos += 2;
        match marker {
            0xFF => {
                pos -= 1; // fill byte, resync on next 0xFF
                continue;
            }
            0xD8 | 0x01 | 0xD0..=0xD7 => continue, // no payload
            0xD9 | 0xDA => return None,            // EOI / entropy data: no SOF found
            _ => {}
        }
        let seg_len = usize::from(u16::from_be_bytes([data[pos], data[pos + 1]]));
        if seg_len < 2 || pos + seg_len > data.len() {
            return None;
        }
        let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if seg_len < 7 {
                return None;
            }
            let height = u32::from(u16::from_be_bytes([data[pos + 3], data[pos + 4]]));
            let width = u32::from(u16::from_be_bytes([data[pos + 5], data[pos + 6]]));
            return (width > 0 && height > 0).then_some((width, height));
        }
        pos += seg_len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn jpeg_with_exif_padding(width: u16, height: u16, padding: usize) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        // APP1 segment full of padding (like a big Exif block).
        let seg_len = (padding + 2) as u16;
        j.extend_from_slice(&[0xFF, 0xE1]);
        j.extend_from_slice(&seg_len.to_be_bytes());
        j.extend(std::iter::repeat_n(0xAB, padding));
        // SOF0
        j.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]);
        j.extend_from_slice(&height.to_be_bytes());
        j.extend_from_slice(&width.to_be_bytes());
        j.extend_from_slice(&[0x01, 0x11, 0x00]);
        j.extend_from_slice(&[0xFF, 0xD9]);
        j
    }

    #[test]
    fn sniffs_dimensions_past_app_segments() {
        let jpeg = jpeg_with_exif_padding(1616, 1080, 5000);
        let len = jpeg.len() as u64;
        let mut cur = Cursor::new(jpeg);
        assert_eq!(
            sniff_dimensions(&mut cur, 0, len).unwrap(),
            Some((1616, 1080))
        );
    }

    #[test]
    fn progressive_sof2_is_found() {
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xC2, 0x00, 0x0B, 0x08];
        j.extend_from_slice(&720u16.to_be_bytes());
        j.extend_from_slice(&1080u16.to_be_bytes());
        j.extend_from_slice(&[0x01, 0x11, 0x00, 0xFF, 0xD9]);
        assert_eq!(parse_sof(&j), Some((1080, 720)));
    }

    #[test]
    fn garbage_and_truncation_yield_none() {
        assert_eq!(parse_sof(&[]), None);
        assert_eq!(parse_sof(&[0xFF, 0xD8]), None);
        assert_eq!(parse_sof(&[0x00; 100]), None);
        // SOI then garbage
        let mut j = vec![0xFF, 0xD8];
        j.extend_from_slice(&[0x12, 0x34, 0x56]);
        assert_eq!(parse_sof(&j), None);
        // Truncated mid-SOF
        assert_eq!(parse_sof(&[0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08]), None);
    }

    #[test]
    fn dht_is_not_mistaken_for_sof() {
        // DHT (0xC4) followed by EOI — must not be parsed as SOF.
        let j = [0xFF, 0xD8, 0xFF, 0xC4, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xD9];
        assert_eq!(parse_sof(&j), None);
    }
}
