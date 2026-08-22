//! Minimal TIFF/IFD walker: finds `JPEGInterchangeFormat` pointers.
//!
//! Deliberately not a general TIFF library — it reads only IFD tables (a few
//! KB) and follows SubIFD pointers and the next-IFD chain, hardened against
//! hostile files: offset cycles, absurd entry counts, and out-of-range offsets
//! terminate the walk instead of the process.

use super::endian::Endian;
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, thiserror::Error)]
pub enum TiffError {
    #[error("not a TIFF container: {0}")]
    NotTiff(&'static str),
    #[error("malformed TIFF structure: {0}")]
    Malformed(&'static str),
    #[error("I/O error reading RAW file")]
    Io(#[from] std::io::Error),
}

// Tags we care about.
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_IMAGE_WIDTH: u16 = 0x0100;
const TAG_IMAGE_LENGTH: u16 = 0x0101;
const TAG_SUB_IFDS: u16 = 0x014A;
const TAG_JPEG_OFFSET: u16 = 0x0201;
const TAG_JPEG_LENGTH: u16 = 0x0202;

/// Hard caps so hostile files cannot make the walk unbounded.
const MAX_IFDS: usize = 64;
const MAX_ENTRIES_PER_IFD: u16 = 512;
const MAX_SUB_IFDS: u32 = 16;

/// A JPEG pointer as recorded in an IFD; dimensions only when the IFD carried
/// `ImageWidth`/`ImageLength` tags.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JpegPointer {
    pub offset: u64,
    pub len: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Walk all IFDs reachable from the TIFF header (next-IFD chain + SubIFDs,
/// breadth-first) and collect JPEG pointers.
pub(crate) struct WalkResult {
    pub jpegs: Vec<JpegPointer>,
    /// EXIF orientation (1 when absent): first value found during the walk
    /// wins — for ARW that is IFD0 (which always carries it); files whose
    /// IFD0 lacks the tag may pick it up from a sub-IFD.
    pub orientation: u16,
}

pub(crate) fn walk_jpeg_pointers<R: Read + Seek>(reader: &mut R) -> Result<WalkResult, TiffError> {
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0u8; 8];
    reader
        .read_exact(&mut header)
        .map_err(|_| TiffError::NotTiff("file shorter than TIFF header"))?;
    let endian = Endian::from_marker(&header[0..2])
        .ok_or(TiffError::NotTiff("missing II/MM byte-order mark"))?;
    if endian.u16([header[2], header[3]]) != 42 {
        return Err(TiffError::NotTiff("bad TIFF magic"));
    }

    let mut queue = vec![u64::from(
        endian.u32([header[4], header[5], header[6], header[7]]),
    )];
    let mut visited = std::collections::HashSet::new();
    let mut found = Vec::new();
    let mut orientation = 0u16;

    while let Some(offset) = queue.pop() {
        if offset == 0 || !visited.insert(offset) || visited.len() > MAX_IFDS {
            continue;
        }
        // A malformed IFD ends that branch of the walk, not the whole scan:
        // other IFDs may still hold usable previews.
        if read_ifd(
            reader,
            offset,
            endian,
            &mut queue,
            &mut found,
            &mut orientation,
        )
        .is_err()
        {
            continue;
        }
    }
    Ok(WalkResult {
        jpegs: found,
        orientation: if (1..=8).contains(&orientation) {
            orientation
        } else {
            1
        },
    })
}

fn read_ifd<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    endian: Endian,
    queue: &mut Vec<u64>,
    found: &mut Vec<JpegPointer>,
    orientation: &mut u16,
) -> Result<(), TiffError> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut b2 = [0u8; 2];
    reader.read_exact(&mut b2)?;
    let entry_count = endian.u16(b2);
    if entry_count == 0 || entry_count > MAX_ENTRIES_PER_IFD {
        return Err(TiffError::Malformed("implausible IFD entry count"));
    }

    let mut table = vec![0u8; usize::from(entry_count) * 12 + 4];
    reader.read_exact(&mut table)?;

    let mut jpeg_offset = None;
    let mut jpeg_len = None;
    let mut width = None;
    let mut height = None;
    let mut sub_ifds: Option<(u32, [u8; 4])> = None;

    let (entries, _) = table[..usize::from(entry_count) * 12].as_chunks::<12>();
    for entry in entries {
        let tag = endian.u16([entry[0], entry[1]]);
        let typ = endian.u16([entry[2], entry[3]]);
        let count = endian.u32([entry[4], entry[5], entry[6], entry[7]]);
        let value = [entry[8], entry[9], entry[10], entry[11]];
        // We only need SHORT (3) and LONG (4) scalar values.
        let scalar = match typ {
            3 => Some(u32::from(endian.u16([value[0], value[1]]))),
            4 => Some(endian.u32(value)),
            _ => None,
        };
        match tag {
            TAG_JPEG_OFFSET => jpeg_offset = scalar.map(u64::from),
            TAG_JPEG_LENGTH => jpeg_len = scalar.map(u64::from),
            TAG_ORIENTATION => {
                if *orientation == 0 {
                    *orientation = scalar.unwrap_or(0) as u16;
                }
            }
            TAG_IMAGE_WIDTH => width = scalar,
            TAG_IMAGE_LENGTH => height = scalar,
            // Type 4 = LONG; type 13 = IFD (used by some vendors for SubIFDs).
            TAG_SUB_IFDS if (typ == 4 || typ == 13) && count >= 1 => {
                sub_ifds = Some((count, value));
            }
            _ => {}
        }
    }

    // Record this IFD's own contributions FIRST: a failure dereferencing the
    // SubIFD array below must not discard the JPEG pointer or the rest of the
    // next-IFD chain (a truncated SubIFD array would otherwise lose every
    // preview in an otherwise intact file).
    if let (Some(offset), Some(len)) = (jpeg_offset, jpeg_len) {
        found.push(JpegPointer {
            offset,
            len,
            width,
            height,
        });
    }

    // Next IFD in the chain (last 4 bytes after the entry table).
    let next = &table[usize::from(entry_count) * 12..];
    queue.push(u64::from(endian.u32([next[0], next[1], next[2], next[3]])));

    // SubIFD offsets: inline when count == 1, else an offset to a LONG array.
    // An unreadable array only loses those sub-branches.
    if let Some((count, value)) = sub_ifds {
        let count = count.min(MAX_SUB_IFDS);
        if count == 1 {
            queue.push(u64::from(endian.u32(value)));
        } else if let Ok(buf) = read_at(reader, u64::from(endian.u32(value)), count as usize * 4) {
            let (words, _) = buf.as_chunks::<4>();
            for chunk in words {
                queue.push(u64::from(
                    endian.u32([chunk[0], chunk[1], chunk[2], chunk[3]]),
                ));
            }
        }
    }
    Ok(())
}

fn read_at<R: Read + Seek>(reader: &mut R, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    /// Builder for synthetic TIFFs, little- or big-endian.
    pub(crate) struct TiffBuilder {
        le: bool,
        pub bytes: Vec<u8>,
    }

    impl TiffBuilder {
        pub fn new(le: bool) -> Self {
            let mut bytes = if le { b"II".to_vec() } else { b"MM".to_vec() };
            let mut b = Self { le, bytes: vec![] };
            bytes.extend_from_slice(&b.n16(42));
            bytes.extend_from_slice(&[0, 0, 0, 0]); // IFD0 offset patched later
            b.bytes = bytes;
            b
        }

        pub fn n16(&self, v: u16) -> [u8; 2] {
            if self.le {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        }
        pub fn n32(&self, v: u32) -> [u8; 4] {
            if self.le {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        }

        pub fn set_ifd0(&mut self, offset: u32) {
            let b = self.n32(offset);
            self.bytes[4..8].copy_from_slice(&b);
        }

        /// Append an IFD at the current end; entries are (tag, type, count, value).
        /// Returns its offset.
        pub fn add_ifd(&mut self, entries: &[(u16, u16, u32, u32)], next: u32) -> u32 {
            let offset = self.bytes.len() as u32;
            let count = self.n16(entries.len() as u16);
            self.bytes.extend_from_slice(&count);
            for &(tag, typ, cnt, val) in entries {
                let (t, ty, c) = (self.n16(tag), self.n16(typ), self.n32(cnt));
                self.bytes.extend_from_slice(&t);
                self.bytes.extend_from_slice(&ty);
                self.bytes.extend_from_slice(&c);
                let v = if typ == 3 {
                    let short = self.n16(val as u16);
                    [short[0], short[1], 0, 0]
                } else {
                    self.n32(val)
                };
                self.bytes.extend_from_slice(&v);
            }
            let n = self.n32(next);
            self.bytes.extend_from_slice(&n);
            offset
        }

        /// Append raw bytes (e.g. a JPEG payload); returns their offset.
        pub fn add_blob(&mut self, blob: &[u8]) -> u32 {
            let offset = self.bytes.len() as u32;
            self.bytes.extend_from_slice(blob);
            offset
        }

        pub fn cursor(&self) -> Cursor<Vec<u8>> {
            Cursor::new(self.bytes.clone())
        }
    }

    /// Minimal JPEG: SOI + SOF0 with the given dimensions + EOI.
    pub(crate) fn tiny_jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8]; // SOI
        j.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08]); // SOF0, len 11
        j.extend_from_slice(&height.to_be_bytes());
        j.extend_from_slice(&width.to_be_bytes());
        j.extend_from_slice(&[0x01, 0x11, 0x00]); // 1 component
        j.extend_from_slice(&[0xFF, 0xD9]); // EOI
        j
    }

    fn walk(builder: &TiffBuilder) -> Vec<JpegPointer> {
        walk_jpeg_pointers(&mut builder.cursor()).unwrap().jpegs
    }

    #[test]
    fn orientation_is_captured_from_first_ifd() {
        let mut b = TiffBuilder::new(true);
        let ifd = b.add_ifd(&[(TAG_ORIENTATION, 3, 1, 6)], 0);
        b.set_ifd0(ifd);
        assert_eq!(walk_jpeg_pointers(&mut b.cursor()).unwrap().orientation, 6);
        // Absent or absurd values normalize to 1.
        let mut b2 = TiffBuilder::new(true);
        let ifd2 = b2.add_ifd(&[(TAG_ORIENTATION, 3, 1, 99)], 0);
        b2.set_ifd0(ifd2);
        assert_eq!(walk_jpeg_pointers(&mut b2.cursor()).unwrap().orientation, 1);
    }

    #[test]
    fn finds_jpeg_pointer_in_ifd0_both_endians() {
        for le in [true, false] {
            let mut b = TiffBuilder::new(le);
            let jpeg = tiny_jpeg(100, 50);
            let payload = b.add_blob(&jpeg);
            let ifd = b.add_ifd(
                &[
                    (TAG_JPEG_OFFSET, 4, 1, payload),
                    (TAG_JPEG_LENGTH, 4, 1, jpeg.len() as u32),
                ],
                0,
            );
            b.set_ifd0(ifd);
            let found = walk(&b);
            assert_eq!(found.len(), 1, "endian le={le}");
            assert_eq!(found[0].offset, u64::from(payload));
            assert_eq!(found[0].len, jpeg.len() as u64);
        }
    }

    #[test]
    fn follows_next_ifd_chain_and_subifds() {
        let mut b = TiffBuilder::new(true);
        let j1 = tiny_jpeg(10, 10);
        let p1 = b.add_blob(&j1);
        let j2 = tiny_jpeg(20, 20);
        let p2 = b.add_blob(&j2);
        // sub IFD with a JPEG
        let sub = b.add_ifd(
            &[
                (TAG_JPEG_OFFSET, 4, 1, p2),
                (TAG_JPEG_LENGTH, 4, 1, j2.len() as u32),
            ],
            0,
        );
        // chained IFD with a JPEG
        let chained = b.add_ifd(
            &[
                (TAG_JPEG_OFFSET, 4, 1, p1),
                (TAG_JPEG_LENGTH, 4, 1, j1.len() as u32),
                (TAG_IMAGE_WIDTH, 3, 1, 10),
                (TAG_IMAGE_LENGTH, 3, 1, 10),
            ],
            0,
        );
        let ifd0 = b.add_ifd(&[(TAG_SUB_IFDS, 4, 1, sub)], chained);
        b.set_ifd0(ifd0);
        let found = walk(&b);
        assert_eq!(found.len(), 2);
        let with_dims = found.iter().find(|p| p.width.is_some()).unwrap();
        assert_eq!((with_dims.width, with_dims.height), (Some(10), Some(10)));
    }

    #[test]
    fn ifd_cycle_terminates() {
        let mut b = TiffBuilder::new(true);
        // IFD whose next pointer is itself.
        let placeholder = b.add_ifd(&[(TAG_IMAGE_WIDTH, 3, 1, 1)], 0);
        // Patch next pointer to itself: last 4 bytes of the IFD block.
        let next_pos = b.bytes.len() - 4;
        let self_ref = b.n32(placeholder);
        b.bytes[next_pos..].copy_from_slice(&self_ref);
        b.set_ifd0(placeholder);
        assert!(walk(&b).is_empty()); // terminates, finds nothing
    }

    #[test]
    fn absurd_entry_count_is_rejected_not_alloc_bombed() {
        let mut b = TiffBuilder::new(true);
        let pos = b.bytes.len() as u32;
        let count = b.n16(0xFFFF);
        b.bytes.extend_from_slice(&count); // entry count 65535, no entries follow
        b.set_ifd0(pos);
        assert!(walk(&b).is_empty());
    }

    #[test]
    fn not_tiff_errors() {
        for bytes in [
            b"".to_vec(),
            b"GIF89a".to_vec(),
            vec![0xFF, 0xD8, 0xFF, 0xE0],
        ] {
            assert!(matches!(
                walk_jpeg_pointers(&mut Cursor::new(bytes)),
                Err(TiffError::NotTiff(_))
            ));
        }
    }

    #[test]
    fn ifd_offset_beyond_eof_is_survivable() {
        let mut b = TiffBuilder::new(true);
        b.set_ifd0(0xFFFF_0000);
        assert!(walk(&b).is_empty());
    }

    /// SubIFDs recorded with TIFF type 13 (IFD) must be followed like LONGs.
    #[test]
    fn type_13_subifd_is_followed() {
        let mut b = TiffBuilder::new(true);
        let j = tiny_jpeg(25, 25);
        let p = b.add_blob(&j);
        let sub = b.add_ifd(
            &[
                (TAG_JPEG_OFFSET, 4, 1, p),
                (TAG_JPEG_LENGTH, 4, 1, j.len() as u32),
            ],
            0,
        );
        let ifd0 = b.add_ifd(&[(TAG_SUB_IFDS, 13, 1, sub)], 0);
        b.set_ifd0(ifd0);
        assert_eq!(walk(&b).len(), 1);
    }

    /// Regression (validator finding): a SubIFD *array* whose offset lies
    /// beyond EOF must not discard the IFD's own JPEG pointer nor the rest of
    /// the next-IFD chain.
    #[test]
    fn truncated_subifd_array_keeps_own_jpeg_and_chain() {
        let mut b = TiffBuilder::new(true);
        let j1 = tiny_jpeg(30, 30);
        let p1 = b.add_blob(&j1);
        let j2 = tiny_jpeg(40, 40);
        let p2 = b.add_blob(&j2);
        let chained = b.add_ifd(
            &[
                (TAG_JPEG_OFFSET, 4, 1, p2),
                (TAG_JPEG_LENGTH, 4, 1, j2.len() as u32),
            ],
            0,
        );
        let ifd0 = b.add_ifd(
            &[
                (TAG_JPEG_OFFSET, 4, 1, p1),
                (TAG_JPEG_LENGTH, 4, 1, j1.len() as u32),
                (TAG_SUB_IFDS, 4, 2, 0xFFFF_0000), // array offset beyond EOF
            ],
            chained,
        );
        b.set_ifd0(ifd0);
        let found = walk(&b);
        assert_eq!(
            found.len(),
            2,
            "IFD0's JPEG and the chained IFD's JPEG must both survive"
        );
    }
}
