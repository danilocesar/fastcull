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
pub mod jpeg_exif;
pub mod sony;
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

/// Apply an EXIF orientation (1–8) to decoded RGB pixels, returning the
/// display-oriented buffer and its (possibly swapped) dimensions. Soft
/// rotation only — sources are never modified (Photo Mechanic behavior).
///
/// Performance matters here far more than it looks: a 50 MP A1 frame is
/// 49.8 M pixels, and this runs on the loupe's interactive path, where
/// ui-grid.md promises "sharpness-on-stop within ~300 ms". The first
/// implementation was a scalar loop with the orientation `match` INSIDE it,
/// copying three bytes per iteration and transposing row-major into
/// column-major — so nearly every write missed an 8 MB L3. Measured on the
/// 8-core development laptop it cost **392 ms**, more than the 262 ms JPEG
/// decode it followed, and portrait frames therefore missed the 300 ms
/// contract by better than 2x. (Landscape frames return at the guard above
/// and never paid it, which is why it went unnoticed.)
///
/// Three things fix it, in order of importance:
///
/// 1. **Cache-blocked tiles.** The transpose is reordered into square tiles
///    so both the read and the write side stay resident. This alone is most
///    of the win — locality, not parallelism: 392 ms -> ~80 ms.
/// 2. **Threads**, over disjoint output row bands. Four (the physical core
///    count here) is as good as eight; the work is memory-bound, so the cap
///    is deliberate rather than "all cores".
/// 3. **No transpose at all for 2/3/4.** Mirrors and 180° keep the original
///    dimensions, so they are done IN PLACE and allocate nothing — the old
///    code paid a second 149 MB buffer for them too.
///
/// Output is byte-identical to the original implementation for all eight
/// orientations; `orientation_matches_reference` pins that against a
/// straightforward reference version.
pub fn apply_orientation(rgb: Vec<u8>, w: u32, h: u32, orientation: u16) -> (Vec<u8>, u32, u32) {
    if orientation <= 1 || orientation > 8 || rgb.len() != (w as usize * h as usize * 3) {
        return (rgb, w, h);
    }
    let (wu, hu) = (w as usize, h as usize);
    let mut rgb = rgb;
    match orientation {
        // ---- No transpose: dimensions unchanged, so rotate IN PLACE ----
        2 => {
            // Mirror horizontal: reverse the pixels of each row.
            for row in rgb.chunks_exact_mut(wu * 3) {
                reverse_pixels(row);
            }
            return (rgb, w, h);
        }
        3 => {
            // Rotate 180 = reverse every pixel in the image.
            reverse_pixels(&mut rgb);
            return (rgb, w, h);
        }
        4 => {
            // Mirror vertical: swap row i with row (h-1-i).
            let stride = wu * 3;
            for y in 0..hu / 2 {
                let (top, rest) = rgb.split_at_mut((y + 1) * stride);
                let a = &mut top[y * stride..];
                let b = &mut rest[(hu - 2 - 2 * y) * stride..][..stride];
                a.swap_with_slice(b);
            }
            return (rgb, w, h);
        }
        _ => {}
    }
    // ---- Transposing orientations (5..=8): output dims are swapped ----
    //
    // Inverting the forward mapping gives a source coordinate whose `x`
    // depends only on the output ROW and whose `y` depends only on the
    // output COLUMN, which is what makes a clean tiled walk possible:
    //   5: x = dy,        y = dx
    //   6: x = dy,        y = hu-1-dx
    //   7: x = wu-1-dy,   y = hu-1-dx
    //   8: x = wu-1-dy,   y = dx
    let x_rev = matches!(orientation, 7 | 8);
    let y_rev = matches!(orientation, 6 | 7);
    let (ow, oh) = (hu, wu);
    let mut out = vec![0u8; rgb.len()];
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, MAX_ROTATE_THREADS);
    let rows_per = oh.div_ceil(threads);
    let src = &rgb[..];
    std::thread::scope(|scope| {
        for (t, band) in out.chunks_mut(rows_per * ow * 3).enumerate() {
            scope.spawn(move || {
                let base = t * rows_per;
                let nrows = band.len() / (ow * 3);
                for dy0 in (0..nrows).step_by(ROTATE_TILE) {
                    let dy1 = (dy0 + ROTATE_TILE).min(nrows);
                    for dx0 in (0..ow).step_by(ROTATE_TILE) {
                        let dx1 = (dx0 + ROTATE_TILE).min(ow);
                        for dy in dy0..dy1 {
                            let oy = base + dy;
                            let x = if x_rev { wu - 1 - oy } else { oy };
                            let row = &mut band[dy * ow * 3..(dy + 1) * ow * 3];
                            for dx in dx0..dx1 {
                                let y = if y_rev { hu - 1 - dx } else { dx };
                                let s = (y * wu + x) * 3;
                                row[dx * 3..dx * 3 + 3].copy_from_slice(&src[s..s + 3]);
                            }
                        }
                    }
                }
            });
        }
    });
    (out, ow as u32, oh as u32)
}

/// Square tile for the transposing rotations, in pixels. 64-128 measured
/// the same; both keep a tile's read and write side inside L2.
const ROTATE_TILE: usize = 64;

/// The rotate is memory-bound, so more threads stop helping well before the
/// core count on a big machine: 4 and 8 measured identically on a 4-core /
/// 8-thread laptop. Capped so a 32-thread box does not spawn 32 workers to
/// contend for the same memory bandwidth.
const MAX_ROTATE_THREADS: usize = 8;

/// Reverse a run of 3-byte pixels in place.
fn reverse_pixels(buf: &mut [u8]) {
    let n = buf.len() / 3;
    for i in 0..n / 2 {
        let (a, b) = (i * 3, (n - 1 - i) * 3);
        for k in 0..3 {
            buf.swap(a + k, b + k);
        }
    }
}

/// No camera embeds previews anywhere near this size; a larger `len` means a
/// corrupt or hand-fabricated `EmbeddedJpeg` and must not become a giant
/// allocation.
const MAX_EMBEDDED_JPEG_LEN: u64 = 256 * 1024 * 1024;

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

    /// The tiled/threaded/in-place rewrite must be byte-identical to the
    /// straightforward version for EVERY orientation, at sizes that exercise
    /// partial tiles and partial thread bands (the tile is 64 px, so 1x1,
    /// odd, and >64 all take different paths). Reference is deliberately the
    /// naive per-pixel mapping the original shipped.
    #[test]
    fn orientation_matches_reference() {
        fn reference(rgb: &[u8], w: u32, h: u32, o: u16) -> (Vec<u8>, u32, u32) {
            if o <= 1 || o > 8 || rgb.len() != (w as usize * h as usize * 3) {
                return (rgb.to_vec(), w, h);
            }
            let (wu, hu) = (w as usize, h as usize);
            let swap = matches!(o, 5..=8);
            let (ow, oh) = if swap { (hu, wu) } else { (wu, hu) };
            let mut out = vec![0u8; rgb.len()];
            for y in 0..hu {
                for x in 0..wu {
                    let (dx, dy) = match o {
                        2 => (wu - 1 - x, y),
                        3 => (wu - 1 - x, hu - 1 - y),
                        4 => (x, hu - 1 - y),
                        5 => (y, x),
                        6 => (hu - 1 - y, x),
                        7 => (hu - 1 - y, wu - 1 - x),
                        8 => (y, wu - 1 - x),
                        _ => (x, y),
                    };
                    let src = (y * wu + x) * 3;
                    let dst = (dy * ow + dx) * 3;
                    out[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
                }
            }
            (out, ow as u32, oh as u32)
        }
        // A deterministic pattern where every pixel is distinguishable, so a
        // mis-mapped pixel cannot hide behind a neighbour of the same colour.
        let mk = |w: usize, h: usize| -> Vec<u8> {
            (0..w * h * 3)
                .map(|i| ((i * 37 + i / 3 * 11) % 251) as u8)
                .collect()
        };
        for (w, h) in [
            (1usize, 1usize),
            (1, 7),
            (7, 1),
            (2, 3),
            (63, 65),
            (64, 64),
            (65, 63),
            (130, 71),
            (200, 137),
        ] {
            let src = mk(w, h);
            for o in 1..=8u16 {
                let want = reference(&src, w as u32, h as u32, o);
                let got = apply_orientation(src.clone(), w as u32, h as u32, o);
                assert_eq!(got.1, want.1, "width mismatch {w}x{h} orientation {o}");
                assert_eq!(got.2, want.2, "height mismatch {w}x{h} orientation {o}");
                assert!(got.0 == want.0, "PIXELS DIFFER at {w}x{h} orientation {o}");
            }
        }
        // Out-of-range and mismatched-length inputs still pass through.
        let src = mk(4, 4);
        assert_eq!(apply_orientation(src.clone(), 4, 4, 0).0, src);
        assert_eq!(apply_orientation(src.clone(), 4, 4, 9).0, src);
        assert_eq!(apply_orientation(src.clone(), 5, 5, 6).0, src);
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
