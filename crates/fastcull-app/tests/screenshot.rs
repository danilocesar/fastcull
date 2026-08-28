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
/// phantom "drag" into pan_center. Since issue #46 this holds
/// STRUCTURALLY — `capture_pan` (whose "pan fold" trace this greps) is
/// deleted and pan mutations come only from the explicit drag event —
/// so the test now guards against any future read-back reintroducing
/// the trace, alongside the clean-exit smoke. LIMITATION, recorded in
/// issue #6: the visible 0x0-frame symptom needs the GPU renderer +
/// real key repeat and is NOT reproducible under the software renderer
/// — the structural fixes plus this misfold guard are what CAN be
/// checked headlessly; the visual check stays manual.
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
        "150:resize:1200x800;2500:key:i;3000:click.1050,177;3200:dump.focused;\
         3400:key:w;3500:key:i;3600:key:p;4000:open:{};5200:dump.swapped;\
         5400:key:+;5800:dump.end",
        dir_b.display()
    );
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &drive)],
        &out,
    );
    // The Title-field click really took focus (anti-vacuity, gate
    // finding: a missed click would type the `p` as a real grid PICK
    // and fail this test later with a false "committed the abandoned
    // text" diagnosis — a miss must be loud and unambiguous).
    let focused = qedump(&stderr, "focused");
    assert!(
        focused.contains("keysfocus=false"),
        "the Title-field click missed — the field never took focus: {focused}"
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
///
/// KNOWN INTERMITTENT FAILURE (measured 2026-08-22, ~1 run in 4, on an
/// idle machine): the leaked sidecar contains the abandoned `wip`
/// keyword, in the OLD session's folder — so this is a real race in the
/// discard rule, not test noise. It reproduces on `c060e7c` (before the
/// clash-question work): interleaved runs of 6 gave 2/6 failures there
/// and 1/6 on the tree that followed. Do NOT quiet this test; when it
/// fails it is telling the truth. Recorded in ui-grid.md.
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

/// Gate finding on the fix's first cut: File > Copy Picks opened from
/// the menu while a panel field owns the keyboard (QE RUN14) was still
/// held together by init-timing luck — the dialog's own init claim
/// happening to run after the menu's focus restore, with no deferred
/// claim behind it. This is a GUARD, green before and after the ordering
/// hardened (the luck holds today): the dialog's scope must own the
/// keyboard, blind typing must not reach the hidden field or the
/// metadata path, and Esc must close the dialog and hand the keys back.
#[test]
fn copy_picks_from_the_menu_over_a_focused_field_owns_the_keyboard() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-run14");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-run14.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "2500:key:k;3000:click.22,19;3400:click.80,93;3800:dump.opened;\
                 4000:key:x;4300:key:escape;4700:dump.closed;4900:key:+;\
                 5200:dump.end",
            ),
        ],
        &out,
    );
    // The dialog opened via the real menu and its scope owns the keys.
    let opened = qedump(&stderr, "opened");
    assert!(
        opened.contains("copy=true"),
        "the File > Copy Picks click missed (dialog never opened): {opened}"
    );
    assert!(
        opened.contains("keysfocus=false"),
        "the main key scope holds the keys behind the copy dialog — N/Y \
         would fire at the hidden grid: {opened}"
    );
    // Esc closed the dialog and the keyboard returned…
    let closed = qedump(&stderr, "closed");
    assert!(
        closed.contains("copy=false") && closed.contains("keysfocus=true"),
        "Esc did not close the dialog and restore the keyboard: {closed}"
    );
    // …the blind `x` never became metadata…
    assert!(
        closed.contains("revert=\"\""),
        "blind typing behind the copy dialog reached the metadata path: {closed}"
    );
    let sidecars: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "xmp"))
        .collect();
    assert!(
        sidecars.is_empty(),
        "blind typing behind the copy dialog produced a sidecar: {sidecars:?}"
    );
    // …and the next keystroke works.
    assert!(
        qedump(&stderr, "end").contains("zoom=2"),
        "the `+` after the dialog closed was dead:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Issue #46: transit fit-flash (M1) and fling-survives-navigation (M3).
// These drive real pointer sequences through the promoted press./move./
// release. tokens and assert on dump./trace state — pixel assertions are
// useless here (a far-panned 1:1 snapshots black under the software
// renderer, and a wrong-position frame is a state nothing re-renders).
// Red-run claims are per-test; the red runs execute against the pre-fix
// build (6d15ed1 + the drive-harness commit) in RELEASE mode, where the
// reproduction was proven 5/5 and 3/3 deterministic.
// ---------------------------------------------------------------------------

/// A field=value token out of a QEDUMP line (fields never contain spaces
/// except the quoted status/summary strings, which these fields precede
/// or follow as whole tokens).
fn dump_field<'a>(line: &'a str, field: &str) -> &'a str {
    let tag = format!("{field}=");
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(&tag))
        .unwrap_or_else(|| panic!("no {field}= in dump line: {line}"))
}

/// A Debug-quoted dump field (` name="…"`): the text between the quotes,
/// up to the first unescaped closing quote. Used for the fields that
/// contain spaces (summary, copynote, report, confirm).
fn dump_text<'a>(dump: &'a str, name: &str) -> &'a str {
    let tag = format!(" {name}=\"");
    let rest = dump
        .split(&tag)
        .nth(1)
        .unwrap_or_else(|| panic!("no {name} field in dump: {dump}"));
    let mut prev = b' ';
    let end = rest
        .bytes()
        .position(|b| {
            let close = b == b'"' && prev != b'\\';
            prev = b;
            close
        })
        .unwrap_or(rest.len());
    &rest[..end]
}

/// The millisecond stamp a `FASTCULL_TRACE` line carries, as in
/// `fastcull-trace: [51] loupe thumb idx 0 ...` — the app's own clock, so
/// a test can tell a startup event from one inside a drive script's
/// window. `None` for any line without a well-formed stamp; callers decide
/// whether that is disqualifying (an unstamped line is never "early").
fn trace_ms(line: &str) -> Option<u64> {
    line.split_once('[')?.1.split_once(']')?.0.parse().ok()
}

/// Ten files cycling the three A1 classes: identical per-class EXIF
/// capture times make the capture sort interleave VIEW order against
/// image-id order — the issue #46 M1 shape, where the pre-fix id-space
/// prefetch ring left every arrow neighbor cold, deterministically.
fn interleaved_session(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let classes = [
        "A1_full_compressed.ARW",
        "A1_full_lossless_compressed.ARW",
        "A1_full_uncompressed.ARW",
    ];
    for i in 0..10 {
        place_fixture(
            &raws_dir().join(classes[i % 3]),
            &dir.join(format!("IMG_{i:04}.ARW")),
        );
    }
}

/// Issue #46 M1 + the persona's jump-navigation condition: at deep 1:1,
/// landing on a stone-cold image (End — far outside ANY prefetch ring)
/// must keep the overlay up at the carried factor and pan centre —
/// never an EXCUSE-LESS drop to fit. The target's thumb was never
/// visible so it is not even in `st.textures.images` yet: the overlay
/// HOLDs the previous pixels (that is where the +80 ms dump lands — the
/// hold engages synchronously with the End refresh and `OVERLAY_HOLD_CAP`
/// cannot fire before 250 ms), then the freshly prepped thumb renders
/// (~150–300 ms behind the cook hold in release; later in debug, where
/// the kitchen queue is congested by 149 MB debug-profile fills and the
/// hold cap may legitimately fire first — the spec'd bounded drop,
/// which must RE-RAISE the moment the thumb lands; the far "landed"
/// dump covers both timelines).
///
/// RED on pre-fix code (+ the drive-harness commit): `one2one=false` at
/// the mid-gap dump (the overlay dropped and the strip showed the whole
/// frame at fit), a "loupe overlay dropped … (no rung in hand)" trace —
/// the excuse-less drop, which post-fix is structurally impossible —
/// and neither a "loupe hold" nor a "loupe thumb" render anywhere.
///
/// RELEASE ONLY (validator, gate round 2): in debug the run rides the
/// app's own 60 s screenshot-readiness cap — the cursor's 50 MP debug
/// decode plus ten thumb jobs plus the cook hold landed at 58.5 s on a
/// loaded 8-core laptop, so under contention (or a 2-vCPU CI runner)
/// the app exits 1 at the cap before the shutter can fire. The debug
/// profile keeps its no-drop coverage through paced_taps and
/// transit_at_zoom_stays_soft; the phase pins here bind in release,
/// the profile the reproduction and the red-run were proven in (the
/// perf_budgets precedent).
#[test]
fn transit_to_a_cold_frame_keeps_the_overlay_at_the_carried_center() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    if cfg!(debug_assertions) {
        eprintln!("skipped: debug build rides the 60 s readiness cap (run with --release)");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("i46-m1");
    interleaved_session(&dir);
    let out = out_dir().join("i46-m1.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_KITCHEN_COOK_MS", "150"),
            (
                "FASTCULL_DRIVE",
                "20000:dump.pre;20050:end;20130:dump.midgap;26500:dump.landed",
            ),
        ],
        &out,
    );
    let pre = qedump(&stderr, "pre");
    let midgap = qedump(&stderr, "midgap");
    let landed = qedump(&stderr, "landed");
    assert_eq!(
        dump_field(pre, "one2one"),
        "true",
        "overlay must be up before the jump (soft or sharp):\n{pre}"
    );
    // THE bug: pre-fix the overlay dropped here and the strip rendered
    // the whole next frame at fit.
    assert_eq!(
        dump_field(midgap, "one2one"),
        "true",
        "the overlay dropped to fit on a cold jump — the M1 fit-flash:\n{stderr}"
    );
    assert_eq!(
        dump_field(midgap, "soft"),
        "true",
        "a no-rung window must be flagged by the cue pill:\n{midgap}"
    );
    assert_eq!(
        dump_field(midgap, "pan"),
        "0.5000,0.5000",
        "the carried pan centre was disturbed by the cold jump:\n{midgap}"
    );
    assert!(
        stderr.contains("loupe hold idx"),
        "the residual hold never engaged — where did the mid-gap pixels come from?\n{stderr}"
    );
    // The thumb-rung render is deterministic in RELEASE (the cook hold
    // sequences thumb ahead of mid, with a refresh between the two
    // kitchen completions). A congested debug kitchen can adopt both in
    // one drain, where rendering the better rung directly is correct —
    // so this pin binds in release only (the perf_budgets precedent);
    // the hold, no-excuse-less-drop and one2one pins bind everywhere.
    if !cfg!(debug_assertions) {
        assert!(
            stderr.contains("loupe thumb idx"),
            "the thumb rung never rendered once the thumb was prepped:\n{stderr}"
        );
    } else {
        eprintln!("thumb-rung pin skipped: debug build (run with --release)");
    }
    // The EXCUSE-LESS drop is the bug and must be impossible. The spec'd
    // bounded drops (decode failure; hold cap under a congested debug
    // kitchen) carry their reason and re-raise — the landed dump below
    // proves the recovery.
    assert!(
        !stderr.contains("(no rung in hand)"),
        "the overlay dropped with no excuse during transit:\n{stderr}"
    );
    // Geometry continuity, checkable when the factor had RESOLVED before
    // the jump (a release-profile run; debug decodes may still be at the
    // virgin pin at the pre dump, where extents legitimately differ):
    // same aspect, same carried factor => identical offsets.
    if dump_field(pre, "soft") == "false" {
        let vx = |l: &str| dump_field(l, "vx").parse::<f32>().unwrap();
        assert!(
            (vx(pre) - vx(midgap)).abs() <= 1.5,
            "carried offset moved across the thumb render: pre {} vs midgap {}",
            vx(pre),
            vx(midgap)
        );
    }
    assert_eq!(
        dump_field(landed, "one2one"),
        "true",
        "the overlay must still be up after the landing:\n{landed}"
    );
}

/// Issue #46 M3 (and the F3/F4 contracts): loupe drag-pan is 1:1 with
/// the pointer and STOPS on release — no fling physics exists to survive
/// into a navigation, and the pan centre is folded only by the real drag
/// itself (the #16/#22 positive-signal doctrine).
///
/// One app run, three phases at a resolved 1:1 (the 45 s lead time is
/// what a debug-profile full-res decode needs; the `predrag` guard fails
/// loudly rather than letting a slow run pass vacuously):
///  1. slow drag — pans 1:1 (the guard half, green on both sides);
///  2. flick — five fast moves and release: offsets must be IDENTICAL
///     at +100 ms and +400 ms after release (pre-fix: the Flickable's
///     deceleration binding was still animating them);
///  3. arrow during where the decay would be — the next image must keep
///     the drag-carried pan centre (pre-fix: phantom `pan fold`s degraded
///     it toward the corner and the view parked at 0,0).
///
/// RED on pre-fix code (+ the drive-harness commit, release build):
/// `pan fold` traces present, offsets drift between the two post-release
/// dumps, and the post-navigation pan centre no longer matches the
/// post-drag one.
#[test]
fn loupe_drag_pans_one_to_one_and_a_fling_never_survives_navigation() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("i46-m3");
    std::fs::create_dir_all(&dir).unwrap();
    for (src, dst) in [
        ("A1_full_compressed.ARW", "a.ARW"),
        ("A1_full_lossless_compressed.ARW", "b.ARW"),
        ("A1_full_uncompressed.ARW", "c.ARW"),
    ] {
        place_fixture(&raws_dir().join(src), &dir.join(dst));
    }
    let out = out_dir().join("i46-m3.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                // Phase 1: slow drag right+down by (100, 40). Phase 2:
                // the flick (5 events, 16 ms apart — the velocity ring
                // buffer needs real timing). Phase 3: arrow mid-"decay".
                "45000:dump.predrag;45050:press.700,450;45150:move.750,470;45250:move.800,490;\
                 45350:release.800,490;45450:dump.dragged;\
                 45600:press.700,450;45616:move.800,520;45632:move.900,590;45648:move.1000,660;\
                 45664:move.1100,730;45680:release.1100,730;\
                 45780:dump.afterfling1;46080:dump.afterfling2;\
                 46200:right;46300:dump.afternav;47100:dump.late",
            ),
        ],
        &out,
    );
    let predrag = qedump(&stderr, "predrag");
    let dragged = qedump(&stderr, "dragged");
    let fling1 = qedump(&stderr, "afterfling1");
    let fling2 = qedump(&stderr, "afterfling2");
    let afternav = qedump(&stderr, "afternav");
    let late = qedump(&stderr, "late");
    // Guard: the 1:1 must be RESOLVED before the pointer work, or the
    // extents are fit-sized and nothing can pan — a vacuous pass.
    assert_eq!(
        dump_field(predrag, "one2one"),
        "true",
        "overlay not up before the drag:\n{predrag}"
    );
    assert_eq!(
        dump_field(predrag, "soft"),
        "false",
        "full-res not resolved 45 s in — the drag would have no pan range \
         and every assertion below would be vacuous:\n{predrag}"
    );
    let vx = |l: &str| dump_field(l, "vx").parse::<f32>().unwrap();
    let vy = |l: &str| dump_field(l, "vy").parse::<f32>().unwrap();
    let pan = |l: &str| {
        let (x, y) = dump_field(l, "pan").split_once(',').expect("pan pair");
        (x.parse::<f32>().unwrap(), y.parse::<f32>().unwrap())
    };
    // Phase 1 — the drag contract: 1:1 with pointer motion (±12 px
    // absorbs the drag threshold), folded into the carried centre.
    assert!(
        (vx(dragged) - vx(predrag) - 100.0).abs() <= 12.0
            && (vy(dragged) - vy(predrag) - 40.0).abs() <= 12.0,
        "drag is not 1:1 with the pointer: {} -> {} / {} -> {}\n{stderr}",
        vx(predrag),
        vx(dragged),
        vy(predrag),
        vy(dragged)
    );
    assert!(
        pan(dragged).0 < pan(predrag).0 - 0.005,
        "the drag never folded into the pan centre: {:?} -> {:?}",
        pan(predrag),
        pan(dragged)
    );
    // Phase 2 — release stops the image dead: identical offsets 100 ms
    // and 400 ms after release. Pre-fix the deceleration binding was
    // still animating them here.
    assert!(
        (vx(fling1) - vx(fling2)).abs() < 0.5 && (vy(fling1) - vy(fling2)).abs() < 0.5,
        "offsets still moving after release — fling physics installed: \
         +100ms {},{} vs +400ms {},{}\n{stderr}",
        vx(fling1),
        vy(fling1),
        vx(fling2),
        vy(fling2)
    );
    // Phase 3 — nothing survives into navigation: the carried centre is
    // exactly where the (real) flick-drag left it, on the next image and
    // 900 ms later. Pre-fix, phantom folds ground it toward the corner
    // and the view parked at offset 0,0.
    assert!(
        !stderr.contains("pan fold"),
        "a pan fold was inferred — displacement-derived drags are back:\n{stderr}"
    );
    let (fx, fy) = pan(fling2);
    for (name, line) in [("afternav", afternav), ("late", late)] {
        let (px, py) = pan(line);
        assert!(
            (px - fx).abs() < 0.003 && (py - fy).abs() < 0.003,
            "{name}: carried pan centre corrupted after navigation: \
             {fx:.4},{fy:.4} -> {px:.4},{py:.4}\n{stderr}"
        );
    }
    assert_eq!(
        dump_field(late, "one2one"),
        "true",
        "overlay lost after the navigation:\n{late}"
    );
}

/// Issue #46 F2: the loupe prefetch ring walks VIEW order, so paced taps
/// over a capture-sorted session with interleaved ids land on WARM
/// frames. Pre-fix (+ the drive-harness commit), the id-space ring left
/// every arrow neighbor cold and this exact five-tap script dropped the
/// overlay five out of five times.
#[test]
fn paced_taps_over_an_interleaved_session_land_warm() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("i46-f2");
    interleaved_session(&dir);
    let out = out_dir().join("i46-f2.jpg");
    // The first tap, shared with the warm-landing assertion below so the
    // script and the window it is judged over cannot drift apart.
    const FIRST_TAP_MS: u64 = 8000;
    let drive = format!(
        "{FIRST_TAP_MS}:right;8600:right;9200:right;9800:right;10400:right;11000:dump.done"
    );
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &drive)],
        &out,
    );
    let fired = stderr.lines().filter(|l| l.contains("drive: ")).count();
    assert!(
        fired >= 5,
        "tap script never ran ({fired} drive marks):\n{stderr}"
    );
    assert!(
        !stderr.contains("loupe overlay dropped"),
        "a paced tap still hit a cold frame and dropped the overlay:\n{stderr}"
    );
    // F2 specifically, not F1 masking it: a warm landing renders from the
    // mid or better — the thumb rung is the cold-path rescue and must not
    // be needed at a 600 ms cadence with a view-order ring. RELEASE
    // profile only (the perf_budgets precedent): a debug build decodes a
    // mid slower than the tap cadence, so the thumb rescue legitimately
    // fires there — the no-drop and one2one assertions above still bind.
    //
    // Scoped to the TAP WINDOW, which is what the message claims. The
    // cold start is not a paced tap: at t=0 nothing is decoded yet, so
    // the very first frame legitimately renders through the thumb rescue
    // (that IS issue #46's cold path) before the mid lands milliseconds
    // later. A whole-session `contains` also caught that startup render,
    // so on a loaded runner — where the mid loses the opening race — the
    // test failed while every one of the five taps had landed at the top
    // rung. Observed on CI 2026-08-11 (run 31455826044): the only thumb
    // was `[51] loupe thumb idx 0`, superseded by `[76] loupe soft idx 0`,
    // with all five taps at 8000-10400 ms rendering the full 8640x5760.
    // Anything at or after the first tap still fails, which is the
    // regression this pins.
    if !cfg!(debug_assertions) {
        let late_thumb = stderr
            .lines()
            .filter(|l| l.contains("loupe thumb idx"))
            .find(|l| trace_ms(l).is_none_or(|ms| ms >= FIRST_TAP_MS));
        assert!(
            late_thumb.is_none(),
            "a paced tap fell to the THUMB rung — the ring is not warming \
             the view neighbors:\n{}\n--- full trace ---\n{stderr}",
            late_thumb.unwrap_or_default()
        );
    } else {
        eprintln!("warm-landing pin skipped: debug build (run with --release)");
    }
    assert_eq!(
        dump_field(qedump(&stderr, "done"), "one2one"),
        "true",
        "overlay down after the taps:\n{stderr}"
    );
}

/// Issue #46 gate gap (QE): the overlay's wheel wiring — the fit
/// surface's and the overlay TouchArea's separate notch accumulators
/// and the post-Flickable coordinate terms — was reachable by no test
/// and no Wayland automation. Driven here with real dispatched scroll
/// events (`wheel.` token): one notch at fit enters the ladder, one
/// notch over the risen overlay climbs it, and two half-notches
/// accumulate into exactly one stop. A guard (green on both sides of
/// the #46 fix — wheel SEMANTICS did not change, only its wiring):
/// non-vacuous because a dead scroll path leaves zf at 1.0.
#[test]
fn overlay_wheel_still_zooms_one_stop_per_notch() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("i46-wheel");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("i46-wheel.jpg");
    let stderr = shoot_env_stderr(
        &["--start-loupe", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "8000:wheel.700,450,60;9500:dump.w1;10000:wheel.700,450,60;10500:dump.w2;\
                 11000:wheel.700,450,30;11200:wheel.700,450,30;11700:dump.w3",
            ),
        ],
        &out,
    );
    assert_eq!(
        dump_field(qedump(&stderr, "w1"), "zf"),
        "1.500",
        "a wheel notch at fit did not enter the zoom ladder:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "w2"), "zf"),
        "2.250",
        "a wheel notch over the zoom overlay did not climb one stop:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "w3"), "zf"),
        "3.375",
        "two half-notches did not accumulate into exactly one stop:\n{stderr}"
    );
}

/// Issue #46 gate round 2 (validator MEDIUM): a decode-FAILED cursor
/// must skip the thumb rescue — pre-gate, a corrupt image whose thumb
/// texture was already in memory rendered at 1:1 behind a "loading"
/// pill that could never complete, hiding the strip's failed badge.
///
/// The shape is UNREACHABLE as a static file (the grid thumb and the
/// loupe's first rung decode the same grid_source() bytes — they live
/// or die together; QE, gate round 2), so this test manufactures the
/// field route: the file dies on disk AFTER its thumb was decoded. A
/// helper thread zeroes the copy from byte 200,000 to EOF at T+9 s —
/// after every thumb has landed (~2 s in release), before the first
/// End-jump focuses the file.
///
/// Sequence: the FIRST End renders the thumb once (the failure is not
/// knowable until the decode attempt fails milliseconds later — the
/// causally unavoidable transient, which also proves non-vacuously
/// that the masking shape was armed), then drops with the
/// "(decode failed)" excuse; the SECOND End, failure known, drops in
/// the same tick with NO thumb render. The script ends on a healthy
/// cursor because a --start-11 shutter whose final cursor is failed
/// above fit trips the 60 s readiness cap (recorded limitation).
///
/// RED on the pre-gate build (b2ce1f9): the thumb renders on EVERY
/// End (count >= 2) and the "(decode failed)" drop never appears.
/// RELEASE ONLY: debug rides the 60 s readiness cap (see the M1 test).
#[test]
fn a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    if cfg!(debug_assertions) {
        eprintln!("skipped: debug build rides the 60 s readiness cap (run with --release)");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("i46-failgate");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..11 {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &dir.join(format!("IMG_{i:04}.ARW")),
        );
    }
    // A real COPY, never a symlink: the corrupter must not touch the
    // shared fixture RAW.
    let corrupt = dir.join("zz_corrupt.ARW");
    std::fs::copy(raws_dir().join("A1_full_compressed.ARW"), &corrupt).unwrap();
    let corrupter = {
        let path = corrupt.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(9));
            use std::io::{Seek, Write};
            let len = std::fs::metadata(&path).unwrap().len();
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(std::io::SeekFrom::Start(200_000)).unwrap();
            f.write_all(&vec![0u8; (len - 200_000) as usize]).unwrap();
        })
    };
    let out = out_dir().join("i46-failgate.jpg");
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "15000:end;15250:dump.t1;16000:home;17000:end;17150:dump.t2;18000:home",
            ),
        ],
        &out,
    );
    corrupter.join().unwrap();
    let thumb_renders = stderr.matches("loupe thumb idx 11 ").count();
    // FLAKE (issue #50): this precondition races. Arming the masking shape
    // needs the thumb render to win a same-tick contest against the decode
    // failure; under full parallel load it loses ~15% of the time — measured
    // at that rate on unmodified main too, so it is this test's design, not
    // product drift. The guard stays (without it the test passes vacuously);
    // #50 tracks making the ordering deterministic.
    assert!(
        thumb_renders >= 1,
        "the corrupt image's thumb never rendered at all — the masking \
         shape was never armed and this test proves nothing:\n{stderr}"
    );
    assert!(
        stderr.contains("loupe overlay dropped idx 11 (decode failed)"),
        "a failed cursor never dropped to fit — the thumb rescue is \
         masking the failed badge again:\n{stderr}"
    );
    assert_eq!(
        thumb_renders, 1,
        "the thumb rendered again on a KNOWN-failed cursor (the second \
         End) — the gate is gone:\n{stderr}"
    );
    for label in ["t1", "t2"] {
        assert_eq!(
            dump_field(qedump(&stderr, label), "one2one"),
            "false",
            "the overlay is still up on the failed cursor at {label} — \
             the fit strip (and its failed badge) is hidden:\n{stderr}"
        );
    }
}

/// The 2026-08-21 Copy Picks re-run bug, end to end (fileops.md): copy
/// two picks, delete the landed pairs BY HAND while the app is live,
/// Ctrl+E again into the same folder. RED pre-fix: the dialog said "0 B
/// to copy · 2 sidecars will be refreshed", the report "Nothing needed
/// copying", and the folder ended up with XMPs only. Now: the destination
/// is empty, so nothing clashes, no question is asked, the amber note
/// names the gone copies, the report says "2 copied, all checksums
/// verified", and both pairs are back on disk. This is the one app-level
/// test that moves REAL A1 files (~126 MB), so it also proves the copy
/// engine on real bytes; the clash question's own flows are driven on
/// small fixtures below. The deletion runs on a helper thread that polls
/// the destination — the drive script is wall-clock timed, so the second
/// phase starts well after the first copy can finish. Fixtures are
/// symlinks (the copy follows them); the copies are removed at the end.
#[test]
fn copy_picks_rerun_recopies_hand_deleted_files() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("rerun-src");
    let dest = out_dir().join("rerun-dest");
    std::fs::create_dir_all(&src).unwrap();
    let raw = raws_dir().join("A1_full_compressed.ARW");
    place_fixture(&raw, &src.join("a.ARW"));
    place_fixture(&raw, &src.join("b.ARW"));
    let raw_len = std::fs::metadata(&raw).unwrap().len();

    // Phase 1 lands at ~3.2 s; the helper deletes as soon as both pairs
    // are complete; phase 2 starts at 12 s with a wide margin for a debug
    // build hashing 126 MB twice.
    let script = format!(
        "1600:key:y;1900:key:y;2200:copydest:{dest};2600:key:ctrl+e;3000:dump.first;\
         3200:key:return;12000:key:escape;12400:key:ctrl+e;12800:dump.second;\
         13000:key:return;19000:dump.third;19300:key:escape;19600:dump.end",
        dest = dest.display()
    );
    let landed = ["a.ARW", "a.ARW.xmp", "b.ARW", "b.ARW.xmp"];
    let deleter = {
        let dest = dest.clone();
        std::thread::spawn(move || -> Result<(), String> {
            let deadline = Instant::now() + Duration::from_secs(11);
            while !landed.iter().all(|n| dest.join(n).exists()) {
                if Instant::now() > deadline {
                    return Err(format!(
                        "the first copy did not land within 11 s: {:?}",
                        std::fs::read_dir(&dest).map(|d| d
                            .filter_map(|e| e.ok())
                            .map(|e| e.file_name())
                            .collect::<Vec<_>>())
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            // WHOLE pairs, both of them: with the RAW and its sidecar
            // gone, nothing at the destination is in the way any more, so
            // the copy just happens — no clash question in the middle of
            // the regression this test exists for. (A half-deleted pair
            // leaves the sidecar NAME occupied, which is a clash by
            // design and is covered by the clash-question test below.)
            for n in ["a.ARW", "a.ARW.xmp", "b.ARW", "b.ARW.xmp"] {
                std::fs::remove_file(dest.join(n)).map_err(|e| format!("rm {n}: {e}"))?;
            }
            Ok(())
        })
    };
    let out = out_dir().join("copy-rerun.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let deleted = deleter.join().expect("deleter thread");
    let on_disk: Vec<(String, u64)> = std::fs::read_dir(&dest)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| {
                    (
                        e.file_name().to_string_lossy().into_owned(),
                        e.metadata().map(|m| m.len()).unwrap_or(0),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    std::fs::remove_dir_all(&dest).ok();

    assert_eq!(deleted, Ok(()), "hand deletion did not happen:\n{stderr}");
    let field = dump_text;
    let first = qedump(&stderr, "first");
    assert!(
        field(first, "summary").contains("2 picked") && field(first, "copynote").is_empty(),
        "the first plan is not a plain two-file copy: {first}"
    );
    let second = qedump(&stderr, "second");
    let summary = field(second, "summary");
    let note = field(second, "copynote");
    assert!(
        summary.contains("2 picked") && !summary.contains("0 B to copy"),
        "the re-run plan still skips the hand-deleted copies: {second}"
    );
    assert!(
        note.contains("2 copied earlier but gone from the destination — copying again"),
        "the re-run plan does not name the gone copies: {second}"
    );
    assert!(
        !note.contains("refreshed") && !note.contains("already at destination"),
        "the re-run plan still claims a skip/refresh over deleted files: {second}"
    );
    let third = qedump(&stderr, "third");
    let report = field(third, "report");
    assert!(
        report.starts_with("2 copied, all checksums verified") && !report.contains("refreshed"),
        "the re-run did not copy both pairs again: {third}"
    );
    for n in landed {
        assert!(
            on_disk.iter().any(|(f, _)| f == n),
            "{n} missing after the re-run: {on_disk:?}"
        );
    }
    assert!(
        !on_disk.iter().any(|(f, _)| f.contains("partial")),
        "partial files left behind: {on_disk:?}"
    );
    // The RAWs came back whole: the copy followed the symlinks and wrote
    // every byte (the report's verified line is the checksum proof).
    for n in ["a.ARW", "b.ARW"] {
        let len = on_disk.iter().find(|(f, _)| f == n).map(|(_, l)| *l);
        assert_eq!(len, Some(raw_len), "{n} is not a whole copy: {on_disk:?}");
    }
}

/// The clash question, end to end (fileops.md, "The clash question"):
/// every answer, driven through the real dialog with real key events.
///
/// One folder already holds a file under a name a pick would take. The
/// dialog must ASK — once, for the whole run — and then:
///   * Enter must NOT answer it (Ctrl+E, Enter, Enter is muscle memory;
///     it may never mass-replace or mass-duplicate),
///   * "Keep both" (B) lands the clashing pick as `a_1.ARW`, sidecar in
///     lockstep, and leaves the file that was there byte-for-byte alone,
///   * "Overwrite" (O) replaces the differing file and re-VERIFIES the
///     one that is already identical instead of re-sending it,
///   * Esc cancels: the dialog stays open on its plan (destination and
///     template intact) and NOTHING is copied — not even the clash-free
///     file, which is the half of Cancel that a second destination folder
///     proves on disk at the end of the run.
///
/// Fixtures are 2 KB files with RAW extensions (they scan as images and
/// fail to decode, exactly like `broken.ARW` elsewhere in this file): the
/// dialog, the answers and the disk are what is under test here, and the
/// real-bytes path is covered by the re-run test above.
///
/// RED pre-change (verified against a worktree at c060e7c): there is no
/// question at all — the clashing pick is silently auto-suffixed `_2` and
/// `copystate` does not exist in the dump.
#[test]
fn copy_picks_asks_once_and_each_answer_does_what_it_says() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("clash-src");
    let src2 = out_dir().join("clash-src2");
    let dest = out_dir().join("clash-dest");
    let dest2 = out_dir().join("clash-dest2");
    for d in [&src, &src2, &dest, &dest2] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(src2.join("other.ARW"), vec![0xEFu8; 2048]).unwrap();
    let (a_bytes, b_bytes) = (vec![0xABu8; 2048], vec![0xCDu8; 2048]);
    std::fs::write(src.join("a.ARW"), &a_bytes).unwrap();
    std::fs::write(src.join("b.ARW"), &b_bytes).unwrap();
    // The other body's frame, under a name one of the picks wants.
    let foreign = b"another body's frame".to_vec();
    std::fs::write(dest.join("a.ARW"), &foreign).unwrap();
    std::fs::write(dest2.join("a.ARW"), &foreign).unwrap();

    let script = format!(
        "1500:key:y;1700:key:y;1900:copydest:{dest};2100:key:ctrl+e;2400:dump.preview;\
         2600:key:return;2900:dump.question;3100:key:return;3300:dump.inert;\
         3380:key:ctrl+o;3440:dump.accel;\
         3500:key:b;4300:dump.kept;4600:key:escape;\
         4800:key:ctrl+e;5100:key:return;5400:dump.q2;5600:key:o;6400:dump.over;\
         6700:key:escape;6900:copydest:{dest2};7100:key:ctrl+e;7400:key:return;\
         7700:dump.q3;7900:key:escape;8200:dump.cancelled;8500:key:escape;8800:dump.end;\
         9000:key:ctrl+e;9300:key:return;9600:dump.q4;9800:open:{src2};10200:dump.swapped",
        dest = dest.display(),
        dest2 = dest2.display(),
        src2 = src2.display()
    );
    let out = out_dir().join("copy-clash.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let listing = |d: &Path| -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = std::fs::read_dir(d)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().into_owned(),
                            e.metadata().map(|m| m.len()).unwrap_or(0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    let on_disk = listing(&dest);
    let cancelled_disk = listing(&dest2);
    let landed_a = std::fs::read(dest.join("a.ARW")).ok();
    let landed_a1 = std::fs::read(dest.join("a_1.ARW")).ok();
    let landed_a1_xmp = std::fs::read(dest.join("a_1.ARW.xmp")).ok();
    let src_a_xmp = std::fs::read(src.join("a.ARW.xmp")).ok();
    for d in [&src, &src2, &dest, &dest2] {
        std::fs::remove_dir_all(d).ok();
    }

    // --- the question exists, and states the split -----------------------
    let preview = qedump(&stderr, "preview");
    assert_eq!(dump_field(preview, "copystate"), "0", "{preview}");
    assert!(
        dump_text(preview, "copynote").contains("1 new · 1 already exist here"),
        "the plan preview does not pre-announce the clash: {preview}"
    );
    let question = qedump(&stderr, "question");
    assert_eq!(
        dump_field(question, "copystate"),
        "3",
        "Copy did not ask the clash question: {question}"
    );
    let asked = dump_text(question, "confirm");
    assert!(
        asked.contains("1 of your 2 picks already have files with these names in")
            && asked.contains("The other 1 copies normally"),
        "the question does not state the counts: {asked}"
    );
    // --- Enter is inert on it --------------------------------------------
    let inert = qedump(&stderr, "inert");
    assert_eq!(
        dump_field(inert, "copystate"),
        "3",
        "Enter answered the clash question — Ctrl+E, Enter, Enter must never \
         replace or duplicate anything: {inert}"
    );
    // --- an ACCELERATOR must not answer it either -------------------------
    // Ctrl+O (Open Folder) reaches this scope as a plain "o" plus a
    // modifier: unguarded, the reflex answered the question — with the
    // destructive answer (gate finding).
    assert_eq!(
        dump_field(qedump(&stderr, "accel"), "copystate"),
        "3",
        "Ctrl+O answered the clash question: {stderr}"
    );
    // --- B: keep both -----------------------------------------------------
    let kept = qedump(&stderr, "kept");
    assert_eq!(dump_field(kept, "copystate"), "2", "{kept}");
    let kept_report = dump_text(kept, "report");
    assert!(
        kept_report.contains("2 copied, all checksums verified")
            && kept_report.contains("1 landed under new names (a_1.ARW"),
        "keep-both did not copy both under a fresh name: {kept_report}"
    );
    // --- O: overwrite -----------------------------------------------------
    let q2 = qedump(&stderr, "q2");
    assert_eq!(
        dump_field(q2, "copystate"),
        "3",
        "a re-run over the session's own copies must ask again: {q2}"
    );
    assert!(
        dump_text(q2, "confirm").contains("2 of your 2 picks"),
        "{q2}"
    );
    let over = qedump(&stderr, "over");
    let over_report = dump_text(over, "report");
    assert!(
        over_report.contains("1 copied")
            && over_report.contains("1 already identical — re-verified in place")
            && over_report.contains("1 replaced"),
        "overwrite re-sent the identical file (or did not replace the other): {over_report}"
    );
    // --- Esc: cancel -------------------------------------------------------
    let q3 = qedump(&stderr, "q3");
    assert_eq!(dump_field(q3, "copystate"), "3", "{q3}");
    let cancelled = qedump(&stderr, "cancelled");
    assert_eq!(
        (
            dump_field(cancelled, "copystate"),
            dump_field(cancelled, "copy")
        ),
        ("0", "true"),
        "Esc on the question must return to the plan, not close the dialog: {cancelled}"
    );
    assert!(
        dump_text(cancelled, "report").is_empty(),
        "a cancelled question left a copy report behind: {cancelled}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "end"), "copy"),
        "false",
        "the second Esc did not close the dialog"
    );
    // --- a session swap UNDER the question -----------------------------
    // The menu bar stays live while the dialog is up, so a folder can be
    // opened underneath the question — and the answer is a policy that
    // gets replanned, which would apply "overwrite everything" to a set
    // of picks the user never saw named.
    assert_eq!(dump_field(qedump(&stderr, "q4"), "copystate"), "3");
    assert_eq!(
        dump_field(qedump(&stderr, "swapped"), "copystate"),
        "0",
        "opening a folder under the clash question left it answerable for \
         picks that are no longer the session's: {stderr}"
    );

    // --- what the disk says ------------------------------------------------
    assert_eq!(
        cancelled_disk,
        vec![("a.ARW".to_string(), foreign.len() as u64)],
        "Cancel copied something — it must copy NOTHING, not even the clash-free pick \
         (and neither may the session swap under the second question)"
    );
    let names: Vec<&str> = on_disk.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "a.ARW",
            "a.ARW.xmp",
            "a_1.ARW",
            "a_1.ARW.xmp",
            "b.ARW",
            "b.ARW.xmp"
        ],
        "unexpected destination contents: {on_disk:?}"
    );
    assert_eq!(
        landed_a1.as_deref(),
        Some(a_bytes.as_slice()),
        "keep-both did not land the pick under _1"
    );
    // The pairing invariant this whole change exists to protect: the
    // sidecar beside `a_1.ARW` is a's sidecar, not the one belonging to
    // the file that was already there (gate finding: the app test checked
    // the pair by NAME only).
    assert_eq!(
        landed_a1_xmp, src_a_xmp,
        "a_1.ARW.xmp is not the sidecar of the RAW beside it"
    );
    assert_eq!(
        landed_a.as_deref(),
        Some(a_bytes.as_slice()),
        "overwrite did not replace the file that was there"
    );
}

/// `{camera}` used to expand to nothing in the app: both template engines
/// were handed `camera: None`, so a rename template of `{camera}.{ext}`
/// wrote `.ARW` — a hidden file with no name of its own — and an IPTC
/// template stamped an empty string (docs/metadata.md carried a "currently
/// broken, avoid it" warning). The EXIF model now travels with the session,
/// and this drives the whole path with the real reference files: two A1
/// frames, one camera, so the copy also has to resolve the in-batch name
/// collision it creates (`ILCE-1.ARW` + `ILCE-1_1.ARW`).
#[test]
fn camera_template_stamps_the_exif_model() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("camtpl-src");
    let dest = out_dir().join("camtpl-dest");
    std::fs::create_dir_all(&src).unwrap();
    for (i, name) in ["A1_full_compressed.ARW", "A1_full_lossless_compressed.ARW"]
        .iter()
        .enumerate()
    {
        place_fixture(&raws_dir().join(name), &src.join(format!("cam_{i}.ARW")));
    }
    let script = format!(
        "1600:key:y;1900:key:y;2200:copydest:{dest};2600:key:ctrl+e;\
         3000:copytemplate:{{camera}}.{{ext}};3400:dump.planned;3600:key:return;\
         16000:dump.done;16400:key:escape;16800:dump.end",
        dest = dest.display()
    );
    let out = out_dir().join("camera-template.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let mut on_disk: Vec<String> = std::fs::read_dir(&dest)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    on_disk.sort();
    std::fs::remove_dir_all(&dest).ok();
    std::fs::remove_dir_all(&src).ok();

    let planned = qedump(&stderr, "planned");
    assert!(
        !planned.contains("copystate=3"),
        "one camera over two picks is an in-batch collision, which must \
         never raise the clash question: {planned}"
    );
    let done = qedump(&stderr, "done");
    assert!(
        done.contains("2 copied"),
        "the templated copy did not finish: {done}"
    );
    assert_eq!(
        on_disk,
        vec![
            "ILCE-1.ARW".to_string(),
            "ILCE-1.ARW.xmp".to_string(),
            "ILCE-1_1.ARW".to_string(),
            "ILCE-1_1.ARW.xmp".to_string(),
        ],
        "{{camera}} did not stamp the EXIF model (empty would give \
         hidden `.ARW` names)"
    );
}

// ---------------------------------------------------------------------------
// M9, Export Frames as Video (video-export.md). Two driven tests: one over
// the REAL A1 frames, which is the only place the whole chain — preview
// discovery, the byte copy, the container, the verification — is exercised
// on real camera bytes; and one over tiny synthetic RAWs, where the clash
// question's three answers can be driven quickly.
//
// RED pre-change (verified against a worktree at 7b035d6, the commit before
// the app wiring): `clip=` does not exist in the dump, Ctrl+Shift+E opens
// the COPY dialog (the Ctrl+E branch matched the letter without looking at
// Shift), and no `.mov` is ever written.
// ---------------------------------------------------------------------------

/// A synthetic RAW: a little-endian TIFF whose IFD0 points at one embedded
/// "full-res" JPEG of the given size. Kilobytes rather than the 60 MB of a
/// real A1 file, so a test that drives three exports in one run finishes
/// inside the harness deadline. The app scans it by extension and the
/// preview walker finds the JPEG exactly as it does in a camera file.
fn write_synthetic_raw(path: &Path, w: u16, h: u16, orientation: u16, len: usize) {
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x0B, 0x08];
    jpeg.extend_from_slice(&h.to_be_bytes());
    jpeg.extend_from_slice(&w.to_be_bytes());
    jpeg.extend_from_slice(&[0x01, 0x11, 0x00, 0xFF, 0xD9]);
    assert!(len >= jpeg.len(), "padding only");
    jpeg.resize(len, 0x5A);

    let mut out: Vec<u8> = b"II".to_vec();
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // IFD0 offset, patched below
    let jpeg_at = out.len() as u32;
    out.extend_from_slice(&jpeg);
    let ifd_at = out.len() as u32;
    let entries: [(u16, u16, u32); 5] = [
        (0x0100, 3, u32::from(w)),           // ImageWidth
        (0x0101, 3, u32::from(h)),           // ImageLength
        (0x0112, 3, u32::from(orientation)), // Orientation
        (0x0201, 4, jpeg_at),                // JPEGInterchangeFormat
        (0x0202, 4, len as u32),             // ...Length
    ];
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (tag, typ, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&typ.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        // A SHORT lives in the first two bytes of the value field.
        if typ == 3 {
            out.extend_from_slice(&(value as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
        } else {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD
    out[4..8].copy_from_slice(&ifd_at.to_le_bytes());
    std::fs::write(path, out).unwrap();
}

/// The one file the export wrote, read back through the in-tree reader —
/// the check that runs identically on the Windows runner, where there is
/// no ffprobe.
fn read_movie_at(path: &Path) -> fastcull_core::clip::qt::Movie {
    let mut file = std::fs::File::open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    fastcull_core::clip::qt::read_movie(&mut file)
        .unwrap_or_else(|e| panic!("the export did not parse back: {e}"))
}

/// The embedded full-res JPEG of a RAW, as bytes — what every sample in
/// the finished file has to be, byte for byte.
fn embedded_fullres(path: &Path) -> Vec<u8> {
    let mut file = std::fs::File::open(path).unwrap();
    let previews = fastcull_core::raw::find_embedded_jpegs(&mut file).unwrap();
    let jpeg = previews.fullres().expect("a full-res preview").clone();
    fastcull_core::raw::read_jpeg(&mut file, &jpeg).unwrap()
}

/// The whole feature over REAL camera frames: select three A1 files,
/// Ctrl+Shift+E, Enter — and a Motion JPEG `.mov` lands whose samples are
/// the camera's own JPEGs, byte for byte.
///
/// Also the "never a silent grey item" rule (video-export.md): with no
/// selection and the cursor on a single frame there is nothing to export,
/// the menu item is disabled — and the KEYSTROKE still answers, in the
/// status line, instead of doing nothing.
#[test]
fn export_frames_as_video_writes_a_real_motion_jpeg() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("clip-src");
    let dest = out_dir().join("clip-dest");
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    let raws = raws_dir();
    // Named so that capture order and NAME order disagree: these three
    // A1 references were shot at 15:29:13, :40 and :55, and they are
    // named c, a, b in that order — so a file that came out in the grid's
    // (name) order would be a.ARW first, and the name would be `a-c.mov`
    // instead of `c-b.mov`.
    for (name, fixture) in [
        ("c.ARW", "A1_full_compressed.ARW"),
        ("a.ARW", "A1_full_lossless_compressed.ARW"),
        ("b.ARW", "A1_full_uncompressed.ARW"),
    ] {
        place_fixture(&raws.join(fixture), &src.join(name));
    }
    // What the RAWs and any sidecars look like before the export: ADR
    // 0003/0004 say this operation reads them and writes nothing here.
    // SORTED: directory order is not guaranteed stable between two
    // readings of the same folder, and comparing unsorted lists would be
    // a flake waiting for a busy runner.
    let listing = |d: &Path| -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = std::fs::read_dir(d)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().into_owned(),
                            e.metadata().map(|m| m.len()).unwrap_or(0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    let before = listing(&src);

    // 8.5 s for the export itself: it measures ~1.6 s on the development
    // laptop in a DEBUG build (the release screenshot job is faster
    // still), so this is a five-fold margin for a loaded CI runner.
    let script = format!(
        "1600:dump.idle;1900:key:ctrl+shift+e;2200:dump.refused;\
         2500:select-all;2700:clipdest:{dest};2900:key:ctrl+shift+e;\
         3100:key:n;3200:key:y;3300:key:ctrl+o;3400:dump.plan;\
         3500:key:return;12000:dump.done;\
         12400:key:escape;12700:dump.end",
        dest = dest.display()
    );
    let out = out_dir().join("clip-export.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let landed: Vec<String> = listing(&dest).into_iter().map(|(n, _)| n).collect();
    let movie_path = dest.join("c-b.mov");
    let movie = movie_path.is_file().then(|| read_movie_at(&movie_path));
    let movie_bytes = std::fs::read(&movie_path).unwrap_or_default();
    let after = listing(&src);
    // In CAPTURE order, which is what the samples must be.
    let sources: Vec<Vec<u8>> = ["c.ARW", "a.ARW", "b.ARW"]
        .iter()
        .map(|n| embedded_fullres(&src.join(n)))
        .collect();
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
    }

    // --- nothing to export: the item is off AND the key explains itself --
    let idle = qedump(&stderr, "idle");
    assert_eq!(
        dump_field(idle, "clipavail"),
        "false",
        "a lone unmarked frame is not a video: {idle}"
    );
    let refused = qedump(&stderr, "refused");
    assert_eq!(
        dump_field(refused, "clip"),
        "false",
        "the dialog must not open with nothing to export: {refused}"
    );
    assert!(
        dump_text(refused, "status").contains("select frames or stand in a burst"),
        "a refused export must say why, where the user is looking: {refused}"
    );

    // --- the plan, before a byte is written -------------------------------
    let plan = qedump(&stderr, "plan");
    assert_eq!(dump_field(plan, "clip"), "true", "{plan}");
    assert_eq!(dump_field(plan, "clipstate"), "0", "{plan}");
    assert_eq!(
        dump_field(plan, "keysfocus"),
        "false",
        "the dialog owns the keyboard while it is up (issues #41/#42): {plan}"
    );
    // Keyboard CONTAINMENT, not just focus (issue #42): the `N`, `Y` and
    // `Ctrl+O` sent while the dialog was up must have died in it. A mark
    // would show in the status counters, and `Ctrl+O` reaching the grid
    // would open a native folder picker — which blocks the event loop, so
    // that failure arrives as a hung run rather than a wrong assertion.
    let status = dump_text(plan, "status");
    assert!(
        status.contains("· unmarked") && status.contains("★0 ✕0"),
        "a key pressed at the export dialog marked a photo behind it: {status}"
    );
    let summary = dump_text(plan, "clipsummary");
    assert!(
        summary.starts_with("3 frames · 8640×5760 ·"),
        "the plan line does not describe the file: {summary}"
    );
    assert!(
        summary.contains("c-b.mov"),
        "the plan line must name the file it will write: {summary}"
    );
    // These three frames are 27 s and 15 s apart, which is not a cadence:
    // the plan says so BEFORE Enter, in the same words the report uses.
    assert!(
        summary.contains("clamped to 10 fps"),
        "the fallback cadence must be visible before Enter: {summary}"
    );

    // --- what happened ----------------------------------------------------
    let done = qedump(&stderr, "done");
    assert_eq!(dump_field(done, "clipstate"), "2", "{done}");
    let report = dump_text(done, "clipreport");
    assert!(
        report.starts_with("Exported 3 frames")
            && report.contains("all checksums verified")
            && report.contains("c-b.mov"),
        "the report does not say a verified file landed: {report}"
    );
    assert!(
        report.contains("clamped to 10 fps"),
        "the report must repeat the plan's own words: {report}"
    );
    let end = qedump(&stderr, "end");
    assert_eq!(
        dump_field(end, "clip"),
        "false",
        "Esc did not close the dialog"
    );
    assert!(
        dump_text(end, "status").contains("★0 ✕0"),
        "the export changed the user's marks: {end}"
    );

    // --- what is on the disk ----------------------------------------------
    assert_eq!(
        landed,
        vec!["c-b.mov".to_string()],
        "exactly one file, and no `.fastcull-partial-*` left behind"
    );
    let movie = movie.expect("the export must have landed a file");
    assert_eq!(movie.samples.len(), 3);
    assert_eq!(&movie.format, b"jpeg", "Motion JPEG, not something else");
    assert_eq!(&movie.major_brand, b"qt  ");
    assert!(movie.co64, "64-bit offsets always");
    assert!(movie.moov_before_mdat, "it must play while it copies");
    assert_eq!((movie.width, movie.height), (8640, 5760));
    assert_eq!(movie.sample_ms, 100, "10 fps, the clamped cadence");
    assert_eq!(movie.stts_entries, 1, "constant frame rate");
    // Every sample is the camera's own JPEG — and IN CAPTURE ORDER, which
    // for these three files is a, b, c, not the grid's name order.
    for (i, sample) in movie.samples.iter().enumerate() {
        let at = sample.offset as usize;
        let end = at + sample.size as usize;
        assert_eq!(
            movie_bytes[at..end],
            sources[i][..],
            "sample {i} is not frame {} of the capture-ordered burst",
            i + 1
        );
    }

    // --- and the originals are exactly as they were -----------------------
    assert_eq!(
        before, after,
        "the export changed something beside the RAWs (ADR 0003: it may only read them)"
    );
}

/// The clash question, end to end, on tiny synthetic RAWs so three exports
/// fit comfortably inside one driven run: the export must ASK, Enter must
/// not answer, and each answer must do exactly what it says on the disk.
#[test]
fn the_video_export_asks_before_replacing_a_file() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("clipq-src");
    let dest = out_dir().join("clipq-dest");
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    // Three frames that can share a track, and one that cannot — a
    // different frame size, which must be SKIPPED and reported rather
    // than scaled to fit.
    write_synthetic_raw(&src.join("a.ARW"), 400, 300, 1, 4096);
    write_synthetic_raw(&src.join("b.ARW"), 400, 300, 1, 5000);
    write_synthetic_raw(&src.join("c.ARW"), 400, 300, 1, 4500);
    write_synthetic_raw(&src.join("d.ARW"), 380, 285, 1, 4096);
    // A second folder, for the session-swap-under-the-question strand.
    let other = out_dir().join("clipq-other");
    std::fs::remove_dir_all(&other).ok();
    std::fs::create_dir_all(&other).unwrap();
    write_synthetic_raw(&other.join("x.ARW"), 400, 300, 1, 4096);
    write_synthetic_raw(&other.join("y.ARW"), 400, 300, 1, 4096);
    let foreign = b"another day's export".to_vec();
    std::fs::write(dest.join("a-c.mov"), &foreign).unwrap();

    let script = format!(
        "1500:select-all;1700:clipdest:{dest};1900:key:ctrl+shift+e;\
         2200:dump.plan;2400:key:return;2700:dump.question;\
         2900:key:return;3100:dump.inert;3300:key:ctrl+o;3500:dump.accel;\
         3700:key:b;5000:dump.kept;5300:key:escape;\
         5600:key:ctrl+shift+e;5900:key:return;6200:key:o;7500:dump.over;\
         7800:key:escape;8100:dump.end;\
         8400:key:ctrl+shift+e;8700:key:return;9000:dump.q2;\
         9200:open:{other};9800:dump.swapped",
        dest = dest.display(),
        other = other.display()
    );
    let out = out_dir().join("clip-clash.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let mut landed: Vec<String> = std::fs::read_dir(&dest)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    landed.sort();
    let replaced = std::fs::read(dest.join("a-c.mov")).unwrap_or_default();
    let kept = dest
        .join("a-c_1.mov")
        .is_file()
        .then(|| read_movie_at(&dest.join("a-c_1.mov")));
    for d in [&src, &dest, &other] {
        std::fs::remove_dir_all(d).ok();
    }

    // --- the plan leaves the odd frame out, and says so -------------------
    let plan = qedump(&stderr, "plan");
    assert_eq!(dump_field(plan, "clipstate"), "0", "{plan}");
    let skipped = dump_text(plan, "clipskipped");
    assert!(
        skipped.contains("1 frame: different size (380×285)"),
        "the plan must name what it is leaving out: {plan}"
    );
    assert!(
        dump_text(plan, "clipsummary").starts_with("3 frames · 400×300 ·"),
        "{plan}"
    );

    // --- it asks, and Enter is inert on the question ----------------------
    let question = qedump(&stderr, "question");
    assert_eq!(
        dump_field(question, "clipstate"),
        "3",
        "the export replaced a file without asking: {question}"
    );
    assert!(
        dump_text(question, "clipconfirm").contains("a-c.mov"),
        "the question must name the file it is about: {question}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "inert"), "clipstate"),
        "3",
        "Enter answered the question — Ctrl+Shift+E, Enter, Enter must never replace a file"
    );
    // An accelerator reaches this scope as a plain letter plus a modifier;
    // unguarded, the Open Folder reflex answers with the DESTRUCTIVE one.
    assert_eq!(
        dump_field(qedump(&stderr, "accel"), "clipstate"),
        "3",
        "Ctrl+O answered the clash question: {stderr}"
    );

    // --- B: keep both ------------------------------------------------------
    let kept_dump = qedump(&stderr, "kept");
    assert_eq!(dump_field(kept_dump, "clipstate"), "2", "{kept_dump}");
    let kept_report = dump_text(kept_dump, "clipreport");
    assert!(
        kept_report.contains("a-c_1.mov") && kept_report.contains("all checksums verified"),
        "keep-both did not land the video under a fresh name: {kept_report}"
    );
    assert!(
        kept_report.contains("assumed 15 fps"),
        "synthetic frames carry no timing, and the report must say so: {kept_report}"
    );

    // --- O: overwrite ------------------------------------------------------
    let over = qedump(&stderr, "over");
    let over_report = dump_text(over, "clipreport");
    assert!(
        over_report.contains("replaced the file that was already there"),
        "overwrite did not report what it did: {over_report}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "end"), "clip"),
        "false",
        "Esc did not close the dialog"
    );

    // --- a session swap UNDER the question ---------------------------------
    // The menu bar stays live while the dialog is up, so a folder can be
    // opened underneath the question — and the answer is a policy that
    // gets REPLANNED, which would apply "overwrite" to a set of frames
    // the user never saw named. (The Copy Picks dialog has the same
    // strand for the same reason.)
    assert_eq!(dump_field(qedump(&stderr, "q2"), "clipstate"), "3");
    assert_eq!(
        dump_field(qedump(&stderr, "swapped"), "clipstate"),
        "0",
        "opening a folder under the question left it answerable for frames \
         that are no longer the session's: {stderr}"
    );

    // --- what the disk says -------------------------------------------------
    assert_eq!(
        landed,
        vec!["a-c.mov".to_string(), "a-c_1.mov".to_string()],
        "unexpected destination contents (a partial file would show here too)"
    );
    let kept = kept.expect("keep-both must have written a-c_1.mov");
    assert_eq!(kept.samples.len(), 3);
    assert_eq!(&kept.format, b"jpeg");
    assert_ne!(replaced, foreign, "overwrite did not replace the old file");
    assert_eq!(
        &replaced[4..8],
        b"ftyp",
        "the replacement is not a QuickTime file"
    );
}

/// Issue #55: Shift+`]` / Shift+`[` extend the selection by WHOLE bursts,
/// Ctrl+Shift+B selects the burst under the cursor, and Esc clears the
/// selection — driven through real key events (with the Shift and Control
/// modifiers held the way a keyboard holds them) over the `--bursts`
/// synthetic pattern: single 0, burst A = 1..=5, single 6, burst B =
/// 7..=9, burst C = 10..=17 (`SYNTHETIC_BURST_RUNS`).
#[test]
fn burst_keys_select_whole_bursts_and_esc_clears() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("burst-select.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "600:dump.start;800:key:];1000:dump.b1;\
                 1200:key:shift+];1400:dump.ext1;1600:key:shift+];1800:dump.ext2;\
                 2000:key:shift+[;2200:dump.shrink;2400:key:shift+right;2600:dump.frame;\
                 2800:key:escape;3000:dump.cleared;\
                 3200:key:right;3400:key:ctrl+shift+b;3600:dump.burst;\
                 3800:key:ctrl+shift+b;4000:dump.idem;\
                 4200:key:};4400:dump.brace;4600:key:{;4800:dump.brace2",
            ),
        ],
        &out,
    );
    // (cursor image id, selection count) at a dump.
    let at = |label: &str| -> (usize, usize) {
        let d = qedump(&stderr, label);
        (
            dump_field(d, "cursor").parse().unwrap(),
            dump_field(d, "selected").parse().unwrap(),
        )
    };
    assert_eq!(at("start"), (0, 0));
    assert_eq!(at("b1"), (1, 0), "`]` lands on A's opener, selects nothing");
    // The heron: one press from A's opener takes ALL of A plus the single
    // it lands on (the next "burst" in `]`'s territory rule).
    assert_eq!(at("ext1"), (6, 6), "Shift+`]`: A whole plus the single");
    assert_eq!(
        at("ext2"),
        (7, 9),
        "Shift+`]` again: B whole, cursor on B's opener"
    );
    assert_eq!(
        at("shrink"),
        (6, 6),
        "Shift+`[` drops B whole — never half of it"
    );
    // Shift+arrow after a burst span is frame-precise from the burst's
    // edge: "A plus the single plus B's first frame" (persona: one rule).
    assert_eq!(at("frame"), (7, 7), "Shift+Right adds exactly one frame");
    assert_eq!(
        at("cleared"),
        (7, 0),
        "Esc clears the selection; cursor stays"
    );
    assert_eq!(
        at("burst"),
        (8, 3),
        "Ctrl+Shift+B mid-burst: B whole, cursor unmoved"
    );
    assert_eq!(at("idem"), (8, 3), "a double-tap changes nothing");
    // The shifted characters a US keyboard actually sends.
    assert_eq!(
        at("brace"),
        (10, 11),
        "`}}` is Shift+`]`: B plus C, cursor on C's opener"
    );
    assert_eq!(at("brace2"), (7, 3), "`{{` is Shift+`[`: back to just B");
    // The status bar tells the same story the count does.
    assert!(
        dump_text(qedump(&stderr, "brace"), "status").contains("11 selected"),
        "{}",
        qedump(&stderr, "brace")
    );
    assert!(
        !dump_text(qedump(&stderr, "cleared"), "status").contains("selected"),
        "an empty selection is silent: {}",
        qedump(&stderr, "cleared")
    );
}

/// The burst keys in the LOUPE, where no wash shows a selection — and the
/// rule that makes them safe there: Esc clears the selection from inside
/// the loupe (user decision 2026-08-28) while G leaves it alone, the "go
/// and look at what I selected" exit. Before #55 a loupe selection cost
/// forty Shift+arrows; now it is one press, so a stale one that took the
/// next caption would be a daily hazard, not a rare one.
#[test]
fn esc_clears_a_burst_selection_from_inside_the_loupe() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("burst-select-loupe.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts", "--start-loupe"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "600:key:];800:key:shift+];1000:dump.loupe;\
                 1200:key:g;1400:dump.grid;\
                 1600:zoom-in;1700:zoom-in;1800:zoom-in;1900:zoom-in;2000:zoom-in;\
                 2200:dump.back;2400:key:escape;2600:dump.out",
            ),
        ],
        &out,
    );
    let loupe = qedump(&stderr, "loupe");
    let grid = qedump(&stderr, "grid");
    let back = qedump(&stderr, "back");
    let out_dump = qedump(&stderr, "out");
    assert_eq!(dump_field(loupe, "cursor"), "6", "{loupe}");
    assert_eq!(
        dump_field(loupe, "selected"),
        "6",
        "Shift+`]` works in the loupe: {loupe}"
    );
    // G: back to the grid, selection kept.
    assert_ne!(
        dump_field(grid, "zoom"),
        dump_field(loupe, "zoom"),
        "G left the loupe: {grid}"
    );
    assert_eq!(
        dump_field(grid, "selected"),
        "6",
        "G keeps the selection: {grid}"
    );
    // Zoom back into the loupe (`+` five times from 8 columns; `Z` needs a
    // decoded full-res image a synthetic session never has): the
    // selection is still there.
    assert_eq!(
        dump_field(back, "zoom"),
        dump_field(loupe, "zoom"),
        "back in the loupe: {back}"
    );
    assert_eq!(dump_field(back, "selected"), "6", "{back}");
    // Esc from inside the loupe: selection gone AND the loupe left.
    assert_eq!(
        dump_field(out_dump, "selected"),
        "0",
        "Esc clears from the loupe: {out_dump}"
    );
    assert_ne!(
        dump_field(out_dump, "zoom"),
        dump_field(loupe, "zoom"),
        "Esc still leaves the loupe: {out_dump}"
    );
}
