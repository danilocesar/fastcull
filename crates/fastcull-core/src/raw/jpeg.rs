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

/// True when the JPEG stream's entropy-coded scan reaches a terminating
/// EOI marker — i.e. the stream was written to completion.
///
/// Issue #31: zune-jpeg 0.4 zero-fills missing scan data and reports a
/// truncated stream as a SUCCESSFUL decode (`bitstream.rs` stops counting
/// `overread_by` once it starts zero-filling, so even strict mode's
/// "premature end of buffer" check can never fire), which turned cut-off
/// files into giant mostly-blank frames instead of a Failed badge. The
/// decoder offers no bytes-consumed accessor at 0.4.21 (and 0.5.15 is a
/// measured performance regression — raw-pipeline.md), so completeness is
/// checked on the raw bytes instead: within the entropy-coded data every
/// 0xFF is either stuffed (FF 00) or a real marker, so a genuine FF D9
/// pair at or after the first SOS is an end-of-image marker. The search
/// runs BACKWARDS from the tail because intact camera files end with EOI
/// (plus at most a little padding) — the hit is immediate; only an
/// actually-truncated stream pays a full reverse scan before rejection.
/// Scanning from SOS, not from 0: pre-SOS APP1 segments legitimately
/// embed a whole thumbnail JPEG including its own EOI, which must not
/// vouch for the main scan.
pub(crate) fn scan_is_terminated(data: &[u8]) -> bool {
    let Some(scan_start) = first_sos_end(data) else {
        return false; // no SOS: nothing decodable was ever written
    };
    data[scan_start..]
        .windows(2)
        .rev()
        .any(|w| w == [0xFF, 0xD9])
}

/// Offset of the first byte after the first SOS segment header (where
/// entropy-coded data begins), or `None` if the stream has no SOS.
fn first_sos_end(data: &[u8]) -> Option<usize> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut pos = 2;
    loop {
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
            0xD9 => return None,                   // EOI before any SOS
            _ => {}
        }
        let seg_len = usize::from(u16::from_be_bytes([data[pos], data[pos + 1]]));
        if seg_len < 2 || pos + seg_len > data.len() {
            return None;
        }
        if marker == 0xDA {
            return Some(pos + seg_len);
        }
        pos += seg_len;
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

/// Test-only builders for hostile JPEG streams (issue #31): real encoded
/// streams whose SOF dimension claim is patched and/or whose entropy-coded
/// scan is cut off — the crafted-stream shape from the issue's repro.
/// Committed fixtures must be tiny and synthetic (CLAUDE.md), so these are
/// built in memory from `jpeg_encoder` output, never from real RAWs.
#[cfg(test)]
pub(crate) mod hostile {
    /// A real, decodable baseline JPEG (mid-gray) of the given size.
    pub(crate) fn encoded(w: u16, h: u16) -> Vec<u8> {
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(
                &vec![128u8; usize::from(w) * usize::from(h) * 3],
                w,
                h,
                jpeg_encoder::ColorType::Rgb,
            )
            .expect("test JPEG encodes");
        out
    }

    /// Overwrite the SOF height/width fields in place (the hostile header
    /// claim). Panics if the stream has no SOF — test-only.
    pub(crate) fn patch_sof_dims(jpeg: &mut [u8], w: u16, h: u16) {
        let pos = sof_payload_offset(jpeg).expect("stream has a SOF segment");
        // Payload layout: len(2) precision(1) height(2) width(2).
        jpeg[pos + 3..pos + 5].copy_from_slice(&h.to_be_bytes());
        jpeg[pos + 5..pos + 7].copy_from_slice(&w.to_be_bytes());
    }

    /// Cut the stream `keep` bytes into the entropy-coded scan: everything
    /// after that point — including the EOI — is dropped, exactly what a
    /// half-written file on a dying card looks like.
    pub(crate) fn truncate_scan(jpeg: &[u8], keep: usize) -> Vec<u8> {
        let scan = super::first_sos_end(jpeg).expect("stream has a SOS segment");
        jpeg[..(scan + keep).min(jpeg.len().saturating_sub(2))].to_vec()
    }

    /// Offset of the first SOF segment's payload (its length bytes).
    fn sof_payload_offset(data: &[u8]) -> Option<usize> {
        let mut pos = 2;
        loop {
            if pos + 4 > data.len() || data[pos] != 0xFF {
                return None;
            }
            let marker = data[pos + 1];
            pos += 2;
            match marker {
                0xFF => {
                    pos -= 1;
                    continue;
                }
                0xD8 | 0x01 | 0xD0..=0xD7 => continue,
                0xD9 | 0xDA => return None,
                _ => {}
            }
            let is_sof = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
            if is_sof {
                return Some(pos);
            }
            let seg_len = usize::from(u16::from_be_bytes([data[pos], data[pos + 1]]));
            if seg_len < 2 {
                return None;
            }
            pos += seg_len;
        }
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

    /// Issue #31: a complete stream's scan ends in EOI; a truncated one
    /// never reaches a terminating marker and must be detected from the
    /// bytes alone (zune-jpeg 0.4 decodes it as "success").
    #[test]
    fn scan_termination_detects_truncation() {
        let intact = hostile::encoded(64, 64);
        assert!(scan_is_terminated(&intact), "an intact stream terminates");
        let truncated = hostile::truncate_scan(&intact, 16);
        assert!(
            !scan_is_terminated(&truncated),
            "a scan cut off before EOI must be flagged"
        );
        // Truncated to exactly zero scan bytes (cut right after SOS).
        assert!(!scan_is_terminated(&hostile::truncate_scan(&intact, 0)));
        // Not even headers.
        assert!(!scan_is_terminated(&[]));
        assert!(!scan_is_terminated(&[0xFF, 0xD8]));
        // SOI+SOF+EOI but no SOS ever written: nothing decodable exists.
        let mut no_sos = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08];
        no_sos.extend_from_slice(&64u16.to_be_bytes());
        no_sos.extend_from_slice(&64u16.to_be_bytes());
        no_sos.extend_from_slice(&[0x01, 0x11, 0x00, 0xFF, 0xD9]);
        assert!(!scan_is_terminated(&no_sos));
    }

    /// An EOI that lives inside a pre-SOS APP1 segment (EXIF thumbnails
    /// are whole JPEGs, EOI included) must NOT vouch for a truncated main
    /// scan — the search space starts at the first SOS.
    #[test]
    fn app1_thumbnail_eoi_does_not_mask_a_truncated_scan() {
        let intact = hostile::encoded(64, 64);
        // SOI, then an APP1 whose payload contains a full EOI pair.
        let mut with_app1 = vec![0xFF, 0xD8];
        with_app1.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x06, 0xFF, 0xD8, 0xFF, 0xD9]);
        with_app1.extend_from_slice(&intact[2..]); // rest of the real stream
        assert!(scan_is_terminated(&with_app1), "still intact overall");
        let truncated = hostile::truncate_scan(&with_app1, 16);
        assert!(
            !scan_is_terminated(&truncated),
            "the APP1 thumbnail's EOI must not count for the main scan"
        );
    }
}
