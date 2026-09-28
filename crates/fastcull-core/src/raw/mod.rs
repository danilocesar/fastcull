//! RAW file access: embedded-JPEG discovery and extraction.
//!
//! Spec: `specs/modules/raw-pipeline.md`. The culling hot path never decodes
//! RAW sensor data; it locates the camera-written JPEG previews inside the RAW
//! container with surgical reads (IFD tables + JPEG headers + chosen payload),
//! never the whole file.
//!
//! Layout of a Sony A1 ARW (verified against the three reference files):
//! IFD0 holds the 1616×1080 preview (`JPEGInterchangeFormat`, dimensions only
//! in the JPEG SOF header), the IFD chain continues to a 160×120 thumbnail and
//! then to the 8640×5760 full-resolution JPEG (with `ImageWidth`/`ImageLength`
//! tags); raw sensor data lives in a SubIFD with no JPEG pointer tags.

mod endian;
mod jpeg;
#[cfg(test)]
pub(crate) use jpeg::hostile as jpeg_hostile;
pub(crate) use jpeg::{scan_is_terminated, sof_dimensions, without_header_gaps};
#[cfg(test)]
pub(crate) use tiff::tests as tiff_testutil;
pub mod jpeg_exif;
pub(crate) mod orient;
pub mod sony;
pub use orient::{apply_orientation, apply_orientation_with, Scratch};
mod tiff;

use std::io::{Read, Seek, SeekFrom};

pub use tiff::TiffError;

/// Failure text when a file carries no preview this app can use at all —
/// no grid source, no ladder rung. Both engines badge with it (the grid
/// pipeline and the loupe ladder), so it exists once.
///
/// This text reaches the user as a badge, so it is pinned by
/// `badge_text_is_pinned` below rather than left to drift on a typo.
pub(crate) const NO_USABLE_PREVIEW: &str = "no usable embedded preview";

/// Failure text when previews were FOUND but none of them decoded. Kept
/// distinct from `NO_USABLE_PREVIEW` on purpose: "nothing to read" and
/// "read it, it was broken" are different bug reports.
pub(crate) const NO_DECODABLE_PREVIEW: &str = "no decodable preview";

/// An embedded JPEG discovered inside a RAW container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedJpeg {
    /// Absolute byte offset of the JPEG stream in the RAW file.
    pub offset: u64,
    /// Length of the JPEG stream in bytes.
    pub len: u64,
    pub width: u32,
    pub height: u32,
}

impl EmbeddedJpeg {
    pub fn pixels(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// Grid thumbnails come from the largest preview at or below this pixel count
/// (~2 MP); anything larger is loupe material (spec: raw-pipeline.md).
const GRID_SOURCE_MAX_PIXELS: u64 = 2_100_000;

/// Previews too small to render anything (e.g. 160×120 thumbnails) are never
/// useful on screen.
const USEFUL_MIN_PIXELS: u64 = 100_000;

/// All embedded JPEGs of one RAW file, largest first.
#[derive(Debug, Clone)]
pub struct EmbeddedPreviews {
    /// The embedded JPEGs whose byte range lies inside the file: the ones
    /// every consumer may read whole.
    pub candidates: Vec<EmbeddedJpeg>,
    /// The embedded JPEGs the file was CUT inside — an interrupted copy, a
    /// dying card: each begins inside the file with a JPEG signature and a
    /// plausible declared length that runs past the file's end. Kept apart
    /// so no consumer reads one as whole: the grid thumb, the video export
    /// and [`fullres`](Self::fullres) see only `candidates`, and
    /// [`read_jpeg`] names a cut one "truncated" without reading it. The
    /// loupe alone takes one as the file's top rung
    /// ([`loupe_top`](Self::loupe_top)), so a RAW cut inside its full keeps
    /// its mid as a lower rung — soft, cued — and names the cut on stderr
    /// (raw-pipeline.md, "Hostile-input bounds"; QE 2026-09-28, D1). Always
    /// empty for a bare JPEG, whose one candidate is the whole file.
    pub cut: Vec<EmbeddedJpeg>,
    /// True when the source IS a bare image file (issue #8): the single
    /// whole-file candidate is the actual image, not a 160x120 embedded
    /// thumbnail — the min-useful-pixels filter must not apply (QE: a
    /// 380x260 messenger JPEG became a Failed cell).
    pub whole_file: bool,
    /// EXIF orientation (1–8; 1 = as stored). Previews are stored in sensor
    /// orientation — apply this to decoded pixels before display
    /// (raw-pipeline.md, user requirement 2026-07-25).
    pub orientation: u16,
}

impl Default for EmbeddedPreviews {
    fn default() -> Self {
        Self {
            candidates: Vec::new(),
            cut: Vec::new(),
            whole_file: false,
            orientation: 1,
        }
    }
}

impl EmbeddedPreviews {
    /// Source for the grid thumbnail: the largest useful preview ≤ ~2 MP,
    /// falling back to the smallest larger one (cheapest decode that still
    /// yields a thumb) if no mid-size preview exists.
    pub fn grid_source(&self) -> Option<&EmbeddedJpeg> {
        if self.whole_file {
            return self.candidates.first();
        }
        self.candidates
            .iter()
            .filter(|c| (USEFUL_MIN_PIXELS..=GRID_SOURCE_MAX_PIXELS).contains(&c.pixels()))
            .max_by_key(|c| c.pixels())
            .or_else(|| {
                self.candidates
                    .iter()
                    .filter(|c| c.pixels() > GRID_SOURCE_MAX_PIXELS)
                    .min_by_key(|c| c.pixels())
            })
    }

    /// The largest embedded JPEG the file holds WHOLE: the full-res source
    /// for every consumer that reads it (the video export's frame; the
    /// loupe's, when nothing larger was cut — [`loupe_top`](Self::loupe_top)).
    pub fn fullres(&self) -> Option<&EmbeddedJpeg> {
        if self.whole_file {
            return self.candidates.first();
        }
        self.candidates
            .iter()
            .filter(|c| c.pixels() >= USEFUL_MIN_PIXELS)
            .max_by_key(|c| (c.pixels(), c.len))
    }

    /// The loupe's top rung: the largest useful embedded JPEG, whole or CUT.
    /// When the file was cut inside a JPEG larger than any it holds whole —
    /// a RAW cut inside its full — that JPEG is still the file's best, so the
    /// rungs below it are never its best (never `terminal`), and its read
    /// fails as truncated: the loupe keeps the lower rung, cued, and names
    /// the cut on stderr, where dropping it made the mid the file's best —
    /// shown unflagged at fit, with no line (raw-pipeline.md, "Hostile-input
    /// bounds"; QE 2026-09-28, D1). A whole candidate wins a tie.
    pub fn loupe_top(&self) -> Option<&EmbeddedJpeg> {
        let whole = self.fullres();
        let cut = self
            .cut
            .iter()
            .filter(|c| c.pixels() >= USEFUL_MIN_PIXELS)
            .max_by_key(|c| (c.pixels(), c.len));
        match (whole, cut) {
            (Some(w), Some(c)) if (c.pixels(), c.len) > (w.pixels(), w.len) => Some(c),
            (Some(w), _) => Some(w),
            (None, c) => c,
        }
    }
}

/// Walk the TIFF structure of `reader` and return every embedded JPEG whose
/// byte range lies inside the file, sorted largest-first by pixel count —
/// and, apart, every one the file was CUT inside
/// ([`EmbeddedPreviews::cut`]).
///
/// Reads only IFD tables and JPEG headers — a few KB total. Candidates whose
/// payload does not start with a JPEG signature or whose dimensions cannot be
/// determined are dropped, and so are pointers the file cannot hold at all:
/// an empty range, one that starts at or past the file's end, and one that
/// runs past it with a length no embedded JPEG has (over
/// `MAX_EMBEDDED_JPEG_LEN`: a hostile claim, not a cut).
pub fn find_embedded_jpegs<R: Read + Seek>(reader: &mut R) -> Result<EmbeddedPreviews, TiffError> {
    let file_len = reader.seek(SeekFrom::End(0))?;

    // A bare JPEG file (issue #8) IS its own single "embedded preview"
    // covering the whole file — every rung of the thumb/loupe ladder
    // then works unchanged, format-agnostically.
    if jpeg::has_jpeg_signature(reader, 0)? {
        if let Some((width, height)) = jpeg::sniff_dimensions(reader, 0, file_len)? {
            // Orientation from the JPEG's own APP1 (degrades to 1) — the
            // pipeline soft-rotates every rung with it, so portrait phone
            // shots come out upright (persona requirement).
            let orientation = jpeg_exif::read_jpeg_exif(reader)
                .map(|e| e.orientation)
                .unwrap_or(1);
            return Ok(EmbeddedPreviews {
                candidates: vec![EmbeddedJpeg {
                    offset: 0,
                    len: file_len,
                    width,
                    height,
                }],
                cut: Vec::new(),
                whole_file: true,
                orientation,
            });
        }
        // JPEG signature but undecipherable headers: a JPEG-flavored
        // error, not the misleading "not a TIFF container" (QE note).
        return Err(TiffError::Malformed(
            "JPEG signature but no parseable SOF header",
        ));
    }

    let walk = tiff::walk_jpeg_pointers(reader)?;

    let mut candidates: Vec<EmbeddedJpeg> = Vec::new();
    let mut cut: Vec<EmbeddedJpeg> = Vec::new();
    for loc in walk.jpegs {
        if loc.len == 0
            || loc.offset >= file_len
            || candidates
                .iter()
                .chain(&cut)
                .any(|c| c.offset == loc.offset)
        {
            continue;
        }
        // `loc.offset < file_len`, so the end overflows only for a length
        // no file holds; either way the range runs past the file's end.
        let whole = loc
            .offset
            .checked_add(loc.len)
            .is_some_and(|end| end <= file_len);
        if !whole && loc.len > MAX_EMBEDDED_JPEG_LEN {
            continue; // a hostile length, not a cut-off copy
        }
        let dims = match (loc.width, loc.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => {
                // Trust the IFD dimensions but still require a JPEG signature.
                match jpeg::has_jpeg_signature(reader, loc.offset)? {
                    true => Some((w, h)),
                    false => None,
                }
            }
            // Only the bytes the file still holds can be sniffed.
            _ => jpeg::sniff_dimensions(reader, loc.offset, loc.len.min(file_len - loc.offset))?,
        };
        if let Some((width, height)) = dims {
            let found = EmbeddedJpeg {
                offset: loc.offset,
                len: loc.len,
                width,
                height,
            };
            if whole {
                candidates.push(found);
            } else {
                cut.push(found);
            }
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse((c.pixels(), c.len)));
    cut.sort_by_key(|c| std::cmp::Reverse((c.pixels(), c.len)));
    Ok(EmbeddedPreviews {
        candidates,
        cut,
        whole_file: false,
        orientation: walk.orientation,
    })
}

/// No camera embeds previews anywhere near this size; a larger `len` means a
/// corrupt or hand-fabricated `EmbeddedJpeg` and must not become a giant
/// allocation.
const MAX_EMBEDDED_JPEG_LEN: u64 = 256 * 1024 * 1024;

/// Output-side twin of [`MAX_EMBEDDED_JPEG_LEN`] (issue #31): decode buffers
/// are sized from SOF header dimensions BEFORE any scan data is validated, so
/// a sub-KB stream claiming huge dimensions must be rejected here, not
/// trusted. 500 MP is ~10x the Sony A1's 8640x5760 (49.8 MP) and ~3x the
/// largest shipping sensor (Phase One IQ4, 150 MP), with room for stitched
/// panoramas — while the JPEG format ceiling (65535x65535 = 4.29 GP) would
/// commit ~12.9 GB of RGB per buffer. At this cap a hostile stream costs at
/// most ~1.5 GB per decode buffer instead.
pub(crate) const MAX_DECODED_PIXELS: u64 = 500_000_000;

/// True when SOF-declared dimensions are small enough to size decode/rotate
/// buffers from (see [`MAX_DECODED_PIXELS`]).
pub(crate) fn plausible_decoded_dims(width: usize, height: usize) -> bool {
    (width as u64).saturating_mul(height as u64) <= MAX_DECODED_PIXELS
}

/// Read one embedded JPEG's bytes.
///
/// A JPEG the file ends inside — a cut-off copy, the commonest field
/// corruption — is refused as [`TiffError::Truncated`] before a byte of it is
/// read or a buffer sized for it, naming the cause where a short read would
/// only say the buffer did not fill (raw-pipeline.md, "Hostile-input
/// bounds"; QE 2026-09-28, D1). The check costs one seek.
pub fn read_jpeg<R: Read + Seek>(
    reader: &mut R,
    jpeg: &EmbeddedJpeg,
) -> Result<Vec<u8>, TiffError> {
    if jpeg.len > MAX_EMBEDDED_JPEG_LEN {
        return Err(TiffError::Malformed("implausible embedded JPEG length"));
    }
    let len = usize::try_from(jpeg.len).map_err(|_| TiffError::Malformed("JPEG length"))?;
    let file_len = reader.seek(SeekFrom::End(0))?;
    let present = file_len.saturating_sub(jpeg.offset).min(jpeg.len);
    if present < jpeg.len {
        return Err(TiffError::Truncated {
            present,
            declared: jpeg.len,
        });
    }
    reader.seek(SeekFrom::Start(jpeg.offset))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::tiff::tests::{tiny_jpeg, TiffBuilder};
    use super::*;
    use std::io::Cursor;

    fn jpeg(width: u32, height: u32) -> EmbeddedJpeg {
        EmbeddedJpeg {
            offset: 0,
            len: 1000,
            width,
            height,
        }
    }

    #[test]
    fn orientation_rotations_are_correct() {
        // 2x1 image: red then green. Orientation 6 (90 CW) => 1x2 with red
        // at the top-right... i.e. column layout red-over-green becomes
        // green? Verify by explicit expectation.
        let rgb = vec![255, 0, 0, 0, 255, 0]; // (0,0)=red (1,0)=green
        let (r90, w, h) = apply_orientation(rgb.clone(), 2, 1, 6);
        assert_eq!((w, h), (1, 2));
        assert_eq!(&r90[0..3], &[255, 0, 0]); // red now at (0,0)
        assert_eq!(&r90[3..6], &[0, 255, 0]); // green below
        let (r180, w2, h2) = apply_orientation(rgb.clone(), 2, 1, 3);
        assert_eq!((w2, h2), (2, 1));
        assert_eq!(&r180[0..3], &[0, 255, 0]); // reversed
        let (same, ..) = apply_orientation(rgb.clone(), 2, 1, 1);
        assert_eq!(same, rgb);
        // Round-trip: 90 CW then 270 CW restores the original.
        let (once, ow, ohh) = apply_orientation(rgb.clone(), 2, 1, 6);
        let (back, ..) = apply_orientation(once, ow, ohh, 8);
        assert_eq!(back, rgb);
    }

    #[test]
    fn grid_source_prefers_largest_at_or_below_2mp() {
        let previews = EmbeddedPreviews {
            candidates: vec![jpeg(8640, 5760), jpeg(1616, 1080), jpeg(160, 120)],
            cut: Vec::new(),
            whole_file: false,
            orientation: 1,
        };
        let grid = previews.grid_source().unwrap();
        assert_eq!((grid.width, grid.height), (1616, 1080));
        let full = previews.fullres().unwrap();
        assert_eq!((full.width, full.height), (8640, 5760));
    }

    /// Regression (validator finding): with only >2MP previews available, the
    /// fallback must pick the *smallest* of them, not the largest.
    #[test]
    fn grid_source_falls_back_to_smallest_larger_preview() {
        let previews = EmbeddedPreviews {
            candidates: vec![jpeg(8640, 5760), jpeg(4000, 3000)],
            cut: Vec::new(),
            whole_file: false,
            orientation: 1,
        };
        let grid = previews.grid_source().unwrap();
        assert_eq!((grid.width, grid.height), (4000, 3000));
    }

    #[test]
    fn tiny_thumbnails_are_never_selected() {
        let previews = EmbeddedPreviews {
            candidates: vec![jpeg(160, 120)],
            cut: Vec::new(),
            whole_file: false,
            orientation: 1,
        };
        assert!(previews.grid_source().is_none());
        assert!(previews.fullres().is_none());
    }

    /// Issue #8 / QE D1: a WHOLE-FILE candidate (bare JPEG) is the actual
    /// image, exempt from the min-useful-pixels filter — a 380x260
    /// messenger JPEG must never become a Failed cell.
    #[test]
    fn whole_file_candidate_is_exempt_from_min_pixels() {
        let previews = EmbeddedPreviews {
            candidates: vec![jpeg(380, 260)],
            cut: Vec::new(),
            whole_file: true,
            orientation: 1,
        };
        assert!(previews.grid_source().is_some());
        assert!(previews.fullres().is_some());
        assert_eq!(
            previews.grid_source().map(|c| c.offset),
            previews.fullres().map(|c| c.offset),
            "single rung serves both roles"
        );
    }

    /// Regression (validator finding): two IFDs pointing at the same payload
    /// offset must collapse to one candidate even when their declared
    /// dimensions differ (non-adjacent after sorting).
    #[test]
    fn duplicate_offsets_collapse_to_one_candidate() {
        let mut b = TiffBuilder::new(true);
        let j = tiny_jpeg(500, 400);
        let payload = b.add_blob(&j);
        let second = b.add_ifd(
            &[
                (0x0201, 4, 1, payload),
                (0x0202, 4, 1, j.len() as u32),
                (0x0100, 3, 1, 5000), // lies about dimensions
                (0x0101, 3, 1, 4000),
            ],
            0,
        );
        let ifd0 = b.add_ifd(
            &[(0x0201, 4, 1, payload), (0x0202, 4, 1, j.len() as u32)],
            second,
        );
        b.set_ifd0(ifd0);
        let previews = find_embedded_jpegs(&mut b.cursor()).unwrap();
        assert_eq!(previews.candidates.len(), 1);
    }

    /// The no-preview badge text, pinned end to end (gate finding: the
    /// constants were introduced with a claim that "the tests match it by
    /// substring", and no test did — a typo could have changed the text
    /// the user reads, in both engines at once, unnoticed).
    ///
    /// A container that parses cleanly but carries no JPEG pointer is the
    /// exact trigger, so this drives the real grid path rather than only
    /// asserting a string against itself.
    #[test]
    fn no_preview_badges_with_the_pinned_text() {
        let mut b = TiffBuilder::new(true);
        // An IFD with dimensions but no 0x0201/0x0202 pointer pair: valid
        // TIFF, nothing this app can show.
        let ifd0 = b.add_ifd(&[(0x0100, 3, 1, 4000), (0x0101, 3, 1, 3000)], 0);
        b.set_ifd0(ifd0);
        let previews = find_embedded_jpegs(&mut b.cursor()).unwrap();
        assert!(previews.grid_source().is_none(), "fixture must have none");

        let dir = crate::testutil::scratch_dir("nopreview");
        let path = dir.join("no_preview.ARW");
        std::fs::write(&path, b.cursor().into_inner()).unwrap();
        let spec = crate::pipeline::JobSpec {
            path,
            size: 0,
            mtime: None,
        };
        let err = crate::pipeline::make_grid_thumb(&spec)
            .expect_err("a container with no preview must fail the thumb");
        assert_eq!(err, NO_USABLE_PREVIEW);
        std::fs::remove_dir_all(&dir).ok();

        // The wording itself (both engines badge with these).
        assert_eq!(NO_USABLE_PREVIEW, "no usable embedded preview");
        assert_eq!(NO_DECODABLE_PREVIEW, "no decodable preview");
    }

    /// A RAW cut by an interrupted copy, laid out as an A1 is: IFD0 pointing
    /// at a whole `mid`, IFD1 at `full`, then the two JPEGs, the full last —
    /// and the file cut `keep` bytes into the full. `dims` puts the full's
    /// size in IFD1, as an A1's IFD2 carries it; `declared` is the length
    /// IFD1 gives the full.
    fn cut_inside_the_full(
        mid: &[u8],
        full: &[u8],
        dims: Option<(u32, u32)>,
        declared: u32,
        keep: usize,
    ) -> Vec<u8> {
        let ifd_len = |entries: u32| 2 + 12 * entries + 4;
        let mut b = TiffBuilder::new(true);
        let ifd0 = b.bytes.len() as u32;
        let ifd1 = ifd0 + ifd_len(2);
        let mid_off = ifd1 + ifd_len(if dims.is_some() { 4 } else { 2 });
        let full_off = mid_off + mid.len() as u32;
        assert_eq!(
            b.add_ifd(
                &[(0x0201, 4, 1, mid_off), (0x0202, 4, 1, mid.len() as u32)],
                ifd1
            ),
            ifd0
        );
        let mut entries = Vec::new();
        if let Some((w, h)) = dims {
            entries.extend([(0x0100, 3, 1, w), (0x0101, 3, 1, h)]);
        }
        entries.extend([(0x0201, 4, 1, full_off), (0x0202, 4, 1, declared)]);
        assert_eq!(b.add_ifd(&entries, 0), ifd1);
        assert_eq!(b.add_blob(mid), mid_off);
        assert_eq!(b.add_blob(full), full_off);
        b.set_ifd0(ifd0);
        b.bytes.truncate(full_off as usize + keep);
        b.bytes
    }

    /// QE round 1 of brief 008, D1 (raw-pipeline.md, "Hostile-input
    /// bounds"): a JPEG the file was cut inside is kept APART from the ones it
    /// holds whole. Sized from its IFD, or — with no size there — from the
    /// bytes the file still holds; no consumer reads it as whole (`fullres`
    /// and `grid_source` pick among the whole ones, every one of which lies
    /// inside the file); the loupe's top rung is the largest whole or cut, a
    /// whole one winning a tie; `read_jpeg` refuses it as truncated, naming
    /// how much of it the file holds. A pointer at the file's end and one
    /// whose length no embedded JPEG has are dropped as before. Red with cut
    /// JPEGs dropped, with the length guard gone, with `loupe_top` reading the
    /// whole ones alone, and with `read_jpeg`'s check gone (the short read's
    /// I/O error names no cause).
    #[test]
    fn a_jpeg_the_file_was_cut_inside_is_kept_apart_as_cut() {
        let mid = tiny_jpeg(500, 400);
        let full = tiny_jpeg(1000, 800); // SOI, a 13-byte SOF, EOI: 16 bytes
        let declared = full.len() as u32;
        let find = |bytes: Vec<u8>| find_embedded_jpegs(&mut Cursor::new(bytes)).unwrap();
        let dims = |c: Option<&EmbeddedJpeg>| c.map(|c| (c.width, c.height));

        // The A1's shape: IFD dims, the file ending inside the full.
        let bytes = cut_inside_the_full(&mid, &full, Some((1000, 800)), declared, 10);
        let file_len = bytes.len() as u64;
        let previews = find(bytes.clone());
        assert_eq!(dims(previews.candidates.first()), Some((500, 400)));
        assert_eq!(previews.candidates.len(), 1, "only the mid is whole");
        for c in &previews.candidates {
            assert!(c.offset + c.len <= file_len, "a whole one lies inside");
        }
        assert_eq!(previews.cut.len(), 1, "the full is kept, apart");
        assert_eq!(
            (
                previews.cut[0].width,
                previews.cut[0].height,
                previews.cut[0].len
            ),
            (1000, 800, u64::from(declared)),
            "sized from its IFD, with the length the IFD declares"
        );
        assert_eq!(dims(previews.fullres()), Some((500, 400)));
        assert_eq!(dims(previews.grid_source()), Some((500, 400)));
        assert_eq!(
            dims(previews.loupe_top()),
            Some((1000, 800)),
            "the loupe's top is the cut full"
        );
        let mut reader = Cursor::new(bytes);
        let err = read_jpeg(&mut reader, &previews.cut[0]).expect_err("the file ends inside it");
        assert!(
            matches!(
                err,
                TiffError::Truncated { present: 10, declared: d } if d == u64::from(declared)
            ),
            "{err:?}"
        );
        assert!(err.to_string().starts_with("truncated"), "{err}");
        let mid_bytes = read_jpeg(&mut reader, &previews.candidates[0]).expect("the mid is whole");
        assert_eq!(mid_bytes, mid);

        // No size in the IFD: sized from what the file holds — the SOF sits
        // in the first 15 bytes.
        let previews = find(cut_inside_the_full(&mid, &full, None, declared, 15));
        assert_eq!(dims(previews.cut.first()), Some((1000, 800)));
        // ... and with the SOF itself cut away there is nothing to size.
        let previews = find(cut_inside_the_full(&mid, &full, None, declared, 6));
        assert!(previews.cut.is_empty(), "{:?}", previews.cut);

        // Dropped as before: a pointer at the file's very end (nothing of it
        // is left), and a length no embedded JPEG has (a hostile claim).
        let previews = find(cut_inside_the_full(
            &mid,
            &full,
            Some((1000, 800)),
            declared,
            0,
        ));
        assert!(previews.cut.is_empty(), "at the end: {:?}", previews.cut);
        let hostile = u32::try_from(MAX_EMBEDDED_JPEG_LEN + 1).unwrap();
        let previews = find(cut_inside_the_full(
            &mid,
            &full,
            Some((1000, 800)),
            hostile,
            10,
        ));
        assert!(
            previews.cut.is_empty(),
            "hostile length: {:?}",
            previews.cut
        );

        // A whole JPEG wins a tie with a cut one: the whole 1000x800 mid slot
        // over a cut full claiming the same size and length.
        let previews = find(cut_inside_the_full(
            &full,
            &full,
            Some((1000, 800)),
            declared,
            10,
        ));
        let top = previews.loupe_top().expect("a top rung");
        assert!(
            previews.candidates.contains(top),
            "the whole one wins the tie: {top:?}"
        );
    }

    /// Issue #31 boundary: the pixel cap admits everything up to and
    /// including MAX_DECODED_PIXELS and nothing beyond — including the
    /// 30000x30000 hostile claim and overflow-shaped values.
    #[test]
    fn decoded_pixel_cap_boundaries() {
        assert!(plausible_decoded_dims(8640, 5760), "the A1 full-res");
        assert!(plausible_decoded_dims(25000, 20000), "exactly at the cap");
        assert!(!plausible_decoded_dims(25001, 20000), "one row over");
        assert!(
            !plausible_decoded_dims(30000, 30000),
            "the issue's repro claim"
        );
        assert!(!plausible_decoded_dims(65535, 65535), "the format ceiling");
        assert!(
            !plausible_decoded_dims(usize::MAX, usize::MAX),
            "overflow-safe"
        );
    }

    #[test]
    fn read_jpeg_rejects_implausible_length() {
        let huge = EmbeddedJpeg {
            offset: 0,
            len: MAX_EMBEDDED_JPEG_LEN + 1,
            width: 1,
            height: 1,
        };
        let result = read_jpeg(&mut Cursor::new(vec![0u8; 16]), &huge);
        assert!(matches!(result, Err(TiffError::Malformed(_))));
    }
}
