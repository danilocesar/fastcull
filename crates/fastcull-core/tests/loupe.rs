//! Loupe engine integration tests against the real A1 files.

use std::path::PathBuf;
use std::time::Duration;

/// Engine tests decode 50 MP JPEGs; run them serially — four parallel
/// engines on a debug-mode CI runner starved each other past the event
/// timeouts (Windows flake). What was measured is that flake: the
/// timeouts, on that seat, with the engines running in parallel. The
/// "2-vCPU" this line used to give as the cause was never measured and
/// is wrong — both CI seats are 4 vCPU with ~16 GB (CI audit
/// 2026-09-04) — and four debug-mode 50 MP decoders oversubscribe four
/// cores nearly as thoroughly as two, so the observed starvation, not
/// an arithmetic ratio, is what this mutex answers.
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

/// The ring reaches what ARROWS reach (issue #46), through the public
/// api: with a view whose positions interleave image ids (the
/// capture-sorted multi-body shape), a settled focus must prefetch the
/// VIEW neighbors — and must NOT spend workers on the id neighbors,
/// which are frames no arrow can reach from here.
#[test]
fn prefetch_follows_the_view_order_through_the_public_api() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (engine, rx) = LoupeEngine::start(a1_cycled(12), DEFAULT_BUDGET_BYTES);
    // Capture-sort interleave over three file classes: view pos -> id.
    let view = [0usize, 3, 6, 9, 1, 4, 7, 2, 5, 8, 11, 10];
    engine.set_view(&view);
    // Settled focus on id 9 = view position 3: the ±PREFETCH ring is
    // positions 1..=5 = ids {3, 6, 1, 4}. Target 2000 px: the mid rung
    // serves it, so the whole ring is debug-build cheap.
    engine.focus(9, 2000);
    let want = [9usize, 3, 6, 1, 4];
    let best = collect(&rx, 120, |b| want.iter().all(|i| b.contains_key(i)));
    for i in want {
        assert!(
            best.contains_key(&i),
            "view-ring member {i} was never decoded: saw {:?}",
            {
                let mut k: Vec<_> = best.keys().copied().collect();
                k.sort_unstable();
                k
            }
        );
    }
    // Drain a further grace period, then assert the id-space neighbors
    // of 9 (ids 7, 8, 10, 11 — all outside the view ring) never decoded:
    // that is precisely the work the old ring wasted while the real
    // neighbors stayed cold.
    let late = collect(&rx, 3, |_| false);
    for stranger in [7usize, 8, 10, 11] {
        assert!(
            !best.contains_key(&stranger) && !late.contains_key(&stranger),
            "id-space neighbor {stranger} was prefetched — the ring is \
             still walking id order"
        );
    }
    drop(engine);
}

/// `decode_oriented` must actually APPLY the orientation it is given —
/// the wiring, not the kernel. QE proved this seam unpinned (2026-08-02):
/// deleting the `apply_orientation_with` call from `decode_oriented` left
/// the ENTIRE fastcull-core suite green, because every fixture is
/// orientation 1 and the rotation kernel's own tests exercise the kernel
/// directly rather than the shipped decode path that calls it.
#[test]
fn decode_oriented_actually_rotates() {
    let path = &a1_paths()[0];
    let mut f = std::fs::File::open(path).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut f).unwrap();
    // The mid preview keeps this test at ~5 ms of decode, not ~250.
    let mid = previews
        .grid_source()
        .expect("A1 exposes a mid preview")
        .clone();
    let bytes = fastcull_core::raw::read_jpeg(&mut f, &mid).unwrap();

    // Reference: plain decode, then the (independently pinned) kernel.
    let (plain, w, h) = fastcull_core::loupe::decode_oriented(&bytes, 1).unwrap();
    assert!(
        w > h,
        "fixture must be landscape for the swap to mean anything"
    );
    let reference = fastcull_core::raw::apply_orientation(plain.clone(), w, h, 6);

    // The shipped path with orientation 6 (rotate 90 CW): dims must swap —
    // this alone kills the skip-the-rotate mutant — and the bytes must be
    // the kernel's, not the unrotated originals with swapped metadata.
    let (rot, rw, rh) = fastcull_core::loupe::decode_oriented(&bytes, 6).unwrap();
    assert_eq!((rw, rh), (h, w), "orientation 6 must swap the dimensions");
    assert_eq!(rot.len(), reference.0.len());
    assert!(
        rot == reference.0,
        "decode_oriented(o=6) differs from decode + apply_orientation"
    );

    // And orientation 1 is a true no-op relative to the raw decode.
    let (again, aw, ah) = fastcull_core::loupe::decode_oriented(&bytes, 1).unwrap();
    assert_eq!((aw, ah), (w, h));
    assert!(again == plain, "orientation 1 must not alter pixels");
}
