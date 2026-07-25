//! Loupe engine integration tests against the real A1 files.

use std::path::PathBuf;
use std::time::Duration;

use fastcull_core::loupe::{LoupeEngine, LoupeEvent, DEFAULT_BUDGET_BYTES};

fn testdata(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/raws")
        .join(name);
    assert!(path.is_file(), "missing {path:?} — run testdata/fetch.sh");
    path
}

fn a1_paths() -> Vec<PathBuf> {
    [
        "A1_full_compressed.ARW",
        "A1_full_lossless_compressed.ARW",
        "A1_full_uncompressed.ARW",
    ]
    .into_iter()
    .map(testdata)
    .collect()
}

#[test]
fn focus_decodes_fullres_and_prefetches_neighbors() {
    let (engine, rx) = LoupeEngine::start(a1_paths(), DEFAULT_BUDGET_BYTES);
    assert!(engine.focus(1).is_none(), "cold cache");
    // Focus 1 must arrive; neighbors 0 and 2 follow via prefetch.
    let mut ready = std::collections::HashSet::new();
    while ready.len() < 3 {
        match rx.recv_timeout(Duration::from_secs(60)).expect("event") {
            LoupeEvent::Ready { index, image } => {
                assert_eq!((image.width, image.height), (8640, 5760), "idx {index}");
                assert_eq!(image.rgb.len(), 8640 * 5760 * 3);
                ready.insert(index);
            }
            LoupeEvent::Failed { index, reason } => panic!("{index} failed: {reason}"),
        }
    }
    assert_eq!(ready, [0, 1, 2].into());
    // Warm focus returns instantly.
    assert!(engine.focus(1).is_some());
    assert!(engine.peek(0).is_some());
}

#[test]
fn corrupt_file_reports_failed_and_engine_survives() {
    let dir = std::env::temp_dir().join(format!("fastcull-loupe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.ARW");
    std::fs::write(&bad, b"junk").unwrap();
    let paths = vec![bad, testdata("A1_full_compressed.ARW")];
    let (engine, rx) = LoupeEngine::start(paths, DEFAULT_BUDGET_BYTES);
    engine.focus(0);
    let mut got_fail = false;
    let mut got_ok = false;
    while !(got_fail && got_ok) {
        match rx.recv_timeout(Duration::from_secs(60)).expect("event") {
            LoupeEvent::Failed { index: 0, .. } => got_fail = true,
            LoupeEvent::Ready { index: 1, .. } => got_ok = true,
            other => panic!("unexpected {other:?}"),
        }
    }
    // Negative cache: re-focusing the failed index must not re-decode or
    // re-emit (validator finding — a corrupt file was retried forever).
    engine.focus(0);
    assert!(
        rx.recv_timeout(Duration::from_millis(800)).is_err(),
        "failed index was re-attempted"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn tight_budget_evicts_but_serves_focus() {
    // Budget below two A1 images: the engine must still serve each focus.
    let (engine, rx) = LoupeEngine::start(a1_paths(), 200 * 1024 * 1024);
    for target in [0usize, 1, 2, 0] {
        engine.focus(target);
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if engine.peek(target).is_some() {
                break;
            }
            match rx.recv_timeout(deadline - std::time::Instant::now()) {
                Ok(_) => continue,
                Err(e) => panic!("waiting for {target}: {e}"),
            }
        }
    }
}
