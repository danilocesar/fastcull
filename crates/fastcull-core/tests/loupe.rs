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

/// QE round 1 of brief 008, D1 (raw-pipeline.md, "Hostile-input bounds"): the
/// real shape of the commonest field corruption — `A1_full_compressed.ARW`
/// cut at 10,000,000 bytes, inside its full JPEG (741,376 to 13,054,886), as
/// an interrupted copy leaves it. The walker holds the 1616x1080 mid whole
/// and the 8640x5760 full cut; the full is the loupe's top rung, and its read
/// fails as truncated. Through the public engine, at 1:1 and at fit on a
/// 3840x2160 box: the mid arrives NOT terminal — never the file's best, so
/// the app cues it where it does not serve and zooms past it — and nothing
/// else follows, no Failed, not even after a second focus (the memo). Red on
/// the walker that dropped the cut full: the mid arrived terminal.
#[test]
fn an_a1_cut_inside_its_full_keeps_its_mid_below_the_top_rung() {
    use std::io::Read;
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = std::env::temp_dir().join(format!("fastcull-cut-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let cut = dir.join("cut.ARW");
    let mut head = vec![0u8; 10_000_000];
    std::fs::File::open(testdata("A1_full_compressed.ARW"))
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    std::fs::write(&cut, &head).unwrap();

    let mut file = std::fs::File::open(&cut).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut file).unwrap();
    let size = |c: Option<&fastcull_core::raw::EmbeddedJpeg>| c.map(|c| (c.width, c.height));
    assert_eq!(
        size(previews.fullres()),
        Some((1616, 1080)),
        "the premise: only the mid is whole"
    );
    let top = previews.loupe_top().expect("the cut full");
    assert_eq!((top.width, top.height), (8640, 5760), "the full is the top");
    let err = fastcull_core::raw::read_jpeg(&mut file, top)
        .expect_err("the file ends inside the full")
        .to_string();
    assert!(err.starts_with("truncated"), "{err}");

    for phase in ["1:1", "fit on 3840x2160"] {
        let (engine, rx) = LoupeEngine::start(vec![cut.clone()], DEFAULT_BUDGET_BYTES);
        let focus = || match phase {
            "1:1" => {
                engine.focus(0, u32::MAX);
            }
            _ => {
                engine.focus_fit(0);
            }
        };
        if phase != "1:1" {
            engine.set_fit_box(Some(fastcull_core::loupe::FitBox {
                width: 3840,
                height: 2160,
            }));
        }
        focus();
        match rx.recv_timeout(Duration::from_secs(120)).expect("event") {
            LoupeEvent::Ready {
                image, terminal, ..
            } => {
                assert_eq!((image.width, image.height), (1616, 1080), "{phase}");
                assert!(
                    !terminal,
                    "{phase}: the mid is never the best of a cut file"
                );
            }
            other => panic!("{phase}: unexpected {other:?}"),
        }
        assert!(
            rx.recv_timeout(Duration::from_millis(800)).is_err(),
            "{phase}: the cut full fails nothing and publishes nothing"
        );
        focus();
        assert!(
            rx.recv_timeout(Duration::from_millis(800)).is_err(),
            "{phase}: refocused, nothing is climbed again"
        );
        drop(engine);
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// A 24-slot folder made of the three real A1 files, so ring arithmetic
/// has room to be wrong in (`RING_AHEAD` is 15, `RING_BEHIND` 2; the settled
/// ring of an engine with no fit box, `PREFETCH`, is 2).
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
    // 11 + RING_BEHIND is 13 (the ring behind is 2, as TRANSIT_BEHIND was
    // before brief 008, so this bound is exactly what it was); anything at
    // 14+ can only come from a ring that flipped forward, which is the bug.
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

/// Brief 008 A12 (raw-pipeline.md, "The RSS ceiling"): release, Linux with
/// glibc only — symlinks, `VmHWM` from `/proc/self/status` and glibc's
/// tunables are all three Linux-with-glibc facts, and the helpers live in
/// this module with their only users, so nothing here is dead code on the
/// Windows build.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod rss_ceiling {
    use super::*;
    use std::collections::HashSet;
    use std::io::{BufRead, BufReader, Read};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Instant;

    use fastcull_core::budget::{LoupeSizes, MMAP_THRESHOLD};
    use fastcull_core::loupe::{FitBox, RungKind, REF_FRAME_BYTES};

    /// The variable that turns [`walk_child`] on: the walk runs only in the
    /// child the parent starts, never in an ordinary run of this binary.
    const WALK_VAR: &str = "FASTCULL_A12_WALK";
    /// The parent's liveness bound: the child's two phase deadlines and its
    /// setup. A child past it is killed and the test fails — never a longer
    /// bound.
    const LIVENESS: Duration = Duration::from_secs(900);
    /// Each phase's liveness bound: a slow seat fails instead of hanging.
    /// Not a performance gate — a correct build that reaches it is a
    /// finding to bring back, never a reason for a longer one.
    const PHASE_DEADLINE: Duration = Duration::from_secs(300);
    /// The walk's folder: 5,000 links cycling the three A1 files.
    const FILES: usize = 5_000;
    /// A held key's repeat, as the app's driven holds use.
    const KEY: Duration = Duration::from_millis(40);
    /// During a stop the walk re-focuses the same frame this often, as the
    /// app's refreshes do: a settled ring is asked by a focus call.
    const REFRESH: Duration = Duration::from_millis(100);
    /// A stop between the 1:1 holds.
    const STOP: Duration = Duration::from_secs(2);
    /// The keys of one hold at 1:1.
    const HOLD_KEYS: usize = 30;
    /// The ceiling's allowance over the cache and the decoders' buffers
    /// (the spec's box; the decoders' input JPEGs ride in it too).
    const ALLOWANCE: u64 = 200_000_000;

    /// The build's target directory — where the I/O-touching budgets put their
    /// fixtures, so 1,000 files never land on the development machine's tmpfs
    /// `/tmp` (whose quota this repo has exhausted before). `CARGO_TARGET_DIR`
    /// wins when the caller set one: the gate runs a validator and a QE agent
    /// in their own target dirs, and a fixture written outside them is invisible
    /// to their cleanup. A relative override resolves against the WORKSPACE
    /// root, not the test's cwd — cargo runs a test binary from its package
    /// directory, so a bare `target-qe-1` would otherwise land one level deep.
    fn target_dir() -> PathBuf {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        // `join` with an absolute path replaces the base, so both forms work.
        workspace.join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()))
    }

    /// A fixture directory that is deleted even when an assertion panics.
    ///
    /// Without it a red budget leaks its 1,000 files under the target
    /// directory, which nothing else cleans up (`cargo clean` aside).
    struct Fixture {
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    /// `/proc/meminfo`'s `MemAvailable`, in bytes.
    fn mem_available() -> Option<u64> {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
        let kib: u64 = line["MemAvailable:".len()..]
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .ok()?;
        kib.checked_mul(1024)
    }

    /// This process's peak resident set, `/proc/self/status`'s `VmHWM`, in
    /// bytes: a monotone high-water mark, so a periodic read is the running
    /// peak.
    fn vm_hwm() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
        let line = status
            .lines()
            .find(|l| l.starts_with("VmHWM:"))
            .expect("VmHWM in /proc/self/status");
        let kib: u64 = line["VmHWM:".len()..]
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .expect("VmHWM is a number of kB");
        kib * 1024
    }

    /// THE PARENT (raw-pipeline.md, "The RSS ceiling"; Manager ruling
    /// 2026-09-27): the walk runs in a child of this test binary under the
    /// app's allocator threshold, `GLIBC_TUNABLES` built from
    /// `budget::MMAP_THRESHOLD` — core cannot run the app's `main`, and the
    /// tunable sets what the app's `mallopt` sets (the app manifest's glibc
    /// canary). The parent relays the child's output as it comes, bounds it
    /// by a liveness deadline, and fails unless the child exited cleanly AND
    /// printed its reading: a child that ran no walk — a filter that matches
    /// nothing exits 0 with "0 passed" — is red, never a vacuous green.
    /// Skipped, with the reason printed, in a debug build and when the
    /// machine has less memory available than the cache + 2 GiB.
    #[test]
    fn the_engine_walk_holds_the_rss_ceiling() {
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        if cfg!(debug_assertions) {
            eprintln!("A12 skipped: a release-profile measurement (run with --release)");
            return;
        }
        let cache = LoupeSizes::from_machine().cache_bytes;
        let available = mem_available().unwrap_or(0);
        let needed = cache + (2 << 30);
        if available < needed {
            eprintln!(
                "A12 skipped: {} MiB available, the walk needs the cache + 2 GiB, {} MiB",
                available >> 20,
                needed >> 20
            );
            return;
        }
        let started = Instant::now();
        let mut child =
            std::process::Command::new(std::env::current_exe().expect("the test binary"))
                .args([
                    "--exact",
                    "rss_ceiling::walk_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(WALK_VAR, "1")
                .env(
                    "GLIBC_TUNABLES",
                    format!("glibc.malloc.mmap_threshold={MMAP_THRESHOLD}"),
                )
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("the walk child starts");
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        // One drain per pipe, each echoing every line to this process's
        // stderr as it arrives: a full pipe would block the child.
        let relay = |pipe: Box<dyn Read + Send>| {
            let lines = Arc::clone(&lines);
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    eprintln!("{line}");
                    lines
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(line);
                }
            })
        };
        let stdout = relay(Box::new(child.stdout.take().expect("stdout piped")));
        let stderr = relay(Box::new(child.stderr.take().expect("stderr piped")));
        let status = loop {
            if let Some(status) = child.try_wait().expect("waiting on the walk child") {
                break status;
            }
            if started.elapsed() > LIVENESS {
                child.kill().ok();
                child.wait().ok();
                let lines = lines.lock().unwrap_or_else(PoisonError::into_inner);
                let tail = lines[lines.len().saturating_sub(20)..].join("\n");
                panic!(
                    "the walk child was still running after {:.0} s (the liveness bound is \
                     {LIVENESS:?}) and was killed; its last lines:\n{tail}",
                    started.elapsed().as_secs_f64()
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        stdout.join().ok();
        stderr.join().ok();
        let lines = lines.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if !status.success() {
            let at = lines.iter().position(|l| l.contains("panicked at"));
            let why = match at {
                Some(i) => lines[i..(i + 6).min(lines.len())].join("\n"),
                None => lines[lines.len().saturating_sub(20)..].join("\n"),
            };
            panic!("the walk child failed ({status}):\n{why}");
        }
        assert!(
            lines.iter().any(|l| l.contains("MEASURED A12 VmHWM")),
            "the walk child printed no reading, so it ran no walk:\n{}",
            lines.join("\n")
        );
    }

    /// What the drain has seen: the distinct indexes per kind, and the bytes
    /// of the first image of each kind (what "the cache's worth" counts in).
    #[derive(Default)]
    struct Seen {
        screen: HashSet<usize>,
        full: HashSet<usize>,
        screen_bytes: Option<u64>,
        full_bytes: Option<u64>,
        failed: Vec<(usize, String)>,
    }

    /// The walk: the engine, what its drain has seen, the ceiling and the
    /// running peak.
    struct Walk {
        engine: LoupeEngine,
        seen: Arc<Mutex<Seen>>,
        cache: u64,
        ceiling: u64,
        peak: u64,
        focuses: u64,
        next: usize,
    }

    impl Walk {
        fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
            self.seen.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// Read the peak; the first read past the ceiling ends the walk.
        fn read(&mut self, phase: &str) {
            let hwm = vm_hwm();
            self.peak = self.peak.max(hwm);
            assert!(
                hwm <= self.ceiling,
                "A12, the {phase} phase: VmHWM {} MiB, past the ceiling of {} MiB (the cache \
                 + the decoders × 2 × 149,299,200 B + 200 MB)",
                hwm >> 20,
                self.ceiling >> 20
            );
            let failed = &self.seen().failed;
            assert!(failed.is_empty(), "A12: a decode failed: {failed:?}");
        }

        /// One focus call, at fit or at 1:1, and a peak read at every 10th.
        fn focus(&mut self, index: usize, at_fit: bool, phase: &str) {
            if at_fit {
                self.engine.focus_fit(index);
            } else {
                self.engine.focus(index, u32::MAX);
            }
            self.focuses += 1;
            if self.focuses.is_multiple_of(10) {
                self.read(phase);
            }
        }

        /// A hold: the next frames, one per key period.
        fn hold(&mut self, keys: usize, at_fit: bool, phase: &str) {
            for _ in 0..keys {
                assert!(
                    self.next < FILES,
                    "A12, the {phase} phase: the folder ran out"
                );
                self.focus(self.next, at_fit, phase);
                self.next += 1;
                std::thread::sleep(KEY);
            }
        }

        /// A stop on the last frame the hold reached, re-focused as the app's
        /// refreshes re-focus it.
        fn stop(&mut self, how_long: Duration, at_fit: bool, phase: &str) {
            let on = self.next.saturating_sub(1);
            let started = Instant::now();
            while started.elapsed() < how_long {
                self.focus(on, at_fit, phase);
                std::thread::sleep(REFRESH);
            }
        }

        /// 1.5 × the cache's worth of frames of one kind, the worth counted
        /// in the bytes of the first such image received — never an assumed
        /// size — or `None` before one arrives.
        fn target(&self, bytes: Option<u64>) -> Option<usize> {
            bytes.map(|b| usize::try_from(self.cache * 3 / (2 * b)).unwrap_or(usize::MAX))
        }

        /// Fail a phase that has not reached its count by its deadline.
        fn within_deadline(
            &self,
            phase: &str,
            started: Instant,
            reached: usize,
            target: Option<usize>,
        ) {
            assert!(
                started.elapsed() < PHASE_DEADLINE,
                "A12, the {phase} phase: {reached} distinct frames of the {target:?} it needs \
                 after {:.0} s (its liveness deadline is {PHASE_DEADLINE:?})",
                started.elapsed().as_secs_f64()
            );
        }
    }

    /// THE CHILD: the walk itself, in its own process under the tunable the
    /// parent set. At fit on a 2560×1440 box — the 2/8 rung, 2160×1440, the
    /// shape where a 16 MiB threshold keeps its 9 MB buffers — a hold until
    /// the drain has counted 1.5 × the cache's worth of distinct screen
    /// rungs, then a stop; at 1:1, 30-key holds and 2 s stops in turn until
    /// 1.5 × the cache's worth of distinct full-res frames, then a stop —
    /// during a 1:1 hold the backlog serves the focused frame's fit-box rung
    /// first, so the full-res frames come mostly from the stops' settled
    /// rings. `VmHWM` stays ≤ the cache + the decoders × 2 × 149,299,200 B +
    /// 200 MB, read at every 10th focus and at each phase's end.
    #[test]
    fn walk_child() {
        if std::env::var_os(WALK_VAR).is_none() {
            return; // not the child: nothing to do
        }
        let _serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let sizes = LoupeSizes::from_machine();
        let decoders = u64::try_from(sizes.decoders).expect("a decoder count");
        let ceiling = sizes.cache_bytes + decoders * 2 * REF_FRAME_BYTES + ALLOWANCE;
        eprintln!("{sizes}");

        let dir = target_dir().join(format!("a12-walk-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("the walk's folder");
        let _fixture = Fixture { dir: dir.clone() };
        let sources = a1_paths();
        let paths: Vec<PathBuf> = (0..FILES)
            .map(|i| {
                let link = dir.join(format!("DSC{i:05}.ARW"));
                std::os::unix::fs::symlink(&sources[i % sources.len()], &link)
                    .expect("a symlink to an A1 file");
                link
            })
            .collect();

        let (engine, rx) = LoupeEngine::start_with(
            paths,
            usize::try_from(sizes.cache_bytes).expect("the cache fits a usize"),
            sizes.decoders,
        );
        engine.set_fit_box(Some(FitBox {
            width: 2560,
            height: 1440,
        }));
        let seen = Arc::new(Mutex::new(Seen::default()));
        // Receives every event and DROPS its image at once: the engine's
        // cache is all that holds pixels, as in the app minus its textures.
        let drain = {
            let seen = Arc::clone(&seen);
            std::thread::spawn(move || {
                for event in rx {
                    let mut seen = seen.lock().unwrap_or_else(PoisonError::into_inner);
                    match event {
                        LoupeEvent::Ready { index, image, .. } => {
                            let bytes = image.rgb.len() as u64;
                            match image.kind {
                                RungKind::Screen => {
                                    seen.screen.insert(index);
                                    seen.screen_bytes.get_or_insert(bytes);
                                }
                                RungKind::Full => {
                                    seen.full.insert(index);
                                    seen.full_bytes.get_or_insert(bytes);
                                }
                                RungKind::Mid => {}
                            }
                        }
                        LoupeEvent::Failed { index, reason } => seen.failed.push((index, reason)),
                    }
                }
            })
        };
        let mut walk = Walk {
            engine,
            seen,
            cache: sizes.cache_bytes,
            ceiling,
            peak: 0,
            focuses: 0,
            next: 0,
        };

        // AT FIT: one hold until 1.5 × the cache's worth of screen rungs.
        let started = Instant::now();
        loop {
            let (reached, target) = {
                let seen = walk.seen();
                (seen.screen.len(), walk.target(seen.screen_bytes))
            };
            if target.is_some_and(|t| reached >= t) {
                break;
            }
            walk.within_deadline("fit", started, reached, target);
            walk.hold(1, true, "fit");
        }
        walk.stop(STOP, true, "fit");
        walk.read("fit");
        eprintln!(
            "MEASURED A12 phase fit {:.1} s {} frames",
            started.elapsed().as_secs_f64(),
            walk.seen().screen.len()
        );

        // AT 1:1: holds and stops in turn until 1.5 × the cache's worth of
        // full-res frames.
        let started = Instant::now();
        loop {
            walk.hold(HOLD_KEYS, false, "1:1");
            walk.stop(STOP, false, "1:1");
            let (reached, target) = {
                let seen = walk.seen();
                (seen.full.len(), walk.target(seen.full_bytes))
            };
            if target.is_some_and(|t| reached >= t) {
                break;
            }
            walk.within_deadline("1:1", started, reached, target);
        }
        walk.read("1:1");
        eprintln!(
            "MEASURED A12 phase 1:1 {:.1} s {} frames",
            started.elapsed().as_secs_f64(),
            walk.seen().full.len()
        );
        eprintln!(
            "MEASURED A12 VmHWM {} MiB (ceiling {} MiB)",
            walk.peak >> 20,
            walk.ceiling >> 20
        );
        let Walk { engine, .. } = walk;
        drop(engine);
        drain.join().ok();
    }
}
