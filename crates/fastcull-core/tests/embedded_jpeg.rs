//! Integration tests for embedded-JPEG discovery against the real Sony A1
//! reference files (specs/modules/raw-pipeline.md acceptance criteria).
//!
//! Requires `testdata/fetch.sh` to have run; tests panic with a clear message
//! if the files are absent so a missing fetch is never a silent pass.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use fastcull_core::raw::{find_embedded_jpegs, read_jpeg};

const A1_FILES: [&str; 3] = [
    "A1_full_compressed.ARW",
    "A1_full_lossless_compressed.ARW",
    "A1_full_uncompressed.ARW",
];

fn testdata(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/raws")
        .join(name);
    assert!(
        path.is_file(),
        "missing test file {path:?} — run testdata/fetch.sh first"
    );
    path
}

/// Wraps a reader and counts every byte read, to prove the surgical-read
/// budget from the spec: grid path ≤ 20 MB of a ~100 MB file.
struct CountingReader<R> {
    inner: R,
    bytes_read: u64,
}

impl<R> CountingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            bytes_read: 0,
        }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn a1_files_expose_mid_preview_and_fullres() {
    for name in A1_FILES {
        let mut f = File::open(testdata(name)).unwrap();
        let previews = find_embedded_jpegs(&mut f).unwrap();

        let grid = previews
            .grid_source()
            .unwrap_or_else(|| panic!("{name}: no grid source"));
        assert_eq!(
            (grid.width, grid.height),
            (1616, 1080),
            "{name}: grid source must be the 1616x1080 preview"
        );

        let fullres = previews
            .fullres()
            .unwrap_or_else(|| panic!("{name}: no fullres"));
        assert_eq!(
            (fullres.width, fullres.height),
            (8640, 5760),
            "{name}: fullres must be the embedded full-resolution JPEG"
        );
        assert!(
            fullres.len > 8_000_000,
            "{name}: fullres implausibly small ({} bytes)",
            fullres.len
        );
    }
}

#[test]
fn grid_path_reads_stay_under_budget() {
    for name in A1_FILES {
        let file_len = std::fs::metadata(testdata(name)).unwrap().len();
        let mut reader = CountingReader::new(File::open(testdata(name)).unwrap());
        let previews = find_embedded_jpegs(&mut reader).unwrap();
        let grid = previews.grid_source().unwrap().clone();
        let payload = read_jpeg(&mut reader, &grid).unwrap();

        assert!(payload.starts_with(&[0xFF, 0xD8]), "{name}: not a JPEG");
        assert_eq!(payload.len() as u64, grid.len);
        assert!(
            reader.bytes_read <= 20 * 1024 * 1024,
            "{name}: grid path read {} bytes of a {} byte file (budget 20 MB)",
            reader.bytes_read,
            file_len
        );
        // The real number should be dramatically lower; record it in failure
        // messages of the tighter sanity bound to catch accidental full reads.
        assert!(
            reader.bytes_read < file_len / 4,
            "{name}: grid path read {} of {} bytes — no longer surgical",
            reader.bytes_read,
            file_len
        );
    }
}

#[test]
fn fullres_payload_is_valid_jpeg() {
    let mut f = File::open(testdata(A1_FILES[1])).unwrap();
    let previews = find_embedded_jpegs(&mut f).unwrap();
    let fullres = previews.fullres().unwrap().clone();
    let payload = read_jpeg(&mut f, &fullres).unwrap();
    assert!(payload.starts_with(&[0xFF, 0xD8]));
    assert!(payload.ends_with(&[0xFF, 0xD9]));
}

#[test]
fn hostile_inputs_error_cleanly() {
    let dir = tempdir();

    // Zero-byte file
    let empty = dir.join("empty.ARW");
    std::fs::write(&empty, b"").unwrap();
    assert!(find_embedded_jpegs(&mut File::open(&empty).unwrap()).is_err());

    // A JPEG renamed to .ARW is not a TIFF container
    let fake = dir.join("fake.ARW");
    std::fs::write(&fake, [0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0, 0]).unwrap();
    assert!(find_embedded_jpegs(&mut File::open(&fake).unwrap()).is_err());

    // Truncated real file: header parses, out-of-range payloads are dropped,
    // no panic.
    let mut head = vec![0u8; 128 * 1024];
    File::open(testdata(A1_FILES[0]))
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    let trunc = dir.join("trunc.ARW");
    std::fs::write(&trunc, &head).unwrap();
    let previews = find_embedded_jpegs(&mut File::open(&trunc).unwrap()).unwrap();
    for c in &previews.candidates {
        assert!(c.offset + c.len <= head.len() as u64);
    }

    std::fs::remove_dir_all(&dir).ok();
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fastcull-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
