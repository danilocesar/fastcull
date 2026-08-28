//! The video export's container, against the real reference frames
//! (specs/modules/video-export.md, "Acceptance criteria").
//!
//! The muxer's own unit tests are hermetic — they run on a machine with
//! no sample RAWs and no ffmpeg, which is every Windows runner. This file
//! is the other half: it proves that the header those tests pin is the
//! header the REAL Sony A1 frames produce, that every sample in the
//! finished file is the camera's JPEG byte for byte, and — where ffprobe
//! exists — that a real decoder agrees the result is Motion JPEG.
//!
//! ffprobe is never a build or test dependency: where it is missing the
//! check is SKIPPED with a note, never failed.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use fastcull_core::clip::qt;

/// The three A1 files `testdata/fetch.sh` fetches, in the order the
/// golden header was generated from.
const REFERENCE_RAWS: [&str; 3] = [
    "A1_full_compressed.ARW",
    "A1_full_lossless_compressed.ARW",
    "A1_full_uncompressed.ARW",
];

/// 33 ms per frame: the median gap of a 30 fps A1 burst, and the cadence
/// the golden header was generated at.
const REFERENCE_MS: u32 = 33;

fn raws_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/raws");
    assert!(
        dir.join(REFERENCE_RAWS[0]).is_file(),
        "missing sample RAWs — run testdata/fetch.sh first"
    );
    dir
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fastcull-clip-{tag}-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The embedded full-res JPEG of one RAW, as bytes.
fn fullres(path: &Path) -> (Vec<u8>, u32, u32) {
    let mut file = std::fs::File::open(path).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut file).unwrap();
    let jpeg = previews.fullres().expect("an A1 file has a full-res JPEG");
    let (w, h) = (jpeg.width, jpeg.height);
    (
        fastcull_core::raw::read_jpeg(&mut file, jpeg).unwrap(),
        w,
        h,
    )
}

/// Mux the three reference frames into `out`, exactly as the export does:
/// header first, then the camera JPEGs back to back, untouched.
fn mux_reference(out: &Path) -> (qt::TrackSpec, Vec<Vec<u8>>) {
    let dir = raws_dir();
    let frames: Vec<(Vec<u8>, u32, u32)> = REFERENCE_RAWS
        .iter()
        .map(|n| fullres(&dir.join(n)))
        .collect();
    let (w, h) = (frames[0].1, frames[0].2);
    assert_eq!((w, h), (8640, 5760), "the A1 full-res frame size");
    let spec = qt::TrackSpec {
        width: w,
        height: h,
        orientation: 1,
        sample_ms: REFERENCE_MS,
        sample_sizes: frames.iter().map(|(b, ..)| b.len() as u64).collect(),
    };
    let mut file = std::io::BufWriter::new(std::fs::File::create(out).unwrap());
    qt::write_header(&mut file, &spec).unwrap();
    for (bytes, ..) in &frames {
        file.write_all(bytes).unwrap();
    }
    file.into_inner().unwrap().sync_all().unwrap();
    (spec, frames.into_iter().map(|(b, ..)| b).collect())
}

/// The hermetic golden in `qt.rs` pins a header built from three hard-
/// coded sample sizes. This is what keeps those numbers honest: the real
/// A1 files, read through the real preview walker, must produce that
/// header byte for byte. If a future change to preview selection picked a
/// different JPEG out of the RAW, this fails and the golden does not.
#[test]
fn the_real_reference_frames_produce_the_golden_header() {
    let dir = scratch("golden");
    let mov = dir.join("reference.mov");
    let (spec, _) = mux_reference(&mov);
    let header_len = qt::header_len(3, spec.sample_bytes());
    let written = std::fs::read(&mov).unwrap();
    let golden = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/clip-3frames.mov.header"),
    )
    .expect("the golden header file");
    assert_eq!(
        &written[..header_len as usize],
        &golden[..],
        "the real A1 frames no longer produce the pinned header"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Every sample is the camera's own JPEG, byte for byte — the promise the
/// whole feature is built on ("no decode, no scale, no crop, no
/// re-encode"). Checked through the in-tree reader's own idea of where
/// the samples are, so a correct-looking file with a wrong index fails
/// here too.
#[test]
fn every_sample_is_the_camera_jpeg_byte_for_byte() {
    let dir = scratch("samples");
    let mov = dir.join("reference.mov");
    let (_, sources) = mux_reference(&mov);
    let mut file = std::fs::File::open(&mov).unwrap();
    let movie = qt::read_movie(&mut file).unwrap();
    assert_eq!(movie.samples.len(), 3);
    assert!(movie.co64 && movie.moov_before_mdat);
    assert_eq!(&movie.format, b"jpeg");
    let bytes = std::fs::read(&mov).unwrap();
    for (i, sample) in movie.samples.iter().enumerate() {
        let at = sample.offset as usize;
        let end = at + sample.size as usize;
        assert_eq!(
            blake3::hash(&bytes[at..end]),
            blake3::hash(&sources[i]),
            "sample {i} is not the camera's JPEG"
        );
        // ...and it really is a JPEG stream, start and end.
        assert_eq!(&bytes[at..at + 2], &[0xFF, 0xD8], "sample {i} SOI");
        assert_eq!(&bytes[end - 2..end], &[0xFF, 0xD9], "sample {i} EOI");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A real decoder's verdict on the file, where one is installed. This is
/// the check that cannot run on the Windows runner, so it is SKIPPED
/// there rather than failed — the in-tree reader covers the same facts on
/// both platforms, and this is the independent second opinion.
///
/// Recorded from ffprobe 8.1.2 on 2026-08-27 (Fedora):
/// `mjpeg`, tag `jpeg`, 8640x5760, `yuvj422p`, 3 frames, 1000/33 fps.
#[test]
fn ffprobe_agrees_it_is_motion_jpeg() {
    let Some(ffprobe) = ffprobe_path() else {
        eprintln!("ffprobe check skipped: no ffprobe on this machine");
        return;
    };
    let dir = scratch("ffprobe");
    let mov = dir.join("reference.mov");
    mux_reference(&mov);
    let out = std::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,codec_tag_string,width,height,pix_fmt,nb_read_frames,r_frame_rate",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(&mov)
        .output()
        .expect("run ffprobe");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    std::fs::remove_dir_all(&dir).ok();
    assert!(out.status.success(), "ffprobe failed: {out:?}");
    for want in [
        "codec_name=mjpeg",
        "codec_tag_string=jpeg",
        "width=8640",
        "height=5760",
        "pix_fmt=yuvj422p",
        "nb_read_frames=3",
        "r_frame_rate=1000/33",
    ] {
        assert!(
            text.contains(want),
            "ffprobe did not report {want}:\n{text}"
        );
    }
}

/// A portrait export must come back from a real decoder as a rotated
/// video, not as a sideways one. Same skip rule as above.
///
/// ffprobe 8.x reports the display matrix as stream side data
/// (`rotation=-90` for a 90° clockwise turn); older builds report a
/// `rotate` tag instead, so both spellings are accepted.
#[test]
fn ffprobe_sees_the_rotation_of_a_portrait_export() {
    let Some(ffprobe) = ffprobe_path() else {
        eprintln!("ffprobe rotation check skipped: no ffprobe on this machine");
        return;
    };
    let dir = scratch("rotation");
    let raw = raws_dir().join(REFERENCE_RAWS[0]);
    let (jpeg, w, h) = fullres(&raw);
    for (orientation, expect) in [
        (6u16, ["rotation=-90", "rotate=270"]),
        (8, ["rotation=90", "rotate=90"]),
    ] {
        let mov = dir.join(format!("portrait-{orientation}.mov"));
        let spec = qt::TrackSpec {
            width: w,
            height: h,
            orientation,
            sample_ms: REFERENCE_MS,
            sample_sizes: vec![jpeg.len() as u64; 2],
        };
        let mut file = std::io::BufWriter::new(std::fs::File::create(&mov).unwrap());
        qt::write_header(&mut file, &spec).unwrap();
        for _ in 0..2 {
            file.write_all(&jpeg).unwrap();
        }
        file.into_inner().unwrap().sync_all().unwrap();
        let out = std::process::Command::new(&ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream_side_data=rotation:stream_tags=rotate",
                "-of",
                "default=noprint_wrappers=1",
            ])
            .arg(&mov)
            .output()
            .expect("run ffprobe");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            expect.iter().any(|e| text.contains(e)),
            "EXIF orientation {orientation} did not reach the player as a rotation \
             (wanted one of {expect:?}):\n{text}"
        );
        std::fs::remove_file(&mov).ok();
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// `ffprobe` on PATH, or nothing. Never an error: a runner without it
/// still runs every other check in this file.
fn ffprobe_path() -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(exe))
        .find(|p| p.is_file())
}
