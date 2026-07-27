//! EXIF extraction for BARE JPEG files (issue #8): the APP1 segment
//! carries a self-contained TIFF block; this walks it with the same
//! hardened discipline as `raw/sony.rs` (bounded IFDs, capped ASCII
//! reads, malformed data degrades to `None` — never an error that fails
//! metadata for the whole image).
//!
//! Fields read: IFD0 Make (0x010F), Model (0x0110), Orientation
//! (0x0112); Exif IFD (0x8769) DateTimeOriginal (0x9003),
//! SubSecTimeOriginal (0x9291), BodySerialNumber (0xA431).

use std::io::{Read, Seek, SeekFrom};

use super::jpeg;
use super::sony::{find_in_ifd, En};

/// Longest ASCII value we will allocate for (model/serial/date strings
/// are tens of bytes; anything bigger is hostile).
const MAX_ASCII: u32 = 1024;

/// What a bare JPEG's APP1 tells us. All fields optional; orientation
/// defaults to 1 (as stored).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JpegExif {
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    /// `"YYYY:MM:DD HH:MM:SS"` — same shape as the RAW path.
    pub date_time_original: Option<String>,
    pub subsec_original: Option<String>,
    pub orientation: u16,
}

/// A base-offset shift over `inner`: TIFF offsets inside an APP1 block
/// are relative to the TIFF header, so the IFD walkers need a reader
/// whose position 0 IS that header. HONESTY NOTE: only `SeekFrom::End`
/// is clamped to the window — reads themselves can range over the rest
/// of the file (same whole-file property as the sony.rs walker); safety
/// comes from the bounded allocations (`MAX_ASCII`, entry caps), not
/// from windowing.
struct OffsetReader<R> {
    inner: R,
    base: u64,
    len: u64,
}

impl<R: Read> Read for OffsetReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl<R: Seek> Seek for OffsetReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let translated = match pos {
            SeekFrom::Start(p) => SeekFrom::Start(self.base + p),
            SeekFrom::End(d) => SeekFrom::Start(
                (self.base + self.len).saturating_add_signed(d.min(0)), // never past the window
            ),
            SeekFrom::Current(d) => SeekFrom::Current(d),
        };
        let abs = self.inner.seek(translated)?;
        Ok(abs.saturating_sub(self.base))
    }
}

/// ASCII tag value (trimmed, NUL-stripped); `None` when absent/hostile.
fn ascii_value<R: Read + Seek>(reader: &mut R, en: &En, ifd: u64, tag: u16) -> Option<String> {
    let (ty, count, val) = find_in_ifd(reader, ifd, en, tag)?;
    if ty != 2 || count == 0 || count > MAX_ASCII {
        return None;
    }
    let bytes = if count <= 4 {
        val[..count as usize].to_vec()
    } else {
        reader.seek(SeekFrom::Start(u64::from(en.u32(val)))).ok()?;
        let mut buf = vec![0u8; count as usize];
        reader.read_exact(&mut buf).ok()?;
        buf
    };
    let s: String = bytes
        .into_iter()
        .take_while(|b| *b != 0)
        .map(|b| b as char)
        .collect();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Read the EXIF facts from a bare JPEG. `None` when there is no
/// parseable `Exif\0\0` APP1 — the caller degrades (no capture time,
/// orientation 1), it never errors.
pub fn read_jpeg_exif<R: Read + Seek>(reader: &mut R) -> Option<JpegExif> {
    let (base, len) = jpeg::app1_tiff_bounds(reader).ok()??;
    let mut tiff = OffsetReader {
        inner: reader,
        base,
        len,
    };
    read_tiff_exif(&mut tiff)
}

/// Read the same EXIF facts from a reader whose position 0 IS a TIFF
/// header — which is exactly what an ARW file is. This replaces the
/// rawler/mmap path for the import metadata pass (perf investigation
/// 2026-07-27): rawler's `RawSource` mmaps the whole ~82 MB file and
/// every worker thread serializes on the process `mmap_lock`; over a
/// FUSE/NTFS mount each IFD page fault is a userspace round trip. This
/// walker touches a handful of KB via targeted seek+read instead —
/// the same discipline as `find_embedded_jpegs` and `sony.rs`.
pub fn read_tiff_exif<R: Read + Seek>(tiff: &mut R) -> Option<JpegExif> {
    tiff.seek(SeekFrom::Start(0)).ok()?;
    let mut header = [0u8; 8];
    tiff.read_exact(&mut header).ok()?;
    let en = match &header[0..2] {
        b"II" => En(true),
        b"MM" => En(false),
        _ => return None,
    };
    if en.u16([header[2], header[3]]) != 42 {
        return None;
    }
    let ifd0 = u64::from(en.u32([header[4], header[5], header[6], header[7]]));

    let mut out = JpegExif {
        make: ascii_value(tiff, &en, ifd0, 0x010F),
        model: ascii_value(tiff, &en, ifd0, 0x0110),
        orientation: 1,
        ..Default::default()
    };
    if let Some((ty, count, val)) = find_in_ifd(tiff, ifd0, &en, 0x0112) {
        if ty == 3 && count >= 1 {
            let v = en.u16([val[0], val[1]]);
            if (1..=8).contains(&v) {
                out.orientation = v;
            }
        }
    }
    if let Some((_, _, exif_val)) = find_in_ifd(tiff, ifd0, &en, 0x8769) {
        let exif_ifd = u64::from(en.u32(exif_val));
        out.date_time_original = ascii_value(tiff, &en, exif_ifd, 0x9003);
        out.subsec_original = ascii_value(tiff, &en, exif_ifd, 0x9291);
        out.serial = ascii_value(tiff, &en, exif_ifd, 0xA431);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Minimal JPEG: SOI + APP1(Exif TIFF) + EOI. The TIFF block is
    /// little-endian with IFD0 {Make, Model, Orientation, ExifIFD} and
    /// an Exif IFD {DateTimeOriginal, SubSecTimeOriginal}.
    fn synthetic_jpeg() -> Vec<u8> {
        let mut tiff: Vec<u8> = Vec::new();
        tiff.extend(b"II");
        tiff.extend(42u16.to_le_bytes());
        tiff.extend(8u32.to_le_bytes()); // IFD0 at 8
        let make = b"SONY\0";
        let model = b"TESTCAM-1\0";
        let dto = b"2026:07:26 10:11:12\0";
        let subsec = b"57\0";
        // Layout: IFD0 (4 entries) at 8; Exif IFD (2 entries) after it;
        // then the data area.
        let ifd0_at = 8u32;
        let ifd0_size = 2 + 4 * 12 + 4;
        let exif_at = ifd0_at + ifd0_size;
        let exif_size = 2 + 2 * 12 + 4;
        let data_at = exif_at + exif_size;
        let make_at = data_at;
        let model_at = make_at + make.len() as u32;
        let dto_at = model_at + model.len() as u32;
        // IFD0
        tiff.extend(4u16.to_le_bytes());
        for (tag, ty, count, value) in [
            (0x010Fu16, 2u16, make.len() as u32, make_at),
            (0x0110, 2, model.len() as u32, model_at),
            (0x0112, 3, 1, 6), // orientation 6 = 90° CW, inline
            (0x8769, 4, 1, exif_at),
        ] {
            tiff.extend(tag.to_le_bytes());
            tiff.extend(ty.to_le_bytes());
            tiff.extend(count.to_le_bytes());
            tiff.extend(value.to_le_bytes());
        }
        tiff.extend(0u32.to_le_bytes()); // no IFD1
                                         // Exif IFD
        tiff.extend(2u16.to_le_bytes());
        for (tag, ty, count, value) in [
            (0x9003u16, 2u16, dto.len() as u32, dto_at),
            (
                0x9291,
                2,
                subsec.len() as u32,
                u32::from_le_bytes([b'5', b'7', 0, 0]),
            ),
        ] {
            tiff.extend(tag.to_le_bytes());
            tiff.extend(ty.to_le_bytes());
            tiff.extend(count.to_le_bytes());
            tiff.extend(value.to_le_bytes());
        }
        tiff.extend(0u32.to_le_bytes());
        // Data area
        tiff.extend(make);
        tiff.extend(model);
        tiff.extend(dto);

        let mut jpg: Vec<u8> = vec![0xFF, 0xD8]; // SOI
        let payload_len = 2 + 6 + tiff.len(); // len bytes + "Exif\0\0" + tiff
        jpg.extend([0xFF, 0xE1]);
        jpg.extend((payload_len as u16).to_be_bytes());
        jpg.extend(b"Exif\0\0");
        jpg.extend(&tiff);
        jpg.extend([0xFF, 0xD9]); // EOI
        jpg
    }

    #[test]
    fn reads_the_documented_fields_from_a_synthetic_app1() {
        let jpg = synthetic_jpeg();
        let exif = read_jpeg_exif(&mut Cursor::new(jpg)).expect("parse");
        assert_eq!(exif.make.as_deref(), Some("SONY"));
        assert_eq!(exif.model.as_deref(), Some("TESTCAM-1"));
        assert_eq!(
            exif.date_time_original.as_deref(),
            Some("2026:07:26 10:11:12")
        );
        assert_eq!(exif.subsec_original.as_deref(), Some("57"));
        assert_eq!(exif.orientation, 6);
        assert_eq!(exif.serial, None);
    }

    #[test]
    fn hostile_inputs_degrade_to_none() {
        use std::io::Cursor;
        assert_eq!(read_jpeg_exif(&mut Cursor::new(b"nope".to_vec())), None);
        // JPEG without APP1.
        let bare = vec![0xFF, 0xD8, 0xFF, 0xD9];
        assert_eq!(read_jpeg_exif(&mut Cursor::new(bare)), None);
        // APP1 that claims Exif but truncates mid-TIFF.
        let mut trunc: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x0C];
        trunc.extend(b"Exif\0\0");
        trunc.extend(b"II\x2a\x00"); // header cut before the IFD offset
        assert_eq!(read_jpeg_exif(&mut Cursor::new(trunc)), None);
        // Orientation out of range is ignored, not adopted.
        let mut jpg = synthetic_jpeg();
        // Patch the inline orientation value (find tag 0x0112 entry).
        let pos = jpg
            .windows(2)
            .position(|w| w == [0x12, 0x01])
            .expect("orientation tag");
        jpg[pos + 8] = 99;
        let exif = read_jpeg_exif(&mut Cursor::new(jpg)).expect("parse");
        assert_eq!(exif.orientation, 1, "out-of-range orientation -> 1");
    }
}
