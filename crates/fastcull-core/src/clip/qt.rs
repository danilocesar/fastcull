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

// ------------------------------------------------------------------- writer

/// A growable buffer that knows how to close an atom over what was
/// written into it. Atoms nest, and every one of them starts with its own
/// total size — which is only known once its children are there — so the
/// size is written as a placeholder and patched when the atom closes.
struct Builder(Vec<u8>);

impl Builder {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn raw(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    fn zeros(&mut self, n: usize) {
        self.0.resize(self.0.len() + n, 0);
    }
    /// Version byte + 24 flag bits, the head of every "full" atom.
    fn full(&mut self, version: u8, flags: u32) {
        self.u32((u32::from(version) << 24) | (flags & 0x00ff_ffff));
    }
    /// A QuickTime counted (Pascal) string: one length byte, then the
    /// text. The two handler names are the only ones this module writes,
    /// and both are short — the length byte is a `u8` and nothing here
    /// approaches it.
    fn counted(&mut self, text: &str) {
        let bytes = text.as_bytes();
        self.0.push(bytes.len() as u8);
        self.raw(bytes);
    }
    fn atom(&mut self, kind: &[u8; 4], body: impl FnOnce(&mut Builder)) {
        let start = self.0.len();
        self.u32(0); // size, patched below
        self.raw(kind);
        body(self);
        let size = (self.0.len() - start) as u32;
        self.0[start..start + 4].copy_from_slice(&size.to_be_bytes());
    }
}

/// A duration that has to fit a 32-bit field. Saturating rather than
/// wrapping: an absurd duration should show as "very long", never as
/// "zero-length" — a wrapped 0 makes a file that some players refuse to
/// open at all. In practice this cannot trigger (a 4 GB export at 10 fps
/// is 400 s = 400,000 ticks).
fn ticks32(ms: u64) -> u32 {
    u32::try_from(ms).unwrap_or(u32::MAX)
}

/// A sample count in the 32-bit field the format gives it. Saturating
/// like [`ticks32`], and for the same reason: a selection of four billion
/// frames cannot exist, and a silent wrap would be the worst possible way
/// to find out otherwise.
fn sample_count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Write `ftyp`, the whole `moov`, and the `mdat` header — everything
/// before the first sample. The caller then writes the samples back to
/// back, in the order of `spec.sample_sizes`, and the file is complete.
pub fn write_header<W: std::io::Write>(w: &mut W, spec: &TrackSpec) -> std::io::Result<()> {
    let n = spec.sample_sizes.len();
    let payload = spec.sample_bytes();
    let duration = ticks32(spec.duration_ms());
    let offsets = sample_offsets(spec);
    let mut b = Builder(Vec::with_capacity(header_len(n, payload) as usize));

    b.atom(b"ftyp", |b| {
        b.raw(b"qt  "); // major brand
        b.u32(0x0000_0200); // minor version, as ffmpeg writes it
        b.raw(b"qt  "); // the one compatible brand
    });

    b.atom(b"moov", |b| {
        b.atom(b"mvhd", |b| {
            b.full(0, 0);
            b.u32(0); // creation time: not recorded (see the note below)
            b.u32(0); // modification time
            b.u32(TIMESCALE);
            b.u32(duration);
            b.u32(0x0001_0000); // rate 1.0
            b.u16(0x0100); // volume 1.0 (no audio track, but the field is)
            b.zeros(2 + 8); // reserved
            for v in display_matrix(1) {
                b.i32(v); // the MOVIE matrix stays identity; the track turns
            }
            b.zeros(24); // pre_defined
            b.u32(2); // next track id
        });
        b.atom(b"trak", |b| {
            b.atom(b"tkhd", |b| {
                b.full(0, 0x3); // enabled | in movie
                b.u32(0); // creation time
                b.u32(0); // modification time
                b.u32(1); // track id
                b.u32(0); // reserved
                b.u32(duration);
                b.zeros(8); // reserved
                b.u16(0); // layer
                b.u16(0); // alternate group
                b.u16(0); // volume: silent, this is video
                b.u16(0); // reserved
                for v in display_matrix(spec.orientation) {
                    b.i32(v);
                }
                b.u32(fixed16(spec.width));
                b.u32(fixed16(spec.height));
            });
            b.atom(b"mdia", |b| {
                b.atom(b"mdhd", |b| {
                    b.full(0, 0);
                    b.u32(0); // creation time
                    b.u32(0); // modification time
                    b.u32(TIMESCALE);
                    b.u32(duration);
                    b.u16(0x7fff); // language: unspecified
                    b.u16(0); // quality
                });
                b.atom(b"hdlr", |b| {
                    b.full(0, 0);
                    b.raw(b"mhlr"); // a media handler
                    b.raw(b"vide"); // ...for video
                    b.zeros(12); // manufacturer, flags, flags mask
                    b.counted("VideoHandler");
                });
                b.atom(b"minf", |b| {
                    b.atom(b"vmhd", |b| {
                        b.full(0, 1); // the flag QuickTime requires here
                        b.u16(0); // graphics mode: copy
                        b.zeros(6); // opcolor
                    });
                    b.atom(b"hdlr", |b| {
                        b.full(0, 0);
                        b.raw(b"dhlr"); // a data handler
                        b.raw(b"url ");
                        b.zeros(12);
                        b.counted("DataHandler");
                    });
                    b.atom(b"dinf", |b| {
                        b.atom(b"dref", |b| {
                            b.full(0, 0);
                            b.u32(1); // one data reference...
                            b.atom(b"url ", |b| b.full(0, 1)); // ...ourselves
                        });
                    });
                    b.atom(b"stbl", |b| {
                        b.atom(b"stsd", |b| {
                            b.full(0, 0);
                            b.u32(1); // one sample description
                            b.atom(b"jpeg", |b| {
                                b.zeros(6); // reserved
                                b.u16(1); // data reference index
                                b.u16(0); // version
                                b.u16(0); // revision
                                b.u32(0); // vendor
                                b.u32(512); // temporal quality
                                b.u32(512); // spatial quality
                                b.u16(short(spec.width));
                                b.u16(short(spec.height));
                                b.u32(0x0048_0000); // 72 dpi horizontal
                                b.u32(0x0048_0000); // 72 dpi vertical
                                b.u32(0); // data size
                                b.u16(1); // frames per sample
                                b.zeros(32); // compressor name: none
                                b.u16(24); // depth
                                b.u16(0xffff); // colour table: none (-1)
                            });
                        });
                        b.atom(b"stts", |b| {
                            b.full(0, 0);
                            b.u32(1); // ONE entry: every frame lasts the same
                            b.u32(sample_count(n));
                            b.u32(spec.sample_ms);
                        });
                        b.atom(b"stsc", |b| {
                            b.full(0, 0);
                            b.u32(1); // one entry: one sample per chunk...
                            b.u32(1); // ...starting at chunk 1
                            b.u32(1);
                            b.u32(1); // sample description index
                        });
                        b.atom(b"stsz", |b| {
                            b.full(0, 0);
                            b.u32(0); // 0 = sizes differ, they follow
                            b.u32(sample_count(n));
                            for len in &spec.sample_sizes {
                                b.u32(u32::try_from(*len).unwrap_or(u32::MAX));
                            }
                        });
                        b.atom(b"co64", |b| {
                            b.full(0, 0);
                            b.u32(sample_count(n));
                            for at in &offsets {
                                b.u64(*at);
                            }
                        });
                    });
                });
            });
        });
    });

    // `mdat`, whose payload the caller writes: the 32-bit header when the
    // payload fits it, the `largesize` form when it does not.
    if mdat_header_len(payload) == MDAT_LARGE {
        b.u32(1); // "the real size is 64 bits, after the type"
        b.raw(b"mdat");
        b.u64(payload + MDAT_LARGE);
    } else {
        b.u32((payload + ATOM) as u32);
        b.raw(b"mdat");
    }

    debug_assert_eq!(
        b.0.len() as u64,
        header_len(n, payload),
        "the quoted header size and the written header must agree"
    );
    w.write_all(&b.0)
}

/// Pixels as QuickTime's 16.16 fixed point, saturating at the 16-bit
/// integer part the format has room for.
fn fixed16(px: u32) -> u32 {
    u32::from(short(px)) << 16
}

fn short(px: u32) -> u16 {
    u16::try_from(px).unwrap_or(u16::MAX)
}

// ------------------------------------------------------------------- reader

/// A minimal reader for the files this module writes.
///
/// It exists to VERIFY, not to play: every export re-parses its own
/// finished file and checks that the index describes exactly the samples
/// that were written (video-export.md, "Verified"). It is also what the
/// tests assert against on a runner with no ffmpeg — which is every
/// Windows runner.
///
/// It is written to survive hostile input, because a test will feed it
/// some: nothing is allocated from a count the file claims until the file
/// is known to be long enough to hold that many entries, atom sizes below
/// their own header are rejected, and nesting is depth-limited.
#[derive(Debug, thiserror::Error)]
pub enum QtError {
    #[error("read error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a QuickTime movie: {0}")]
    Malformed(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub offset: u64,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct Movie {
    pub major_brand: [u8; 4],
    /// The media timescale, in ticks per second.
    pub timescale: u32,
    /// How long each sample is on screen, in media ticks (milliseconds,
    /// at this module's timescale).
    pub sample_ms: u32,
    /// How many entries the time-to-sample table has. One means a
    /// constant frame rate; this module never writes anything else.
    pub stts_entries: usize,
    /// The track's display size from `tkhd` (integer part).
    pub width: u32,
    pub height: u32,
    /// The stored frame size from the sample description.
    pub sample_width: u32,
    pub sample_height: u32,
    /// The `tkhd` display matrix.
    pub matrix: [i32; 9],
    /// The sample description's format — `jpeg` for Motion JPEG.
    pub format: [u8; 4],
    pub samples: Vec<Sample>,
    /// True when the offsets came from `co64` rather than `stco`.
    pub co64: bool,
    /// True when `moov` is ahead of `mdat`, i.e. the file plays while it
    /// is still being copied.
    pub moov_before_mdat: bool,
}

/// The biggest `moov` this reader will hold in memory. This module's own
/// is 607 + 12 bytes per frame — 5 KB for a 400-frame export — so the cap
/// is four orders of magnitude of headroom and still refuses a file whose
/// header claims to be a gigabyte.
const MAX_MOOV: u64 = 64 * 1024 * 1024;

/// How deep atoms may nest before the reader gives up. The layout this
/// module writes is six deep (moov/trak/mdia/minf/stbl/stsd/jpeg).
const MAX_DEPTH: usize = 16;

/// Cap on a UNIFORM sample-size table's claimed count — the one table
/// whose length the file does not bound (see `parse_stsz`).
const MAX_UNIFORM_SAMPLES: usize = 16 * 1024 * 1024;

/// Parse the header of a QuickTime movie.
pub fn read_movie<R: std::io::Read + std::io::Seek>(r: &mut R) -> Result<Movie, QtError> {
    let end = r.seek(std::io::SeekFrom::End(0))?;
    r.seek(std::io::SeekFrom::Start(0))?;
    let mut at = 0u64;
    let mut major_brand = [0u8; 4];
    let mut moov: Option<Vec<u8>> = None;
    let mut mdat_at: Option<u64> = None;
    let mut moov_at: Option<u64> = None;
    while at + 8 <= end {
        let (kind, size, body) = read_atom_header(r, at, end)?;
        match &kind {
            b"ftyp" => {
                let mut brand = [0u8; 4];
                if size >= 12 {
                    r.seek(std::io::SeekFrom::Start(body))?;
                    r.read_exact(&mut brand)?;
                    major_brand = brand;
                }
            }
            b"moov" => {
                let len = size - (body - at);
                if len > MAX_MOOV {
                    return Err(QtError::Malformed("moov is implausibly large"));
                }
                let mut buf = vec![0u8; len as usize];
                r.seek(std::io::SeekFrom::Start(body))?;
                r.read_exact(&mut buf)?;
                moov = Some(buf);
                moov_at = Some(at);
            }
            b"mdat" => mdat_at = Some(at),
            _ => {}
        }
        at += size;
    }
    let moov = moov.ok_or(QtError::Malformed("no moov"))?;
    let mut m = Parsed::default();
    walk(&moov, &mut m, 0)?;

    let samples = m.build_samples()?;
    Ok(Movie {
        major_brand,
        timescale: m.timescale.ok_or(QtError::Malformed("no mdhd"))?,
        sample_ms: m.sample_delta.ok_or(QtError::Malformed("no stts"))?,
        stts_entries: m.stts_entries,
        width: m.width,
        height: m.height,
        sample_width: m.sample_width,
        sample_height: m.sample_height,
        matrix: m.matrix,
        format: m
            .format
            .ok_or(QtError::Malformed("no sample description"))?,
        samples,
        co64: m.co64,
        moov_before_mdat: match (moov_at, mdat_at) {
            (Some(mo), Some(md)) => mo < md,
            _ => false,
        },
    })
}

/// Read one atom header at `at`, returning (type, total size, body offset).
fn read_atom_header<R: std::io::Read + std::io::Seek>(
    r: &mut R,
    at: u64,
    end: u64,
) -> Result<([u8; 4], u64, u64), QtError> {
    r.seek(std::io::SeekFrom::Start(at))?;
    let mut head = [0u8; 8];
    r.read_exact(&mut head)?;
    let mut kind = [0u8; 4];
    kind.copy_from_slice(&head[4..8]);
    let short_size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let (size, body) = match short_size {
        // The `largesize` form: a 64-bit size follows the type.
        1 => {
            let mut big = [0u8; 8];
            r.read_exact(&mut big)?;
            (u64::from_be_bytes(big), at + 16)
        }
        // "to the end of the file"
        0 => (end.saturating_sub(at), at + 8),
        n => (u64::from(n), at + 8),
    };
    if size < body - at || at + size > end {
        return Err(QtError::Malformed(
            "atom size runs past the end of the file",
        ));
    }
    Ok((kind, size, body))
}

#[derive(Default)]
struct Parsed {
    timescale: Option<u32>,
    sample_delta: Option<u32>,
    stts_entries: usize,
    width: u32,
    height: u32,
    sample_width: u32,
    sample_height: u32,
    matrix: [i32; 9],
    format: Option<[u8; 4]>,
    sizes: Vec<u64>,
    offsets: Vec<u64>,
    /// (first_chunk, samples_per_chunk) rows.
    stsc: Vec<(u32, u32)>,
    co64: bool,
}

impl Parsed {
    /// Turn the four tables into a flat sample list, in playback order.
    ///
    /// The general case is a walk over chunks; this module always writes
    /// one sample per chunk, but the reader does the real thing so that a
    /// file written differently is READ correctly rather than silently
    /// mis-verified.
    fn build_samples(&self) -> Result<Vec<Sample>, QtError> {
        if self.stsc.is_empty() || self.offsets.is_empty() {
            return Err(QtError::Malformed("no sample tables"));
        }
        let mut samples = Vec::with_capacity(self.sizes.len());
        let mut next = 0usize; // index into `sizes`
                               // The sample-to-chunk table is a RUN-LENGTH list: each row says
                               // "from this chunk on, N samples per chunk". Walking it from the
                               // start for every chunk would be quadratic, and both tables are
                               // bounded only by the size of a 64 MB `moov` — millions of rows
                               // times millions of chunks is a hang, not a parse. So the cursor
                               // only ever moves forward.
        let mut row = 0usize;
        for (chunk_index, chunk_offset) in self.offsets.iter().enumerate() {
            let chunk = chunk_index as u32 + 1;
            while self
                .stsc
                .get(row + 1)
                .is_some_and(|(first, _)| *first <= chunk)
            {
                row += 1;
            }
            let (first, per_chunk) = self.stsc[row];
            if first > chunk {
                return Err(QtError::Malformed("sample-to-chunk table starts late"));
            }
            let mut at = *chunk_offset;
            for _ in 0..per_chunk {
                let Some(size) = self.sizes.get(next) else {
                    return Err(QtError::Malformed(
                        "the chunk table describes more samples than the size table",
                    ));
                };
                samples.push(Sample {
                    offset: at,
                    size: *size,
                });
                at = at
                    .checked_add(*size)
                    .ok_or(QtError::Malformed("sample runs past 2^64"))?;
                next += 1;
            }
        }
        Ok(samples)
    }
}

/// Container atoms whose children have to be walked.
const CONTAINERS: [&[u8; 4]; 6] = [b"trak", b"mdia", b"minf", b"stbl", b"dinf", b"edts"];

fn walk(buf: &[u8], out: &mut Parsed, depth: usize) -> Result<(), QtError> {
    if depth > MAX_DEPTH {
        return Err(QtError::Malformed("atoms nested too deeply"));
    }
    let mut at = 0usize;
    while at + 8 <= buf.len() {
        let size = be32(buf, at)? as usize;
        let kind: [u8; 4] = [buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]];
        // Inside a moov every atom carries a real 32-bit size; the 0 and 1
        // forms are for top-level boxes and would make this loop spin.
        if size < 8 || at.checked_add(size).is_none_or(|end| end > buf.len()) {
            return Err(QtError::Malformed(
                "child atom size does not fit its parent",
            ));
        }
        let body = &buf[at + 8..at + size];
        if CONTAINERS.contains(&&kind) {
            walk(body, out, depth + 1)?;
        } else {
            match &kind {
                b"tkhd" => parse_tkhd(body, out)?,
                b"mdhd" => parse_mdhd(body, out)?,
                b"stsd" => parse_stsd(body, out)?,
                b"stts" => parse_stts(body, out)?,
                b"stsc" => parse_stsc(body, out)?,
                b"stsz" => parse_stsz(body, out)?,
                b"stco" => parse_offsets(body, out, 4)?,
                b"co64" => parse_offsets(body, out, 8)?,
                _ => {}
            }
        }
        at += size;
    }
    Ok(())
}

fn be32(buf: &[u8], at: usize) -> Result<u32, QtError> {
    let end = at
        .checked_add(4)
        .ok_or(QtError::Malformed("offset overflow"))?;
    let slice = buf
        .get(at..end)
        .ok_or(QtError::Malformed("atom ends mid-field"))?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn be64(buf: &[u8], at: usize) -> Result<u64, QtError> {
    let end = at
        .checked_add(8)
        .ok_or(QtError::Malformed("offset overflow"))?;
    let slice = buf
        .get(at..end)
        .ok_or(QtError::Malformed("atom ends mid-field"))?;
    let mut b = [0u8; 8];
    b.copy_from_slice(slice);
    Ok(u64::from_be_bytes(b))
}

fn be16(buf: &[u8], at: usize) -> Result<u16, QtError> {
    let end = at
        .checked_add(2)
        .ok_or(QtError::Malformed("offset overflow"))?;
    let slice = buf
        .get(at..end)
        .ok_or(QtError::Malformed("atom ends mid-field"))?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

/// How many entries of `each` bytes a table body can really hold. The
/// count in the file is a claim; this is the fact.
fn entry_count(body: &[u8], header: usize, each: usize, claimed: u32) -> Result<usize, QtError> {
    let room = body.len().saturating_sub(header) / each;
    let claimed = claimed as usize;
    if claimed > room {
        return Err(QtError::Malformed(
            "a sample table claims more entries than it contains",
        ));
    }
    Ok(claimed)
}

fn parse_tkhd(body: &[u8], out: &mut Parsed) -> Result<(), QtError> {
    // version 0 and version 1 differ only in the width of the four time
    // fields before the reserved block.
    let version = *body.first().ok_or(QtError::Malformed("empty tkhd"))?;
    let matrix_at = if version == 1 {
        4 + 32 + 16
    } else {
        4 + 20 + 16
    };
    for (i, slot) in out.matrix.iter_mut().enumerate() {
        *slot = be32(body, matrix_at + i * 4)? as i32;
    }
    out.width = be32(body, matrix_at + 36)? >> 16;
    out.height = be32(body, matrix_at + 40)? >> 16;
    Ok(())
}

fn parse_mdhd(body: &[u8], out: &mut Parsed) -> Result<(), QtError> {
    let version = *body.first().ok_or(QtError::Malformed("empty mdhd"))?;
    out.timescale = Some(if version == 1 {
        be32(body, 4 + 16)?
    } else {
        be32(body, 4 + 8)?
    });
    Ok(())
}

fn parse_stsd(body: &[u8], out: &mut Parsed) -> Result<(), QtError> {
    // full header (4) + entry count (4), then the first entry: size (4),
    // format (4), reserved (6), data reference index (2), and — for a
    // visual entry — 16 more bytes before the stored dimensions.
    if be32(body, 4)? == 0 {
        return Err(QtError::Malformed("no sample description entries"));
    }
    let entry = 8;
    let mut format = [0u8; 4];
    let f = body
        .get(entry + 4..entry + 8)
        .ok_or(QtError::Malformed("sample description ends early"))?;
    format.copy_from_slice(f);
    out.format = Some(format);
    out.sample_width = u32::from(be16(body, entry + 32)?);
    out.sample_height = u32::from(be16(body, entry + 34)?);
    Ok(())
}

fn parse_stts(body: &[u8], out: &mut Parsed) -> Result<(), QtError> {
    let count = entry_count(body, 8, 8, be32(body, 4)?)?;
    out.stts_entries = count;
    if count > 0 {
        out.sample_delta = Some(be32(body, 8 + 4)?);
    }
    Ok(())
}

fn parse_stsc(body: &[u8], out: &mut Parsed) -> Result<(), QtError> {
    let count = entry_count(body, 8, 12, be32(body, 4)?)?;
    out.stsc = Vec::with_capacity(count);
    for i in 0..count {
        let at = 8 + i * 12;
        out.stsc.push((be32(body, at)?, be32(body, at + 4)?));
    }
    Ok(())
}

fn parse_stsz(body: &[u8], out: &mut Parsed) -> Result<(), QtError> {
    let uniform = be32(body, 4)?;
    let claimed = be32(body, 8)?;
    if uniform != 0 {
        // Every sample the same size: NO table follows, so the atom's own
        // length says nothing about the count and it has to be bounded by
        // hand. `MAX_UNIFORM_SAMPLES` is four hours of 1000 fps video —
        // far past anything this app writes (it never writes a uniform
        // table at all), and small enough that the allocation below is
        // 128 MB rather than 34 GB.
        if claimed as usize > MAX_UNIFORM_SAMPLES {
            return Err(QtError::Malformed(
                "a uniform sample-size table claims an implausible number of samples",
            ));
        }
        out.sizes = vec![u64::from(uniform); claimed as usize];
        return Ok(());
    }
    let count = entry_count(body, 12, 4, claimed)?;
    out.sizes = Vec::with_capacity(count);
    for i in 0..count {
        out.sizes.push(u64::from(be32(body, 12 + i * 4)?));
    }
    Ok(())
}

fn parse_offsets(body: &[u8], out: &mut Parsed, each: usize) -> Result<(), QtError> {
    let count = entry_count(body, 8, each, be32(body, 4)?)?;
    out.co64 = each == 8;
    out.offsets = Vec::with_capacity(count);
    for i in 0..count {
        let at = 8 + i * each;
        out.offsets.push(if each == 8 {
            be64(body, at)?
        } else {
            u64::from(be32(body, at)?)
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// The three A1 reference frames the phone test was run on: the exact
    /// byte lengths of the full-res JPEGs embedded in
    /// `testdata/raws/A1_full_*.ARW`. Kept as constants so the golden
    /// below is hermetic — it pins the header on a runner that has no
    /// sample RAWs and no ffmpeg (i.e. every Windows runner) — while
    /// `clip_muxer.rs` proves the real files still have these sizes.
    pub(crate) const REFERENCE_SIZES: [u64; 3] = [12_313_510, 9_751_756, 12_284_420];
    pub(crate) const REFERENCE_W: u32 = 8640;
    pub(crate) const REFERENCE_H: u32 = 5760;
    /// 33 ms: the median gap of a 30 fps A1 burst.
    pub(crate) const REFERENCE_MS: u32 = 33;

    pub(crate) fn reference_spec() -> TrackSpec {
        TrackSpec {
            width: REFERENCE_W,
            height: REFERENCE_H,
            orientation: 1,
            sample_ms: REFERENCE_MS,
            sample_sizes: REFERENCE_SIZES.to_vec(),
        }
    }

    fn spec(sizes: &[u64], orientation: u16) -> TrackSpec {
        TrackSpec {
            width: 8640,
            height: 5760,
            orientation,
            sample_ms: 33,
            sample_sizes: sizes.to_vec(),
        }
    }

    /// A whole little movie in memory: header plus `sizes` bytes of
    /// stand-in sample data, so the reader has a real file to walk.
    fn movie_bytes(spec: &TrackSpec) -> Vec<u8> {
        let mut out = Vec::new();
        write_header(&mut out, spec).unwrap();
        for (i, len) in spec.sample_sizes.iter().enumerate() {
            out.extend(std::iter::repeat_n(i as u8, *len as usize));
        }
        out
    }

    /// The size the plan quotes and the size the writer produces are two
    /// separate pieces of arithmetic, and the free-space check and the
    /// `co64` offsets both trust the first one. If they ever disagree,
    /// every sample offset in the file is wrong by the difference.
    #[test]
    fn the_quoted_header_size_is_the_written_one() {
        for n in [2usize, 3, 17, 400] {
            let sizes: Vec<u64> = (0..n).map(|i| 1000 + i as u64).collect();
            let s = spec(&sizes, 1);
            let mut out = Vec::new();
            write_header(&mut out, &s).unwrap();
            assert_eq!(
                out.len() as u64,
                header_len(n, s.sample_bytes()),
                "{n} samples: the quoted header size is not the written one"
            );
            assert_eq!(sample_offsets(&s)[0], out.len() as u64);
        }
    }

    /// Everything the export promises about the container, asserted on
    /// the bytes: brand, `moov` before `mdat`, a `jpeg` sample entry, a
    /// millisecond timescale, ONE time-to-sample entry (constant frame
    /// rate), `co64`, and every sample where the plan put it.
    #[test]
    fn the_reader_confirms_what_the_writer_promised() {
        let s = spec(&[500, 640, 480], 1);
        let bytes = movie_bytes(&s);
        let m = read_movie(&mut Cursor::new(bytes.clone())).unwrap();
        assert_eq!(&m.major_brand, b"qt  ");
        assert!(m.moov_before_mdat, "the file must play while it copies");
        assert_eq!(&m.format, b"jpeg");
        assert_eq!(m.timescale, TIMESCALE);
        assert_eq!(m.sample_ms, 33);
        assert_eq!(m.stts_entries, 1, "constant frame rate, one entry");
        assert!(m.co64, "64-bit offsets always, never stco");
        assert_eq!((m.width, m.height), (8640, 5760));
        assert_eq!((m.sample_width, m.sample_height), (8640, 5760));
        assert_eq!(m.matrix, display_matrix(1));
        assert_eq!(
            m.samples,
            sample_offsets(&s)
                .into_iter()
                .zip(&s.sample_sizes)
                .map(|(offset, size)| Sample {
                    offset,
                    size: *size
                })
                .collect::<Vec<_>>()
        );
        // ...and the samples really are those bytes, at those offsets.
        for (i, sample) in m.samples.iter().enumerate() {
            let at = sample.offset as usize;
            let end = at + sample.size as usize;
            assert!(bytes[at..end].iter().all(|b| *b == i as u8), "sample {i}");
        }
    }

    /// A portrait frame keeps its pixels and turns the DISPLAY, which is
    /// what the track matrix is for. The four matrices are pinned as
    /// bytes because they are the whole feature for portrait bursts, and
    /// a sign flip in one of them is invisible until a phone plays the
    /// file sideways.
    #[test]
    fn portrait_frames_turn_the_display_not_the_pixels() {
        const ONE: i32 = 0x0001_0000;
        assert_eq!(display_matrix(1), [ONE, 0, 0, 0, ONE, 0, 0, 0, 0x4000_0000]);
        assert_eq!(display_matrix(3)[0], -ONE);
        assert_eq!(display_matrix(3)[4], -ONE);
        // EXIF 6 is a 90° clockwise turn; EXIF 8 the other way. The two
        // must not be the same matrix, and each must be the other's
        // transpose in the 2x2 block.
        assert_eq!([display_matrix(6)[1], display_matrix(6)[3]], [ONE, -ONE]);
        assert_eq!([display_matrix(8)[1], display_matrix(8)[3]], [-ONE, ONE]);
        // The stored frame size does NOT swap: the pixels are untouched.
        let s = spec(&[100, 100], 6);
        let m = read_movie(&mut Cursor::new(movie_bytes(&s))).unwrap();
        assert_eq!((m.width, m.height), (8640, 5760));
        assert_eq!(m.matrix, display_matrix(6));
    }

    /// `co64` is the whole reason a 400-frame selection works: past 4 GB
    /// a 32-bit offset table silently wraps, and the last frames of the
    /// file point back into its header.
    ///
    /// Proven on the arithmetic and on the BYTES, without writing 4 GB to
    /// anyone's disk: the offsets are computed for a 5 GB export and the
    /// `co64` entries are decoded straight out of the written header.
    #[test]
    fn offsets_past_four_gigabytes_are_written_as_64_bit() {
        // 40 frames of 128 MB: 5.1 GB, and every offset past the first
        // 32 is over the 32-bit ceiling.
        let sizes: Vec<u64> = vec![128 * 1024 * 1024; 40];
        let s = spec(&sizes, 1);
        let offsets = sample_offsets(&s);
        assert!(
            *offsets.last().unwrap() > u64::from(u32::MAX),
            "the fixture must actually cross 4 GB"
        );
        let mut header = Vec::new();
        write_header(&mut header, &s).unwrap();
        // The 64-bit `mdat` header is used, so the payload size is honest.
        let mdat = header.len() - 16;
        assert_eq!(&header[mdat + 4..mdat + 8], b"mdat");
        assert_eq!(
            &header[mdat..mdat + 4],
            &1u32.to_be_bytes(),
            "largesize form"
        );
        assert_eq!(
            u64::from_be_bytes(header[mdat + 8..mdat + 16].try_into().unwrap()),
            s.sample_bytes() + 16
        );
        // ...and the offset table itself.
        let at = find(&header, b"co64").expect("a co64 atom") + 4;
        assert_eq!(be32(&header, at + 4).unwrap() as usize, offsets.len());
        for (i, want) in offsets.iter().enumerate() {
            assert_eq!(be64(&header, at + 8 + i * 8).unwrap(), *want, "offset {i}");
        }
    }

    fn find(haystack: &[u8], needle: &[u8; 4]) -> Option<usize> {
        haystack.windows(4).position(|w| w == needle)
    }

    /// The reader is pointed at hostile bytes on purpose: it verifies
    /// this app's own output, but a corrupted file is exactly the case it
    /// exists to catch, and it must come back with an error rather than a
    /// panic or a 4 GB allocation.
    #[test]
    fn hostile_bytes_come_back_as_errors_not_panics() {
        let good = movie_bytes(&spec(&[64, 64], 1));
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("garbage", vec![0xAB; 4096]),
            ("header only, no samples", {
                let mut v = Vec::new();
                write_header(&mut v, &spec(&[64, 64], 1)).unwrap();
                v
            }),
            ("truncated mid-moov", good[..40].to_vec()),
            ("truncated mid-samples", good[..good.len() - 32].to_vec()),
            ("an atom claiming the whole address space", {
                let mut v = good.clone();
                v[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
                v
            }),
            ("a zero-sized child atom", {
                let mut v = good.clone();
                let at = find(&v, b"stts").unwrap() - 4;
                v[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
                v
            }),
            ("stsz claiming four billion samples", {
                let mut v = good.clone();
                let at = find(&v, b"stsz").unwrap() + 4;
                v[at + 8..at + 12].copy_from_slice(&u32::MAX.to_be_bytes());
                v
            }),
            ("co64 claiming four billion chunks", {
                let mut v = good.clone();
                let at = find(&v, b"co64").unwrap() + 4;
                v[at + 4..at + 8].copy_from_slice(&u32::MAX.to_be_bytes());
                v
            }),
            ("a uniform stsz with an absurd count", {
                let mut v = good.clone();
                let at = find(&v, b"stsz").unwrap() + 4;
                v[at + 4..at + 8].copy_from_slice(&64u32.to_be_bytes());
                v[at + 8..at + 12].copy_from_slice(&u32::MAX.to_be_bytes());
                v
            }),
        ];
        for (name, bytes) in cases {
            let result = read_movie(&mut Cursor::new(bytes));
            assert!(result.is_err(), "{name}: must be refused, got {result:?}");
        }
        // A no-moov file is an error, not a default-shaped Movie.
        let mut only_ftyp = Vec::new();
        write_header(&mut only_ftyp, &spec(&[8], 1)).unwrap();
        only_ftyp.truncate(20);
        assert!(read_movie(&mut Cursor::new(only_ftyp)).is_err());
    }

    /// The reader walks the chunk tables for real rather than assuming
    /// this module's one-sample-per-chunk layout — so a file written by
    /// something else is read correctly instead of being mis-verified.
    #[test]
    fn the_reader_walks_real_chunk_tables() {
        let mut p = Parsed {
            sizes: vec![10, 20, 30, 40, 50],
            offsets: vec![1000, 2000],
            stsc: vec![(1, 3), (2, 2)],
            ..Default::default()
        };
        assert_eq!(
            p.build_samples().unwrap(),
            vec![
                Sample {
                    offset: 1000,
                    size: 10
                },
                Sample {
                    offset: 1010,
                    size: 20
                },
                Sample {
                    offset: 1030,
                    size: 30
                },
                Sample {
                    offset: 2000,
                    size: 40
                },
                Sample {
                    offset: 2040,
                    size: 50
                },
            ]
        );
        // More samples claimed than the size table has: an error, never a
        // silent short read.
        p.sizes.pop();
        assert!(p.build_samples().is_err());
    }

    /// The sample-to-chunk table is a RUN-LENGTH list, and reading it
    /// wrong is silent: every sample lands at a plausible-looking wrong
    /// offset. This drives a table with a GAP in it (rows for chunk 1 and
    /// chunk 5, nothing between) and at a size where re-scanning the
    /// table for every chunk — which is what this reader used to do —
    /// stops being a parse and becomes a hang.
    #[test]
    fn the_chunk_table_is_read_forwards_only() {
        // A gap: chunks 1-4 hold 2 samples each, chunks 5-6 hold 3.
        let p = Parsed {
            sizes: (0..14).map(|i| 10 + i).collect(),
            offsets: (0..6).map(|c| 1000 * (c + 1)).collect(),
            stsc: vec![(1, 2), (5, 3)],
            ..Default::default()
        };
        let samples = p.build_samples().unwrap();
        assert_eq!(samples.len(), 14);
        assert_eq!(samples[0].offset, 1000);
        assert_eq!(samples[2].offset, 2000, "chunk 2 starts the third sample");
        assert_eq!(samples[8].offset, 5000, "chunk 5 starts the ninth");
        assert_eq!(samples[11].offset, 6000, "and holds three");

        // A table whose first row does not start at chunk 1 describes
        // nothing for the chunks before it.
        let late = Parsed {
            sizes: vec![10; 4],
            offsets: vec![1000, 2000],
            stsc: vec![(2, 2)],
            ..Default::default()
        };
        assert!(late.build_samples().is_err());

        // At scale: 20,000 chunks against a 20,000-row table. Re-scanning
        // the table per chunk is 200 million steps here — the reader must
        // walk each table once.
        let n = 20_000usize;
        let big = Parsed {
            sizes: vec![8; n],
            offsets: (0..n as u64).map(|c| 100 + c * 8).collect(),
            stsc: (0..n as u32).map(|r| (r + 1, 1)).collect(),
            ..Default::default()
        };
        let samples = big.build_samples().unwrap();
        assert_eq!(samples.len(), n);
        assert_eq!(samples[n - 1].offset, 100 + (n as u64 - 1) * 8);
    }

    /// A UNIFORM sample-size table (one size for every sample, no table
    /// following) is legal QuickTime — this module never writes one, but
    /// the reader must not reject a real file over it, and must not
    /// allocate 34 GB when one lies about its count.
    #[test]
    fn a_uniform_sample_size_table_is_read_but_bounded() {
        let mut p = Parsed::default();
        let mut body = Vec::new();
        body.extend_from_slice(&0u32.to_be_bytes()); // version + flags
        body.extend_from_slice(&64u32.to_be_bytes()); // every sample is 64 B
        body.extend_from_slice(&5u32.to_be_bytes()); // ...and there are 5
        parse_stsz(&body, &mut p).unwrap();
        assert_eq!(p.sizes, vec![64u64; 5]);

        body[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(
            parse_stsz(&body, &mut Parsed::default()).is_err(),
            "four billion samples of a claimed size is not a file"
        );
    }

    /// The golden file: the exact header bytes for the three A1 reference
    /// frames the phone test used. Any change to the container — a field,
    /// a size, an atom order — shows up here as a byte diff and has to be
    /// argued for, exactly like the XMP serializer's golden.
    ///
    /// ffprobe 8.1.2 on the file this header produces from the real
    /// frames reports (recorded 2026-08-27, `clip_muxer.rs` re-checks it
    /// wherever ffprobe exists):
    ///
    /// ```text
    /// codec_name=mjpeg  codec_tag_string=jpeg  width=8640  height=5760
    /// pix_fmt=yuvj422p  nb_read_frames=3  r_frame_rate=1000/33
    /// ```
    #[test]
    fn the_container_layout_is_pinned_to_a_golden_file() {
        let mut written = Vec::new();
        write_header(&mut written, &reference_spec()).unwrap();
        let golden_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/clip-3frames.mov.header");
        // Regenerating, when a container change is deliberate:
        //   FASTCULL_UPDATE_GOLDEN=1 cargo test -p fastcull-core golden
        // and then read the diff in `git diff` before committing it.
        if std::env::var_os("FASTCULL_UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden_path.parent().unwrap()).unwrap();
            std::fs::write(&golden_path, &written).unwrap();
            eprintln!("golden re-generated: {golden_path:?}");
        }
        let golden = std::fs::read(&golden_path).expect("the golden header file");
        if written != golden {
            let at = written
                .iter()
                .zip(&golden)
                .position(|(a, b)| a != b)
                .unwrap_or(golden.len().min(written.len()));
            panic!(
                "the container changed at byte {at} (written {} bytes, golden {}):\n\
                 written: {:02x?}\n golden: {:02x?}\n\
                 If the change is intended, re-generate {golden_path:?} and say why.",
                written.len(),
                golden.len(),
                &written[at.saturating_sub(8)..(at + 8).min(written.len())],
                &golden[at.saturating_sub(8)..(at + 8).min(golden.len())],
            );
        }
    }
}
