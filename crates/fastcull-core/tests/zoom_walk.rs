//! THE ZOOM-WALK QUALITY TEST (user-mandated 2026-07-25).
//!
//! the user's reproduction: open a folder, press `+` to 2 columns (4 images
//! on screen), arrow forward repeatedly — around the 8th image the shown
//! picture degraded to the upscaled 320 px thumb and never recovered.
//!
//! Contract enforced here: walking forward through a folder at a zoom whose
//! cells outgrow the thumb, EVERY visited image must settle on an asset
//! that satisfies the 25% ladder rule. This test MUST pass before any
//! zoom-quality problem is declared fixed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use fastcull_core::loupe::{LoupeEngine, LoupeEvent, DEFAULT_BUDGET_BYTES, UPSCALE_THRESHOLD};
use fastcull_core::viewassets::ViewAssets;

fn testdata(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/raws")
        .join(name);
    assert!(path.is_file(), "missing {path:?} — run testdata/fetch.sh");
    path
}

/// A 20-image folder built from symlinks to the three real A1 files.
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
    let paths: Vec<PathBuf> = (0..20)
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
        let visible = cursor.saturating_sub(1)..(cursor + 3).min(count);
        va.prune(&visible); // UI drops off-screen textures — the trigger of the bug
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            for (index, image) in va.ensure(visible.clone(), cell_phys, &engine) {
                // (the app would build a texture here)
                let long = image.width.max(image.height);
                assert!(long >= 320, "index {index}");
                va.note_held(index, long);
            }
            while let Ok(event) = rx.recv_timeout(Duration::from_millis(20)) {
                if let LoupeEvent::Ready { index, image } = event {
                    va.note_held(index, image.width.max(image.height));
                }
            }
            let all_good = visible.clone().all(|i| va.satisfied(i, cell_phys));
            if all_good {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cursor at {cursor}: images {:?} stuck below the {cell_phys}px rung \
                 (the user-reported walk regression) — held state: {:?}",
                visible
                    .clone()
                    .filter(|i| !va.satisfied(*i, cell_phys))
                    .collect::<Vec<_>>(),
                visible
                    .clone()
                    .map(|i| (i, va.satisfied(i, cell_phys)))
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
