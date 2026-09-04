//! THE ZOOM-WALK QUALITY TEST (user-mandated 2026-07-25).
//!
//! The user's reproduction: open a folder, press `+` to 2 columns (4 images
//! on screen), arrow forward repeatedly — around the 8th image the shown
//! picture degraded to the upscaled 320 px thumb and never recovered.
//!
//! Contract enforced here: walking forward through a folder at a zoom whose
//! cells outgrow the thumb, EVERY visited image must settle on an asset
//! that satisfies the 25% ladder rule. This test MUST pass before any
//! zoom-quality problem is declared fixed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

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

use fastcull_core::loupe::{LoupeEngine, LoupeEvent, DEFAULT_BUDGET_BYTES, UPSCALE_THRESHOLD};
use fastcull_core::viewassets::ViewAssets;

fn testdata(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/raws")
        .join(name);
    assert!(path.is_file(), "missing {path:?} — run testdata/fetch.sh");
    path
}

/// A folder built from symlinks to the three real A1 files (copies on
/// Windows — kept smaller there to bound CI disk/time).
fn walk_count() -> usize {
    if cfg!(windows) {
        8
    } else {
        20
    }
}

fn folder_of_20() -> (PathBuf, Vec<PathBuf>) {
    let dir = std::env::temp_dir().join(format!(
        "fastcull-zoomwalk-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let sources = [
        testdata("A1_full_compressed.ARW"),
        testdata("A1_full_lossless_compressed.ARW"),
        testdata("A1_full_uncompressed.ARW"),
    ];
    let paths: Vec<PathBuf> = (0..walk_count())
        .map(|i| {
            let p = dir.join(format!("DSC{i:05}.ARW"));
            #[cfg(unix)]
            std::os::unix::fs::symlink(&sources[i % 3], &p).unwrap();
            #[cfg(not(unix))]
            std::fs::copy(&sources[i % 3], &p).unwrap();
            p
        })
        .collect();
    (dir, paths)
}

#[test]
fn walking_at_two_columns_never_leaves_an_image_below_its_rung() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, paths) = folder_of_20();
    let count = paths.len();
    let (engine, rx) = LoupeEngine::start(paths, DEFAULT_BUDGET_BYTES);
    let mut va = ViewAssets::default();

    // 2 columns on a ~1900 px window: cells ~940 physical px — beyond the
    // 320 px thumb's 25% reach, served by the 1616 mid rung.
    let cell_phys: u32 = 940;

    // Walk the cursor forward like arrow keys; the visible window is the
    // 2x2 block around the cursor (like the app's windowed model).
    for cursor in 0..count {
        let visible: Vec<usize> = (cursor.saturating_sub(1)..(cursor + 3).min(count)).collect();
        va.prune(&visible); // UI drops off-screen textures — the trigger of the bug
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            for (index, image) in va.ensure(&visible, cell_phys, &engine) {
                // (the app would build a texture here)
                let long = image.width.max(image.height);
                assert!(long >= 320, "index {index}");
                va.note_held(index, long);
            }
            while let Ok(event) = rx.recv_timeout(Duration::from_millis(20)) {
                if let LoupeEvent::Ready { index, image, .. } = event {
                    va.note_held(index, image.width.max(image.height));
                }
            }
            let all_good = visible.iter().all(|i| va.satisfied(*i, cell_phys));
            if all_good {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cursor at {cursor}: images {:?} stuck below the {cell_phys}px rung \
                 (the user-reported walk regression) — held state: {:?}",
                visible
                    .iter()
                    .filter(|i| !va.satisfied(**i, cell_phys))
                    .collect::<Vec<_>>(),
                visible
                    .iter()
                    .map(|i| (*i, va.satisfied(*i, cell_phys)))
                    .collect::<Vec<_>>()
            );
        }
    }

    // Sanity on the rule itself: a satisfied 940px cell holds ≥ 940/1.25.
    let min_needed = (cell_phys as f32 / UPSCALE_THRESHOLD) as u32;
    assert!(
        min_needed <= 1616,
        "mid rung must be able to serve the cells"
    );
    drop(engine);
    std::fs::remove_dir_all(&dir).ok();
}

/// Fast continuous scroll (validator finding): sweeping the cursor without
/// waiting must NOT leave the final window starved behind a stale backlog
/// of scrolled-past requests — want() culls them. The decode count proves
/// culling: without it every swept cell gets cooked (~walk_count decodes).
#[test]
fn fast_scroll_backlog_does_not_starve_final_window() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dir, paths) = folder_of_20();
    let count = paths.len();
    let (engine, rx) = LoupeEngine::start(paths, DEFAULT_BUDGET_BYTES);
    let mut va = ViewAssets::default();
    let cell_phys: u32 = 940;

    // Sweep like a fast scroll: no settling between steps.
    for cursor in 0..count {
        let visible: Vec<usize> = (cursor.saturating_sub(1)..(cursor + 3).min(count)).collect();
        va.prune(&visible);
        for (index, image) in va.ensure(&visible, cell_phys, &engine) {
            va.note_held(index, image.width.max(image.height));
        }
    }

    // The final window must settle promptly and cheaply.
    let final_visible: Vec<usize> = (count.saturating_sub(3)..count).collect();
    let mut ready_events = 0usize;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for (index, image) in va.ensure(&final_visible, cell_phys, &engine) {
            va.note_held(index, image.width.max(image.height));
        }
        while let Ok(event) = rx.recv_timeout(Duration::from_millis(20)) {
            if let LoupeEvent::Ready { index, image, .. } = event {
                ready_events += 1;
                va.note_held(index, image.width.max(image.height));
            }
        }
        if final_visible.iter().all(|i| va.satisfied(*i, cell_phys)) {
            break;
        }
        assert!(Instant::now() < deadline, "final window starved by backlog");
    }
    assert!(
        ready_events <= 10,
        "stale backlog was decoded instead of culled: {ready_events} Ready events"
    );
    drop(engine);
    std::fs::remove_dir_all(&dir).ok();
}
