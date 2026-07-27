//! EXIF summary: the few metadata fields the culling workflow needs, read
//! via the in-tree targeted-read TIFF walker (`raw/jpeg_exif.rs`).
//!
//! History (perf investigation 2026-07-27): this used to go through
//! rawler's `RawSource`, which mmaps the ENTIRE RAW file per read. All
//! import workers then serialize on the process-wide `mmap_lock`
//! (measured: the EXIF pass peaked at ~500 files/s and got SLOWER with
//! more threads — 506/s at 8 threads, 429/s at 24 — while the seek+read
//! thumbnail path scaled to 1,557/s), and over FUSE mounts (ntfs-3g
//! backup drives) every faulted page is a userspace round trip. The
//! in-tree walker reads a few KB per file instead.
//!
//! Capture times are kept in EXIF string form (`"YYYY:MM:DD HH:MM:SS"`): the
//! format is fixed-width and zero-padded, so lexicographic order equals
//! chronological order and no calendar math is needed for sorting. Burst
//! grouping (M7) will add real Δt computation when it arrives.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ExifError {
    #[error("cannot open RAW file: {0}")]
    Open(String),
    #[error("cannot decode RAW metadata: {0}")]
    Metadata(String),
}

/// Camera identity and capture time of one image.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExifSummary {
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub serial_number: Option<String>,
    /// EXIF `DateTimeOriginal`, e.g. `"2021:04:05 17:41:23"`.
    pub capture_time: Option<String>,
    /// EXIF `SubSecTimeOriginal`: fractional-second digits, e.g. `"57"`.
    pub subsec: Option<String>,
    /// Sony burst sequence (M7, maker-note Tag9400c via `raw::sony`):
    /// None = tag absent / non-Sony; Some(0) = declared single (normal
    /// drive); Some(n>=1) = 1-based position in a continuous burst.
    /// `#[serde(default)]` because pre-M7 cache rows lack it (the cache
    /// schema version was bumped so those rows re-read anyway).
    #[serde(default)]
    pub sequence_number: Option<u32>,
}

impl ExifSummary {
    /// Chronologically ordered sort key: capture time plus subseconds
    /// normalized to exactly three digits (`"2021:04:05 17:41:23.570"`).
    /// `None` when the image has no capture time.
    pub fn sort_key(&self) -> Option<String> {
        let time = self.capture_time.as_deref()?;
        let subsec = self.subsec.as_deref().unwrap_or("");
        let mut millis: String = subsec
            .chars()
            .filter(char::is_ascii_digit)
            .take(3)
            .collect();
        while millis.len() < 3 {
            millis.push('0');
        }
        Some(format!("{time}.{millis}"))
    }
}

/// Read the EXIF summary of one RAW file (~5 µs: a few KB of targeted
/// IFD-table reads) — or of a bare JPEG (issue #8: the APP1 block, via
/// the same in-tree hardened walker; rawler has no JPEG decoder).
pub fn read_exif_summary(path: &Path) -> Result<ExifSummary, ExifError> {
    let is_jpeg = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"));
    if is_jpeg {
        let mut file = std::fs::File::open(path).map_err(|e| ExifError::Open(e.to_string()))?;
        // Absent/hostile APP1 degrades to an empty summary (no capture
        // time -> filename-order sort), matching the RAW failure rule.
        let exif = crate::raw::jpeg_exif::read_jpeg_exif(&mut file).unwrap_or_default();
        return Ok(ExifSummary {
            camera_make: exif.make,
            camera_model: exif.model,
            serial_number: exif.serial,
            capture_time: exif.date_time_original,
            subsec: exif.subsec_original,
            sequence_number: None, // Sony JPEG maker notes: out of scope v1
        });
    }
    // An ARW IS a TIFF: the in-tree walker reads IFD0 + ExifIFD with a
    // few KB of targeted reads on the ONE open handle — no mmap, no
    // whole-file access. Every TIFF-shaped RAW (ARW/NEF/CR2/DNG…)
    // takes this path; absent individual fields degrade to None.
    let mut file = std::fs::File::open(path).map_err(|e| ExifError::Open(e.to_string()))?;
    if let Some(exif) = crate::raw::jpeg_exif::read_tiff_exif(&mut file) {
        // Maker-note pass (in-tree — same file handle): failure of any
        // kind degrades to None; bursts fall back to the Δt-only path.
        let sequence_number = crate::raw::sony::read_sequence(&mut file).map(|s| s.burst_seq());
        return Ok(ExifSummary {
            // Preserve the retired rawler path's vendor normalization
            // ("SONY" in the IFD -> "Sony") so summaries stay
            // byte-stable across the swap — cached rows and tests never
            // see the raw spelling change underneath them.
            camera_make: exif.make.map(|m| normalize_make(&m)),
            camera_model: exif.model,
            serial_number: exif.serial,
            capture_time: exif.date_time_original,
            subsec: exif.subsec_original,
            sequence_number,
        });
    }
    drop(file);
    // Not a classic-TIFF container (CR3/RAF/X3F and friends — the
    // "other cameras: best-effort" promise in 00-overview.md): fall
    // back to rawler's parser so those formats keep their capture
    // times. The mmap cost this fix removed applies only to these
    // rare files, never to the TIFF-shaped hot path. Sony maker-note
    // sequence is TIFF-only, hence None here (as before: read_sequence
    // returned None for non-TIFF containers).
    let source =
        rawler::rawsource::RawSource::new(path).map_err(|e| ExifError::Open(e.to_string()))?;
    let decoder = rawler::get_decoder(&source).map_err(|e| ExifError::Metadata(e.to_string()))?;
    let metadata = decoder
        .raw_metadata(&source, &rawler::decoders::RawDecodeParams::default())
        .map_err(|e| ExifError::Metadata(e.to_string()))?;
    let non_empty = |s: String| (!s.trim().is_empty()).then_some(s);
    Ok(ExifSummary {
        camera_make: non_empty(metadata.make.clone()),
        camera_model: non_empty(metadata.model.clone()),
        serial_number: metadata.exif.serial_number.clone().and_then(non_empty),
        capture_time: metadata.exif.date_time_original.clone().and_then(non_empty),
        subsec: metadata
            .exif
            .sub_sec_time_original
            .clone()
            .and_then(non_empty),
        sequence_number: None,
    })
}

/// Vendor-name normalization matching what rawler's metadata path did
/// for the cameras this tool supports: the all-caps IFD spelling
/// becomes the conventional one. Unknown makes pass through untouched.
fn normalize_make(make: &str) -> String {
    if make.eq_ignore_ascii_case("sony") {
        "Sony".into()
    } else {
        make.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(time: Option<&str>, subsec: Option<&str>) -> ExifSummary {
        ExifSummary {
            capture_time: time.map(String::from),
            subsec: subsec.map(String::from),
            ..Default::default()
        }
    }

    #[test]
    fn sort_key_normalizes_subseconds_to_millis() {
        let t = "2021:04:05 17:41:23";
        assert_eq!(
            summary(Some(t), None).sort_key().unwrap(),
            "2021:04:05 17:41:23.000"
        );
        assert_eq!(
            summary(Some(t), Some("5")).sort_key().unwrap(),
            "2021:04:05 17:41:23.500"
        );
        assert_eq!(
            summary(Some(t), Some("57")).sort_key().unwrap(),
            "2021:04:05 17:41:23.570"
        );
        assert_eq!(
            summary(Some(t), Some("5712")).sort_key().unwrap(),
            "2021:04:05 17:41:23.571"
        );
    }

    #[test]
    fn sort_key_orders_chronologically() {
        let a = summary(Some("2021:04:05 17:41:23"), Some("9"));
        let b = summary(Some("2021:04:05 17:41:24"), Some("0"));
        let c = summary(Some("2021:12:01 05:00:00"), None);
        assert!(a.sort_key() < b.sort_key());
        assert!(b.sort_key() < c.sort_key());
    }

    #[test]
    fn sort_key_requires_capture_time() {
        assert_eq!(summary(None, Some("57")).sort_key(), None);
    }

    #[test]
    fn non_digit_subsec_does_not_corrupt_key() {
        assert_eq!(
            summary(Some("2021:04:05 17:41:23"), Some("x?"))
                .sort_key()
                .unwrap(),
            "2021:04:05 17:41:23.000"
        );
    }
}
