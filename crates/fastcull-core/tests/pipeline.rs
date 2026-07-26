//! Pipeline integration tests against the real Sony A1 files
//! (specs/modules/raw-pipeline.md + catalog-cache.md acceptance criteria).

use std::path::PathBuf;
use std::time::Duration;

use fastcull_core::pipeline::{JobSpec, Pipeline, Priority, SessionEvent};

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

fn spec_for(path: PathBuf) -> JobSpec {
    let md = std::fs::metadata(&path).unwrap();
    JobSpec {
        path,
        size: md.len(),
        mtime: md.modified().ok(),
    }
}

fn a1_specs() -> Vec<JobSpec> {
    [
        "A1_full_compressed.ARW",
        "A1_full_lossless_compressed.ARW",
        "A1_full_uncompressed.ARW",
    ]
    .into_iter()
    .map(|n| spec_for(testdata(n)))
    .collect()
}

fn collect_events(
    rx: &std::sync::mpsc::Receiver<SessionEvent>,
    expected: usize,
) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    while events.len() < expected {
        match rx.recv_timeout(Duration::from_secs(30)) {
            // Sidecar events depend on stray .xmp files in the environment
            // (a reviewer demo once polluted testdata) — never count them
            // toward exact totals.
            Ok(SessionEvent::Sidecar { .. }) => continue,
            Ok(e) => events.push(e),
            Err(e) => panic!("timed out waiting for events ({e}); got {events:#?}"),
        }
    }
    events
}

/// A duplicate-event regression must not hide behind exact-count collection.
fn assert_no_more_events(rx: &std::sync::mpsc::Receiver<SessionEvent>) {
    if let Ok(extra) = rx.recv_timeout(Duration::from_millis(500)) {
        panic!("surplus event after all expected ones: {extra:?}");
    }
}

fn tmp() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "fastcull-pipe-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a1_files_produce_320px_thumbs_and_metadata() {
    let (pipeline, rx) = Pipeline::start(a1_specs(), None, 4);
    let events = collect_events(&rx, 6); // 3x MetadataReady + 3x ThumbReady
    assert_no_more_events(&rx);
    drop(pipeline);

    let mut thumbs = 0;
    let mut metas = 0;
    for event in events {
        match event {
            SessionEvent::ThumbReady {
                thumb_jpeg,
                width,
                height,
                from_cache,
                ..
            } => {
                assert!(!from_cache);
                // Output size only; that the SOURCE is the 1616x1080 preview
                // (not the 8640x5760 full-res, same aspect) is asserted by
                // grid_source tests in tests/embedded_jpeg.rs.
                assert_eq!((width, height), (320, 213), "320px long edge");
                assert!(thumb_jpeg.starts_with(&[0xFF, 0xD8]), "thumb is a JPEG");
                assert!(
                    (5_000..200_000).contains(&thumb_jpeg.len()),
                    "plausible q80 size, got {}",
                    thumb_jpeg.len()
                );
                thumbs += 1;
            }
            SessionEvent::MetadataReady { exif, .. } => {
                assert_eq!(exif.camera_model.as_deref(), Some("ILCE-1"));
                metas += 1;
            }
            SessionEvent::Failed { index, reason } => {
                panic!("unexpected failure for job {index}: {reason}")
            }
            SessionEvent::Sidecar { .. } => {}
        }
    }
    assert_eq!((thumbs, metas), (3, 3));
}

/// Spec: a corrupt file yields Failed and does not poison the pipeline.
#[test]
fn corrupt_file_fails_alone_others_complete() {
    let dir = tmp();
    let garbage = dir.join("broken.ARW");
    std::fs::write(&garbage, vec![0xAB; 4096]).unwrap();

    let mut jobs = a1_specs();
    jobs.insert(1, spec_for(garbage));

    let (pipeline, rx) = Pipeline::start(jobs, None, 2);
    // 3 good files x 2 events + 1 Failed.
    let events = collect_events(&rx, 7);
    assert_no_more_events(&rx);
    drop(pipeline);

    let failed: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Failed { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(failed, [1], "exactly the garbage job fails");
    let thumbs = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::ThumbReady { .. }))
        .count();
    assert_eq!(thumbs, 3, "all real files still produce thumbs");
    std::fs::remove_dir_all(&dir).ok();
}

/// Spec (catalog-cache): reopening a folder paints entirely from cache with
/// zero RAW reads. Proven by making the RAW files unreadable on the second
/// run — only the cache can serve them.
#[test]
fn second_run_serves_from_cache_without_touching_raws() {
    let dir = tmp();
    let db = dir.join("cache.db");

    // First run: populate the cache from copies of one A1 file.
    let copies: Vec<PathBuf> = (0..3)
        .map(|i| {
            let p = dir.join(format!("copy{i}.ARW"));
            std::fs::copy(testdata("A1_full_compressed.ARW"), &p).unwrap();
            p
        })
        .collect();
    let jobs: Vec<JobSpec> = copies.iter().cloned().map(spec_for).collect();
    let (pipeline, rx) = Pipeline::start(jobs.clone(), Some(db.clone()), 2);
    collect_events(&rx, 6);
    drop(pipeline);

    // Second run: same jobs, but the RAW bytes are now unreadable. Only the
    // cache can produce these events.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in &copies {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
    }
    let (pipeline, rx) = Pipeline::start(jobs, Some(db), 2);
    let events = collect_events(&rx, 6);
    drop(pipeline);
    for event in &events {
        match event {
            SessionEvent::ThumbReady { from_cache, .. }
            | SessionEvent::MetadataReady { from_cache, .. } => {
                assert!(from_cache, "second run must be served from cache")
            }
            SessionEvent::Failed { index, reason } => {
                panic!("cache should have served job {index}: {reason}")
            }
            SessionEvent::Sidecar { .. } => {}
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for p in &copies {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// An unusable cache path degrades the pipeline to cache-less operation —
/// thumbs still arrive (QE untested-area follow-up).
#[test]
fn unusable_cache_path_degrades_gracefully() {
    let bad_db = PathBuf::from("/dev/null/not-a-dir/cache.db");
    let (pipeline, rx) = Pipeline::start(a1_specs(), Some(bad_db), 2);
    let events = collect_events(&rx, 6);
    drop(pipeline);
    let thumbs = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                SessionEvent::ThumbReady {
                    from_cache: false,
                    ..
                }
            )
        })
        .count();
    assert_eq!(thumbs, 3);
}

/// Spec: with a saturated queue, promoted (visible) jobs complete before at
/// least 90% of background jobs.
#[test]
fn promoted_jobs_finish_before_background_bulk() {
    // 30 jobs over the same 3 files keeps the queue saturated long enough
    // for the promotion to matter on a 2-thread pool.
    let jobs: Vec<JobSpec> = a1_specs().into_iter().cycle().take(30).collect();
    let (pipeline, rx) = Pipeline::start(jobs, None, 2);
    pipeline.set_visible(24..27);
    pipeline.promote(20..22, Priority::Prefetch);

    let events = collect_events(&rx, 60);
    drop(pipeline);

    let thumb_order: Vec<usize> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ThumbReady { index, .. } => Some(*index),
            _ => None,
        })
        .collect();
    assert_eq!(thumb_order.len(), 30);
    for promoted in 24..27 {
        let pos = thumb_order.iter().position(|&i| i == promoted).unwrap();
        assert!(
            pos < 3 + 4, // 3 promoted + up to in-flight slop on 2 threads
            "visible job {promoted} finished at position {pos}; order {thumb_order:?}"
        );
    }
}

/// Sidecar-at-open (M1-deferred criterion, approved for M3): a folder with
/// existing sidecars yields Sidecar events so previous culls reappear.
#[test]
fn existing_sidecars_are_reported_at_load() {
    let dir = tmp();
    let raw = dir.join("marked.ARW");
    std::fs::copy(testdata("A1_full_compressed.ARW"), &raw).unwrap();
    fastcull_core::xmp::write_pick(&raw, fastcull_core::catalog::PickState::Rejected).unwrap();

    let (pipeline, rx) = Pipeline::start(vec![spec_for(raw)], None, 1);
    let mut got_sidecar = false;
    let mut terminal = false;
    while !terminal {
        match rx.recv_timeout(Duration::from_secs(120)).expect("event") {
            SessionEvent::Sidecar {
                index: 0,
                pick,
                iptc,
            } => {
                assert_eq!(pick, fastcull_core::catalog::PickState::Rejected);
                // M5: the event now carries the full sidecar IPTC state.
                assert_eq!(*iptc, fastcull_core::iptc::IptcData::default());
                got_sidecar = true;
            }
            SessionEvent::ThumbReady { .. } | SessionEvent::Failed { .. } => terminal = true,
            _ => {}
        }
    }
    assert!(got_sidecar, "no Sidecar event for a folder with sidecars");
    drop(pipeline);
    std::fs::remove_dir_all(&dir).ok();
}

/// Issue #8: a bare JPEG flows through the SAME pipeline as a RAW —
/// thumb decoded from the file itself, metadata from its APP1 (here:
/// none, an extracted preview) — and never fails the session.
#[test]
fn jpeg_source_produces_thumb_and_metadata() {
    // Build a real JPEG from an A1 embedded preview.
    let arw = testdata("A1_full_compressed.ARW");
    let mut f = std::fs::File::open(&arw).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut f).unwrap();
    let grid = previews.grid_source().expect("mid preview");
    let bytes = fastcull_core::raw::read_jpeg(&mut f, grid).unwrap();
    let dir = tmp();
    let jpg = dir.join("solo.jpg");
    std::fs::write(&jpg, &bytes).unwrap();

    let (pipeline, rx) = Pipeline::start(vec![spec_for(jpg)], None, 2);
    let events = collect_events(&rx, 2);
    let mut thumb_ok = false;
    let mut meta_ok = false;
    for event in &events {
        match event {
            SessionEvent::ThumbReady {
                index, thumb_jpeg, ..
            } => {
                assert_eq!(*index, 0);
                assert!(!thumb_jpeg.is_empty(), "thumb must decode from the file");
                thumb_ok = true;
            }
            SessionEvent::MetadataReady { index, exif, .. } => {
                assert_eq!(*index, 0);
                // Extracted preview has no APP1: empty summary, no error.
                assert_eq!(exif.capture_time, None);
                meta_ok = true;
            }
            SessionEvent::Failed { reason, .. } => {
                panic!("JPEG source must not fail: {reason}")
            }
            SessionEvent::Sidecar { .. } => {}
        }
    }
    assert!(thumb_ok && meta_ok);
    drop(pipeline);
    std::fs::remove_dir_all(&dir).ok();
}
