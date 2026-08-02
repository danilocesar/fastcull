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

mod jpeg;
#[cfg(test)]
pub(crate) use jpeg::hostile as jpeg_hostile;
pub(crate) use jpeg::scan_is_terminated;
#[cfg(test)]
pub(crate) use tiff::tests as tiff_testutil;
pub mod jpeg_exif;
pub(crate) mod orient;
pub mod sony;
pub use orient::{apply_orientation, apply_orientation_with, Scratch};
mod tiff;

use std::io::{Read, Seek, SeekFrom};

pub use tiff::TiffError;

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
    pub candidates: Vec<EmbeddedJpeg>,
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

    /// Source for loupe fit/1:1: the largest embedded JPEG.
    pub fn fullres(&self) -> Option<&EmbeddedJpeg> {
        if self.whole_file {
            return self.candidates.first();
        }
        self.candidates
            .iter()
            .filter(|c| c.pixels() >= USEFUL_MIN_PIXELS)
            .max_by_key(|c| (c.pixels(), c.len))
    }
}

/// Walk the TIFF structure of `reader` and return every embedded JPEG whose
/// byte range lies inside the file, sorted largest-first by pixel count.
///
/// Reads only IFD tables and JPEG headers — a few KB total. Candidates whose
/// payload does not start with a JPEG signature or whose dimensions cannot be
/// determined are dropped.
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
    for loc in walk.jpegs {
        if loc.len == 0
            || loc
                .offset
                .checked_add(loc.len)
                .is_none_or(|end| end > file_len)
            || candidates.iter().any(|c| c.offset == loc.offset)
        {
            continue;
        }
        let dims = match (loc.width, loc.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => {
                // Trust the IFD dimensions but still require a JPEG signature.
                match jpeg::has_jpeg_signature(reader, loc.offset)? {
                    true => Some((w, h)),
                    false => None,
                }
            }
            _ => jpeg::sniff_dimensions(reader, loc.offset, loc.len)?,
        };
        if let Some((width, height)) = dims {
            candidates.push(EmbeddedJpeg {
                offset: loc.offset,
                len: loc.len,
                width,
                height,
            });
        }
    }
    candidates.sort_by_key(|c| std::cmp::Reverse((c.pixels(), c.len)));
    Ok(EmbeddedPreviews {
        candidates,
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
pub fn read_jpeg<R: Read + Seek>(
    reader: &mut R,
    jpeg: &EmbeddedJpeg,
) -> Result<Vec<u8>, TiffError> {
    if jpeg.len > MAX_EMBEDDED_JPEG_LEN {
        return Err(TiffError::Malformed("implausible embedded JPEG length"));
    }
    let len = usize::try_from(jpeg.len).map_err(|_| TiffError::Malformed("JPEG length"))?;
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
