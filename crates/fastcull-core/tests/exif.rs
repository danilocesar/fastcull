//! EXIF summary integration tests against the real Sony A1 reference files.

use std::path::PathBuf;

use fastcull_core::exif::read_exif_summary;

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

#[test]
fn a1_files_yield_camera_identity_and_capture_time() {
    for name in [
        "A1_full_compressed.ARW",
        "A1_full_lossless_compressed.ARW",
        "A1_full_uncompressed.ARW",
    ] {
        let summary = read_exif_summary(&testdata(name)).unwrap();
        // rawler normalizes vendor names: "SONY" in raw EXIF becomes "Sony".
        assert_eq!(summary.camera_make.as_deref(), Some("Sony"), "{name}");
        assert_eq!(summary.camera_model.as_deref(), Some("ILCE-1"), "{name}");
        // Serial is the burst-grouping generic-path key; a rawler upgrade
        // regressing it must not pass silently (value verified via exiv2).
        assert_eq!(summary.serial_number.as_deref(), Some("04470536"), "{name}");

        let time = summary
            .capture_time
            .as_deref()
            .unwrap_or_else(|| panic!("{name}: no capture time"));
        // EXIF format: "YYYY:MM:DD HH:MM:SS", fixed width.
        assert_eq!(time.len(), 19, "{name}: unexpected format {time:?}");
        assert_eq!(&time[4..5], ":");
        assert_eq!(&time[10..11], " ");

        let key = summary.sort_key().unwrap();
        assert_eq!(key.len(), 23, "{name}: bad sort key {key:?}");
    }
}

#[test]
fn garbage_file_yields_error_not_panic() {
    let dir = std::env::temp_dir().join(format!("fastcull-exif-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bogus = dir.join("bogus.ARW");
    std::fs::write(&bogus, b"not a raw file at all").unwrap();
    assert!(read_exif_summary(&bogus).is_err());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn nonexistent_path_yields_open_error() {
    let missing = std::env::temp_dir().join("fastcull-definitely-not-here.ARW");
    assert!(matches!(
        read_exif_summary(&missing),
        Err(fastcull_core::exif::ExifError::Open(_))
    ));
}

#[test]
fn unicode_path_reads_fine() {
    let dir = std::env::temp_dir().join(format!("fastcull-exif-ünï-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("苍鹭 über blurry.ARW");
    std::fs::copy(testdata("A1_full_compressed.ARW"), &target).unwrap();
    let summary = read_exif_summary(&target).unwrap();
    assert_eq!(summary.camera_model.as_deref(), Some("ILCE-1"));
    std::fs::remove_dir_all(&dir).ok();
}
