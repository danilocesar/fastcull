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
    // display 8640 forces the top rung of the ladder.
    assert!(engine.focus(1, 8640).is_none(), "cold cache");
    // Every index publishes rungs ending at full-res (mid rung may precede).
    let mut best = std::collections::HashMap::new();
    while best.len() < 3 || best.values().any(|&(w, _)| w != 8640) {
        match rx.recv_timeout(Duration::from_secs(60)).expect("event") {
            LoupeEvent::Ready { index, image } => {
                best.insert(index, (image.width, image.height));
            }
            LoupeEvent::Failed { index, reason } => panic!("{index} failed: {reason}"),
        }
    }
    for (i, dims) in &best {
        assert_eq!(*dims, (8640, 5760), "idx {i}");
    }
    // Warm focus returns instantly.
    assert!(engine.focus(1, 8640).is_some());
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
    engine.focus(0, 8640);
    let mut got_fail = false;
    let mut got_top_rung = false;
    // Drain until the failure AND the good file's final (full-res) ladder
    // rung have both arrived, so the quiet-window check below can't be
    // tripped by a still-cooking rung of index 1.
    while !(got_fail && got_top_rung) {
        match rx.recv_timeout(Duration::from_secs(60)).expect("event") {
            LoupeEvent::Failed { index: 0, .. } => got_fail = true,
            LoupeEvent::Ready { index: 1, image } => got_top_rung = image.width == 8640,
            other => panic!("unexpected {other:?}"),
        }
    }
    // Negative cache: re-focusing the failed index must not re-decode or
    // re-emit (validator finding — a corrupt file was retried forever).
    engine.focus(0, 8640);
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
        engine.focus(target, 8640);
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

/// Ladder rule: a ~1.6k display is served by the mid preview alone — the
/// expensive full-res rung must NOT be cooked (user's 25% rule).
#[test]
fn small_display_stops_at_mid_rung() {
    let (engine, rx) = LoupeEngine::start(a1_paths(), DEFAULT_BUDGET_BYTES);
    engine.focus(1, 1600);
    let mut got = 0;
    while got < 3 {
        match rx.recv_timeout(Duration::from_secs(60)).expect("event") {
            LoupeEvent::Ready { image, .. } => {
                assert_eq!((image.width, image.height), (1616, 1080));
                got += 1;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    // No further (full-res) events: the ladder stopped at the mid rung.
    assert!(rx.recv_timeout(Duration::from_millis(800)).is_err());
    // Zooming to 1:1 later cooks the top rung for the same index.
    engine.focus(1, u32::MAX);
    loop {
        if let LoupeEvent::Ready { index: 1, image } =
            rx.recv_timeout(Duration::from_secs(60)).expect("event")
        {
            if image.width == 8640 {
                break;
            }
        }
    }
}
