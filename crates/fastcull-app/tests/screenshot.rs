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
    shoot_env_stderr_watching(args, envs, out, |_| {})
}

/// `shoot_env_stderr` with a LIVE view of the child's trace: `on_line` is
/// called on the drain thread for every stderr line as it arrives, before
/// the run ends. The collected string is identical either way.
///
/// The one thing the collected-at-the-end string cannot do is let a helper
/// thread act ON what the app just said (issue #50): a test that
/// manufactures a mid-run file corruption has to anchor it to the app's
/// own progress, or it is guessing at a wall clock that a loaded runner
/// does not honour.
///
/// `on_line` runs ON THE DRAIN THREAD, so it must not block: it is the
/// only reader of the child's stderr pipe, and an observer that waits on
/// a lock or a bounded channel stalls the drain until the pipe fills and
/// the child blocks writing to it — the deadlock this thread exists to
/// prevent. Signal with something that cannot wait (an unbounded
/// `mpsc::Sender`, an atomic) and do the waiting elsewhere.
fn shoot_env_stderr_watching(
    args: &[&str],
    envs: &[(&str, &str)],
    out: &Path,
    mut on_line: impl FnMut(&str) + Send + 'static,
) -> String {
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
    let stderr_pipe = child.stderr.take().expect("stderr piped");
    let drain = std::thread::spawn(move || {
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(stderr_pipe);
        let (mut buf, mut line) = (String::new(), String::new());
        loop {
            line.clear();
            // A read error (a non-UTF-8 byte from a native library) ends
            // the drain, keeping everything read so far — the assertions
            // then report on a truncated log instead of an empty one.
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    on_line(&line);
                    buf.push_str(&line);
                }
            }
        }
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
            let trace = write_trace(out, &stderr);
            assert!(
                status.success(),
                "app exited with {status}; trace: {}; stderr:\n{stderr}",
                trace.display()
            );
            return stderr;
        }
        if Instant::now() >= deadline {
            child.kill().ok();
            // The buffer is KEPT on this path (it used to be dropped): a
            // child killed by the watchdog is the one run whose trace
            // nobody can reconstruct afterwards, and on CI the panic
            // message is all a reader gets.
            let stderr = drain.join().unwrap_or_default();
            let trace = write_trace(out, &stderr);
            panic!(
                "screenshot run timed out (no exit within 90 s); trace: {}; stderr:\n{stderr}",
                trace.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Write a run's stderr next to its shot as `<name>.trace.log`, and
/// return that path.
///
/// Unconditional and BEFORE any panic: a red run on a remote runner is
/// read from the uploaded shots directory, and the assertions here quote
/// a rectangle or a dump line, never the whole app trace that explains it
/// (issue #70 — three Windows failures whose geometry had no witness in
/// the CI log). A failed write is ignored on purpose: losing the
/// diagnostic must never turn a green run red, nor mask the real panic.
fn write_trace(out: &Path, stderr: &str) -> PathBuf {
    let path = out.with_extension("trace.log");
    std::fs::write(&path, stderr).ok();
    path
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

/// A decoded snapshot plus the grid geometry a PER-CELL badge assertion
/// needs (issue #56). Columns, gaps and the cell aspect are core
/// constants, but the row's top is not: the menu bar's height comes from
/// the platform's font metrics, the same dependency the menu-click tests
/// calibrate for. So the first row is LOCATED in the picture — the first
/// bright run down a column the chrome never reaches — and a probe that
/// finds nothing plausible panics instead of returning a wrong rectangle.
struct GridShot {
    w: usize,
    px: Vec<u8>,
    cell_w: f64,
    cell_h: f64,
    row_top: f64,
}

fn grid_shot(path: &Path, columns: usize) -> GridShot {
    let bytes = std::fs::read(path).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (w, h) = dec.dimensions().expect("dims");
    // Device pixels below are compared against LOGICAL offsets (CELL_GAP,
    // the 8/28 px badge steps), which is only valid at scale factor 1 —
    // the harness window is 1440 logical px wide. A HiDPI runner would
    // put every rectangle in the wrong place (the precedent: a hardcoded
    // box that missed the star entirely on the Windows runner), so refuse
    // loudly instead of measuring the wrong pixels.
    assert_eq!(
        w, 1440,
        "grid_shot assumes scale factor 1 (a 1440 px snapshot of the 1440 px window); got {w} px"
    );
    let gap = f64::from(fastcull_core::grid::CELL_GAP);
    let cell_w = (w as f64 - gap * (columns as f64 + 1.0)) / columns as f64;
    let cell_h = cell_w / f64::from(fastcull_core::grid::CELL_ASPECT);
    let luma = |x: usize, y: usize| {
        let i = (y * w + x) * 3;
        0.299 * f64::from(px[i]) + 0.587 * f64::from(px[i + 1]) + 0.114 * f64::from(px[i + 2])
    };
    // Down the middle of the THIRD column: the menu items and the filter
    // chips both stop well left of it, so the first bright run there is
    // the top of the first row of thumbnails. Twenty rows, not a handful:
    // a chip is ~18 px tall, a thumbnail ~180, so a chip that ever reached
    // the probe column cannot pass for a row.
    let probe_x = (gap + 2.0 * (cell_w + gap) + cell_w / 2.0) as usize;
    let mut row_top = None;
    let mut run = 0usize;
    for y in 0..h {
        if luma(probe_x, y) > 60.0 {
            run += 1;
            if run >= 20 {
                row_top = Some((y + 1 - run) as f64);
                break;
            }
        } else {
            run = 0;
        }
    }
    let row_top = row_top.expect("no row of thumbnails in the snapshot");
    // A SANITY CHECK on the probe, not a layout assertion: it says the
    // bright run found is a row of thumbnails and not a piece of chrome
    // (or the whole window). The chrome above row 0 is platform-dependent
    // and MUST stay free to move — measured 80 on the Linux runners (a
    // 40 px in-window menu bar plus the chip bar) and exactly 40 on
    // Windows, where the menu bar is the OS one and only the 34 px chip
    // bar and the 6 px gap are left. The lower bound therefore sits well
    // under the Windows value: a font-metric px in the chip bar must not
    // redden every grid_shot test on one platform (validator 2026-09-02).
    assert!(
        (24.0..250.0).contains(&row_top),
        "the probe found its first bright run at y={row_top}, which is no \
         plausible first cell row — it locked onto the chrome, or onto \
         nothing"
    );
    GridShot {
        w,
        px,
        cell_w,
        cell_h,
        row_top,
    }
}

impl GridShot {
    /// Every pixel of a CELL-LOCAL rectangle of column `col` in row 0, as
    /// (r, g, b). Cell-local so a badge's own offsets read the same here
    /// as they do in `main.slint`.
    fn cell_px(&self, col: usize, x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(f64, f64, f64)> {
        let gap = f64::from(fastcull_core::grid::CELL_GAP);
        let ox = gap + col as f64 * (self.cell_w + gap);
        let mut out = Vec::new();
        for y in (self.row_top + y0) as usize..(self.row_top + y1) as usize {
            for x in (ox + x0) as usize..(ox + x1) as usize {
                let i = (y * self.w + x) * 3;
                out.push((
                    f64::from(self.px[i]),
                    f64::from(self.px[i + 1]),
                    f64::from(self.px[i + 2]),
                ));
            }
        }
        assert!(!out.is_empty(), "empty badge rectangle in column {col}");
        out
    }

    /// What fraction of a cell-local rectangle is DARK — the statistic a
    /// badge pill answers to. Not the mean: the pill's bright glyph sits
    /// in the middle of its own dark background and cancels most of it,
    /// so a mean can barely tell a pill from a photograph. The pill's
    /// backing is `#202028` at 80 % over the picture, i.e. luma ≈ 48,
    /// while the riverbank these frames show never gets near that in the
    /// badge band.
    fn dark_fraction(&self, col: usize, x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
        let px = self.cell_px(col, x0, y0, x1, y1);
        px.iter()
            .filter(|(r, g, b)| 0.299 * r + 0.587 * g + 0.114 * b < 60.0)
            .count() as f64
            / px.len() as f64
    }

    /// How much GREENER than its other channels a rectangle is on
    /// average — the ✓ badge's own signal (`#6ade8a`), read against the
    /// same rectangle of a cell that has no ✓ rather than against an
    /// absolute threshold, because the photograph underneath is foliage.
    fn greenness(&self, col: usize, x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
        let px = self.cell_px(col, x0, y0, x1, y1);
        px.iter().map(|(r, g, b)| g - r.max(*b)).sum::<f64>() / px.len() as f64
    }

    /// Where the first badge PILL of column `col` starts and ends in the
    /// badge band `y0..y1`, cell-local x, or `None` when the band is bare
    /// picture.
    ///
    /// One pixel column at a time across x 0..70: a column belongs to a
    /// pill when at least 30 % of its band is dark (`dark_fraction`'s own
    /// threshold), runs separated by 4 px or less are MERGED — the glyph's
    /// bright strokes cut the pill into two or three runs — and the first
    /// merged run at least 8 px wide is the answer.
    ///
    /// What a badge test may assert is this LEFT EDGE. The width is the
    /// font's: the Windows runner draws ▶ from a face that boxes it, so
    /// the same pill measures 26 px there against 19 px on the ubuntu
    /// runner (and 21 px on the development seat: the Linux face is not
    /// one thing either) —
    /// which is why the fixed 30..46 rectangle this replaced read 0.26
    /// dark on Windows and failed a `< 0.15` control (issue #70, measured
    /// on PR #71's two CI artifacts). The layout is right on both; only
    /// the old assertion assumed one platform's glyph metrics.
    fn pill_span(&self, col: usize, y0: f64, y1: f64) -> Option<(usize, usize)> {
        let mut runs: Vec<(usize, usize)> = Vec::new();
        for x in 0..70usize {
            if self.dark_fraction(col, x as f64, y0, x as f64 + 1.0, y1) < 0.3 {
                continue;
            }
            match runs.last_mut() {
                Some(last) if x - last.1 <= 4 => last.1 = x + 1,
                _ => runs.push((x, x + 1)),
            }
        }
        runs.into_iter().find(|(x0, x1)| x1 - x0 >= 8)
    }

    /// The bright pixels of a rectangle — the glyph strokes — and the
    /// worst channel spread among them. A text glyph takes the `color`
    /// the UI gives it (`#d8d8e0`: bright and neutral); a COLOUR EMOJI
    /// bitmap ignores it, which is the failure this measures.
    fn bright_spread(&self, col: usize, x0: f64, y0: f64, x1: f64, y1: f64) -> (usize, f64) {
        let px = self.cell_px(col, x0, y0, x1, y1);
        let bright: Vec<_> = px
            .iter()
            .filter(|(r, g, b)| 0.299 * r + 0.587 * g + 0.114 * b > 150.0)
            .collect();
        let worst = bright
            .iter()
            .map(|(r, g, b)| r.max(*g).max(*b) - r.min(*g).min(*b))
            .fold(0.0f64, f64::max);
        (bright.len(), worst)
    }
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

/// One app child at a time. Every test takes this lock as its first
/// statement (after its skip guard) and holds it to the end, so the suite
/// is serial no matter how cargo is invoked — two driven `fastcull-app`
/// processes would race each other for the machine and for the shot dir.
///
/// It also means libtest's default thread pool runs NOTHING in parallel
/// here: all the pool ever did was start each test's clock when it was
/// QUEUED rather than when it ran, which is where the 39 "has been
/// running for over 60 seconds" warnings of the v0.13.1 CI run
/// (33694019447) came from — a test whose own work is under a second
/// warned after 60 s of lock-wait — and why the per-test times in those
/// logs were wait, not work. CI therefore runs the suite with
/// `--test-threads=1` (ci.yml, 2026-09-03); delete that flag and the
/// warnings come back — nothing else changes. Nothing is hidden by it
/// either: the pool only ever overlapped core's own test binaries, whose
/// scratch paths are unique per process and thread, so there is no
/// cross-test race here for the flag to mask.
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
                // The LEADING wait is the other half of the same anti-vacuity
                // argument (issue #73). At one column a settle arrives in
                // `claim_cursor_at_loupe` as `view_mutated`, so a settle
                // landing AFTER the `3000:resize` fires a second re-anchor
                // that REPAIRS the state under test — the mutant passes.
                // Worst measured margin between the settle and that resize
                // on the Windows debug runner: 146 ms. Gating in front of
                // `home` (not of the resize) also removes the residual
                // dependence on name-order == capture-order for which image
                // `right` lands on. `schedule_from` rebases the tail on the
                // moment the wait fires and keeps every authored gap, so the
                // 1200 ms lead to the resize survives verbatim.
                "FASTCULL_DRIVE",
                "1400:wait:load settled gen 0;1500:home;1800:right;\
                 3000:resize:1440x700;3050:wait:window geometry 1440x700;\
                 3600:about;4000:about",
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the resize under \
         test was timed against the load, not gated on it:\n{stderr}"
    );
    // The same fact as an ORDERING, so the gate survives a later re-time:
    // the settle's line must sit before the first step it gates. A settle
    // after this point can still re-anchor, which is the vacuity the wait
    // exists to prevent.
    let settled_at = stderr.find("load settled gen 0").unwrap_or_else(|| {
        panic!("the view never settled, so its re-anchor could still repair the state:\n{stderr}")
    });
    let first_key = stderr
        .find("drive: home")
        .unwrap_or_else(|| panic!("the `home` step never ran:\n{stderr}"));
    assert!(
        settled_at < first_key,
        "the load settled after the first key — its own re-anchor can then \
         land after the resize and repair the very offset the assertions \
         below read:\n{stderr}"
    );
    // THE ANTI-VACUITY GUARD (issue #65). Everything below also holds at
    // the default 1440x900, so without this a run whose resize the
    // compositor ignored passes having exercised nothing — measured: with
    // the `resize:` token neutered this test stayed green. The wait means
    // "the app's LAYOUT reached that geometry", which is what the
    // relayout path under test needs; see ui-grid.md on what it does and
    // does not promise about the window afterwards.
    assert!(
        stderr.contains("wait:window geometry 1440x700 (satisfied"),
        "the resize never reached the layout — this run measured the \
         default geometry, where the assertions below hold anyway \
         (issue #65):\n{stderr}"
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
    // Through the shared helper like every other test (QE 2026-09-02):
    // a bare `Command::output()` has no 90 s watchdog — the failure mode
    // validator M2 exists to prevent — writes no `center-anchor.trace.log`
    // beside the shot for the CI artifact, and, missing
    // `FASTCULL_NO_CONFIG`, read the user's real ui.toml.
    let raws = raws_dir();
    let stderr = shoot_env_stderr(
        &["--start-11", raws.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1")],
        &out,
    );
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
            // Settle, then keep the engine busy well past the flip — and
            // the settle is now a FACT, not a clock (issue #73). The 3000 ms
            // pin was 0.95-1.27 s ahead of the settle on the Windows debug
            // runner and lost that race on this seat in 2 of 6 idle runs and
            // 5 of 5 under load. Losing it is SILENT: every engine event
            // then fires BEFORE the flip, the head-follow property this test
            // exists for is never exercised, and both assertions below still
            // pass at the shutter. The wait costs nothing when it is not
            // needed (satisfied after 0 ms on all eleven CI artifacts
            // measured, and the 1000 ms gaps behind it — the "keep the
            // engine busy" cadence — are preserved by `schedule_from`).
            // It needs the one-column mark: at 2900 ms the app is at ONE
            // column in both profiles, the first zoom-out not yet fired.
            (
                "FASTCULL_DRIVE",
                "2900:wait:load settled gen 0;3000:zoom-out;4000:one2one;5000:zoom-out",
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the engine steps \
         were timed, not gated:\n{stderr}"
    );
    // And the same fact as an ORDERING, so the gate survives a later
    // re-time: the settle's own line must sit before the first engine step's
    // echo. Without it the `(satisfied` assertion above proves only that a
    // wait ran, and a script edit could put the zoom-out back in front of
    // the flip with nothing going red (the idiom is the export-dialog wheel
    // test's `settled_at < first_wheel`).
    let settled_at = stderr.find("load settled gen 0").unwrap_or_else(|| {
        panic!("the view never settled, so no engine event fired after the flip:\n{stderr}")
    });
    let first_engine_step = stderr
        .find("drive: zoom-out")
        .unwrap_or_else(|| panic!("the first zoom-out never ran:\n{stderr}"));
    assert!(
        settled_at < first_engine_step,
        "the load settled AFTER the first engine step — every event this \
         test drives fired before the flip, so the head-follow rule was \
         never re-applied and the assertions below prove nothing:\n{stderr}"
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

/// The three `wait:thumb landed idx N` steps of a THREE-FILE fixture, from
/// `ms` and 1 ms apart — one segment per index, because the textures land in
/// any order and a wait can only ask "has this one landed yet". Placed LAST
/// in a script, they hold the shutter (its pending-step count stands while a
/// wait is unsatisfied) until every thumbnail a pixel assertion reads is
/// actually on screen.
///
/// Two limits live in the mark itself. It carries NO session generation, so
/// in a two-session script the old session's landing satisfies the new
/// session's wait — only single-session shots may use it. And it has no
/// index terminator, so `idx 1` is satisfied by `idx 10`: three files, view
/// indices 0-2, is what makes the token unambiguous here.
fn thumb_waits_from(ms: u32) -> String {
    format!(
        "{ms}:wait:thumb landed idx 0;{}:wait:thumb landed idx 1;\
         {}:wait:thumb landed idx 2",
        ms + 1,
        ms + 2
    )
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
    // Three real thumbnails must be ON SCREEN in both shots: the left-edge
    // variance below is photo content, and at grid zoom the shutter has no
    // texture gate of its own — it fires on its bare 1.5 s floor, which is
    // exactly where those textures land on the Windows debug runner (a
    // sibling run over the same three A1 references adopted them at 1554,
    // 1593 and 1628 ms). So each run ends on `thumb_waits_from`, by index
    // because they land in any order; the fixture is the three fetched
    // RAWs and each shot spawns ONE session, which is what makes that
    // token safe here (see the helper). Measured under six spinners in a
    // debug build, the waits held these two shots for 1042 and 1096 ms —
    // without them the floor would have fired with two of the three cells
    // still placeholders.
    //
    // WHAT THE WAITS DO NOT SAY (corrected 2026-09-04, validator F6). The
    // mark carries no retarget generation, so in the open run — toggle at
    // 600 ms, waits at 1000-1002 ms — an adoption from BEFORE the toggle
    // satisfies them, and on a fast seat that is exactly what happens: a
    // release run here landed all three at 42-46 ms (two runs) and every
    // wait reported `satisfied after 0 ms`. So the gate says "three real
    // textures exist", never "these textures were re-cooked at the
    // panel-open cell size" (173x116 closed, 136x90 open in that run) —
    // no token in the harness can say the latter today. It is still the
    // gate worth having: it is what stopped both runs photographing
    // placeholders, and what the variance below reads is photo content
    // versus flat background, not sharpness. The re-cook is covered by
    // the CLOCK, as it always was — the toggle keeps its 600 ms and stays
    // FIRST, so the shutter's 1.5 s floor leaves ≥900 ms of reflow (903 ms
    // in that run), where waits placed in front of the toggle would leave
    // a single 250 ms poll. Both runs are traced, so the waits have a
    // witness — these two shots used to write an empty trace log.
    let thumbs = thumb_waits_from(1000);
    let closed_err = shoot_env_stderr(
        &[raws.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &thumbs)],
        &closed,
    );
    let open_err = shoot_env_stderr(
        &[raws.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_DRIVE", &format!("600:iptc;{thumbs}")),
        ],
        &open,
    );
    for (name, err) in [("closed", &closed_err), ("open", &open_err)] {
        assert!(
            err.contains("wait:thumb landed idx 2 (satisfied"),
            "the {name} run's thumb waits never fired — the shot was timed, \
             not gated, and the variance below may be reading placeholders:\n{err}"
        );
    }
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
            // TIMED, not gated, and deliberately so (issue #73). This
            // schedule used to call itself "settle-then-pin"; it never was.
            // On the Windows debug runner the load settles at 4.7-6.3 s
            // while `home` fires at 1.5-3.1 s, so every step here runs on a
            // still-loading view — and that is harmless, because this
            // fixture CANNOT re-sort: six copies of one file carry one
            // capture key, and `filter.rs`'s comparator breaks a
            // capture-time tie on the filename, so a1..a6 sort identically
            // before and after the settle. A `wait:load settled gen 0` here
            // would buy no ordering and cost the measured +3.2 to +4.9 s of
            // tail (controlled A/B: +3.8 s of script bought +3.9 s of
            // shutter) out of the shutter's 60 s readiness cap — the same
            // budget whose exhaustion is the sibling resize test's only
            // recorded failure mechanism. The historical churn this comment
            // used to blame (Windows CI 2026-07-27: keyed files sorting
            // before keyless mid-load) was fixed at the source by issue #25;
            // the view now holds filename order until the load finishes.
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
///
/// KNOWN INTERMITTENT under load on an 8-core seat, and NOT about the
/// resize (measured 2026-08-31, validator + QE): this is the heaviest
/// script in the suite — six 50 MP frames decoded at 1:1 — and it races
/// the shutter's 60 s texture-readiness cap, so a loaded runner times out
/// before the cursor's texture arrives. HEAD failed 3/3 to 4/5 under six
/// spinners with the same symptom, and the issue #65 wait is satisfied in
/// 0-184 ms in every failing run, so the geometry gate is innocent and
/// the new script is if anything marginally better. When it fails, look
/// for the shutter's readiness timeout, not for the resize; do not blame
/// the wait and do not quiet the test.
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
            // TIMED, not gated, same reasoning as the panel-toggle test and
            // for the same fixture: six copies of one file cannot re-sort
            // (see that test's comment), so a `wait:load settled gen 0`
            // would buy no ordering — and it would cost it here out of the
            // one budget this test is known to lose. Measured on the
            // Windows debug runner the gate is +3.7 to +4.8 s of tail
            // against 18.6-23.6 s of remaining readiness headroom. This
            // test has SIX recorded failing jobs, all Windows, all
            // 2026-07-27, in TWO mechanisms: four are the shutter's 60 s
            // cap (runs 58, 62, 65, 70 — twice it took three tests down
            // with it), two are `the relayout path never fired` guard
            // below going red on bunched resizes (runs 60, 71). The cap is
            // the dominant one and the reason this tail is not spent; the
            // second is the guard hardened below, and the paragraph after
            // this one is why the two resizes sit 4 s apart. The cap's own
            // defect — a texture budget set by script length — is a
            // separate issue.
            // The two resizes sit 4 s apart: a stalled CI event loop
            // fires overdue timers BUNCHED, and back-to-back resizes
            // between two refreshes are a net geometry no-op — the
            // "relayout must fire" guard then fails vacuously (Windows
            // run 30304892053: a ~2.8 s startup stall bunched the whole
            // schedule). Bunching this pair now needs a 4 s stall; the
            // shutter waits for the full script, so the gap is free.
            (
                "FASTCULL_DRIVE",
                // The RESTORE at 6500 is deliberately ungated (issue #65):
                // it asks for the DEFAULT geometry, which the app already
                // announced at its first layout, and a `wait:` asks "has
                // this happened yet" — past marks count, so the wait would
                // be satisfied by that startup line without the restore
                // having landed. It gates nothing, so it claims nothing.
                // The first resize is the one under test and it is gated.
                "1500:home;1650:right;1800:right;1950:right;2100:right;\
                 2500:resize:1000x700;2550:wait:window geometry 1000x700;\
                 6500:resize:1440x900",
            ),
        ],
        &out,
    );
    assert!(
        !stderr.contains("follow-scroll claim"),
        "window resize misread as scrolling — the cursor was claimed:\n{stderr}"
    );
    // The guard must actually have run (validator: without this the test
    // goes vacuously green if the resize stops dislodging the cursor), and
    // it is POSITIONAL (issue #73): `relayout re-anchor` is not the resize's
    // private word. At one column the load settle reaches
    // `claim_cursor_at_loupe` as a view mutation and can emit the identical
    // string with no resize anywhere in the script — QE watched it do so
    // (`relayout re-anchor: cursor kept at pos 0, scroll 794 -> 0`, at the
    // settle's own millisecond). An order-blind `contains` would take that
    // for the resize. What keeps it honest today is the fixture — six
    // copies of one file cannot re-sort, so the settle leaves the cursor's
    // cell wholly visible and the re-anchor arm never fires — and a fixture
    // is not a property. Read the ordering instead, the way the
    // export-dialog wheel test reads its settle — and read it as the
    // SUFFIX after the resize echo, not as "the first re-anchor came
    // after it": a run that re-anchors both before AND after the resize
    // did exercise the path, and only a run with NO re-anchor after the
    // resize failed to. The suffix runs to the end of the trace, the 6500
    // restore included: that step asks for the DEFAULT geometry, so it
    // can only re-anchor if the resize under test landed and moved the
    // layout — and the bunched-resize failure this guard is here to catch
    // leaves neither resize a re-anchor to emit.
    let resized_at = stderr
        .find("drive: resize:1000x700")
        .unwrap_or_else(|| panic!("the resize under test never ran:\n{stderr}"));
    assert!(
        stderr[resized_at..].contains("relayout re-anchor"),
        "no `relayout re-anchor` AFTER the resize under test — the \
         relayout path never fired, so the resize wasn't exercised (a \
         re-anchor earlier in the run belongs to something else and the \
         guard must not take it for the resize):\n{stderr}"
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
                "150:resize:1200x800;200:wait:window geometry 1200x800;\
                 500:end;700:pgup;800:pgup;900:pgup;1000:pgup",
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
                "150:resize:1200x800;200:wait:window geometry 1200x800;\
                 500:end;700:pgup;800:pgup;900:pgup;1000:pgup;\
                 1150:resize:900x800;1200:wait:window geometry 900x800",
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
                "150:resize:1200x800;200:wait:window geometry 1200x800;\
                 500:end;1000:resize:1500x800;\
                 1050:wait:window geometry 1500x800",
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
                "150:resize:900x800;200:wait:window geometry 900x800;\
                 400:down;500:down;600:down;700:down;\
                 1000:resize:1600x800;1050:wait:window geometry 1600x800",
            ),
        ],
        &out,
    );
    // THE ANTI-VACUITY GUARDS (issue #65). Both assertions below are ABSENCES — no re-anchor, cursor still
    // visible — and both hold at the default geometry, so a dropped
    // resize made this test green while exercising nothing.
    assert!(
        stderr.contains("wait:window geometry 900x800 (satisfied"),
        "the resize to 900x800 never reached the layout — this run measured \
         a geometry where the assertions below hold anyway, which is how \
         this test passed with the `resize:` token neutered (issue \
         #65):\n{stderr}"
    );
    assert!(
        stderr.contains("wait:window geometry 1600x800 (satisfied"),
        "the resize to 1600x800 never reached the layout — this run measured \
         a geometry where the assertions below hold anyway, which is how \
         this test passed with the `resize:` token neutered (issue \
         #65):\n{stderr}"
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
            (
                "FASTCULL_DRIVE",
                "150:resize:1200x800;200:wait:window geometry 1200x800;\
                 800:resize:900x800;850:wait:window geometry 900x800",
            ),
        ],
        &out,
    );
    // THE ANTI-VACUITY GUARDS (issue #65). The assertion below is an ABSENCE that holds at any geometry, so
    // without these the test passed with the `resize:` token neutered.
    assert!(
        stderr.contains("wait:window geometry 1200x800 (satisfied"),
        "the resize to 1200x800 never reached the layout — this run measured \
         a geometry where the assertions below hold anyway, which is how \
         this test passed with the `resize:` token neutered (issue \
         #65):\n{stderr}"
    );
    assert!(
        stderr.contains("wait:window geometry 900x800 (satisfied"),
        "the resize to 900x800 never reached the layout — this run measured \
         a geometry where the assertions below hold anyway, which is how \
         this test passed with the `resize:` token neutered (issue \
         #65):\n{stderr}"
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
    // The two scripts are the same schedule; only RELEASE gates the
    // toggles on the sharp baseline the release-strength half asserts
    // below (`wait:loupe idx 0 factor` — the full-res render's own line;
    // the soft and thumb rungs carry their own word between `loupe` and
    // `idx` and cannot satisfy it). It lands at 374 ms on the Linux
    // release runner, so the wait is free there. DEBUG keeps the clock on
    // purpose: the same mark lands at 28.3 s on the Windows debug runner
    // and the harness's 30 s wait cap runs from the STEP, so a wait at
    // 1600 would end those runs at ~31.6 s — while the debug half asserts
    // only post-close stability and is content in the soft regime. Edit
    // the two consts together: they must stay one schedule.
    #[cfg(not(debug_assertions))]
    const DRIVE: &str = "1500:home;1600:wait:loupe idx 0 factor;2000:iptc;2600:iptc";
    #[cfg(debug_assertions)]
    const DRIVE: &str = "1500:home;2000:iptc;2600:iptc";
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", DRIVE)],
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
    // release the toggles WAIT for the sharp view, so the teeth can only
    // be skipped if the wait was dropped or the mark renamed — and then
    // this must FAIL loudly, not pass vacuously forever. What the wait
    // took away is the other half of the old assertion: a release runner
    // whose 50 MP decode took 20 s used to fail here, and now waits. That
    // decode's budget belongs to `perf_budgets`, which measures it
    // directly rather than inferring it from a screenshot schedule.
    #[cfg(not(debug_assertions))]
    assert!(
        stderr.contains("wait:loupe idx 0 factor (satisfied"),
        "the `wait:loupe idx 0 factor` step never fired — the toggles were \
         timed, not gated:\n{stderr}"
    );
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
/// keyboard (user decision: "swallow everything in that screen"). Real
/// N and P keystrokes with About open must mark NOTHING.
///
/// Rewritten for issue #13's fidelity note: this used to open About with
/// the `about` drive token and press N/P as NAV tokens, and both are
/// replicas of the shipped path rather than the path. The nav tokens
/// never reach the `keys` FocusScope at all — the harness mirrors the
/// containment with an `if` of its own — so the test asserted the
/// mirror, and the real guard (the FocusScope's `about-visible` arm)
/// could have been deleted with the suite still green. It now opens
/// About through the REAL Help menu where the geometry is calibrated
/// (the menu's own focus save/restore is the machinery #41 D2 broke in)
/// and sends REAL key events, so what swallows them is the shipped
/// FocusScope. The keyboard's whereabouts is asserted with them: a
/// stranded keyboard would swallow the keys just as thoroughly and mean
/// the opposite.
#[test]
fn about_dialog_renders_and_contains_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("about-dialog.jpg");
    // Off the calibrated runners the popup is opened by the token — it
    // runs the menu item's own `activated` body (visible + modal-opened),
    // so the containment under test is reached honestly; only the menu's
    // focus-restore strand is skipped there.
    let open = if menu_clicks_are_calibrated() {
        "600:click.115,19;900:click.180,93"
    } else {
        "900:about"
    };
    let script = format!(
        "{open};1300:dump.up;1600:key:n;1900:key:p;2300:dump.contained;\
         2600:key:escape;2900:dump.closed;3200:key:n;3600:dump.control;\
         3900:about;4300:dump.shot"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "200"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // The dialog is up (a missed menu click cannot pass), and the modal —
    // not some destroyed element — owns the keyboard.
    let up = qedump(&stderr, "up");
    assert_eq!(
        dump_field(up, "about"),
        "true",
        "About never opened (the Help menu click missed?):\n{stderr}"
    );
    assert_eq!(
        dump_field(up, "focusowner"),
        "0",
        "the keyboard is not on the main scope with About up — a stranded \
         keyboard swallows keys too, and would make the containment below \
         mean nothing (issue #41 D2). Read through the owner token, not \
         `keysfocus`: a deactivated window reads false there with the \
         keyboard alive (issue #63):\n{stderr}"
    );
    // THE containment: two real keystrokes, no mark.
    let contained = qedump(&stderr, "contained");
    assert_eq!(
        dump_field(contained, "about"),
        "true",
        "About closed itself under the stray keys:\n{stderr}"
    );
    assert!(
        dump_text(contained, "status").contains("★0 ✕0"),
        "a mark leaked through the About modal: {contained}"
    );
    // The control: Esc closes it and the SAME key now marks. Without this
    // the containment assertion also passes on a build where N is simply
    // dead.
    assert_eq!(
        dump_field(qedump(&stderr, "closed"), "about"),
        "false",
        "Esc did not close About:\n{stderr}"
    );
    let control = qedump(&stderr, "control");
    assert!(
        dump_text(control, "status").contains("★0 ✕1"),
        "the N after About closed did not reject either — the containment \
         assertion above is vacuous: {control}"
    );
    // Re-opened for the shutter: the pixel assertion at the bottom needs
    // the card on screen, and the closing above is what the control needs.
    assert_eq!(
        dump_field(qedump(&stderr, "shot"), "about"),
        "true",
        "About was not re-opened for the screenshot:\n{stderr}"
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
/// under the scrim. Same containment as About, and driven the same way
/// after issue #13's fidelity note: the REAL Help > Keyboard Shortcuts
/// item, a REAL N, and the keyboard's whereabouts asserted alongside the
/// mark counts (see the About test for why the token-plus-nav version
/// was testing the harness rather than the app).
#[test]
fn shortcuts_popup_contains_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("shortcuts-contained.jpg");
    let open = if menu_clicks_are_calibrated() {
        "600:click.115,19;900:click.180,61"
    } else {
        "900:shortcuts"
    };
    let script = format!(
        "{open};1300:dump.up;1600:key:n;2000:dump.contained;\
         2300:key:escape;2600:dump.closed;2900:key:n;3300:dump.control;\
         3600:shortcuts;4000:dump.shot"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "200"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let up = qedump(&stderr, "up");
    assert_eq!(
        dump_field(up, "shortcuts"),
        "true",
        "the shortcuts popup never opened (the Help menu click missed?):\n{stderr}"
    );
    assert_eq!(
        dump_field(up, "focusowner"),
        "0",
        "the keyboard is not on the main scope with the popup up — a \
         stranded keyboard would swallow the N for the wrong reason \
         (issue #41 D2). Through the owner token, not `keysfocus`, for \
         the deactivation reason in issue #63:\n{stderr}"
    );
    let contained = qedump(&stderr, "contained");
    assert_eq!(
        dump_field(contained, "shortcuts"),
        "true",
        "the popup closed itself under the stray key:\n{stderr}"
    );
    assert!(
        dump_text(contained, "status").contains("★0 ✕0"),
        "a mark leaked through the shortcuts modal: {contained}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "closed"), "shortcuts"),
        "false",
        "Esc did not close the shortcuts popup:\n{stderr}"
    );
    let control = qedump(&stderr, "control");
    assert!(
        dump_text(control, "status").contains("★0 ✕1"),
        "the N after the popup closed did not reject either — the \
         containment assertion above is vacuous: {control}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "shot"), "shortcuts"),
        "true",
        "the popup was not re-opened for the screenshot:\n{stderr}"
    );
}

/// The shortcuts card after the 2026-09-04 rebuild: a 780 px two-column
/// key sheet whose HEIGHT IS ITS CONTENT'S, opened and closed from the
/// keyboard and by a click on the card itself, and fitting whole at the
/// smallest window the design claims.
///
/// Nothing in the suite asserted this popup's size or position before —
/// the only record of it was a doc comment two thousand lines down — so a
/// redesign that scrolled, clipped its footer or hung off the window would
/// have shipped green.
///
/// **NOT ONE PIXEL OF FONT METRICS IS PINNED HERE, and that is the whole
/// design of this test.** It first shipped with a height band
/// (`480..=794`) and a big-window/small-window height EQUALITY, and both
/// were this development seat's Noto Sans wearing the costume of a
/// property: forcing fonts that exist on the CI runners broke them
/// immediately — Liberation Sans lays the card out 473 px tall and fails
/// the band's floor, Noto Sans Mono 609 px and fails the equality, because
/// the equality silently carried a 594 px ceiling (the 1000x700 modal
/// layer, 634, minus the 40 px clamp) with no way to say so. The Windows
/// runner draws in Segoe UI and the ubuntu runner in DejaVu Sans; neither
/// is what this seat renders. So what is pinned is what the DESIGN
/// guarantees, and each of these holds in any font:
///
/// 1. **`?` and F1 open it, and close it.** It used to be reachable only
///    from Help > Keyboard Shortcuts, i.e. only with the mouse, in a
///    keyboard-first app.
/// 2. **A click ON THE CARD closes it** — at the centre at 1440x900, and
///    on the body's right edge at 1010x520, where the card is clamped.
///    The hint says "click anywhere", and the card is where a hand aiming
///    at "anywhere" lands. It works only because nothing in the card takes
///    the pointer, which is why the body is a non-interactive `Flickable`
///    and not a `ScrollView`. The second click is the discriminating one
///    and the first is not: a fluent ScrollView wraps a Flickable that is
///    ALSO non-interactive and hides its ScrollBar until something
///    overflows, so it eats a click only on the 14 px strip its bar
///    occupies, only while the card is clamped. Mutation-checked at both
///    points — see the comment on the assertions.
/// 3. **780 px wide, exactly.** That number is geometric (18 + 2 x (104
///    key + 14 gutter + 240 action) + 28 + 18) and so is the same on every
///    seat: it is the fixed key cell — the whole alignment contract — plus
///    the room the action column was measured to need.
/// 4. **It lies inside the modal layer, and it FITS WHOLE at the smallest
///    supported window.** Not a height in pixels — a relation to the layer
///    it is centred in. The card is clamped 40 px inside that layer, so a
///    clamped card leaves exactly 20 px between its floor and the layer's;
///    more than 20 means it fits, and "it never scrolls at a supported
///    size" is exactly that. The layer's FLOOR is the status bar's top on
///    every platform, which is why the check is written against it: its
///    top is not, the menu bar being in-window on Linux and the OS
///    window frame's on Windows.
/// 5. **The same height at both window sizes** — asserted after 4, so it
///    can only mean what it says: the content and the width are identical
///    at 1440x900 and at 1000x700, so the height must be too, whatever
///    face draws it. Before 4 it was doing clamp detection in disguise.
/// 6. **The footer is inside the card, at both sizes.** The body is the
///    child that yields when the window is short (issue #62's rule), so
///    this is the assertion that says the yielding lands where it should —
///    and it is also what catches a card that collapsed to nothing, which
///    is the failure the band's floor was aimed at.
///
/// The last strand is the one the new binding owes: with the keyboard in
/// the keyword field, `?` is a question mark, not a popup.
#[test]
fn shortcuts_card_is_a_two_column_sheet_that_fits_its_window() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("shortcuts-card.jpg");
    let script = format!(
        "{PIN_WINDOW};900:key:?;1300:dump.opened;1600:key:?;1900:dump.closed;\
         2200:key:f1;2600:dump.f1;\
         3000:click:shortcuts card;3400:dump.clicked;\
         3800:key:f1;\
         4200:resize:1000x700;4400:wait:shortcuts card laid out at 110,;\
         4800:dump.small;5200:key:f1;5500:dump.gone;\
         5800:resize:1010x520;6100:key:f1;\
         6300:wait:shortcuts card laid out at 115,;\
         6700:click.870,250;7100:dump.clickedclamped;7400:key:escape;\
         7700:resize:1440x900;8100:key:k;\
         8200:wait:iptc field 0 laid out at 1150;\
         8600:key:?;9000:dump.typing"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "200"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // The panel gate doubles as the resize gate: `iptc field 0` is laid out
    // at x=1150 only in a 1440 px window, so a `k` that arrived before the
    // window came back would never satisfy it — a failure, not a silent
    // re-timing (issue #13's rule).
    for gate in [
        "wait:shortcuts card laid out at 110, (satisfied",
        "wait:shortcuts card laid out at 115, (satisfied",
        "wait:iptc field 0 laid out at 1150 (satisfied",
    ] {
        assert!(
            stderr.contains(gate),
            "the `{gate}…` gate never fired — the steps after it were timed:\n{stderr}"
        );
    }

    // --- 1: the keyboard opens it, and closes it
    assert_eq!(
        dump_field(qedump(&stderr, "opened"), "shortcuts"),
        "true",
        "`?` did not open the shortcuts popup:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "closed"), "shortcuts"),
        "false",
        "`?` did not close the shortcuts popup it had opened:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "f1"), "shortcuts"),
        "true",
        "F1 did not open the shortcuts popup:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "gone"), "shortcuts"),
        "false",
        "F1 did not close the shortcuts popup:\n{stderr}"
    );

    // --- 2: a click ON THE CARD closes it, twice, and the second one is
    // the one that bites
    //
    // `click:<element>` resolves to the CENTRE of the rectangle the card
    // last reported, which is squarely on the body. The spec says this
    // card closes "with a click anywhere, INCLUDING on the card", and
    // until this line the suite only ever clicked the scrim.
    assert_eq!(
        dump_field(qedump(&stderr, "clicked"), "shortcuts"),
        "false",
        "a click at the centre of the card did not close it — the hint on \
         it promises \"click anywhere\", and something in the card is now \
         eating the pointer before the scrim's TouchArea sees it (a hover \
         highlight, any TouchArea at all):\n{stderr}"
    );
    // The second click is at 1010x520, where the card is CLAMPED and the
    // list really does scroll, on the 14 px strip down the body's right
    // edge — `x 870` is the middle of 863..877, the card's content ending
    // at 1010/2 + 390 − 18 = 877. That strip is where a `ScrollView` puts
    // its ScrollBar, and the ScrollBar owns a TouchArea.
    //
    // MEASURED, because the reason recorded for choosing a Flickable over
    // a ScrollView was wrong about the mechanism and this is what is
    // actually true (i-slint-compiler 1.17.1
    // `widgets/fluent/scrollview.slint`): the fluent ScrollView's own
    // Flickable is `interactive: false` (:174-176), exactly like ours, and
    // its ScrollBar is `visible` only while `maximum > 0` (:54). So where
    // the card FITS, a ScrollView is as transparent to the pointer as a
    // Flickable is — swapping one in leaves the centre click above green —
    // and the difference appears only once the card is clamped, precisely
    // where the safety valve is doing its job. Driven both ways at this
    // size and this point: Flickable closes the card, ScrollView leaves it
    // open.
    assert_eq!(
        dump_field(qedump(&stderr, "clickedclamped"), "shortcuts"),
        "false",
        "at 1010x520 the card is clamped and its list scrolls; a click on \
         the strip where a scrollbar would live did NOT close it, so \
         \"click anywhere\" has quietly stopped being true over a band of \
         the card while the hint still promises it:\n{stderr}"
    );

    // --- 3, 4, 6: the card's shape, at both sizes
    let (_, _, _, big_h) = laid_out_at(&stderr, "shortcuts card", "opened");
    assert_shortcuts_card_shape(&stderr, "opened", 1440.0, 900.0);
    assert_shortcuts_card_shape(&stderr, "small", 1000.0, 700.0);

    // --- 5: and it is the CONTENT's height, not the window's
    //
    // Only meaningful because neither size clamped (asserted just above):
    // the two windows show the same 27 rows at the same 780 px, so the
    // preferred height they add up to is the same number in any font. A
    // difference here means some length in the card is reading the window
    // — which is the one thing a content-driven card must not do.
    let (_, _, _, small_h) = laid_out_at(&stderr, "shortcuts card", "small");
    assert_eq!(
        small_h, big_h,
        "the card is {small_h} px tall at 1000x700 but {big_h} at 1440x900, \
         and neither is clamped — so its height depends on the window it \
         is centred in, not on the list in it:\n{stderr}"
    );

    // --- the new binding cannot fire from a text field
    let typing = qedump(&stderr, "typing");
    assert_ne!(
        dump_field(typing, "focusowner"),
        "0",
        "the keyword field does not hold the keyboard, so the `?` below \
         proves nothing about text fields:\n{stderr}"
    );
    assert_eq!(
        dump_field(typing, "shortcuts"),
        "false",
        "`?` typed into the keyword field opened the shortcuts popup — the \
         opener lives in the main key scope precisely so it cannot:\n{stderr}"
    );
}

/// The shortcuts card's shape at one window size, in the terms the design
/// guarantees and no others: 780 px wide, inside the modal layer, and
/// FITTING WHOLE there — plus its footer inside it.
///
/// The one number that is deliberately absent is a height. The height is
/// the sum of ~27 text line boxes and belongs to whatever face the seat
/// draws with (549 px in this machine's Noto Sans, 491 in Liberation
/// Sans, 512 in Nimbus Sans / Carlito / Cantarell, 525 in Montserrat,
/// 627 in Noto Sans Mono); pinning it, or a band around it, pins a font. What the card actually promises is a
/// relation to the layer it is centred in, and that is what is checked.
///
/// **How "it fits whole" is measured without knowing the ceiling.** The
/// card's height is `min(content, layer − 40px)`, so a CLAMPED card is
/// exactly `layer − 40` tall and sits exactly 20 px above the layer's
/// floor; an unclamped one leaves more. The layer's floor is the status
/// bar's top — `window − 26` — on every platform. Its TOP is not: the
/// menu bar is drawn in-window on Linux (40 px) and belongs to the OS
/// window frame on Windows (ui-grid.md's CI section), which moves the
/// layer's top, its height and therefore the ceiling by 40 px between the
/// two runners. Measuring the slack under the card instead of the height
/// against a ceiling makes the check the same sentence on both.
fn assert_shortcuts_card_shape(stderr: &str, label: &str, window_w: f32, window_h: f32) {
    let (x, y, w, h) = laid_out_at(stderr, "shortcuts card", label);

    // 780 px is arithmetic, not a measurement: 18 padding + 2 x (104 key +
    // 14 gutter + 240 action) + 28 column gutter + 18 padding. It is the
    // same on every seat, so it is the one length that may be an equality.
    assert_eq!(
        w, 780.0,
        "dump.{label}: the shortcuts card is {w} px wide at \
         {window_w}x{window_h}, not the 780 the two 104 px key columns and \
         their action columns add up to:\n{stderr}"
    );

    let layer_floor = window_h - 26.0;
    assert!(
        x >= 0.0 && x + w <= window_w && y >= 0.0 && y + h <= layer_floor,
        "dump.{label}: the shortcuts card ({x},{y} {w}x{h}) is not inside \
         the modal layer of a {window_w}x{window_h} window (which ends at \
         y={layer_floor}, the top of the status bar):\n{stderr}"
    );

    let slack = layer_floor - (y + h);
    assert!(
        slack > 20.0,
        "dump.{label}: THE CARD OUTGREW ITS SMALLEST WINDOW. It is {h} px \
         tall at {window_w}x{window_h} and leaves {slack} px between its \
         floor and the status bar — the clamp's own 20 px, which is what a \
         card pinned at `layer − 40` leaves, so the list inside it now \
         scrolls at a size the design says it must not. The ceiling here is \
         {} px where the menu bar is drawn in-window (window − 26 status − \
         40 menu − 40 clamp), 40 more where it is the OS's. Either the card \
         grew a section or this seat's face is far taller than the ones it \
         was measured on:\n{stderr}",
        window_h - 106.0
    );

    assert_footer_inside_the_shortcuts_card(stderr, label, window_h);
}

/// The shortcuts card's footer (the zoom-ladder line) is inside the card,
/// and the card is inside the window. Issue #62's contract, on the third
/// card that grows with its content — the body is the child with
/// `vertical-stretch: 1; min-height: 0px`, so a window too short for the
/// list must clip the LIST, never push the footer through the card's floor.
///
/// This is also what stands in for the height band's floor: a card that
/// collapsed puts its footer outside itself, and fails here by name.
fn assert_footer_inside_the_shortcuts_card(stderr: &str, label: &str, window_h: f32) {
    let (_, card_y, _, card_h) = laid_out_at(stderr, "shortcuts card", label);
    let (_, foot_y, _, foot_h) = laid_out_at(stderr, "shortcuts footer", label);
    assert!(
        foot_y >= card_y,
        "dump.{label}: the shortcuts footer starts above its card:\n{stderr}"
    );
    assert!(
        foot_y + foot_h <= card_y + card_h + 0.5,
        "dump.{label}: the shortcuts footer ends at {} but the card ends at \
         {} — the zoom ladder is outside the card:\n{stderr}",
        foot_y + foot_h,
        card_y + card_h
    );
    assert!(
        card_y + card_h <= window_h,
        "dump.{label}: the shortcuts card ends at {} in a {window_h}px \
         window:\n{stderr}",
        card_y + card_h
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
///
/// Three files also means view indices 0, 1 and 2 and nothing higher, which
/// is what lets these scripts gate their shots on `thumb_waits_from` — see
/// that helper for the token's small print.
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
    // pinned on the SETTLED view before anything is selected — waited for
    // since 2026-09-03 rather than assumed, because on the Windows debug
    // runner the settle lands at ~1.5 s, AFTER the old 700 ms `home`, and
    // `place_three_distinct`'s filename order happening to equal its capture
    // order is all that kept the two runs addressing the same cells.
    //
    // The settle is the ordering premise, not the gate. What these two shots
    // compare is RENDERED PIXELS in one region across two processes, and the
    // settle means every thumb's BYTES were drained, not that any texture is
    // on screen: on the Windows debug runner the textures land 36-660 ms
    // behind the bytes, and at grid zoom the shutter has no texture gate of
    // its own (it fires on its 1.5 s floor). Both of today's Windows shots
    // read `0/3 loaded · sorting by name until loaded`, i.e. the pair is
    // comparable only because both sit on the same side of adoption; gating
    // on the settle alone would have moved them INTO that window, one on
    // each side. So each run ends by waiting for the three textures the
    // samples read, by index because they land in any order, and LAST so the
    // shutter's pending-step count holds the shot until they are in. The
    // distinction is measurable on any seat: under six spinners in a debug
    // build the settle here fired 1.6-1.8 s late and the LAST of the three
    // textures landed 47-49 ms after it, with both shots then reporting
    // `3 thumbs loaded` instead of the Windows runner's `0/3`.
    let thumbs = thumb_waits_from(800);
    let (fx0, fy0, fx1, fy1) = (0.02, 0.11, 0.10, 0.20);

    let plain = out_dir().join("sel-wash-none.jpg");
    let plain_err = shoot_env_stderr(
        &[folder],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                &format!("600:wait:load settled gen 0;700:home;{thumbs}"),
            ),
        ],
        &plain,
    );
    let sel = out_dir().join("sel-wash-some.jpg");
    let sel_err = shoot_env_stderr(
        &[folder],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                &format!(
                    "600:wait:load settled gen 0;700:home;900:shift-right;\
                     1000:shift-right;{}",
                    thumb_waits_from(1100)
                ),
            ),
        ],
        &sel,
    );
    for (name, err) in [("plain", &plain_err), ("selected", &sel_err)] {
        assert!(
            err.contains("wait:load settled gen 0 (satisfied"),
            "the {name} run's `wait:load settled gen 0` never fired — its \
             `home` was timed, not gated:\n{err}"
        );
        assert!(
            err.contains("wait:thumb landed idx 2 (satisfied"),
            "the {name} run's thumb waits never fired — the shot was timed, \
             not gated, and the two runs can photograph different mixes of \
             placeholder and photo:\n{err}"
        );
    }
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

    // Same two gates as `selection_wash_tints_the_grid_and_status_counts`,
    // for the same reasons: `wait:load settled gen 0` before the positional
    // `home` (the Windows debug settle lands at ~1.5 s, after the old 700 ms
    // step, and `pick` marks whichever image the current order puts under
    // the cursor), and the three `thumb landed` waits LAST, because the
    // star search runs over rendered pixels — `region_glyph_yellowness`
    // counts pixels with `r >= 180 && r - b >= 60`, a set that changes with
    // the background under the star, so a photo in one shot and a
    // placeholder in the other is a difference the 40-point threshold
    // cannot tell from a washed badge.
    let picked = out_dir().join("sel-badge-plain.jpg");
    let picked_err = shoot_env_stderr(
        &[folder],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                &format!(
                    "600:wait:load settled gen 0;700:home;900:pick;{}",
                    thumb_waits_from(1000)
                ),
            ),
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
                &format!(
                    "600:wait:load settled gen 0;700:home;900:pick;1100:home;\
                     1300:shift-right;{}",
                    thumb_waits_from(1400)
                ),
            ),
        ],
        &both,
    );
    for (name, err) in [("picked", &picked_err), ("both", &both_err)] {
        assert!(
            err.contains("wait:load settled gen 0 (satisfied"),
            "the {name} run's `wait:load settled gen 0` never fired — its \
             `home` and `pick` were timed, not gated:\n{err}"
        );
        assert!(
            err.contains("wait:thumb landed idx 2 (satisfied"),
            "the {name} run's thumb waits never fired — the shot was timed, \
             not gated, and the star search can run over a placeholder in \
             one shot and a photograph in the other:\n{err}"
        );
    }
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
/// stalled loop) would be the same vacuity by another route; the writer's
/// own close count (`sidecar writer closed gen 0: 1 pending flushed`,
/// traced by the swap) fails loud on it instead — validator F2, asserted
/// on the app's account since 2026-09-03, where it used to be a measured
/// gap between two drive echoes.
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
        "800:open:{};1100:wait:load settled gen 0;1200:pick;1500:open:{}",
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
    // The pick is made on the SETTLED view, so the 300 ms that has to stay
    // inside the debounce carries no load work: on the Windows debug
    // runner the old 1200 ms pick fired 246 ms BEFORE the settle, which
    // put the re-sort and its full refresh between the mark and the swap —
    // the one variable-cost thing in that window, and a busy loop is
    // exactly what makes a Slint timer fire late.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the pick was \
         timed, not gated:\n{stderr}"
    );
    // The swap must have landed INSIDE the debounce window, or the writer's
    // own timer wrote the sidecar before the swap and the flush assertion
    // below is testing nothing (validator F2). Asserted on the WRITER's own
    // account, not on two drive-echo timestamps: the swap closes session
    // A's writer by hand and traces how many writes that close had to
    // flush. One pick, 300 ms into a 700 ms debounce, is exactly one; a
    // schedule that slipped past the debounce (Slint timers fire late under
    // a stalled loop — Windows CI has measured ~60% slower runs) reports
    // zero, the same loud retune signal with nothing left for timer drift
    // to falsify. `gen 0` is session A: the 800 ms bogus-path open failed
    // and closed nothing.
    let closed = stderr
        .lines()
        .find(|l| l.contains("sidecar writer closed gen 0:"))
        .unwrap_or_else(|| panic!("the swap never closed session A's writer:\n{stderr}"));
    assert!(
        closed.contains(": 1 pending flushed"),
        "the writer had nothing pending when the swap closed it — the \
         pick's 700 ms debounce had already fired, so the flush assertion \
         below would be vacuous; retune the schedule ({closed}):\n{stderr}"
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
    // (index and claim) now point AWAY from B's expected outcome.
    // `wait:load settled gen 1` is what holds the shutter until B's two
    // files have loaded and RE-SORTED; the trailing `grid` (a zoom key
    // never claims the cursor) is the harmless backstop the shutter used
    // to ride alone. B is `gen 1` — one successful open — so the token
    // names B's settle and cannot be satisfied by A's. The schedule is
    // unchanged: a wait polls from its OWN timestamp, so 8400 is still
    // 8400 and the mark (3381 ms on the Windows debug runner, 2017 ms on
    // the Linux release one) is already there when it does; only a
    // slower session B moves the `grid` behind it.
    let script = format!(
        "1000:right;2000:open:{};8400:wait:load settled gen 1;8500:grid",
        dir_b.display()
    );
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
    assert!(
        stderr.contains("wait:load settled gen 1 (satisfied"),
        "the `wait:load settled gen 1` step never fired — the hold before \
         the shot was timed, not gated:\n{stderr}"
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
    // Behind the settle wait the first half is definitional rather than
    // lucky: `metadata_complete()` IS `thumbs_done >= labels.len()`
    // (state.rs), and `thumbs_done` is what the status counts — so a
    // settled generation cannot report fewer. It stays as the anti-vacuity
    // reading of the wait, not as an independent race.
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
/// grid from y=61). On Windows there is no in-window MenuBar to click:
/// `i-slint-backend-winit` reports `supports_native_menu_bar()` there
/// (the `muda` dependency), so the menus are the OS menu bar, outside the
/// client area and unreachable by a dispatched pointer event — not a font
/// drift, which is what this comment used to say. The in-window bar these
/// coordinates address is the `fluent` style's, 40 px tall, on the Linux
/// runners (item geometry within it does follow the platform's font
/// metrics, DejaVu Sans there).
///
/// The focus machinery under test is platform-independent Slint core, and
/// every non-menu strand still runs on Windows. Each menu test asserts an
/// intermediate state that FAILS LOUDLY if a click missed its target, so
/// no drift can make one pass vacuously.
fn menu_clicks_are_calibrated() -> bool {
    !cfg!(windows)
}

/// Where a self-reporting element (`iptc field N`, `copy card`, `clip
/// buttons`) was last laid out BEFORE the trace line `before` — the app's
/// own report (`<what> laid out at X,Y size WxH`, the same mark a script's
/// `wait:` gates a click on), in window-logical px.
fn laid_out_rect(stderr: &str, what: &str, before: &str) -> (f32, f32, f32, f32) {
    let head = stderr
        .split_once(before)
        .unwrap_or_else(|| panic!("no `{before}` line in stderr:\n{stderr}"))
        .0;
    // Anchored on the trace prefix's `]`: a script that `wait:`s on this
    // very text puts the substring in the log too, and the mark is the one
    // that starts the line's message.
    let tag = format!("] {what} laid out at ");
    let geom = head
        .lines()
        .filter_map(|l| l.split_once(&tag))
        .next_back()
        .unwrap_or_else(|| panic!("{what} never reported a layout before `{before}`:\n{stderr}"))
        .1;
    let parse = || -> Option<(f32, f32, f32, f32)> {
        let (pos, size) = geom.split_once(" size ")?;
        let (x, y) = pos.split_once(',')?;
        let (w, h) = size.trim().split_once('x')?;
        Some((
            x.parse().ok()?,
            y.parse().ok()?,
            w.parse().ok()?,
            h.parse().ok()?,
        ))
    };
    parse().unwrap_or_else(|| panic!("malformed field-layout trace: {geom:?}"))
}

/// [`laid_out_rect`] for the IPTC panel's field row `i`.
fn iptc_field_rect(stderr: &str, i: usize, before: &str) -> (f32, f32, f32, f32) {
    laid_out_rect(stderr, &format!("iptc field {i}"), before)
}

/// Assert that a `click:<element>` step resolved, and that the point it
/// resolved to is inside the rectangle the app reported for that element.
///
/// The calibration guard, now read off the harness's own echo (`drive ptr
/// click X,Y (<element>)`) instead of a coordinate repeated in the test.
/// A scripted point was measured on ONE platform's layout: the Windows
/// runner draws no in-window menu bar (the OS one lives outside the client
/// area), so every in-window y sits ~40 px higher there and three of these
/// clicks landed 43 px below the Title field's centre (issue #70). What is
/// left to check is that the click happened at all and against a real
/// laid-out rectangle — a missing echo means the element never reported
/// itself and the run was abandoned, which is loud on its own.
///
/// The LAST resolution is the one checked: these scripts click the same
/// field several times, and a rebuild between two clicks moves the
/// rectangle. The anchor is the STEP echo (`drive: click:<element>`), not
/// the pointer echo the click emits afterwards: the rectangle compared is
/// the last one reported BEFORE the step — the one the harness resolved —
/// and the point is the first pointer echo AFTER it, so a relayout the
/// click itself triggers cannot be mistaken for the resolved rectangle
/// (validator 2026-09-02).
fn assert_click_resolved(stderr: &str, element: &str) {
    let step = format!("] drive: click:{element}");
    let step_line = stderr
        .lines()
        .rfind(|l| l.ends_with(&step))
        .unwrap_or_else(|| panic!("no `drive: click:{element}` step in the trace:\n{stderr}"));
    let after = stderr
        .rfind(step_line)
        .map(|at| &stderr[at + step_line.len()..])
        .unwrap_or("");
    let tag = format!(" ({element})");
    let line = after
        .lines()
        .find(|l| l.contains("] drive ptr click ") && l.ends_with(&tag))
        .unwrap_or_else(|| {
            panic!(
                "no `drive ptr click … ({element})` echo after the click:{element} \
                 step — it never resolved:\n{stderr}"
            )
        });
    let point = || -> Option<(f32, f32)> {
        let at = line.split_once("drive ptr click ")?.1;
        let (x, y) = at.split_once(' ')?.0.split_once(',')?;
        Some((x.parse().ok()?, y.parse().ok()?))
    };
    let (cx, cy) = point().unwrap_or_else(|| panic!("malformed click echo: {line:?}"));
    let (x, y, w, h) = laid_out_rect(stderr, element, step_line);
    assert!(
        cx >= x && cx <= x + w && cy >= y && cy <= y + h,
        "the click resolved to ({cx}, {cy}), outside the {element} rectangle \
         the app reported (x {x}..{}, y {y}..{}):\n{stderr}",
        x + w,
        y + h
    );
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
                // Gated, not timed (issue #73), for the reason the eight
                // other focus-family scripts already record: the IPTC rows
                // are REBUILT when the metadata lands, and a rebuild
                // arriving after the K is indistinguishable from the blur
                // this test measures. Free — the one-file fixture settles at
                // 33 ms on the Linux release runner (the only runner that
                // runs this test: `menu_clicks_are_calibrated()` is
                // `!cfg!(windows)`), so the wait is satisfied after 0 ms and
                // the menu-click choreography behind it keeps every authored
                // gap. The margin against `harness::install` is small on
                // that runner and is recorded in ui-grid.md beside the
                // conversion; if the settle ever beat install the wait could
                // never be satisfied, and the failure would be loud
                // (`wait never satisfied`, exit 1), not silent.
                "FASTCULL_DRIVE",
                "3400:wait:load settled gen 0;3500:key:k;4000:dump.k;\
                 4400:click.72,19;4800:click.128,125;\
                 5200:dump.closed;5400:key:g;5800:dump.end",
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the K was timed \
         against the rows rebuild, not gated on it:\n{stderr}"
    );
    // The same fact as an ORDERING, so the gate survives a later re-time.
    let settled_at = stderr.find("load settled gen 0").unwrap_or_else(|| {
        panic!("the view never settled, so a rows rebuild could still follow the K:\n{stderr}")
    });
    let first_key = stderr
        .find("drive: key:k")
        .unwrap_or_else(|| panic!("the `key:k` step never ran:\n{stderr}"));
    assert!(
        settled_at < first_key,
        "the load settled after the K — a rows rebuild landing on top of it \
         reads exactly like the blur this test measures:\n{stderr}"
    );
    // The K really landed in the field (anti-vacuity: panel open and the
    // keyboard NOT on the main scope — the dangerous state is armed).
    let k = qedump(&stderr, "k");
    assert!(
        k.contains("iptc=true")
            && dump_field(k, "focusowner") == "12"
            && k.contains("one2one=true"),
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
        dump_field(closed, "focusowner") == "0",
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
        k.contains("iptc=true") && dump_field(k, "focusowner") == "12",
        "K did not open the panel and focus the keyword field: {k}"
    );
    let closed = qedump(&stderr, "closed");
    assert!(
        closed.contains("iptc=false"),
        "the View > IPTC Panel click missed (panel still open): {closed}"
    );
    assert!(
        dump_field(closed, "focusowner") == "0",
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
                "2400:wait:load settled gen 0;2500:key:k;3000:click.115,19;3400:click.180,93;\
                 3800:dump.about;\
                 4000:key:b;4100:key:a;4200:key:d;4400:key:escape;4800:dump.esc;\
                 5000:key:+;5300:dump.end",
            ),
        ],
        &out,
    );
    // The panel opens BEHIND the load settle (2026-09-03): a rows rebuild
    // the load adds after the panel key is indistinguishable from the
    // blur, menu and swap rebuilds this family counts. Of these six sites
    // only `keyword_enter_commit_still_writes_and_returns_focus` runs on
    // Windows, and it measured the margin there at 1.1 s (settle 1397 ms
    // against a 2500 ms key); the five behind `menu_clicks_are_
    // calibrated()` never measured it at all.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
    );
    let about = qedump(&stderr, "about");
    assert!(
        about.contains("about=true"),
        "the Help > About click missed (dialog never opened): {about}"
    );
    // THE fix: the modal's keyboard steal survived the menu focus restore.
    assert!(
        dump_field(about, "focusowner") == "0",
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
/// pre-fix: the keyboard is dead on the fresh session (the `+` is inert).
/// The mid-edit text is DISCARDED (user decision: no commit-on-destroy) —
/// asserted on disk: no sidecar in either folder.
///
/// KNOWN INTERMITTENT FAILURE, on the DISCARD assertion only (measured
/// 2026-08-30, ~1 group run in 6 on a busy desktop seat; reproduced
/// identically on the unmodified tree, so it is not this change): if the
/// WINDOW is deactivated while the field holds half-typed text — anything
/// else taking focus, which on a developer's seat happens on its own —
/// Slint delivers a real `FocusOut` to the live editor, and its blur
/// handler COMMITS, exactly as a click-away would. A sidecar then appears
/// in folder A and this test fails saying the abandoned text was
/// committed. The signature in the trace is a lone `focus: iptc field N
/// lost` that no `gained` follows and no `focus-keys (…)` precedes,
/// BEFORE the rebuild — the blur commits first, so the rebuild-generation
/// stamp that enforces the discard rule cannot catch it. That is the
/// pre-existing deactivation-commit defect recorded in ui-grid.md (the
/// same one that best explains issue #54's leak), not a regression of the
/// focus reclaim — the keyboard assertions above it stay green when it
/// fires, and QE measured it at 3/10 on this tree against 4/10 on a tree
/// with the reclaim removed, with an identical trace shape. Do NOT quiet
/// this test; when it fails it is telling the truth about a real bug.
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
    // The Title field is clicked BY NAME (`click:iptc field 0`, issue
    // #70), so the point is the app's own rectangle rather than a number
    // measured on one platform's layout. The window size still has to be
    // known — the `wait:` below pins the geometry the panel's width and
    // the grid-cell coordinates were measured in. It is PINNED
    // at the default (`PIN_WINDOW`), not changed: this script used to ask
    // for 1200x800, and a `resize:` is a REQUEST to the compositor, which
    // under load goes unanswered for the life of the run. That, not a slow
    // layout, is the cause of issue #61's flake (17 of 20 runs under six
    // busy cores). Measured on this tree with the old script and six
    // spinners, 9 runs of 10: no `iptc field 0 laid out at 910` ever
    // appears, `geometry at shutter` reads `grid 1140x800`, and the
    // snapshot the app takes 12 s later is 1440 px wide — the window never
    // became 1200 while anything was watching. So the panel's left edge
    // stayed at 1140 instead of 900 (the row itself at 1150 instead of
    // 910, a 240 px shift), and the click at x=1050 fell 90 px short of
    // the panel onto the grid, which the test then reported as "the field
    // never took focus". Asking for the size it already has cannot go
    // unanswered in a way that matters.
    //
    // The `wait:` then gates the click on the Title row's own layout
    // report INCLUDING its x — `at 1150` is where that row is in a 1440 px
    // window and nowhere else — so the run happens at the width the rest
    // of this script's coordinates were measured at, and the row the
    // click resolves against is laid out before it is asked for. If the
    // window is ever some third size, the wait ends the run with that
    // sentence instead of the script proceeding at a width nothing here
    // was measured for. The steps after it keep the gaps written here.
    // THE CONTRACT IS ASSERTED BY ACTING (issue #63): `key:+` 50 ms after
    // the swap, and the grid must zoom. `keysfocus` cannot carry this
    // test — Slint sends a FocusOut on window DEACTIVATION while
    // `WindowInner::focus_item` keeps routing keys to the same scope, so
    // an unfocused window reads `keysfocus=false` with a perfectly live
    // keyboard (proven with no clicks at all: `keysfocus=false`, then a
    // `+` zoomed). Both of this test's recorded reds were that artifact,
    // and gating the dump on `load settled` only widened the exposure by
    // moving it seconds later. A keystroke that ACTS cannot be faked by a
    // deactivation.
    //
    // The `wait:` stays, but only for the LATER dumps: the discard
    // assertions want a settled new session, and `load settled gen 1` is
    // the second folder (`session-gen` counts from 0 for the one on the
    // command line). The keystroke probe fires before it, immediately
    // after the swap, which is where the ownerless window was.
    let drive = format!(
        "{PIN_WINDOW};2400:wait:load settled gen 0;2500:key:i;2600:wait:iptc field 0 laid out at 1150;\
         3000:click:iptc field 0;3200:dump.focused;\
         3400:key:w;3500:key:i;3600:key:p;4000:open:{};\
         4050:key:+;4400:dump.after;\
         4500:wait:load settled gen 1;5200:dump.swapped;\
         5400:key:+;5800:dump.end",
        dir_b.display()
    );
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &drive)],
        &out,
    );
    // The wait really gated the click (a dropped token would silently put
    // the schedule back on the clock that issue #61 is about).
    assert!(
        stderr.contains("wait:iptc field 0 laid out at 1150 (satisfied"),
        "the `wait:` step never fired — the click below was timed, not \
         gated:\n{stderr}"
    );
    // …and the panel OPEN, the one ungated link in this script until
    // 2026-09-03, is behind A's settle: a rows rebuild the load adds after
    // `key:i` looks exactly like the swap's own rebuild that this test is
    // about (settle 1396 ms against the `i` at 2500 on the Windows debug
    // runner — 1.1 s of luck).
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
    );
    // The click resolved against the rectangle the app reported for that
    // row, and landed inside it — before the outcome assertions, so a
    // click that never happened fails as itself.
    assert_click_resolved(&stderr, "iptc field 0");
    // The Title-field click really took focus (anti-vacuity, gate
    // finding: a missed click would type the `p` as a real grid PICK
    // and fail this test later with a false "committed the abandoned
    // text" diagnosis — a miss must be loud and unambiguous). Asserted
    // through the OWNER TOKEN, not `keysfocus`: `focusowner=1` names the
    // Title row positively, where `keysfocus=false` only says "not the
    // main scope" and a deactivated window says that too.
    let focused = qedump(&stderr, "focused");
    assert_eq!(
        dump_field(focused, "focusowner"),
        "1",
        "the Title-field click missed — the Title row never took the \
         keyboard (issue #63): {focused}"
    );
    // THE CONTRACT (issue #41 D3, issue #63): a keystroke 50 ms after the
    // swap ACTS. This is the assertion the whole test exists for, and it
    // is a keystroke rather than a focus reading for the reason above the
    // script.
    let after = qedump(&stderr, "after");
    assert!(
        after.contains("zoom=2"),
        "the first keystroke on the fresh session was DEAD — the keyboard \
         was stranded by the swap (issue #41 D3, the ownerless window of \
         issue #63): {after}"
    );
    // …and the window in which nobody owned the keyboard is closed BY
    // CONSTRUCTION, which is the half a scripted keystroke cannot prove.
    // The probe above is the user's contract but a weak mutant-killer:
    // the deferred claim it races is a zero-length timer, and the drive
    // step that sends the `+` is a timer too, so on an idle machine the
    // claim usually wins anyway (measured: a tree with the reclaim
    // removed still passes that assertion 19 runs in 20). This is the
    // assertion that fails 20/20 on such a tree — the reclaim must be
    // the FIRST claim after the rebuild that destroyed the editor,
    // i.e. in the rebuild's own pass rather than an event loop later.
    let rebuilt = stderr
        .find("iptc rows rebuilt (gen 1)")
        .unwrap_or_else(|| panic!("the swap never rebuilt the panel rows:\n{stderr}"));
    let first_claim = stderr[rebuilt..]
        .split("focus-keys (")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .unwrap_or("<none>");
    assert_eq!(
        first_claim, "rebuild -> keys",
        "the keyboard was not reclaimed in the rebuild's own pass — the \
         first claim after the swap's rows rebuild was `{first_claim}`, \
         so there is an ownerless window again (issue #63):\n{stderr}"
    );
    // The new session really settled before the discard dumps below
    // (issue #63): those read the revert slot and the disk, and a folder
    // still scanning has not finished writing anything.
    assert!(
        stderr.contains("wait:load settled gen 1 (satisfied"),
        "the post-swap `wait:` never fired — the discard assertions below \
         were timed against a session that may still be loading:\n{stderr}"
    );
    // The swap really happened (anti-vacuity).
    let swapped = qedump(&stderr, "swapped");
    assert!(
        swapped.contains("two.ARW"),
        "the open: swap never landed: {swapped}"
    );
    // Still alive once the new session has settled — the deferred claim
    // that follows the swap must not have stranded it afterwards. Acting
    // again, for the same reason.
    let end = qedump(&stderr, "end");
    assert!(
        end.contains("zoom=3"),
        "the keyboard died between the swap and the settled session: {end}"
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
    // The panel opens BEHIND the settle (2026-09-03): a panel built before
    // the metadata lands is rebuilt again when it does, and a load-driven
    // rows rebuild is indistinguishable from the swap's own — the hazard
    // `a_cursor_move_rebuild_keeps_the_keyboard_in_the_field` added this
    // token for. The margin was 1.1 s on the Windows debug runner (settle
    // 1402 ms against the `k` at 2500), i.e. green by luck rather than by
    // construction. `5200:dump.swapped` is deliberately NOT gated on
    // `load settled gen 1`: nothing it asserts needs B's settle (the status
    // names `two.ARW` from the provisional view, `focusowner` is the swap's
    // reclaim), and ui-grid.md records the #63 lesson that moving a
    // keyboard dump behind a wait widens its exposure.
    let drive = format!(
        "2400:wait:load settled gen 0;2500:key:k;3000:key:w;3100:key:i;3200:key:p;4000:open:{};\
         5200:dump.swapped;5400:key:+;5800:dump.end",
        dir_b.display()
    );
    let stderr = shoot_env_stderr(
        &[dir_a.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", &drive)],
        &out,
    );
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
    );
    let swapped = qedump(&stderr, "swapped");
    assert!(
        swapped.contains("two.ARW"),
        "the open: swap never landed: {swapped}"
    );
    // Asserted through the token and, below, by ACTING: `keysfocus` reads
    // false on window deactivation while keys still route (issue #63 —
    // see the harness section of ui-grid.md), so it cannot carry a
    // keyboard-liveness claim. After a swap the reclaim routes to the
    // topmost scope, which is `focusowner=0`.
    assert_eq!(
        dump_field(swapped, "focusowner"),
        "0",
        "the keyboard did not return to the grid after a swap \
         mid-keyword-edit (issue #41 D3): {swapped}"
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
        opened.contains("copy=true") && dump_field(opened, "focusowner") == "-1",
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
        esc2.contains("copy=false") && dump_field(esc2, "focusowner") == "0",
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
///
/// The click is gated (`wait:`) on the overlay actually being UP — any
/// rung, which is what `idx 0 factor` matches — because the surface it
/// must hit exists only then: before the first rung the same point belongs
/// to the fit surface, whose click ALSO claims the keyboard, so the test
/// would go green having exercised the wrong element. Under load that is
/// what the run captured (issue #61: `one2one=false` at the clicked dump).
/// Not the sharp rung: a debug-build 50 MP decode on a loaded machine
/// legitimately takes tens of seconds (the recorded reason the M1 tests
/// are release-only), and the claim under test is the overlay's, not the
/// top rung's.
///
/// The click lands at 800,500 — inside the image rect of even the smallest
/// rung the overlay can show (the 320 px thumb, centred) and deliberately
/// OFF its centre, so the re-centre it produces is visible in `pan`. That
/// is the assertion that says the press reached the overlay's own
/// TouchArea: a click that fell through to the cell behind it would claim
/// the keyboard too, and leave the pan at dead centre.
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
                "2000:wait:idx 0 factor;2500:key:k;3000:dump.k;\
                 3400:click.800,500;3800:dump.clicked;4000:key:g;4400:dump.end",
            ),
        ],
        &out,
    );
    // The wait really gated the click (see the session-swap test).
    assert!(
        stderr.contains("wait:idx 0 factor (satisfied"),
        "the `wait:` step never fired — the click below was timed, not \
         gated:\n{stderr}"
    );
    // K parked the keyboard in the keyword field (the stranded-adjacent
    // state), all at 1:1.
    let k = qedump(&stderr, "k");
    assert!(
        dump_field(k, "focusowner") == "12"
            && k.contains("one2one=true")
            && k.contains("iptc=true"),
        "K did not focus the keyword field at 1:1: {k}"
    );
    // The press really landed on the OVERLAY (not on the cell behind it):
    // only that surface re-centres, and the click was off centre.
    let clicked = qedump(&stderr, "clicked");
    assert_ne!(
        dump_field(clicked, "pan"),
        "0.5000,0.5000",
        "the loupe click did not re-centre — it missed the zoom overlay's \
         own surface, so the focus claim below is some other element's: \
         {clicked}"
    );
    // The click on the zoomed image claimed the keyboard back…
    //
    // If this fails while the assertion above passed, the click DID reach
    // the overlay and the CLAIM is what failed — the shipped
    // `keys.focus()` in the overlay's `clicked` handler did not stick,
    // which is issue #64's family (a focus claim made while the item tree
    // is being rebuilt under the same dispatch). Seen once in a full debug
    // suite, with the panel open and a soft rung up; the test is telling
    // the truth there and must not be quieted.
    assert!(
        dump_field(clicked, "focusowner") == "0" && clicked.contains("one2one=true"),
        "a 1:1 loupe click did not claim the keyboard (issue #41 defense \
         in depth; the re-centre above proves the click reached the \
         overlay, so this is the claim failing — issue #64's family): \
         {clicked}"
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
        dump_field(zoomed, "focusowner") == "0",
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
                "2400:wait:load settled gen 0;2500:key:k;3000:key:o;3100:key:k;\
                 3300:key:return;\
                 3700:dump.committed;3900:key:+;4200:dump.end",
            ),
        ],
        &out,
    );
    // The panel opens BEHIND the load settle (2026-09-03): a rows rebuild
    // the load adds after the panel key is indistinguishable from the
    // blur, menu and swap rebuilds this family counts. Of these six sites
    // only `keyword_enter_commit_still_writes_and_returns_focus` runs on
    // Windows, and it measured the margin there at 1.1 s (settle 1397 ms
    // against a 2500 ms key); the five behind `menu_clicks_are_
    // calibrated()` never measured it at all.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
    );
    let committed = qedump(&stderr, "committed");
    assert!(
        dump_field(committed, "focusowner") == "0",
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
        opened.contains("copy=true") && dump_field(opened, "focusowner") == "-1",
        "Ctrl+E did not open the copy dialog with its own key scope: {opened}"
    );
    let closed = qedump(&stderr, "closed");
    assert!(
        closed.contains("copy=false") && dump_field(closed, "focusowner") == "0",
        "Esc did not close the dialog and hand the keyboard back: {closed}"
    );
    assert!(
        qedump(&stderr, "end").contains("zoom=2"),
        "the `+` after the dialog closed was dead:\n{stderr}"
    );
}

/// Issue #63 FAIL-1 (validator finding 2026-08-30): a rows rebuild that is
/// NOT triggered by the editor's own blur — here a cursor move, the
/// commonest one in real use as sidecars land — used to strand the
/// keyboard, 10 runs in 10.
///
/// The mechanism, and why nothing else in the suite catches it: a Slint
/// repeater does not tear its children down when the model is replaced,
/// they die at its next update. So the DOOMED row instance is still alive
/// and still watching `iptc-refocus-row`, and its `changed want-refocus`
/// runs first — it consumed the flag in the rebuild's own millisecond,
/// focused itself, cleared the flag, and then died. The recreated row saw
/// nothing, and `focus-owner` still read that row while no element owned
/// the keyboard at all. The blur-triggered path hid it: there the commit
/// runs inside the blur, so the timing differs. The fix stamps the flag
/// with the item-tree generation it was armed for, and a row claims only
/// if it was BORN for that generation.
///
/// Asserted by ACTING, three keystrokes deep, because the previous probes
/// for this family asserted only the DISK — and a dead keyboard satisfies
/// "no sidecar was written" perfectly.
///
/// KNOWN INTERMITTENT, inherited: this test leaves a field focused with
/// half-typed text, so it carries the same window-deactivation exposure
/// as `session_swap_mid_field_edit_discards_and_keeps_the_keyboard` (see
/// that test's banner). The fingerprint appeared ~2 times in 35 runs of
/// the equivalent probe — `Revert: … on 1 image(s)` with ★0 and a stale
/// owner token, from a lone `focus: … lost` that no `gained` follows.
/// That is the pre-existing deactivation-commit defect, not this change:
/// QE caught a release-idle instance where the `lost` arrived 28 ms after
/// the keystroke and the rebuild only afterwards, so the blur came from
/// outside the app and beat the rebuild entirely. Do NOT quiet it — the
/// assertion below names it, so a run that hits it says #68 instead of
/// blaming the reclaim.
///
/// AND DO NOT ASSUME A RED RUN IS THAT ONE. This test went red on CI at
/// v0.13.0 and the cause was the reclaim after all: the arm timers beat
/// the repeater's own update, the recreated rows were born into an
/// already-armed flag, and `changed want-refocus` cannot fire for a
/// value that was already true at birth (see the owner-invariant section
/// of ui-grid.md). The two look alike in a dump — a committed field and
/// a dead keyboard both leave `revert=…` standing — and they are told
/// apart only in the trace: deactivation is a lone `focus: … lost`
/// BEFORE the rebuild with no claim before it, the reclaim residual is a
/// rebuild with no claim AFTER it. Here the `revert` line is this
/// script's own seeding Enter and appears in every green run too.
#[test]
fn a_cursor_move_rebuild_keeps_the_keyboard_in_the_field() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-rowgen");
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["a.ARW", "b.ARW"] {
        place_fixture(&raws_dir().join("A1_full_compressed.ARW"), &dir.join(name));
    }
    let out = out_dir().join("focus-rowgen.jpg");
    // Seed b.ARW with a Title first (3000-3600) so that moving the cursor
    // onto it later really CHANGES a row and rebuilds the model — the
    // whole point is a rebuild the focused editor did not cause. Then
    // focus a.ARW's Title, type, and move the cursor with a nav token
    // (which bypasses focus, exactly as a sidecar landing would).
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                &format!(
                    "{PIN_WINDOW};2400:wait:load settled gen 0;2500:key:i;2600:wait:iptc field 0 laid out at 1150;\
                     3000:right;3300:click:iptc field 0;3500:key:z;3600:key:return;\
                     4000:left;4400:click:iptc field 0;4600:key:q;\
                     4900:right;4950:wait:row 0 (gen 4);5100:dump.rebuilt;\
                     5300:key:w;5500:key:return;5800:key:y;6200:dump.after;\
                     6500:click:iptc field 0;6700:key:v;7000:select-all;\
                     7300:dump.mixed;7500:key:u;7700:key:return;8100:dump.mixedafter"
                ),
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("wait:iptc field 0 laid out at 1150 (satisfied"),
        "the `wait:` never fired — the clicks were timed, not gated:\n{stderr}"
    );
    // …and the keys after the rebuild are gated on the RECLAIM, not on a
    // timestamp (issue #69): the dump and the `w` used to fire 200 and
    // 400 ms after the cursor move, and on a seat lagging past ~400 ms a
    // frame they landed inside the gap between the rebuild and the row's
    // claim — the keystrokes went nowhere and the test blamed the
    // reclaim. `gen 4` is what makes that wait mean the claim from THIS
    // rebuild: the mark is `focus-keys (row 0 (gen K))` and K is
    // `iptc-rebuild-gen` at the row's birth, i.e. the number of
    // content-changing rows rebuilds so far. This script forces exactly
    // four before the move (panel open, the seeding Enter, `left`,
    // `right`), and a wait cannot ask for the NEXT occurrence of a mark
    // it has already seen — the "put what differs into the mark" idiom of
    // ui-grid.md's harness section, `wait:load settled gen 1` being the
    // other instance. If a future edit adds or removes a rebuild before
    // the move, this wait is never satisfied and the app ends the run
    // naming the substring: re-read K from the trace, do not delete the
    // wait.
    // A `wait never satisfied: row 0 (gen 4)` has TWO readings and the
    // trace tells them apart: either the script's rebuild count changed
    // (there IS a `row 0 (gen N)` claim with another N — K is wrong,
    // re-read it), or no claim came at all (the trace shows the arms,
    // `rebuild -> row 0` / `restore -> row 0`, and no `row 0 (gen N)`
    // anywhere after them) — which is the reclaim itself failing, the
    // pre-existing #63/#68 family, seen once in 80 runs behind a 5.9 s
    // load settle. The second is a real defect and must not be quieted by
    // moving the wait.
    // K = 4 is a property of the script only if the panel opens AFTER the
    // metadata landed: a rebuild the load adds after `key:i` would shift
    // every later generation by one and turn the wait below into a 30 s
    // red on a slow runner. The script waits for `load settled gen 0`
    // before opening the panel, and this guard keeps that wait from being
    // tidied away without the reason on record (validator 2026-09-02).
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the panel opened before the load settled, so the rebuild count \
         below is not the script's:\n{stderr}"
    );
    assert!(
        stderr.contains("wait:row 0 (gen 4) (satisfied"),
        "the reclaim `wait:` never fired — the keys after the rebuild were \
         timed, not gated (issue #69), or the rebuild count before the \
         cursor move has changed:\n{stderr}"
    );
    assert_click_resolved(&stderr, "iptc field 0");
    // The cursor move really rebuilt the rows (anti-vacuity): without the
    // seeded Title the two images look identical to the panel, no model is
    // replaced, and this test would prove nothing.
    let after_move = stderr
        .rfind("drive: right")
        .map(|i| &stderr[i..])
        .unwrap_or("");
    assert!(
        after_move.contains("iptc rows rebuilt"),
        "the cursor move did not rebuild the panel rows — the seeded \
         Title is missing, so there is no rebuild to survive:\n{stderr}"
    );
    // Which defect a missing claim IS (issue #68 vs issue #63). The
    // editor losing the keyboard BEFORE the rebuild, with nothing having
    // claimed it, is the window being deactivated mid-edit — the blur
    // commits the half-typed text and there is no editor left for the
    // rebuild to rescue. That is a real defect and this still FAILS, but
    // it must fail under its own name: the run never reached the property
    // below, and reading it as a reclaim regression sends the next reader
    // to the wrong mechanism (it nearly did, on the CI red at v0.13.0).
    // The window runs from the click that focused Title to the cursor
    // move (validator 2026-09-01: from `key:q` it missed a blur landing
    // between the click and the first character, which loses the `q`
    // silently and lets the run pass). Nothing but a deactivation can
    // take the keyboard from the editor in there.
    let click_to_move = stderr
        .find("drive: key:q")
        .map(|q| stderr[..q].rfind("drive: click:").unwrap_or(q))
        .zip(stderr.rfind("drive: right"))
        .filter(|(from, mv)| from < mv)
        .map(|(from, mv)| &stderr[from..mv])
        .unwrap_or("");
    assert!(
        !click_to_move.contains("focus: iptc field 0 lost"),
        "the Title editor lost the keyboard between the click that \
         focused it and the cursor move — the window was deactivated \
         mid-edit and the blur committed the half-typed text (issue #68). \
         Not this test's property, and not a reclaim failure:\n{stderr}"
    );
    // The RECREATED row took the keyboard, not the doomed instance.
    assert!(
        after_move.contains("row 0 (gen"),
        "no row claimed the keyboard after the rebuild — either the flag \
         was consumed by the dying instance, or the recreated row was \
         born into an already-armed flag and never saw a `changed` edge \
         (issue #63 FAIL-1 and its 2026-09-01 CI residual):\n{stderr}"
    );
    let rebuilt = qedump(&stderr, "rebuilt");
    assert_eq!(
        dump_field(rebuilt, "focusowner"),
        "1",
        "the Title row does not own the keyboard after the rebuild: {rebuilt}"
    );
    // THE CONTRACT, by acting: type into the field that came back, commit
    // it, and mark with the key the grid gets afterwards. On the pre-fix
    // tree all three are dead and nothing marks.
    let after = qedump(&stderr, "after");
    assert!(
        after.contains("★1"),
        "the typing, the Enter and the `y` after the cursor-move rebuild \
         were all dead although a row claimed (asserted above). Compare \
         the `row 0 (gen` claim's time with `drive: key:w` in the trace: \
         a claim AFTER the keys is this script's fixed-time keys falling \
         inside the reclaim gap on a seat lagging past ~400 ms per frame \
         (issue #69, 1 in 20 under six spinners plus a build loop in \
         debug); a claim BEFORE them that still left the keys dead is a \
         real strand (issue #63 FAIL-1): {after}\n{stderr}"
    );
    // THE SECOND REBUILD SHAPE, which is how QE reproduced the same
    // defect: no cursor move at all — `select-all` grows the batch, the
    // Title row goes ‹multiple values› and the model is replaced for that
    // reason alone. The two shapes reach the rebuild by different routes
    // and both have to be covered; neither is exotic, since any sidecar
    // landing for the batch does the same thing.
    let mixed = qedump(&stderr, "mixed");
    assert_eq!(
        dump_field(mixed, "selected"),
        "2",
        "select-all did not grow the batch, so the row never went mixed \
         and there is no second rebuild to survive:\n{stderr}"
    );
    assert_eq!(
        dump_field(mixed, "focusowner"),
        "1",
        "the Title row does not own the keyboard after a mixed-value \
         rebuild (issue #63 FAIL-1, QE's shape): {mixed}"
    );
    // Acting again: typing and Enter must commit across the grown batch,
    // which only a live editor can do.
    assert!(
        dump_text(qedump(&stderr, "mixedafter"), "revert").contains("2 image(s)"),
        "the keyboard was stranded by a mixed-value rebuild — the typing \
         and the Enter after it committed nothing (issue #63 FAIL-1):\n{stderr}"
    );
}

/// Issue #63 (QE finding 2026-08-30): a menu ITEM activated while a panel
/// FIELD ROW holds half-typed text used to strand the keyboard outright —
/// 5 runs in 5. The chain is three shipped rules colliding: opening the
/// menu blurs the field, the blur COMMITS it (G7), the commit rebuilds
/// the field rows and destroys the editor — and then the MenuBar restores
/// focus to that destroyed item, after the activation has returned, where
/// no synchronous reclaim can undo it. View > Filter Bar is the probe
/// because it queues no claim of its own, unlike View > IPTC Panel.
///
/// Asserted by ACTING, twice over, and deliberately not through
/// `keysfocus` (see the harness notes in ui-grid.md): Enter must commit
/// and hand the keyboard to the grid, and the `y` after it must MARK the
/// photo. On the pre-fix tree both keys die and the mark count stays 0.
/// The keyboard landing back in the FIELD rather than on the grid is the
/// point of the fix, so the probe cannot be a bare `y` — that would type
/// a `y` into the Title, which is correct behaviour and marks nothing.
#[test]
fn a_menu_item_over_a_focused_field_row_keeps_the_keyboard() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-menurow");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-menurow.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                &format!(
                    "{PIN_WINDOW};2400:wait:load settled gen 0;\
                     2500:key:i;2600:wait:iptc field 0 laid out at 1150;\
                     3000:click:iptc field 0;3200:key:a;3300:key:b;\
                     3600:click.72,19;4000:click.128,157;\
                     4300:wait:menu -> row 0;4400:dump.menu;\
                     4600:key:return;4900:key:y;5300:dump.after"
                ),
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("wait:iptc field 0 laid out at 1150 (satisfied"),
        "the `wait:` never fired — the field click was timed, not gated:\n{stderr}"
    );
    // #69's shape, menu flavour: the keys after the item activation wait
    // for the claim that activation causes, not for the clock. The mark is
    // `menu -> row 0` — emitted once, only after the activation — because
    // the row's own `row 0 (gen 2)` claim carries the SAME generation as
    // the one the menu-open blur produced, so waiting on it would be
    // satisfied by the earlier mark and gate nothing (validator
    // 2026-09-02).
    assert!(
        stderr.contains("wait:menu -> row 0 (satisfied"),
        "the menu item's own claim never came, so the keys after it ran on \
         the clock:\n{stderr}"
    );
    // The panel opens BEHIND the load settle (2026-09-03): a rows rebuild
    // the load adds after the panel key is indistinguishable from the
    // blur, menu and swap rebuilds this family counts. Of these six sites
    // only `keyword_enter_commit_still_writes_and_returns_focus` runs on
    // Windows, and it measured the margin there at 1.1 s (settle 1397 ms
    // against a 2500 ms key); the five behind `menu_clicks_are_
    // calibrated()` never measured it at all.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
    );
    assert_click_resolved(&stderr, "iptc field 0");
    // The menu really acted (anti-vacuity): the filter bar toggled, so
    // the clicks at 72,19 and 128,157 hit the View menu and its item.
    // Without this a missed menu click would leave the keyboard happily
    // in the field and the assertions below would pass having tested
    // nothing.
    let menu = qedump(&stderr, "menu");
    assert_eq!(
        dump_field(menu, "focusowner"),
        "1",
        "after the menu item the keyboard is not in the Title row — \
         either the menu click missed, or the row was left stranded \
         (issue #63):\n{stderr}"
    );
    // THE CONTRACT, by acting: Enter commits and returns to the grid…
    // …and `y` marks the photo there. Both keys die on the pre-fix tree.
    let after = qedump(&stderr, "after");
    assert!(
        after.contains("★1"),
        "the keyboard was stranded by the menu activation — Enter and \
         the `y` after it were both dead (issue #63): {after}"
    );
}

/// Issue #63 FAIL-3 (validator finding 2026-08-30): a menu opened over a
/// focused field row and then DISMISSED without choosing anything.
///
/// It is the nastiest shape in the family because nothing announces it:
/// opening the menu blurs the field, the blur COMMITS it (G7), the commit
/// rebuilds the rows and destroys the editor — and then the menu is
/// dismissed and Slint's MenuBar restores focus to the destroyed
/// instance. No `activated` fires, so the `menu-activated` claim never
/// runs, and Slint 1.17 exposes no menu open/dismiss callback to hang one
/// on. Measured dead 10 runs in 10 before the fix.
///
/// What rescues it is not a new claim but the DEFERRAL of the rebuild
/// reclaim's flag write (FAIL-1's fix): armed one event-loop iteration
/// late, it lands on a row that is alive and can actually take focus, and
/// it survives the restore. Esc is the probe because it needs no
/// coordinates beyond the menu-bar click; the click-elsewhere and
/// click-the-menu-bar-again routes measure the same, 5/5 each.
/// The keys after the Esc stay on the clock, deliberately (validator
/// 2026-09-02): a dismissed menu produces NO claim mark to wait for — the
/// rescue is the deferred flag write, whose only trace is the `row 0
/// (gen 2)` claim the menu-open blur already emitted — so there is no
/// mark that differs, and `wait:` would be satisfied by the past one. The
/// menu-item test has one (`menu -> row 0`) and waits on it. Linux-only
/// either way.
#[test]
fn a_dismissed_menu_over_a_focused_field_row_keeps_the_keyboard() {
    if !has_display() || !menu_clicks_are_calibrated() {
        eprintln!("skipped: no display or uncalibrated menu geometry");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("focus-menudismiss");
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("focus-menudismiss.jpg");
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                &format!(
                    "{PIN_WINDOW};2400:wait:load settled gen 0;\
                     2500:key:i;2600:wait:iptc field 0 laid out at 1150;\
                     3000:click:iptc field 0;3200:key:q;\
                     3600:click.72,19;4000:key:escape;4400:dump.dismissed;\
                     4600:key:return;4900:key:y;5300:dump.after"
                ),
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("wait:iptc field 0 laid out at 1150 (satisfied"),
        "the `wait:` never fired — the field click was timed, not gated:\n{stderr}"
    );
    // The panel opens BEHIND the load settle (2026-09-03): a rows rebuild
    // the load adds after the panel key is indistinguishable from the
    // blur, menu and swap rebuilds this family counts. Of these six sites
    // only `keyword_enter_commit_still_writes_and_returns_focus` runs on
    // Windows, and it measured the margin there at 1.1 s (settle 1397 ms
    // against a 2500 ms key); the five behind `menu_clicks_are_
    // calibrated()` never measured it at all.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
    );
    assert_click_resolved(&stderr, "iptc field 0");
    // The menu really opened and the field's blur really rebuilt the rows
    // (anti-vacuity): without both there is nothing for the dismiss to
    // strand, and this test would pass on a build where the menu click
    // missed entirely.
    let after_menu = stderr
        .find("drive: click.72,19")
        .map(|i| &stderr[i..])
        .unwrap_or("");
    assert!(
        after_menu.contains("iptc rows rebuilt"),
        "opening the menu did not blur-and-rebuild the panel rows — there \
         is no destroyed editor to recover from:\n{stderr}"
    );
    let dismissed = qedump(&stderr, "dismissed");
    assert_eq!(
        dump_field(dismissed, "focusowner"),
        "1",
        "after the menu was dismissed the Title row does not own the \
         keyboard (issue #63 FAIL-3): {dismissed}"
    );
    // THE CONTRACT, by acting: Enter commits and hands the keyboard to
    // the grid, and the `y` marks there. Both die on the pre-fix tree.
    let after = qedump(&stderr, "after");
    assert!(
        after.contains("★1"),
        "the keyboard was stranded by dismissing a menu over a focused \
         field — the Enter and the `y` after it were both dead (issue \
         #63 FAIL-3): {after}"
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
                "2400:wait:load settled gen 0;2500:key:k;3000:key:w;3300:click.72,19;\
                 3700:click.128,157;\
                 4100:dump.toggled;4300:key:return;4700:dump.after;\
                 4900:key:+;5200:dump.end",
            ),
        ],
        &out,
    );
    // The panel opens BEHIND the load settle (2026-09-03): a rows rebuild
    // the load adds after the panel key is indistinguishable from the
    // blur, menu and swap rebuilds this family counts. Of these six sites
    // only `keyword_enter_commit_still_writes_and_returns_focus` runs on
    // Windows, and it measured the margin there at 1.1 s (settle 1397 ms
    // against a 2500 ms key); the five behind `menu_clicks_are_
    // calibrated()` never measured it at all.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
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
        dump_field(toggled, "focusowner") == "12",
        "the field lost the keyboard across the filter-bar toggle: {toggled}"
    );
    assert!(
        dump_field(qedump(&stderr, "after"), "focusowner") == "0",
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
                "2400:wait:load settled gen 0;2500:key:k;3000:click.22,19;3400:click.80,93;\
                 3800:dump.opened;\
                 4000:key:x;4300:key:escape;4700:dump.closed;4900:key:+;\
                 5200:dump.end",
            ),
        ],
        &out,
    );
    // The panel opens BEHIND the load settle (2026-09-03): a rows rebuild
    // the load adds after the panel key is indistinguishable from the
    // blur, menu and swap rebuilds this family counts. Of these six sites
    // only `keyword_enter_commit_still_writes_and_returns_focus` runs on
    // Windows, and it measured the margin there at 1.1 s (settle 1397 ms
    // against a 2500 ms key); the five behind `menu_clicks_are_
    // calibrated()` never measured it at all.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened \
         on the clock:\n{stderr}"
    );
    // The dialog opened via the real menu and its scope owns the keys.
    let opened = qedump(&stderr, "opened");
    assert!(
        opened.contains("copy=true"),
        "the File > Copy Picks click missed (dialog never opened): {opened}"
    );
    assert!(
        dump_field(opened, "focusowner") == "-1",
        "the main key scope holds the keys behind the copy dialog — N/Y \
         would fire at the hidden grid: {opened}"
    );
    // Esc closed the dialog and the keyboard returned…
    let closed = qedump(&stderr, "closed");
    assert!(
        closed.contains("copy=false") && dump_field(closed, "focusowner") == "0",
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
        "blind typing behind the copy dialog produced a sidecar: \
         {sidecars:?}\n{stderr}"
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
/// loaded 8-core laptop, so under contention (or on a CI runner, which
/// the audit of 2026-09-04 measured at 4 vCPU where this line said 2)
/// the app exits 1 at the cap before the shutter can fire. The number
/// that carries the decision is 58.5 against 60 — a 1.5 s margin
/// measured on a machine with TWICE the runner's cores; correcting 2 to
/// 4 halves the shortfall without giving the margin back. The debug
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
/// One app run, three phases at a resolved 1:1, GATED on the sharp
/// render's own mark instead of on a lead time long enough for the
/// slowest profile. `wait:loupe idx 0 factor` is satisfied only by the
/// full-res arm: the rungs below it say `loupe soft idx 0 factor` and
/// `loupe thumb idx 0 factor`, which do not contain the substring, and
/// the trailing ` factor` closes the `idx 0` prefix against `idx 10`.
/// The wait step is PROFILE-SPLIT (2026-09-04, validator F5), the shape
/// `panel_toggle_at_one_to_one_reanchors_the_crop` already uses. DEBUG
/// keeps it at 20 s: the harness's 30 s cap runs from the STEP
/// (harness.rs `WAIT_CAP`) and a debug-profile full-res adoption lands at
/// 26-40 s on the Windows CI runner (30.3 s in this test's own run,
/// measured 2026-09-02), so the cap has to reach 50 s where the fixed
/// 45 s lead it replaced reached only 45. RELEASE puts the same step at
/// 1.5 s, because there the sharp mark lands in under half a second
/// (381, 454 and 457 ms across three release runs on this seat,
/// 2026-09-04, each wait then `satisfied after 0 ms`): its cap still
/// reaches 31.5 s, ~31 s of headroom over a decode that takes 0.45 s, and
/// the release run stops spending 18.5 s of dead clock: the script's last
/// step moves from 22.2 s to 3.7 s and the test measured 4.0 s of libtest
/// time in all three runs. What
/// each profile's wait covers is therefore different — debug waits for a
/// decode that may genuinely take half a minute, release waits for one
/// that is already done — and the schedule behind it is the same in both,
/// because the steps after a wait keep their gaps from the WAIT's
/// timestamp and everything below is gaps. The end of the script is
/// `sharp + 2.2 s` in both profiles, which is what the shutter's 60 s
/// readiness cap gets back: it still waits for idx 1's texture exactly as
/// before, from an earlier start. The `predrag` guard stays as the proof
/// the wait meant what it says:
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
    // Every timestamp after the wait is rebased on the moment it fires,
    // so the numbers below are gaps, not offsets. Phase 1: slow drag
    // right+down by (100, 40). Phase 2: the flick (5 events, 16 ms
    // apart — the velocity ring buffer needs real timing). Phase 3:
    // arrow mid-"decay".
    //
    // The two forms are ONE schedule with two bases: the wait's step
    // (20 s debug, 1.5 s release — see the doc comment for why each) and
    // then the identical gaps +100/+150/+250/+350/+450/+550, +700/+716/
    // +732/+748/+764/+780, +880/+1180, +1300/+1400/+2200. Every one of
    // those is physics some assertion below reads — the 16 ms flick
    // cadence feeds the velocity ring buffer, the +100/+400 ms pair after
    // release is the fling test, the +900 ms after the arrow is the
    // carried-centre test. Edit the two consts together.
    #[cfg(debug_assertions)]
    const DRIVE: &str = "20000:wait:loupe idx 0 factor;\
         20100:dump.predrag;20150:press.700,450;20250:move.750,470;20350:move.800,490;\
         20450:release.800,490;20550:dump.dragged;\
         20700:press.700,450;20716:move.800,520;20732:move.900,590;20748:move.1000,660;\
         20764:move.1100,730;20780:release.1100,730;\
         20880:dump.afterfling1;21180:dump.afterfling2;\
         21300:right;21400:dump.afternav;22200:dump.late";
    #[cfg(not(debug_assertions))]
    const DRIVE: &str = "1500:wait:loupe idx 0 factor;\
         1600:dump.predrag;1650:press.700,450;1750:move.750,470;1850:move.800,490;\
         1950:release.800,490;2050:dump.dragged;\
         2200:press.700,450;2216:move.800,520;2232:move.900,590;2248:move.1000,660;\
         2264:move.1100,730;2280:release.1100,730;\
         2380:dump.afterfling1;2680:dump.afterfling2;\
         2800:right;2900:dump.afternav;3700:dump.late";
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", DRIVE)],
        &out,
    );
    let predrag = qedump(&stderr, "predrag");
    let dragged = qedump(&stderr, "dragged");
    let fling1 = qedump(&stderr, "afterfling1");
    let fling2 = qedump(&stderr, "afterfling2");
    let afternav = qedump(&stderr, "afternav");
    let late = qedump(&stderr, "late");
    // The gate really fired: a dropped or misspelled token is a script
    // quietly back on the clock, and the phases below would then run
    // 100 ms after launch instead of 100 ms after the sharp render.
    assert!(
        stderr.contains("wait:loupe idx 0 factor (satisfied"),
        "the `wait:loupe idx 0 factor` step never fired — the pointer \
         work was timed, not gated:\n{stderr}"
    );
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
        "full-res not resolved when the wait let the drag through — no pan \
         range, so every assertion below would be vacuous:\n{predrag}"
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
    // The first tap. The warm-landing assertion below splits the log at
    // this step's own echo rather than at this number, so the script and
    // the window it is judged over cannot drift apart however late the
    // step fires.
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
    //
    // The split is the tap's OWN echo, not its scripted timestamp (CI
    // audit item 6, 2026-09-03): the harness prints `drive: right` before
    // it dispatches the key, so everything after that byte offset is the
    // tap window and everything before it the cold start, whenever the
    // step actually fired. Comparing trace clocks against FIRST_TAP_MS
    // instead makes the window a guess about the runner — the Windows
    // debug runner fired this first tap at 9480 ms, 1480 ms late, and on
    // a release runner that lost the same 1.5 s a startup thumb at
    // 8100 ms would have been read as a tap's. The failgate test splits
    // its log the same way (`match_indices("drive: end").nth(1)`).
    if !cfg!(debug_assertions) {
        let first_tap = stderr
            .find("drive: right")
            .unwrap_or_else(|| panic!("the first tap never ran:\n{stderr}"));
        let late_thumb = stderr[first_tap..]
            .lines()
            .find(|l| l.contains("loupe thumb idx"));
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
///
/// It also pins the NOTCH SIZE itself (issue #13). "One notch = 60
/// logical px" is winit's line-delta conversion, and the accumulator in
/// `main.slint` is written against that number: 59 px must fire nothing
/// and the 60th px must fire a stop, which is what `d1`/`w1` assert.
/// The number was comment-only until then — a Slint upgrade that changed
/// the conversion would have made every wheel notch a fraction of a stop
/// with nothing to say so. The same pair pins the residue carry (the
/// accumulator subtracts 60 rather than zeroing) from the other side of
/// the `w3` half-notch pair.
///
/// And the reserved no-op: a wheel DOWN at fit does nothing at all
/// (pointer contract). Below fit there is no ladder, and browsing by
/// wheel was taken away on purpose (user decision, issue #11) — the
/// event must neither zoom nor fall through to the grid behind the fit
/// surface. That is asserted here on the zoom side (`d0`); the "and it
/// does not scroll the grid either" half needs a session with somewhere
/// to scroll and lives in `the_wheel_routing_table_holds_over_every_surface`.
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
                "6000:wheel.700,450,-60;6400:dump.d0;\
                 6800:wheel.700,450,59;7200:dump.d1;\
                 7600:wheel.700,450,1;8000:dump.w1;\
                 10000:wheel.700,450,60;10500:dump.w2;\
                 11000:wheel.700,450,30;11200:wheel.700,450,30;11700:dump.w3",
            ),
        ],
        &out,
    );
    // A full notch DOWN at fit: the reserved no-op.
    assert_eq!(
        dump_field(qedump(&stderr, "d0"), "zf"),
        "1.000",
        "a wheel notch DOWN at fit moved the zoom ladder:\n{stderr}"
    );
    // 59 px is not a notch…
    assert_eq!(
        dump_field(qedump(&stderr, "d1"), "zf"),
        "1.000",
        "59 logical px fired a notch — the accumulator's threshold is not \
         the 60 px winit delivers per line:\n{stderr}"
    );
    // …and the 60th px is.
    assert_eq!(
        dump_field(qedump(&stderr, "w1"), "zf"),
        "1.500",
        "the 60th px did not complete a notch (or the notch did not enter \
         the zoom ladder):\n{stderr}"
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
/// field route: the file dies on disk AFTER its thumb was read. A
/// helper thread zeroes the copy from byte 200,000 to EOF, anchored to
/// the app's own `thumb bytes idx 11` trace (the pipeline has the
/// embedded JPEG) and floored at T+9 s. Both halves matter: corrupting
/// before the read leaves idx 11 with no thumb at all (verified — the
/// app then drops on arrival and the masking shape never exists),
/// corrupting after the first End leaves the file readable.
///
/// The thumb path is TWO stages, which is what the old guard got
/// wrong: the pipeline reads every embedded JPEG at scan time (~0.1 s
/// here), but the kitchen only decodes one into a texture when its
/// cell comes near the view — for idx 11 of 12 in a 1-column loupe,
/// that is the first End itself. So the texture lands at ~15.0 s, the
/// failed full decode arrives ~17 ms later, and "did the rescue render
/// once before the failure?" is a same-tick coin flip — ~15 % red
/// under load, and product-neutral: both orders are correct.
///
/// What is NOT a coin flip, and is what this test exists for, is the
/// SECOND End: the failure is known, the thumb texture is in memory,
/// and the rescue must NOT render. So armed-ness is asserted as the
/// texture landing (`thumb landed idx 11`, which must precede the
/// second End — nothing evicts a thumb texture within a session, so
/// from there the rescue has one in hand), and the render count is
/// asserted where it binds: AFTER the `t1` dump it must be zero.
///
/// The script ends on a healthy cursor because a --start-11 shutter
/// whose final cursor is failed above fit trips the 60 s readiness cap
/// (recorded limitation). The ~2 s window the texture used to have to
/// land in is closed: the second End is held by `wait:thumb landed idx
/// 11` (issue #13's token), so a runner slow enough to take longer moves
/// the End with it instead of losing the arming. The assertion on that
/// same line stays — the wait proves the texture landed, the assertion
/// proves the ordering the count below is read against.
///
/// RED on the pre-gate build (b2ce1f9): the thumb renders on EVERY
/// End (so the after-t1 count is 1) and the "(decode failed)" drop
/// never appears. That the REWRITE fixed the flake rather than hiding
/// it was proven the other way round too: with idx 11's thumb decode
/// deliberately delayed 600 ms so the failure wins the race, the OLD
/// body fails with the issue's own "never rendered at all" message
/// while this one passes.
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
    // shared fixture RAW. The unlink first makes hard rule 1 structural
    // rather than conventional — copying ONTO a symlink of that name
    // would write straight through to the fixture.
    let corrupt = dir.join("zz_corrupt.ARW");
    std::fs::remove_file(&corrupt).ok();
    std::fs::copy(raws_dir().join("A1_full_compressed.ARW"), &corrupt).unwrap();
    // Corruption timing is the app's to decide, not a wall clock's: the
    // thread waits for the trace that says idx 11's embedded JPEG is in
    // memory, so a scan slowed by a loaded runner moves the corruption
    // with it instead of beating the pipeline to the file. The two real
    // constraints are that anchor and the first End at 15 s; nothing
    // reads the file in between. The T+9 s floor protects nothing today
    // — it is kept only so the corruption lands where this test's
    // schedule has always put it. The recv deadline is a liveness escape
    // only: corrupting anyway lets the run finish, and the armed-ness
    // guard below then names the real problem instead of a bare
    // "(decode failed) never appeared".
    let (bytes_tx, bytes_rx) = std::sync::mpsc::channel();
    let corrupter = {
        let path = corrupt.clone();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = bytes_rx.recv_timeout(Duration::from_secs(12));
            let at = std::cmp::max(
                Instant::now() + Duration::from_secs(1),
                started + Duration::from_secs(9),
            );
            std::thread::sleep(at.saturating_duration_since(Instant::now()));
            use std::io::{Seek, Write};
            let len = std::fs::metadata(&path).unwrap().len();
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.seek(std::io::SeekFrom::Start(200_000)).unwrap();
            f.write_all(&vec![0u8; (len - 200_000) as usize]).unwrap();
        })
    };
    let out = out_dir().join("i46-failgate.jpg");
    let stderr = shoot_env_stderr_watching(
        &["--start-11", dir.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "15000:end;15250:dump.t1;16000:home;\
                 16500:wait:thumb landed idx 11;17000:end;17150:dump.t2;18000:home",
            ),
        ],
        &out,
        // End-anchored: `idx 11` must not match `idx 110` if this shape
        // is ever copied to a bigger session.
        move |line| {
            if line.trim_end().ends_with("thumb bytes idx 11") {
                let _ = bytes_tx.send(());
            }
        },
    );
    corrupter.join().unwrap();
    // The gate was really in force: a `wait:` reports when it fires, so
    // this is the difference between "the token held the second End" and
    // "the token was a typo the parser dropped".
    assert!(
        stderr.contains("wait:thumb landed idx 11 (satisfied"),
        "the `wait:thumb landed idx 11` step never fired — the second End \
         was not gated on anything:\n{stderr}"
    );
    // The anchor must have FIRED, not merely timed out into the 9 s floor:
    // a renamed trace mark would otherwise leave the observer dead and
    // this test green on the floor alone (QE finding 2026-08-29).
    assert!(
        stderr.contains("thumb bytes idx 11\n"),
        "the corrupter's anchor `thumb bytes idx 11` never appeared — the \
         trace mark was renamed, or the pipeline never read the corrupt \
         copy:\n{stderr}"
    );
    // Non-vacuity, deterministic: idx 11's thumb TEXTURE reached memory
    // before the second End, so the rescue rung had something to render
    // there and chose not to. This is an ORDERING on one serial trace
    // stream (~2 s apart in practice), not a same-tick contest — the old
    // guard demanded a thumb RENDER on the FIRST End, which is exactly
    // the coin flip issue #50 was filed for. The script's own
    // `wait:thumb landed idx 11` makes the ordering causal rather than
    // scheduled (a renamed mark ends the run loudly at the wait's own
    // cap); this assertion reads the same fact off the log, and is what
    // still binds if the wait is ever taken out of the script.
    let second_end = stderr
        .match_indices("drive: end")
        .nth(1)
        .unwrap_or_else(|| panic!("the drive script's second End never ran:\n{stderr}"))
        .0;
    assert!(
        stderr[..second_end].contains("thumb landed idx 11\n"),
        "idx 11's thumb texture never reached memory before the second \
         End — the masking shape was never armed and this test proves \
         nothing:\n{stderr}"
    );
    assert!(
        stderr.contains("loupe overlay dropped idx 11 (decode failed)"),
        "a failed cursor never dropped to fit — the thumb rescue is \
         masking the failed badge again:\n{stderr}"
    );
    // The gate, where it is deterministic: after the t1 dump the failure
    // is known and the texture is in hand, so the SECOND End must render
    // no thumb at all. (The total bound keeps the first End honest too:
    // it may render the transient once, never twice.)
    //
    // The two ways this count could be zero for free are both closed by
    // assertions, not by reasoning: the second End actually landed on
    // idx 11 with the 1:1 desire intact (`cursor`/`zf` below — a
    // swallowed key or a dropped pin would otherwise buy the zero), and
    // the ladder was really re-entered above fit there (the drop
    // assertion below). Mutant A — the gate branch removed — corroborates
    // from the other side: it renders the thumb at the second End and
    // turns this assertion red.
    let after_t1 = stderr
        .split_once("QEDUMP t1 ")
        .unwrap_or_else(|| panic!("no `dump.t1` trace in stderr:\n{stderr}"))
        .1;
    assert_eq!(
        after_t1.matches("loupe thumb idx 11 ").count(),
        0,
        "the thumb rendered on a KNOWN-failed cursor (the second End) — \
         the gate is gone:\n{stderr}"
    );
    assert!(
        stderr.matches("loupe thumb idx 11 ").count() <= 1,
        "the thumb rescue rendered more than the one causally \
         unavoidable transient on the first End:\n{stderr}"
    );
    // The gate's own precondition, asserted: `render_rung` emits the
    // DecodeFailed drop ONLY when the overlay was wanted AND was up, so
    // this line IS "the second End re-entered the ladder above fit and
    // the rung was attempted there". Deterministic — the `home` at 16 s
    // re-raises the overlay on idx 0, which is healthy and warm.
    assert!(
        after_t1.contains("loupe overlay dropped idx 11 (decode failed)"),
        "the second End never re-entered the zoom ladder on idx 11 — the \
         zero thumb-render count above proves nothing:\n{stderr}"
    );
    for label in ["t1", "t2"] {
        let dump = qedump(&stderr, label);
        assert_eq!(
            dump_field(dump, "cursor"),
            "11",
            "the End at {label} never reached the corrupt image — a \
             swallowed key makes every count above zero for free:\n{stderr}"
        );
        // The 1:1 PIN, which is what makes the rung attempted at all:
        // `one2one=false` alone is also what a session simply sitting at
        // fit looks like, and a future "a failed cursor drops the pin
        // too" would make the render count vacuous without this.
        assert_eq!(
            dump_field(dump, "zf"),
            "inf",
            "the zoom desire was gone at {label} — the ladder was never \
             entered, so nothing about the thumb rescue was tested:\n{stderr}"
        );
        assert_eq!(
            dump_field(dump, "one2one"),
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
/// the destination for the landed pairs; the second phase waits for the
/// app's own `copy finished run 1` mark and then leaves the helper an
/// authored gap for its four unlinks. Fixtures are symlinks (the copy
/// follows them); the copies are removed at the end.
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

    // Phase 2 is gated on phase 1's report card (`wait:copy finished run
    // 1`, the app's own mark) rather than on a guess about 126 MB hashed
    // twice on a debug build. The wait sits right AFTER its trigger, not
    // just before the consumer, on purpose: the 8.7 s the script leaves
    // between the mark and the second phase's Escape — 9.1 s before the
    // Ctrl+E that recomputes the plan — prices the helper thread's four
    // local unlinks, bounded work, and has to survive a slow copy
    // too. CI run 98735565222 (Windows, 2026-08-28) is this suite's one
    // PROVEN red of that shape: the copy overran the old 12 s clock and
    // the helper's 11 s deadline together. The re-run's dump waits the
    // same way.
    let script = format!(
        "1600:key:y;1900:key:y;2200:copydest:{dest};2600:key:ctrl+e;3000:dump.first;\
         3200:key:return;3300:wait:copy finished run 1;\
         12000:key:escape;12400:key:ctrl+e;12800:dump.second;\
         13000:key:return;18900:wait:copy finished run 2;19000:dump.third;\
         19300:key:escape;19600:dump.end",
        dest = dest.display()
    );
    let landed = ["a.ARW", "a.ARW.xmp", "b.ARW", "b.ARW.xmp"];
    let deleter = {
        let dest = dest.clone();
        std::thread::spawn(move || -> Result<(), String> {
            // Liveness escape only: the script's own `wait:copy finished
            // run 1` ends a run whose copy stalls long before this, so the
            // deadline is not part of the ordering argument any more.
            let deadline = Instant::now() + Duration::from_secs(60);
            while !landed.iter().all(|n| dest.join(n).exists()) {
                if Instant::now() > deadline {
                    return Err(format!(
                        "the first copy never landed (60 s escape): {:?}",
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
    // Both gates really fired: a dropped or misspelled `wait:` token puts
    // the phase back on the clock this conversion removed.
    for token in [
        "wait:copy finished run 1 (satisfied",
        "wait:copy finished run 2 (satisfied",
    ] {
        assert!(
            stderr.contains(token),
            "`{token}` never fired — that phase was timed, not gated:\n{stderr}"
        );
    }
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
/// One further round is answered with the MOUSE rather than a key, against
/// a destination of its own so the rounds after it see the disk they always
/// saw: the answer rows moved inside the dialog's scrolling body in issue
/// #62, and every other answer here is a keystroke, so nothing else would
/// notice if a press stopped reaching them.
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
    // A destination of its own for the MOUSE round below, so the rounds
    // that follow see exactly the disk they always saw.
    let dest0 = out_dir().join("clash-dest0");
    for d in [&src, &src2, &dest, &dest2, &dest0] {
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
    std::fs::write(dest0.join("a.ARW"), &foreign).unwrap();

    // ONE round answered with the MOUSE, ahead of the keyboard rounds and
    // against its own destination so nothing below sees a different disk.
    // Every other answer here is a key press, and the answer rows live
    // inside the dialog's scrolling body since issue #62 — a change to
    // Slint's drag threshold, or to what a ScrollView does with a press,
    // would take mouse answers away silently. `700,483` is the Keep-both
    // row at the default 1440x900 (probed, 2026-08-30); a coordinate that
    // drifts off it leaves `copystate` at 3 and the assertion below says
    // so. Gated like the other coordinate-dependent strands: the rows sit
    // under a Text whose height is a font metric.
    let mouse_answer = menu_clicks_are_calibrated();
    let mouse_round = if mouse_answer {
        // The dump waits for the copy this click starts (QE 2026-09-02):
        // 700 ms is tighter than the 800 ms the keyboard round used, which
        // is what went red on Windows, and a 900 ms copy reddens it on
        // either tree. This is the ONE answer given with the pointer, so
        // the click can also miss — and then the wait ends the run after
        // 30 s naming `copy finished run 1`, which says the same thing the
        // `copystate` assertion below would have: the click answered
        // nothing and no copy ever ran.
        format!(
            "1900:copydest:{dest0};2100:key:ctrl+e;2400:key:return;2700:dump.qclick;\
             2900:click.700,483;3000:wait:copy finished run 1;\
             3600:dump.clicked;3900:key:escape;",
            dest0 = dest0.display()
        )
    } else {
        String::new()
    };
    // Which copy of this PROCESS each answer below starts: the mouse round
    // ran one already. The two dumps that read a finished copy wait for
    // THAT run's report card instead of allowing it 800 ms — a budget the
    // Windows runner does not keep (issue #70: `copystate` read 1, the
    // copy was still going), and one no runner is obliged to keep.
    let n = if mouse_answer { 2 } else { 1 };
    let (n2, kept_wait, over_wait) = (
        n + 1,
        format!("wait:copy finished run {n} (satisfied"),
        format!("wait:copy finished run {} (satisfied", n + 1),
    );
    let script = format!(
        "1500:key:y;1700:key:y;{mouse_round}\
         4400:copydest:{dest};4600:key:ctrl+e;4900:dump.preview;\
         5100:key:return;5400:dump.question;5600:key:return;5800:dump.inert;\
         5880:key:ctrl+o;5940:dump.accel;\
         6000:key:b;6100:wait:copy finished run {n};6800:dump.kept;7100:key:escape;\
         7300:key:ctrl+e;7600:key:return;7900:dump.q2;8100:key:o;\
         8200:wait:copy finished run {n2};8900:dump.over;\
         9200:key:escape;9400:copydest:{dest2};9600:key:ctrl+e;9900:key:return;\
         10200:dump.q3;10400:key:escape;10700:dump.cancelled;11000:key:escape;11300:dump.end;\
         11500:key:ctrl+e;11800:key:return;12100:dump.q4;12300:open:{src2};12700:dump.swapped",
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
    let mouse_disk = listing(&dest0);
    let landed_a = std::fs::read(dest.join("a.ARW")).ok();
    let landed_a1 = std::fs::read(dest.join("a_1.ARW")).ok();
    let landed_a1_xmp = std::fs::read(dest.join("a_1.ARW.xmp")).ok();
    let src_a_xmp = std::fs::read(src.join("a.ARW.xmp")).ok();
    for d in [&src, &src2, &dest, &dest2, &dest0] {
        std::fs::remove_dir_all(d).ok();
    }

    // --- the one answer given with the mouse ------------------------------
    if mouse_answer {
        assert_eq!(
            dump_field(qedump(&stderr, "qclick"), "copystate"),
            "3",
            "the mouse round never reached the question:\n{stderr}"
        );
        assert!(
            stderr.contains("wait:copy finished run 1 (satisfied"),
            "the mouse round's `wait:` never fired — its dump was timed, \
             not gated:\n{stderr}"
        );
        assert_eq!(
            dump_field(qedump(&stderr, "clicked"), "copystate"),
            "2",
            "the click on the Keep-both row did not answer the question — \
             a mouse-only user cannot answer it at all (or the coordinate \
             drifted off the row):\n{stderr}"
        );
        let names: Vec<&str> = mouse_disk.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec!["a.ARW", "a_1.ARW", "a_1.ARW.xmp", "b.ARW", "b.ARW.xmp"],
            "the clicked Keep-both did not land the pick under a fresh name: {mouse_disk:?}"
        );
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
    // The dump below reads a FINISHED copy because the script waited for
    // one, not because 800 ms was thought to be enough (a dropped or
    // misnumbered token would put it back on that clock silently).
    assert!(
        stderr.contains(&kept_wait),
        "the `wait:copy finished run {n}` step never fired — the keep-both \
         dump was timed, not gated:\n{stderr}"
    );
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
    assert!(
        stderr.contains(&over_wait),
        "the `wait:copy finished run {n2}` step never fired — the overwrite \
         dump was timed, not gated:\n{stderr}"
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
    // `dump.done` reads the copy's report card, so it waits for the copy's
    // own mark rather than for 12.4 s of clock: `copy finished run 1` is
    // numbered at `start_copy`, and the only copy this script starts is the
    // Enter at 3600 (measured 4686 ms on the Windows debug runner, 3805 ms
    // on the Linux release one — the wait is satisfied instantly at 15900
    // in both). The 16000 stays as a backstop; a runner slower than 12.4 s
    // for two 50 MP frames now shifts the tail instead of dumping mid-copy.
    let script = format!(
        "1600:key:y;1900:key:y;2200:copydest:{dest};2600:key:ctrl+e;\
         3000:copytemplate:{{camera}}.{{ext}};3400:dump.planned;3600:key:return;\
         15900:wait:copy finished run 1;16000:dump.done;16400:key:escape;16800:dump.end",
        dest = dest.display()
    );
    let out = out_dir().join("camera-template.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    assert!(
        stderr.contains("wait:copy finished run 1 (satisfied"),
        "the `wait:copy finished run 1` step never fired — `dump.done` was \
         timed, not gated:\n{stderr}"
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

    // `dump.done` waits for the export's own report-card mark. The first
    // Ctrl+Shift+E at 1900 is REFUSED (no destination yet), and the run
    // number is taken at `start_export`, so `run 1` is the export the
    // Enter at 3500 starts and nothing else. The 8.5 s the schedule still
    // leaves after that Enter (the export measures ~1.6 s on the
    // development laptop in a DEBUG build, 245 ms on the Windows CI
    // runner) is a backstop the wait rides through when it is already
    // satisfied; a slower runner shifts the tail instead of failing.
    let script = format!(
        "1600:dump.idle;1900:key:ctrl+shift+e;2200:dump.refused;\
         2500:select-all;2700:clipdest:{dest};2900:key:ctrl+shift+e;\
         3100:key:n;3200:key:y;3300:key:ctrl+o;3400:dump.plan;\
         3500:key:return;11900:wait:clip export finished run 1;12000:dump.done;\
         12400:key:escape;12700:dump.end",
        dest = dest.display()
    );
    let out = out_dir().join("clip-export.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    assert!(
        stderr.contains("wait:clip export finished run 1 (satisfied"),
        "the `wait:clip export finished run 1` step never fired — \
         `dump.done` was timed, not gated:\n{stderr}"
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
        dump_field(plan, "focusowner"),
        "-1",
        "the dialog owns the keyboard while it is up (issues #41/#42); \
         through the owner token, which names the dialog scope rather \
         than merely denying the main one (issue #63): {plan}"
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

/// The badge PIXEL criterion of the test below, factored out so it can be
/// replayed over a CI artifact — a `clip-badge.jpg` from either runner —
/// and not only over a shot this machine just took (issue #70).
///
/// It asserts the pill's LEFT EDGE, never a rectangle it must fill. The
/// ▶ glyph comes from a different face on Windows, which draws it BOXED:
/// `pill_span` over PR #71's two artifacts reports the pill in the ✓'s
/// slot at x 9..28 on Linux against 9..35 on Windows, and the stepped one
/// at 28..47 against 28..54 — the SAME left edge, 19 px against 26 px of
/// width. A font difference, not a defect; the fixed `x 30..46` control
/// this replaced read 0.26 dark on Windows (against a `< 0.15` bound)
/// because the wider pill's right end reached into it.
fn assert_badge_pixels(shot: &GridShot) {
    // The badge band, in the badges' own cell-local coordinates: the ✓
    // lives at x 8 and the ▶ falls back to that slot, stepping to x 28
    // only when a ✓ is in the way.
    let (band0, band1) = (shot.cell_h - 20.0, shot.cell_h - 6.0);
    let check = (8.0, shot.cell_h - 19.0, 20.0, shot.cell_h - 7.0);
    let span = |col: usize| shot.pill_span(col, band0, band1);
    let green = |col: usize, r: (f64, f64, f64, f64)| shot.greenness(col, r.0, r.1, r.2, r.3);

    // Column 0 is the control: `c` lost its only video and was never
    // copied, so nothing in its band may read as a pill at all.
    assert!(
        span(0).is_none(),
        "the unbadged frame carries a pill at x {:?} — `c` has no ✓ and no \
         ▶, so its badge band must be bare picture",
        span(0)
    );
    // `b` is exported and NOT copied: with no ✓ in the way the pill takes
    // the left slot — one half of that one line of layout.
    let b = span(2).unwrap_or_else(|| {
        panic!("no ▶ pill at all on the exported, uncopied frame — its badge band is bare")
    });
    assert!(
        (6..=12).contains(&b.0),
        "the ▶ pill of the exported, uncopied frame starts at x {} — with \
         no ✓ to step past it belongs in the ✓'s own slot at x 8",
        b.0
    );
    // `a` is copied AND exported: the ✓ keeps x 8 and the pill steps right.
    let a = span(1).unwrap_or_else(|| {
        panic!("no ▶ pill beside the ✓ on the exported, copied frame — its badge band is bare")
    });
    assert!(
        a.0 >= 20,
        "the ▶ pill did not step past the ✓ — the first pill of the copied, \
         exported frame starts at x {}, inside the ✓'s slot",
        a.0
    );
    assert!(
        (26..=32).contains(&a.0),
        "the ▶ pill of the copied, exported frame starts at x {} — the step \
         past the ✓ puts it at x 28",
        a.0
    );
    // The WIDTH is bounded on both sides: loosely, because it is the
    // font's — 19 px on the ubuntu runner, 21 px on the development seat,
    // 26 px on Windows, where the glyph is boxed — but not open-ended.
    // The upper bound is the widest measured pill plus 8 px, which is
    // what still catches a pill drawn twice its size or two pills merged
    // into one run (the fixed rectangle this replaced caught that
    // incidentally; validator 2026-09-02).
    for (what, (x0, x1)) in [("stepped past the ✓", a), ("in the ✓'s slot", b)] {
        let width = x1 - x0;
        assert!(
            (14..=34).contains(&width),
            "the ▶ pill {what} spans x {x0}..{x1}, {width} px — a pill \
             measures 19-26 px across the runners (26 with Windows's boxed \
             glyph), so this is the photograph, two pills run together, or \
             a badge drawn at the wrong size"
        );
    }
    assert!(
        green(1, check) > green(0, check) + 8.0,
        "the ✓ is gone from the copied frame: greenness {:.1} against \
         {:.1} on the frame that has none",
        green(1, check),
        green(0, check)
    );
    // MONOCHROME, mechanized: the glyph took the UI's own `#d8d8e0`, so
    // its strokes are bright and neutral. A colour-emoji bitmap ignores
    // the `color` property, and U+25B6 is in the emoji-presentation set —
    // this is the check that says which one the font gave us. Read over
    // the pill this run MEASURED, not over a fixed rectangle: on Windows
    // the strokes of the boxed glyph reach past x 46.
    let (bright, spread) = shot.bright_spread(1, a.0 as f64, band0, a.1 as f64, band1);
    assert!(
        bright >= 12,
        "no bright glyph strokes inside the ▶ pill — it rendered as a \
         dark bitmap, not as text in the UI's colour ({bright} px)"
    );
    assert!(
        spread <= 40.0,
        "the ▶ glyph is not monochrome (worst channel spread {spread:.0}) \
         — the font gave us a colour emoji; the spec's fallback is ▸ U+25B8"
    );
}

/// The ▶ exported badge and the dialog's counted hint (issue #56), end to
/// end on real camera frames — the whole session-only contract in one run,
/// asserted in the GRID's pixels and not only in the ledger's state.
///
/// One frame is copied first (Copy Picks) so the final screenshot carries
/// all three badge layouts at once: `c` with neither badge, `a` with ✓ and
/// ▶ side by side (the offset branch), `b` with ▶ alone in the ✓'s slot.
///
/// Two exports out of the same three files, so both hint shapes appear for
/// real: frames 2-3 alone land `a-b.mov`, then all three land `c-b.mov`.
/// Between them the dialog must say "2 of 3 frames are already in
/// a-b.mov"; after them, "all 3 frames … in 2 videos — c-b.mov and 1 more".
///
/// Then the FOLLOW-THE-DISK half: a helper thread deletes `c-b.mov` while
/// the app is live (the corrupter's shape — the app must never be told).
/// Nothing re-checks until the next dialog open, which is the design (no
/// stat storm per repaint), and that open drops exactly the badge that
/// stopped being true: `c` loses its ▶ while `a` and `b` keep theirs
/// through `a-b.mov`.
#[test]
fn an_exported_frame_wears_a_badge_until_its_video_is_gone() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("badge-src");
    let dest = out_dir().join("badge-dest");
    let copied = out_dir().join("badge-copied");
    for d in [&src, &dest, &copied] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    let raws = raws_dir();
    // Shot at 15:29:13, :40 and :55 and named c, a, b IN THAT ORDER, so
    // the grid (capture order, the default sort) reads c, a, b. `home`
    // then `right` then `shift-right` therefore selects `a` and `b` —
    // whose video is `a-b.mov`, a different name from the all-three
    // `c-b.mov`, so the second export is a fresh write and not a clash
    // question.
    for (name, fixture) in [
        ("c.ARW", "A1_full_compressed.ARW"),
        ("a.ARW", "A1_full_lossless_compressed.ARW"),
        ("b.ARW", "A1_full_uncompressed.ARW"),
    ] {
        place_fixture(&raws.join(fixture), &src.join(name));
    }
    // ADR 0003 guard: the RAWs are read, never written. Sidecars are
    // allowed and this run makes one (it marks a pick), so the RAW
    // entries are compared on their own and every ADDITION has to be a
    // sidecar — a stricter statement than the sibling test's, which marks
    // nothing and can compare the whole listing.
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
    let raws_only = |v: &[(String, u64)]| -> Vec<(String, u64)> {
        v.iter()
            .filter(|(n, _)| n.ends_with(".ARW"))
            .cloned()
            .collect()
    };
    let before = listing(&src);

    // The deletion has to land AFTER the hint that names `c-b.mov`
    // (`dump.hint2`) and BEFORE the dialog re-opens (`dump.stale`), and it
    // is DETERMINISTIC by construction rather than timed hopefully: it
    // fires 10 s after the victim appears, and the victim's appearance is
    // bracketed by the script itself — CAUSALLY, since the script gates on
    // the app's own marks and every timestamp after a `wait:` is rebased
    // on it, so the gaps below hold whatever a slow runner adds. The
    // second export's `key:return` is 7.7 s before `dump.hint2` (more if
    // the export is slow — see the bound below), so the file cannot appear
    // before then → fire 2.3 s past the hint. And
    // `dump.done2` runs only once `wait:clip export finished run 2` has
    // seen the export finish, so the file exists before it fires → fire
    // ≤ 10.9 s after done2 (the three unlink retries included), before the
    // `dump.stale` 12.7 s after it. Anchoring on the FILE rather than on
    // this thread's clock is what makes both ends hold whatever the
    // process's startup cost was — the wall clock here starts before the
    // app boots.
    //
    // The EARLY end carries a bound: it holds while the second export
    // takes under 8.7 s (its wait step sits 6.4 s after the Enter and the
    // hint 1.3 s behind that, against the 10 s sleep). A slower export
    // pushes the whole tail past the deletion, and the run then fails at
    // `dump.hint2` with the victim already gone — not at `dump.done2`.
    // Read a red there as "the second export took longer than 8.7 s", not
    // as a badge that never appeared. Measured export duration: 253 ms on
    // the Windows debug runner, the slower of the two artifact sets.
    //
    // The poll's own deadline is a failure guard, not part of that
    // argument: it only turns "the export never happened" into a message
    // instead of a hang, so it is set well past the script's own end.
    let victim = dest.join("c-b.mov");
    let deleter = {
        let victim = victim.clone();
        std::thread::spawn(move || -> Result<(), String> {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !victim.exists() {
                if Instant::now() > deadline {
                    return Err("the second export never landed c-b.mov".to_string());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            std::thread::sleep(Duration::from_secs(10));
            // Windows CI runs this file: a scanner or an indexer holding
            // the freshly written `.mov` open is a sharing violation, not
            // a product fact, and this test must fail on the ledger or
            // not at all.
            let mut last = String::new();
            for _ in 0..3 {
                match std::fs::remove_file(&victim) {
                    Ok(()) => return Ok(()),
                    Err(e) => last = e.to_string(),
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(format!("rm c-b.mov: {last}"))
        })
    };

    // POSITION-BASED NAVIGATION IS GATED ON THE SETTLED SORT. The view is
    // in provisional FILENAME order until the last frame's metadata lands
    // (issue #25), and it then re-sorts to capture order under the script
    // — which silently turns `right`/`shift-right` into a different
    // selection (validator finding, 2026-08-29: it selected b+c, whose
    // video is the SAME name the second export wants, and the run died in
    // a clash question). `home` now WAITS for the settle itself
    // (`wait:load settled gen 0`), and `dump.sorted` keeps asserting it as
    // the proof — the mark IS the thumb-bytes count (state.rs
    // `metadata_complete`), so "3 thumbs loaded" behind it is definitional.
    let script = format!(
        "1900:clipdest:{dest};2000:copydest:{copied};\
         4900:wait:load settled gen 0;5000:home;5200:dump.sorted;\
         8000:right;8200:key:y;8400:key:ctrl+e;8600:dump.copyplan;8800:key:return;\
         16900:wait:copy finished run 1;17000:dump.copied;17300:key:escape;\
         17600:home;17800:right;18000:shift-right;\
         18200:key:ctrl+shift+e;18400:dump.plan1;18600:key:return;\
         25000:wait:clip export finished run 1;25100:dump.done1;25400:key:escape;\
         25700:dump.badges1;\
         26000:select-all;26200:key:ctrl+shift+e;26500:dump.plan2;26800:key:return;\
         33200:wait:clip export finished run 2;33300:dump.done2;33600:key:escape;\
         33900:dump.badges2;\
         34200:key:ctrl+shift+e;34500:dump.hint2;34800:key:escape;\
         46000:dump.stale;46500:key:ctrl+shift+e;46800:dump.gone;\
         47100:key:escape;47400:key:escape;47700:home;48000:dump.end",
        dest = dest.display(),
        copied = copied.display()
    );
    let out = out_dir().join("clip-badge.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let deleted = deleter.join().expect("deleter thread");
    let mut landed: Vec<String> = std::fs::read_dir(&dest)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    landed.sort();
    let after = listing(&src);
    for d in [&src, &dest, &copied] {
        std::fs::remove_dir_all(d).ok();
    }
    assert_eq!(
        deleted,
        Ok(()),
        "the hand deletion did not happen:\n{stderr}"
    );

    // The four gates really fired: a dropped or misspelled `wait:` token
    // is a script quietly back on the clock, and every dump below then
    // reads a state the app is no longer promised to be in.
    for token in [
        "wait:load settled gen 0 (satisfied",
        "wait:copy finished run 1 (satisfied",
        "wait:clip export finished run 1 (satisfied",
        "wait:clip export finished run 2 (satisfied",
    ] {
        assert!(
            stderr.contains(token),
            "`{token}` never fired — that step was timed, not gated:\n{stderr}"
        );
    }

    // --- the sort settled before anything counted positions ---------------
    let sorted = qedump(&stderr, "sorted");
    let status = dump_text(sorted, "status");
    assert!(
        status.contains("3 thumbs loaded"),
        "the metadata had not all landed, so the view was still in the \
         provisional filename order and every position below means \
         something else: {sorted}"
    );
    assert!(
        status.starts_with("c.ARW (1/3)"),
        "the cursor is not on c.ARW: either the view had not settled into \
         capture order (c, a, b) when `home` ran, or it re-sorted after — a \
         position-based script cannot run on it: {sorted}"
    );
    assert_eq!(dump_field(sorted, "exported"), "0", "{sorted}");
    assert_eq!(dump_field(sorted, "curexported"), "false", "{sorted}");
    assert_eq!(dump_text(sorted, "cliphint"), "", "{sorted}");

    // --- one pick copied, so the ✓ badge is on screen too ------------------
    let copyplan = qedump(&stderr, "copyplan");
    assert_eq!(dump_field(copyplan, "copy"), "true", "{copyplan}");
    assert!(
        dump_text(copyplan, "summary").contains("1 picked"),
        "the Y did not mark exactly one frame: {copyplan}"
    );
    let done_copy = qedump(&stderr, "copied");
    assert!(
        dump_text(done_copy, "report").starts_with("1 copied"),
        "the copy did not finish: {done_copy}"
    );

    // --- before anything was exported: no badge, no hint ------------------
    let plan1 = qedump(&stderr, "plan1");
    assert_eq!(dump_field(plan1, "clipstate"), "0", "{plan1}");
    assert_eq!(dump_field(plan1, "selected"), "2", "two frames selected");
    assert_eq!(
        dump_text(plan1, "cliphint"),
        "",
        "nothing has been exported yet, so the dialog must claim nothing"
    );
    assert_eq!(dump_field(plan1, "exported"), "0", "{plan1}");
    assert!(
        dump_text(plan1, "clipsummary").starts_with("2 frames · ")
            && dump_text(plan1, "clipsummary").contains("a-b.mov"),
        "the selection is not the two frames right of the first: {plan1}"
    );

    // --- after the first export: two frames badged, cursor included -------
    let done1 = qedump(&stderr, "done1");
    assert_eq!(
        dump_field(done1, "clipstate"),
        "2",
        "the first export did not finish: {done1}"
    );
    let badges1 = qedump(&stderr, "badges1");
    assert_eq!(
        dump_field(badges1, "exported"),
        "2",
        "only the two exported frames may wear the badge: {badges1}"
    );
    assert_eq!(
        dump_field(badges1, "curexported"),
        "true",
        "the cursor sits on the last exported frame: {badges1}"
    );

    // --- the counted hint, on the scope the user is about to export -------
    let plan2 = qedump(&stderr, "plan2");
    assert_eq!(dump_field(plan2, "selected"), "3", "{plan2}");
    assert_eq!(
        dump_text(plan2, "cliphint"),
        "2 of 3 frames are already in a-b.mov",
        "the dialog must count what is already in a video: {plan2}"
    );
    // READS, NEVER DECIDES: the plan still takes all three frames, with
    // two of them already in a video (video-export.md).
    assert!(
        dump_text(plan2, "clipsummary").starts_with("3 frames · "),
        "the ledger shrank the next export: {plan2}"
    );

    // --- after the second: all three, and the hint counts the videos ------
    let done2 = qedump(&stderr, "done2");
    assert_eq!(
        dump_field(done2, "clipstate"),
        "2",
        "the second export did not finish: {done2}"
    );
    let badges2 = qedump(&stderr, "badges2");
    assert_eq!(dump_field(badges2, "exported"), "3", "{badges2}");
    let hint2 = qedump(&stderr, "hint2");
    assert_eq!(
        dump_text(hint2, "cliphint"),
        "all 3 frames are already in 2 videos — c-b.mov and 1 more",
        "the hint must count the VIDEOS and name one, on one line: {hint2}"
    );

    // --- the file is gone, and NOTHING noticed until the dialog opened ----
    // The deleter's window is bracketed by the script (see above), so this
    // is a real assertion: a badge still standing here is the design, one
    // that has already dropped means the grid re-stats the disk per
    // repaint.
    let stale = qedump(&stderr, "stale");
    assert_eq!(
        dump_field(stale, "exported"),
        "3",
        "the badge re-stats the disk per repaint — the design says it \
         re-checks at an export's end and at a dialog open, and nowhere \
         else: {stale}"
    );
    let gone = qedump(&stderr, "gone");
    assert_eq!(
        dump_field(gone, "exported"),
        "2",
        "opening the dialog did not re-check the disk: {gone}"
    );
    assert_eq!(
        dump_text(gone, "cliphint"),
        "2 of 3 frames are already in a-b.mov",
        "the hint still points at the video that was deleted: {gone}"
    );
    // Per FRAME, not per burst: the frame that was only ever in the
    // deleted file is the one that lost its badge.
    let end = qedump(&stderr, "end");
    assert_eq!(dump_field(end, "clip"), "false", "Esc did not close: {end}");
    assert_eq!(
        dump_field(end, "curexported"),
        "false",
        "the first frame is only in the video that was deleted, so it must \
         have lost its badge while the other two kept theirs: {end}"
    );
    assert_eq!(dump_field(end, "exported"), "2", "{end}");

    // --- and the disk agrees ----------------------------------------------
    assert_eq!(
        landed,
        vec!["a-b.mov".to_string()],
        "the survivor is the video the badges still point at"
    );
    assert_eq!(
        raws_only(&before),
        raws_only(&after),
        "a RAW changed (hard rule 1 / ADR 0003: this session may only read them)"
    );
    for (name, _) in &after {
        assert!(
            before.iter().any(|(n, _)| n == name) || name.ends_with(".xmp"),
            "{name} appeared beside the RAWs and is not a sidecar: {after:?}"
        );
    }

    // --- THE GRID ITSELF --------------------------------------------------
    // The dump above proves the LEDGER; only pixels prove that the cell
    // wears the badge. Without this, deleting the Slint block or sending
    // `exported: false` keeps the whole suite green (validator finding,
    // 2026-08-29).
    //
    // Final state, left by the script: `c` (col 0) has no ✓ (not picked)
    // and no ▶ (its only video was deleted); `a` (col 1) has both, the ✓
    // at x 8 and the ▶ pill stepped right to x 28; `b` (col 2) has the ▶
    // alone, in the ✓'s own slot at x 8. Column 0 is therefore the
    // photograph-only control.
    //
    // What is asserted is each pill's LEFT EDGE, not a rectangle it fills:
    // the Windows runner's ▶ comes from a face that draws it BOXED, so the
    // pill is 26 px wide there against 19 px on the ubuntu runner (a font
    // difference, not a defect — see `assert_badge_pixels`, which is also
    // replayable over a CI artifact from either platform: it passes on
    // both of PR #71's `clip-badge.jpg` files unchanged).
    let (w, h, luma) = analyze(&out);
    assert!(w >= 640 && h >= 480 && luma > 5.0, "{w}x{h} luma {luma:.2}");
    assert_badge_pixels(&grid_shot(&out, 8));
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
         3700:key:b;3800:wait:clip export finished run 1;5000:dump.kept;\
         5300:key:escape;\
         5600:key:ctrl+shift+e;5900:key:return;6200:key:o;\
         6300:wait:clip export finished run 2;7500:dump.over;\
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
    // Gated on the export's own report card, not on the 1.3 s the write
    // and its verify pass used to be given (issue #70): the run number is
    // what lets the second answer below wait for ITS export rather than
    // being satisfied by the first one's mark.
    assert!(
        stderr.contains("wait:clip export finished run 1 (satisfied"),
        "the keep-both `wait:` never fired — the dump was timed, not \
         gated:\n{stderr}"
    );
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
    // A SKIPPED FRAME IS NEVER BADGED (issue #56): four frames are in the
    // view and were selected, `d` could not share the track, so three are
    // in the file — and exactly three may wear the ▶. This is the one
    // driven run with a non-uniform frame set, which is why the badge
    // claim is asserted here rather than in the badge test's own uniform
    // fixtures (validator finding, 2026-08-29).
    assert_eq!(
        dump_field(kept_dump, "exported"),
        "3",
        "a frame the export SKIPPED wears a badge for a video it is not \
         in — or a frame that is in it does not: {kept_dump}"
    );

    // --- O: overwrite ------------------------------------------------------
    assert!(
        stderr.contains("wait:clip export finished run 2 (satisfied"),
        "the overwrite `wait:` never fired — the dump was timed, not \
         gated:\n{stderr}"
    );
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

/// Both dialog cards keep their button row inside the card, at every size
/// and whatever their text says (issue #62).
///
/// `card` and `buttons` are the trace prefixes ("clip"/"copy"), `label` the
/// dump the geometry is read at, `window_h` the window's height at that
/// moment. Three things are checked, and the third is what the issue is:
/// the row is below the card's top, the row's bottom is inside the card,
/// and the card itself is inside the window.
fn assert_buttons_inside_card(stderr: &str, dialog: &str, label: &str, window_h: f32) {
    let (_, card_y, _, card_h) = laid_out_at(stderr, &format!("{dialog} card"), label);
    let (_, btn_y, _, btn_h) = laid_out_at(stderr, &format!("{dialog} buttons"), label);
    assert!(
        btn_y >= card_y,
        "dump.{label}: the {dialog} button row starts above its card:\n{stderr}"
    );
    assert!(
        btn_y + btn_h <= card_y + card_h + 0.5,
        "dump.{label}: the {dialog} button row ends at {} but the card ends at \
         {} — the row is outside the card (issue #62):\n{stderr}",
        btn_y + btn_h,
        card_y + card_h
    );
    assert!(
        card_y + card_h <= window_h,
        "dump.{label}: the {dialog} card ends at {} in a {window_h}px window:\n{stderr}",
        card_y + card_h
    );
}

/// The last `<what> laid out at X,Y size WxH` trace before the QEDUMP
/// labelled `label` — the rectangle as it stood at the moment of the dump.
///
/// Scanned in order rather than searched from the end: these marks fire on
/// every relayout, so the last one in the whole run belongs to whatever
/// state the app ended in, not to the state the assertion is about.
fn laid_out_at(stderr: &str, what: &str, label: &str) -> (f32, f32, f32, f32) {
    let tag = format!("] {what} laid out at ");
    let dump = format!("QEDUMP {label} ");
    let mut last: Option<(f32, f32, f32, f32)> = None;
    for line in stderr.lines() {
        if let Some((_, rest)) = line.split_once(&tag) {
            // "X,Y size WxH"
            let parse = || -> Option<(f32, f32, f32, f32)> {
                let (xy, wh) = rest.split_once(" size ")?;
                let (x, y) = xy.split_once(',')?;
                let (w, h) = wh.split_once('x')?;
                Some((
                    x.trim().parse().ok()?,
                    y.trim().parse().ok()?,
                    w.trim().parse().ok()?,
                    h.trim().parse().ok()?,
                ))
            };
            if let Some(rect) = parse() {
                last = Some(rect);
            }
        }
        if line.contains(&dump) {
            return last.unwrap_or_else(|| {
                panic!("no `{what} laid out` trace before dump.{label}:\n{stderr}")
            });
        }
    }
    panic!("no `dump.{label}` trace in stderr:\n{stderr}")
}

/// Where a dialog's scrolling body stands at the QEDUMP labelled `label`:
/// 0 at the top, negative going down (`<what> scrolled to Y`).
///
/// Absent means 0 — the mark fires on CHANGE, so a body that has never
/// been scrolled emits nothing, which is exactly "at the top".
///
/// `#[cfg(unix)]` to match its only callers: the Copy Picks overflow test
/// below arranges its long failure report with `chmod`, so it is unix-only
/// and this helper is dead code on Windows — where `cargo clippy
/// --all-targets -- -D warnings` turns "never used" into a build failure
/// (CI, windows-latest, v0.13.0). `laid_out_at` and
/// `assert_buttons_inside_card` above need no such gate: the clip-report
/// test calls them on every platform.
#[cfg(unix)]
fn body_scroll_at(stderr: &str, what: &str, label: &str) -> f32 {
    let tag = format!("] {what} scrolled to ");
    let dump = format!("QEDUMP {label} ");
    let mut last = 0.0f32;
    for line in stderr.lines() {
        if let Some((_, rest)) = line.split_once(&tag) {
            if let Ok(y) = rest.trim().parse::<f32>() {
                last = y;
            }
        }
        if line.contains(&dump) {
            return last;
        }
    }
    panic!("no `dump.{label}` trace in stderr:\n{stderr}")
}

/// Issue #62: a refusal that names a dozen frame sizes must not push the
/// dialog's buttons out of its card — and neither must anything else, at
/// any window size.
///
/// Three mechanisms are under test and this run exercises all of them:
///
///   * core bounds the sentence — at most three reasons named, the rest
///     folded into one tail — so the refusal is two lines instead of nine;
///   * the card's height follows its content between a floor and the
///     window, so a longer sentence is given room rather than ignored;
///   * past the window the card stops growing and the BODY scrolls: the
///     text region is the only row the layout may shrink, so the deficit
///     lands there and the button row stays pinned inside the card. The
///     `small` dump is that case — a 640x300 window, where the ceiling is
///     below the card's own floor.
///
/// The geometry is asserted as a RELATION between two laid-out rectangles,
/// not against numbers: the card is centred and its height is now an
/// outcome, so a hard-coded y would be a coincidence. A screenshot cannot
/// stand in for this — the card does not clip, so an escaped row is drawn
/// over the scrim looking almost right, and Slint hit-tests it as clickable
/// either way.
///
/// Gated on app facts, not the clock (issue #61): the session swap waits
/// for `load settled gen 1` — the SECOND folder, since `session-gen` counts
/// from 0 for the folder the app opened with — the export waits for `clip
/// export finished`, and the resize waits for the card's own relayout at
/// the new window width (x = (640 - 560) / 2 = 40, which cannot be the
/// 1440-wide window's 440).
///
/// RED on the parent tree, measured 2026-08-30 (both halves reverted, the
/// witness kept): at `dump.refusal` the row ended 29 px below the card.
#[test]
fn a_long_refusal_keeps_the_export_buttons_inside_the_card() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let refuse = out_dir().join("i62-refuse");
    let export = out_dir().join("i62-export");
    let dest = out_dir().join("i62-dest");
    for d in [&refuse, &export, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    // THIRTEEN frames, THIRTEEN sizes. The first in order is the one the
    // track would be built from, so twelve are skipped in twelve groups:
    // three named and nine folded, which is the sentence asserted below.
    //
    // EVERY size stays above `raw::USEFUL_MIN_PIXELS` (100,000): a smaller
    // preview is not "a different size", it is "no usable embedded JPEG",
    // and twelve of those are ONE group — the test would then measure a
    // sentence that was never long. 280x400 = 112,000 is the smallest here.
    for i in 0..13u32 {
        write_synthetic_raw(
            &refuse.join(format!("f{i:02}.ARW")),
            (400 - i * 10) as u16,
            400,
            1,
            4096,
        );
    }
    // The plan/report fixture: two frames that CAN share a track (so the
    // export runs and the report card appears) and four that cannot, in
    // four sizes — the same sentence, bounded, on the report card.
    //
    // The two kept frames wear 100-character stems, which is what pushes
    // the card past its 260 px floor: the output name is built from the
    // first and last stem, so the plan line carries a 205-character file
    // name and wraps. Without it the card would sit ON the floor and the
    // "it grows" half of the fix would be untested (validator finding).
    let long_a = format!("a{}", "n".repeat(99));
    let long_z = format!("z{}", "n".repeat(99));
    write_synthetic_raw(&export.join(format!("{long_a}.ARW")), 400, 400, 1, 4096);
    write_synthetic_raw(&export.join(format!("{long_z}.ARW")), 400, 400, 1, 4200);
    for i in 0..4u32 {
        write_synthetic_raw(
            &export.join(format!("m{i}.ARW")),
            (380 - i * 10) as u16,
            400,
            1,
            4096,
        );
    }

    // The destination is set BEFORE the first Ctrl+Shift+E: without one
    // the dialog never plans at all ("13 frames. Choose a destination.")
    // and there is no refusal to measure.
    let script = format!(
        "{PIN_WINDOW};1400:clipdest:{dest};1500:select-all;\
         1800:key:ctrl+shift+e;2200:dump.refusal;\
         2500:key:escape;2700:open:{export};2800:wait:load settled gen 1;\
         3000:select-all;3100:clipdest:{dest};3300:key:ctrl+shift+e;\
         3600:dump.plan;3900:key:return;4000:wait:clip export finished;\
         4200:dump.report;\
         4500:resize:640x300;4600:wait:clip card laid out at 40,;\
         4900:dump.small;5200:key:escape",
        export = export.display(),
        dest = dest.display()
    );
    let out = out_dir().join("i62-card-overflow.jpg");
    let stderr = shoot_env_stderr(
        &[refuse.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    for d in [&refuse, &export, &dest] {
        std::fs::remove_dir_all(d).ok();
    }

    // The three gates fired, so nothing below is on a clock (issue #13's
    // rule: a dropped token must fail the test, not silently re-time it).
    for gate in [
        "wait:load settled gen 1 (satisfied",
        "wait:clip export finished (satisfied",
        "wait:clip card laid out at 40, (satisfied",
    ] {
        assert!(
            stderr.contains(gate),
            "the `{gate}…` gate never fired — the steps after it were timed:\n{stderr}"
        );
    }

    let refusal = qedump(&stderr, "refusal");
    assert_eq!(
        dump_field(refusal, "clip"),
        "true",
        "the export dialog did not open:\n{stderr}"
    );

    // --- THE CONTRACT: the buttons are inside the card, in every state
    // Asserted before the wording below because this is the failure the
    // issue is about, and a test that reported the sentence first would
    // hide it behind a text diff.
    for label in ["refusal", "plan", "report"] {
        assert_buttons_inside_card(&stderr, "clip", label, 900.0);
    }
    assert_buttons_inside_card(&stderr, "clip", "small", 300.0);

    // --- the card really did GROW, and really was CLAMPED --------------
    // Without these two the relation above would hold on a card that never
    // moved off its floor, which is the state the fix is not about.
    let (_, _, _, report_h) = laid_out_at(&stderr, "clip card", "report");
    assert!(
        report_h > 260.0,
        "the report card measured {report_h}px — at or below the 260px floor, \
         so the buttons could be inside it without the height following the \
         content at all:\n{stderr}"
    );
    let (_, small_y, _, small_h) = laid_out_at(&stderr, "clip card", "small");
    assert!(
        small_h <= 260.0,
        "the card measured {small_h}px in a 300px-tall window: the ceiling \
         did not bind, so the scrolling body is untested here:\n{stderr}"
    );
    assert!(
        small_y + small_h <= 300.0,
        "the card runs past the bottom of a 300px window:\n{stderr}"
    );

    // --- and the sentence that used to grow is bounded ----------------
    let error = dump_text(refusal, "cliperror");
    assert!(
        error.contains("9 other sizes"),
        "the refusal must fold the sizes it did not name: {error}\n{stderr}"
    );
    assert_eq!(
        error.matches("different size (").count(),
        3,
        "at most three reasons may be named: {error}\n{stderr}"
    );

    // The report state must be the one that was measured, not a dialog
    // that closed early and left the plan's rectangles standing.
    let report = qedump(&stderr, "report");
    assert_eq!(
        dump_field(report, "clipstate"),
        "2",
        "the export never reached its report card, so the geometry above \
         was measured on the wrong state:\n{stderr}"
    );
    // The report carries the plan's own sentence (video-export.md), so it
    // is bounded by the same helper: four skipped frames in four sizes,
    // three named and one folded — singular in both halves of the tail.
    let report_text = dump_text(report, "clipreport");
    assert!(
        report_text.contains("1 more frame in 1 other size"),
        "the report's skipped sentence must be bounded too: {report_text}\n{stderr}"
    );
    assert_eq!(
        report_text.matches("different size (").count(),
        3,
        "the report may name at most three reasons: {report_text}\n{stderr}"
    );
}

/// The Copy Picks card's own unbounded text (issue #62): `report_lines`
/// prints one `FAILED name: reason` line per file that failed, so a
/// destination that goes read-only mid-run words itself as long as the run
/// was. Sixty-one picks into a `chmod 555` folder is that report — far
/// taller than the window, so the card is pinned at its ceiling and the
/// body has to scroll for the buttons to stay inside it.
///
/// RED on the parent tree with THIS fixture (2026-08-30): the card stayed
/// at its old constant 480 px, ending at y=697, and the row ended at
/// y=1527 — 830 px below the card and 627 px below the window.
///
/// Unix only: the whole point is a destination the process may not write
/// to, and `chmod` is how that is arranged. On Windows the claim is
/// review-only, like the other permission-based tests in the suite.
#[test]
#[cfg(unix)]
fn a_failure_report_longer_than_the_window_keeps_the_copy_buttons_inside_the_card() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let _s = serial();
    let src = out_dir().join("i62-copy-src");
    let dest = out_dir().join("i62-copy-dest");
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    const PICKS: usize = 61;
    for i in 0..PICKS {
        write_synthetic_raw(&src.join(format!("p{i:02}.ARW")), 400, 400, 1, 4096);
    }
    // Readable and searchable, not writable: the plan builds, every copy
    // fails, and each failure is a line in the report.
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o555)).unwrap();

    // `y` marks the CURSOR and auto-advances, so picking the folder is one
    // keystroke per frame — a selection is not a pick (ui-grid.md).
    let picks: String = (0..PICKS)
        .map(|i| format!("{}:key:y;", 1500 + i * 30))
        .collect();
    let script = format!(
        "{PIN_WINDOW};{picks}\
         3600:copydest:{dest};3900:key:ctrl+e;4200:key:return;\
         4300:wait:copy finished;4500:dump.failed;\
         4700:key:pgdn;5000:dump.paged;5300:key:home;5600:dump.homed;\
         5900:key:escape",
        dest = dest.display()
    );
    let out = out_dir().join("i62-copy-overflow.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).ok();
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
    }

    assert!(
        stderr.contains("wait:copy finished (satisfied"),
        "the `wait:copy finished` gate never fired — the dump below was \
         timed, not gated:\n{stderr}"
    );
    let failed = qedump(&stderr, "failed");
    assert_eq!(
        dump_field(failed, "copystate"),
        "2",
        "the copy never reached its report card:\n{stderr}"
    );
    // Non-vacuity: a report of two lines would keep the buttons inside any
    // card. This one has to be the long one. It is also the root guard —
    // root writes into a 0o555 folder, every copy then SUCCEEDS, and this
    // assertion fails loudly ("only 0 failures") instead of the geometry
    // below passing on a two-line report that proves nothing.
    let report = dump_text(failed, "report");
    assert!(
        report.matches("FAILED ").count() > 40,
        "only {} failures in the report — not the overflowing card this \
         test is about: {report}\n{stderr}",
        report.matches("FAILED ").count()
    );

    assert_buttons_inside_card(&stderr, "copy", "failed", 900.0);
    // ...and the card is at its ceiling, which is what makes the body the
    // only thing that could have given way.
    let (_, card_y, _, card_h) = laid_out_at(&stderr, "copy card", "failed");
    assert!(
        card_h > 480.0,
        "the copy card measured {card_h}px — still on its 480px floor, so \
         this run never reached the ceiling case:\n{stderr}"
    );
    assert!(
        card_y + card_h <= 860.0,
        "the copy card ends at {} — past the window's 900px minus the 40px \
         margin the ceiling keeps:\n{stderr}",
        card_y + card_h
    );

    // --- and the KEYBOARD can read the part below the fold ---------------
    // The scrollbar is for the mouse; this app is driven from the keyboard,
    // and a report only the mouse can reach is a report the user cannot
    // read (QE finding 2026-08-30 — before this, PgDn did nothing at all
    // because the dialog scope swallowed it).
    assert_eq!(
        body_scroll_at(&stderr, "copy body", "failed"),
        0.0,
        "the report did not start at the top:\n{stderr}"
    );
    let paged = body_scroll_at(&stderr, "copy body", "paged");
    assert!(
        paged < -100.0,
        "PgDn moved the report by {paged}px — the lines past the fold are \
         unreachable without a mouse:\n{stderr}"
    );
    assert_eq!(
        body_scroll_at(&stderr, "copy body", "homed"),
        0.0,
        "Home did not return the report to its first line:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Issue #49: the Copy Picks and Export Frames as Video scrims are hand-rolled
// copies of `ModalScrim` with no `scroll-event` arm, so a wheel over either
// fell through to the grid's Flickable behind it and the user came back to a
// different place in the folder (persona: IN-MY-WAY when it bites).
//
// All three tests have the same three parts, and the middle one is the
// contract:
//   1. a wheel BEFORE the modal — the grid must really move, or the test
//      that follows is measuring a grid that cannot scroll at all;
//   2. the same wheel with the modal up — `vpy` must not budge;
//   3. the same wheel after Esc — the grid moves again, which is what makes
//      part 2 an observation about the scrim rather than about a dead token.
//
// The third test covers the shared `ModalScrim` itself (About, shortcuts),
// which nothing else pinned: its arm was never the bug, but a component two
// call sites rely on should not be the one scrim with no test.
//
// RED verified on this tree with the `scroll-event` arms removed — the two
// hand-rolled ones for the dialog tests, `ModalScrim`'s for the popup test:
// part 2 fails, `vpy=-360.0` where -180.0 is required.
// ---------------------------------------------------------------------------

/// Every script pins the window first. The card and field coordinates below
/// are geometry, not guesses, and they are only that geometry at 1440x900:
/// at `resize:1024x768` the card sits higher (y 151..631) and the same
/// click lands ~66 px BELOW the rename field, on the summary line
/// (measured). The default IS 1440x900, so this asserts the assumption
/// rather than changing anything (validator, 2026-08-29).
const PIN_WINDOW: &str = "200:resize:1440x900";

/// One notch is 60 logical px (the `wheel.` contract), so this is three
/// notches down. Every fixture below is deep enough that no scroll a script
/// drives lands on the Flickable's bottom clamp — where "unmoved" would mean
/// "out of room" rather than "swallowed".
///
/// The coordinates put the pointer over the CARD, not over bare scrim. At
/// 1440x900 the modal layer starts under the 40 px menu bar and is
/// `900 - 40 - 26` = 834 px tall (the status bar is 26 px), so a centred
/// card of height H spans y `40 + (834 - H) / 2` .. that plus H:
/// Copy Picks (480) y 217..697, the export dialog (260) y 327..587,
/// the shortcuts popup (549) y 182..731, About (348) y 283..631.
/// Cards are 560 px wide, 480 for About, and 780 for the shortcuts popup,
/// centred in 1440.
///
/// The shortcuts figures were `(560) y 177..737` until the card was
/// rebuilt around a fixed key column (2026-09-04): its height is its
/// CONTENT's now, so 549 is a MEASUREMENT ON THIS SEAT and not a constant
/// a reader can find in the .slint file — Liberation Sans lays the same
/// card out at 491 px. `shortcuts_card_is_a_two_column_sheet_that_fits_
/// its_window` therefore pins no height at all; what keeps the number
/// above honest is that a pointer 100 px off the card's centre is still
/// on the card in every face measured.
///
/// The first two numbers are FLOORS since issue #62, not constants: those
/// cards grow with their content up to the window. These scripts use
/// neither an error, a hint, nor a report, so both sit on their floor and
/// the spans above are what they measure (verified by the `card laid out`
/// traces, 2026-08-30). A wheel over a card now lands on the dialog's own
/// scrolling body rather than on bare card — which is still not the grid,
/// which is all these tests claim.
const THREE_NOTCHES_DOWN: &str = "wheel.700,400,-180";

/// The rename field's vertical centre inside the Copy Picks card. From the
/// card top at y=217 above: 18 px padding, the title row, 10 px spacing, the
/// 34 px destination row, 10 px spacing, then the 28 px field — measured at
/// y 311..338. Probed, not computed: the title's height is a font metric,
/// which is also why the strand that uses this is gated on
/// `menu_clicks_are_calibrated()`.
const RENAME_FIELD_Y: u32 = 324;

/// The shared assertions. `dialog` is the QEDUMP field that says this
/// dialog is up (`copy` / `clip`).
fn assert_wheel_over_the_dialog_is_swallowed(stderr: &str, dialog: &str) {
    let vpy = |label: &str| dump_field(qedump(stderr, label), "vpy");
    assert_eq!(
        vpy("prewheel"),
        "-180.0",
        "the wheel never reached the grid, so nothing below proves anything \
         ({dialog}):\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(stderr, "open"), dialog),
        "true",
        "the {dialog} dialog did not open:\n{stderr}"
    );
    assert_eq!(
        vpy("open"),
        "-180.0",
        "opening the {dialog} dialog moved the grid:\n{stderr}"
    );
    // The contract.
    assert_eq!(
        vpy("wheeled"),
        "-180.0",
        "a wheel over the {dialog} dialog scrolled the grid behind it \
         (issue #49):\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(stderr, "closed"), dialog),
        "false",
        "Esc did not close the {dialog} dialog:\n{stderr}"
    );
    assert_eq!(
        vpy("closed"),
        "-180.0",
        "closing the {dialog} dialog replayed the swallowed scroll:\n{stderr}"
    );
    // Non-vacuity: the same token, the same coordinates, no dialog.
    assert_eq!(
        vpy("control"),
        "-360.0",
        "the control wheel did not move the grid either, so the assertions \
         above are vacuous ({dialog}):\n{stderr}"
    );
}

/// Copy Picks: pick a frame, Ctrl+E, wheel. `--synthetic 300` because the
/// contract is about the scrim, not about the files — 300 cells give the
/// grid far more room than the two scrolls this drives need. The `y` is
/// the state a user actually reaches Ctrl+E from; the dialog opens either
/// way (its emptiness is fileops.md's business, not this test's).
///
/// This half also wheels over the RENAME FIELD, the one child of either
/// card that owns a `TextInput`: over bare card the scrim is provably the
/// only thing that can swallow a scroll, but over a text input a green
/// assertion could be the child's doing. The click-then-keystroke before
/// it is the calibration guard — if the coordinate misses the field, the
/// character never lands in `template` and the test says so instead of
/// asserting over the wrong element.
///
/// That strand is gated like the menu-click tests (`!cfg!(windows)` —
/// CI's matrix is Linux and Windows, so in practice Linux only)
/// and for the same reason: the rows ABOVE the field include a Text whose
/// height is a font metric, so the field's y drifts with the platform
/// font. It is not merely gated but not DRIVEN off Linux — a click that
/// drifted 40 px up would hit "Choose…" and raise the native folder
/// picker, which no headless run can dismiss. The bare-card contract is
/// font-independent (deep inside a 480 px centred card) and still runs
/// everywhere.
#[test]
fn a_wheel_over_the_copy_dialog_never_scrolls_the_grid_behind_it() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i49-copy-wheel.jpg");
    let over_the_field = menu_clicks_are_calibrated();
    let field_steps = if over_the_field {
        format!(
            "4300:click.700,{fy};4500:key:t;4700:dump.field;\
             5000:wheel.700,{fy},-180;5700:dump.overfield;",
            fy = RENAME_FIELD_Y
        )
    } else {
        String::new()
    };
    let script = format!(
        "{PIN_WINDOW};1600:key:y;1900:{w};2400:dump.prewheel;\
         2700:key:ctrl+e;3000:dump.open;\
         3300:{w};4000:dump.wheeled;\
         {field_steps}\
         6000:key:escape;6300:dump.closed;\
         6600:{w};7300:dump.control",
        w = THREE_NOTCHES_DOWN
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // The bare-card case first: it is the primary contract, and a scrim
    // that leaks fails HERE rather than in the child case below, where the
    // message would send the reader after the wrong element.
    assert_wheel_over_the_dialog_is_swallowed(&stderr, "copy");
    if !over_the_field {
        eprintln!("over-the-field strand skipped: uncalibrated card geometry");
        return;
    }
    // The pointer really is on the rename field: the click focused it and
    // the keystroke landed there rather than in the dialog's key scope.
    assert_eq!(
        dump_text(qedump(&stderr, "field"), "template"),
        "t",
        "the click missed the rename field, so the wheel below would be \
         over the wrong element:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "overfield"), "vpy"),
        "-180.0",
        "a wheel over the rename field scrolled the grid behind the dialog \
         (issue #49):\n{stderr}"
    );
}

/// Export Frames as Video: the same contract on the fourth scrim. A REAL
/// folder, because the export offer is off for a session with no paths —
/// 120 tiny synthetic RAWs rather than symlinked A1 files, so the grid is
/// deep enough to scroll at the default zoom without paying for 120
/// full-size preview decodes (the three real RAWs of `testdata/raws` fill
/// less than one screen and do not scroll at ANY zoom, which would make
/// the control vacuous). Nothing is exported here: the dialog opens with
/// no destination, which is all the wheel needs to be over.
///
/// The settled-sort gate (ui-grid.md) still matters here even though no
/// positional key is driven: the load-settled edge WRITES `vp_y` itself
/// (`presenter.rs`, via `grid::scroll_after_resort`), so a re-sort landing
/// between two dumps would move the very number this test reads. The
/// script therefore WAITS for the settle before the first wheel
/// (2026-09-03) instead of resting on a margin — 120 kilobyte fixtures
/// settle at ~160 ms against a 1,900 ms wheel today, but a margin is a
/// guess about a runner and this one is 120 file reads wide. The
/// assertion below reads the same ordering off the log, so a wait that is
/// ever tidied away still fails loudly rather than silently.
#[test]
fn a_wheel_over_the_export_dialog_never_scrolls_the_grid_behind_it() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("i49-clip-src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..120 {
        write_synthetic_raw(&src.join(format!("f{i:03}.ARW")), 160, 120, 1, 512);
    }
    let out = out_dir().join("i49-clip-wheel.jpg");
    let script = format!(
        "{PIN_WINDOW};1600:select-all;1800:wait:load settled gen 0;1900:{w};2400:dump.prewheel;\
         2700:key:ctrl+shift+e;3000:dump.open;\
         3300:{w};4000:dump.wheeled;\
         4300:key:escape;4600:dump.closed;\
         4900:{w};5600:dump.control",
        w = THREE_NOTCHES_DOWN
    );
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    std::fs::remove_dir_all(&src).ok();
    // The re-sort's own `vp_y` write is behind us before the first wheel,
    // now by construction: the script waits for the settle. This reads the
    // same fact off the log as an ORDERING — the settle line before the
    // first wheel's echo — so it still binds if the wait is ever taken
    // out, and a slow settle delays the wheel instead of failing the run.
    // The old form compared the settle's trace clock against the scripted
    // 1900 and would have gone red on exactly the runner the wait exists
    // for.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the first wheel \
         was timed, not gated:\n{stderr}"
    );
    let settled_at = stderr.find("load settled gen 0").unwrap_or_else(|| {
        panic!("the view never settled, so the sort could still move it:\n{stderr}")
    });
    let first_wheel = stderr
        .find("drive: wheel.")
        .unwrap_or_else(|| panic!("the first wheel never ran:\n{stderr}"));
    assert!(
        settled_at < first_wheel,
        "the sort settled after the first wheel — `vpy` below would be the \
         re-sort's number, not the scrim's:\n{stderr}"
    );
    assert_wheel_over_the_dialog_is_swallowed(&stderr, "clip");
}

/// The shared `ModalScrim` (About, Keyboard Shortcuts): the arm the two
/// hand-rolled copies were missing. It was never broken, but nothing
/// tested it either — so "the other two hold by construction" rested on
/// reading the component, and a future edit to it would take About and the
/// shortcuts popup down with no test saying so.
///
/// It also keeps the nav-token modal mirror under test (issue #13): the
/// containment tests moved to real keys and real menu items, and the two
/// assertions that were about the HARNESS rather than the app — the
/// `about toggled to true` line and the "drive swallowed by modal" count —
/// moved here, to the test that still opens a popup by token. Driven nav
/// actions must keep dying at that mirror, or every token-driven script in
/// the suite silently starts marking photographs behind a scrim.
///
/// One run covers both call sites and both card shapes: the shortcuts card
/// (780x549 on this seat, clicks pass through to the scrim) and About (480x348,
/// `card-eats-clicks`, whose extra `TouchArea` has no `scroll-event` arm of
/// its own — the wheel has to fall through it to the scrim below, which is
/// the same "over a child" question the copy dialog's rename field asks).
/// `wheel.700,400` is inside both cards at the pinned window size — and
/// over the shortcuts card it now lands on the non-interactive `Flickable`
/// that wraps that card's body, which consumes the wheel itself. Still not
/// the grid, which is all this test claims.
#[test]
fn a_wheel_over_the_help_popups_never_scrolls_the_grid_behind_them() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i49-popup-wheel.jpg");
    let script = format!(
        "{PIN_WINDOW};1600:{w};2100:dump.prewheel;\
         2400:about;2700:dump.aboutup;2800:reject;2900:pick;\
         3000:{w};3700:dump.aboutwheeled;\
         4000:key:escape;4300:shortcuts;4600:dump.shortcutsup;\
         4900:{w};5600:dump.shortcutswheeled;\
         5900:key:escape;6200:dump.closed;6500:{w};7200:dump.control",
        w = THREE_NOTCHES_DOWN
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // The nav-token modal mirror, which lives here because this is where
    // the tokens still legitimately open a popup (issue #13: the two
    // containment tests moved to real keys, and these two assertions —
    // the toggle's own trace, and the harness swallowing driven nav
    // actions while a modal is up — would otherwise have been dropped
    // rather than moved). It is the MIRROR, not the FocusScope: what the
    // shipped guard does with a real keystroke is asserted in
    // `about_dialog_renders_and_contains_the_keyboard`.
    assert!(
        stderr.contains("about toggled to true"),
        "the `about` drive token did not report opening the dialog:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("drive swallowed by modal").count(),
        2,
        "the driven reject/pick were not both swallowed while About was \
         up:\n{stderr}"
    );
    let vpy = |label: &str| dump_field(qedump(&stderr, label), "vpy");
    assert!(
        dump_text(qedump(&stderr, "closed"), "status").contains("★0 ✕0"),
        "a driven mark leaked through the About modal:\n{stderr}"
    );
    assert_eq!(
        vpy("prewheel"),
        "-180.0",
        "the wheel never reached the grid, so nothing below proves \
         anything:\n{stderr}"
    );
    for (up, wheeled, flag) in [
        ("aboutup", "aboutwheeled", "about"),
        ("shortcutsup", "shortcutswheeled", "shortcuts"),
    ] {
        assert_eq!(
            dump_field(qedump(&stderr, up), flag),
            "true",
            "the {flag} popup did not open:\n{stderr}"
        );
        assert_eq!(
            vpy(up),
            "-180.0",
            "opening the {flag} popup moved the grid:\n{stderr}"
        );
        assert_eq!(
            vpy(wheeled),
            "-180.0",
            "a wheel over the {flag} popup scrolled the grid behind it \
             (issue #49):\n{stderr}"
        );
    }
    let closed = qedump(&stderr, "closed");
    assert_eq!(dump_field(closed, "about"), "false", "{closed}");
    assert_eq!(dump_field(closed, "shortcuts"), "false", "{closed}");
    assert_eq!(
        dump_field(closed, "vpy"),
        "-180.0",
        "closing the popups replayed a swallowed scroll:\n{stderr}"
    );
    // Non-vacuity: the same token, the same coordinates, no popup.
    assert_eq!(
        vpy("control"),
        "-360.0",
        "the control wheel did not move the grid either, so the assertions \
         above are vacuous:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Pointer ROUTING (issue #13): which Slint surface receives a physical
// click, drag or wheel — the question every previous test had to answer by
// reading the .slint file. The primitives that make it answerable
// (`click.`, `press./move./release.`, `wheel.`, `dump.`) all exist now, so
// these four tests drive real dispatched events through Slint's own
// hit-testing and assert what the app did with them.
//
// House rules, all four: the window is pinned first (coordinates are
// geometry, and geometry is only that geometry at 1440x900); every click
// that must LAND carries an intermediate assertion that fails loudly and
// specifically when it misses; every "nothing happened" claim is paired
// with a control in the same run that proves the same token DOES do
// something when it should, so a dead pointer path can never buy a green.
//
// The panel tests wait on `iptc field 0 laid out at 1150` — the row's x at
// the pinned width and at no other, so the wait means "the window really
// is the size these coordinates were measured in, and the panel is laid
// out in it". A `resize:` is a request to the compositor, which under load
// takes its time answering (issue #61).
// ---------------------------------------------------------------------------

/// Issue #12's deferral, finally driven: a click inside the docked IPTC
/// panel must not reach the grid. The panel's first child is a bare
/// `TouchArea` whose whole job is to eat clicks that would otherwise fall
/// through to a cell — where they would move the cursor and collapse a
/// multi-selection in the middle of keywording it, which is the shape the
/// issue describes. Nothing tested it: `cell-clicked` fires from Slint's
/// hit-test, so only a real dispatched press can ask the question.
///
/// Two clicks, because the panel has two kinds of surface: bare chrome
/// (its padding strip) and an editor (the Title field, which must take
/// the keyboard and still not touch the cursor).
///
/// What the chrome click actually discriminates, measured by mutation:
/// the panel is protected TWICE and the test binds on the conjunction.
/// Removing the containment `TouchArea` alone leaves it green — the grid's
/// Flickable is only `grid-width` wide, so there is no cell under the
/// panel to reach. Extending the grid under the panel alone (issue #12's
/// docking bug) leaves it green too — the containment `TouchArea` eats the
/// press. With BOTH, the click lands on a cell and this test fails at the
/// cursor assertion. That is the honest shape of the guarantee, and worth
/// knowing: whoever removes one layer will find the other one holding.
#[test]
fn a_click_inside_the_iptc_panel_never_reaches_the_grid() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i13-panel-click.jpg");
    // 300 synthetic cells: the panel docks over what would otherwise be
    // grid, and a selection of 300 makes a leaked `cell-clicked` unmissable
    // (a plain click collapses the whole selection).
    //
    // The click on a CELL sets the cursor the panel clicks must not move,
    // and gives the selection something to be collapsed FROM — a plain
    // cell click clears it, so `select-all` comes after.
    //
    // Both cell coordinates are interiors of the PANEL-OPEN layout (grid
    // 1140 px wide: 8 columns of 135.75 px on a 141.75 px pitch, rows
    // 90.5 px on 96.5), which is not the same grid as before the panel
    // docked — 358,318 is cell 18's middle and 783,511 is cell 37's.
    let script = format!(
        "{PIN_WINDOW};1200:key:i;1300:wait:iptc field 0 laid out at 1150;\
         1700:click.358,318;2000:select-all;\
         2300:dump.before;2600:click.1145,400;3000:dump.chrome;\
         3300:click:iptc field 0;3600:key:t;3800:key:return;4100:dump.field;\
         4400:click.783,511;4800:dump.control"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // The wait really gated the clicks (a dropped token would silently put
    // the schedule back on the clock).
    assert!(
        stderr.contains("wait:iptc field 0 laid out at 1150 (satisfied"),
        "the `wait:` step never fired — the clicks were timed, not gated:\n{stderr}"
    );
    let before = qedump(&stderr, "before");
    assert_eq!(
        dump_field(before, "iptc"),
        "true",
        "the panel never opened, so no click below is inside it:\n{stderr}"
    );
    assert_eq!(
        dump_field(before, "selected"),
        "300",
        "select-all did not select the view, so a leaked grid click would \
         have nothing to collapse:\n{stderr}"
    );
    assert_eq!(
        dump_text(before, "revert"),
        "",
        "something had already armed the revert slot, so the field click's \
         proof below is not its own:\n{stderr}"
    );
    // Re-enabled with the issue #63/#64 fix (it was left out while opening
    // the panel with a real `I` stranded the keyboard about one run in
    // eight, so a POINTER-ROUTING test would have gone red for a focus bug
    // it is not about) — but through the OWNER TOKEN, never `keysfocus`:
    // that property reads false whenever the WINDOW is deactivated, with
    // the keyboard perfectly alive, and planting it here would trade one
    // borrowed flake for another. What it pins is bounded and worth
    // stating: the cell click at 358,318 above claims the keyboard
    // itself, so this says "nothing between the `I` and here left the
    // keyboard on a destroyed editor", not "the `I` alone kept it".
    assert_eq!(
        dump_field(before, "focusowner"),
        "0",
        "the keyboard is not on the main scope with the panel open — an \
         editor or a destroyed row holds it (issue #41 family, #63/#64):\n{stderr}"
    );
    let cursor_before = dump_field(before, "cursor");
    // Calibration, against the rectangle the app itself reported. The
    // chrome click is in the panel's own 10 px padding strip, LEFT of every
    // field row: the one place in the panel where nothing but the
    // containment TouchArea stands between the pointer and the grid.
    // Over the field column the fields Flickable and the editors absorb
    // presses themselves — proven by mutation (removing the containment
    // TouchArea *and* extending the grid under the panel leaves a click at
    // x=1200 still absorbed, while this one reaches a cell), so a chrome
    // click there would assert their doing, not the panel's.
    let f0 = iptc_field_rect(&stderr, 0, "drive: click.1145,400");
    assert!(
        (f0.0 - 10.0..f0.0).contains(&1145.0),
        "the chrome click at x=1145 is not in the panel's padding strip \
         (x {}..{}) — the panel padding or dock width changed:\n{stderr}",
        f0.0 - 10.0,
        f0.0
    );
    assert_click_resolved(&stderr, "iptc field 0");
    // The contract: neither click moved the cursor or touched the selection.
    for label in ["chrome", "field"] {
        let dump = qedump(&stderr, label);
        assert_eq!(
            dump_field(dump, "cursor"),
            cursor_before,
            "a click on the panel's {label} moved the cursor — it reached a \
             grid cell (issue #12):\n{stderr}"
        );
        assert_eq!(
            dump_field(dump, "selected"),
            "300",
            "a click on the panel's {label} collapsed the selection — it \
             reached a grid cell (issue #12):\n{stderr}"
        );
    }
    // The other half a cursor assertion cannot tell: the field click landed
    // ON the field. Proven by what a user would call proof — the `t` and
    // the Enter after it COMMITTED a Title across the selection, which
    // arms the revert slot. A click that missed the LineEdit (or was eaten
    // by the containment TouchArea beneath it) sends those two keys to the
    // main scope, where `t` is not a binding and nothing arms.
    //
    // Proven through the COMMIT rather than through `keysfocus`, and it
    // stays that way now that the keyboard assertion is back at the
    // `before` dump: these two are different questions. `keysfocus` says
    // some element owns the keyboard; only the commit says the pointer
    // landed on THIS field. The commit is also immune to the focus
    // question — Enter returns focus to the grid either way.
    assert_ne!(
        dump_text(qedump(&stderr, "field"), "revert"),
        "",
        "typing after the Title-field click committed nothing — the click \
         missed the field:\n{stderr}"
    );
    // The control, same token, over the grid: a click there DOES move the
    // cursor and collapse the selection. Without it every assertion above
    // would also pass on a build where no click reaches anything.
    let control = qedump(&stderr, "control");
    assert_ne!(
        dump_field(control, "cursor"),
        cursor_before,
        "the control click over the grid moved nothing either — the \
         assertions above are vacuous:\n{stderr}"
    );
    assert_eq!(
        dump_field(control, "selected"),
        "0",
        "the control click over the grid did not collapse the selection — \
         the assertions above are vacuous:\n{stderr}"
    );
}

/// The wheel ROUTING table, over the three surfaces that must not scroll
/// the grid and the one that must. Which element receives a physical wheel
/// is Slint's hit-test answer, not the app's: the fit surface, the overlay
/// scrollbar and the docked panel each sit over (or beside) the grid's
/// Flickable, and the only previous evidence that a wheel over them leaves
/// the grid alone was a reading of `main.slint`.
///
/// The zoom half of the table — one notch up at fit enters the ladder, one
/// more climbs it — lives in `overlay_wheel_still_zooms_one_stop_per_notch`
/// (it needs a real RAW's zoom ceiling). What this test adds is where the
/// wheel does NOT go, plus the inert direction at fit.
///
/// `--synthetic 300`: 38 rows of cells, so no scroll a script drives lands
/// on the Flickable's bottom clamp, where "unmoved" would mean "out of
/// room". A synthetic session also has no metadata to stream, so the
/// settled-sort gate the positional-nav idiom demands does not apply here
/// (the re-sort edge writes `vp_y` itself, and there is no re-sort) —
/// the same reason the issue #49 dialog tests on `--synthetic` carry no
/// such guard while the real-folder one does. Each swallowing surface is
/// compared against a dump taken after
/// the state change that precedes it, never against the number from before
/// it — opening the panel legitimately re-anchors the viewport (the pitch
/// changes with the grid width), and a comparison across that would be
/// asserting the re-anchor, not the wheel.
#[test]
fn the_wheel_routing_table_holds_over_every_surface() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i13-wheel-routing.jpg");
    let script = format!(
        "{PIN_WINDOW};1600:{w};2100:dump.grid;\
         2400:wheel.1430,400,-180;2900:dump.sb;\
         3200:key:i;3300:wait:iptc field 0 laid out at 1150;3700:dump.panelopen;\
         4000:wheel.1250,400,-180;4500:dump.panel;\
         4800:key:i;5100:key:+;5200:key:+;5300:key:+;5400:key:+;5500:key:+;\
         5900:dump.loupe;6200:{w};6700:dump.fitwheel;\
         7000:key:g;7400:dump.grid2;7700:{w};8200:dump.control",
        w = THREE_NOTCHES_DOWN
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // The wait really gated the clicks (a dropped token would silently put
    // the schedule back on the clock).
    assert!(
        stderr.contains("wait:iptc field 0 laid out at 1150 (satisfied"),
        "the `wait:` step never fired — the clicks were timed, not gated:\n{stderr}"
    );
    let vpy = |label: &str| dump_field(qedump(&stderr, label), "vpy");
    // Row 1: over the grid, the wheel scrolls it. Everything below reads
    // against this — a dead `wheel.` token would make the rest vacuous.
    assert_eq!(
        vpy("grid"),
        "-180.0",
        "three notches over the grid did not scroll it:\n{stderr}"
    );
    // Row 2: over the overlay scrollbar. Its TouchArea swallows the scroll
    // deliberately (a wheel there is not loupe input and must not fall
    // through to the fit surface either).
    assert_eq!(
        vpy("sb"),
        "-180.0",
        "a wheel over the overlay scrollbar scrolled the grid:\n{stderr}"
    );
    // Row 3: over the docked IPTC panel. Two things could break this — the
    // panel letting the wheel through, or the grid extending under the
    // panel again (issue #12's docking bug, where the Flickable really was
    // beneath these pixels).
    assert_eq!(
        dump_field(qedump(&stderr, "panelopen"), "iptc"),
        "true",
        "the panel never opened, so the wheel below was over the grid:\n{stderr}"
    );
    assert_eq!(
        vpy("panel"),
        vpy("panelopen"),
        "a wheel over the IPTC panel scrolled the grid beside it:\n{stderr}"
    );
    // Row 4: at loupe fit the wheel belongs to the zoom ladder, and DOWN
    // from fit is the reserved no-op of the pointer contract — it must
    // neither zoom out nor fall through and browse. At one column the
    // Flickable underneath has 300 screens of room, so "unmoved" is a real
    // claim here.
    let loupe = qedump(&stderr, "loupe");
    assert_eq!(
        dump_field(loupe, "zoom"),
        "6",
        "five zoom-ins did not reach the loupe (one column):\n{stderr}"
    );
    assert_eq!(
        vpy("fitwheel"),
        vpy("loupe"),
        "a wheel down at loupe fit browsed the grid behind the fit \
         surface:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "fitwheel"), "zf"),
        "1.000",
        "a wheel down at fit moved the zoom ladder — the reserved no-op \
         fired:\n{stderr}"
    );
    // The control: back at a grid zoom the same token still scrolls, so
    // none of the three "unmoved" rows above is a dead pointer path.
    let (grid2, control) = (vpy("grid2"), vpy("control"));
    assert_ne!(
        grid2, control,
        "the control wheel over the grid moved nothing either — the \
         swallowing assertions above are vacuous:\n{stderr}"
    );
}

/// Issue #11, first half: a DRAG over the grid scrolls it without
/// clicking the cell under it. That is Slint's own `clicked` definition
/// (press and release with no drag between), which ui-grid.md records as
/// "enforced by Slint" — a statement no test made until this one, and one
/// the app depends on completely: a grid drag that also moved the cursor
/// would silently re-cull the frame the user was only scrolling past.
///
/// The control comes FIRST, on purpose: a plain click on a cell moves the
/// cursor (so the pointer path is provably alive, and the coordinates are
/// provably cells), and the drag right after it must leave that cursor
/// exactly where the click put it. Doing it the other way round would
/// have to click after a drag, i.e. after a flick has scrolled the grid
/// by an amount no script can predict.
///
/// It is a dependency pin rather than a test of app code, which is also
/// why no app-side mutation can redden the "no click" half: claiming the
/// cursor from the cell's raw pointer release (`PointerEventKind.up`)
/// changes nothing, because once the Flickable takes the gesture the cell
/// stops receiving events at all — the suppression is a grab, not a
/// filter. What does redden it: making the Flickable non-interactive, and
/// shortening the drag below Slint's 8 px threshold — both fail the
/// "it scrolled" precondition, which is the same statement from the other
/// side.
#[test]
fn a_grid_drag_scrolls_without_clicking_the_cell_under_it() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i13-grid-drag.jpg");
    // The drag is four events over 180 ms: `click.`'s single-tick sequence
    // has no displacement and no elapsed time, which is why the separately
    // schedulable phases exist (issue #46) — but the span has to stay well
    // INSIDE Slint's own window, which is measured against a frame clock
    // that lags under load. A Flickable takes a gesture only if it passes
    // DISTANCE_THRESHOLD (8 px) within DURATION_THRESHOLD (500 ms,
    // `flickable.rs`). A 600 ms drag fits on an idle machine and lost that
    // race in debug under six spinners (~1 run in 10; release was clean),
    // so the moves land at +60/+120 ms and the release at +180: a third of
    // the budget, and still a real multi-event gesture 140 px long.
    //
    // 272,260 is the centre of cell 9 at 8 columns and scroll 0 (column
    // centres at 92.5 + 179.25c, row centres at 138 + 122r in window
    // coordinates). Centres, not "somewhere in the cell": x=900 sits in
    // the 6 px gutter between columns 4 and 5 and hits nothing at all —
    // measured.
    let script = format!(
        "{PIN_WINDOW};1200:click.272,260;1500:dump.clicked;\
         1800:press.630,504;1860:move.630,434;1920:move.630,364;\
         1980:release.630,364;2900:dump.dragged"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // The control: a click on a cell DOES move the cursor. Without it the
    // assertion below would also pass on a build where no pointer event
    // reaches the grid at all.
    let clicked = qedump(&stderr, "clicked");
    assert_eq!(
        dump_field(clicked, "cursor"),
        "9",
        "the control click did not land on cell 9 — the pointer path is \
         dead, or these coordinates are not a cell any more:\n{stderr}"
    );
    // The drag really was a drag: the Flickable took the gesture.
    let dragged = qedump(&stderr, "dragged");
    assert_ne!(
        dump_field(dragged, "vpy"),
        dump_field(clicked, "vpy"),
        "the press/move/release over the grid did not scroll it, so \
         'a drag does not click' is asserted about nothing:\n{stderr}"
    );
    // The contract: the cell under the press never got its `clicked`.
    assert_eq!(
        dump_field(dragged, "cursor"),
        "9",
        "a drag over the grid moved the cursor — the drag did not suppress \
         the click (issue #11):\n{stderr}"
    );
}

/// Issue #11, second half: two clicks far apart are two clicks, never a
/// double-click. Also Slint's, and also load-bearing: the app deliberately
/// holds NO proximity state of its own (the guard that did was deleted
/// after it vetoed every double-click above fit), so the whole rule is
/// `check_repeat` restarting the click count beyond 10 logical px
/// (`i-slint-core`'s `input.rs`). If a Slint upgrade changes that, the
/// persona's "eye, then beak, then wingtip" becomes a jump to 1:1 and only
/// this test says so.
///
/// No drag in this run, deliberately: the two rules used to share one
/// script, and a flick's scroll left every later coordinate landing
/// somewhere unpredictable — one run in twenty clicked into a gutter and
/// the far pair "missed the grid entirely". Two runs, two questions.
///
/// The near pair is the control and it gets its OWN point: with it reusing
/// the far pair's second point (three clicks on one cell), the pairing
/// sometimes did not happen — Slint restarts its click count whenever the
/// top item changes (`window.rs`), and under a Flickable's delayed
/// forwarding that identity is not stable across a gap. Both pairs share
/// the same 100 ms cadence, well inside `click_interval` (500 ms), so the
/// only difference between them is the distance the rule is about.
#[test]
fn two_distant_clicks_are_two_clicks_not_a_double_click() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i13-dblclick.jpg");
    let script = format!(
        "{PIN_WINDOW};1500:dump.pre;\
         1800:click.272,260;1900:click.809,504;2400:dump.far;\
         2900:click.451,626;3000:click.451,626;3500:dump.near"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let pre = qedump(&stderr, "pre");
    let far = qedump(&stderr, "far");
    // Both clicks landed (cell 9, then cell 28) — a pair that missed the
    // grid would prove nothing about pairing.
    assert_eq!(
        dump_field(far, "cursor"),
        "28",
        "the second distant click did not land on cell 28 — the pair \
         missed the grid:\n{stderr}"
    );
    // THE rule: 600 px apart, 100 ms apart — two cursor moves, no loupe.
    assert_eq!(
        dump_field(far, "zoom"),
        dump_field(pre, "zoom"),
        "two clicks 600 px apart opened the loupe — they were folded into \
         a double-click:\n{stderr}"
    );
    // The control: same cadence, one point, and THAT is a double-click.
    assert_eq!(
        dump_field(qedump(&stderr, "near"), "zoom"),
        "6",
        "two clicks on the same point did not open the loupe — the \
         double-click path is dead and the distance rule above proves \
         nothing:\n{stderr}"
    );
}

/// The follow-scroll claim (issues #16/#22), asserted POSITIVE for the
/// first time. At one column the visible image IS the cursor, so scrolling
/// the loupe moves the cursor — but only on a real scrollbar signal
/// (`sb-activity`), never on geometry moving underneath. Every existing
/// test asserts the claim does NOT fire; none could assert that it does,
/// because the flag is raised by the scrollbar's own `moved`/`clicked`
/// handlers and nothing headless could reach them. A `press./move./
/// release.` on the overlay scrollbar can, so this pins the claim's live
/// half: the trace line, the new cursor, and that the cursor really moved
/// far (the claim targets the centre row of the new viewport).
#[test]
fn a_scrollbar_drag_in_the_loupe_claims_the_cursor() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i13-sb-claim.jpg");
    // One column over 300 cells: the scrollbar exists (the viewport is one
    // image tall against 300 images of content) and the cursor's own cell
    // leaves the viewport long before the thumb reaches mid-track, which
    // is the claim's precondition.
    let script = format!(
        "{PIN_WINDOW};1300:key:+;1400:key:+;1500:key:+;1600:key:+;1700:key:+;\
         2100:dump.loupe;\
         2400:press.1430,80;2600:move.1430,300;2800:move.1430,500;\
         3000:release.1430,500;3400:dump.dragged"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let loupe = qedump(&stderr, "loupe");
    assert_eq!(
        dump_field(loupe, "zoom"),
        "6",
        "five zoom-ins did not reach the loupe, so the scrollbar drag below \
         is a GRID scroll and claims nothing:\n{stderr}"
    );
    // The drag reached the scrollbar: the viewport moved. (A press outside
    // it would leave `vpy` alone and the claim would be asserted about a
    // scroll that never happened.)
    let dragged = qedump(&stderr, "dragged");
    assert_ne!(
        dump_field(dragged, "vpy"),
        dump_field(loupe, "vpy"),
        "the scrollbar drag did not scroll the loupe — the press missed the \
         bar (x 1422..1440 at this window size):\n{stderr}"
    );
    // THE claim: scrolling the loupe with the bar moves the cursor with it.
    assert!(
        stderr.contains("follow-scroll claim: cursor pos "),
        "a scrollbar drag at one column never claimed the cursor — the \
         positive half of the sb-activity gate is dead (issues \
         #16/#22):\n{stderr}"
    );
    assert_ne!(
        dump_field(dragged, "cursor"),
        dump_field(loupe, "cursor"),
        "the follow-scroll claim traced but the cursor did not move:\n{stderr}"
    );
}
