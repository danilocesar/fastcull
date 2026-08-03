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
        // Never read or write the user's real ui.toml either (issue #13
        // gap, surfaced by the issue #41 sweep): a driven copy dialog
        // otherwise shows the user's real remembered destination.
        .env("FASTCULL_NO_CONFIG", "1")
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
    let stderr = shoot_env_stderr(
        &[raws_dir().to_str().unwrap(), "--start-loupe"],
        &[("FASTCULL_TRACE", "1")],
        &out,
    );
    // PRIMARY assertion, on the app's own logical-pixel numbers: whatever
    // the runner's resolution or DPI, the one-column cell must fit the grid
    // area, because that is what "the whole image is on screen" means.
    let (cell_h, grid_h, _, _) = shutter_geometry(&stderr);
    assert!(
        cell_h + 12.0 <= grid_h + 0.5,
        "the loupe cell is {cell_h} tall in a {grid_h} grid area — it \
         overflows, so the bottom of every frame is below the fold"
    );
    // SECONDARY, on pixels: the width a true fit gives up must show as black
    // pillarbox bars. Measured on one scanline through the middle of the
    // grid area — no texture assumptions, so a smooth patch cannot break it
    // the way a contiguous-texture walk did on the Windows runner.
    let (w, h, _) = analyze(&out);
    let bars = black_bars_on_scanline(&out, h / 2);
    assert!(
        bars.0 > 40 && bars.1 > 40,
        "no pillarbox bars (left {} px, right {} px of {w}) — the frame is \
         filling the width, which it can only do by cropping",
        bars.0,
        bars.1
    );
}

/// Width of the leading and trailing near-black runs on one scanline,
/// skipping the 12 px that the cursor border occupies at each edge.
fn black_bars_on_scanline(path: &Path, y: usize) -> (usize, usize) {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, _) = dec.dimensions().expect("dims");
    let dark = |x: usize| {
        let i = (y * w + x) * 3;
        (px[i] as u32 + px[i + 1] as u32 + px[i + 2] as u32) / 3 < 14
    };
    let mut left = 0;
    for x in 12..w {
        if dark(x) {
            left += 1;
        } else {
            break;
        }
    }
    let mut right = 0;
    for x in (0..w - 12).rev() {
        if dark(x) {
            right += 1;
        } else {
            break;
        }
    }
    (left, right)
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
    let stderr = shoot_env_stderr(
        &[raws_dir().to_str().unwrap(), "--start-loupe"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                // The trailing pair is a SETTLE step, not decoration. The
                // relayout re-anchor corrects the offset and then schedules
                // a 0 ms follow-up refresh to render it; with the resize as
                // the last drive action the shutter can fire in that same
                // tick and capture the pre-correction offset. Measured
                // flake before this step: 4/20 on HEAD, 3/20 here — it is
                // not a regression, it is a test that was always racing.
                // The settle must NOT itself re-anchor, or it repairs the
                // very state under test: a panel-toggle pair was tried and
                // made this test vacuous (the mutant passed), because the
                // toggle changes grid width and triggers its own relayout
                // correction. The About modal changes no grid geometry, so
                // it holds the shutter and nothing else.
                "FASTCULL_DRIVE",
                "1500:home;1800:right;3000:resize:1440x700;3600:about;4000:about",
            ),
        ],
        &out,
    );
    let (cell_h, grid_h, scroll, cursor_top) = shutter_geometry(&stderr);
    // The cursor's WHOLE cell must lie inside the scrolled viewport. When
    // the re-anchor fired only for a wholly off-screen cell, a height
    // resize left it straddling the fold — the bottom of one photo above
    // the top of the next.
    assert!(
        cursor_top >= scroll - 0.5,
        "cursor cell top {cursor_top} is above the scroll offset {scroll}"
    );
    assert!(
        cursor_top + cell_h <= scroll + grid_h + 0.5,
        "cursor cell ends at {} but the viewport ends at {} — the frame is \
         split across the fold",
        cursor_top + cell_h,
        scroll + grid_h
    );
}

/// `(cell_height, grid_height, scroll, cursor_top)` in LOGICAL px from the
/// app's `geometry at shutter` trace. Pixel measurements of the rendered
/// frame are resolution- and DPI-dependent and broke twice on the Windows
/// runner while the app behaved correctly; these requirements are
/// statements about numbers, so assert the numbers.
fn shutter_geometry(stderr: &str) -> (f64, f64, f64, f64) {
    let geom = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("geometry at shutter: ").nth(1))
        .unwrap_or_else(|| panic!("no geometry trace in stderr:\n{stderr}"))
        .to_string();
    let field = |after: &str, idx: usize| -> f64 {
        geom.split(after)
            .nth(1)
            .unwrap_or_else(|| panic!("field {after:?} missing: {geom}"))
            .trim()
            .split(['x', ' '])
            .nth(idx)
            .unwrap_or_else(|| panic!("component {idx} of {after:?}: {geom}"))
            .parse()
            .unwrap_or_else(|e| panic!("{after:?} not a number ({e}): {geom}"))
    };
    assert_eq!(field("columns ", 0), 1.0, "not at the loupe: {geom}");
    (
        field("cell ", 1),
        field("grid ", 1),
        field("scroll ", 0),
        field("cursor-top ", 0),
    )
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

/// The menu bar must be READABLE regardless of the desktop's colour
/// scheme (user bug 2026-08-02: on a light-mode desktop the bar looked
/// empty, yet clicking it opened fully readable menus).
///
/// Mechanism: FastCull hand-draws a dark UI, but the fluent MenuBar's
/// label colour follows the PLATFORM scheme — light mode makes the labels
/// 90%-alpha black over the app's hardcoded #161618, i.e. invisible. The
/// fix pins `Palette.color-scheme` to dark at the root window. This test
/// forces the scheme-resolution to the failing branch DETERMINISTICALLY
/// by pointing the session bus at a nonexistent socket: the winit backend
/// then cannot reach the xdg-desktop-portal, the scheme resolves Unknown,
/// and fluent's fallback picks the LIGHT palette — the exact failing
/// state, without touching the real desktop's setting. (This also means
/// the suite's OTHER screenshots inherited whatever scheme the ambient
/// desktop had on the day — the archived July shots contain both — which
/// is why 32 tests never caught chrome going invisible: none asserted on
/// the strip, and the input was uncontrolled. The pin makes the scheme a
/// constant; this assertion keeps anyone from removing it.)
#[test]
fn menu_bar_labels_survive_a_light_scheme_desktop() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("menu-light-scheme.jpg");
    shoot_env(
        &[raws_dir().to_str().unwrap()],
        // An unreachable bus, NOT dbus-run-session: an isolated session
        // bus auto-starts a fresh portal that re-reads the real desktop
        // setting (QE measured dark text under it — a vacuous pass).
        &[("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")],
        &out,
    );
    let bytes = std::fs::read(&out).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, _) = dec.dimensions().expect("dims");
    // Menu strip: the top 40 rows. Luma per pixel, then median.
    let mut lumas: Vec<f64> = (0..40)
        .flat_map(|y| (0..w).map(move |x| (y, x)))
        .map(|(y, x)| {
            let i = (y * w + x) * 3;
            0.299 * px[i] as f64 + 0.587 * px[i + 1] as f64 + 0.114 * px[i + 2] as f64
        })
        .collect();
    let mut sorted = lumas.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[sorted.len() / 2];
    // Anti-vacuity: the bar itself must still be the app's DARK surface.
    // If this fails, the whole window went light and the test is
    // measuring a different design, not label visibility.
    assert!(
        median < 60.0,
        "menu strip median luma {median:.1} — the bar is no longer the \
         app's dark chrome, so the label assertion below is meaningless"
    );
    // The labels: pixels far ABOVE the median are light glyphs. QE's
    // calibration: pinned build = 260-372 bright px here; the unpinned
    // build under this env = 0 (labels drawn in near-black, max luma 29).
    let bright = lumas.drain(..).filter(|l| l - median > 60.0).count();
    assert!(
        bright >= 100,
        "only {bright} bright pixels in the menu strip — the menu labels \
         are invisible against the dark bar (light-scheme palette leak)"
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

/// Issue #25 / user decision 2026-07-31: "during the loading phase,
/// whatever is currently selected stays selected, and stays visible in the
/// screen" — and it must STAY selected, not for one frame.
///
/// This INVERTS what the fixture used to assert. It was an issue #4
/// regression ("a folder must open on the capture-first image, not the
/// name-first one"), and the same two files now pin the opposite: the view
/// is filename-ordered while loading, so the cursor starts on `a_late`, and
/// the settling re-sort must LEAVE IT THERE even though `b_early` becomes
/// the head. The user was shown this exact cost — an untouched cursor that
/// started at the top ends up mid-grid — and chose it, because the frame
/// you are looking at is worth more than its position number.
///
/// The zoom steps matter: they fire background decodes AFTER the load has
/// settled. A first implementation kept the cursor only on the load-settled
/// EDGE, so the next engine event re-applied the head-follow rule and
/// snapped the photograph away — invisible to any assertion taken at the
/// flip alone (validator FAIL, 2026-07-31).
///
/// Asserted on the STATUS BAR, not on the 1:1 overlay trace. The overlay
/// line is emitted only by the sharp full-res branch, so in the debug
/// profile — which is how CI runs `cargo test --workspace`, on Windows too
/// — the 50 MP decode never lands inside the drive window and the line
/// simply does not exist. The status bar needs no decode, and it carries
/// BOTH facts this test needs in one string.
#[test]
fn engine_events_after_loading_never_move_an_untouched_cursor() {
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
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap(), "--start-11"],
        &[
            ("FASTCULL_TRACE", "1"),
            // Settle, then keep the engine busy well past the flip.
            ("FASTCULL_DRIVE", "3000:zoom-out;4000:one2one;5000:zoom-out"),
        ],
        &out,
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .unwrap_or_else(|| panic!("no status trace in stderr:\n{stderr}"))
        .to_string();
    // Anti-vacuity, both halves in one line:
    //   "2 thumbs loaded" — the load really finished, so the re-sort really
    //   happened and there was something to resist;
    //   "(2/2)"           — a_late really sorted LAST by capture time, so
    //   filename order and capture order really disagree. Were they to
    //   agree, a_late would read (1/2) and the cursor assertion below would
    //   be true for the wrong reason.
    assert!(
        status.contains("2 thumbs loaded"),
        "fixture never finished loading, so nothing re-sorted: {status}"
    );
    assert!(
        status.starts_with("a_late.ARW (2/2)"),
        "the cursor moved off the photograph it opened on — an untouched \
         cursor must survive the load-settled re-sort AND every engine event \
         after it. Expected `a_late.ARW (2/2)` (kept its image; capture time \
         put it last), got: {status}"
    );
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
            // exposed the churn). That churn is what issue #25 fixed —
            // the view now holds filename order until the load finishes —
            // but the settle-then-pin schedule stays: it makes the step
            // deterministic regardless, and `home` touches the cursor on
            // the settled view.
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
    // Issue #26: off a release tag the suffix carries the COMMIT DATE as
    // well as the hash — `X.Y.Z-devel-YYYYMMDD-<hash>`. Asserted as a shape,
    // not a literal, because both halves legitimately vary: a build from a
    // tagged commit is plain `X.Y.Z`, and a build with no git (a tarball) is
    // too. Only the devel form is constrained.
    let version = stderr
        .lines()
        .find_map(|l| l.split("about version ").nth(1))
        .expect("no about-version trace")
        .trim()
        .to_string();
    // Whether this build SHOULD carry a suffix is decided by git, not by
    // hope: CI checks out shallow with no tags, so it is always off-tag and
    // the devel form is mandatory there. Without this the suffix could
    // vanish entirely and the weaker branch below would pass green — the
    // exact regression class issue #23 introduced the suffix to prevent.
    let on_release_tag = std::process::Command::new("git")
        .args(["describe", "--tags", "--exact-match", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .is_some_and(|t| t == format!("v{}", fastcull_core::VERSION));
    match version.strip_prefix(&format!("{}-devel-", fastcull_core::VERSION)) {
        Some(suffix) => {
            // `YYYYMMDD-<hash>`, or bare `<hash>` when git could not give a
            // usable date. The dateless form is SPEC-SANCTIONED (ui-grid.md:
            // "the date is additive and never costs the hash") and really
            // happens — `log.showsignature=true` puts gpg output on stdout,
            // and git before `--date=format:` cannot produce it at all. QE
            // reproduced both; rejecting it would fail the suite on a
            // correctly-behaving build.
            match suffix.split_once('-') {
                Some((date, hash)) => {
                    assert!(
                        date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()),
                        "devel date is not YYYYMMDD: {version:?}"
                    );
                    assert!(
                        !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                        "devel hash is not hex: {version:?}"
                    );
                }
                None => assert!(
                    !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_hexdigit()),
                    "dateless devel suffix must still be a bare hex hash: {version:?}"
                ),
            }
        }
        None => {
            assert_eq!(
                version,
                fastcull_core::VERSION,
                "a build without `-devel-` must be the bare release version"
            );
            assert!(
                on_release_tag,
                "off a release tag the version MUST carry a -devel- suffix, \
                 got the bare {version:?} — the suffix has gone missing"
            );
        }
    }
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
/// Strongest glyph yellowness found by sliding the sample box over a
/// generous search area — the star badge sits at a fixed LOGICAL offset in
/// the cell, so its fractional position moves with the runner's window size
/// and DPI. A single hardcoded box found the star on the dev machine and
/// missed it entirely on the Windows runner (CI, 2026-07-31).
fn best_glyph_yellowness(
    path: &Path,
    (fx0, fy0, fx1, fy1): (f64, f64, f64, f64),
    (bw, bh): (f64, f64),
) -> Option<f64> {
    let mut best: Option<f64> = None;
    let (mut y, step) = (fy0, 0.004);
    while y + bh <= fy1 {
        let mut x = fx0;
        while x + bw <= fx1 {
            if let Some(v) = region_glyph_yellowness(path, x, y, x + bw, y + bh) {
                best = Some(best.map_or(v, |b: f64| b.max(v)));
            }
            x += step;
        }
        y += step;
    }
    best
}

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
    // The star sits at a fixed LOGICAL offset (8px/4px, 20px tall) inside
    // cell 1 — but its FRACTIONAL position depends on the window size and
    // DPI, so search the cell's top-left corner rather than one fixed box
    // (the hardcoded box missed the star entirely on the Windows runner).
    let search = (0.0, 0.06, 0.09, 0.20);
    let box_size = (0.022, 0.037);

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
    let star_plain = best_glyph_yellowness(&picked, search, box_size)
        .expect("no pick star found in the unselected frame");
    let star_washed = best_glyph_yellowness(&both, search, box_size)
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

/// Issue #34, target 1: an app-level session swap MID-FLIGHT. Open folder B
/// (one corrupt file) while folder A (six real RAWs) is still cooking in the
/// texture kitchen, via the `open:PATH` drive token — the Open Folder menu
/// action minus the native dialog, same shared code path. The kitchen's
/// generation fence is unit-verified; what was review-verified only is the
/// WIRING — `load_folder` retargeting the kitchen and restarting the
/// pipeline while work is genuinely in flight.
///
/// FASTCULL_KITCHEN_COOK_MS holds every cook for 1.5 s, so the six thumb
/// jobs span ~9 s of kitchen time and the 6.7 s swap provably lands
/// mid-queue in BOTH profiles (without it, release drains a screenful of
/// thumbs in tens of milliseconds and the test is timing roulette). 6.7 s
/// sits mid-hold, hundreds of ms from every cook boundary — in release
/// the boundaries land near 1.5/3.0/4.5/6.0/7.5 s, and a swap scheduled
/// ON a boundary races the worker's pop against the retarget (validator
/// F3). The dropped-queued count in the retarget trace is the
/// anti-vacuity guard: zero would mean there was nothing to fence and
/// every assertion below passes for the wrong reason — so zero FAILS,
/// loudly, and means the schedule needs retuning, not that the fence
/// broke.
///
/// The trailing `grid` action holds the shutter (and thus the app) open
/// past the point where the swap-orphaned work would land if it were going
/// to: the in-flight cook finishes ~1.5 s after the swap and must die at
/// the drain's generation filter, and under the no-retarget mutation the
/// leftover queue keeps cooking for ~6 s — both need the app still alive
/// to be observable at all.
#[test]
fn open_folder_mid_flight_swaps_sessions_without_stale_kitchen_work() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir_a = out_dir().join("swap-a");
    std::fs::create_dir_all(&dir_a).unwrap();
    for i in 1..=6 {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir_a.join(format!("a{i}.ARW")),
        );
    }
    let dir_b = out_dir().join("swap-b");
    std::fs::create_dir_all(&dir_b).unwrap();
    std::fs::write(dir_b.join("broken.ARW"), vec![0xAB; 2048]).unwrap();
    let out = out_dir().join("swap-mid-flight.jpg");
    let script = format!("6700:open:{};12500:grid", dir_b.display());
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_KITCHEN_COOK_MS", "1500"),
            ("FASTCULL_DRIVE", &script),
        ],
        &out,
    );
    let open_pos = stderr
        .find("drive: open:")
        .unwrap_or_else(|| panic!("the open drive never fired:\n{stderr}"));
    // The swap retargeted the kitchen (the startup load also traces a
    // retarget, with nothing to drop — only the post-open one counts) …
    let after_open = &stderr[open_pos..];
    let dropped: usize = after_open
        .lines()
        .find_map(|l| l.split("kitchen: retarget dropped ").nth(1))
        .unwrap_or_else(|| {
            panic!("no kitchen retarget on the driven open — load_folder no longer fences the kitchen:\n{stderr}")
        })
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .expect("unparseable dropped-queued count");
    // … and it did so MID-FLIGHT. Zero dropped means the queue had already
    // drained and nothing below can distinguish "fence held" from "nothing
    // to fence" — fail loudly and retune the schedule (more cook hold or a
    // later swap), the same policy as the nav-barrage test.
    assert!(
        dropped >= 1,
        "the swap did not land mid-flight ({dropped} queued jobs dropped) — \
         the no-stale-work assertions below would be vacuous:\n{stderr}"
    );
    // After the retarget: session A's queue is gone, session B (one corrupt
    // file) submits nothing, so the kitchen must never cook again. The
    // no-retarget mutation keeps the leftover queue cooking for seconds
    // past the swap and fails here. (The `cooking` trace is printed while
    // the queue lock is held, so it can never interleave AFTER the
    // retarget line unless the pop really followed the retarget.)
    let retarget_pos = open_pos + after_open.find("kitchen: retarget dropped").unwrap();
    let tail = &stderr[retarget_pos..];
    assert!(
        !tail.contains("kitchen: cooking"),
        "the kitchen kept cooking dead-session work after the swap:\n{stderr}"
    );
    // The cook in flight AT the swap finishes ~1.5 s later, into session B's
    // lifetime — its completion carries the dead generation and must die at
    // drain, never adopt. (Session B legitimately adopts nothing: its only
    // file fails to decode.) Deleting the drain's generation filter fails
    // here.
    assert!(
        !tail.contains("kitchen: adopting"),
        "a dead-session texture was adopted after the swap — the generation \
         fence did not hold at the app level:\n{stderr}"
    );
    // Coherent post-swap state: the status bar names folder B's file with
    // honest counts, and its pipeline really ran (a failed decode still
    // counts as a finished job — without this line session B never loaded
    // and the silence above proves nothing).
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .unwrap_or_else(|| panic!("no status trace in stderr:\n{stderr}"));
    assert!(
        status.starts_with("broken.ARW (1/1)"),
        "post-swap session is incoherent — expected folder B's file at \
         (1/1), got: {status}"
    );
    assert!(
        status.contains("1 thumbs loaded"),
        "folder B's pipeline never ran to completion after the swap: {status}"
    );
}

/// Issue #34, target 2: marks pending in the debounce window (700 ms) are
/// FLUSHED to sidecars by the session swap (xmp-sidecars.md: "flushed on
/// session close"). The schedule marks a1 and swaps 300 ms later — inside
/// the debounce window — so the swap CLOSES the old writer with that write
/// still pending, and the exit-time flush only drains the NEW session's
/// writer: a session close that drops pending marks instead of draining
/// them loses this one forever, which is what the file-exists assertion
/// distinguishes (mutation-verified: skipping the writer's shutdown drain
/// turns this red). Precision about what it does NOT pin: a writer merely
/// LEAKED alive at swap would still write ~700 ms later on its own
/// debounce timer, indistinguishably from the flush — the on-disk-BEFORE-
/// the-new-session-starts ordering half of the barrier has no cheap
/// black-box observable and stays covered by the core writer units. A
/// schedule that SLIPS past the debounce (Slint timers fire late under a
/// stalled loop) would be the same vacuity by another route; the
/// trace-clock guard below fails loud on it instead (validator F2).
///
/// The first `open:` targets a nonexistent path: the error branch of the
/// real Open Folder action must leave the running session intact (status
/// error, no session teardown) — proven by the pick that follows still
/// landing on folder A's first image.
#[test]
fn session_swap_flushes_pending_marks_to_sidecars() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir_a = out_dir().join("flush-a");
    std::fs::create_dir_all(&dir_a).unwrap();
    for name in ["a1.ARW", "a2.ARW"] {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir_a.join(name),
        );
    }
    // Stale sidecars from a previous run would make the flush assertion
    // vacuous (validator M1 class: a fixture picked once is picked forever).
    // The dir is fresh per run (pid-named out_dir), but be explicit anyway.
    assert!(
        !dir_a.join("a1.ARW.xmp").exists(),
        "fixture dir not clean before the run"
    );
    let dir_b = out_dir().join("flush-b-empty");
    std::fs::create_dir_all(&dir_b).unwrap();
    let out = out_dir().join("swap-flush.jpg");
    let script = format!(
        "800:open:{};1200:pick;1500:open:{}",
        dir_b.join("does-not-exist").display(),
        dir_b.display()
    );
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &script)],
        &out,
    );
    // The failed open surfaced its OWN error — matched on the catalog's
    // actual message, because a bare `fastcull: ` prefix also matches the
    // read pool's unconditional resize line and the assertion would hold
    // with the error branch deleted (validator F1, the vacuous-match trap).
    // Session A surviving the failure is proven by the sidecar below.
    assert!(
        stderr
            .lines()
            .any(|l| l.starts_with("fastcull: ") && l.contains("not a directory")),
        "the failed open never reported its error:\n{stderr}"
    );
    // The swap must have landed INSIDE the debounce window, or the writer's
    // own timer wrote the sidecar before the swap and the flush assertion
    // below is testing nothing (validator F2: Slint timers fire late under
    // a stalled loop — Windows CI has measured ~60% slower runs). Loud
    // retune signal, same policy as the mid-flight guard in the swap test.
    // LAST match for the open: the 800 ms bogus-path open also traces
    // `drive: open:` — the swap under test is the second one.
    let drive_ms = |needle: &str| -> u64 {
        stderr
            .lines()
            .rev()
            .find(|l| l.contains(needle))
            .and_then(|l| l.split('[').nth(1))
            .and_then(|r| r.split(']').next())
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("no trace clock for {needle:?}:\n{stderr}"))
    };
    let gap = drive_ms("drive: open:").saturating_sub(drive_ms("drive: pick"));
    assert!(
        gap < 700,
        "the swap fired {gap} ms after the pick — outside the 700 ms \
         debounce, so the flush assertion below would be vacuous; retune \
         the schedule:\n{stderr}"
    );
    // THE flush assertion: a1's mark, still inside the 700 ms debounce at
    // swap time, is on disk — written by the swap, since its writer no
    // longer exists to be flushed at exit.
    let sidecar = dir_a.join("a1.ARW.xmp");
    assert!(
        sidecar.exists(),
        "the pending mark was LOST by the session swap — no sidecar at {}:\n{stderr}",
        sidecar.display()
    );
    let xmp = std::fs::read_to_string(&sidecar).expect("read sidecar");
    assert!(
        xmp.contains("xmp:Rating=\"1\""),
        "sidecar exists but does not carry the picked rating:\n{xmp}"
    );
    // No spurious writes: the unmarked neighbour has no sidecar.
    assert!(
        !dir_a.join("a2.ARW.xmp").exists(),
        "an unmarked image grew a sidecar across the swap"
    );
    // And the swap itself landed: the empty folder B session reports an
    // honest empty view (issue #19's "(0/0)", never a fabricated count).
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .unwrap_or_else(|| panic!("no status trace in stderr:\n{stderr}"));
    assert!(
        status.contains("(0/0)"),
        "the swap to the empty folder never landed: {status}"
    );
}

/// Issue #34, target 3 (#25 across sessions): the provisional-order flip —
/// filename order while loading, ONE re-sort at completion, an untouched
/// cursor keeping its photograph — must re-arm for a session opened by the
/// in-app swap, not only for the process's first folder. Session A has its
/// cursor CLAIMED (a nav key) before the swap; session B must open with a
/// fresh, unclaimed cursor on ITS name-first image and hold it through B's
/// own settle re-sort. A leaked cursor index from session A (the reset in
/// `load_folder` gone missing) parks the cursor on b_early instead and
/// fails the status assertion; same two-fixture trick as
/// `engine_events_after_loading_never_move_an_untouched_cursor`, one
/// session later.
#[test]
fn provisional_order_flip_rearms_after_an_in_app_swap() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir_a = out_dir().join("rearm-a");
    std::fs::create_dir_all(&dir_a).unwrap();
    for name in ["a.ARW", "b.ARW"] {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir_a.join(name),
        );
    }
    // a_late: captured 15:29:55; b_early: 15:29:13 — name-first is
    // capture-LAST, so B's flip really moves the head (the anti-vacuity
    // both assertions below rest on, same fixtures as the #25 test).
    let dir_b = out_dir().join("rearm-b");
    std::fs::create_dir_all(&dir_b).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_uncompressed.ARW"),
        &dir_b.join("a_late.ARW"),
    );
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir_b.join("b_early.ARW"),
    );
    let out = out_dir().join("swap-rearm.jpg");
    // `right` claims session A's cursor on index 1 — both leak flavours
    // (index and claim) now point AWAY from B's expected outcome. The
    // trailing `grid` holds the shutter until B's two files have loaded
    // and re-sorted (zoom keys never claim the cursor).
    let script = format!("1000:right;2000:open:{};8500:grid", dir_b.display());
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &script)],
        &out,
    );
    // Schedule guard, stated precisely (validator F4): this proves the
    // `right` FIRED before the swap, not that it claimed the cursor —
    // the trace prints before dispatch. That `right` claims is
    // handle_nav's own claim list (its removal is the accepted residual
    // here); what THIS test pins by mutation is load_folder's cursor
    // reset, which needs the right to have fired at all.
    assert!(
        stderr.contains("drive: right"),
        "session A's `right` never fired — the cursor reset under test \
         was never armed:\n{stderr}"
    );
    let status = stderr
        .lines()
        .rev()
        .find_map(|l| l.split("status at shutter: ").nth(1))
        .unwrap_or_else(|| panic!("no status trace in stderr:\n{stderr}"));
    // Both halves in one line, exactly as the single-session test pins them:
    // "2 thumbs loaded" — B's load finished, so B's re-sort really happened;
    // "a_late.ARW (2/2)" — the cursor opened on B's name-first image and
    // kept it through the flip (capture time sorts it last).
    assert!(
        status.contains("2 thumbs loaded"),
        "session B never finished loading, so its re-sort never happened: {status}"
    );
    assert!(
        status.starts_with("a_late.ARW (2/2)"),
        "the provisional-order contract did not re-arm across the swap — \
         expected `a_late.ARW (2/2)`, got: {status}"
    );
}

// ---------------------------------------------------------------------------
// Focus continuity (issues #41/#42): when the focused editor is destroyed
// or covered, the keyboard must deterministically return to the topmost
// key scope. These tests drive REAL key and pointer events through the
// Slint focus system (`key:` / `click.` tokens) — the nav tokens bypass
// focus entirely and provably cannot see this bug class. Every red-run
// claim below was verified by running the test against the pre-fix build
// (the drive-harness commit without the fix).
// ---------------------------------------------------------------------------

/// The QEDUMP trace line for a `dump.<label>` drive action.
fn qedump<'a>(stderr: &'a str, label: &str) -> &'a str {
    let tag = format!("QEDUMP {label} ");
    stderr
        .lines()
        .find(|l| l.contains(&tag))
        .unwrap_or_else(|| panic!("no `dump.{label}` trace in stderr:\n{stderr}"))
}

/// The menu-path tests click the in-window MenuBar at fixed logical
/// coordinates (File 22, View 72, Help 115 in the bar; items on a 32 px
/// grid from y=61). Item geometry follows the platform's font metrics;
/// these coordinates are calibrated for the Linux runners (DejaVu Sans).
/// The focus machinery under test is platform-independent Slint core, and
/// every non-menu strand still runs on Windows. Each menu test asserts an
/// intermediate state that FAILS LOUDLY if a click missed its target, so
/// a font drift can never make one pass vacuously.
fn menu_clicks_are_calibrated() -> bool {
    !cfg!(windows)
}

/// Issue #41 D1, the user's live hit, at 1:1 (priority repro — RUN12):
/// with the keyword field focused (K), closing the IPTC panel via
/// View > IPTC Panel destroys the focused editor; the menu's own focus
/// restore then targets a dead element and the keyboard is stranded with
/// NO discoverable recovery at 1:1. RED pre-fix: keysfocus=false after
/// the close, and the `-` that should drop 1:1 back to fit is dead.
#[test]
fn panel_close_from_the_menu_at_one_to_one_keeps_the_keyboard() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-d1-11");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-d1-11.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap(), "--start-11"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "3500:key:k;4000:dump.k;4400:click.72,19;4800:click.128,125;\
                 5200:dump.closed;5400:key:g;5800:dump.end",
            ),
        ],
        &out,
    );
    // The K really landed in the field (anti-vacuity: panel open and the
    // keyboard NOT on the main scope — the dangerous state is armed).
    let k = qedump(&stderr, "k");
    assert!(
        k.contains("iptc=true") && k.contains("keysfocus=false") && k.contains("one2one=true"),
        "K did not open the panel and focus the keyword field at 1:1: {k}"
    );
    // The menu clicks really closed the panel (a missed click cannot pass).
    let closed = qedump(&stderr, "closed");
    assert!(
        closed.contains("iptc=false"),
        "the View > IPTC Panel click missed (panel still open): {closed}"
    );
    // THE fix: focus returned to the main key scope…
    assert!(
        closed.contains("keysfocus=true"),
        "keyboard stranded after panel close from the menu (issue #41 D1): {closed}"
    );
    // …and the next keystroke works: `G` left the loupe for the grid.
    // G, not `-`: a `-` from 1:1 legitimately lands on an intermediate
    // ladder rung once the full-res factor is resolved (release builds
    // resolve it before the key fires; debug builds may not), so its
    // outcome is profile-dependent — G exits to the grid in every state.
    let end = qedump(&stderr, "end");
    assert!(
        end.contains("one2one=false") && end.contains("zoom=1"),
        "the `G` after the panel close was dead — still stuck in the loupe: {end}"
    );
}

/// Issue #41 D1, grid variant (RUN6): same close-from-menu strand at grid
/// zoom. RED pre-fix: keysfocus=false and the `+` is dead (zoom stays 1).
#[test]
fn panel_close_from_the_menu_keeps_the_keyboard_in_the_grid() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let out = out_dir().join("focus-d1-grid.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "1600:key:k;2000:dump.k;2400:click.72,19;2800:click.128,125;\
                 3200:dump.closed;3400:key:+;3700:dump.end",
            ),
        ],
        &out,
    );
    let k = qedump(&stderr, "k");
    assert!(
        k.contains("iptc=true") && k.contains("keysfocus=false"),
        "K did not open the panel and focus the keyword field: {k}"
    );
    let closed = qedump(&stderr, "closed");
    assert!(
        closed.contains("iptc=false"),
        "the View > IPTC Panel click missed (panel still open): {closed}"
    );
    assert!(
        closed.contains("keysfocus=true"),
        "keyboard stranded after panel close from the menu (issue #41 D1): {closed}"
    );
    let end = qedump(&stderr, "end");
    assert!(
        end.contains("zoom=2"),
        "the `+` after the panel close was dead — zoom never moved: {end}"
    );
}

/// Issue #41 D2, the payload strand (RUN11): opening Help > About while a
/// field owns the keyboard used to leave the modal un-dismissable (the
/// menu's focus restore overrode the modal's keyboard steal), with every
/// keystroke landing invisibly in the field behind the scrim — and
/// committable as metadata. RED pre-fix: keysfocus=false with About up,
/// and the Esc never closes it. The metadata assertion is on DISK: the
/// blind-typed text must not become a keyword — no sidecar may exist at
/// exit, and the revert slot must never arm.
#[test]
fn modal_over_a_focused_field_owns_the_keyboard_and_writes_nothing() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-d2");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-d2.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "2500:key:k;3000:click.115,19;3400:click.180,93;3800:dump.about;\
                 4000:key:b;4100:key:a;4200:key:d;4400:key:escape;4800:dump.esc;\
                 5000:key:+;5300:dump.end",
            ),
        ],
        &out,
    );
    let about = qedump(&stderr, "about");
    assert!(
        about.contains("about=true"),
        "the Help > About click missed (dialog never opened): {about}"
    );
    // THE fix: the modal's keyboard steal survived the menu focus restore.
    assert!(
        about.contains("keysfocus=true"),
        "a hidden field still owns the keyboard behind the About scrim \
         (issue #41 D2): {about}"
    );
    // Esc closed the modal (pre-fix it was un-dismissable)…
    let esc = qedump(&stderr, "esc");
    assert!(
        esc.contains("about=false"),
        "Esc did not close About — the modal was stuck (issue #41 D2): {esc}"
    );
    // …the keyboard lives…
    let end = qedump(&stderr, "end");
    assert!(
        end.contains("zoom=2"),
        "the `+` after the modal closed was dead: {end}"
    );
    // …and NOTHING was written: the blind-typed \"bad\" never became
    // metadata. Revert never armed, and no sidecar exists on disk.
    assert!(
        end.contains("revert=\"\""),
        "a batch mutation armed the revert slot — blind typing reached \
         the metadata path: {end}"
    );
    let sidecars: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "xmp"))
        .collect();
    assert!(
        sidecars.is_empty(),
        "blind typing behind the About scrim produced a sidecar write: {sidecars:?}"
    );
}

/// Issue #41 D3 (RUN8): a session swap while a panel FIELD owns the
/// keyboard rebuilds the field rows, destroying the focused editor. RED
/// pre-fix: the keyboard is dead on the fresh session (keysfocus=false,
/// `+` inert). The mid-edit text is DISCARDED (user decision: no
/// commit-on-destroy) — asserted on disk: no sidecar in either folder.
#[test]
fn session_swap_mid_field_edit_discards_and_keeps_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir_a = out_dir().join("focus-d3-a");
    let dir_b = out_dir().join("focus-d3-b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir_a.join("one.ARW"),
    );
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir_b.join("two.ARW"),
    );
    let out = out_dir().join("focus-d3.jpg");
    // resize first: the Title field is clicked at fixed coordinates
    // inside the right-docked panel, so the window size must be pinned.
    let drive = format!(
        "150:resize:1200x800;2500:key:i;3000:click.1050,177;3400:key:w;3500:key:i;\
         3600:key:p;4000:open:{};5200:dump.swapped;5400:key:+;5800:dump.end",
        dir_b.display()
    );
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &drive)],
        &out,
    );
    // The swap really happened (anti-vacuity).
    let swapped = qedump(&stderr, "swapped");
    assert!(
        swapped.contains("two.ARW"),
        "the open: swap never landed: {swapped}"
    );
    // THE fix: the first keystroke on the fresh session is alive.
    assert!(
        swapped.contains("keysfocus=true"),
        "keyboard stranded after a session swap mid-edit (issue #41 D3): {swapped}"
    );
    let end = qedump(&stderr, "end");
    assert!(
        end.contains("zoom=2"),
        "the `+` after the swap was dead: {end}"
    );
    // Discard-on-destroy: the half-typed \"wip\" went NOWHERE.
    for dir in [&dir_a, &dir_b] {
        let sidecars: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "xmp"))
            .collect();
        assert!(
            sidecars.is_empty(),
            "a swap mid-edit committed the abandoned text (issue #41 D3 \
             discard rule): {sidecars:?}"
        );
    }
    assert!(
        end.contains("revert=\"\""),
        "a swap mid-edit armed the revert slot — the abandoned text was \
         committed somewhere: {end}"
    );
}

/// Issue #41 D3, keyword-editor variant: the keyword field SURVIVES a
/// swap (it is not a per-row conditional), so the focus steal that
/// returns the keyboard blurs a still-alive editor holding the OLD
/// session's text. The session-generation stamp must discard it — the
/// first fix cut committed \"wip\" against the NEW session's image (a
/// sidecar appeared in folder B), which is the exact cross-session write
/// this test pins. RED pre-fix: keysfocus=false after the swap.
#[test]
fn session_swap_mid_keyword_edit_never_writes_into_the_new_session() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir_a = out_dir().join("focus-d3kw-a");
    let dir_b = out_dir().join("focus-d3kw-b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir_a.join("one.ARW"),
    );
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir_b.join("two.ARW"),
    );
    let out = out_dir().join("focus-d3kw.jpg");
    let drive = format!(
        "2500:key:k;3000:key:w;3100:key:i;3200:key:p;4000:open:{};\
         5200:dump.swapped;5400:key:+;5800:dump.end",
        dir_b.display()
    );
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &drive)],
        &out,
    );
    let swapped = qedump(&stderr, "swapped");
    assert!(
        swapped.contains("two.ARW"),
        "the open: swap never landed: {swapped}"
    );
    assert!(
        swapped.contains("keysfocus=true"),
        "keyboard stranded after a swap mid-keyword-edit (issue #41 D3): {swapped}"
    );
    assert!(
        qedump(&stderr, "end").contains("zoom=2"),
        "the `+` after the swap was dead:\n{stderr}"
    );
    // The old session's half-typed keyword must not land ANYWHERE —
    // most of all not on the new session's images.
    for (dir, side) in [(&dir_a, "old"), (&dir_b, "NEW")] {
        let sidecars: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "xmp"))
            .collect();
        assert!(
            sidecars.is_empty(),
            "the abandoned keyword text was committed into the {side} \
             session (issue #41 D3 discard rule): {sidecars:?}"
        );
    }
}

/// Issue #42: Esc over stacked modals must close the TOPMOST one. With
/// About opened over the live Copy Picks dialog, the first Esc used to
/// act on the HIDDEN dialog — discarding its plan state while About
/// stayed up. RED pre-fix: after the first Esc, copy=false + about=true.
/// Post-fix: About closes first, the dialog and its plan survive
/// untouched, the second Esc closes the dialog, and marks stay contained
/// throughout (the driven N never rejects). Uses the `about` toggle (the
/// shipped modal-open path) rather than menu clicks, so it runs on both
/// platforms; the menu-restore machinery has its own tests above.
#[test]
fn esc_over_stacked_modals_closes_the_topmost_first() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("esc-topmost.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "1600:key:y;2000:key:ctrl+e;2400:dump.opened;2700:about;\
                 3100:key:n;3400:key:escape;3800:dump.esc1;4000:key:escape;\
                 4400:dump.esc2;4600:key:+;4900:dump.end",
            ),
        ],
        &out,
    );
    // The dialog opened with a real Ctrl+E and owns the keyboard.
    let opened = qedump(&stderr, "opened");
    assert!(
        opened.contains("copy=true") && opened.contains("keysfocus=false"),
        "Ctrl+E did not open the copy dialog with its own key scope: {opened}"
    );
    // The plan summary as it stood when the dialog opened ("N picked
    // images…"), to be compared verbatim after the first Esc.
    fn summary_of(dump: &str) -> &str {
        dump.split(" summary=")
            .nth(1)
            .and_then(|s| s.split(" template=").next())
            .expect("no summary field in dump")
    }
    let plan = summary_of(opened).to_string();
    assert!(
        plan.contains("picked"),
        "the dialog opened without a plan summary: {opened}"
    );
    // First Esc: About (topmost) closes, the dialog SURVIVES with its
    // plan intact, and the N pressed while both were up marked nothing.
    let esc1 = qedump(&stderr, "esc1");
    assert!(
        esc1.contains("about=false"),
        "the first Esc did not close About: {esc1}"
    );
    assert!(
        esc1.contains("copy=true"),
        "the first Esc closed the HIDDEN copy dialog under About \
         (issue #42): {esc1}"
    );
    assert_eq!(
        summary_of(esc1),
        plan,
        "the copy dialog's plan state did not survive the first Esc: {esc1}"
    );
    assert!(
        esc1.contains("★1 ✕0"),
        "a driven N leaked through the stacked modals and marked a photo: {esc1}"
    );
    // Second Esc: the dialog itself closes and the keyboard returns.
    let esc2 = qedump(&stderr, "esc2");
    assert!(
        esc2.contains("copy=false") && esc2.contains("keysfocus=true"),
        "the second Esc did not close the copy dialog and restore the \
         keyboard: {esc2}"
    );
    assert!(
        qedump(&stderr, "end").contains("zoom=2"),
        "the `+` after the dialogs closed was dead:\n{stderr}"
    );
}

/// Issue #41 defense in depth: at 1:1 the zoomed-loupe click surface now
/// claims the keyboard exactly like the grid-cell and fit surfaces — it
/// was the ONE click surface that did not, which is why the stranded
/// keyboard had no discoverable recovery at 1:1. RED pre-fix:
/// keysfocus=false after the click. Additive: the click still re-centers
/// (user decision — click semantics unchanged).
#[test]
fn one_to_one_click_claims_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-loupeclick");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-loupeclick.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap(), "--start-11"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "3500:key:k;4000:dump.k;4400:click.400,400;4800:dump.clicked;\
                 5000:key:g;5400:dump.end",
            ),
        ],
        &out,
    );
    // K parked the keyboard in the keyword field (the stranded-adjacent
    // state), all at 1:1.
    let k = qedump(&stderr, "k");
    assert!(
        k.contains("keysfocus=false") && k.contains("one2one=true") && k.contains("iptc=true"),
        "K did not focus the keyword field at 1:1: {k}"
    );
    // The click on the zoomed image claimed the keyboard back…
    let clicked = qedump(&stderr, "clicked");
    assert!(
        clicked.contains("keysfocus=true") && clicked.contains("one2one=true"),
        "a 1:1 loupe click did not claim the keyboard (issue #41 defense \
         in depth): {clicked}"
    );
    // …and the next keystroke works: `G` exits to the grid (G, not `-`,
    // for the profile-independence reason in the panel-close 1:1 test).
    let end = qedump(&stderr, "end");
    assert!(
        end.contains("one2one=false") && end.contains("zoom=1"),
        "the `G` after the loupe click was dead: {end}"
    );
}

/// Clean-path guard (RUN4/5): menu activation with the main key scope
/// focused must keep working exactly as before the focus-continuity fix
/// — the menu's own restore hands the keyboard back and the action fires.
#[test]
fn menu_activation_with_keys_focused_stays_clean() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let out = out_dir().join("focus-menu-clean.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "1600:click.72,19;2000:click.128,61;2400:dump.zoomed;\
                 2600:key:+;2900:dump.end",
            ),
        ],
        &out,
    );
    let zoomed = qedump(&stderr, "zoomed");
    assert!(
        zoomed.contains("zoom=2"),
        "View > Zoom In via the menu did not fire (missed click?): {zoomed}"
    );
    assert!(
        zoomed.contains("keysfocus=true"),
        "menu activation stole the keyboard from the main scope: {zoomed}"
    );
    assert!(
        qedump(&stderr, "end").contains("zoom=3"),
        "the `+` after the menu action was dead:\n{stderr}"
    );
}

/// Clean-path guard (G4 / RUN16a): K → type → Enter still commits the
/// keyword, arms revert, writes the sidecar, and returns the keyboard to
/// the grid. This also pins the edit-generation stamping: the first fix
/// cut silently DISCARDED text whose editor was focused via the panel's
/// init path (the `changed has-focus` callback does not fire for an
/// init-time gain), which turned this commit into a no-op.
#[test]
fn keyword_enter_commit_still_writes_and_returns_focus() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-g4");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-g4.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "2500:key:k;3000:key:o;3100:key:k;3300:key:return;\
                 3700:dump.committed;3900:key:+;4200:dump.end",
            ),
        ],
        &out,
    );
    let committed = qedump(&stderr, "committed");
    assert!(
        committed.contains("keysfocus=true"),
        "Enter did not return the keyboard to the grid (G4): {committed}"
    );
    assert!(
        committed.contains("revert=\"Revert: keywords on 1 image(s)\""),
        "the keyword commit never armed the revert slot — the typed text \
         was lost (G4): {committed}"
    );
    assert!(
        qedump(&stderr, "end").contains("zoom=2"),
        "the `+` after the commit was dead:\n{stderr}"
    );
    let sidecar = dir.join("one.ARW.xmp");
    let xmp = std::fs::read_to_string(&sidecar)
        .unwrap_or_else(|e| panic!("no sidecar written for the committed keyword: {e}"));
    assert!(
        xmp.contains(">ok<"),
        "the sidecar does not contain the committed keyword: {xmp}"
    );
}

/// Clean-path guard (RUN9): the copy dialog lifecycle — a real Ctrl+E
/// opens it with its own key scope, a real Esc closes it and the
/// keyboard returns to the grid.
#[test]
fn copy_dialog_esc_returns_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("focus-copy-esc.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "1600:key:ctrl+e;2000:dump.opened;2200:key:escape;\
                 2600:dump.closed;2800:key:+;3100:dump.end",
            ),
        ],
        &out,
    );
    let opened = qedump(&stderr, "opened");
    assert!(
        opened.contains("copy=true") && opened.contains("keysfocus=false"),
        "Ctrl+E did not open the copy dialog with its own key scope: {opened}"
    );
    let closed = qedump(&stderr, "closed");
    assert!(
        closed.contains("copy=false") && closed.contains("keysfocus=true"),
        "Esc did not close the dialog and hand the keyboard back: {closed}"
    );
    assert!(
        qedump(&stderr, "end").contains("zoom=2"),
        "the `+` after the dialog closed was dead:\n{stderr}"
    );
}

/// Clean-path guard (RUN17): toggling the filter bar from the menu while
/// the keyword field holds half-typed text. Opening the menu is a G7
/// click-away exit — the text commits (revert arms) — and the menu's
/// restore puts the keyboard back in the field; Enter then no-ops and
/// returns to the grid. This is the guard that catches a discard rule
/// grown too greedy (the fix must never eat a same-session commit).
#[test]
fn filter_bar_toggle_mid_edit_commits_and_keeps_the_field_coherent() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-run17");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-run17.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "2500:key:k;3000:key:w;3300:click.72,19;3700:click.128,157;\
                 4100:dump.toggled;4300:key:return;4700:dump.after;\
                 4900:key:+;5200:dump.end",
            ),
        ],
        &out,
    );
    let toggled = qedump(&stderr, "toggled");
    // The half-typed keyword committed on the menu-open exit (G7), so
    // the revert slot is armed — NOT discarded, NOT lost.
    assert!(
        toggled.contains("revert=\"Revert: keywords on 1 image(s)\""),
        "the mid-edit keyword was lost instead of committing on the \
         menu-open exit (G7): {toggled}"
    );
    // The menu restore put the keyboard back in the still-alive field.
    assert!(
        toggled.contains("keysfocus=false"),
        "the field lost the keyboard across the filter-bar toggle: {toggled}"
    );
    assert!(
        qedump(&stderr, "after").contains("keysfocus=true"),
        "Enter did not return the keyboard to the grid:\n{stderr}"
    );
    assert!(
        qedump(&stderr, "end").contains("zoom=2"),
        "the `+` after the toggle was dead:\n{stderr}"
    );
}
