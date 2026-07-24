//! EXIF summary: the few metadata fields the culling workflow needs, read via
//! rawler (`specs/modules/catalog-cache.md`).
//!
//! Capture times are kept in EXIF string form (`"YYYY:MM:DD HH:MM:SS"`): the
//! format is fixed-width and zero-padded, so lexicographic order equals
//! chronological order and no calendar math is needed for sorting. Burst
//! grouping (M7) will add real Δt computation when it arrives.

use std::path::Path;

use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
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

/// Read the EXIF summary of one RAW file (a few ms: IFD tables only).
pub fn read_exif_summary(path: &Path) -> Result<ExifSummary, ExifError> {
    let source = RawSource::new(path).map_err(|e| ExifError::Open(e.to_string()))?;
    let decoder = rawler::get_decoder(&source).map_err(|e| ExifError::Metadata(e.to_string()))?;
    let metadata = decoder
        .raw_metadata(&source, &RawDecodeParams::default())
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
    })
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
