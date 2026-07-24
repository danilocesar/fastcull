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
#[derive(Debug, Clone, Default)]
pub struct EmbeddedPreviews {
    pub candidates: Vec<EmbeddedJpeg>,
}

impl EmbeddedPreviews {
    /// Source for the grid thumbnail: the largest useful preview ≤ ~2 MP,
    /// falling back to the smallest larger one (cheapest decode that still
    /// yields a thumb) if no mid-size preview exists.
    pub fn grid_source(&self) -> Option<&EmbeddedJpeg> {
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
    let located = tiff::walk_jpeg_pointers(reader)?;

    let mut candidates: Vec<EmbeddedJpeg> = Vec::new();
    for loc in located {
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
    Ok(EmbeddedPreviews { candidates })
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

    #[test]
    fn grid_source_prefers_largest_at_or_below_2mp() {
        let previews = EmbeddedPreviews {
            candidates: vec![jpeg(8640, 5760), jpeg(1616, 1080), jpeg(160, 120)],
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
        };
        let grid = previews.grid_source().unwrap();
        assert_eq!((grid.width, grid.height), (4000, 3000));
    }

    #[test]
    fn tiny_thumbnails_are_never_selected() {
        let previews = EmbeddedPreviews {
            candidates: vec![jpeg(160, 120)],
        };
        assert!(previews.grid_source().is_none());
        assert!(previews.fullres().is_none());
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
