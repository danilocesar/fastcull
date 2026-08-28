//! The QuickTime container this app writes — a hand-rolled, dependency-
//! free Motion JPEG muxer and the reader that proves what it wrote
//! (specs/modules/video-export.md, "The container").
//!
//! Why in-tree: no crate on crates.io writes a `jpeg` sample entry (the
//! Motion JPEG description QuickTime and every phone editor expect), the
//! atom set needed for one video track is small and fixed, and the file
//! is the one irreversible artefact this feature produces — so it is
//! golden-file tested like the XMP serializer rather than trusted to a
//! dependency.
//!
//! # The layout
//!
//! ```text
//! ftyp                          major brand `qt  `
//! moov                          BEFORE the payload, so the file plays
//!  └ mvhd                       while it is still being transferred
//!  └ trak
//!     └ tkhd                    display size + the rotation matrix
//!     └ mdia
//!        └ mdhd                 timescale 1000 (milliseconds)
//!        └ hdlr  `vide`
//!        └ minf
//!           └ vmhd
//!           └ hdlr  `url `
//!           └ dinf/dref         self-contained: the samples are in here
//!           └ stbl
//!              └ stsd  `jpeg`   one sample description, Motion JPEG
//!              └ stts           one entry: every frame is on screen for
//!              └ stsc           the same time; one sample per chunk
//!              └ stsz           per-sample sizes
//!              └ co64           64-BIT offsets, always
//! mdat                          the untouched camera JPEGs, back to back
//! ```
//!
//! Two choices are load-bearing and never conditional. **`moov` first**
//! is possible only because every sample size is known from the plan
//! before a byte is written; it is what makes a half-transferred file
//! playable. **`co64` always** (rather than `stco` when the file happens
//! to be small) means the offset table has ONE shape: a 400-frame
//! selection of A1 frames is 4.4 GB, so the 32-bit table would be the
//! rare case, and a rare case is the one that ships broken.
//!
//! Everything else follows what ffmpeg's `-c:v copy` produces from a JPEG
//! sequence, because that file is the one the user's phone editor was
//! tested against; `edts`, `udta` and the `wide` placeholder are dropped
//! (they carry no information this export has).

/// Movie and media timescale: 1000 ticks per second, so a duration in
/// ticks IS a duration in milliseconds — the unit the frames' own
/// `SubSecTimeOriginal` timestamps arrive in.
pub const TIMESCALE: u32 = 1000;

// --- fixed atom sizes ------------------------------------------------------
//
// Every atom in the header except `stsz` and `co64` has a size that does
// not depend on the number of frames, so the whole header can be measured
// before it is built. `header_len` below is what the plan quotes as the
// finished file's size, and `write_header` must agree with it to the
// byte — `header_len_matches_the_writer` holds them together.

const FTYP: u64 = 20;
const MVHD: u64 = 108;
const TKHD: u64 = 92;
const MDHD: u64 = 32;
/// `hdlr` for the media handler, whose name is the counted string
/// "VideoHandler".
const HDLR_MEDIA: u64 = 45;
/// `hdlr` for the data handler, name "DataHandler".
const HDLR_DATA: u64 = 44;
const VMHD: u64 = 20;
const DREF: u64 = 28;
const DINF: u64 = 8 + DREF;
const STSD: u64 = 102;
const STTS: u64 = 24;
const STSC: u64 = 28;
/// `stsz` and `co64` without their per-sample rows.
const STSZ_FIXED: u64 = 20;
const CO64_FIXED: u64 = 16;

/// An atom header is 8 bytes; `mdat` grows to 16 when the payload needs a
/// 64-bit size (the `largesize` form).
const ATOM: u64 = 8;
const MDAT_LARGE: u64 = 16;

/// The description of the one video track this module writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackSpec {
    /// Frame size, in pixels, of every sample. Not the DISPLAY size: a
    /// portrait frame is stored in sensor orientation and turned by the
    /// matrix (see [`display_matrix`]).
    pub width: u32,
    pub height: u32,
    /// EXIF orientation, already reduced to an unmirrored rotation
    /// (1, 3, 6, 8) by `clip::unmirrored`.
    pub orientation: u16,
    /// How long each frame is on screen, in milliseconds.
    pub sample_ms: u32,
    /// Every sample's size in bytes, in playback order.
    pub sample_sizes: Vec<u64>,
}

impl TrackSpec {
    pub fn sample_bytes(&self) -> u64 {
        self.sample_sizes.iter().sum()
    }

    pub fn duration_ms(&self) -> u64 {
        self.sample_sizes.len() as u64 * u64::from(self.sample_ms)
    }
}

/// Does this payload need `mdat`'s 64-bit `largesize` header?
///
/// The 32-bit header is what ffmpeg writes and what every tool has read
/// for thirty years, so it stays the normal case; the large form appears
/// only when the payload genuinely cannot be described in 32 bits. Both
/// are decided from sizes the plan already knows, so the offsets in
/// `co64` are exact either way.
fn mdat_header_len(payload: u64) -> u64 {
    if payload + ATOM > u64::from(u32::MAX) {
        MDAT_LARGE
    } else {
        ATOM
    }
}

/// Bytes before the first sample: `ftyp` + `moov` + the `mdat` header.
///
/// This is the "header allowance" the plan adds to the sum of the JPEG
/// lengths, so the size the dialog quotes is the size the file ends up
/// having — exactly, not approximately.
pub fn header_len(samples: usize, payload: u64) -> u64 {
    let n = samples as u64;
    let stbl = ATOM + STSD + STTS + STSC + (STSZ_FIXED + 4 * n) + (CO64_FIXED + 8 * n);
    let minf = ATOM + VMHD + HDLR_DATA + DINF + stbl;
    let mdia = ATOM + MDHD + HDLR_MEDIA + minf;
    let trak = ATOM + TKHD + mdia;
    let moov = ATOM + MVHD + trak;
    FTYP + moov + mdat_header_len(payload)
}

/// Where each sample lands in the finished file, in playback order.
pub fn sample_offsets(spec: &TrackSpec) -> Vec<u64> {
    let mut at = header_len(spec.sample_sizes.len(), spec.sample_bytes());
    spec.sample_sizes
        .iter()
        .map(|len| {
            let here = at;
            at += len;
            here
        })
        .collect()
}

/// The `tkhd` display matrix for an (unmirrored) EXIF orientation.
///
/// Pixels are never rotated — that would be a re-encode — so the
/// rotation lives here, exactly as a phone records portrait video. The
/// nine values are QuickTime's 3x3 transform: `a b u / c d v / x y w`,
/// with a, b, c, d, x, y in 16.16 fixed point and u, v, w in 2.30.
///
/// The four matrices below are byte-for-byte what `ffmpeg
/// -display_rotation` writes, verified against rendered frames on
/// 2026-08-27: a marker image muxed with each matrix and decoded back
/// through ffmpeg's autorotate landed where the EXIF orientation says it
/// should. Translation terms stay zero, which is also what ffmpeg does —
/// every reader that honours the matrix at all (ffmpeg, Android's
/// extractor) derives the rotation from the 2x2 part alone.
pub fn display_matrix(orientation: u16) -> [i32; 9] {
    const ONE: i32 = 0x0001_0000; // 1.0 in 16.16
    const W: i32 = 0x4000_0000; // 1.0 in 2.30
    match orientation {
        // 180°.
        3 => [-ONE, 0, 0, 0, -ONE, 0, 0, 0, W],
        // 90° clockwise (EXIF 6: the sensor's top edge is on the right).
        6 => [0, ONE, 0, -ONE, 0, 0, 0, 0, W],
        // 270° clockwise (EXIF 8).
        8 => [0, -ONE, 0, ONE, 0, 0, 0, 0, W],
        // 1, and anything unrecognised: as stored.
        _ => [ONE, 0, 0, 0, ONE, 0, 0, 0, W],
    }
}
