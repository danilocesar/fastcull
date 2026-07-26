//! Sony maker-note SequenceNumber reader (burst-grouping.md, M7).
//!
//! rawler 0.7 exposes no maker notes (upstream parsing is commented out),
//! so this walks the file with the same hardened discipline as
//! `raw/tiff.rs`: bounded IFD walks, no allocation from untrusted sizes
//! beyond a fixed cap, malformed data degrades to `None` (never an error
//! that would fail metadata for the whole image).
//!
//! Layout (validated against the real A1 files + the exiftool Sony.pm
//! ground truth, 2026-07-26):
//! - IFD0 → tag 0x8769 (Exif IFD) → tag 0x927C (MakerNote, type 7).
//! - In ARW the maker note is a HEADERLESS IFD (entry count right at the
//!   offset, value offsets absolute); the "SONY DSC \0\0\0" +12 header
//!   form is also accepted for JPEG-extracted notes.
//! - Tag 0x9400 (type 7): enciphered blob. The version byte is checked on
//!   the CIPHERED first byte (exiftool's Condition runs pre-decipher);
//!   {0x23,0x24,0x26,0x28,0x31,0x32,0x33,0x41,0x43} select the Tag9400c
//!   layout (ILCE-1 = 0x31, confirmed on the test files).
//! - Cipher: c = p³ mod 249 for p in 0..=248 (a bijection; 249..=255
//!   pass through). Decipher = inverse table.
//! - Deciphered Tag9400c fields: releaseMode2 @0x09 (u8; 0 = normal
//!   single drive), SequenceImageNumber @0x12 (u32 LE, raw; exiftool
//!   displays raw+1 = "number of images captured in burst sequence").

use std::io::{Read, Seek, SeekFrom};

/// Raw sequence facts from the maker note. Interpretation into the
/// burst model's `seq` (0 = single, >=1 = burst position) happens in
/// [`SonySequence::burst_seq`] so the policy is testable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SonySequence {
    /// Tag9400c releaseMode2: 0 = normal (single drive); continuous and
    /// bracketing modes are non-zero.
    pub release_mode2: u8,
    /// SequenceImageNumber, 1-based (raw value + 1 per the exiftool
    /// convention).
    pub sequence_image_number: u32,
}

impl SonySequence {
    /// The burst model's sequence semantics: a normal-drive frame is a
    /// declared single (0); any non-zero release mode contributes its
    /// 1-based position. Empirical basis: single-shot A1 files read
    /// release_mode2 = 0, raw seq 0. Continuous-drive values are per the
    /// exiftool table; the Δt gap + reset rules still guard misreads.
    pub fn burst_seq(&self) -> u32 {
        if self.release_mode2 == 0 {
            0
        } else {
            self.sequence_image_number
        }
    }
}

const MAX_ENTRIES: u16 = 512;
const MAX_BLOB: u32 = 64 * 1024;
const TAG9400C_VERSIONS: [u8; 9] = [0x23, 0x24, 0x26, 0x28, 0x31, 0x32, 0x33, 0x41, 0x43];

fn decipher_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    for p in 0..=248u32 {
        let c = (p * p * p) % 249;
        table[c as usize] = p as u8;
    }
    for (v, slot) in table.iter_mut().enumerate().skip(249) {
        *slot = v as u8;
    }
    table
}

pub(super) struct En(pub(super) bool); // little?

impl En {
    pub(super) fn u16(&self, b: [u8; 2]) -> u16 {
        if self.0 {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }
    pub(super) fn u32(&self, b: [u8; 4]) -> u32 {
        if self.0 {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    }
}

/// Find `tag` in the IFD at `offset`; return (type, count, value bytes).
pub(super) fn find_in_ifd<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    en: &En,
    tag: u16,
) -> Option<(u16, u32, [u8; 4])> {
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut b2 = [0u8; 2];
    reader.read_exact(&mut b2).ok()?;
    let n = en.u16(b2);
    if n == 0 || n > MAX_ENTRIES {
        return None;
    }
    let mut table = vec![0u8; usize::from(n) * 12];
    reader.read_exact(&mut table).ok()?;
    for e in table.chunks_exact(12) {
        if en.u16([e[0], e[1]]) == tag {
            return Some((
                en.u16([e[2], e[3]]),
                en.u32([e[4], e[5], e[6], e[7]]),
                [e[8], e[9], e[10], e[11]],
            ));
        }
    }
    None
}

/// Read the Sony sequence facts from an ARW (or TIFF-shaped) file.
/// `None` for non-Sony files, absent tags, or malformed data — never an
/// error: bursts degrade to the Δt-only path.
pub fn read_sequence<R: Read + Seek>(reader: &mut R) -> Option<SonySequence> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).ok()?;
    let en = match &header[0..2] {
        b"II" => En(true),
        b"MM" => En(false),
        _ => return None,
    };
    if en.u16([header[2], header[3]]) != 42 {
        return None;
    }
    let ifd0 = u64::from(en.u32([header[4], header[5], header[6], header[7]]));

    let (_, _, exif_val) = find_in_ifd(reader, ifd0, &en, 0x8769)?;
    let exif_ifd = u64::from(en.u32(exif_val));
    let (mk_type, mk_count, mk_val) = find_in_ifd(reader, exif_ifd, &en, 0x927C)?;
    if mk_type != 7 || mk_count <= 4 {
        return None;
    }
    let mk_off = u64::from(en.u32(mk_val));

    // Headerless IFD (ARW) or "SONY DSC \0\0\0" + IFD at +12 (JPEG form).
    reader.seek(SeekFrom::Start(mk_off)).ok()?;
    let mut head = [0u8; 12];
    reader.read_exact(&mut head).ok()?;
    let note_ifd = if head.starts_with(b"SONY DSC") {
        mk_off + 12
    } else {
        mk_off
    };

    let (t9400_type, t9400_count, t9400_val) = find_in_ifd(reader, note_ifd, &en, 0x9400)?;
    if t9400_type != 7 || !(0x20..=MAX_BLOB).contains(&t9400_count) {
        return None;
    }
    let blob_off = u64::from(en.u32(t9400_val));
    reader.seek(SeekFrom::Start(blob_off)).ok()?;
    let mut blob = vec![0u8; t9400_count as usize];
    reader.read_exact(&mut blob).ok()?;

    // Version check on the CIPHERED first byte (exiftool semantics).
    if !TAG9400C_VERSIONS.contains(&blob[0]) {
        return None;
    }
    let table = decipher_table();
    for b in blob.iter_mut() {
        *b = table[*b as usize];
    }
    let release_mode2 = blob[0x09];
    let raw_seq = u32::from_le_bytes([blob[0x12], blob[0x13], blob[0x14], blob[0x15]]);
    Some(SonySequence {
        release_mode2,
        sequence_image_number: raw_seq.saturating_add(1),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cipher_is_a_bijection_and_matches_the_formula() {
        let table = decipher_table();
        let mut seen = [false; 256];
        for p in 0..=248u32 {
            let c = ((p * p * p) % 249) as usize;
            assert!(!seen[c], "cipher collision at {c}");
            seen[c] = true;
            assert_eq!(table[c], p as u8);
        }
        // ILCE-1's ciphered version byte deciphers AWAY from 0x31 — the
        // version check must run pre-decipher (validated on real files).
        assert_eq!((0x31u32.pow(3) % 249) as u8, 0x79);
        assert_ne!(table[0x31], 0x31);
    }

    #[test]
    fn single_shot_semantics() {
        let s = SonySequence {
            release_mode2: 0,
            sequence_image_number: 1,
        };
        assert_eq!(s.burst_seq(), 0, "normal drive = declared single");
        let b = SonySequence {
            release_mode2: 1,
            sequence_image_number: 7,
        };
        assert_eq!(b.burst_seq(), 7);
    }

    #[test]
    fn garbage_and_non_tiff_read_as_none() {
        use std::io::Cursor;
        assert_eq!(read_sequence(&mut Cursor::new(b"nope".to_vec())), None);
        assert_eq!(read_sequence(&mut Cursor::new(vec![0u8; 4096])), None);
        // TIFF header but truncated body.
        let mut t = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
        t.extend_from_slice(&[0xFF; 4]);
        assert_eq!(read_sequence(&mut Cursor::new(t)), None);
    }
}
