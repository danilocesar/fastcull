//! JPEG header sniffing: signature check and SOF dimension extraction without
//! decoding. Reads at most `SNIFF_LIMIT` bytes from the stream. One marker
//! walker, [`Markers`], serves every header search here and the header-gap
//! pre-pass (raw-pipeline.md, "The decoder's complaints").

use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

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
    for segment in Markers::new(&head) {
        if segment.marker != 0xE1 {
            continue; // the walk itself ends at SOS / EOI: image data, no Exif APP1
        }
        // APP1: its payload starts after the 2-byte length. The segment may
        // run past the 64 KB head (an APP1 is up to 64 KB of its own); only
        // its first bytes are read here, so that is fine.
        let payload = segment.at + 4;
        if payload + 6 <= head.len() && &head[payload..payload + 6] == b"Exif\0\0" {
            let tiff = payload + 6;
            let tiff_len = segment.len.saturating_sub(2 + 6);
            if tiff_len >= 8 {
                return Ok(Some((tiff as u64, tiff_len as u64)));
            }
            return Ok(None);
        }
    }
    Ok(None)
}

/// One header segment the [`Markers`] walk found: its marker code, the
/// offset of the FF that introduces it, and its declared length — the
/// 2-byte field, which counts itself — so the segment spans `at .. at + 2 +
/// len`. That span may run past the data: the walk yields such a segment
/// and ends only when asked to step past it, so a caller that needs the
/// whole segment checks [`Segment::end`] itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    marker: u8,
    at: usize,
    len: usize,
}

impl Segment {
    /// One past the segment's last byte.
    fn end(&self) -> usize {
        self.at + 2 + self.len
    }
}

/// THE JPEG marker walker (raw-pipeline.md, "Header gaps are skipped before
/// any decode"): the one walk behind the SOF sniff, the byte check's SOS
/// search, the APP1 Exif search and the header-gap pre-pass. It steps from
/// SOI through the header segments the way libjpeg's `next_marker` does
/// (`jdmarker.c`): wherever a segment should start, every byte that is not
/// FF, and every FF 00 pair, is a GAP byte — skipped and its range
/// remembered, where a walk that stopped there would call the stream
/// desynchronized; FF fill bytes before a marker are legal and kept. Then
/// the marker: a second SOI ends the walk with nothing found, as it ends
/// libjpeg's (`JERR_SOI_DUPLICATE`), so a walk that resyncs into an
/// embedded thumbnail stops at its SOI instead of taking the thumbnail's
/// markers for the main image's; 01 and D0–D7 carry no payload; EOI ends
/// the walk; any other reads its 2-byte length, which must be at least 2.
/// SOS is the last segment yielded — the entropy-coded data follows it.
/// Allocates nothing but the gap list, and that only when there are gaps.
struct Markers<'a> {
    data: &'a [u8],
    pos: usize,
    done: bool,
    /// The gap ranges skipped so far, in order, adjacent ones merged.
    gaps: Vec<Range<usize>>,
}

impl<'a> Markers<'a> {
    /// A walk of `data`, or an empty one when it does not start with SOI.
    fn new(data: &'a [u8]) -> Self {
        let has_soi = data.len() >= 2 && data[0] == 0xFF && data[1] == 0xD8;
        Markers {
            data,
            pos: 2,
            done: !has_soi,
            gaps: Vec::new(),
        }
    }

    fn skip_gap(&mut self, range: Range<usize>) {
        match self.gaps.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => self.gaps.push(range.clone()),
        }
        self.pos = range.end;
    }

    /// How many gap bytes the walk has skipped so far.
    fn gap_bytes(&self) -> usize {
        self.gaps.iter().map(|g| g.len()).sum()
    }
}

impl Iterator for Markers<'_> {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        if self.done {
            return None;
        }
        let data = self.data;
        loop {
            let p = self.pos;
            if p >= data.len() {
                self.done = true;
                return None;
            }
            if data[p] != 0xFF {
                self.skip_gap(p..p + 1);
                continue;
            }
            // An FF: skip the fill FFs to the code that follows.
            let mut code_at = p + 1;
            while code_at < data.len() && data[code_at] == 0xFF {
                code_at += 1;
            }
            if code_at >= data.len() {
                self.done = true;
                return None;
            }
            let code = data[code_at];
            if code == 0x00 {
                // A stuffed FF 00 (any fill before it included): no marker.
                self.skip_gap(p..code_at + 1);
                continue;
            }
            let at = code_at - 1; // the FF that introduces the marker
            match code {
                // A second SOI, or EOI before any SOS: nothing more to find.
                0xD8 | 0xD9 => {
                    self.done = true;
                    return None;
                }
                // TEM and RST0-7 carry no payload.
                0x01 | 0xD0..=0xD7 => {
                    self.pos = code_at + 1;
                    continue;
                }
                _ => {}
            }
            if at + 4 > data.len() {
                self.done = true;
                return None;
            }
            let len = usize::from(u16::from_be_bytes([data[at + 2], data[at + 3]]));
            if len < 2 {
                self.done = true;
                return None;
            }
            let segment = Segment {
                marker: code,
                at,
                len,
            };
            if code == 0xDA {
                self.done = true; // entropy-coded data follows the SOS header
            } else {
                self.pos = segment.end();
            }
            return Some(segment);
        }
    }
}

/// The stream without the gap bytes the marker walk skips between header
/// segments before the first SOS (raw-pipeline.md, "Header gaps are skipped
/// before any decode"), and how many bytes that removed. A stream with no
/// gap comes back BORROWED, so the A1 path copies nothing; one with a gap
/// is copied once, without its gaps — bounded by `MAX_EMBEDDED_JPEG_LEN`,
/// what `read_jpeg` will read. Everything from the first SOS on is
/// untouched, and a stream with no SOS is returned as given (the decoders
/// refuse it; the byte check names it).
pub(crate) fn without_header_gaps(data: &[u8]) -> (Cow<'_, [u8]>, usize) {
    let mut walk = Markers::new(data);
    let reached_sos = walk
        .by_ref()
        .any(|segment| segment.marker == 0xDA && segment.end() <= data.len());
    let removed = walk.gap_bytes();
    if !reached_sos || removed == 0 {
        return (Cow::Borrowed(data), 0);
    }
    let mut out = Vec::with_capacity(data.len() - removed);
    let mut from = 0;
    for gap in &walk.gaps {
        out.extend_from_slice(&data[from..gap.start]);
        from = gap.end;
    }
    out.extend_from_slice(&data[from..]);
    (Cow::Owned(out), removed)
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
/// entropy-coded data begins), or `None` if the marker walk reaches no
/// whole SOS header — header gaps skipped, as libjpeg skips them.
fn first_sos_end(data: &[u8]) -> Option<usize> {
    Markers::new(data)
        .find(|segment| segment.marker == 0xDA)
        .map(|segment| segment.end())
        .filter(|end| *end <= data.len())
}

/// The (width, height) a JPEG stream held in memory declares in its own SOF —
/// the size the decoder scales, where an IFD's claim is only what the
/// container says (raw-pipeline.md, "The factor rule": the loupe plans its
/// screen rung from this; QE 2026-09-28, D2). `None` when the marker walk
/// reaches no SOF that libjpeg's checks would accept. The walk stops at the
/// first SOS, so a whole stream costs no more than its headers.
pub(crate) fn sof_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    parse_sof(data)
}

/// Scan JPEG segments for SOF0–SOF15 (excluding DHT/JPG/DAC markers) and
/// return (width, height) — only from a SOF that passes the checks libjpeg's
/// `get_sof` makes (`jdmarker.c`): a non-zero height, width and component
/// count, and a length of exactly 8 + 3 × the component count. Anything else
/// is a stream libjpeg refuses (`JERR_EMPTY_IMAGE`, `JERR_BAD_LENGTH`), and
/// the check is what bounds the walk's resync: a walk through junk must not
/// size a stray FF Cx.
fn parse_sof(data: &[u8]) -> Option<(u32, u32)> {
    let segment = Markers::new(data).find(|segment| {
        matches!(segment.marker, 0xC0..=0xCF) && !matches!(segment.marker, 0xC4 | 0xC8 | 0xCC)
    })?;
    // Payload after the length: precision(1) height(2) width(2) count(1),
    // then 3 bytes per component.
    if segment.end() > data.len() || segment.len < 8 {
        return None;
    }
    let p = segment.at + 4;
    let height = u32::from(u16::from_be_bytes([data[p + 1], data[p + 2]]));
    let width = u32::from(u16::from_be_bytes([data[p + 3], data[p + 4]]));
    let components = usize::from(data[p + 5]);
    let sane = width > 0 && height > 0 && components > 0 && segment.len == 8 + 3 * components;
    sane.then_some((width, height))
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

    /// [`encoded`] in any of `jpeg_encoder`'s colour types, mid-grey in
    /// every component. For `Cmyk` and `CmykAsYcck` the encoder writes four
    /// components behind an Adobe APP14 whose transform (0 or 2) makes a
    /// decoder read the stream as CMYK or YCCK -- the print-ready bare JPEGs
    /// of brief 008 R14, which libjpeg-turbo will not convert to RGB.
    pub(crate) fn encoded_as(w: u16, h: u16, color: jpeg_encoder::ColorType) -> Vec<u8> {
        use jpeg_encoder::ColorType;
        let components = match color {
            ColorType::Luma => 1,
            ColorType::Rgb | ColorType::Bgr | ColorType::Ycbcr => 3,
            ColorType::Rgba
            | ColorType::Bgra
            | ColorType::Cmyk
            | ColorType::CmykAsYcck
            | ColorType::Ycck => 4,
        };
        let mut out = Vec::new();
        jpeg_encoder::Encoder::new(&mut out, 90)
            .encode(
                &vec![128u8; usize::from(w) * usize::from(h) * components],
                w,
                h,
                color,
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

    /// A VALID 8x8 grey progressive JPEG (SOF2) of exactly `scans` scans,
    /// 64..=127 — the scan-limit shape (brief 008 R2; raw-pipeline.md,
    /// "Progressive scans: at most 100"). One block, every coefficient zero,
    /// and one-code Huffman tables whose single code, the bit `0`, is the
    /// DC's "no difference" and the AC's end-of-band. A DC scan plus one
    /// scan per AC coefficient make 64; each scan past that splits one more
    /// AC coefficient into a first pass at Al = 1 and a refinement at
    /// Ah = 1 / Al = 0 — a legal progression, so libjpeg-turbo decodes it
    /// without a warning (a bogus one would warn, and a warning fails the
    /// loupe's decode for a reason that is not the limit). Each scan's data
    /// is one byte, the code bit padded with ones, so no 0xFF ever appears
    /// outside a marker and the SOS markers can be counted byte-wise.
    pub(crate) fn progressive(scans: usize) -> Vec<u8> {
        assert!(
            (64..=127).contains(&scans),
            "this shape carries 64..=127 scans, not {scans}"
        );
        fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
            let len = u16::try_from(payload.len() + 2).expect("a tiny segment");
            out.extend_from_slice(&[0xFF, marker]);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(payload);
        }
        fn scan(out: &mut Vec<u8>, coefficient: u8, ah: u8, al: u8) {
            // One component (id 1, Huffman tables 0 / 0), the spectral band
            // `coefficient..=coefficient`, successive approximation ah / al.
            segment(
                out,
                0xDA,
                &[1, 1, 0x00, coefficient, coefficient, (ah << 4) | al],
            );
            out.push(0x7F);
        }
        let mut out = vec![0xFF, 0xD8];
        let mut dqt = vec![0x00]; // 8-bit quantisation table 0, all ones
        dqt.extend_from_slice(&[1; 64]);
        segment(&mut out, 0xDB, &dqt);
        // SOF2: 8-bit samples, 8x8, one component (id 1, 1x1, table 0).
        segment(&mut out, 0xC2, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]);
        // Sixteen code-length counts (one code of length 1), then its symbol,
        // 0: for the DC table 0 (class 0x00) and the AC table 0 (0x10).
        for class_and_id in [0x00, 0x10] {
            let mut dht = vec![class_and_id, 1];
            dht.extend_from_slice(&[0; 15]);
            dht.push(0);
            segment(&mut out, 0xC4, &dht);
        }
        scan(&mut out, 0, 0, 0); // the DC, in one pass
        let refined = scans - 64;
        for coefficient in 1..=63u8 {
            if usize::from(coefficient) <= refined {
                scan(&mut out, coefficient, 0, 1);
                scan(&mut out, coefficient, 1, 0);
            } else {
                scan(&mut out, coefficient, 0, 0);
            }
        }
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    /// THE BASE FIXTURE of the other-cameras tests (brief 008; raw-pipeline.md,
    /// "The decoder's complaints"): `turbojpeg`'s 1024x768 Mandelbrot
    /// image compressed by libjpeg-turbo 3.1.0 at quality 90 — baseline,
    /// one scan, no restart markers, its JFIF APP0 the compressor's. The
    /// stream the other-cameras probe measured, where 1 to 7 junk bytes
    /// before EOI raise no message at all: a different image moves where
    /// the Huffman decoder's bit buffer stops, so the no-message rows hold
    /// on this fixture, not on any stream.
    pub(crate) fn mandelbrot_baseline() -> Vec<u8> {
        mandelbrot_turbo(false)
    }

    /// [`mandelbrot_baseline`] compressed progressive (`set_progressive`).
    pub(crate) fn mandelbrot_progressive() -> Vec<u8> {
        mandelbrot_turbo(true)
    }

    fn mandelbrot_turbo(progressive: bool) -> Vec<u8> {
        let img = turbojpeg::Image::mandelbrot(1024, 768, turbojpeg::PixelFormat::RGB);
        let mut c = turbojpeg::Compressor::new().expect("a compressor");
        c.set_quality(90).expect("quality 90");
        c.set_progressive(progressive).expect("progressive");
        c.compress_to_vec(img.as_deref()).expect("the base encodes")
    }

    /// The same pixels written by `jpeg_encoder` at quality 90 with a restart
    /// marker every 4 MCUs: the base with restart intervals (RST0 first).
    pub(crate) fn mandelbrot_restart() -> Vec<u8> {
        let img = turbojpeg::Image::mandelbrot(1024, 768, turbojpeg::PixelFormat::RGB);
        let mut out = Vec::new();
        let mut encoder = jpeg_encoder::Encoder::new(&mut out, 90);
        encoder.set_restart_interval(4);
        encoder
            .encode(&img.pixels, 1024, 768, jpeg_encoder::ColorType::Rgb)
            .expect("the restart base encodes");
        out
    }

    /// An 8x8 grey progressive JPEG of six scans, every coefficient zero,
    /// one-code Huffman tables (the shape of [`progressive`]): the DC in
    /// two passes (Al = 1, then its refinement), then two AC bands, 1..=5 and
    /// 6..=63, each a first scan at Al = 1 and its refinement. With `bogus`
    /// the first AC band's refinement comes BEFORE its first scan:
    /// libjpeg-turbo warns `JWRN_BOGUS_PROGRESSION` ("Inconsistent
    /// progression sequence") and decodes on — an inter-scan inconsistency
    /// it treats as a warning.
    pub(crate) fn progressive_six_scans(bogus: bool) -> Vec<u8> {
        fn segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
            let len = u16::try_from(payload.len() + 2).expect("a tiny segment");
            out.extend_from_slice(&[0xFF, marker]);
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(payload);
        }
        fn scan(out: &mut Vec<u8>, ss: u8, se: u8, ah: u8, al: u8) {
            segment(out, 0xDA, &[1, 1, 0x00, ss, se, (ah << 4) | al]);
            out.push(0x7F);
        }
        let mut out = vec![0xFF, 0xD8];
        let mut dqt = vec![0x00];
        dqt.extend_from_slice(&[1; 64]);
        segment(&mut out, 0xDB, &dqt);
        segment(&mut out, 0xC2, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]);
        for class_and_id in [0x00, 0x10] {
            let mut dht = vec![class_and_id, 1];
            dht.extend_from_slice(&[0; 15]);
            dht.push(0);
            segment(&mut out, 0xC4, &dht);
        }
        scan(&mut out, 0, 0, 0, 1);
        scan(&mut out, 0, 0, 1, 0);
        for (band, (ss, se)) in [(1u8, 5u8), (6, 63)].into_iter().enumerate() {
            if bogus && band == 0 {
                scan(&mut out, ss, se, 1, 0);
                scan(&mut out, ss, se, 0, 1);
            } else {
                scan(&mut out, ss, se, 0, 1);
                scan(&mut out, ss, se, 1, 0);
            }
        }
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    /// Offset of the first occurrence of `needle`.
    pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Offset of the occurrence of `needle` after `nth` earlier ones.
    pub(crate) fn find_nth(haystack: &[u8], needle: &[u8], nth: usize) -> Option<usize> {
        let mut from = 0;
        let mut hit = None;
        for _ in 0..=nth {
            let at = find(&haystack[from..], needle)? + from;
            hit = Some(at);
            from = at + 1;
        }
        hit
    }

    /// `stream` with `bytes` inserted at `at`.
    pub(crate) fn insert(stream: &[u8], at: usize, bytes: &[u8]) -> Vec<u8> {
        let mut out = stream[..at].to_vec();
        out.extend_from_slice(bytes);
        out.extend_from_slice(&stream[at..]);
        out
    }

    /// `stream` with `bytes` inserted right before its final EOI.
    pub(crate) fn before_eoi(stream: &[u8], bytes: &[u8]) -> Vec<u8> {
        assert_eq!(&stream[stream.len() - 2..], &[0xFF, 0xD9], "ends with EOI");
        insert(stream, stream.len() - 2, bytes)
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

    /// Brief 008, other cameras (raw-pipeline.md, "Header gaps are skipped
    /// before any decode"): bytes that are not a marker between two header
    /// segments — a segment whose declared length falls short, a writer's
    /// padding — are what libjpeg's `next_marker` skips with a warning. The
    /// one marker walker skips them too, where it used to call the stream
    /// desynchronized: the byte check then called a gapped stream
    /// "truncated", and the SOF sniff and the Exif search found nothing.
    /// Every walker is asked here, and the pre-pass that copies a stream
    /// without its gaps.
    ///
    /// Two bounds on the resync are pinned too. A second SOI ends the walk,
    /// as it ends libjpeg's: an APP1 declared short over an embedded
    /// thumbnail otherwise resyncs INTO the thumbnail, takes its SOS for
    /// the main image's, and its EOI then vouches for a main scan cut before
    /// its own. And the sniff sizes only a SOF that passes libjpeg's
    /// `get_sof` checks, so a walk through junk never sizes a stray FF Cx.
    #[test]
    fn a_header_gap_is_skipped_by_every_marker_walker() {
        use std::borrow::Cow;
        let base = hostile::mandelbrot_baseline();
        let dqt = hostile::find(&base, &[0xFF, 0xDB]).expect("the base has a DQT");
        let gapped = hostile::insert(&base, dqt, &[0x01, 0x02, 0x03]);
        // The byte check's SOS search, three bytes further on.
        assert_eq!(
            first_sos_end(&gapped),
            first_sos_end(&base).map(|end| end + 3),
            "the SOS is found past the gap"
        );
        assert!(scan_is_terminated(&gapped), "a gap is not a truncation");
        // The pre-pass: the gap-free stream, byte for byte, in one copy.
        let (clean, removed) = without_header_gaps(&gapped);
        assert!(matches!(clean, Cow::Owned(_)), "a gapped stream is copied");
        assert_eq!(removed, 3);
        assert!(
            clean[..] == base[..],
            "without its gap the stream is the base, byte for byte"
        );
        // A gap-free stream is not copied.
        let (same, removed) = without_header_gaps(&base);
        assert!(matches!(same, Cow::Borrowed(_)), "no gap, no copy");
        assert_eq!(removed, 0);
        // A gap before the SOF (the DQT's gap is one too): the sniff sizes it.
        assert_eq!(parse_sof(&gapped), Some((1024, 768)));
        let sof = hostile::find(&base, &[0xFF, 0xC0]).expect("the base has a SOF0");
        assert_eq!(
            parse_sof(&hostile::insert(&base, sof, &[0x01, 0x02, 0x03])),
            Some((1024, 768)),
            "a gap right before the SOF"
        );
        // A gap right after SOI, before an Exif APP1: the Exif search finds
        // the TIFF block at 15, 8 bytes long (SOI 2 + gap 3 + FF E1 and its
        // length 4 + "Exif\0\0" 6; the APP1's length 16 less its own 2 and
        // "Exif\0\0").
        let mut exif = vec![0xFF, 0xD8, 0x11, 0x22, 0x33, 0xFF, 0xE1, 0x00, 0x10];
        exif.extend_from_slice(b"Exif\0\0");
        exif.extend_from_slice(b"II*\0\x08\0\0\0");
        exif.extend_from_slice(&[0xFF, 0xD9]);
        assert_eq!(
            app1_tiff_bounds(&mut Cursor::new(exif)).unwrap(),
            Some((15, 8))
        );
        // A stuffed FF 00 between two segments is two gap bytes; FF fill
        // bytes before a marker are legal and stay.
        let (_, removed) = without_header_gaps(&hostile::insert(&base, dqt, &[0xFF, 0x00]));
        assert_eq!(removed, 2, "an FF 00 pair counts two");
        let filled = hostile::insert(&base, dqt, &[0xFF, 0xFF]);
        let (fill, removed) = without_header_gaps(&filled);
        assert_eq!(removed, 0, "fill bytes are not a gap");
        assert!(matches!(fill, Cow::Borrowed(_)));
        // A second SOI ends the walk.
        assert_eq!(
            first_sos_end(&hostile::insert(&base, 2, &[0xFF, 0xD8])),
            None,
            "two SOI markers: nothing to find"
        );

        // THE VOUCHING ROW: an APP1 whose declared length ends right after
        // "Exif\0\0"; the rest of its Exif block (no FF byte in it), then
        // a whole embedded thumbnail, SOI to EOI; then the main image with
        // its scan cut before EOI. The walk resyncs past the short APP1
        // into the thumbnail's SOI and must stop there — or the thumbnail's
        // EOI vouches for the cut main scan.
        let thumbnail = hostile::encoded(16, 16);
        let main_cut = hostile::truncate_scan(&hostile::encoded(64, 64), 16);
        let mut vouching = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x08];
        vouching.extend_from_slice(b"Exif\0\0");
        vouching.extend_from_slice(b"II*\0\x08\0\0\0");
        vouching.extend_from_slice(&thumbnail);
        vouching.extend_from_slice(&main_cut[2..]);
        assert!(
            !scan_is_terminated(&vouching),
            "an embedded thumbnail's EOI must not vouch for a main scan cut before its own"
        );

        // THE SOF CHECK: a one-component SOF2 (`progressive_sof2_is_found`'s,
        // 720x1080) with room after it for a longer declared length.
        let sof2 = |len: u16, components: u8| {
            let mut j = vec![0xFF, 0xD8, 0xFF, 0xC2];
            j.extend_from_slice(&len.to_be_bytes());
            j.push(0x08);
            j.extend_from_slice(&720u16.to_be_bytes());
            j.extend_from_slice(&1080u16.to_be_bytes());
            j.extend_from_slice(&[components, 0x11, 0x00, 0x00]);
            j.extend_from_slice(&[0x00; 8]);
            j.extend_from_slice(&[0xFF, 0xD9]);
            j
        };
        assert_eq!(parse_sof(&sof2(11, 1)), Some((1080, 720)), "the control");
        assert_eq!(
            parse_sof(&sof2(14, 1)),
            None,
            "a length that is not 8 + 3 x the component count"
        );
        assert_eq!(parse_sof(&sof2(11, 0)), None, "zero components");
        // A stray FF C0 in the junk ahead of the real SOF, one component and
        // a length of 14: libjpeg refuses the stream there, and the sniff
        // must not size it as 200x100.
        let mut stray = vec![0x11, 0x22, 0xFF, 0xC0, 0x00, 0x0E, 0x08];
        stray.extend_from_slice(&100u16.to_be_bytes());
        stray.extend_from_slice(&200u16.to_be_bytes());
        stray.extend_from_slice(&[0x01, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(
            parse_sof(&hostile::insert(&base, sof, &stray)),
            None,
            "a stray SOF in the junk is never sized"
        );
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
