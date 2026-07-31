//! Screenshot smoke tests (ui-grid.md acceptance): launch the real app in
//! --screenshot mode (software renderer), decode the frame, and assert the
//! grid actually rendered content. Skips when no display server exists.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn has_display() -> bool {
    cfg!(windows)
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var_os("DISPLAY").is_some()
}

fn shoot(args: &[&str], out: &Path) {
    shoot_env(args, &[], out);
}

/// Like `shoot`, with extra env vars — and the SAME 90 s watchdog: a hung
/// child must be killed, not block the harness forever (validator M2; the
/// issue #12 test initially bypassed this via a bare `Command::status()`).
fn shoot_env(args: &[&str], envs: &[(&str, &str)], out: &Path) {
    shoot_env_stderr(args, envs, out);
}

/// Watchdogged run that also captures stderr (for FASTCULL_TRACE
/// assertions). Every test that spawns the app goes through this — a bare
/// `Command::output()`/`status()` has no deadline and hangs the harness
/// (validator M2, re-found on the issue #6 test).
fn shoot_env_stderr(args: &[&str], envs: &[(&str, &str)], out: &Path) -> String {
    let bin = env!("CARGO_BIN_EXE_fastcull-app");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args)
        .arg("--screenshot")
        .arg(out)
        .env("FASTCULL_NO_CACHE", "1") // never touch the user's real cache
        .stderr(std::process::Stdio::piped());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn app");
    // Drain stderr on a thread so a chatty child can't fill the pipe and
    // deadlock against our try_wait loop.
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let drain = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        stderr_pipe.read_to_string(&mut buf).ok();
        buf
    });
    // Strictly beyond the app's own 60 s readiness cap (measured from timer
    // start, i.e. after startup/scan): the cap must be able to fire and
    // exit(1) with its diagnostic BEFORE this harness gives up, or a slow
    // runner reports a generic timeout and leaks the child (validator M2).
    // NOTE: the shutter (and thus the 60 s cap) is deferred while a
    // FASTCULL_DRIVE script has unfired actions — a script scheduling
    // past ~90 s on a never-ready input would hit THIS deadline instead
    // of the app's own diagnostic (loud either way; keep scripts short).
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            let stderr = drain.join().unwrap_or_default();
            assert!(
                status.success(),
                "app exited with {status}; stderr:\n{stderr}"
            );
            return stderr;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            drain.join().ok();
            panic!("screenshot run timed out (no exit within 90 s)");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Decode the snapshot and return (width, height, mean_luma).
fn analyze(path: &Path) -> (usize, usize, f64) {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let sum: u64 = px.iter().map(|b| *b as u64).sum();
    (w, h, sum as f64 / px.len() as f64)
}

/// Luma variance inside the top-left region (first grid cell / loupe photo):
/// flat placeholders sit near zero, real photo texture is orders higher —
/// this is what actually distinguishes "thumbnails rendered" from "gray
/// boxes rendered" (validator/QE finding on the old luma-only assert).
fn region_variance(path: &Path, frac_w: f64, frac_h: f64) -> f64 {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let (rw, rh) = ((w as f64 * frac_w) as usize, (h as f64 * frac_h) as usize);
    let mut lumas = Vec::with_capacity(rw * rh);
    for y in 0..rh {
        for x in 0..rw {
            let i = (y * w + x) * 3;
            lumas.push(0.299 * px[i] as f64 + 0.587 * px[i + 1] as f64 + 0.114 * px[i + 2] as f64);
        }
    }
    let mean = lumas.iter().sum::<f64>() / lumas.len() as f64;
    lumas.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / lumas.len() as f64
}

/// Luma (mean, variance) of an arbitrary fractional sub-rectangle —
/// the panel-docking test needs edge strips, not just the top-left corner.
fn region_stats(path: &Path, fx0: f64, fy0: f64, fx1: f64, fy1: f64) -> (f64, f64) {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let (x0, x1) = ((w as f64 * fx0) as usize, (w as f64 * fx1) as usize);
    let (y0, y1) = ((h as f64 * fy0) as usize, (h as f64 * fy1) as usize);
    let mut lumas = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * 3;
            lumas.push(0.299 * px[i] as f64 + 0.587 * px[i + 1] as f64 + 0.114 * px[i + 2] as f64);
        }
    }
    let mean = lumas.iter().sum::<f64>() / lumas.len() as f64;
    let var = lumas.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / lumas.len() as f64;
    (mean, var)
}

/// Link (unix) or copy (windows) a fixture RAW into a test dir: the
/// six-copy tests leaked 1.2 GB of tmpfs per suite run and exhausted
/// the disk quota inside the reaper's grace window — the root cause of
/// a string of "unexplained" local one-off failures (gate finding M2).
/// The app follows symlinks (catalog spec: a link.ARW is first-class).
fn place_fixture(src: &Path, dst: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(src, dst).unwrap();
    #[cfg(not(unix))]
    std::fs::copy(src, dst).map(|_| ()).unwrap();
}

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn out_dir() -> PathBuf {
    // Best-effort reaping of STALE sibling dirs first: each run leaks a
    // pid-named dir (~6 MB of JPEGs), and on a tmpfs /tmp the accumulation
    // has exhausted the disk quota twice (QE finding). One hour of grace
    // keeps concurrent/recent runs (and their failure artifacts) intact.
    let tmp = std::env::temp_dir();
    if let Ok(entries) = std::fs::read_dir(&tmp) {
        let cutoff = std::time::SystemTime::now() - Duration::from_secs(3600);
        for e in entries.flatten() {
            let name = e.file_name();
            let stale = name.to_string_lossy().starts_with("fastcull-shots-")
                && e.metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t < cutoff);
            if stale {
                std::fs::remove_dir_all(e.path()).ok();
            }
        }
    }
    let dir = tmp.join(format!("fastcull-shots-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn raws_dir() -> PathBuf {
    let raws = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/raws");
    assert!(
        raws.join("A1_full_compressed.ARW").is_file(),
        "run testdata/fetch.sh"
    );
    raws
}

#[test]
fn grid_screenshot_shows_real_thumbnails() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("grid.jpg");
    shoot(&[raws_dir().to_str().unwrap()], &out);
    let (w, h, luma) = analyze(&out);
    assert!(w >= 640 && h >= 480, "implausible snapshot size {w}x{h}");
    assert!(
        luma > 5.0,
        "snapshot is black (mean luma {luma:.2}) — renderer regression"
    );
    assert!(
        luma < 250.0,
        "snapshot is blank white (mean luma {luma:.2})"
    );
    // The first cell must contain PHOTO texture, not a flat placeholder —
    // luma alone cannot tell them apart (validator/QE finding).
    let var = region_variance(&out, 0.12, 0.12);
    assert!(
        var > 100.0,
        "first cell has no photo texture (variance {var:.1}) — thumbnails never loaded"
    );
}

#[test]
fn loupe_fit_screenshot_shows_fullsize_photo() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("loupe-fit.jpg");
    shoot(&[raws_dir().to_str().unwrap(), "--start-loupe"], &out);
    let var = region_variance(&out, 0.5, 0.5);
    assert!(var > 100.0, "loupe fit shows no photo (variance {var:.1})");
}

#[test]
fn one_to_one_screenshot_shows_pixels() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let fit = out_dir().join("fit-for-diff.jpg");
    shoot(&[raws_dir().to_str().unwrap(), "--start-loupe"], &fit);
    let out = out_dir().join("loupe-11.jpg");
    shoot(&[raws_dir().to_str().unwrap(), "--start-11"], &out);
    let var = region_variance(&out, 0.5, 0.5);
    assert!(var > 50.0, "1:1 overlay shows no photo (variance {var:.1})");
    // 1:1 must actually differ from the fit view — a byte-identical frame
    // means the shutter fired before full-res was adopted (validator
    // finding: the old fixed delay made this test pass vacuously).
    let diff = mean_abs_diff(&fit, &out);
    assert!(
        diff > 2.0,
        "1:1 frame is (near-)identical to fit (mean abs diff {diff:.2}) — captured the wrong state"
    );
}

/// Mean absolute per-channel difference between two same-sized frames.
fn mean_abs_diff(a: &Path, b: &Path) -> f64 {
    let decode = |p: &Path| {
        let bytes = std::fs::read(p).expect("frame");
        let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
        dec.decode().expect("decode")
    };
    let (pa, pb) = (decode(a), decode(b));
    assert_eq!(pa.len(), pb.len(), "frame size mismatch");
    let sum: u64 = pa
        .iter()
        .zip(pb.iter())
        .map(|(x, y)| (*x as i16 - *y as i16).unsigned_abs() as u64)
        .sum();
    sum as f64 / pa.len() as f64
}

/// On-screen bounding box of the PHOTO in a snapshot, as (x0, y0, x1, y1).
///
/// Chrome (menu bar, filter bar, status bar) and the pillarbox bars are
/// flat; photo pixels are not. Rows are found in a narrow band of middle
/// COLUMNS (where every chrome strip is empty — its text sits on the left),
/// then columns are found within those rows.
fn photo_rect(path: &Path) -> (usize, usize, usize, usize) {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let luma = |x: usize, y: usize| {
        let i = (y * w + x) * 3;
        0.299 * px[i] as f64 + 0.587 * px[i + 1] as f64 + 0.114 * px[i + 2] as f64
    };
    let variance = |vals: &[f64]| {
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        vals.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64
    };
    const TEXTURE: f64 = 40.0; // flat chrome/bars sit far below this
                               // Tolerate a flat band inside the photo, but stay BELOW `CELL_GAP` (6)
                               // so the run cannot walk across the gap between two cells: at N=1 a
                               // mis-anchored strip shows the bottom of one frame above the top of the
                               // next, and a wider tolerance measures the pair as one tall "photo"
                               // whose aspect looks perfectly healthy (validator finding C3).
    const GAP: usize = 4;
    // Contiguous run through the centre, NOT first-to-last: the overlay
    // scrollbar's thumb is a detached high-variance strip near the right
    // edge and would otherwise be counted as part of the photo.
    let run = |is_photo: &dyn Fn(usize) -> bool, centre: usize, limit: usize| {
        // lo/hi stay on the outermost line that really IS photo.
        let (mut lo, mut i, mut miss) = (centre, centre, 0);
        while i > 0 && miss < GAP {
            i -= 1;
            if is_photo(i) {
                lo = i;
                miss = 0;
            } else {
                miss += 1;
            }
        }
        let (mut hi, mut j, mut miss) = (centre, centre, 0);
        while j + 1 < limit && miss < GAP {
            j += 1;
            if is_photo(j) {
                hi = j;
                miss = 0;
            } else {
                miss += 1;
            }
        }
        (lo, hi)
    };
    let mid: Vec<usize> = (w * 45 / 100..w * 55 / 100).step_by(3).collect();
    let row_is_photo = |y: usize| {
        let v: Vec<f64> = mid.iter().map(|&x| luma(x, y)).collect();
        variance(&v) > TEXTURE
    };
    assert!(
        row_is_photo(h / 2),
        "no textured photo row at the vertical centre of {path:?} — either \
         nothing rendered, or the fixture is too flat for this measurement"
    );
    let (y0, y1) = run(&row_is_photo, h / 2, h);
    let band: Vec<usize> = (y0..=y1).step_by(5).collect();
    let col_is_photo = |x: usize| {
        let v: Vec<f64> = band.iter().map(|&y| luma(x, y)).collect();
        variance(&v) > TEXTURE
    };
    let (x0, x1) = run(&col_is_photo, w / 2, w);
    (x0, y0, x1, y1)
}

/// The loupe fit view shows the WHOLE frame (`ui-grid.md`: `Fit` = "the
/// whole image is on screen"; the pointer contract's drag row is justified
/// by "nothing is off-screen").
///
/// This is the regression the 29 shipped screenshot tests could not see:
/// the N=1 grid cell was a 3:2 box of the full grid width — taller than the
/// viewport — so a 3:2 frame rendered edge-to-edge at ~1.80 aspect with its
/// bottom 17-23% below the fold, and every existing assertion (mean luma,
/// centre-region variance) passed exactly as happily as it does now. The
/// aspect of the rendered photo is what tells the two apart.
#[test]
fn loupe_fit_shows_the_whole_frame_not_a_crop() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("loupe-fit-whole.jpg");
    shoot(&[raws_dir().to_str().unwrap(), "--start-loupe"], &out);
    let (x0, y0, x1, y1) = photo_rect(&out);
    let (pw, ph) = ((x1 - x0 + 1) as f64, (y1 - y0 + 1) as f64);
    let aspect = pw / ph;
    // The A1 fixtures are 3:2 (the mid rung is 1616x1080 = 1.4963).
    assert!(
        (1.40..=1.60).contains(&aspect),
        "loupe fit renders the frame at aspect {aspect:.3} ({pw}x{ph}) — a 3:2 \
         frame drawn at ~1.8 means the cell overflows the viewport and the \
         bottom of every photo is cropped"
    );
    // ...and the width that a true fit gives up must show as BLACK BARS,
    // not as a wider crop: on any window wider than 1.5x the grid height
    // there is a bar on each side.
    let (w, _, _) = analyze(&out);
    assert!(
        x0 > 20 && x1 < w - 20,
        "no pillarbox bars (photo spans x {x0}..{x1} of {w}) — the frame is \
         filling the width, which it can only do by cropping"
    );
}

/// A VERTICAL resize in the loupe must leave one whole frame on screen.
///
/// Bounding the N=1 cell to the viewport made its height depend on the
/// viewport height for the first time, so a height-only resize reflows the
/// strip. Keeping the raw pixel offset then lands mid-strip: the loupe shows
/// the bottom of one photo with the top of the next below it — strictly
/// worse than the crop the bound was added to fix (validator FAIL-1,
/// 2026-07-30). The re-anchor must fire when the cell is not WHOLLY
/// visible, not only when it has left the viewport entirely.
#[test]
fn loupe_survives_a_vertical_resize_with_one_whole_frame() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("loupe-resize.jpg");
    // Land on an image the strip has to scroll to (pos 1), THEN shrink the
    // window vertically: position 0 is anchored at scroll 0 and cannot show
    // the defect.
    shoot_env(
        &[raws_dir().to_str().unwrap(), "--start-loupe"],
        &[(
            "FASTCULL_DRIVE",
            "1500:home;1800:right;3000:resize:1440x700",
        )],
        &out,
    );
    let (x0, y0, x1, y1) = photo_rect(&out);
    let (pw, ph) = ((x1 - x0 + 1) as f64, (y1 - y0 + 1) as f64);
    let aspect = pw / ph;
    assert!(
        (1.40..=1.60).contains(&aspect),
        "after a vertical resize the loupe renders {pw}x{ph} (aspect \
         {aspect:.3}) — not one whole 3:2 frame"
    );
    // A stitched pair reads as ONE run only if the measurement walks the
    // inter-cell gap; `photo_rect`'s tolerance is below CELL_GAP so it
    // cannot. Belt and braces: the frame must start below the filter bar
    // rather than being clipped by it.
    assert!(y0 > 40, "frame starts at y {y0} — clipped at the top");
    let (_, h, _) = analyze(&out);
    assert!(
        y1 < h - 20,
        "frame runs to y {y1} of {h} — clipped at the bottom"
    );
}

/// Double-click must reach 1:1 from ABOVE fit, not only from fit.
///
/// This is the gesture issue #11 was built around, and it shipped dead: the
/// bridge's own proximity guard compared the two clicks as image fractions
/// taken either side of the first click's re-centre, so the "distance" it
/// measured was the recentre displacement and every double-click above fit
/// was vetoed. From fit it worked (a click there re-centres nothing), which
/// is exactly why two review gates passed it.
#[test]
fn loupe_double_click_above_fit_reaches_one_to_one() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("loupe-dblclick.jpg");
    // Zoom one rung above fit, let it settle, then double-click off-centre —
    // the case the guard rejected. The trace names the resulting factor.
    let stderr = shoot_env_stderr(
        &[raws_dir().to_str().unwrap(), "--start-loupe"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "1500:zoom-in;3000:dblclick:1000,300"),
        ],
        &out,
    );
    let factors: Vec<f32> = stderr
        .lines()
        .filter_map(|l| l.split("factor ").nth(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|f| f.parse::<f32>().ok())
        .collect();
    assert!(
        !factors.is_empty(),
        "no loupe factor traced — the drive script never reached the overlay\n{stderr}"
    );
    let peak = factors.iter().cloned().fold(f32::MIN, f32::max);
    // One rung above fit is 1.5x; 1:1 on an A1 frame in this window is ~6.9.
    assert!(
        peak > 3.0,
        "double-click above fit peaked at {peak:.3}x — it never reached 1:1 \
         (a stuck 1.5 means the gesture was vetoed, the shipped defect)\n{stderr}"
    );
}

#[test]
fn failed_badge_state_renders() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    // One good file + one corrupt: the failed cell renders its badge and
    // the session survives (badge pixels not asserted — smoke level).
    let dir = out_dir().join("mixed");
    std::fs::create_dir_all(&dir).unwrap();
    let good = raws_dir().join("A1_full_compressed.ARW");
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(&good, dir.join("good.ARW"));
    #[cfg(not(unix))]
    let _ = std::fs::copy(&good, dir.join("good.ARW"));
    std::fs::write(dir.join("broken.ARW"), vec![0xAB; 2048]).unwrap();
    let out = out_dir().join("badge.jpg");
    shoot(&[dir.to_str().unwrap()], &out);
    let (_, _, luma) = analyze(&out);
    assert!(luma > 5.0, "mixed-state frame black (luma {luma:.2})");
}

#[test]
fn synthetic_screenshot_renders_cells() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("synthetic.jpg");
    shoot(&["--synthetic", "500"], &out);
    let (_, _, luma) = analyze(&out);
    assert!(
        luma > 5.0,
        "synthetic grid rendered black (mean luma {luma:.2})"
    );
}

/// Center-anchored 1:1 entry regression (ui-grid.md zoom ladder; THE user
/// bug: 1:1 opened on the top-left corner). Runs --start-11 with tracing
/// and asserts the overlay's own report: factor at the ceiling, pan center
/// at the image center, and STRICTLY negative offsets on both axes — the
/// corner bug rendered at off 0,0.
#[test]
fn one_to_one_entry_is_center_anchored() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("center-anchor.jpg");
    let bin = env!("CARGO_BIN_EXE_fastcull-app");
    let output = std::process::Command::new(bin)
        .arg("--start-11")
        .arg(raws_dir())
        .arg("--screenshot")
        .arg(&out)
        .env("FASTCULL_NO_CACHE", "1")
        .env("FASTCULL_TRACE", "1")
        .output()
        .expect("run app");
    assert!(output.status.success(), "app exited with {}", output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .rfind(|l| l.contains("loupe idx"))
        .unwrap_or_else(|| panic!("no loupe trace line in stderr:\n{stderr}"));
    assert!(
        line.contains("center 0.500,0.500"),
        "1:1 entry did not anchor on the image center: {line}"
    );
    let offsets: Vec<f32> = line
        .split(" off ")
        .nth(1)
        .and_then(|s| s.trim().split(',').map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_else(|| panic!("unparseable offsets in: {line}"));
    assert!(
        offsets.len() == 2 && offsets.iter().all(|o| *o < -50.0),
        "corner-entry regression: offsets {offsets:?} (center entry needs \
         strictly negative pan on both axes): {line}"
    );
}

/// Issue #4 regression: opening a folder where NAME order diverges from
/// CAPTURE order must land the cursor on the capture-first image (view
/// position 0), not the name-first one (image id 0) — a real 1,450-file
/// folder opened with the cursor stranded at position 795. Crafted
/// fixture: the LATEST capture gets the name that sorts first. Asserted
/// via the 1:1 overlay trace (loupe idx = settled cursor id).
#[test]
fn cursor_opens_on_capture_first_image() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("cursor-order");
    std::fs::create_dir_all(&dir).unwrap();
    // a_late.ARW: captured 15:29:55 (uncompressed fixture); b_early.ARW:
    // captured 15:29:13 (compressed fixture). Name-first = capture-LAST.
    place_fixture(
        &raws_dir().join("A1_full_uncompressed.ARW"),
        &dir.join("a_late.ARW"),
    );
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("b_early.ARW"),
    );
    let out = out_dir().join("cursor-order.jpg");
    let bin = env!("CARGO_BIN_EXE_fastcull-app");
    // Up to 3 attempts: under full-suite CPU load the snapshot's 1.5 s
    // floor can fire BEFORE the second file's EXIF lands, and the
    // name-order cursor is then legitimately correct for that instant.
    // The guarded regression (corner-entry cursor stranding) is
    // deterministic — it fails all attempts.
    let mut last = String::new();
    for _ in 0..3 {
        let output = std::process::Command::new(bin)
            .arg("--start-11")
            .arg(&dir)
            .arg("--screenshot")
            .arg(&out)
            .env("FASTCULL_NO_CACHE", "1")
            .env("FASTCULL_TRACE", "1")
            .output()
            .expect("run app");
        assert!(output.status.success(), "app exited with {}", output.status);
        let stderr = String::from_utf8_lossy(&output.stderr);
        last = stderr
            .lines()
            .rfind(|l| l.contains("loupe idx"))
            .unwrap_or_else(|| panic!("no loupe trace line in stderr:\n{stderr}"))
            .to_string();
        if last.contains("loupe idx 1 ") {
            return; // capture-first cursor confirmed
        }
    }
    panic!("cursor must open on the capture-first image (b_early = id 1), got: {last}");
}

/// Issue #12 regression: opening the IPTC panel must DOCK it — the grid
/// stays pinned to the left edge (Slint centers an element whose width is
/// bound but whose x is not, which shifted the grid right by panel-w/2 and
/// slid the other half under the panel). Two shots of the same folder,
/// panel closed vs open (via the FASTCULL_DRIVE "iptc" action added for
/// exactly this: the bug shipped because no automated run could reach the
/// panel-open state).
#[test]
fn iptc_panel_docks_without_gutter() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let raws = raws_dir();
    let closed = out_dir().join("panel-closed.jpg");
    let open = out_dir().join("panel-open.jpg");
    shoot(&[raws.to_str().unwrap()], &closed);
    shoot_env(
        &[raws.to_str().unwrap()],
        &[("FASTCULL_DRIVE", "600:iptc")],
        &open,
    );
    // Sample the FIRST ROW of cells (the 3 A1 fixtures land in columns
    // 1-3 of 8; lower strips are empty background in both shots). The
    // band starts BELOW the filter-bar pills: a band overlapping them
    // reads their bright pixels as "texture" and passes on the broken
    // tree too (QE reproduced exactly that with 0.08; gutter strip
    // variance is 0.0 from y >= 0.12).
    const ROW1: (f64, f64) = (0.12, 0.20);
    // The panel must actually have opened: the right strip (inside the
    // 300px panel) goes from FLAT empty-grid background (the 3 photos
    // don't reach it) to panel chrome with field labels and borders.
    let (_, var_right_closed) = region_stats(&closed, 0.85, ROW1.0, 1.0, ROW1.1);
    let (_, var_right_open) = region_stats(&open, 0.85, ROW1.0, 1.0, ROW1.1);
    assert!(
        var_right_closed < 50.0 && var_right_open > 50.0,
        "panel never opened? right-strip variance closed {var_right_closed:.0} -> open {var_right_open:.0}"
    );
    // The regression itself: with the panel open, the LEFT edge must still
    // be grid photo content — the bug left a flat window-background gutter
    // (variance collapses to ~0) in x < panel_w/2.
    let (_, var_left_open) = region_stats(&open, 0.0, ROW1.0, 0.08, ROW1.1);
    assert!(
        var_left_open > 100.0,
        "left-edge gutter with panel open (variance {var_left_open:.1}) — grid is not left-pinned (issue #12)"
    );
}

/// Issue #12 / spec criterion: with the panel open, the overlay scrollbar
/// sits BETWEEN grid and panel, never buried under the panel
/// (ui-grid.md "the bar sits between grid and panel"). A --synthetic
/// session (overflowing grid → scrollbar instantiated) with the panel
/// driven open: the translucent thumb (#ffffff50 over hsv-value-0.22
/// cells ≈ luma 150) must show up in the 18px seam band left of the
/// panel edge (grid width 1140 of 1440 logical → x ∈ [0.779, 0.792]).
#[test]
fn scrollbar_sits_between_grid_and_panel() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("panel-scrollbar.jpg");
    shoot_env(
        &["--synthetic", "200"],
        &[("FASTCULL_DRIVE", "600:iptc")],
        &out,
    );
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let (x0, x1) = ((w as f64 * 0.779) as usize, (w as f64 * 0.792) as usize);
    let (y0, y1) = ((h as f64 * 0.05) as usize, (h as f64 * 0.70) as usize);
    let bright = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| (y * w + x) * 3))
        .filter(|i| {
            0.299 * px[*i] as f64 + 0.587 * px[*i + 1] as f64 + 0.114 * px[*i + 2] as f64 > 75.0
        })
        .count();
    // HOVER-INDEPENDENT thresholds (issue #16 gate finding): the IDLE
    // 6px #ffffff50 thumb over the reflowed dark grid edge measures max
    // luma 96-115 — the old >120 cutoff only passed when the desktop
    // pointer happened to hover the grab zone and brightened the thumb
    // (and before the #17 reflow fix, via cell content leaking under the
    // seam). Backdrop tops out ~60, idle thumb >=96: 75 discriminates
    // with margin in both directions and in both thumb styles. A
    // buried/missing thumb still reads 0.
    assert!(
        bright > 30,
        "no scrollbar thumb in the grid/panel seam ({bright} bright px) — \
         bar buried under the panel or not rendered (issue #12 / ui-grid.md)"
    );
}

/// M5 chrome smoke (validator finding: the menu bar, filter bar and empty
/// state had zero automated coverage): an empty folder must render the
/// empty-state message under the chrome and exit cleanly, not crash or
/// paint a uniform frame.
#[test]
fn empty_folder_renders_chrome_and_empty_state() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let empty = out_dir().join("empty-session-dir");
    std::fs::create_dir_all(&empty).unwrap();
    let out = out_dir().join("empty-state.jpg");
    shoot(&[empty.to_str().unwrap()], &out);
    let (w, h, _) = analyze(&out);
    assert!(w >= 640 && h >= 480, "implausible snapshot size {w}x{h}");
    let var = region_variance(&out, 1.0, 1.0);
    assert!(
        var > 1.0,
        "empty-state frame is uniform — no chrome/message rendered (variance {var:.2})"
    );
}

/// Issue #6 smoke: rapid keyboard navigation at 1:1 must never fold a
/// phantom "drag" into pan_center (the capture_pan trace fires on any
/// fold — during pure keyboard nav there must be none) and must exit
/// cleanly. LIMITATION, recorded in the issue: the visible 0x0-frame
/// symptom needs the GPU renderer + real key repeat and is NOT
/// reproducible under the software renderer — the structural fix (the
/// overlay is visibility-toggled, never re-created) plus this misfold
/// guard is what CAN be checked headlessly; the visual check stays
/// manual per the machine-freeze protocol.
#[test]
fn rapid_nav_at_one_to_one_never_folds_a_phantom_drag() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("rapid-nav");
    std::fs::create_dir_all(&dir).unwrap();
    for (src, dst) in [
        ("A1_full_compressed.ARW", "a.ARW"),
        ("A1_full_lossless_compressed.ARW", "b.ARW"),
        ("A1_full_uncompressed.ARW", "c.ARW"),
    ] {
        place_fixture(&raws_dir().join(src), &dir.join(dst));
    }
    let out = out_dir().join("rapid-nav.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            // The whole barrage sits BELOW the shutter's 1500 ms floor, so
            // every key fires before the earliest possible snapshot — with
            // real margin on release runners where all rungs decode in
            // ~400 ms and the readiness gate would otherwise open early
            // (validator: the old 1400+ timing sat exactly at the
            // assertion threshold, one scheduler flip from failure).
            (
                "FASTCULL_DRIVE",
                "700:right;750:left;800:right;850:left;900:right;950:left;1000:right;1050:left",
            ),
        ],
        &out,
    );
    let fired = stderr.lines().filter(|l| l.contains("drive: ")).count();
    assert!(
        fired >= 6,
        "nav barrage never ran ({fired} drive marks) — shutter fired too early, retune timings:\n{stderr}"
    );
    let folds: Vec<&str> = stderr.lines().filter(|l| l.contains("pan fold")).collect();
    assert!(
        folds.is_empty(),
        "phantom drag folded into pan_center during keyboard-only nav:\n{}",
        folds.join("\n")
    );
}

/// Issue #5: launching with NO arguments (desktop launcher, double-clicked
/// binary) must open the normal window in the "No folder open" empty
/// state — never exit(2) with a usage error nobody sees. The old behavior
/// makes `shoot` itself fail (non-zero exit), so this test IS the
/// old-vs-new discriminator.
#[test]
fn no_args_launch_opens_empty_window() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("no-args.jpg");
    let stderr = shoot_env_stderr(&[], &[("FASTCULL_TRACE", "1")], &out);
    let (w, h, _) = analyze(&out);
    assert!(w >= 640 && h >= 480, "implausible snapshot size {w}x{h}");
    let var = region_variance(&out, 1.0, 1.0);
    assert!(
        var > 1.0,
        "folderless frame is uniform — no chrome/message rendered (variance {var:.2})"
    );
    // Issue #19: the empty view must report an HONEST count — the old
    // "(0/1)" fabrication survived two human reviews because status
    // strings were untestable (hence the status-at-shutter trace).
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace line");
    assert!(
        status.contains("(0/0)"),
        "empty view fabricates a count: {status}"
    );
}

/// Issue #16: closing the IPTC panel at 1:1 must NOT swap the displayed
/// photo. Drive to image 5 (idx 4) at 1:1, toggle the panel open and
/// closed: the follow-scroll claim must never fire and the last overlay
/// trace must still be idx 4. (Pre-fix: the close direction snapped the
/// cursor to idx 3 — the QE fuzz hunt's deterministic repro.)
#[test]
fn panel_toggle_at_one_to_one_keeps_the_photo() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("panel-cursor");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 1..=6 {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir.join(format!("a{i}.ARW")),
        );
    }
    let out = out_dir().join("panel-cursor.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            // Let the metadata stream SETTLE before driving, then pin
            // with `home` (Windows CI 2026-07-27: six same-timestamp
            // fixtures re-sort as EXIF lands — keyed files sort before
            // keyless — and a right at 250 ms rode a transient order,
            // landing the cursor one frame off; the #20 badge traces
            // exposed the churn). `home` touches the cursor on the
            // SETTLED view, making every later step deterministic.
            (
                "FASTCULL_DRIVE",
                "1500:home;1650:right;1800:right;1950:right;2100:right;2400:iptc;2700:iptc",
            ),
        ],
        &out,
    );
    assert!(
        !stderr.contains("follow-scroll claim"),
        "panel toggle misread as scrolling — the cursor was claimed:\n{stderr}"
    );
    let last_idx = stderr
        .lines()
        .rev()
        .find_map(|l| {
            l.split("loupe idx ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .map(String::from)
        })
        .expect("no loupe trace lines");
    assert_eq!(
        last_idx, "4",
        "the displayed photo changed across the panel toggle:\n{stderr}"
    );
}

/// Issue #16, the user's ORIGINAL report: open a photo, RESIZE the
/// window — the same photo must still be shown. Uses the new
/// FASTCULL_DRIVE resize action; the relayout re-anchor path must fire
/// (proving the resize was seen as geometry, not scrolling).
#[test]
fn window_resize_keeps_the_photo() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("resize-cursor");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 1..=6 {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir.join(format!("a{i}.ARW")),
        );
    }
    let out = out_dir().join("resize-cursor.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            // Settle-then-pin, same rationale as the panel-toggle test
            // (the load-transient sort race; see that test's comment).
            // The two resizes sit 4 s apart: a stalled CI event loop
            // fires overdue timers BUNCHED, and back-to-back resizes
            // between two refreshes are a net geometry no-op — the
            // "relayout must fire" guard then fails vacuously (Windows
            // run 30304892053: a ~2.8 s startup stall bunched the whole
            // schedule). Bunching this pair now needs a 4 s stall; the
            // shutter waits for the full script, so the gap is free.
            (
                "FASTCULL_DRIVE",
                "1500:home;1650:right;1800:right;1950:right;2100:right;2500:resize:1000x700;6500:resize:1440x900",
            ),
        ],
        &out,
    );
    assert!(
        !stderr.contains("follow-scroll claim"),
        "window resize misread as scrolling — the cursor was claimed:\n{stderr}"
    );
    // The guard must actually have run (validator: without this the test
    // goes vacuously green if the resize stops dislodging the cursor).
    assert!(
        stderr.contains("relayout re-anchor"),
        "the relayout path never fired — the resize wasn't exercised:\n{stderr}"
    );
    let last_idx = stderr
        .lines()
        .rev()
        .find_map(|l| {
            l.split("loupe idx ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .map(String::from)
        })
        .expect("no loupe trace lines");
    assert_eq!(
        last_idx, "4",
        "the displayed photo changed across the window resize:\n{stderr}"
    );
}

/// Issue #17: opening the panel at GRID level must reflow the grid into
/// the remaining width with the cursor still visible — pre-fix the
/// stale-width layout left the cursor cell (and a whole column) hidden
/// UNDER the panel while the panel claimed to be editing it. Ground
/// truth: the cursor's blue border pixels must exist in the visible
/// grid area (left of the panel).
#[test]
fn grid_panel_open_reflows_and_keeps_cursor_visible() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("grid-panel-open.jpg");
    shoot_env(
        &["--synthetic", "500"],
        &[(
            "FASTCULL_DRIVE",
            "300:end;400:left;450:left;500:left;550:left;800:iptc",
        )],
        &out,
    );
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    // Cursor border is #4da3ff (JPEG-fuzzy match). Panel starts at
    // x = 1140/1440 of the width; search only the VISIBLE grid area.
    let x_max = (w as f64 * (1140.0 / 1440.0)) as usize;
    let blue = (0..h)
        .flat_map(|y| (0..x_max).map(move |x| (y * w + x) * 3))
        .filter(|i| {
            let (r, g, b) = (px[*i] as i32, px[*i + 1] as i32, px[*i + 2] as i32);
            (r - 0x4d).abs() < 40 && (g - 0xa3).abs() < 40 && (b - 0xff).abs() < 40
        })
        .count();
    assert!(
        blue > 50,
        "cursor border not visible left of the panel ({blue} blue px) — \
         grid did not reflow on panel open (issue #17)"
    );
}

/// Grid resize anchoring (user report: shrink → "scrolls up", grow →
/// "scrolls down"): a mid-scroll SHRINK must keep the content anchored
/// — pre-fix the raw pixel offset landed ~4 rows deeper and the cursor
/// (top-of-viewport before) vanished above the viewport (QE repro).
#[test]
fn grid_resize_shrink_keeps_content_anchored() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    // Control run (same script, no final resize): the landing position
    // depends on rows-per-page and thus the runner's window geometry —
    // Windows CI landed on (76/300) where local runs land (108/300).
    // The invariant is "the cursor does not move ACROSS THE RESIZE",
    // asserted by comparing against this control, never a hardcoded
    // position.
    let control_out = out_dir().join("grid-resize-shrink-control.jpg");
    let control = shoot_env_stderr(
        &["--synthetic", "300"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "150:resize:1200x800;500:end;700:pgup;800:pgup;900:pgup;1000:pgup",
            ),
        ],
        &control_out,
    );
    let control_status = control
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no control status trace")
        .to_string();
    let out = out_dir().join("grid-resize-shrink.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "150:resize:1200x800;500:end;700:pgup;800:pgup;900:pgup;1000:pgup;1150:resize:900x800",
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("grid relayout re-anchor"),
        "the grid anchoring path never fired:\n{stderr}"
    );
    // The cursor was visible (top of viewport) before the shrink and
    // must still be visible after — pre-fix it was lost above the view.
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let blue = (0..h)
        .flat_map(|y| (0..w).map(move |x| (y * w + x) * 3))
        .filter(|i| {
            let (r, g, b) = (px[*i] as i32, px[*i + 1] as i32, px[*i + 2] as i32);
            (r - 0x4d).abs() < 40 && (g - 0xa3).abs() < 40 && (b - 0xff).abs() < 40
        })
        .count();
    assert!(
        blue > 50,
        "cursor not visible after shrink ({blue} blue px) — content drifted"
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace");
    assert_eq!(
        status, control_status,
        "cursor moved across the resize (control vs resize run)"
    );
}

/// Growing the window at the BOTTOM clamp must keep the bottom pinned —
/// pre-fix the stale offset stranded the viewport mid-list with the
/// last row and cursor lost off-screen (QE edge probe P2b, the worst
/// flavor).
#[test]
fn grid_resize_grow_at_bottom_stays_at_bottom() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("grid-resize-bottom.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "150:resize:1200x800;500:end;1000:resize:1500x800",
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("grid relayout re-anchor"),
        "the grid anchoring path never fired:\n{stderr}"
    );
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let blue = (0..h)
        .flat_map(|y| (0..w).map(move |x| (y * w + x) * 3))
        .filter(|i| {
            let (r, g, b) = (px[*i] as i32, px[*i + 1] as i32, px[*i + 2] as i32);
            (r - 0x4d).abs() < 40 && (g - 0xa3).abs() < 40 && (b - 0xff).abs() < 40
        })
        .count();
    assert!(
        blue > 50,
        "cursor (at End) not visible after grow ({blue} blue px) — viewport stranded mid-list"
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace");
    assert!(
        status.contains("(300/300)"),
        "cursor moved across the resize: {status}"
    );
}

/// D1 (validator+QE): content that FIT the old viewport (old_max == 0,
/// scroll 0) must stay at the TOP when the window grows into overflow —
/// the bottom-pin branch used to classify "fits entirely" as "at the
/// bottom clamp" and jump the viewport to new_max.
#[test]
fn grid_resize_fits_to_overflow_stays_at_top() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("grid-resize-fits.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "64"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "150:resize:900x800;400:down;500:down;600:down;700:down;1000:resize:1600x800",
            ),
        ],
        &out,
    );
    // Pre-fix trace: "grid relayout re-anchor: scroll 0 -> 385" — the
    // fixed code writes no correction at scroll 0.
    assert!(
        !stderr.contains("grid relayout re-anchor"),
        "fits-to-overflow grow wrote a scroll correction:\n{stderr}"
    );
    // The first row must still be at the top: SYN00000's cell content
    // visible implies no jump; ground-truth via the cursor which the
    // downs left mid-view and which must remain visible.
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let blue = (0..h)
        .flat_map(|y| (0..w).map(move |x| (y * w + x) * 3))
        .filter(|i| {
            let (r, g, b) = (px[*i] as i32, px[*i + 1] as i32, px[*i + 2] as i32);
            (r - 0x4d).abs() < 40 && (g - 0xa3).abs() < 40 && (b - 0xff).abs() < 40
        })
        .count();
    assert!(
        blue > 50,
        "cursor lost after fits-to-overflow grow ({blue} blue px)"
    );
}

/// Resize at scroll 0: the top of the list stays pinned, no spurious
/// re-anchor scroll writes (QE edge probe P1).
#[test]
fn grid_resize_at_top_stays_at_top() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("grid-resize-top.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "150:resize:1200x800;800:resize:900x800"),
        ],
        &out,
    );
    // Scroll 0 must stay 0: no re-anchor scroll write may fire (the
    // trace only appears when the offset actually changes — validator:
    // this is the assertion with discriminating power at the top).
    assert!(
        !stderr.contains("grid relayout re-anchor"),
        "a top-of-list resize wrote a scroll correction:\n{stderr}"
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace");
    assert!(
        status.contains("(1/300)"),
        "cursor moved on a top-of-list resize: {status}"
    );
}

/// Issue #21 (user-approved): during held-arrow transit at zoom, the
/// view must stay at the carried factor rendered SOFT from the mid
/// rung (flagged), never drop to fit — and the landing frame must end
/// sharp. The transit naturally outruns the full-res ladder in both
/// profiles (release ~140ms cooks vs 60ms key spacing; debug ~12s
/// cooks with the virgin-pin rule rendering soft on mid adoption).
#[test]
fn transit_at_zoom_stays_soft_never_drops_to_fit() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("soft-transit");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 1..=6 {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir.join(format!("a{i}.ARW")),
        );
    }
    let out = out_dir().join("soft-transit.jpg");
    // No starvation knob: FASTCULL_MAX_READERS governs the thumbnail
    // pipeline, NOT the loupe ladder (gate finding — it was a no-op
    // here). The race is real in both profiles: release full-res cooks
    // ~140ms against 60ms key spacing; debug cooks ~12s, and the
    // virgin-pin rule renders soft the moment the landing mid adopts,
    // long before the shutter's sharp gate opens.
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "700:right;760:right;820:right;880:right;940:right",
            ),
        ],
        &out,
    );
    // The transit rendered SOFT at least once (pre-#21: the string does
    // not exist — the view dropped to fit instead).
    assert!(
        stderr.contains("loupe soft idx"),
        "no soft transit render occurred:\n{stderr}"
    );
    // And the landing frame ended SHARP (a plain sharp loupe line for
    // the final cursor appears after the last soft one).
    let last_soft = stderr.rfind("loupe soft idx").unwrap();
    let sharp_after = stderr[last_soft..].contains("\n")
        && stderr[last_soft..]
            .lines()
            .skip(1)
            .any(|l| l.contains("loupe idx ") && !l.contains("loupe soft"));
    assert!(
        sharp_after,
        "the landing frame never swapped in sharp:\n{stderr}"
    );
}

/// Issue #20: the loupe state badge — the cursor's mark must be readable
/// in the loupe itself, and it must always be the CURRENT frame's mark
/// (auto-advance makes memory of "the frame I marked" one frame stale by
/// construction; the walk-back to compare candidates is the exact case).
/// Pre-#20 neither the badge traces nor the status-bar mark words exist.
#[test]
fn loupe_badge_tracks_marks_across_auto_advance() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("loupe-badge-marks.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "300", "--start-loupe"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "400:pick;700:reject;1000:left;1300:left"),
        ],
        &out,
    );
    // Y/N auto-advance (net movement one image): pick lands on idx 0 →
    // cursor 1; reject lands on 1 → cursor 2; two lefts walk back across
    // the marked frames. Each arrival must trace that frame's OWN mark.
    let rejected_at = stderr.find("loupe badge idx 1 mark rejected");
    let picked_at = stderr.find("loupe badge idx 0 mark picked");
    assert!(
        rejected_at.is_some(),
        "walk-back onto the rejected frame never showed its badge:\n{stderr}"
    );
    assert!(
        picked_at.is_some(),
        "walk-back onto the picked frame never showed its badge:\n{stderr}"
    );
    assert!(
        rejected_at < picked_at,
        "badge states arrived out of walk-back order:\n{stderr}"
    );
    // Status-bar backstop: the state spelled in words at the shutter
    // (cursor rests on the picked frame).
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace");
    assert!(
        status.contains("★ picked"),
        "status bar does not spell the mark: {status}"
    );
}

/// Issue #20 (persona-validated divergence from the grid): a rejected
/// frame is NEVER dimmed in the loupe — you may be re-judging a reject
/// for rescue and need full brightness. Pre-#20 the fit loupe was a
/// grid cell, so the grid's 40% reject dim leaked in (this compare
/// fails on old code).
#[test]
fn loupe_never_dims_a_rejected_frame() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    // Control: the same frame at fit, unmarked.
    let control_out = out_dir().join("loupe-reject-dim-control.jpg");
    shoot_env(&["--synthetic", "300", "--start-loupe"], &[], &control_out);
    let (_, _, control_luma) = analyze(&control_out);
    // Reject idx 0 (auto-advance to 1), walk back onto the reject.
    let out = out_dir().join("loupe-reject-dim.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "300", "--start-loupe"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "400:reject;800:left"),
        ],
        &out,
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace");
    assert!(
        status.contains("✕ rejected"),
        "cursor is not on the rejected frame at the shutter: {status}"
    );
    let (_, _, luma) = analyze(&out);
    assert!(
        luma > control_luma * 0.85,
        "rejected frame is dimmed in the loupe (mean luma {luma:.1} vs \
         unmarked control {control_luma:.1}) — rescue judging needs full \
         brightness:\n{stderr}"
    );
}

/// Issue #20 at 1:1: the badge renders IN PIXELS over the zoomed view
/// (the fit tests above prove state tracking; this proves the zoomed
/// loupe shows it too — the exact view the user culls in). The star
/// glyph is #ffd24d on a dark #202028 pill in the top-left corner.
/// Fixtures are SYMLINKED into a temp dir — driving `pick` writes a
/// real sidecar next to the file, and it must never land in the shared
/// testdata/raws (validator M1: a fixture picked once is picked in
/// every later run — order-dependent state).
#[test]
fn loupe_badge_star_renders_at_one_to_one() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("badge-11");
    std::fs::create_dir_all(&dir).unwrap();
    let src = raws_dir().join("A1_full_compressed.ARW");
    place_fixture(&src, &dir.join("badge_a.ARW"));
    place_fixture(&src, &dir.join("badge_b.ARW"));
    let out = out_dir().join("loupe-badge-11.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "600:pick;1000:left"),
        ],
        &out,
    );
    // pick marks idx 0 and advances; left returns to the picked frame.
    // The shutter's sharp gate then waits for idx 0's full-res.
    assert!(
        stderr.contains("loupe badge idx 0 mark picked"),
        "the walk-back never traced the picked badge:\n{stderr}"
    );
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    // A bare w/8 x h/8 corner sweep scored 58 "yellow" px on a
    // badge-less shot of this RAW's foliage (QE D1: vacuous pass), and
    // the pill's exact position shifts with menu-bar height and DPI.
    // So: a yellow pixel only counts when its ±6 px neighborhood holds
    // dark near-neutral PILL BACKING pixels (#202028cc over a photo) —
    // foliage yellow sits in foliage, never on the pill.
    let is_pill = |x: usize, y: usize| {
        let i = (y * w + x) * 3;
        let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
        r < 0x48 && g < 0x48 && b < 0x50 && (r - g).abs() < 24 && (b - g).abs() < 32
    };
    let (xr, yr) = (w / 6, h / 6);
    let mut on_pill_yellow = 0usize;
    for y in 6..yr {
        for x in 6..xr {
            let i = (y * w + x) * 3;
            let (r, g, b) = (px[i] as i32, px[i + 1] as i32, px[i + 2] as i32);
            let yellowish = (r - 0xff).abs() < 48 && (g - 0xd2).abs() < 48 && (b - 0x4d).abs() < 64;
            if !yellowish {
                continue;
            }
            let dark_neighbors = (y - 6..y + 6)
                .flat_map(|ny| (x - 6..x + 6).map(move |nx| (nx, ny)))
                .filter(|(nx, ny)| is_pill(*nx, *ny))
                .count();
            if dark_neighbors >= 8 {
                on_pill_yellow += 1;
            }
        }
    }
    assert!(
        on_pill_yellow > 6,
        "no star-on-pill in the top-left region at 1:1 \
         ({on_pill_yellow} on-pill yellow px):\n{stderr}"
    );
}

/// Issue #18: the 1:1 anchor recomputes across a panel toggle. OPEN
/// must re-center the crop for the docked width (the original drift
/// kept the stale full-width anchor indefinitely); CLOSE must restore
/// the full-width anchor with no stale frame (the one-frame zoom-pop).
/// Sharp-path anchor values (`loupe idx ... off X,Y`) only exist while
/// full-res is up: in release the sharp view is up before the toggles
/// and the full contract is asserted; in debug the toggles happen in
/// the soft regime, so only the post-toggle stability half applies —
/// the release assertions are the regression teeth (fails on pre-#16
/// code: no docked line ever appeared after open, and close popped a
/// stale docked frame).
#[test]
fn panel_toggle_at_one_to_one_reanchors_the_crop() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("panel-reanchor");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 1..=3 {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir.join(format!("a{i}.ARW")),
        );
    }
    let out = out_dir().join("panel-reanchor.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "1500:home;2000:iptc;2600:iptc"),
        ],
        &out,
    );
    assert!(
        !stderr.contains("follow-scroll claim"),
        "panel toggle misread as scrolling:\n{stderr}"
    );
    // Wrong-frame guard: a toggle is GEOMETRY, never navigation.
    let off_x = |line: &str| -> Option<i64> {
        line.split(" off ")
            .nth(1)?
            .split(',')
            .next()?
            .trim()
            .parse()
            .ok()
    };
    let lines: Vec<&str> = stderr.lines().collect();
    let open_at = lines
        .iter()
        .position(|l| l.contains("drive: iptc"))
        .expect("open toggle missing");
    let close_at = lines
        .iter()
        .rposition(|l| l.contains("drive: iptc"))
        .expect("close toggle missing");
    assert!(close_at > open_at, "both toggles must have fired");
    let sharp_offs = |range: std::ops::Range<usize>| -> Vec<i64> {
        lines[range]
            .iter()
            .filter(|l| l.contains("loupe idx "))
            .filter_map(|l| off_x(l))
            .collect()
    };
    // Both profiles: everything after CLOSE is one stable anchor.
    let after = sharp_offs(close_at..lines.len());
    assert!(
        !after.is_empty(),
        "no sharp anchor line after the close toggle:\n{stderr}"
    );
    assert!(
        after.windows(2).all(|w| w[0] == w[1]),
        "anchor unstable after panel close (drift or pop): {after:?}\n{stderr}"
    );
    // Release-strength half: sharp view was up before the toggles.
    // CI runs the screenshot suite in RELEASE on both platforms, so
    // these are the teeth that actually run there — the debug half
    // above cannot detect a stable-but-WRONG anchor (validator note:
    // don't drop the release CI run thinking debug covers this). In
    // release the teeth may never silently skip: a runner too slow to
    // have the sharp view up before the 2000 ms toggle must FAIL
    // loudly here, not pass vacuously forever.
    let before = sharp_offs(0..open_at);
    #[cfg(not(debug_assertions))]
    assert!(
        !before.is_empty(),
        "release run reached the open toggle without a sharp baseline — \
         the regression teeth would be skipped:\n{stderr}"
    );
    if let Some(&baseline) = before.last() {
        let docked = sharp_offs(open_at..close_at);
        assert!(
            docked.iter().any(|o| *o != baseline),
            "panel OPEN never re-anchored the crop for the docked width \
             (issue #18 drift): baseline {baseline}, open-window {docked:?}\n{stderr}"
        );
        assert!(
            after.iter().all(|o| *o == baseline),
            "panel CLOSE did not restore the full-width anchor (stale \
             pop frame): baseline {baseline}, after {after:?}\n{stderr}"
        );
    }
}

/// Issue #23: the About dialog renders and the modal contains the
/// keyboard (user decision: "swallow everything in that screen").
/// Driven reject/pick with About open must mark NOTHING. Fails on old
/// code: the `about` drive didn't exist (Help > About routed to the
/// shortcuts popup), so the marks fire and the counts assert breaks.
#[test]
fn about_dialog_renders_and_contains_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("about-dialog.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "200"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "600:about;900:reject;1200:pick"),
        ],
        &out,
    );
    assert!(
        stderr.contains("about toggled to true"),
        "About never opened:\n{stderr}"
    );
    // The build-composed version reached the dialog property.
    assert!(
        stderr.contains(&format!("about version {}", fastcull_core::VERSION)),
        "version string not composed from the crate version:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("drive swallowed by modal").count(),
        2,
        "reject/pick were not both swallowed by the modal:\n{stderr}"
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace");
    assert!(
        status.contains("★0 ✕0"),
        "a mark leaked through the About modal: {status}"
    );
    // The card's bright text over the dark backing: the synthetic grid
    // tops out near luma 56 (hsv v=0.22) and its labels at ~130, so
    // >150-luma pixels in the centered card region prove the dialog
    // actually rendered (the About stays open through the shutter).
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let bright = (h * 35 / 100..h * 65 / 100)
        .flat_map(|y| (w * 35 / 100..w * 65 / 100).map(move |x| (y * w + x) * 3))
        .filter(|i| {
            0.299 * px[*i] as f64 + 0.587 * px[*i + 1] as f64 + 0.114 * px[*i + 2] as f64 > 150.0
        })
        .count();
    assert!(
        bright > 100,
        "no dialog text rendered in the center region ({bright} bright px)"
    );
}

/// Issue #23's persona finding: the shortcuts popup used to swallow
/// ONLY Esc — pressing N while reading the key list rejected the photo
/// under the scrim. Same containment as About now. Fails on old code
/// (no `shortcuts` drive: the popup never opens, the reject fires).
#[test]
fn shortcuts_popup_contains_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("shortcuts-contained.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "200"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "600:shortcuts;900:reject"),
        ],
        &out,
    );
    assert!(
        stderr.contains("shortcuts toggled to true"),
        "shortcuts popup never opened:\n{stderr}"
    );
    assert!(
        stderr.contains("drive swallowed by modal: reject"),
        "the reject was not swallowed:\n{stderr}"
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace");
    assert!(
        status.contains("★0 ✕0"),
        "a mark leaked through the shortcuts modal: {status}"
    );
}

/// Mean (B − R) over a fractional sub-rectangle. The selection wash is a BLUE
/// tint, and blue-minus-red isolates it from plain brightness changes: a
/// merely brighter cell lifts every channel equally and moves this number
/// very little, while the wash lifts B and pulls R down.
fn region_blue_bias(path: &Path, fx0: f64, fy0: f64, fx1: f64, fy1: f64) -> f64 {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let (x0, x1) = ((w as f64 * fx0) as usize, (w as f64 * fx1) as usize);
    let (y0, y1) = ((h as f64 * fy0) as usize, (h as f64 * fy1) as usize);
    let (mut acc, mut n) = (0.0f64, 0.0f64);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * 3;
            acc += px[i + 2] as f64 - px[i] as f64;
            n += 1.0;
        }
    }
    acc / n
}

/// Three fixtures with DISTINCT capture times, so view order is deterministic
/// and the same photo lands in cell 1 on every run — the wash assertions
/// compare the same region across two processes.
fn place_three_distinct(dir: &Path) {
    for (name, src) in [
        ("a.ARW", "A1_full_compressed.ARW"),
        ("b.ARW", "A1_full_lossless_compressed.ARW"),
        ("c.ARW", "A1_full_uncompressed.ARW"),
    ] {
        place_fixture(&raws_dir().join(src), &dir.join(name));
    }
}

/// Selection wash (ui-grid.md "Selection", user request 2026-07-28): a
/// selected grid cell carries a translucent accent-blue tint, and the status
/// bar states the blast radius. Both halves are load-bearing — the wash says
/// WHICH images the next batch key hits, the count says HOW MANY (a selection
/// can scroll off-screen, where no tint can help).
#[test]
fn selection_wash_tints_the_grid_and_status_counts() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("sel-wash-grid");
    std::fs::create_dir_all(&dir).unwrap();
    place_three_distinct(&dir);
    let folder = dir.to_str().unwrap();
    // Cell 1's interior at 8 columns. Both runs `home` first so the cursor is
    // pinned on the SETTLED view before anything is selected (the capture-sort
    // churn that bit the #20 badge tests lands well before 700 ms).
    let (fx0, fy0, fx1, fy1) = (0.02, 0.11, 0.10, 0.20);

    let plain = out_dir().join("sel-wash-none.jpg");
    let plain_err = shoot_env_stderr(
        &[folder],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", "700:home")],
        &plain,
    );
    let sel = out_dir().join("sel-wash-some.jpg");
    let sel_err = shoot_env_stderr(
        &[folder],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "700:home;900:shift-right;1000:shift-right",
            ),
        ],
        &sel,
    );
    let status_of = |s: &str| {
        s.lines()
            .rev()
            .find_map(|l| l.split("status at shutter: ").nth(1))
            .expect("no status trace line")
            .to_string()
    };
    let (plain_status, sel_status) = (status_of(&plain_err), status_of(&sel_err));

    // An empty selection is SILENT: the batch is then just the cursor, and
    // "1 selected" on every unmarked image would be noise.
    assert!(
        !plain_status.contains("selected"),
        "empty selection must not report a count: {plain_status}"
    );
    // Shift+Right twice = a 3-image span (anchor included).
    assert!(
        sel_status.contains("· 3 selected"),
        "status must state the blast radius: {sel_status}"
    );
    let (plain_bias, sel_bias) = (
        region_blue_bias(&plain, fx0, fy0, fx1, fy1),
        region_blue_bias(&sel, fx0, fy0, fx1, fy1),
    );
    assert!(
        sel_bias - plain_bias > 8.0,
        "selected cell is not visibly tinted: blue bias {plain_bias:.1} -> {sel_bias:.1}"
    );
    // Spec acceptance criterion: the wash renders on the CURSOR cell too.
    // `home;shift-right;shift-right` parks the cursor on view index 2 — the
    // third cell — so sampling cell 1 alone would leave the pre-wash
    // `&& !cell.is-cursor` exclusion (the exact bug this criterion was
    // written against) passing the suite. QE proved that mutation survived
    // before this block existed.
    let (cx0, cy0, cx1, cy1) = (0.28, 0.11, 0.35, 0.20);
    let (plain_cursor, sel_cursor) = (
        region_blue_bias(&plain, cx0, cy0, cx1, cy1),
        region_blue_bias(&sel, cx0, cy0, cx1, cy1),
    );
    assert!(
        sel_cursor - plain_cursor > 8.0,
        "the CURSOR cell is not tinted — selection state hidden on the one \
         cell whose batch membership is ambiguous: {plain_cursor:.1} -> {sel_cursor:.1}"
    );
}

/// Mean blue of the glyph pixels inside a region — "glyph" being the pick
/// star's yellow. Measured as mean **R − B** ("yellowness") over the glyph
/// pixels, which is what makes this a z-order assertion rather than a
/// brightness one: the photo behind the badge legitimately shifts blue under
/// the wash, the badge must not.
///
/// The selection threshold is deliberately FAR below the value a washed glyph
/// still has (a 25% blend of the star with the accent blue lands near R−B≈100,
/// well above the ≥60 cutoff), so the filter can never truncate the effect
/// being measured. An earlier version of this helper filtered on `b <= 160`
/// and thereby discarded exactly the pixels the wash pushes past that bound —
/// it passed on the very mutation it claimed to catch (validator finding).
fn region_glyph_yellowness(path: &Path, fx0: f64, fy0: f64, fx1: f64, fy1: f64) -> Option<f64> {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    let (x0, x1) = ((w as f64 * fx0) as usize, (w as f64 * fx1) as usize);
    let (y0, y1) = ((h as f64 * fy0) as usize, (h as f64 * fy1) as usize);
    let (mut acc, mut n) = (0.0f64, 0.0f64);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * 3;
            let (r, b) = (px[i] as i32, px[i + 2] as i32);
            if r >= 180 && r - b >= 60 {
                acc += (r - b) as f64;
                n += 1.0;
            }
        }
    }
    (n > 20.0).then(|| acc / n)
}

/// Spec acceptance criterion: the wash is painted BELOW the badges, so
/// ★ / ✕ / ×N / ✓ / ! stay legible on a selected cell. Without this, moving
/// the wash Rectangle to the end of the cell's child list — an easy accident
/// for the next person adding an overlay — washes the badges out and the
/// suite stays green (QE mutation M7).
#[test]
fn selection_wash_stays_below_the_pick_badge() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("sel-wash-badge");
    std::fs::create_dir_all(&dir).unwrap();
    place_three_distinct(&dir);
    let folder = dir.to_str().unwrap();
    // The star sits at 8px/4px inside cell 1 at 20px — a small box in the
    // cell's top-left corner.
    let (gx0, gy0, gx1, gy1) = (0.004, 0.088, 0.026, 0.125);

    // Picked but NOT selected: `pick` marks the cursor image and advances.
    let picked = out_dir().join("sel-badge-plain.jpg");
    shoot_env_stderr(
        &[folder],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "700:home;900:pick"),
        ],
        &picked,
    );
    // Picked AND selected: mark, return home, then span the first two cells.
    let both = out_dir().join("sel-badge-washed.jpg");
    let both_err = shoot_env_stderr(
        &[folder],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "700:home;900:pick;1100:home;1300:shift-right",
            ),
        ],
        &both,
    );
    // Anti-vacuity: the second frame really is a selected, picked cell.
    let status = both_err
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace line");
    assert!(
        status.contains("· 2 selected") && status.contains("★1"),
        "fixture state wrong — test would be vacuous: {status}"
    );
    let star_plain = region_glyph_yellowness(&picked, gx0, gy0, gx1, gy1)
        .expect("no pick star found in the unselected frame");
    let star_washed = region_glyph_yellowness(&both, gx0, gy0, gx1, gy1)
        .expect("no pick star found in the selected frame — badge washed away?");
    // Below the badge: the glyph is untouched, so yellowness barely moves.
    // Above it: a 25% blue blend drains it by ~90 (mutation-verified — the
    // wash-over-badges build must FAIL this assertion, not merely pass it).
    assert!(
        star_plain - star_washed < 40.0,
        "the wash is painted OVER the pick badge: star yellowness \
         {star_plain:.1} -> {star_washed:.1}"
    );
}

/// The wash is GRID ONLY — it must never reach the loupe, where the user is
/// judging pixels (persona, pre-implementation review). The loupe fit view is
/// the grid rendered at ONE column, so this is a real gating risk, not a
/// theoretical one: an ungated `cell.selected` tint would recolor the photo
/// being evaluated.
#[test]
fn selection_wash_never_reaches_the_loupe() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("sel-wash-loupe");
    std::fs::create_dir_all(&dir).unwrap();
    place_three_distinct(&dir);
    let folder = dir.to_str().unwrap();

    let plain = out_dir().join("sel-loupe-none.jpg");
    shoot_env_stderr(
        &["--start-loupe", folder],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", "700:home")],
        &plain,
    );
    let sel = out_dir().join("sel-loupe-all.jpg");
    let sel_err = shoot_env_stderr(
        &["--start-loupe", folder],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", "700:home;900:select-all"),
        ],
        &sel,
    );
    // Anti-vacuity guard: prove the selection was actually ACTIVE in the
    // second run. Without this the test passes trivially if `select-all`
    // silently does nothing, which is exactly how a no-op regression test
    // ships (see the window-resize vacuous-pair fix).
    let sel_status = sel_err
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .expect("no status trace line");
    assert!(
        sel_status.contains("· 3 selected"),
        "select-all did not select in the loupe — test would be vacuous: {sel_status}"
    );
    // Second anti-vacuity guard: a photo must actually be ON SCREEN. If the
    // loupe ever failed to render, both frames would be flat and the
    // "unchanged" assertion below would pass trivially — proving nothing
    // (validator finding; the suite's own convention for "real pixels, not a
    // gray box" is region_variance, which measures ~3300 here).
    for frame in [&plain, &sel] {
        let var = region_variance(frame, 0.8, 0.8);
        assert!(
            var > 100.0,
            "loupe frame has no photo in it (variance {var:.1}) — the \
             comparison below would be vacuous"
        );
    }
    // The photo area must be unchanged. Compared as blue bias rather than a
    // whole-frame diff so the status bar's own "· 3 selected" text (which
    // legitimately differs) cannot mask or fake the result.
    let (plain_bias, sel_bias) = (
        region_blue_bias(&plain, 0.2, 0.2, 0.8, 0.8),
        region_blue_bias(&sel, 0.2, 0.2, 0.8, 0.8),
    );
    assert!(
        (sel_bias - plain_bias).abs() < 2.0,
        "the wash leaked into the loupe: blue bias {plain_bias:.1} -> {sel_bias:.1}"
    );
}
