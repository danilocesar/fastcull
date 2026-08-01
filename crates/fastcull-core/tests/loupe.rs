//! Loupe engine integration tests against the real A1 files.

use std::path::PathBuf;
use std::time::Duration;

/// Engine tests decode 50 MP JPEGs; run them serially — four parallel
/// engines on a 2-vCPU debug-mode CI runner starved each other past the
/// event timeouts (Windows flake).
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (engine, rx) = LoupeEngine::start(a1_paths(), DEFAULT_BUDGET_BYTES);
    // display 8640 forces the top rung of the ladder.
    assert!(engine.focus(1, 8640).is_none(), "cold cache");
    // Every index publishes rungs ending at full-res (mid rung may precede).
    let mut best = std::collections::HashMap::new();
    while best.len() < 3 || best.values().any(|&(w, _)| w != 8640) {
        match rx.recv_timeout(Duration::from_secs(120)).expect("event") {
            LoupeEvent::Ready { index, image, .. } => {
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
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        match rx.recv_timeout(Duration::from_secs(120)).expect("event") {
            LoupeEvent::Failed { index: 0, .. } => got_fail = true,
            LoupeEvent::Ready {
                index: 1, image, ..
            } => got_top_rung = image.width == 8640,
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
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Budget below two A1 images: the engine must still serve each focus.
    let (engine, rx) = LoupeEngine::start(a1_paths(), 200 * 1024 * 1024);
    for target in [0usize, 1, 2, 0] {
        engine.focus(target, 8640);
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
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
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (engine, rx) = LoupeEngine::start(a1_paths(), DEFAULT_BUDGET_BYTES);
    engine.focus(1, 1600);
    let mut got = 0;
    while got < 3 {
        match rx.recv_timeout(Duration::from_secs(120)).expect("event") {
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
        if let LoupeEvent::Ready {
            index: 1, image, ..
        } = rx.recv_timeout(Duration::from_secs(120)).expect("event")
        {
            if image.width == 8640 {
                break;
            }
        }
    }
}

/// Issue #8 / QE gap: the `terminal` flag on Ready events — a bare
/// JPEG's single rung is terminal (the app adopts it as the top rung
/// for the zoom ceiling); an ARW's mid rung is NOT (the full rung
/// follows), and its full rung IS.
#[test]
fn terminal_flag_marks_a_files_best_rung() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Bare JPEG: extract the mid preview of a real A1 file.
    let arw = testdata("A1_full_compressed.ARW");
    let mut f = std::fs::File::open(&arw).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut f).unwrap();
    let grid = previews.grid_source().expect("mid preview");
    let bytes = fastcull_core::raw::read_jpeg(&mut f, grid).unwrap();
    let dir = std::env::temp_dir().join(format!("fastcull-terminal-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let jpg = dir.join("solo.jpg");
    std::fs::write(&jpg, &bytes).unwrap();

    let (engine, rx) = LoupeEngine::start(vec![jpg], DEFAULT_BUDGET_BYTES);
    engine.focus(0, 8640);
    match rx.recv_timeout(Duration::from_secs(120)).expect("event") {
        LoupeEvent::Ready { terminal, .. } => {
            assert!(terminal, "a bare JPEG's only rung is its best");
        }
        other => panic!("unexpected {other:?}"),
    }
    drop(engine);

    // ARW: the mid rung is not terminal, the 8640 top rung is.
    let (engine, rx) = LoupeEngine::start(vec![arw], DEFAULT_BUDGET_BYTES);
    engine.focus(0, 8640);
    let mut seen_mid = false;
    let mut seen_top = false;
    while !(seen_mid && seen_top) {
        match rx.recv_timeout(Duration::from_secs(120)).expect("event") {
            LoupeEvent::Ready {
                image, terminal, ..
            } => {
                if image.width == 1616 {
                    assert!(!terminal, "mid rung must not read as the best");
                    seen_mid = true;
                } else if image.width == 8640 {
                    assert!(terminal, "the top rung IS the best");
                    seen_top = true;
                }
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    drop(engine);
    std::fs::remove_dir_all(&dir).ok();
}

/// A 24-slot folder made of the three real A1 files, so ring arithmetic
/// has room to be wrong in (`TRANSIT_AHEAD` is 8; `PREFETCH` is 2).
fn a1_cycled(n: usize) -> Vec<PathBuf> {
    let base = a1_paths();
    (0..n).map(|i| base[i % base.len()].clone()).collect()
}

/// Drain events for up to `secs`, recording the best rung seen per index.
fn collect(
    rx: &std::sync::mpsc::Receiver<LoupeEvent>,
    secs: u64,
    stop: impl Fn(&std::collections::HashMap<usize, u32>) -> bool,
) -> std::collections::HashMap<usize, u32> {
    let mut best = std::collections::HashMap::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(LoupeEvent::Ready { index, image, .. }) => {
                let long = image.width.max(image.height);
                let e = best.entry(index).or_insert(0);
                *e = (*e).max(long);
                if stop(&best) {
                    break;
                }
            }
            Ok(LoupeEvent::Failed { index, reason }) => panic!("{index} failed: {reason}"),
            Err(_) => break,
        }
    }
    best
}

/// TRANSIT through the PUBLIC api (user requirement 2026-08-01).
///
/// Every other transit test calls the pure helpers directly, so the wiring
/// inside `focus` itself was unpinned: both `let transit = false` and
/// re-deriving the travel direction from the previous focus survived the
/// entire suite (validator + QE, 2026-08-01). This drives the engine the
/// way the app does and fails if transit is not actually reaching it.
///
/// Uses the ring WIDTH as the observable, not the decode: a frame 4+ away
/// is outside `PREFETCH` entirely, so its mere appearance proves the wide
/// transit ring — and mid rungs are cheap enough to assert on in a debug
/// build, which full-res decodes are not.
#[test]
fn a_held_key_reaches_transit_through_the_public_api() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (engine, rx) = LoupeEngine::start(a1_cycled(24), DEFAULT_BUDGET_BYTES);
    // Two focuses in immediate succession: a held key, by definition.
    engine.focus(0, u32::MAX);
    engine.focus(1, u32::MAX);
    let best = collect(&rx, 120, |b| b.keys().any(|&i| i >= 5));
    let far: Vec<_> = best.keys().copied().filter(|&i| i >= 5).collect();
    assert!(
        !far.is_empty(),
        "nothing beyond PREFETCH was even requested, so the wide transit \
         ring never engaged: saw {:?}",
        {
            let mut k: Vec<_> = best.keys().copied().collect();
            k.sort_unstable();
            k
        }
    );
    // And what transit asks for is the MID, never the top rung.
    for i in &far {
        assert!(
            best[i] <= 2020,
            "idx {i} is a look-ahead frame the user has not reached, yet it \
             was decoded at {} px — transit must cap look-ahead at the mid",
            best[i]
        );
    }
}

/// The ring must not re-lean forward when the app re-focuses the SAME index.
///
/// `refresh()` calls `focus(cursor, ..)` on every decode landing, and
/// transit produces one landing per ring member per frame. Deriving the
/// direction from `index >= prev` makes every one of those re-focuses look
/// forward, so a backward hold prefetched the frames the user was moving
/// away from — measured as an effectively 21-wide ring doing half its work
/// behind the user (validator, 2026-08-01). Direction is latched at the
/// real index change instead.
#[test]
fn a_backward_hold_keeps_leaning_backward_across_refocus() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (engine, rx) = LoupeEngine::start(a1_cycled(24), DEFAULT_BUDGET_BYTES);
    // Travel backward: 12 -> 11, then the app's own re-focus storm on 11.
    engine.focus(12, u32::MAX);
    engine.focus(11, u32::MAX);
    for _ in 0..8 {
        engine.focus(11, u32::MAX);
    }
    let best = collect(&rx, 120, |b| b.keys().any(|&i| i <= 6));
    let mut seen: Vec<_> = best.keys().copied().collect();
    seen.sort_unstable();
    assert!(
        seen.iter().any(|&i| i <= 6),
        "a backward hold never reached behind the cursor: saw {seen:?}"
    );
    // 11 + TRANSIT_BEHIND is 13; anything at 14+ can only come from a ring
    // that flipped forward, which is the bug.
    assert!(
        !seen.iter().any(|&i| i >= 14),
        "the ring leaned FORWARD during a backward hold — the app's \
         same-index re-focus flipped it: saw {seen:?}"
    );
}
