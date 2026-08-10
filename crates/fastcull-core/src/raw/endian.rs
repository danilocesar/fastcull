//! Byte order of a TIFF-family container, and the two reads every walker
//! needs.
//!
//! TIFF stores its integers in whichever order the writing camera chose,
//! announced by the `II`/`MM` mark in the first two bytes of the header.
//! All three walkers in this module (the JPEG-pointer walk in `tiff.rs`,
//! the Sony sequence tags in `sony.rs`, the EXIF summary in
//! `jpeg_exif.rs`) had their own copy of this — same struct, same two
//! methods, same II/MM match — which is three chances to get an
//! endianness wrong in a file format where a wrong guess reads plausible
//! garbage rather than failing.

/// Byte order of a TIFF header and everything it points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Endian(pub(crate) bool); // true = little

impl Endian {
    /// The order named by a TIFF byte-order mark (`II` little, `MM` big).
    /// `None` for anything else — the caller decides whether that is an
    /// error or simply "not a file I handle".
    pub(crate) fn from_marker(mark: &[u8]) -> Option<Self> {
        match mark {
            b"II" => Some(Endian(true)),
            b"MM" => Some(Endian(false)),
            _ => None,
        }
    }

    pub(crate) fn u16(self, b: [u8; 2]) -> u16 {
        if self.0 {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }

    pub(crate) fn u32(self, b: [u8; 4]) -> u32 {
        if self.0 {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }
}
