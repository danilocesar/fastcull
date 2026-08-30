//! Folder scan and per-image session records.
//!
//! Spec: `specs/modules/catalog-cache.md`. `Session::open` scans exactly one
//! directory (non-recursive), keeps RAW extensions rawler knows plus
//! UNPAIRED .jpg/.jpeg (issue #8), and returns instantly with placeholder
//! records — file *contents* are never read at scan time (only directory
//! metadata: size and mtime, needed for cache keys). A JPEG with a
//! same-stem RAW sibling stays hidden: the RAW represents the moment
//! (pairing/copy-through is a deferred milestone).
//!
//! M1 scope: records carry load state, EXIF summary slot, and pick state.
//! IPTC data, burst ids, and the copied flag join the record in their own
//! milestones; folder watching (`notify`) arrives with the UI session in M2.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::exif::ExifSummary;

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("cannot open folder {0}: {1}")]
    OpenFolder(PathBuf, std::io::Error),
    #[error("not a directory: {0}")]
    NotADirectory(PathBuf),
}

/// Load progress of one image inside a session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LoadState {
    /// Scanned, nothing read yet — the grid shows a placeholder cell.
    #[default]
    Placeholder,
    /// Metadata (and possibly thumb) delivered.
    Loaded,
    /// Unreadable/corrupt; the reason is shown as a badge tooltip. The
    /// session keeps going — one bad file never poisons the folder.
    Failed(String),
}

/// The user's verdict on one image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickState {
    #[default]
    Unmarked,
    Picked,
    Rejected,
}

/// One RAW file in the session.
#[derive(Debug, Clone)]
pub struct ImageRecord {
    pub path: PathBuf,
    /// File size in bytes at scan time (cache key component).
    pub size: u64,
    /// Modification time at scan time (cache key component).
    pub mtime: Option<SystemTime>,
    pub state: LoadState,
    pub exif: Option<ExifSummary>,
    pub pick: PickState,
}

impl ImageRecord {
    /// Display name; lossy so that non-UTF8 names (possible on Linux) still
    /// render as something rather than an empty grid label.
    pub fn file_name(&self) -> std::borrow::Cow<'_, str> {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default()
    }
}

/// An open folder: the unit of work of the whole application.
#[derive(Debug)]
pub struct Session {
    pub folder: PathBuf,
    /// Records in filename order (capture-time sort happens after EXIF load).
    pub images: Vec<ImageRecord>,
    /// Directory-iteration errors that could not be attributed to a named
    /// entry (so no `Failed` record could be created). Non-zero means the
    /// listing may be incomplete; the UI must surface it.
    pub scan_errors: usize,
}

impl Session {
    /// Scan `folder` (non-recursive) for RAW files — and unpaired JPEGs
    /// (issue #8): a JPEG with a same-stem RAW sibling stays hidden (the
    /// RAW represents the moment; also keeps darktable exports dropped
    /// back into a shoot folder out of the grid), a JPEG without one is
    /// a first-class image. Returns placeholder records in filename
    /// order. No file contents are read.
    pub fn open(folder: &Path) -> Result<Self, CatalogError> {
        if !folder.is_dir() {
            return Err(CatalogError::NotADirectory(folder.to_path_buf()));
        }
        let entries = std::fs::read_dir(folder)
            .map_err(|e| CatalogError::OpenFolder(folder.to_path_buf(), e))?;
        let entries: Vec<_> = entries.collect();

        // Pass 1: the RAW stems, for the paired-JPEG rule (deterministic,
        // folder-content-driven — no setting; persona/user decision).
        // Only REAL files count (validator: a directory named DSC001.ARW
        // must not swallow DSC001.JPG — hiding presumes a shown RAW), so
        // a broken/unreadable "RAW" leaves its JPEG twin visible too.
        let mut raw_stems: HashSet<String> = HashSet::new();
        for entry in entries.iter().flatten() {
            let path = entry.path();
            if has_raw_extension(&path)
                && entry_metadata(&path).map(|m| m.is_file()).unwrap_or(false)
            {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    raw_stems.insert(stem.to_ascii_lowercase());
                }
            }
        }

        let mut images = Vec::new();
        let mut scan_errors = 0usize;
        for entry in entries {
            let Ok(entry) = entry else {
                // Iterator-level error: no name to attach a Failed record to.
                scan_errors += 1;
                continue;
            };
            let path = entry.path();
            let unpaired_jpeg = has_jpeg_extension(&path)
                && path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_none_or(|stem| !raw_stems.contains(&stem.to_ascii_lowercase()));
            if !has_raw_extension(&path) && !unpaired_jpeg {
                continue; // non-images (videos etc.) are silently ignored
            }
            // Metadata only (no open/read of contents). `fs::metadata`
            // follows symlinks so a `link.ARW -> real.ARW` is a first-class
            // image; a broken symlink or unreadable entry is recorded as
            // Failed, not dropped: the user must see that the file exists
            // and could not be used. Directories (or things resolving to
            // non-files: fifos, sockets) named like RAWs are skipped.
            let (size, mtime, state) = match entry_metadata(&path) {
                Ok(md) if md.is_file() => (md.len(), md.modified().ok(), LoadState::Placeholder),
                Ok(_) => continue,
                Err(e) => (0, None, LoadState::Failed(format!("unreadable entry: {e}"))),
            };
            images.push(ImageRecord {
                path,
                size,
                mtime,
                state,
                exif: None,
                pick: PickState::default(),
            });
        }
        images.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        Ok(Self {
            folder: folder.to_path_buf(),
            images,
            scan_errors,
        })
    }
}

#[cfg(test)]
thread_local! {
    /// Metadata probes made by [`entry_metadata`] on THIS thread, so a test
    /// can assert the scan's SHAPE — stats per entry — instead of how long
    /// it took on some runner (issue #59, the same reasoning as
    /// `fileops::PROBES`). Thread-local, not atomic: cargo runs tests on
    /// parallel threads and `Session::open` probes only on its caller's
    /// thread (it starts no worker), so each test sees exactly its own
    /// probes. Compiled out of a real build.
    static METADATA_PROBES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Forget the probes counted so far on this thread.
#[cfg(test)]
fn metadata_probes_reset() {
    METADATA_PROBES.with(|c| c.set(0));
}

/// Probes counted on this thread since the last [`metadata_probes_reset`].
#[cfg(test)]
fn metadata_probes() -> u64 {
    METADATA_PROBES.with(std::cell::Cell::get)
}

/// The scan's only look at a file: `stat` (follows symlinks), never an
/// open — file contents are never read at scan time (catalog-cache.md).
/// Every metadata call in [`Session::open`] goes through here so the count
/// is assertable.
fn entry_metadata(path: &Path) -> std::io::Result<std::fs::Metadata> {
    #[cfg(test)]
    METADATA_PROBES.with(|c| c.set(c.get() + 1));
    std::fs::metadata(path)
}

/// True if the path has an extension rawler can decode (case-insensitive).
fn has_raw_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    // rawler's list contains true RAW containers plus "dng"; it does not
    // contain jpg/jpeg/tif.
    rawler::decoders::supported_extensions()
        .iter()
        .any(|known| known.eq_ignore_ascii_case(&ext))
}

/// True for .jpg/.jpeg (case-insensitive) — the only non-RAW stills
/// imported in v1 (issue #8; HEIC/PNG/TIFF explicitly out of scope).
fn has_jpeg_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture directory that cleans itself up even when an assertion
    /// panics — restoring permissions first, because the 1,000-entry test
    /// denies itself read access on purpose and must not leave a pile of
    /// unreadable stubs in the scratch directory.
    struct Fixture {
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(entries) = std::fs::read_dir(&self.dir) {
                    for entry in entries.flatten() {
                        std::fs::set_permissions(
                            entry.path(),
                            std::fs::Permissions::from_mode(0o644),
                        )
                        .ok();
                    }
                }
            }
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    fn make_folder(files: &[&str]) -> PathBuf {
        let dir = crate::testutil::scratch_dir("catalog");
        for f in files {
            std::fs::write(dir.join(f), b"stub").unwrap();
        }
        dir
    }

    #[test]
    fn scan_keeps_raw_extensions_case_insensitively_and_unpaired_jpegs() {
        let dir = make_folder(&[
            "b.ARW", "a.arw", "c.Arw", "d.CR3", "e.nef", "x.jpg", "y.txt", "z.xmp",
        ]);
        metadata_probes_reset();
        let session = Session::open(&dir).unwrap();
        let probes = metadata_probes();
        let names: Vec<String> = session
            .images
            .iter()
            .map(|i| i.file_name().into_owned())
            .collect();
        // x.jpg has no same-stem RAW sibling: first-class image (issue #8).
        assert_eq!(
            names,
            ["a.arw", "b.ARW", "c.Arw", "d.CR3", "e.nef", "x.jpg"]
        );
        // The scan's cost on a MIXED folder, as a count rather than a clock
        // (issue #59): 2 stats for each of the 5 RAW-extension entries (the
        // stem pass must know whether the entry is a real file, the record
        // pass wants size + mtime), 1 for the unpaired x.jpg (record pass
        // only — the stem pass never stats a JPEG), and 0 for y.txt and
        // z.xmp, which no pass looks at. See the 1,000-entry test for what
        // these constants freeze.
        assert_eq!(probes, 5 * 2 + 1, "5 RAWs x2 + 1 unpaired JPEG + 0 others");
        assert!(session
            .images
            .iter()
            .all(|i| i.state == LoadState::Placeholder && i.pick == PickState::Unmarked));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Issue #8 pairing rule: a JPEG with a same-stem RAW sibling stays
    /// hidden (any case combination); unpaired JPEGs import; JPEG-only
    /// folders work; non-images (videos etc.) are silently ignored.
    #[test]
    fn scan_hides_paired_jpegs_imports_unpaired_ignores_nonimages() {
        let dir = make_folder(&[
            "DSC001.ARW",
            "DSC001.JPG", // paired: hidden (RAW represents the moment)
            "dsc002.arw",
            "DSC002.jpeg", // paired case-insensitively: hidden
            "DSC003.JPG",  // unpaired: imported
            "phone.jpeg",  // unpaired: imported
            "clip.MP4",    // never a broken cell
            "notes.txt",
        ]);
        let session = Session::open(&dir).unwrap();
        let names: Vec<String> = session
            .images
            .iter()
            .map(|i| i.file_name().into_owned())
            .collect();
        assert_eq!(
            names,
            ["DSC001.ARW", "DSC003.JPG", "dsc002.arw", "phone.jpeg"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// JPEG-only folder (phone/family cards): every JPEG imports.
    #[test]
    fn scan_jpeg_only_folder_imports_everything() {
        let dir = make_folder(&["IMG_1.jpg", "IMG_2.JPEG", "IMG_3.jpeg"]);
        let session = Session::open(&dir).unwrap();
        assert_eq!(session.images.len(), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A DIRECTORY named like a RAW must not swallow its same-stem JPEG
    /// (validator repro: the moment vanished entirely — hiding presumes
    /// a SHOWN RAW).
    #[test]
    fn scan_directory_named_like_raw_does_not_hide_the_jpeg() {
        let dir = make_folder(&["DSC002.jpg"]);
        std::fs::create_dir_all(dir.join("DSC001.ARW")).unwrap();
        std::fs::write(dir.join("DSC001.JPG"), b"stub").unwrap();
        let session = Session::open(&dir).unwrap();
        let names: Vec<String> = session
            .images
            .iter()
            .map(|i| i.file_name().into_owned())
            .collect();
        assert_eq!(
            names,
            ["DSC001.JPG", "DSC002.jpg"],
            "the fake-RAW directory neither imports nor hides anything"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scan_is_non_recursive_and_skips_dirs_named_like_raws() {
        let dir = make_folder(&["top.ARW"]);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/nested.ARW"), b"stub").unwrap();
        std::fs::create_dir_all(dir.join("weird.ARW")).unwrap();
        let session = Session::open(&dir).unwrap();
        let names: Vec<String> = session
            .images
            .iter()
            .map(|i| i.file_name().into_owned())
            .collect();
        assert_eq!(names, ["top.ARW"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_and_missing_folders() {
        let dir = make_folder(&[]);
        assert!(Session::open(&dir).unwrap().images.is_empty());
        let missing = dir.join("nope");
        assert!(matches!(
            Session::open(&missing),
            Err(CatalogError::NotADirectory(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_carry_size_and_mtime() {
        let dir = make_folder(&["a.ARW"]);
        let session = Session::open(&dir).unwrap();
        assert_eq!(session.images[0].size, 4); // b"stub"
        assert!(session.images[0].mtime.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression (validator/QE finding): symlinked RAWs are followed and
    /// listed; broken symlinks surface as Failed records, never vanish.
    #[test]
    #[cfg(unix)]
    fn symlinks_are_followed_and_broken_ones_surface() {
        let dir = make_folder(&["real.ARW"]);
        std::os::unix::fs::symlink(dir.join("real.ARW"), dir.join("link.ARW")).unwrap();
        std::os::unix::fs::symlink(dir.join("gone.ARW"), dir.join("broken.ARW")).unwrap();
        let session = Session::open(&dir).unwrap();
        let mut names: Vec<String> = session
            .images
            .iter()
            .map(|i| i.file_name().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["broken.ARW", "link.ARW", "real.ARW"]);
        let broken = session
            .images
            .iter()
            .find(|i| i.file_name() == "broken.ARW")
            .unwrap();
        assert!(matches!(broken.state, LoadState::Failed(_)));
        let link = session
            .images
            .iter()
            .find(|i| i.file_name() == "link.ARW")
            .unwrap();
        assert_eq!(link.state, LoadState::Placeholder);
        assert_eq!(link.size, 4);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Regression (validator finding): an entry whose stat fails is recorded
    /// as Failed, not silently dropped. A readable-but-unsearchable dir lists
    /// names but denies stat.
    #[test]
    #[cfg(unix)]
    fn stat_denied_entries_become_failed_records() {
        use std::os::unix::fs::PermissionsExt;
        let dir = make_folder(&["seen.ARW"]);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o444)).unwrap();
        let session = Session::open(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(session.images.len(), 1);
        assert!(matches!(session.images[0].state, LoadState::Failed(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Spec acceptance (catalog-cache.md): a 1,000-entry folder yields
    /// 1,000 placeholders and reads no file contents.
    ///
    /// Clock-free on purpose. The wall-clock half of the old criterion is
    /// now `budget_folder_scan_1000_entries_under_50ms` in
    /// `tests/perf_budgets.rs` (release-only): a time bound in this debug
    /// unit run measured whatever else the runner was doing — the same
    /// flake class issue #58 removed from the suffix walk, and the reason
    /// this test needed an 8x Windows carve-out for Defender (issue #59).
    ///
    /// "Reads no contents" is asserted structurally, two ways:
    ///
    /// * every stub is made unreadable (mode 0o000) before the scan, so an
    ///   `open` for reading in the scan path fails and the record turns
    ///   `Failed` instead of `Placeholder`. `stat` needs no read permission
    ///   on the file, only search permission on the directory, so the
    ///   metadata pass is untouched. Unix only (the pipeline's
    ///   reopen-from-cache test uses the same trick); on Windows the
    ///   placeholder half still runs and the no-read claim is review-only.
    /// * the probe count pins the SHAPE: exactly two stats per RAW-extension
    ///   entry — one in the stem pass (the paired-JPEG rule must know the
    ///   entry is a real file) and one in the record pass (size + mtime) —
    ///   so a per-file open cannot hide as extra metadata calls. The mixed
    ///   fixture in `scan_keeps_raw_extensions_case_insensitively_and_unpaired_jpegs`
    ///   pins the other two rates (one stat per unpaired JPEG, none for
    ///   paired JPEGs and non-images).
    ///
    /// Both constants freeze TODAY's two-pass shape. Reusing the stem pass's
    /// metadata in the record pass is a legitimate optimisation — it would
    /// halve the RAW rate — but it is a deliberate change: it updates these
    /// two constants and the sentence in `specs/01-architecture.md` that
    /// states the rates. A silent drop to one stat per entry is not what
    /// this asserts.
    ///
    /// Known blind spot: a scan that opened each file and SWALLOWED the
    /// error keeps its placeholders and slips past both halves. The perf
    /// budget does not cover it either — measured 2026-08-30, an added
    /// open + 4-byte read per entry only roughly doubles that median
    /// (2.5 ms -> ~5 ms). That budget's fixture is 4-byte stubs, so it
    /// cannot see a content read of any size; the cost would only appear
    /// on a real folder. So
    /// this test is the discriminator for the shipped shape, and an
    /// error-swallowing open stays a review matter.
    #[test]
    fn thousand_entry_scan_yields_placeholders_without_reading_them() {
        let names: Vec<String> = (0..1000).map(|i| format!("DSC{i:05}.ARW")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        // The guard restores the permissions and deletes the folder even if
        // an assertion below panics.
        let fixture = Fixture {
            dir: make_folder(&refs),
        };
        let dir = &fixture.dir;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in &names {
                std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o000))
                    .unwrap();
            }
            // Root ignores the permission bits, which would make the
            // discriminating half of this test silently vacuous. This one
            // fails loud rather than skipping with a note the way
            // `clip::tests::a_read_only_destination_fails_honestly_and_leaves_nothing`
            // does: that test checks how an error is REPORTED and can be
            // skipped, while this one is structural — silently proving
            // nothing is the outcome it exists to prevent.
            assert!(
                std::fs::File::open(dir.join(&names[0])).is_err(),
                "the 0o000 trick must actually deny reads (running as root?)"
            );
        }

        metadata_probes_reset();
        let session = Session::open(dir).unwrap();
        let probes = metadata_probes();

        assert_eq!(session.images.len(), 1000);
        assert_eq!(
            session.scan_errors, 0,
            "no entry may be lost to the listing"
        );
        assert!(
            session
                .images
                .iter()
                .all(|i| i.state == LoadState::Placeholder),
            "an unreadable-but-stattable file is a placeholder, not a Failed record: {:?}",
            session
                .images
                .iter()
                .find(|i| i.state != LoadState::Placeholder)
        );
        assert_eq!(
            probes, 2000,
            "the scan must cost two stats per RAW entry (stem pass + record \
             pass), linear in the entry count"
        );
    }
}
