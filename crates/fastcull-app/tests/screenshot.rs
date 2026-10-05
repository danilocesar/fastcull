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
    on_line: impl FnMut(&str) + Send + 'static,
) -> String {
    shoot_child(args, envs, out, on_line, false)
}

/// A run WITH the default thumbnail cache — and only in a sandbox: every
/// other driven run is FASTCULL_NO_CACHE so the user's real cache is never
/// touched, and this one may drop that only because the cache dir it
/// resolves is inside the shots dir. It REFUSES to run unless `envs` points
/// both `HOME` and `XDG_CACHE_HOME` under `out_dir()` (the `directories`
/// crate resolves the cache dir from `XDG_CACHE_HOME`, then `$HOME/.cache`,
/// on Linux — the one platform where this redirect exists; Windows asks its
/// known-folder API, which ignores the environment).
fn shoot_with_sandboxed_cache(args: &[&str], envs: &[(&str, &str)], out: &Path) -> String {
    shoot_with_sandboxed_cache_watching(args, envs, out, |_| {})
}

/// [`shoot_with_sandboxed_cache`] with [`shoot_env_stderr_watching`]'s live
/// view of the trace: the same refusal, and `on_line` on the drain thread,
/// which must not block.
fn shoot_with_sandboxed_cache_watching(
    args: &[&str],
    envs: &[(&str, &str)],
    out: &Path,
    on_line: impl FnMut(&str) + Send + 'static,
) -> String {
    let sandbox = out_dir();
    for var in ["HOME", "XDG_CACHE_HOME"] {
        let inside = envs
            .iter()
            .find(|(k, _)| *k == var)
            .is_some_and(|(_, v)| Path::new(v).is_absolute() && Path::new(v).starts_with(&sandbox));
        assert!(
            inside,
            "refusing a run with the cache on: {var} must point under {} so the \
             default cache resolves into the sandbox, never the user's real one",
            sandbox.display()
        );
    }
    shoot_child(args, envs, out, on_line, true)
}

/// The one body every app spawn goes through — the watchdog, the drain and
/// the shutter check of `shoot_env_stderr_watching` — with the thumbnail
/// cache off unless `with_cache` (only `shoot_with_sandboxed_cache` passes
/// true).
fn shoot_child(
    args: &[&str],
    envs: &[(&str, &str)],
    out: &Path,
    mut on_line: impl FnMut(&str) + Send + 'static,
    with_cache: bool,
) -> String {
    let bin = env!("CARGO_BIN_EXE_fastcull-app");
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args).arg("--screenshot").arg(out);
    if with_cache {
        // Any value of FASTCULL_NO_CACHE counts as set, so one inherited
        // from the shell must go rather than be overridden.
        cmd.env_remove("FASTCULL_NO_CACHE");
    } else {
        cmd.env("FASTCULL_NO_CACHE", "1"); // never touch the user's real cache
    }
    // Never read or write the user's real ui.toml either (issue #13 gap,
    // surfaced by the issue #41 sweep): a driven copy dialog otherwise shows
    // the user's real remembered destination.
    cmd.env("FASTCULL_NO_CONFIG", "1")
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
            // Exactly one capture per run (issue #77): a second
            // `status at shutter` means the poll ran again after the
            // capture, and the run photographed a state half a second
            // later than the one CI photographs — on a Wayland seat that
            // was EVERY stock-debug run until 2026-09-05, and never CI.
            // When this fails this way it is that defect; do not quiet it,
            // do not gate it on a platform, and do not "fix" it by reading
            // the last line somewhere else. This is the one function every
            // app spawn goes through, so every driven test enforces it.
            // A run without FASTCULL_TRACE prints no mark at all, so the
            // expected count is 1 when traced and 0 otherwise.
            //
            // Counted as EMITTED MARK LINES, not substring occurrences
            // (senior-developer test-integrity review 2026-09-05, on a QE
            // probe): a session whose file is named `status at shutter:
            // a.ARW` puts the mark's own text into the status string and
            // into a QEDUMP field, and the substring form counted 3 where
            // one mark was emitted — a banner that says "do not doubt this
            // failure" must not be able to fire on a file name.
            let traced = envs.iter().any(|(k, _)| *k == "FASTCULL_TRACE")
                || std::env::var_os("FASTCULL_TRACE").is_some();
            let want = usize::from(traced);
            let shots = mark_lines(&stderr, "status at shutter: ");
            let geoms = mark_lines(&stderr, "geometry at shutter: ");
            assert!(
                shots == want && geoms == want,
                "the shutter fired {shots} time(s) with {geoms} geometry mark(s), \
                 expected {want}; a --screenshot run photographs exactly once \
                 (issue #77); trace: {}",
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

/// Emitted mark lines, not substring occurrences: a mark is one line
/// `fastcull-trace: [<ms>] <label>` (trace.rs `emit`, the one emit site),
/// so anchoring on where the label starts makes a file name that quotes
/// the mark — through the status string or a QEDUMP field — count for
/// nothing (QE probe 2026-09-05: `status at shutter: a.ARW` counted 3 by
/// substring, 1 by line).
fn mark_lines(stderr: &str, mark: &str) -> usize {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("fastcull-trace: ["))
        .filter_map(|r| r.split_once("] "))
        .filter(|(_, label)| label.starts_with(mark))
        .count()
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
/// line is emitted only by the sharp full-res branch, so it depends on a
/// 50 MP decode landing inside the drive window — which in the debug
/// profile CI runs `cargo test --workspace` in, on Windows too, it simply
/// did not do before 2026-09-05 (a stock-profile decode took tens of
/// seconds; issue #76 made dependencies compile optimised in debug, so the
/// line may exist there now). The status bar needs no decode at all, which
/// is why this test reads it: it carries BOTH facts in one string, in
/// every profile and at any decode speed.
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
/// profiles (release ~140ms cooks vs 60ms key spacing; in debug ~12s
/// cooks before 2026-09-05 and ~1-2 s since dependencies compile
/// optimised there (issue #76) — either way far past the key spacing,
/// with the virgin-pin rule rendering soft on mid adoption).
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
    // ~140ms against 60ms key spacing; debug cooked ~12 s before
    // 2026-09-05 and ~1-2 s since (issue #76) — either way past the key
    // spacing — and the virgin-pin rule renders soft the moment the
    // landing mid adopts, long before the shutter's sharp gate opens.
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
/// the soft regime, so only the post-toggle stability half applies
/// (measured 2026-09-05, senior-developer review F2, now that
/// dependencies compile optimised in debug: PR #80's Windows debug
/// artifact has the sharp `loupe idx 0 factor` at 2776 ms against
/// toggles at 2003 and 2604 ms — it holds there by 172 ms — and by
/// 0.2-0.6 s on the development seat) —
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
    // purpose: the same mark landed at 28.3 s on the Windows debug runner
    // and the harness's 30 s wait cap runs from the STEP, so a wait at
    // 1600 would have ended those runs at ~31.6 s — while the debug half
    // asserts only post-close stability and is content in the soft
    // regime. That 28.3 s is HISTORICAL (stock dev profile, before
    // 2026-09-05): dependencies compile optimised in debug now (issue
    // #76), so the mark lands far earlier, and the split is re-timed on
    // the PR's Windows debug artifacts rather than guessed at here. Edit
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
///
/// Tab and Shift+Tab are among the swallowed keys (brief 011, AC4;
/// ui-grid.md "Modal keyboard containment": About holds no control that
/// takes the keyboard): pressed first, they leave the owner token on the
/// main scope and the window's own Tab walk never runs. No code stands
/// behind this — the pre-fix build passes it too, which is the claim.
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
        "{open};1300:dump.up;1400:key:tab;1500:key:shift+tab;1600:key:n;1900:key:p;\
         2300:dump.contained;\
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
    // THE containment: Tab, Shift+Tab and two real keystrokes, no mark.
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
    assert_eq!(
        dump_field(contained, "focusowner"),
        "0",
        "Tab or Shift+Tab under About moved the keyboard off the main scope \
         — About holds no control, so both are swallowed like every other \
         key (ui-grid.md, \"Modal keyboard containment\"; brief 011 AC4):\n{stderr}"
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
/// was testing the harness rather than the app). Tab and Shift+Tab are
/// swallowed here too (brief 011, AC4: the card holds no control that
/// takes the keyboard) — a claim the pre-fix build already met.
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
        "{open};1300:dump.up;1400:key:tab;1500:key:shift+tab;1600:key:n;\
         2000:dump.contained;\
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
        dump_field(contained, "focusowner"),
        "0",
        "Tab or Shift+Tab under the shortcuts card moved the keyboard off the \
         main scope — the card holds no control, so both are swallowed like \
         every other key (ui-grid.md, \"Modal keyboard containment\"; brief \
         011 AC4):\n{stderr}"
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
    // the two windows show the same 29 rows at the same 780 px, so the
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
/// the sum of ~29 text line boxes and belongs to whatever face the seat
/// draws with (568 px in this machine's Noto Sans; the per-face spread
/// measured on the 27-row card of 2026-09-04 was 491 in Liberation Sans,
/// 512 in Nimbus Sans / Carlito / Cantarell, 525 in Montserrat, 627 in
/// Noto Sans Mono); pinning it, or a band around it, pins a font. What the card actually promises is a
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
/// Not the sharp rung: the claim under test is the overlay's, not the top
/// rung's, and a full-res decode on a loaded machine can be seconds behind
/// the first rung either way. (The stronger form of that reason — a
/// debug-build 50 MP decode taking tens of seconds, which is what made the
/// M1 tests release-only — is history since 2026-09-05: issue #76,
/// 01-architecture.md "Build profiles".)
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
/// which must RE-RAISE the moment any rung of the new image lands; the
/// "landed" dump is gated on the sharp rung's own mark and fires 6.3 s
/// behind it, so it covers both timelines by construction, not by a
/// clock).
///
/// RED on pre-fix code (+ the drive-harness commit): `one2one=false` at
/// the mid-gap dump (the overlay dropped and the strip showed the whole
/// frame at fit), a "loupe overlay dropped … (no rung in hand)" trace —
/// the excuse-less drop, which post-fix is structurally impossible —
/// and neither a "loupe hold" nor a "loupe thumb" render anywhere.
///
/// BOTH PROFILES since 2026-09-05 (issue #76). Until then this test ran
/// in RELEASE ONLY (validator, gate round 2): in a debug build the run
/// rode the app's own 60 s screenshot-readiness cap — the cursor's 50 MP
/// decode plus ten thumb jobs plus the cook hold landed at 58.5 s on a
/// loaded 8-core laptop, so under contention (or on a CI runner, which
/// the audit of 2026-09-04 measured at 4 vCPU where that line said 2)
/// the app exited 1 at the cap before the shutter could fire. That
/// 58.5 s was the JPEG decoder — a dependency — compiled at opt-level 0;
/// dependencies compile optimised in the dev profile now
/// (01-architecture.md, "Build profiles"), the same decode lands in
/// about 2 s, and the deferral has nothing left to rest on.
///
/// What the lift exposed (senior-developer diagnosis 2026-09-05): the
/// recovery pin was a CLOCK — `dump.landed` at 26.5 s, 6.45 s after the
/// End — racing the debug KITCHEN, which is workspace code at opt-level
/// 0. Under the #76 load recipe (six spinners and the app on two cores)
/// the new cursor's rescue rungs queue behind off-cursor 149 MB full-res
/// fills of 3.5-5.5 s each — the kitchen pops Full > Wrap > Thumb with
/// no notion of the cursor — so the hold cap fires at the next refresh
/// (the spec'd bounded drop, 14 of 14 loaded runs) and the first rung of
/// the new image lands 5.0-13.9 s after the End; the overlay re-raised
/// EVERY time, but in 8 of 11 runs after the clock had already
/// photographed the honest fit (0 of 4 green as written; the same script
/// in release under the same load: 3 of 3). The dump is gated on the
/// sharp rung's own mark now (`wait:loupe idx 8 factor` at 20.2 s, the
/// dump 6.3 s behind it — the CI-audit shape rule, echo asserted below),
/// which is stricter, not looser: the sharp must land within the wait's
/// 30 s cap and the overlay must be up 6.3 s later. The dump's authored
/// 26.5 s is its FLOOR, not a fallback: it fires 6.3 s after the wait is
/// satisfied, never earlier and never on its own (28.32-28.37 s idle,
/// 39.4-40.6 s under the recipe; a wait that never satisfies aborts the
/// run at 50.2 s with no dump at all — QE 2026-09-05, D3). Measured with the
/// wait on the development seat: debug 10 of 10 idle (the sharp 1.8-2.8 s
/// after the wait's step, no drop) and 3 of 3 under the recipe (9.3-14.4 s,
/// the hold cap firing in each); release 3 of 3 under the recipe
/// (1.6-2.1 s, no drop). The
/// thumb-rung pin below stays release-only for the same kitchen's sake.
/// `paced_taps` keeps the debug no-drop coverage — it asserts no `loupe
/// overlay dropped` at all; `transit_at_zoom_stays_soft` pins the soft
/// render and the sharp landing, not the absence of a bounded drop
/// (corrected 2026-09-05, senior-developer review F5: its own Windows
/// debug run of PR #80 carried a `(hold cap)` drop and passed).
#[test]
fn transit_to_a_cold_frame_keeps_the_overlay_at_the_carried_center() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
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
                "20000:dump.pre;20050:end;20130:dump.midgap;20200:wait:loupe idx 8 factor;26500:dump.landed",
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
    // The landing is GATED on the sharp rung's own mark (2026-09-05): the
    // steps behind a satisfied wait keep their gaps, so `dump.landed`
    // fires 6.3 s after the End target's sharp render whatever the
    // profile and the load. A dropped token or a renamed mark would put
    // the dump back on the clock in silence — the CI-audit shape rule
    // says assert the echo, so that fails loudly here instead.
    assert!(
        stderr.contains("wait:loupe idx 8 factor (satisfied"),
        "the `wait:loupe idx 8 factor` step never fired — the landing was \
         timed, not gated:\n{stderr}"
    );
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
/// (harness.rs `WAIT_CAP`) and a debug-profile full-res adoption landed
/// at 26-40 s on the Windows CI runner (30.3 s in this test's own run,
/// measured 2026-09-02), so the cap has to reach 50 s where the fixed
/// 45 s lead it replaced reached only 45. Those landing times are
/// HISTORICAL (stock dev profile, before 2026-09-05): dependencies
/// compile optimised in debug since issue #76, the adoption lands in
/// seconds, and the 20 s placement is simply satisfied when due — about
/// 18 s of idle schedule — until it is re-timed on the PR's Windows
/// debug artifacts. RELEASE puts the same step at
/// 1.5 s, because there the sharp mark lands in under half a second
/// (381, 454 and 457 ms across three release runs on this seat,
/// 2026-09-04, each wait then `satisfied after 0 ms`): its cap still
/// reaches 31.5 s, ~31 s of headroom over a decode that takes 0.45 s, and
/// the release run stops spending 18.5 s of dead clock: the script's last
/// step moves from 22.2 s to 3.7 s and the test measured 4.0 s of libtest
/// time in all three runs. What
/// each profile's wait covered was therefore different — debug waited for
/// a decode that could genuinely take half a minute, release for one
/// already done; since #76 both wait for a decode of seconds and only the
/// placement still differs — and the schedule behind it is the same in both,
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
    // profile only, on the perf_budgets precedent ALONE: this is a timing
    // pin — a decode raced against a clock — and timing pins bind in the
    // release profile. The reason first written here, "a debug build
    // decodes a mid slower than the tap cadence", stopped being true on
    // 2026-09-05, when dependencies started compiling optimised in the
    // dev profile (issue #76, 01-architecture.md "Build profiles"); the
    // gate stays for the precedent, and the no-drop and one2one
    // assertions above still bind in both profiles.
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
/// BOTH PROFILES since 2026-09-05 (issue #76): the debug skip that used
/// to stand here rested on the 60 s readiness cap and a full-res decode
/// of tens of seconds, and the dev profile's optimised dependencies took
/// that cost away (see the M1 test above and 01-architecture.md, "Build
/// profiles"). Measured in debug before the lift: 10 of 10 idle runs
/// green and 3 of 3 under the #76 load recipe.
///
/// THE KNOWN-FAILED PREMISE IS GATED ON THE APP'S OWN MARK (issue #101,
/// brief 010 R1; test-harness.md, "Rules for script authors"). Everything
/// after `dump.t1` reads a cursor the app KNOWS has failed — the first
/// End's decode failure must have arrived before the second End — and
/// until brief 010 nothing in the script waited for it: `dump.t1` stood on
/// the clock 250 ms after the first End. On a slow runner the failing
/// decode landed 2.3 s after that End, after the second End and its dump
/// (Windows debug, run 37086089258, 1 of 13 runs: `dump.t2` at 17152, the
/// `(decode failed)` drop at 17295, the badge at 17374 — the thumb
/// rendered at the second End because the failure was not yet known, and
/// the count below read 1). Now `wait:failed badge 11 laid out` stands in
/// front of `dump.t1`, and the steps after it keep the gaps they had,
/// rebased on the moment the app knows; no step moved, none added but the
/// wait, no assertion changed.
///
/// Why the badge's mark and not the drop's: `failed badge <id> laid out` is
/// emitted in the refresh that sees `<id>` enter the failed set, whatever
/// the overlay's state (after the drop's mark; the gap is a seat
/// measurement the gate does not depend on — QE 2026-10-03, D3/D5), while
/// `loupe overlay dropped idx 11 (decode failed)` fires only
/// when the overlay was UP and wanted when the failure landed — a failure
/// that lands after a `(hold cap)` drop emits none, and a wait on it would
/// have hung the very run that went red (the idle trace shows `loupe hold
/// idx 11 …` 26 ms after the first End). Its `(satisfied` echo and its
/// ORDER before `QEDUMP t1` are asserted.
///
/// The race, forced: under the senior developer's load recipe (`taskset
/// -c 0,1`, six pinned spinners, rebuild loops) it reproduced 0 of 10 on
/// either script, the failure landing 60–170 ms after the End (the plan's
/// record, 2026-10-03). Deterministically (2026-10-03, brief 010's
/// implementation): with the loupe worker's `Failed` event held 2.5 s
/// before it is sent (a probe, never committed) the script without the
/// wait went red with #101's own message — `left: 1, right: 0`, `dump.t2`
/// at 17151 and the drop at 17543 — and the script with it is green under
/// the same probe. Two residuals the load recipes showed are not this
/// test's race and are left as they are; each ends the run at the
/// `wait:thumb landed idx 11` cap — red, never falsely green — and neither
/// has been seen on CI. (1) Under the senior developer's recipe the wait
/// spent 15–17 s of its 30 s cap and ran past it 2 of 20: the kitchen's
/// thumb cook queued behind idx 0's full-res cook after `home`. (2) With a
/// cold `cargo` build running beside the spinners, the dominant one (QE
/// round 1, 2026-10-03, D4: 16 of 16 red while the build ran, 0 of 6 with
/// the spinners alone once it had finished): the scan settled at
/// 14.3–23.5 s, after the corrupter's 12 s liveness deadline (below) had
/// passed and it had zeroed the copy, so `thumb bytes idx 11` never
/// appeared, idx 11 never had a thumb to arm the masking shape, and the
/// wait could not be satisfied. Both rest on the clock — the first End's
/// fixed 15 s and the corrupter's fixed deadline — not on the app's marks.
#[test]
fn a_decode_failed_cursor_drops_to_fit_instead_of_masking_the_badge() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
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
    // "(decode failed) never appeared". Its other side: on a scan slower
    // than the deadline the copy is zeroed BEFORE the read, idx 11 gets no
    // thumb, and the run ends at the `wait:thumb landed idx 11` cap instead
    // — the doc's second residual (QE round 1, 2026-10-03, D4).
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
                "15000:end;15050:wait:failed badge 11 laid out;15250:dump.t1;16000:home;\
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
    // The known-failed premise was gated on the app's own mark (issue
    // #101): the wait fired, and the first badge mark — the first End's
    // failure — came before the dump everything after it reads. Anchored on
    // the mark's own line (`] failed badge 11 laid out at`): the wait's
    // echo quotes the same words.
    assert!(
        stderr.contains("wait:failed badge 11 laid out (satisfied"),
        "the `wait:failed badge 11 laid out` step never fired — `dump.t1` and \
         the second End were not gated on the app knowing idx 11 failed \
         (issue #101):\n{stderr}"
    );
    let known = stderr.find("] failed badge 11 laid out at ");
    let t1 = stderr.find("] QEDUMP t1 ");
    assert!(
        known.is_some() && t1.is_some() && known < t1,
        "the first `failed badge 11 laid out` mark (at byte {known:?}) does not come \
         before `QEDUMP t1` (at byte {t1:?}) — the dump read a cursor the app did not \
         yet know had failed (issue #101):\n{stderr}"
    );
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
    // would take mouse answers away silently.
    //
    // The row is clicked BY NAME (issue #70). It used to be
    // `click.700,483` — the Keep-both row probed at 1440x900 on
    // 2026-08-30 — and that is exactly the coordinate brief 005 would
    // have broken in silence: adding the New only row on top made that
    // point a DIFFERENT answer, so the round would have copied nothing
    // new while still passing its `copystate` check. A name resolves
    // against the layout this run actually produced, which is also why
    // the round no longer carries the `menu_clicks_are_calibrated()` gate
    // the coordinate needed (Manager ruling on the plan's OQ-1,
    // 2026-09-12): it now runs on Windows too, and a red there would mean
    // a mouse-only user cannot answer this question at all — a defect to
    // diagnose, not a platform to gate off.
    //
    // The dump waits for the copy this click starts (QE 2026-09-02)
    // rather than allowing it a budget: this is the ONE answer given with
    // the pointer, so the click can also miss — and then the wait ends
    // the run after 30 s naming `copy finished run 1`, which says the
    // same thing the `copystate` assertion below would have: the click
    // answered nothing and no copy ever ran.
    let mouse_round = format!(
        "1900:copydest:{dest0};2100:key:ctrl+e;2400:key:return;2700:dump.qclick;\
         2900:click:copy answer B;3000:wait:copy finished run 1;\
         3600:dump.clicked;3900:key:escape;",
        dest0 = dest0.display()
    );
    // Which copy of this PROCESS each answer below starts: the mouse round
    // ran one already, on every platform now. The two dumps that read a
    // finished copy wait for THAT run's report card instead of allowing it
    // 800 ms — a budget the Windows runner does not keep (issue #70:
    // `copystate` read 1, the copy was still going), and one no runner is
    // obliged to keep.
    let (n, n2) = (2, 3);
    let (kept_wait, over_wait) = (
        format!("wait:copy finished run {n} (satisfied"),
        format!("wait:copy finished run {n2} (satisfied"),
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
    // The click resolved against the row the app itself reported, and
    // landed inside it: the guard that replaced the measured coordinate.
    assert_click_resolved(&stderr, "copy answer B");
    assert_eq!(
        dump_field(qedump(&stderr, "clicked"), "copystate"),
        "2",
        "the click on the Keep-both row did not answer the question — \
         a mouse-only user cannot answer it at all:\n{stderr}"
    );
    let names: Vec<&str> = mouse_disk.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        vec!["a.ARW", "a_1.ARW", "a_1.ARW.xmp", "b.ARW", "b.ARW.xmp"],
        "the clicked Keep-both did not land the pick under a fresh name: {mouse_disk:?}"
    );

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

/// The fourth answer, driven through the real dialog (brief 005, issue
/// #86; fileops.md "The clash question" §2 and §6). The fixture is the
/// user's own case in miniature: a destination that already holds one of
/// the picks, with a sidecar beside it that has been DEVELOPED since (the
/// darktable history stack this answer exists to protect — Overwrite
/// byte-replaces it, and until 2026-09-12 no answer added picks to such a
/// folder). Three rounds answer `N` — by key, with nothing new to copy,
/// and by mouse — and a fourth reads the `{seq}` warning off the plan
/// line. AC1 (app level), AC5, AC7 and AC8.
#[test]
fn copy_picks_new_only_copies_the_new_pick_and_leaves_the_rest_alone() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("newonly-src");
    let dest = out_dir().join("newonly-dest");
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    // THREE picks, and the third is what makes the N row's two counts
    // DIFFER (2 new, 1 already here). With two picks both numbers were 1,
    // and a label with `{n}` and `{clashes}` transposed shipped green (QE
    // 2026-09-12, minor 1).
    let (a_bytes, b_bytes, c_bytes) = (vec![0xABu8; 2048], vec![0xCDu8; 2048], vec![0xEFu8; 2048]);
    std::fs::write(src.join("a.ARW"), &a_bytes).unwrap();
    std::fs::write(src.join("b.ARW"), &b_bytes).unwrap();
    std::fs::write(src.join("c.ARW"), &c_bytes).unwrap();
    // The user's own earlier copy of `a`, byte for byte …
    std::fs::write(dest.join("a.ARW"), &a_bytes).unwrap();
    // … and the sidecar beside it, which is NOT ours any more.
    let developed = b"<a darktable history stack, developed at the destination>".to_vec();
    std::fs::write(dest.join("a.ARW.xmp"), &developed).unwrap();
    // The {seq} round's clash: `pick_{seq}.{ext}` over three picks expands
    // to pick_1.ARW, pick_2.ARW and pick_3.ARW (width 1 for a batch of 3),
    // and the first of those is here.
    std::fs::write(dest.join("pick_1.ARW"), b"an earlier run's frame").unwrap();
    let untouched_before = ["a.ARW", "a.ARW.xmp"].map(|n| {
        let path = dest.join(n);
        std::fs::metadata(&path).unwrap().modified().unwrap()
    });

    // Runs are numbered at `start_copy`: 1 the keyboard `N`, 2 the
    // all-left `N`, 3 the clicked one. Every dump that reads a finished
    // copy is gated on its own run's mark, never on a clock — a budget
    // the Windows runner does not keep (issue #70).
    let script = format!(
        "1500:key:y;1650:key:y;1800:key:y;1900:copydest:{dest};2100:key:ctrl+e;2400:dump.preview;\
         2600:key:return;2900:dump.question;3100:key:y;3300:dump.inert_y;\
         3500:key:ctrl+n;3700:dump.accel_n;\
         3900:key:n;4000:wait:copy finished run 1;4600:dump.newonly;4900:key:escape;\
         5100:key:ctrl+e;5400:key:return;5700:dump.allclash;5900:key:n;\
         6000:wait:copy finished run 2;6600:dump.allleft;6900:key:escape;\
         7100:key:ctrl+e;7400:key:return;7700:click:copy answer N;\
         7800:wait:copy finished run 3;8400:dump.clicked;8700:key:escape;\
         8900:key:ctrl+e;9200:copytemplate:pick_{{seq}}.{{ext}};9500:dump.seqnote",
        dest = dest.display()
    );
    let out = out_dir().join("copy-new-only.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let mut on_disk: Vec<String> = std::fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let landed_a = std::fs::read(dest.join("a.ARW")).ok();
    let landed_a_xmp = std::fs::read(dest.join("a.ARW.xmp")).ok();
    let landed_b = std::fs::read(dest.join("b.ARW")).ok();
    let landed_b_xmp = std::fs::read(dest.join("b.ARW.xmp")).ok();
    let src_b_xmp = std::fs::read(src.join("b.ARW.xmp")).ok();
    let landed_c = std::fs::read(dest.join("c.ARW")).ok();
    let landed_c_xmp = std::fs::read(dest.join("c.ARW.xmp")).ok();
    let src_c_xmp = std::fs::read(src.join("c.ARW.xmp")).ok();
    let untouched_after = ["a.ARW", "a.ARW.xmp"].map(|n| {
        let path = dest.join(n);
        std::fs::metadata(&path).unwrap().modified().unwrap()
    });
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
    }

    // --- the plan preview: no {seq} in the template, so no note ---------
    let preview = qedump(&stderr, "preview");
    assert_eq!(dump_field(preview, "copystate"), "0", "{preview}");
    let note = dump_text(preview, "copynote");
    assert!(
        note.contains("2 new · 1 already exist here"),
        "the preview does not pre-announce the split: {note}"
    );
    assert!(
        !note.contains("numbers the whole session"),
        "the {{seq}} note appeared over a template that has no {{seq}} in it: {note}"
    );

    // --- the question, its fourth row, and the order of the four --------
    let question = qedump(&stderr, "question");
    assert_eq!(dump_field(question, "copystate"), "3", "{question}");
    let asked = dump_text(question, "confirm");
    assert!(
        asked.contains("1 of your 3 picks already have files with these names in")
            && asked.contains("The other 2 copy normally"),
        "the question does not state the counts: {asked}"
    );
    assert_eq!(
        dump_text(question, "newonly"),
        "New only — copy the 2, leave the 1 already here untouched",
        "the N row must name both counts IN THE RIGHT ORDER — this is the \
         label the user reads to decide, and with the numbers transposed \
         it promises the opposite of what the answer does — and begin with \
         New so N reads as New and not as No: {question}"
    );
    assert_eq!(
        dump_text(question, "nudge"),
        "Pick one: N, B, O or Esc.",
        "the nudge does not offer the fourth answer: {question}"
    );
    assert_eq!(
        dump_field(question, "nudged"),
        "false",
        "the question is nudging before any key was pressed: {question}"
    );
    assert!(
        dump_text(question, "warning")
            .ends_with("(darktable) are lost. New only leaves them alone."),
        "the amber line warns about the lost edits without naming the \
         answer that avoids them, in the same breath: {question}"
    );
    // The ROW ORDER, from the rows' own layout marks: increasing
    // consequence, New only first, Cancel set apart. RELATIVE y only —
    // a font metric moves a layout by up to 40 px per seat (2026-09-04),
    // so no absolute geometry is pinned here.
    let (_, y_n, _, h_n) = laid_out_rect(&stderr, "copy answer N", question);
    let (_, y_b, _, h_b) = laid_out_rect(&stderr, "copy answer B", question);
    let (_, y_o, _, h_o) = laid_out_rect(&stderr, "copy answer O", question);
    let (_, y_esc, _, h_esc) = laid_out_rect(&stderr, "copy answer Esc", question);
    assert!(
        y_n < y_b && y_b < y_o && y_o < y_esc,
        "the answers are not in order of increasing consequence \
         (N {y_n}, B {y_b}, O {y_o}, Esc {y_esc}): a habitual top-row \
         click must land on the least consequential answer"
    );
    assert!(
        h_n > 0.0 && h_b > 0.0 && h_o > 0.0 && h_esc > 0.0,
        "an answer row has no height: it is drawn but cannot be clicked \
         ({h_n}, {h_b}, {h_o}, {h_esc})"
    );

    // --- Y is inert, and says so; Ctrl+N is not an answer ---------------
    let inert = qedump(&stderr, "inert_y");
    assert_eq!(
        (dump_field(inert, "copystate"), dump_field(inert, "nudged")),
        ("3", "true"),
        "Y answered the question, or died silently — a dead key reads as \
         a frozen dialog: {inert}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "accel_n"), "copystate"),
        "3",
        "Ctrl+N answered the question — the answers are BARE letters only: {stderr}"
    );

    // --- N: the new pick copies, the rest is left -----------------------
    assert!(
        stderr.contains("wait:copy finished run 1 (satisfied"),
        "the `wait:copy finished run 1` step never fired — the dump was \
         timed, not gated:\n{stderr}"
    );
    let done = qedump(&stderr, "newonly");
    assert_eq!(dump_field(done, "copystate"), "2", "{done}");
    let report = dump_text(done, "report");
    assert!(
        report.contains("2 copied, all checksums verified")
            && report.contains(
                "1 already had a file with this name here — left untouched, not re-checked"
            ),
        "the report does not say what was copied and what was left: {report}"
    );
    // The line the user actually watched, end to end (persona G1's
    // MUST-HAVE): the total is the picks this run COPIES, never the
    // whole pick count. Read at the report card, which is safe because
    // nothing resets `copy-progress` when a run ends — this dump is
    // gated on `copy finished run 1`, so the value is the run's last.
    assert_eq!(
        dump_text(done, "copyprogress"),
        "Copying 2 / 2 — c.ARW",
        "the progress line must count the picks this run copies and \
         nothing else — never / 3 (persona G1, fileops.md §6)"
    );
    for never in [
        "identical",
        "replaced",
        "landed under new names",
        "Nothing needed copying",
    ] {
        assert!(
            !report.contains(never),
            "New only reported {never:?} — it neither re-verifies, nor \
             replaces, nor renames, nor copies nothing: {report}"
        );
    }

    // --- nothing new at all: the row says so, and answering it is an
    //     honest no-op with the left line as its whole report ------------
    let all = qedump(&stderr, "allclash");
    assert_eq!(dump_field(all, "copystate"), "3", "{all}");
    let asked = dump_text(all, "confirm");
    assert!(
        asked.contains("3 of your 3 picks") && !asked.contains("The other"),
        "the question claims something still copies normally: {asked}"
    );
    assert_eq!(
        dump_text(all, "newonly"),
        "New only — nothing new to copy, leave the 3 already here untouched",
        "the N row must stay offered and say there is nothing new — a row \
         that appears and disappears moves B and O under the pointer on \
         exactly the destructive question: {all}"
    );
    assert!(
        stderr.contains("wait:copy finished run 2 (satisfied"),
        "the `wait:copy finished run 2` step never fired:\n{stderr}"
    );
    let left = qedump(&stderr, "allleft");
    assert_eq!(dump_field(left, "copystate"), "2", "{left}");
    assert_eq!(
        dump_text(left, "report"),
        "3 already had files with these names here — left untouched, not re-checked",
        "a run that left everything must say so — no green light, and \
         never \"Nothing needed copying\": {left}"
    );

    assert_eq!(
        dump_text(left, "copyprogress"),
        "Starting…",
        "a run that copied nothing must show no Copying and no Skipping \
         line (fileops.md §6)"
    );

    // --- the row answers on click too -----------------------------------
    assert_click_resolved(&stderr, "copy answer N");
    assert!(
        stderr.contains("wait:copy finished run 3 (satisfied"),
        "the `wait:copy finished run 3` step never fired:\n{stderr}"
    );
    let clicked = qedump(&stderr, "clicked");
    assert_eq!(
        dump_field(clicked, "copystate"),
        "2",
        "the click on the New only row did not answer the question — a \
         mouse-only user cannot reach the new answer at all: {clicked}"
    );
    assert_eq!(
        dump_text(clicked, "report"),
        "3 already had files with these names here — left untouched, not re-checked"
    );

    // --- the {seq} note, on the plan line where every answer is ahead ---
    let seqnote = qedump(&stderr, "seqnote");
    assert_eq!(dump_field(seqnote, "copystate"), "0", "{seqnote}");
    let note = dump_text(seqnote, "copynote");
    assert!(
        note.contains(
            "{seq} numbers the whole session — the names already here may now belong to other frames"
        ) && note.contains("2 new · 1 already exist here"),
        "the plan line does not warn that {{seq}} renumbers what is \
         already there: {note}"
    );

    // --- what the disk says ---------------------------------------------
    assert_eq!(
        on_disk,
        vec![
            "a.ARW".to_string(),
            "a.ARW.xmp".to_string(),
            "b.ARW".to_string(),
            "b.ARW.xmp".to_string(),
            "c.ARW".to_string(),
            "c.ARW.xmp".to_string(),
            "pick_1.ARW".to_string()
        ],
        "unexpected destination contents"
    );
    assert_eq!(
        landed_a_xmp.as_deref(),
        Some(developed.as_slice()),
        "the destination sidecar was rewritten — a darktable history \
         stack lives in a file of exactly that name, and New only never \
         opens it"
    );
    assert_eq!(
        untouched_after, untouched_before,
        "New only touched the pair it left (mtime moved)"
    );
    assert_eq!(
        landed_a.as_deref(),
        Some(a_bytes.as_slice()),
        "the RAW that was already there is not the one that was there"
    );
    assert_eq!(
        landed_b.as_deref(),
        Some(b_bytes.as_slice()),
        "the new pick did not land"
    );
    assert!(
        landed_b_xmp.is_some() && landed_b_xmp == src_b_xmp,
        "the new pick's sidecar did not travel with its RAW"
    );
    assert_eq!(
        landed_c.as_deref(),
        Some(c_bytes.as_slice()),
        "the second new pick did not land"
    );
    assert!(
        landed_c_xmp.is_some() && landed_c_xmp == src_c_xmp,
        "the second new pick's sidecar did not travel with its RAW"
    );
}

/// Where the key before `dump.<dump>` put the keyboard: that key's own
/// `drive: key:…` echo — the last one before the dump's echo — and the
/// `focus: … gained` marks between the two, in order (brief 011; the focus
/// marks of test-harness.md). A `lost` is not a landing and is left out,
/// and a repeat of the same `gained` collapses into one: a window that is
/// deactivated and reactivated hands the keyboard back to the control that
/// had it. So a press that lands on a control reads as exactly that
/// control's mark, and a press that moves nothing reads as no mark.
fn landing<'a>(labels: &[&'a str], dump: &str) -> (&'a str, Vec<&'a str>) {
    let echo = format!("drive: dump.{dump}");
    let at = labels
        .iter()
        .position(|l| *l == echo)
        .unwrap_or_else(|| panic!("no `{echo}` in the trace: {labels:?}"));
    let key = labels[..at]
        .iter()
        .rposition(|l| l.starts_with("drive: key:"))
        .unwrap_or_else(|| panic!("no key step before `{echo}`: {labels:?}"));
    let mut gained: Vec<&str> = labels[key + 1..at]
        .iter()
        .copied()
        .filter(|l| l.starts_with("focus: ") && l.ends_with(" gained"))
        .collect();
    gained.dedup();
    (labels[key], gained)
}

/// Where the keyboard was when the step `echo` (a `drive: key:…` line) was
/// dispatched: the last `focus: … gained` mark before that echo (brief 011,
/// the senior developer's review F3). The harness prints a step's echo
/// before it dispatches the step, so nothing the press itself moves is
/// counted. A [`landing`] row reads a press from its key to the next dump;
/// this reads the premise of a press that ACTIVATES a button (Space, Enter)
/// at the press itself, so the gap between the dump before it and the key
/// is covered too. The echo must occur once in the run: a premise is about
/// one press.
fn last_gained_before<'a>(labels: &[&'a str], echo: &str) -> &'a str {
    let at = label_positions(labels, echo);
    assert_eq!(
        at.len(),
        1,
        "`{echo}` is not exactly one step of this run: {labels:?}"
    );
    labels[..at[0]]
        .iter()
        .rev()
        .copied()
        .find(|l| l.starts_with("focus: ") && l.ends_with(" gained"))
        .unwrap_or_else(|| panic!("no `focus: … gained` before `{echo}`: {labels:?}"))
}

/// The first scripted step dispatched after the trace mark at `from`: the
/// index of its `drive:` echo (brief 011, QE's P3). A `wait:`'s own
/// "(satisfied …)" line is narration, not a step, and is passed over: it
/// can be traced in the same timer pass as the mark it waited for, AHEAD of
/// the change handlers that pass set off — Slint fires every due timer and
/// only then runs the change handlers (`update_timers_and_animations`,
/// i-slint-core 1.17.1 `platform.rs`) — so the refocus a run's finish makes
/// may be traced after the wait's line and before the next real step.
fn next_step_after(labels: &[&str], from: usize) -> usize {
    labels[from + 1..]
        .iter()
        .position(|l| l.starts_with("drive: ") && !l.starts_with("drive: wait:"))
        .map(|p| from + 1 + p)
        .unwrap_or_else(|| panic!("no scripted step after `{}`: {labels:?}", labels[from]))
}

/// A run that ends with the keyboard on its running Cancel brings the
/// keyboard home (ui-grid.md, "Modal keyboard containment": every state
/// change of a dialog puts it back on the dialog's scope; brief 011, QE's
/// P3): between `finished` — the run's finished-run mark, emitted once —
/// and the next scripted step, the scope's `focus: <dialog> dialog gained`
/// AND the destroyed Cancel's `focus: <dialog> cancel lost`. The `lost` is
/// traced only because the keyboard leaves Cancel before Cancel is
/// destroyed (test-harness.md, Focus): with the refocus gone there is
/// neither mark, and the next key is delivered to nothing.
fn assert_the_finish_brings_the_keyboard_home(stderr: &str, finished: &str, dialog: &str) {
    let labels = mark_labels(stderr);
    let at = label_positions(&labels, finished);
    assert_eq!(
        at.len(),
        1,
        "`{finished}` is not exactly one mark of this run:\n{stderr}"
    );
    let next = next_step_after(&labels, at[0]);
    for mark in [
        format!("focus: {dialog} dialog gained"),
        format!("focus: {dialog} cancel lost"),
    ] {
        assert!(
            marks_between(&labels, at[0], next, &mark) > 0,
            "after `{finished}` no `{mark}` before the next step: the run's \
             end destroyed the focused Cancel without bringing the keyboard \
             home first, and every key after it goes nowhere (ui-grid.md, \
             \"Modal keyboard containment\"; brief 011, QE's P3). When this \
             fails this way it is that defect; do not quiet it:\n{stderr}"
        );
    }
}

/// AC1 and AC3 of brief 011 (issue #98; ui-grid.md "Modal keyboard
/// containment", fileops.md "The keyboard ring"): in the Copy Picks dialog
/// `Tab` and `Shift+Tab` walk the dialog's own controls in visible order,
/// wrapping, passing over a control the state disables or does not show,
/// and the keyboard never leaves the dialog — `focusowner` reads `-1` after
/// every press, and the control a press lands on is read from that
/// control's own `focus: copy <name> gained` mark (test-harness.md).
///
/// One folder of three 2 KB fakes, `a` and `b` picked and the cursor left
/// on the unpicked `c`, so a key that reached the grid behind the scrim
/// would show in the status line: a `Y` would pick `c`. Seven launches, in
/// this order:
///   1. No destination, so Copy is greyed — the old-red's own shape (brief
///      011 D3). Ctrl+Tab and Ctrl+Shift+Tab first: neither moves the
///      keyboard (ui-grid.md: "`Ctrl+Tab` does nothing unless a dialog's
///      module says otherwise"; QE's P2). About next, over the dialog: a
///      Tab under it moves nothing and the first Esc closes About alone.
///      Then Tab, Tab, Tab land on Choose…, the rename field, and Choose…
///      again, wrapping over the greyed Copy; Shift+Tab goes back over it
///      to the field.
///   2. An empty destination and the template `x.{ext}`, so Copy is live.
///      The first Shift+Tab from the dialog's home lands on Copy, the LAST
///      control — visible order, wrapping, so it is the mirror of the first
///      Tab; then four Tabs — Choose… (the wrap), the field, Copy, Choose…
///      again; a `Y` with the keyboard on Choose… marks nothing; two
///      Shift+Tabs — Copy, the field — and the `z` typed there REPLACES
///      `x.{ext}` (AC3: the field the ring lands on is selected); Esc closes
///      the dialog from the field — the field lets Escape through to the
///      dialog's scope — and the `+` after it zooms the grid. No Enter and
///      no Space: this launch is the first to walk the ring with Copy live.
///
///      2b. Enter in the rename field (QE's P4), in a launch of its own
///      (split out of launch 2 at QE's round 2, D1): launch 2's script, the
///      same string, up to the `z` it types into the field (QE round 3,
///      D1), so every landing before the Enter is one launch 2 asserted.
///      Enter in the field re-plans without copying and brings the keyboard
///      home (`focus: copy dialog gained`), and the next Tab starts the ring
///      over at Choose…, the first control — the home reset on a gain that
///      did not come from the ring (Copy stays live under `z`, so a ring
///      that kept the field's slot would land on Copy). Esc closes the
///      dialog from Choose… (corrected 2026-10-04, QE round 3 D1: 2b
///      reached the field by Tab, Tab from home and called them "the two
///      landings launch 1 asserted" — launch 1 makes them with Copy greyed,
///      and with Copy live a forward Tab's home start moved to the last
///      control left launches 1 and 2 green and put 2b's Enter on
///      Choose…).
///
///      3a. Launch 3, dry (QE round 3, D1; QE's P1): launch 3's script, the
///      same string, with a click on Copy where launch 3 presses Enter on
///      it and Esc where launch 3 presses its closing Space, from the same
///      destination seeded afresh. It runs before launch 3 and asserts every
///      landing launch 3's two presses follow: the mixed path's three Tabs
///      from the home the clash question's Esc leaves, and the report's
///      four from the home the run's finish leaves.
///   3. A destination holding another body's `a.ARW`. First the mixed path
///      (the senior developer's review F1): Tab puts the keyboard on
///      Choose…, a mouse click on Copy — by name, `click:copy copy-close` —
///      asks the clash question, and the keyboard leaves Choose… while
///      Choose… is still enabled (`focus: copy choose lost` and `focus: copy
///      dialog gained` after the click); Esc goes back to the plan, and the
///      next Tab lands on Choose… with its own `gained` — a control the run
///      disabled while it held the keyboard keeps `has-focus` and is landed
///      on silently. Two more Tabs reach Copy; Enter on the focused Copy
///      asks the question and the keyboard is home (`focus: copy dialog
///      gained`); Tab on the question moves nothing and nudges; `B` copies;
///      on the report Tab, Tab, Shift+Tab, Tab walk Open destination,
///      Close, Open destination, Close — past the disabled Choose… and
///      field and the absent Cancel — and Space on the focused Close closes
///      the dialog.
///   4. The running state (QE's P3): an empty destination, and the worker
///      held 3 s before its first file (`FASTCULL_COPY_HOLD_MS`,
///      test-harness.md), so a 2 KB copy is still running while the keys
///      land. Enter from the dialog's home starts it; Tab lands on Cancel,
///      the one control a running copy shows, with the dump reading the
///      running state and its `Starting…` line; a `Y` there marks nothing,
///      and the dump after it still reads the running state. The finish
///      brings the keyboard home: after `copy finished run 1`, before the
///      next step, the scope's `focus: copy dialog gained` and the
///      destroyed Cancel's `focus: copy cancel lost`
///      ([`assert_the_finish_brings_the_keyboard_home`]); the next Tab
///      walks the report from Open destination, and Esc closes.
///   5. The same start, then Space on the running Cancel: the copy ends
///      cancelled before its first file, the report says "cancelled —
///      finished files remain", the keyboard comes home the same way, the
///      next Tab lands on Open destination, Esc closes, and nothing is left
///      at the destination.
///
/// No Enter or Space in this test lands on Choose… or Open destination,
/// whatever a regression does to the ring: they open the native folder
/// picker and the file manager, and the picker holds the app until the
/// watchdog kills it 90 s later, with no assertion reached. What keeps it
/// so: every Enter and every Space follows a walk that an EARLIER launch
/// drove — the same keys, from the same home, in the same dialog state —
/// and asserted landing by landing, so a ring that lands one control off
/// is red at that launch's landing assertion and the launch with the press
/// never runs (`held` and `landings` run between the `run(…)` calls). An
/// earlier launch is the only place a landing can be asserted before a
/// press: a launch's trace is read after its app exits, and no `wait:` can
/// hold a press until the keyboard is where the script put it — a wait is
/// satisfied by a mark emitted at any time in the run (test-harness.md),
/// and a control's `gained` mark recurs. The presses, and the walks they
/// follow:
///   - launch 2b's Enter in the rename field: launch 2's script, up to it;
///   - launch 3's Enter on Copy: the mixed path, then three Tabs from the
///     home the clash question's Esc leaves — launch 3a's script, up to it;
///   - launch 3's closing Space on Close: after the question, the `B` and
///     the run, the report's Tab, Tab, Shift+Tab, Tab from the home the
///     run's finish leaves — launch 3a's script too, which asks its
///     question with a click on the focused Copy where launch 3 presses
///     Enter on it (both reach Copy's `clicked`, which puts the keyboard
///     home before the run starts, and both landings are asserted);
///   - launch 4's Enter: from the dialog's home as it opens, after no
///     landing at all;
///   - launch 5's Space on the running Cancel: launch 4's script, up to it
///     — Enter from home, one Tab (and were launch 5 driven alone under a
///     ring that loses the running Cancel, the M14 mutant below, that Tab
///     would go nowhere and the Space land on the dialog's home, which
///     ignores it).
///
/// (Corrected 2026-10-04, QE round 3 D1: this said the same while launch
/// 3's Enter followed three Tabs from the home the clash question's Esc
/// leaves, which no earlier launch made — under a one-token regression of
/// that home, `self.slot = 0` for `-1` in the scope's `changed state`
/// (QE's A4), launches 1, 2 and 2b stayed green and the Enter landed on
/// Choose… — while it called launch 3's closing Space the one press no
/// earlier launch covered, and while 2b's Tabs were launch 1's landings
/// made with Copy greyed, see 2b. QE round 2 D1 had corrected it once: it
/// said the same while launch 2 still pressed Enter in the field after six
/// landings no earlier launch had asserted, and under the home-start
/// mutant that Return opened the native picker.)
///
/// Each press that activates a button also has its premise read at the
/// press itself (the senior developer's review F3, [`last_gained_before`]):
/// the keyboard on Copy when launch 3's Return goes, on Close when its
/// Space goes (shown red by a script mutant: an Esc slipped in before the
/// Space closes the dialog under it, and the premise reads `focus: keys
/// gained`), and on the running Cancel when launch 5's Space goes. These
/// read the trace after the run, so they name a wrong press after it
/// happened; the earlier launches are what stop one from happening
/// (corrected 2026-10-04, QE round 3 D1: this recorded launch 3's closing
/// Space as a run-time exposure no assertion could close — Close dropped
/// from `slot-ok` in the report state alone would put it on Open
/// destination, the file manager — and launch 3a now walks the report
/// first, red at its dump.r2 under exactly that regression, QE's A6).
///
/// RED on b6c238f, the head before the fix (brief 011 D3 measured the same
/// on f1520b9): launch 1's third Tab puts the keyboard on the grid's scope
/// behind the scrim — `focus: keys gained`, dump.g3 `focusowner=0`. The
/// same build driven through the other two scripts as they stood before
/// review F1: launch 2's first Shift+Tab lands on `keys` at once
/// (`focusowner=0`); in launch 3 the window's own Tab walk reaches the live
/// Copy, Enter there asks the question and leaves the keyboard on the
/// destroyed button, the Tab after it lands on `keys` (`focusowner=0`, no
/// nudge) and the `B` never answers. When this fails that way it is that
/// defect; do not quiet it.
///
/// RED on 6eed28b, the build with the Copy button's layout mark and
/// without review F1's fix: after the click on Copy no `copy choose lost`
/// — the state change brought the keyboard home with Choose… already
/// disabled — and after Esc the Tab onto Choose… is silent (`focus: copy
/// dialog lost` alone, so dump.c2's landing is empty). When this fails that
/// way it is that defect; do not quiet it.
///
/// Mutants (2026-10-04), each alone: the scope's Tab arm removed → red at
/// dump.g3, `focusowner=0`; the rename field left out of `slot-ok` → red at
/// dump.g2, no landing; the wrap removed (`clamp` for `Math.mod` in `walk`)
/// → red at dump.g3, no landing; `select-all()` removed → dump.z reads
/// `zx.{ext}`, the letter typed in at the caret; `copy-keys.focus()` removed
/// from Copy's `clicked` (review F1) → red after launch 3a's first click, no
/// `copy choose lost`; the `changed state` refocus removed (QE's mutant M5)
/// → red on 841bb1d through Enter-on-Copy (no focus at all after the
/// Return, the Tab on the question on `keys`, the run dead at `wait:copy
/// finished run 1`); since F1, which sends the keyboard home from Copy's
/// own `clicked`, it guards the worker-finish path — a run ending under a
/// focused Cancel — which launch 4 drives with the held worker: red at
/// launch 4's dump.f1, `focusowner=0`, the Tab after the finish on `keys`
/// behind the scrim, and neither `copy dialog gained` nor `copy cancel
/// lost` after `copy finished run 1` (corrected 2026-10-04, QE D5: this
/// said no fixture holds a copy long enough to Tab onto Cancel and called
/// that path review-verified — a held worker does); the running Cancel
/// left out of `slot-ok` (`if (s == 3) { return false; }`, QE's mutant
/// M14) → red at launch 4's dump.run, the Tab landing nowhere, and launch
/// 5's script driven alone against that build has its Space land on the
/// dialog's home, where it is ignored, and the copy runs to "2 copied";
/// the home start removed (`slot + dir` from -1, `let start = self.slot;`
/// in `walk`; QE's mutant M6) → the first Shift+Tab lands on the field,
/// red at launch 2's dump.home, `left: ["focus: copy template gained"]
/// right: ["focus: copy copy-close gained"]` (corrected 2026-10-04, QE
/// round 2 D1: this said red at dump.home while launch 2 still pressed
/// Return in the field after that landing — under this mutant the Return
/// landed on Choose…, the native picker opened and the run hung until the
/// watchdog, so the assertion was never reached); `slot-ok` approving the
/// greyed Copy → the ring's `focus()` on it walks on to `keys` behind the
/// scrim, red at dump.g3, `focusowner=0`;
/// the ring's arm moved ahead of the dialog's About containment → the Tab
/// under About lands on Choose…, red at dump.abtab; the arm moved ahead of
/// the clash question's branch → the Tab is eaten without the nudge, red
/// at 3a's dump.qtab; the ring's arm without its `!event.modifiers.control`
/// (QE's mutant X5) → Ctrl+Tab walks the ring, red at dump.ct, its landing
/// `focus: copy choose gained` (the Ctrl+Shift+Tab after it then lands on
/// the field); the scope's own gain no longer resetting the ring to home
/// (`self.slot = -1` removed from `copy-keys`' `changed has-focus`, QE's
/// mutant M12) → after Enter in the rename field the next Tab goes on from
/// the field's slot to Copy, red at launch 2b's dump.hometab, `focus: copy
/// copy-close gained`; every state change leaving the ring at Choose…
/// instead of home (`self.slot = 0` for `-1` in the scope's `changed
/// state`, QE's mutant A4) → after the clash question's Esc the first Tab
/// lands on the field, red at launch 3a's dump.c2, `left: ["focus: copy
/// template gained"] right: ["focus: copy choose gained"]`, before any Enter
/// or Space reaches that walk (QE round 3 D1: before launch 3a, launches 1,
/// 2 and 2b stayed green and launch 3's Enter landed on Choose…); Close left
/// out of `slot-ok` on the report (`if (s == 4) { return root.copy-state ==
/// 0 && root.copy-ready; }`, QE's mutant A6) → the report's second Tab
/// stays on Open destination, red at launch 3a's dump.r2, `left: []`, no
/// Space pressed; a forward Tab's home start moved from before the first
/// control to before the last (`(dir > 0 ? 3 : 5)` for `(dir > 0 ? -1 : 5)`
/// in `walk`) → launches 1 and 2 stay green — launch 1's first Tab passes
/// the greyed Copy on to Choose…, and launch 2 starts with a Shift+Tab —
/// and the Tab after Enter in the field lands on Copy, red at launch 2b's
/// dump.hometab, `left: ["focus: copy copy-close gained"]` (QE round 3 D1:
/// when 2b reached the field by Tab, Tab from home, this put its Enter on
/// Choose…). The controls' own token writes have no red mutant —
/// `focus-slot` writes the token too, and nothing else writes it while the
/// dialog is up — and stay as the owner token's claim-site rule.
#[test]
fn copy_picks_tab_walks_its_own_controls_and_never_leaves_the_dialog() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("tabring-copy-src");
    let dest = out_dir().join("tabring-copy-dest");
    let dest2 = out_dir().join("tabring-copy-dest2");
    // Launches 4 and 5 copy for real, each into its own empty folder.
    let dest3 = out_dir().join("tabring-copy-dest3");
    let dest4 = out_dir().join("tabring-copy-dest4");
    for d in [&src, &dest, &dest2, &dest3, &dest4] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    for (name, byte) in [("a", 0xABu8), ("b", 0xCD), ("c", 0xEF)] {
        std::fs::write(src.join(format!("{name}.ARW")), vec![byte; 2048]).unwrap();
    }
    // Launches 3a and 3 each start from this destination as seeded here —
    // another body's `a.ARW` and nothing else. Launch 3a copies into it, so
    // it is seeded afresh before each: the two launches play the same
    // script, destination path included, from the same folder.
    let seed_clash_dest = || {
        std::fs::remove_dir_all(&dest2).ok();
        std::fs::create_dir_all(&dest2).unwrap();
        std::fs::write(dest2.join("a.ARW"), b"another body's frame").unwrap();
    };
    let run = |shot: &str, script: &str| -> String {
        shoot_env_stderr(
            &[src.to_str().unwrap()],
            &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
            &out_dir().join(shot),
        )
    };
    // Launches 4 and 5: the worker held 3 s before its first file
    // (test-harness.md, `FASTCULL_COPY_HOLD_MS`), so the run is still
    // running when their keys land. Their in-run strand ends 1 s after the
    // Return, which leaves 2 s before the finish; a premise that reads red
    // on a slow runner is answered with a longer hold, never a later key
    // and never a dropped premise.
    let run_held = |shot: &str, script: &str| -> String {
        shoot_env_stderr(
            &[src.to_str().unwrap()],
            &[
                ("FASTCULL_TRACE", "1"),
                ("FASTCULL_COPY_HOLD_MS", "3000"),
                ("FASTCULL_DRIVE", script),
            ],
            &out_dir().join(shot),
        )
    };
    // Every press's landing: its key, and the one control it reached.
    let landings = |stderr: &str, rows: &[(&str, &str, &str)]| {
        let labels = mark_labels(stderr);
        for (dump, key, mark) in rows {
            let (step, gained) = landing(&labels, dump);
            assert_eq!(
                step,
                format!("drive: key:{key}"),
                "the script's step before dump.{dump} is not the key this row \
                 is about:\n{stderr}"
            );
            assert_eq!(
                gained,
                vec![format!("focus: {mark} gained")],
                "the {key} before dump.{dump} did not land on {mark} alone \
                 (fileops.md, \"The keyboard ring\"):\n{stderr}"
            );
        }
    };
    // The token after every press — the old-red, asserted first.
    let held = |stderr: &str, dumps: &[&str]| {
        for dump in dumps {
            let line = qedump(stderr, dump);
            assert_eq!(
                dump_field(line, "focusowner"),
                "-1",
                "at dump.{dump} the keyboard has left the Copy Picks dialog \
                 (focusowner is not the dialog's -1): a Tab or Shift+Tab \
                 carried it behind the scrim — issue #98. When this fails \
                 this way it is that defect; do not quiet it:\n{stderr}"
            );
            assert_eq!(dump_field(line, "copy"), "true", "{line}");
        }
    };

    // --- 1. no destination: the greyed Copy is passed over both ways ------
    let grey = run(
        "tabring-copy-grey.jpg",
        "1500:wait:load settled gen 0;1700:key:y;1900:key:y;2300:key:ctrl+e;\
         2700:dump.grey;2800:key:ctrl+tab;3100:dump.ct;3300:key:ctrl+shift+tab;3600:dump.cst;\
         3700:about;4000:dump.ab;4200:key:tab;4500:dump.abtab;\
         4700:key:escape;5000:dump.abclosed;\
         5200:key:tab;5500:dump.g1;5700:key:tab;6000:dump.g2;\
         6200:key:tab;6500:dump.g3;6700:key:shift+tab;7000:dump.g4",
    );
    held(
        &grey,
        &[
            "grey", "ct", "cst", "ab", "abtab", "abclosed", "g1", "g2", "g3", "g4",
        ],
    );
    assert!(
        grey.contains("wait:load settled gen 0 (satisfied"),
        "the picks were pressed before the folder settled:\n{grey}"
    );
    let premise = qedump(&grey, "grey");
    assert_eq!(dump_field(premise, "copystate"), "0", "{premise}");
    assert_eq!(
        dump_text(premise, "summary"),
        "2 picked images. Choose a destination.",
        "the premise is a plan with no destination, where Copy is greyed: {premise}"
    );
    let status = dump_text(premise, "status");
    assert!(
        status.contains("c.ARW (3/3) · unmarked") && status.contains("★2 ✕0"),
        "the premise is a and b picked and the cursor on the unmarked c: {premise}"
    );
    // Ctrl+Tab and Ctrl+Shift+Tab move nothing (ui-grid.md, "Modal keyboard
    // containment"; QE's P2). The ring's arm declines a Tab with Ctrl held,
    // and Slint's window refuses its own Tab walk for one with Control,
    // Meta or Alt (`window.rs` `process_key_input`, its `extra_mod`), so the
    // arm's `!event.modifiers.control` is the one thing between the chord
    // and the ring — which is the promise this pins.
    let labels = mark_labels(&grey);
    for (dump, chord) in [("ct", "ctrl+tab"), ("cst", "ctrl+shift+tab")] {
        let echo = format!("drive: key:{chord}");
        assert_eq!(
            landing(&labels, dump),
            (echo.as_str(), Vec::new()),
            "{chord} moved the keyboard in the Copy Picks dialog — ui-grid.md: \
             \"`Ctrl+Tab` does nothing unless a dialog's module says \
             otherwise\":\n{grey}"
        );
    }
    // About over the dialog takes Tab like every other key: the ring's arm
    // comes after the containment arm, so nothing moves behind the popup,
    // and the first Esc closes About alone (ui-grid.md, "Modal keyboard
    // containment").
    assert_eq!(dump_field(qedump(&grey, "ab"), "about"), "true", "{grey}");
    assert_eq!(
        landing(&mark_labels(&grey), "abtab").1,
        Vec::<&str>::new(),
        "Tab under About moved the keyboard behind the popup — the ring ran \
         before the dialog's containment arm:\n{grey}"
    );
    let abclosed = qedump(&grey, "abclosed");
    assert_eq!(
        (
            dump_field(abclosed, "about"),
            dump_field(abclosed, "copystate")
        ),
        ("false", "0"),
        "the first Esc did not close About alone: {abclosed}"
    );
    landings(
        &grey,
        &[
            ("g1", "tab", "copy choose"),
            ("g2", "tab", "copy template"),
            ("g3", "tab", "copy choose"),
            ("g4", "shift+tab", "copy template"),
        ],
    );
    assert_eq!(
        dump_text(qedump(&grey, "g4"), "status"),
        status,
        "a key reached the grid behind the dialog:\n{grey}"
    );

    // --- 2. Copy live: the whole ring, a Y, the field selected ------------
    // Launch 2's walk up to the `z` it types into the field. Launch 2b plays
    // this same string before its Enter in the field (QE round 3, D1), so
    // every landing that Enter follows is one launch 2 has asserted.
    let ring_walk = format!(
        "1500:wait:load settled gen 0;1700:key:y;1900:key:y;2100:copydest:{dest};\
         2300:key:ctrl+e;2600:copytemplate:x.{{ext}};2900:dump.plan;\
         3100:key:shift+tab;3400:dump.home;\
         3600:key:tab;3900:dump.t1;4100:key:y;4400:dump.y1;4600:key:tab;4900:dump.t2;\
         5100:key:tab;5400:dump.t3;5600:key:tab;5900:dump.t4;\
         6100:key:shift+tab;6400:dump.s1;6600:key:shift+tab;6900:dump.s2;\
         7100:key:z;7400:dump.z;",
        dest = dest.display()
    );
    let ring = run(
        "tabring-copy-ring.jpg",
        &format!("{ring_walk}7600:key:escape;7900:dump.closed;8100:key:+;8400:dump.zoom"),
    );
    held(
        &ring,
        &[
            "plan", "home", "t1", "y1", "t2", "t3", "t4", "s1", "s2", "z",
        ],
    );
    let plan = qedump(&ring, "plan");
    assert_eq!(dump_field(plan, "copystate"), "0", "{plan}");
    assert_eq!(dump_text(plan, "template"), "x.{ext}", "{plan}");
    assert!(
        dump_text(plan, "summary").contains(" to copy · ")
            && dump_text(plan, "copyerror").is_empty(),
        "the premise is a clean plan, where Copy is live: {plan}"
    );
    landings(
        &ring,
        &[
            ("home", "shift+tab", "copy copy-close"),
            ("t1", "tab", "copy choose"),
            ("t2", "tab", "copy template"),
            ("t3", "tab", "copy copy-close"),
            ("t4", "tab", "copy choose"),
            ("s1", "shift+tab", "copy copy-close"),
            ("s2", "shift+tab", "copy template"),
        ],
    );
    // A Y with the keyboard on Choose… marks nothing: the button takes only
    // Space and Enter, and the dialog's scope swallows the rest.
    let y1 = qedump(&ring, "y1");
    assert_eq!(
        (dump_text(y1, "status"), dump_field(y1, "cursor")),
        (dump_text(plan, "status"), dump_field(plan, "cursor")),
        "the Y on the focused Choose… reached the grid behind the dialog:\n{ring}"
    );
    assert_eq!(
        landing(&mark_labels(&ring), "y1").1,
        Vec::<&str>::new(),
        "the Y moved the keyboard:\n{ring}"
    );
    // AC3: the field the ring lands on is selected, so the letter replaces
    // the template instead of joining it. Tab and Shift+Tab arrive through
    // the same `focus-slot`, so the selection is the arrival's, either way.
    assert_eq!(
        dump_text(qedump(&ring, "z"), "template"),
        "z",
        "the `z` typed after Shift+Tab into the rename field did not replace \
         its text — the ring's arrival did not select it (fileops.md, \"The \
         keyboard ring\"; AC3):\n{ring}"
    );
    let closed = qedump(&ring, "closed");
    assert_eq!(
        (dump_field(closed, "copy"), dump_field(closed, "focusowner")),
        ("false", "0"),
        "Esc in the rename field did not close the dialog and hand the keyboard \
         back to the grid: {closed}"
    );
    assert_eq!(
        dump_field(qedump(&ring, "zoom"), "zoom"),
        "2",
        "the `+` after the dialog closed was dead:\n{ring}"
    );

    // --- 2b. Enter in the rename field (QE's P4) --------------------------
    // A launch of its own (QE's round 2, D1) that plays launch 2's walk, the
    // same string, up to the `z` in the field (QE round 3, D1): its Enter
    // follows only landings launch 2 has asserted, never one this launch is
    // the first to make.
    let enter = run(
        "tabring-copy-enter.jpg",
        &format!(
            "{ring_walk}7600:key:return;7900:dump.acc;\
             8100:key:tab;8400:dump.hometab;8600:key:escape;8900:dump.closed"
        ),
    );
    held(
        &enter,
        &[
            "plan", "home", "t1", "y1", "t2", "t3", "t4", "s1", "s2", "z", "acc", "hometab",
        ],
    );
    let eplan = qedump(&enter, "plan");
    assert_eq!(dump_field(eplan, "copystate"), "0", "{eplan}");
    assert_eq!(dump_text(eplan, "template"), "x.{ext}", "{eplan}");
    assert!(
        dump_text(eplan, "summary").contains(" to copy · ")
            && dump_text(eplan, "copyerror").is_empty(),
        "the premise is a clean plan, where Copy is live: {eplan}"
    );
    // Launch 2's landings, read again here: the walk is the same string.
    landings(
        &enter,
        &[
            ("home", "shift+tab", "copy copy-close"),
            ("t1", "tab", "copy choose"),
            ("t2", "tab", "copy template"),
            ("t3", "tab", "copy copy-close"),
            ("t4", "tab", "copy choose"),
            ("s1", "shift+tab", "copy copy-close"),
            ("s2", "shift+tab", "copy template"),
        ],
    );
    // The `z` replaced the selected template and Copy stays live under it —
    // the premise that lets the Tab after Enter tell the scope's home reset
    // from a ring that kept the field's slot, which would land on Copy.
    let pz = qedump(&enter, "z");
    assert_eq!(
        (
            dump_field(pz, "copystate"),
            dump_text(pz, "template"),
            dump_text(pz, "copyerror")
        ),
        ("0", "z", ""),
        "the `z` typed after Shift+Tab into the rename field did not replace \
         its text, or left Copy greyed: {pz}"
    );
    // Enter in the rename field re-plans and does not copy (the field's
    // `accepted`), with Copy still live.
    let acc = qedump(&enter, "acc");
    assert_eq!(
        (
            dump_field(acc, "copystate"),
            dump_text(acc, "template"),
            dump_text(acc, "copyerror")
        ),
        ("0", "z", ""),
        "Enter in the rename field must re-plan without copying, with Copy \
         still live: {acc}"
    );
    landings(
        &enter,
        &[
            // The keyboard home…
            ("acc", "return", "copy dialog"),
            // …and the ring from its start, not from the field.
            ("hometab", "tab", "copy choose"),
        ],
    );
    let eclosed = qedump(&enter, "closed");
    assert_eq!(
        (
            dump_field(eclosed, "copy"),
            dump_field(eclosed, "focusowner")
        ),
        ("false", "0"),
        "Esc with the keyboard on Choose… did not close the dialog and hand \
         the keyboard back to the grid: {eclosed}"
    );

    // Launch 3's script in pieces, so that launch 3a plays the same string
    // with its two presses swapped out (QE round 3, D1): the walk up to the
    // Enter on Copy — the mixed path, then three Tabs from the home the
    // clash question's Esc leaves — and, after the Enter, the question, the
    // `B`, the run and the report's walk up to the closing Space, its Tabs
    // starting from the home the run's finish leaves.
    let clash_walk = format!(
        "1500:wait:load settled gen 0;1700:key:y;1900:key:y;2100:copydest:{dest2};\
         2300:key:ctrl+e;2700:dump.plan2;\
         2900:key:tab;3200:dump.c1;3400:click:copy copy-close;3700:dump.cq;\
         3900:key:escape;4200:dump.cback;4400:key:tab;4700:dump.c2;\
         4900:key:tab;5200:dump.c3;5400:key:tab;5700:dump.oncopy;",
        dest2 = dest2.display()
    );
    let clash_report = "6200:dump.q;6400:key:tab;6700:dump.qtab;\
         6900:key:b;7000:wait:copy finished run 1;7400:dump.report;\
         7600:key:tab;7900:dump.r1;8100:key:tab;8400:dump.r2;\
         8600:key:shift+tab;8900:dump.r3;9100:key:tab;9400:dump.r4;";

    // --- 3a. launch 3, dry (QE round 3, D1) -------------------------------
    // A click on Copy where launch 3 presses Enter on it, and Esc where it
    // presses its closing Space: every landing launch 3's two presses follow
    // is asserted here first, and launch 3 runs only if this one passed.
    seed_clash_dest();
    let dry = run(
        "tabring-copy-mixed.jpg",
        &format!(
            "{clash_walk}5900:click:copy copy-close;{clash_report}9600:key:escape;9900:dump.end"
        ),
    );
    held(
        &dry,
        &[
            "plan2", "c1", "cq", "cback", "c2", "c3", "oncopy", "q", "qtab", "report", "r1", "r2",
            "r3", "r4",
        ],
    );
    let dplan = qedump(&dry, "plan2");
    assert_eq!(dump_field(dplan, "copystate"), "0", "{dplan}");
    assert!(
        dump_text(dplan, "copynote").contains("already exist here — Copy will ask"),
        "the premise is a live Copy that will ask: {dplan}"
    );
    assert_eq!(
        (
            dump_field(qedump(&dry, "cq"), "copystate"),
            dump_field(qedump(&dry, "cback"), "copystate")
        ),
        ("3", "0"),
        "the click on Copy did not ask the clash question, or Esc did not \
         take it back to the plan:\n{dry}"
    );
    // The first click let go of Choose… while Choose… was still enabled
    // (launch 3 below says why that matters).
    let dlabels = mark_labels(&dry);
    let dclicks = label_positions(&dlabels, "drive: click:copy copy-close");
    let dcq = label_positions(&dlabels, "drive: dump.cq");
    let dq = label_positions(&dlabels, "drive: dump.q");
    assert!(
        dclicks.len() == 2
            && dcq.len() == 1
            && dq.len() == 1
            && dclicks[0] < dcq[0]
            && dcq[0] < dclicks[1]
            && dclicks[1] < dq[0],
        "the two click strands are not in the trace as written:\n{dry}"
    );
    for mark in ["focus: copy choose lost", "focus: copy dialog gained"] {
        assert!(
            marks_between(&dlabels, dclicks[0], dcq[0], mark) > 0,
            "after the click on Copy no `{mark}`: the keyboard did not leave \
             Choose… before the run disabled it (ui-grid.md, \"Modal keyboard \
             containment\"; review F1). When this fails this way it is that \
             defect; do not quiet it:\n{dry}"
        );
    }
    landings(
        &dry,
        &[
            ("c1", "tab", "copy choose"),
            // From the home the clash question's Esc leaves: the walk
            // launch 3's Enter follows.
            ("c2", "tab", "copy choose"),
            ("c3", "tab", "copy template"),
            ("oncopy", "tab", "copy copy-close"),
            // From the home the run's finish leaves: the walk launch 3's
            // closing Space follows.
            ("r1", "tab", "copy open-dest"),
            ("r2", "tab", "copy copy-close"),
            ("r3", "shift+tab", "copy open-dest"),
            ("r4", "tab", "copy copy-close"),
        ],
    );
    // The second click, on the Copy the keyboard is on, asks the question
    // with the keyboard home — what launch 3's Enter on it does: both reach
    // Copy's `clicked`, which sends the keyboard home first (review F1).
    let mut dgained: Vec<&str> = dlabels[dclicks[1] + 1..dq[0]]
        .iter()
        .copied()
        .filter(|l| l.starts_with("focus: ") && l.ends_with(" gained"))
        .collect();
    dgained.dedup();
    assert_eq!(
        (dump_field(qedump(&dry, "q"), "copystate"), dgained),
        ("3", vec!["focus: copy dialog gained"]),
        "the click on the focused Copy did not ask the clash question with \
         the keyboard home:\n{dry}"
    );
    let dqtab = qedump(&dry, "qtab");
    assert_eq!(
        (dump_field(dqtab, "copystate"), dump_field(dqtab, "nudged")),
        ("3", "true"),
        "Tab on the clash question must be swallowed with the nudge, like \
         every key that is not an answer (fileops.md): {dqtab}"
    );
    assert_eq!(
        landing(&dlabels, "qtab").1,
        Vec::<&str>::new(),
        "Tab on the clash question moved the keyboard:\n{dry}"
    );
    assert!(
        dry.contains("wait:copy finished run 1 (satisfied"),
        "the `B` answered nothing — the report dump was timed, not gated:\n{dry}"
    );
    let dreport = qedump(&dry, "report");
    assert!(
        dump_field(dreport, "copystate") == "2"
            && dump_text(dreport, "report").contains("1 landed under new names (a_1.ARW"),
        "the `B` after the click on Copy did not keep both: {dreport}"
    );
    let dend = qedump(&dry, "end");
    assert_eq!(
        (dump_field(dend, "copy"), dump_field(dend, "focusowner")),
        ("false", "0"),
        "Esc with the keyboard on Close did not close the report and hand \
         the keyboard back: {dend}"
    );

    // --- 3. the mixed path, the clash question and the report -------------
    seed_clash_dest();
    let clash = run(
        "tabring-copy-clash.jpg",
        &format!("{clash_walk}5900:key:return;{clash_report}9600:key:space;9900:dump.end"),
    );
    held(
        &clash,
        &[
            "plan2", "c1", "cq", "cback", "c2", "c3", "oncopy", "q", "qtab", "report", "r1", "r2",
            "r3", "r4",
        ],
    );
    let plan2 = qedump(&clash, "plan2");
    assert_eq!(dump_field(plan2, "copystate"), "0", "{plan2}");
    assert!(
        dump_text(plan2, "copynote").contains("already exist here — Copy will ask"),
        "the premise is a live Copy that will ask: {plan2}"
    );
    // The mixed path (review F1): the keyboard on Choose… by Tab, then a
    // mouse click on Copy. The click asked the question and Esc went back
    // to the plan — the premise of the two assertions after it.
    assert_eq!(
        (
            dump_field(qedump(&clash, "cq"), "copystate"),
            dump_field(qedump(&clash, "cback"), "copystate")
        ),
        ("3", "0"),
        "the click on Copy did not ask the clash question, or Esc did not \
         take it back to the plan:\n{clash}"
    );
    // The click let go of Choose… while Choose… was still enabled. The run
    // it starts disables Choose…, and a disabled control ignores the
    // FocusOut (Cargo.toml, the fourth canary's fact 11), so a keyboard
    // moved home only by the state change leaves Choose… holding a stale
    // `has-focus` — its focus border on, the keyboard elsewhere.
    let labels = mark_labels(&clash);
    let click = label_positions(&labels, "drive: click:copy copy-close");
    let cq = label_positions(&labels, "drive: dump.cq");
    assert!(
        click.len() == 1 && cq.len() == 1 && click[0] < cq[0],
        "the click strand is not in the trace as written:\n{clash}"
    );
    for mark in ["focus: copy choose lost", "focus: copy dialog gained"] {
        assert!(
            marks_between(&labels, click[0], cq[0], mark) > 0,
            "after the click on Copy no `{mark}`: the keyboard did not leave \
             Choose… before the run disabled it (ui-grid.md, \"Modal keyboard \
             containment\"; review F1). When this fails this way it is that \
             defect; do not quiet it:\n{clash}"
        );
    }
    // The premise of each press that activates a button, read at the press
    // itself (review F3): the Return was dispatched with the keyboard on
    // Copy, and the closing Space with it on Close. A wrong landing between
    // the dump before a press and the press is caught here, after the run
    // — not before it: see the doc above on what no assertion can do.
    for (echo, on) in [
        ("drive: key:return", "focus: copy copy-close gained"),
        ("drive: key:space", "focus: copy copy-close gained"),
    ] {
        assert_eq!(
            last_gained_before(&labels, echo),
            on,
            "`{echo}` was not dispatched with the keyboard where the script \
             put it — the outcome read after it is not that press's:\n{clash}"
        );
    }
    landings(
        &clash,
        &[
            ("c1", "tab", "copy choose"),
            // After Esc, Choose… is focused again WITH its own `gained`: a
            // stale `has-focus` would make this Tab silent.
            ("c2", "tab", "copy choose"),
            ("c3", "tab", "copy template"),
            ("oncopy", "tab", "copy copy-close"),
            ("q", "return", "copy dialog"),
            ("r1", "tab", "copy open-dest"),
            ("r2", "tab", "copy copy-close"),
            ("r3", "shift+tab", "copy open-dest"),
            ("r4", "tab", "copy copy-close"),
        ],
    );
    assert_eq!(
        dump_field(qedump(&clash, "q"), "copystate"),
        "3",
        "Enter on the focused Copy did not ask the clash question:\n{clash}"
    );
    let qtab = qedump(&clash, "qtab");
    assert_eq!(
        (dump_field(qtab, "copystate"), dump_field(qtab, "nudged")),
        ("3", "true"),
        "Tab on the clash question must be swallowed with the nudge, like \
         every key that is not an answer (fileops.md): {qtab}"
    );
    assert_eq!(
        landing(&mark_labels(&clash), "qtab").1,
        Vec::<&str>::new(),
        "Tab on the clash question moved the keyboard:\n{clash}"
    );
    assert!(
        clash.contains("wait:copy finished run 1 (satisfied"),
        "the `B` answered nothing — the report dump was timed, not gated:\n{clash}"
    );
    let report = qedump(&clash, "report");
    assert_eq!(dump_field(report, "copystate"), "2", "{report}");
    assert!(
        dump_text(report, "report").contains("1 landed under new names (a_1.ARW"),
        "the `B` after Enter on the focused Copy did not keep both: {report}"
    );
    let end = qedump(&clash, "end");
    assert_eq!(
        (dump_field(end, "copy"), dump_field(end, "focusowner")),
        ("false", "0"),
        "Space on the focused Close did not close the dialog and hand the \
         keyboard back: {end}"
    );

    // --- 4. the running ring and the finish (QE's P3) ---------------------
    let running = run_held(
        "tabring-copy-run.jpg",
        &format!(
            "1500:wait:load settled gen 0;1700:key:y;1900:key:y;2100:copydest:{dest3};\
             2300:key:ctrl+e;2700:dump.plan4;\
             2900:key:return;3100:key:tab;3400:dump.run;3600:key:y;3900:dump.inrun;\
             4100:wait:copy finished run 1;4400:dump.fin;\
             4600:key:tab;4900:dump.f1;5100:key:escape;5400:dump.closed4",
            dest3 = dest3.display()
        ),
    );
    held(&running, &["plan4", "run", "inrun", "fin", "f1"]);
    assert!(
        running.contains("fastcull: FASTCULL_COPY_HOLD_MS=3000 — every copy is held"),
        "the copy worker's hold was not read, so this launch's run is not the \
         held one it is about:\n{running}"
    );
    let plan4 = qedump(&running, "plan4");
    assert!(
        dump_field(plan4, "copystate") == "0"
            && dump_text(plan4, "summary").contains(" to copy · ")
            && dump_text(plan4, "copyerror").is_empty(),
        "the premise is a clean plan, where Return from home copies: {plan4}"
    );
    // The launch's premise (QE's P3): the copy is still running — held
    // before its first file — when the Tab lands, and the Tab lands on its
    // Cancel. A run that outran the Tab fails HERE, loudly.
    let run4 = qedump(&running, "run");
    assert_eq!(
        (
            dump_field(run4, "copystate"),
            dump_text(run4, "copyprogress")
        ),
        ("1", "Starting…"),
        "the copy was not running, held before its first file, when the Tab \
         landed: {run4}"
    );
    landings(&running, &[("run", "tab", "copy cancel")]);
    // A Y with the keyboard on the running Cancel marks nothing, and the
    // dump after it still reads the running state: a Y that arrived after
    // the finish would be swallowed at home and pass for the wrong reason.
    let inrun = qedump(&running, "inrun");
    assert_eq!(
        (
            dump_field(inrun, "copystate"),
            dump_text(inrun, "status"),
            dump_field(inrun, "cursor")
        ),
        ("1", dump_text(plan4, "status"), dump_field(plan4, "cursor")),
        "the Y on the running Cancel reached the grid, or the run had ended \
         before it:\n{running}"
    );
    assert_eq!(
        landing(&mark_labels(&running), "inrun"),
        ("drive: key:y", Vec::new()),
        "the Y moved the keyboard:\n{running}"
    );
    assert!(
        running.contains("wait:copy finished run 1 (satisfied"),
        "the copy never finished — the report dump was timed, not gated:\n{running}"
    );
    let fin = qedump(&running, "fin");
    assert!(
        dump_field(fin, "copystate") == "2" && dump_text(fin, "report").contains("2 copied"),
        "the held copy did not finish with both picks copied: {fin}"
    );
    assert_the_finish_brings_the_keyboard_home(&running, "copy finished run 1", "copy");
    landings(&running, &[("f1", "tab", "copy open-dest")]);
    let closed4 = qedump(&running, "closed4");
    assert_eq!(
        (
            dump_field(closed4, "copy"),
            dump_field(closed4, "focusowner")
        ),
        ("false", "0"),
        "Esc with the keyboard on Open destination did not close the report: {closed4}"
    );

    // --- 5. Space on the running Cancel (QE's P3) --------------------------
    // Cancel is reached by launch 4's own path, Return from home then Tab,
    // which launch 4 has asserted: the Space can only press Cancel.
    let cancelling = run_held(
        "tabring-copy-cancel.jpg",
        &format!(
            "1500:wait:load settled gen 0;1700:key:y;1900:key:y;2100:copydest:{dest4};\
             2300:key:ctrl+e;2700:dump.plan5;\
             2900:key:return;3100:key:tab;3400:dump.runb;3600:key:space;\
             3700:wait:copy finished run 1;4000:dump.cancelled;\
             4200:key:tab;4500:dump.cb1;4700:key:escape;5000:dump.closed5",
            dest4 = dest4.display()
        ),
    );
    held(&cancelling, &["plan5", "runb", "cancelled", "cb1"]);
    let runb = qedump(&cancelling, "runb");
    assert_eq!(
        (
            dump_field(runb, "copystate"),
            dump_text(runb, "copyprogress")
        ),
        ("1", "Starting…"),
        "the copy was not running, held before its first file, when the Tab \
         landed: {runb}"
    );
    landings(&cancelling, &[("runb", "tab", "copy cancel")]);
    assert_eq!(
        last_gained_before(&mark_labels(&cancelling), "drive: key:space"),
        "focus: copy cancel gained",
        "the Space was not dispatched with the keyboard on the running Cancel \
         — the outcome read after it is not that press's:\n{cancelling}"
    );
    assert!(
        cancelling.contains("wait:copy finished run 1 (satisfied"),
        "the cancelled copy never put its report up — the dump was timed, \
         not gated:\n{cancelling}"
    );
    let cancelled = qedump(&cancelling, "cancelled");
    assert!(
        dump_field(cancelled, "copystate") == "2"
            && dump_text(cancelled, "report").contains("cancelled — finished files remain"),
        "Space on the running Cancel did not cancel the copy: {cancelled}"
    );
    assert_the_finish_brings_the_keyboard_home(&cancelling, "copy finished run 1", "copy");
    landings(&cancelling, &[("cb1", "tab", "copy open-dest")]);
    let closed5 = qedump(&cancelling, "closed5");
    assert_eq!(
        (
            dump_field(closed5, "copy"),
            dump_field(closed5, "focusowner")
        ),
        ("false", "0"),
        "Esc with the keyboard on Open destination did not close the report: {closed5}"
    );
    assert_eq!(
        std::fs::read_dir(&dest4).map(|d| d.count()).unwrap_or(0),
        0,
        "a copy cancelled during its hold left files at the destination"
    );
    for d in [&src, &dest, &dest2, &dest3, &dest4] {
        std::fs::remove_dir_all(d).ok();
    }
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

/// `"8.0 TB"` back into bytes — and, by returning `None` for anything
/// else, the check that a size on screen really went through the
/// formatter: `<digits>.<one digit>` and a KB/MB/GB/TB label, never a
/// raw count of bytes. Used so the refusal's "free" figure can be held
/// against the plan line's without pinning a number that belongs to
/// whichever disk this seat's temp directory sits on.
#[cfg(unix)]
fn size_token_bytes(token: &str) -> Option<u64> {
    let (number, unit) = token.split_once(' ')?;
    let (whole, frac) = number.split_once('.')?;
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if frac.len() != 1 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let shift = match unit {
        "KB" => 10,
        "MB" => 20,
        "GB" => 30,
        "TB" => 40,
        _ => return None,
    };
    Some((number.parse::<f64>().ok()? * (1u64 << shift) as f64) as u64)
}

/// The copy dialog's free-space refusal, driven through the REAL dialog
/// on the path that can refuse after the user has already answered
/// (fileops.md plan-time errors; brief 006 AC2, D5).
///
/// What this pins is WIRING, which is why it is a driven round and not
/// another unit test: the sentence can be perfect in the bridge and the
/// dialog still print core's developer-facing byte counts, because the
/// arm that fills `copy-error` is a different line from the one that
/// words the sentence. Only a real run through the dialog goes red when
/// that arm is left unrewired.
///
/// The round: two picks, the big one already present at the destination
/// under the same name. The plan preview PASSES — before an answer only
/// the clash-free bytes have to fit (fileops.md §3) — Enter raises the
/// clash question, and `B` (Keep both) replans with every byte counted,
/// which no destination can hold. The dialog drops back to the preview
/// with the refusal on it, having copied nothing.
///
/// Three things about the fixture, each measured (senior-developer plan,
/// 2026-09-12):
///
/// 1. `#[cfg(unix)]`: NTFS allocates real clusters on `set_len` unless
///    the file carries the sparse attribute, so an 8 TiB fixture cannot
///    exist on the Windows runner's disk at all. There the sentence is
///    pinned by the unit test
///    `copy_bridge::the_copy_refusal_reads_in_units_a_person_reads`.
/// 2. 8 TiB, fixed, and not a petabyte: ext4 — the ubuntu runner's
///    `/tmp` — caps a single file at 16 TiB, so anything larger fails
///    there with EFBIG. 8 TiB is half that ceiling and far above the
///    free space of any runner (~75 GB) or seat, so the test needs no
///    free-space call of its own. (A seat with 8 TiB free on its temp
///    filesystem would see the drop-back NOT refuse; assertion 4 says
///    so in its message.)
/// 3. The fixture is a real synthetic TIFF EXTENDED by `set_len`, never
///    an empty file of that length. A file the in-tree TIFF walker
///    rejects is handed to `rawler::rawsource::RawSource::new`, which
///    maps it with `MAP_POPULATE` and therefore pre-faults every page of
///    the mapping: an 8 TiB zero-filled `.ARW` never finished loading —
///    30.8 s of system time, 22.6 GB RSS, the 30 s wait cap missed. With
///    a TIFF at the front the walker answers in a few targeted reads,
///    rawler is never called, and the session settles in 34 ms.
///
/// The gaps between the steps are not a race: `copy_replan_with` runs
/// synchronously inside the Ctrl+E, Enter and `B` handlers on the UI
/// thread, and a `dump.` step is a later event on that same thread, so
/// each dump is ordered after its key by the event loop rather than by
/// the clock. The one gate that genuinely has to wait for work is the
/// load, and it is a `wait:`.
#[test]
#[cfg(unix)]
fn the_copy_refusal_reaches_the_dialog_on_the_drop_back_after_keep_both() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("refusal-src");
    let dest = out_dir().join("refusal-dest");
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    // The CLASHING pick: a real TIFF front (reason 3 above) and 8 TiB of
    // hole behind it. `set_len` allocates no blocks, so this costs the
    // disk nothing and `remove_dir_all` is instant.
    write_synthetic_raw(&src.join("big.ARW"), 400, 300, 1, 4096);
    std::fs::OpenOptions::new()
        .write(true)
        .open(src.join("big.ARW"))
        .unwrap()
        .set_len(8 << 40)
        .unwrap();
    // The clash-free pick, tiny, so the PREVIEW passes.
    write_synthetic_raw(&src.join("small.ARW"), 400, 300, 1, 4096);
    // The destination already holds the big one's name — the clash.
    std::fs::write(
        dest.join("big.ARW"),
        b"another camera's frame under the same name",
    )
    .unwrap();

    let script = format!(
        "500:wait:load settled gen 0;1000:key:y;1150:key:y;1300:copydest:{dest};\
         1500:key:ctrl+e;1800:dump.preview;2000:key:return;2300:dump.question;\
         2500:key:b;2800:dump.dropback",
        dest = dest.display()
    );
    let out = out_dir().join("copy-refusal.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let mut on_disk: Vec<String> = std::fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let fixture_len = std::fs::metadata(src.join("big.ARW")).unwrap().len();
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
    }

    // 1. The one gate that waits for real work fired.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the session never settled, so nothing below was driven:\n{stderr}"
    );

    // 2. The preview: no refusal yet, and the summary's worst case is the
    //    sparse pick plus the small one — `8.0 TB`, which on the
    //    three-tier formatter read `8192.0 GB to copy` (measured
    //    2026-09-12). This line is brief 006 AC1 at app level.
    let preview = qedump(&stderr, "preview");
    assert_eq!(dump_field(preview, "copystate"), "0", "{preview}");
    assert_eq!(
        dump_text(preview, "copyerror"),
        "",
        "the preview refused a plan that fits: {preview}"
    );
    let summary = dump_text(preview, "summary");
    assert!(
        summary.starts_with("2 picked · 8.0 TB to copy · ") && summary.ends_with(" free"),
        "the plan line does not print 8 TiB in the TB tier: {summary}"
    );
    assert!(
        dump_text(preview, "copynote")
            .contains("1 new · 1 already exist here — Copy will ask what to do"),
        "the preview does not pre-announce the clash: {preview}"
    );

    // 3. The clash question came up for the big one.
    let question = qedump(&stderr, "question");
    assert_eq!(dump_field(question, "copystate"), "3", "{question}");
    assert!(
        dump_text(question, "confirm")
            .contains("1 of your 2 picks already have files with these names in"),
        "the question does not state the counts: {question}"
    );

    // 4. The drop-back after Keep both: back on the plan preview, no
    //    report, and the refusal in the DIALOG's words. Split once on the
    //    joint so each half is asserted for what it is.
    let dropback = qedump(&stderr, "dropback");
    assert_eq!(dump_field(dropback, "copystate"), "0", "{dropback}");
    assert_eq!(dump_text(dropback, "report"), "", "{dropback}");
    let refusal = dump_text(dropback, "copyerror");
    let said_it = format!(
        "the drop-back did not say why in the dialog's words (fileops.md, \
         brief 006 R2) — or this seat has 8 TiB or more free on its temp \
         filesystem, which the fixture assumes it does not: {refusal}"
    );
    let (head, tail) = refusal.split_once(" and there is ").expect(&said_it);
    assert_eq!(head, "The copy needs 8.0 TB", "{said_it}");
    let free_token = tail
        .strip_suffix(" free at the destination.")
        .expect(&said_it);
    let refused_free = size_token_bytes(free_token).unwrap_or_else(|| panic!("{said_it}"));
    // The second size must be the FREE figure, not `needed` again and not
    // a constant: it is the same `statvfs` answer the plan line printed a
    // second earlier, so the two agree to well within a tier's rounding.
    let planned_free = size_token_bytes(
        summary
            .rsplit_once(" · ")
            .and_then(|(_, last)| last.strip_suffix(" free"))
            .unwrap_or_else(|| panic!("no free figure on the plan line: {summary}")),
    )
    .unwrap_or_else(|| panic!("the plan line's free figure is not a formatted size: {summary}"));
    let drift = refused_free.abs_diff(planned_free) as f64 / planned_free as f64;
    assert!(
        drift < 0.02,
        "the refusal's second size is not the destination's free space: \
         {refused_free} B against the plan line's {planned_free} B ({refusal})"
    );

    // 5. Nothing ran and nothing landed.
    assert!(
        !stderr.contains("copy finished run"),
        "a refused plan was executed:\n{stderr}"
    );
    assert_eq!(
        on_disk,
        vec!["big.ARW".to_string()],
        "the refused copy left files at the destination"
    );
    assert_eq!(
        fixture_len,
        8 << 40,
        "the app wrote to the source RAW (hard rule 1)"
    );
}

/// The copy dialog's free-space refusal on the PLAN PREVIEW — the other
/// path fileops.md's plan-time error list names ("on the plan preview
/// and on the drop-back after an answer alike"; brief 006 AC2). The twin
/// of `the_copy_refusal_reaches_the_dialog_on_the_drop_back_after_keep_both`,
/// which drives the drop-back and whose preview is built to PASS (it
/// asserts `copyerror=""` there), so the preview's refusing branch was
/// driven by nobody (QE 2026-09-12, D3).
///
/// Two picks and no clash: the tiny one first — its plan FITS and prints
/// the destination's free figure — then the 8 TiB one joins it and the
/// preview itself refuses. The fitting plan line is what the refusal's
/// second size is held against, the check the twin's assertion 4 makes
/// against ITS plan line: the two figures are one `statvfs` answer about
/// a second apart. The fixture's three reasons (`#[cfg(unix)]`, 8 TiB,
/// the TIFF front) are the twin's, written there.
///
/// The gaps between the steps are not a race, for the twin's reason:
/// `copy_replan_with` runs synchronously inside the Ctrl+E handler on the
/// UI thread, Escape closes the dialog on that thread, and every `dump.`
/// is a later event on it. The one gate that waits for work is the load,
/// and it is a `wait:`.
#[test]
#[cfg(unix)]
fn the_copy_refusal_reaches_the_dialog_on_the_plan_preview() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("refusal-preview-src");
    let dest = out_dir().join("refusal-preview-dest");
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    // The names sort the tiny pick FIRST: the first `y` picks it and
    // advances the cursor to the big one, which the second `y` picks
    // after Escape has closed the fitting plan.
    write_synthetic_raw(&src.join("a-small.ARW"), 400, 300, 1, 4096);
    write_synthetic_raw(&src.join("b-big.ARW"), 400, 300, 1, 4096);
    std::fs::OpenOptions::new()
        .write(true)
        .open(src.join("b-big.ARW"))
        .unwrap()
        .set_len(8 << 40)
        .unwrap();

    let script = format!(
        "500:wait:load settled gen 0;1000:key:y;1300:copydest:{dest};\
         1500:key:ctrl+e;1800:dump.fits;2000:key:escape;2300:key:y;\
         2600:key:ctrl+e;2900:dump.refused",
        dest = dest.display()
    );
    let out = out_dir().join("copy-refusal-preview.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let on_disk: Vec<String> = std::fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let fixture_len = std::fs::metadata(src.join("b-big.ARW")).unwrap().len();
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
    }

    // 1. The one gate that waits for real work fired.
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the session never settled, so nothing below was driven:\n{stderr}"
    );

    // 2. The tiny pick alone: the plan fits, and its line carries the
    //    free figure the refusal is held against. `4.1 KB` is the KB
    //    tier at app level (brief 006 AC1): the three-tier formatter
    //    printed `4170 B to copy` here.
    let fits = qedump(&stderr, "fits");
    assert_eq!(dump_field(fits, "copystate"), "0", "{fits}");
    assert_eq!(
        dump_text(fits, "copyerror"),
        "",
        "the preview refused a plan that fits: {fits}"
    );
    let fitting = dump_text(fits, "summary");
    assert!(
        fitting.starts_with("1 picked · 4.1 KB to copy · ") && fitting.ends_with(" free"),
        "the fitting plan line is not the one-pick line: {fitting}"
    );
    let planned_free = size_token_bytes(
        fitting
            .rsplit_once(" · ")
            .and_then(|(_, last)| last.strip_suffix(" free"))
            .unwrap_or_else(|| panic!("no free figure on the plan line: {fitting}")),
    )
    .unwrap_or_else(|| panic!("the plan line's free figure is not a formatted size: {fitting}"));

    // 3. Both picks: the PREVIEW refuses, in the dialog's words, before
    //    any question is asked.
    let refused = qedump(&stderr, "refused");
    assert_eq!(dump_field(refused, "copystate"), "0", "{refused}");
    assert_eq!(dump_text(refused, "report"), "", "{refused}");
    let refusal = dump_text(refused, "copyerror");
    let said_it = format!(
        "the preview did not say why in the dialog's words (fileops.md, \
         brief 006 R2) — or this seat has 8 TiB or more free on its temp \
         filesystem, which the fixture assumes it does not: {refusal}"
    );
    let (head, tail) = refusal.split_once(" and there is ").expect(&said_it);
    assert_eq!(head, "The copy needs 8.0 TB", "{said_it}");
    let free_token = tail
        .strip_suffix(" free at the destination.")
        .expect(&said_it);
    let refused_free = size_token_bytes(free_token).unwrap_or_else(|| panic!("{said_it}"));
    let drift = refused_free.abs_diff(planned_free) as f64 / planned_free as f64;
    assert!(
        drift < 0.02,
        "the refusal's second size is not the destination's free space: \
         {refused_free} B against the plan line's {planned_free} B ({refusal})"
    );

    // 4. Nothing ran and nothing landed.
    assert!(
        !stderr.contains("copy finished run"),
        "a refused plan was executed:\n{stderr}"
    );
    assert!(
        on_disk.is_empty(),
        "the refused copy left files at the destination: {on_disk:?}"
    );
    assert_eq!(
        fixture_len,
        8 << 40,
        "the app wrote to the source RAW (hard rule 1)"
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
         2900:key:return;3100:dump.inert;3150:key:n;3250:dump.inert_n;\
         3300:key:ctrl+o;3500:dump.accel;\
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
    // `N` is NOT an answer here. Copy Picks grew a fourth answer on
    // 2026-09-12 (brief 005), and this question deliberately did not: it
    // writes ONE file, and for one file "skip" IS Cancel (Manager D9), so
    // `n` must stay a swallowed key. The NUDGE half of that sentence —
    // that the swallow raises the "Pick one" line — stays review-verified:
    // this script's Enter on the question has already raised the nudge
    // when the `n` arrives (`clipnudged=true` from dump.inert on), so the
    // `n`'s own nudge cannot be told apart here (corrected 2026-10-04, QE
    // round 3 D3: this said there is no `clipnudged=` dump field and that
    // adding one was not worth the facility — the field exists since brief
    // 011, QE round 2 D3; QE 2026-09-12, senior-developer integrity review,
    // proposal 4).
    let inert_n = qedump(&stderr, "inert_n");
    assert_eq!(
        dump_field(inert_n, "clipstate"),
        "3",
        "N answered the video question — it has no New only (brief 005 \
         D9): one file, and skip is Cancel"
    );
    assert!(
        dump_text(inert_n, "clipconfirm").contains("a-c.mov"),
        "the question is no longer the question after an inert key: {inert_n}"
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

/// AC2 of brief 011 (issue #98; ui-grid.md "Modal keyboard containment",
/// video-export.md "The keyboard ring"): the Copy Picks ring's rule in the
/// Export Frames as Video dialog — Choose…, Cancel and Export in the plan
/// state, Open folder and Close on the report, wrapping — with
/// `focusowner` at the dialog's `-1` after every press and each landing
/// read from the control's own `focus: clip <name> gained` mark
/// (test-harness.md).
///
/// Three synthetic 400×300 frames, all selected, the cursor on `a`, so a
/// key that reached the grid behind the scrim would show: a `Y` would pick
/// `a`, move the cursor and collapse the selection the dialog is about to
/// export. The destination holds another day's `a-c.mov`. Six launches, in
/// this order:
///   1. The plan state. Ctrl+Tab and Ctrl+Shift+Tab first: neither moves
///      the keyboard (QE's P2; the copy test says why the ring's guard is
///      the one thing this pins). About next, over the dialog: a Tab under
///      it moves nothing and the first Esc closes About alone. Then four
///      Tabs — Choose…, Cancel, Export, and the wrap to Choose…; a `Y` with
///      the keyboard on Choose… marks nothing and keeps the selection; two
///      Shift+Tabs — Export (the wrap back), Cancel; Esc with the keyboard
///      on Cancel closes the dialog.
///
///      2a. Launch 2, dry, up to its Enter (QE round 3, D1; QE's P1):
///      launch 2's script, the same string, up to its Enter on Export, and
///      Esc in the Enter's place. It runs before launch 2 and asserts every
///      landing that Enter follows — among them the three Tabs from the home
///      the clash question's Esc leaves. The question is cancelled
///      unanswered, so the destination is left as it was, which the launch
///      asserts.
///   2. Two Tabs from the dialog's home reach Cancel, and Space there
///      closes the dialog. Reopened, the mixed path (the senior developer's
///      review F1, the copy test's strand): Tab puts the keyboard on
///      Choose…, a mouse click on Export — by name, `click:clip
///      export-close` — asks the clash question while the keyboard leaves
///      Choose… still enabled (`focus: clip choose lost`, `focus: clip
///      dialog gained`), Esc goes back to the plan and the next Tab lands
///      on Choose… with its own `gained`; two more Tabs reach Cancel and
///      Export, and Enter there asks the question with the keyboard home
///      (`focus: clip dialog gained`); Tab on the question moves nothing
///      and raises the nudge (`clipnudged=true`, QE round 2 D3); `B`
///      exports; on the report the first Shift+Tab from
///      home lands on Close, the LAST control, and Tab, Tab, Shift+Tab walk
///      Open folder, Close, Open folder — past the disabled Choose… and the
///      absent Cancel — and Esc with the keyboard on Open folder closes.
///   3. Export greyed (QE's P1, 2026-10-04): the destination is a FILE, so
///      the plan refuses it — `cliperror` names the refusal, which is what
///      tells a greyed Export from a selection the plan could not use — and
///      Export is greyed. Tab, Tab, Tab land on Choose…, Cancel and Choose…
///      again: the wrap passes over the greyed Export (video-export.md,
///      "Export skipped while it is greyed"); a `Y` with the keyboard on
///      Choose… marks nothing and keeps the selection; Shift+Tab, Shift+Tab
///      land on Cancel and Choose…; Esc closes. No Enter or Space anywhere
///      in this launch: Choose… opens the native folder picker.
///   4. The running state (QE's P3, the copy test's launch 4): an empty
///      destination, the writer held 3 s before its first frame
///      (`FASTCULL_CLIP_HOLD_MS`, test-harness.md). Enter from home starts
///      it; Tab lands on Cancel, the one control while it runs, with the
///      dump reading the running state and its `Starting…` line; a `Y`
///      there marks nothing and keeps the selection, the run still running
///      at the dump after it; the finish brings the keyboard home — `focus:
///      clip dialog gained` and `focus: clip cancel lost` after `clip
///      export finished run 1`, before the next step — and the next Tab
///      walks the report from Open folder; Esc closes, the selection kept.
///   5. The same start, then Space on the running Cancel: the export ends
///      before its first frame, the report says "Cancelled — nothing was
///      written", the keyboard comes home the same way, the next Tab lands
///      on Open folder, Esc closes, and the destination stays empty — not
///      even the hidden temp file.
///
/// Launches 1 and 2 split what the plan had as one launch, so that no
/// Space or Enter is pressed before the plan-state ring has been asserted:
/// in a single launch a ring without its wrap would put the Space meant for
/// Cancel on Choose… (the native folder picker), and a ring missing Cancel
/// would put the Enter meant for Export there. What keeps every Enter and
/// every Space off Choose… and Open folder (xdg-open), whatever a
/// regression does to the ring, is the copy test's rule: each follows a
/// walk that an EARLIER launch drove — the same keys, from the same home,
/// in the same dialog state — and asserted landing by landing, an earlier
/// launch being the only place a landing can be asserted before a press.
/// The presses, and the walks they follow:
///   - launch 2a's and launch 2's Space on Cancel: Tab, Tab from the
///     dialog's home as it opens — the two landings launch 1 asserted from
///     that home in the same plan state, where launch 1 makes them after a
///     Ctrl+Tab, a Ctrl+Shift+Tab, a Tab under About and (between them) a
///     `Y`, each asserted to move nothing;
///   - launch 2's Enter on Export: the Space on Cancel, the reopen, the
///     mixed path and three Tabs from the home the clash question's Esc
///     leaves — launch 2a's script, up to it; the report after it takes
///     Esc, never Enter or Space on Open folder;
///   - launch 4's Enter: from the dialog's home as it opens, after no
///     landing at all;
///   - launch 5's Space on the running Cancel: launch 4's script, up to it
///     — Enter from home, one Tab.
///
/// (Corrected 2026-10-04, QE round 3 D1: this said launch 2 reached Export
/// only by Tabs launch 1 had asserted, which no ring that passed launch 1
/// could turn onto Choose…, and recorded a regression showing only after
/// the reopen or the mixed path's Esc as a run-time residual; under a
/// one-token regression of the state change's home, `self.slot = 0` for
/// `-1` in the scope's `changed state` (QE's A5), launch 1 stayed green and
/// the Enter landed on Choose….)
///
/// Each press that activates a button also has its premise read at the
/// press itself (review F3, the copy test's [`last_gained_before`]): the
/// keyboard on Cancel when launch 2's Space goes (and launch 2a's), on
/// Export when its Return goes, and on the running Cancel when launch 5's
/// Space goes. These read the trace after the run, so they name a wrong
/// press after it happened; the earlier launches are what stop one from
/// happening.
///
/// RED on b6c238f, the head before the fix (brief 011 D3 measured the same
/// on f1520b9): launch 1's fourth Tab puts the keyboard on the grid's scope
/// behind the scrim — `focus: keys gained`, dump.t4 `focusowner=0` — where
/// D3 saw a `Y` mark the frame and collapse the selection. The same build
/// driven through launch 2 as it stood before review F1: the window's own
/// Tab walk reaches the live Export (`onexport` reads -1), Enter there asks
/// the question and leaves the keyboard on the destroyed button, the Tab
/// after it lands on `keys` (`qtab` reads 0) and the `B` never answers.
/// When this fails that way it is that defect; do not quiet it.
///
/// RED on 6eed28b, the build with the Export button's layout mark and
/// without review F1's fix: after the click on Export no `clip choose
/// lost`, and after Esc the Tab onto Choose… is silent (`focus: clip dialog
/// lost` alone, dump.c2's landing empty). When this fails that way it is
/// that defect; do not quiet it.
///
/// Mutants (2026-10-04), each alone: the scope's Tab arm removed → red at
/// dump.t4, `focusowner=0`; Cancel left out of `slot-ok` → red at dump.t2,
/// the Tab lands on Export; the wrap removed (`clamp` for `Math.mod` in
/// `walk`) → red at dump.t4, no landing; `clip-keys.focus()` removed from
/// the Export button's `clicked` (review F1) → red after launch 2a's click,
/// no `clip choose lost`; the `changed state` refocus removed (QE's mutant
/// E4) → red on 841bb1d through Enter-on-Export (no focus at all after the
/// Return, the Tab on the question on `keys`, `qtab` 0, the run dead at
/// `wait:clip export finished run 1`); since F1, which sends the keyboard
/// home from Export's own `clicked`, it guards the writer-finish path — a
/// run ending under a focused Cancel — which launch 4 drives with the held
/// writer: red at launch 4's dump.f1, `focusowner=0`, the Tab after the
/// finish on `keys` behind the scrim, and neither `clip dialog gained` nor
/// `clip cancel lost` after `clip export finished run 1` (corrected
/// 2026-10-04, QE D5: this said no fixture holds an export long enough to
/// Tab onto Cancel and called that path review-verified — a held writer
/// does); the running Cancel left out of `slot-ok` (`if (s == 2) { return
/// root.clip-state == 0; }`, QE's mutant E8) → red at launch 4's dump.run,
/// the Tab landing nowhere, and launch 5's script driven alone against that
/// build has its Space land on the dialog's home, where it is ignored, and
/// the export runs to its verified file; the home start removed (`slot +
/// dir` from -1) → red at dump.rhome, the first Shift+Tab on the report
/// landing on Open folder instead of Close; the ring's arm moved ahead of
/// the dialog's About containment → the Tab under About lands on Choose…,
/// red at dump.abtab; `slot-ok` approving the greyed Export (QE's mutant E9,
/// `if (s == 3) { return root.clip-state == 0 || root.clip-state == 2; }`)
/// → the ring's `focus()` on the disabled Export walks on in tree order to
/// the grid behind the scrim (the fourth canary's fact 10), red at launch
/// 3's dump.g3, `focusowner=0`, the third Tab's landing `focus: keys
/// gained` — issue #98 back in this dialog, with launches 1 and 2 green;
/// the ring's arm without its `!event.modifiers.control` (QE's mutant X6)
/// → Ctrl+Tab walks the ring, red at dump.ct, its landing `focus: clip
/// choose gained` (the Ctrl+Shift+Tab after it then lands on Export); the
/// ring's arm moved ahead of the clash question's branch (QE's mutant E11)
/// → Tab there is eaten without the nudge, red at dump.qtab, `clipnudged`
/// false (QE round 2 D3: green until the dump carried the field); every
/// state change leaving the ring at Choose… instead of home (`self.slot =
/// 0` for `-1` in the scope's `changed state`, QE's mutant A5) → after the
/// clash question's Esc the first Tab lands on Cancel, red at launch 2a's
/// dump.c2, `left: ["focus: clip cancel gained"] right: ["focus: clip
/// choose gained"]`, before any Enter reaches that walk (QE round 3 D1:
/// before launch 2a, launch 1 stayed green and launch 2's Enter landed on
/// Choose…).
#[test]
fn export_tab_walks_its_own_controls_and_never_leaves_the_dialog() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("tabring-clip-src");
    let dest = out_dir().join("tabring-clip-dest");
    // Launch 3's destination: a FILE, which the plan refuses.
    let dest_file = out_dir().join("tabring-clip-dest-file");
    // Launches 4 and 5 export for real, each into its own empty folder.
    let dest3 = out_dir().join("tabring-clip-dest3");
    let dest4 = out_dir().join("tabring-clip-dest4");
    for d in [&src, &dest, &dest3, &dest4] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    write_synthetic_raw(&src.join("a.ARW"), 400, 300, 1, 4096);
    write_synthetic_raw(&src.join("b.ARW"), 400, 300, 1, 5000);
    write_synthetic_raw(&src.join("c.ARW"), 400, 300, 1, 4500);
    std::fs::write(dest.join("a-c.mov"), b"another day's export").unwrap();
    std::fs::write(&dest_file, b"a file where the folder should be").unwrap();
    let opened = format!(
        "1400:wait:load settled gen 0;1500:select-all;1700:clipdest:{dest};1900:key:ctrl+shift+e;",
        dest = dest.display()
    );
    let run = |shot: &str, script: &str| -> String {
        let script = format!("{opened}{script}");
        shoot_env_stderr(
            &[src.to_str().unwrap()],
            &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
            &out_dir().join(shot),
        )
    };
    // Launches 4 and 5: the writer held 3 s before its first frame
    // (test-harness.md, `FASTCULL_CLIP_HOLD_MS`) — the copy test's hold, for
    // the same reason and with the same rule: a premise that reads red on a
    // slow runner is answered with a longer hold, never a later key. Their
    // own scripts in full: the shared prefix carries the clashing folder.
    let run_held = |shot: &str, script: &str| -> String {
        shoot_env_stderr(
            &[src.to_str().unwrap()],
            &[
                ("FASTCULL_TRACE", "1"),
                ("FASTCULL_CLIP_HOLD_MS", "3000"),
                ("FASTCULL_DRIVE", script),
            ],
            &out_dir().join(shot),
        )
    };
    // Every press's landing: its key, and the one control it reached.
    let landings = |stderr: &str, rows: &[(&str, &str, &str)]| {
        let labels = mark_labels(stderr);
        for (dump, key, mark) in rows {
            let (step, gained) = landing(&labels, dump);
            assert_eq!(
                step,
                format!("drive: key:{key}"),
                "the script's step before dump.{dump} is not the key this row \
                 is about:\n{stderr}"
            );
            assert_eq!(
                gained,
                vec![format!("focus: {mark} gained")],
                "the {key} before dump.{dump} did not land on {mark} alone \
                 (video-export.md, \"The keyboard ring\"):\n{stderr}"
            );
        }
    };
    // The token after every press — the old-red, asserted first.
    let held = |stderr: &str, dumps: &[&str]| {
        for dump in dumps {
            let line = qedump(stderr, dump);
            assert_eq!(
                dump_field(line, "focusowner"),
                "-1",
                "at dump.{dump} the keyboard has left the Export dialog \
                 (focusowner is not the dialog's -1): a Tab or Shift+Tab \
                 carried it behind the scrim — issue #98. When this fails \
                 this way it is that defect; do not quiet it:\n{stderr}"
            );
            assert_eq!(dump_field(line, "clip"), "true", "{line}");
        }
    };

    // --- 1. the plan state's ring, a Y, and Esc ----------------------------
    let ring = run(
        "tabring-clip-ring.jpg",
        "2300:dump.plan;2400:key:ctrl+tab;2700:dump.ct;2900:key:ctrl+shift+tab;3200:dump.cst;\
         3300:about;3600:dump.ab;3800:key:tab;4100:dump.abtab;\
         4300:key:escape;4600:dump.abclosed;\
         4800:key:tab;5100:dump.t1;5300:key:y;5600:dump.y1;\
         5800:key:tab;6100:dump.t2;6300:key:tab;6600:dump.t3;6800:key:tab;7100:dump.t4;\
         7300:key:shift+tab;7600:dump.s1;7800:key:shift+tab;8100:dump.s2;\
         8300:key:escape;8600:dump.closed",
    );
    held(
        &ring,
        &[
            "plan", "ct", "cst", "ab", "abtab", "abclosed", "t1", "y1", "t2", "t3", "t4", "s1",
            "s2",
        ],
    );
    assert!(
        ring.contains("wait:load settled gen 0 (satisfied"),
        "the selection was made before the folder settled:\n{ring}"
    );
    let plan = qedump(&ring, "plan");
    assert_eq!(
        (dump_field(plan, "clipstate"), dump_field(plan, "selected")),
        ("0", "3"),
        "{plan}"
    );
    assert!(
        dump_text(plan, "clipsummary").starts_with("3 frames · 400×300 ·")
            && dump_text(plan, "cliperror").is_empty(),
        "the premise is a clean plan, where Export is live: {plan}"
    );
    assert!(
        dump_text(plan, "status").contains("★0 ✕0"),
        "the premise is a session with no marks: {plan}"
    );
    // Ctrl+Tab and Ctrl+Shift+Tab move nothing (QE's P2; the copy test's
    // strand, for this scope).
    let labels = mark_labels(&ring);
    for (dump, chord) in [("ct", "ctrl+tab"), ("cst", "ctrl+shift+tab")] {
        let echo = format!("drive: key:{chord}");
        assert_eq!(
            landing(&labels, dump),
            (echo.as_str(), Vec::new()),
            "{chord} moved the keyboard in the Export dialog — ui-grid.md: \
             \"`Ctrl+Tab` does nothing unless a dialog's module says \
             otherwise\":\n{ring}"
        );
    }
    // About over the dialog takes Tab like every other key, and the first
    // Esc closes About alone (the copy test's strand, for this scope).
    assert_eq!(dump_field(qedump(&ring, "ab"), "about"), "true", "{ring}");
    assert_eq!(
        landing(&mark_labels(&ring), "abtab").1,
        Vec::<&str>::new(),
        "Tab under About moved the keyboard behind the popup — the ring ran \
         before the dialog's containment arm:\n{ring}"
    );
    let abclosed = qedump(&ring, "abclosed");
    assert_eq!(
        (
            dump_field(abclosed, "about"),
            dump_field(abclosed, "clipstate")
        ),
        ("false", "0"),
        "the first Esc did not close About alone: {abclosed}"
    );
    landings(
        &ring,
        &[
            ("t1", "tab", "clip choose"),
            ("t2", "tab", "clip cancel"),
            ("t3", "tab", "clip export-close"),
            ("t4", "tab", "clip choose"),
            ("s1", "shift+tab", "clip export-close"),
            ("s2", "shift+tab", "clip cancel"),
        ],
    );
    // A Y with the keyboard on Choose… marks nothing, moves nothing and
    // keeps the selection (video-export.md: the dialog never marks, never
    // moves the cursor, never touches the selection).
    let y1 = qedump(&ring, "y1");
    assert_eq!(
        (
            dump_text(y1, "status"),
            dump_field(y1, "cursor"),
            dump_field(y1, "selected")
        ),
        (
            dump_text(plan, "status"),
            dump_field(plan, "cursor"),
            dump_field(plan, "selected")
        ),
        "the Y on the focused Choose… reached the grid behind the dialog:\n{ring}"
    );
    assert_eq!(
        landing(&mark_labels(&ring), "y1").1,
        Vec::<&str>::new(),
        "the Y moved the keyboard:\n{ring}"
    );
    let closed = qedump(&ring, "closed");
    assert_eq!(
        (
            dump_field(closed, "clip"),
            dump_field(closed, "focusowner"),
            dump_field(closed, "selected")
        ),
        ("false", "0", "3"),
        "Esc with the keyboard on Cancel did not close the dialog, hand the \
         keyboard back and keep the selection: {closed}"
    );

    // Launch 2's walk up to its Enter on Export — Space on Cancel, the
    // reopen, the mixed path, and three Tabs from the home the clash
    // question's Esc leaves — which launch 2a plays first (QE round 3, D1).
    let flow_walk = "2300:dump.open;2500:key:tab;2700:key:tab;3000:dump.oncancel;\
         3200:key:space;3500:dump.cancelled;\
         3800:key:ctrl+shift+e;4100:key:tab;4400:dump.c1;\
         4600:click:clip export-close;4900:dump.cq;5100:key:escape;5400:dump.cback;\
         5600:key:tab;5900:dump.c2;6100:key:tab;6400:dump.c3;6600:key:tab;6900:dump.onexport;";

    // --- 2a. launch 2, dry, up to its Enter (QE round 3, D1) --------------
    // Esc where launch 2 presses Enter on Export: every landing that Enter
    // follows is asserted here first, and launch 2 runs only if this one
    // passed. The question is cancelled unanswered, so nothing is written
    // and launch 2 finds the destination as this launch did.
    let mixed = run(
        "tabring-clip-mixed.jpg",
        &format!("{flow_walk}7100:key:escape;7400:dump.closed"),
    );
    held(
        &mixed,
        &[
            "open", "oncancel", "c1", "cq", "cback", "c2", "c3", "onexport",
        ],
    );
    let mlabels = mark_labels(&mixed);
    assert_eq!(
        last_gained_before(&mlabels, "drive: key:space"),
        "focus: clip cancel gained",
        "`drive: key:space` was not dispatched with the keyboard on Cancel — \
         the outcome read after it is not that press's:\n{mixed}"
    );
    let mcancelled = qedump(&mixed, "cancelled");
    assert_eq!(
        (
            dump_field(mcancelled, "clip"),
            dump_field(mcancelled, "focusowner"),
            dump_field(mcancelled, "selected")
        ),
        ("false", "0", "3"),
        "Space on the focused Cancel did not close the dialog, hand the \
         keyboard back and keep the selection: {mcancelled}"
    );
    assert_eq!(
        (
            dump_field(qedump(&mixed, "cq"), "clipstate"),
            dump_field(qedump(&mixed, "cback"), "clipstate")
        ),
        ("3", "0"),
        "the click on Export did not ask the clash question, or Esc did not \
         take it back to the plan:\n{mixed}"
    );
    let mclick = label_positions(&mlabels, "drive: click:clip export-close");
    let mcq = label_positions(&mlabels, "drive: dump.cq");
    assert!(
        mclick.len() == 1 && mcq.len() == 1 && mclick[0] < mcq[0],
        "the click strand is not in the trace as written:\n{mixed}"
    );
    for mark in ["focus: clip choose lost", "focus: clip dialog gained"] {
        assert!(
            marks_between(&mlabels, mclick[0], mcq[0], mark) > 0,
            "after the click on Export no `{mark}`: the keyboard did not leave \
             Choose… before the run disabled it (ui-grid.md, \"Modal keyboard \
             containment\"; review F1). When this fails this way it is that \
             defect; do not quiet it:\n{mixed}"
        );
    }
    landings(
        &mixed,
        &[
            ("oncancel", "tab", "clip cancel"),
            ("c1", "tab", "clip choose"),
            // From the home the clash question's Esc leaves: the walk
            // launch 2's Enter follows.
            ("c2", "tab", "clip choose"),
            ("c3", "tab", "clip cancel"),
            ("onexport", "tab", "clip export-close"),
        ],
    );
    let mclosed = qedump(&mixed, "closed");
    assert_eq!(
        (
            dump_field(mclosed, "clip"),
            dump_field(mclosed, "focusowner"),
            dump_field(mclosed, "selected")
        ),
        ("false", "0", "3"),
        "Esc with the keyboard on Export did not close the dialog, hand the \
         keyboard back and keep the selection: {mclosed}"
    );
    let mut mleft: Vec<String> = std::fs::read_dir(&dest)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    mleft.sort();
    assert_eq!(
        mleft,
        vec!["a-c.mov".to_string()],
        "the question cancelled unanswered wrote at the destination, so \
         launch 2 would not start from the folder launch 2a did"
    );

    // --- 2. Space on Cancel; the mixed path; Enter on Export, the question,
    // the report ----------------------------------------------------------
    let flow = run(
        "tabring-clip-flow.jpg",
        &format!(
            "{flow_walk}7100:key:return;7400:dump.q;7600:key:tab;7900:dump.qtab;\
             8100:key:b;8200:wait:clip export finished run 1;8900:dump.report;\
             9100:key:shift+tab;9400:dump.rhome;9600:key:tab;9900:dump.r1;\
             10100:key:tab;10400:dump.r2;10600:key:shift+tab;10900:dump.r3;\
             11100:key:escape;11400:dump.end"
        ),
    );
    held(
        &flow,
        &[
            "open", "oncancel", "c1", "cq", "cback", "c2", "c3", "onexport", "q", "qtab", "report",
            "rhome", "r1", "r2", "r3",
        ],
    );
    // The mixed path (review F1), the copy test's strand: the keyboard on
    // Choose… by Tab, a mouse click on Export asks the clash question, Esc
    // goes back to the plan.
    assert_eq!(
        (
            dump_field(qedump(&flow, "cq"), "clipstate"),
            dump_field(qedump(&flow, "cback"), "clipstate")
        ),
        ("3", "0"),
        "the click on Export did not ask the clash question, or Esc did not \
         take it back to the plan:\n{flow}"
    );
    // The click let go of Choose… while Choose… was still enabled (the copy
    // test says why: a disabled control ignores the FocusOut, fact 11).
    let labels = mark_labels(&flow);
    let click = label_positions(&labels, "drive: click:clip export-close");
    let cq = label_positions(&labels, "drive: dump.cq");
    assert!(
        click.len() == 1 && cq.len() == 1 && click[0] < cq[0],
        "the click strand is not in the trace as written:\n{flow}"
    );
    for mark in ["focus: clip choose lost", "focus: clip dialog gained"] {
        assert!(
            marks_between(&labels, click[0], cq[0], mark) > 0,
            "after the click on Export no `{mark}`: the keyboard did not leave \
             Choose… before the run disabled it (ui-grid.md, \"Modal keyboard \
             containment\"; review F1). When this fails this way it is that \
             defect; do not quiet it:\n{flow}"
        );
    }
    // The premise of each press that activates a button, read at the press
    // itself (review F3; the copy test's helper): the Space was dispatched
    // with the keyboard on Cancel, the Return with it on Export. The closing
    // Esc needs none: Esc bubbles to the scope wherever the keyboard is and
    // never presses a button.
    for (echo, on) in [
        ("drive: key:space", "focus: clip cancel gained"),
        ("drive: key:return", "focus: clip export-close gained"),
    ] {
        assert_eq!(
            last_gained_before(&labels, echo),
            on,
            "`{echo}` was not dispatched with the keyboard where the script \
             put it — the outcome read after it is not that press's:\n{flow}"
        );
    }
    landings(
        &flow,
        &[
            ("oncancel", "tab", "clip cancel"),
            ("c1", "tab", "clip choose"),
            // After Esc, Choose… is focused again WITH its own `gained`.
            ("c2", "tab", "clip choose"),
            ("c3", "tab", "clip cancel"),
            ("onexport", "tab", "clip export-close"),
            ("q", "return", "clip dialog"),
            ("rhome", "shift+tab", "clip export-close"),
            ("r1", "tab", "clip open-folder"),
            ("r2", "tab", "clip export-close"),
            ("r3", "shift+tab", "clip open-folder"),
        ],
    );
    let cancelled = qedump(&flow, "cancelled");
    assert_eq!(
        (
            dump_field(cancelled, "clip"),
            dump_field(cancelled, "focusowner"),
            dump_field(cancelled, "selected")
        ),
        ("false", "0", "3"),
        "Space on the focused Cancel did not close the dialog, hand the \
         keyboard back and keep the selection: {cancelled}"
    );
    let q = qedump(&flow, "q");
    assert!(
        dump_field(q, "clipstate") == "3" && dump_text(q, "clipconfirm").contains("a-c.mov"),
        "Enter on the focused Export did not ask the clash question: {q}"
    );
    // The question opens without its nudge, so the nudge at dump.qtab is
    // the Tab's.
    assert_eq!(
        dump_field(q, "clipnudged"),
        "false",
        "the clash question opened already nudging: {q}"
    );
    // Tab on the question moves nothing and is swallowed WITH the nudge,
    // like every key that is not an answer (video-export.md, "The keyboard
    // ring"; QE round 2 D3): the question's branch sees the Tab before the
    // ring's arm does.
    let qtab = qedump(&flow, "qtab");
    assert_eq!(
        (
            dump_field(qtab, "clipstate"),
            dump_field(qtab, "clipnudged")
        ),
        ("3", "true"),
        "Tab on the clash question must be swallowed with the nudge, like \
         every key that is not an answer (video-export.md): {qtab}"
    );
    assert_eq!(
        landing(&mark_labels(&flow), "qtab").1,
        Vec::<&str>::new(),
        "Tab on the clash question moved the keyboard:\n{flow}"
    );
    assert!(
        flow.contains("wait:clip export finished run 1 (satisfied"),
        "the `B` answered nothing — the report dump was timed, not gated:\n{flow}"
    );
    let report = qedump(&flow, "report");
    assert_eq!(dump_field(report, "clipstate"), "2", "{report}");
    assert!(
        dump_text(report, "clipreport").contains("a-c_1.mov")
            && dump_text(report, "clipreport").contains("all checksums verified"),
        "the `B` after Enter on the focused Export did not keep both: {report}"
    );
    let end = qedump(&flow, "end");
    assert_eq!(
        (
            dump_field(end, "clip"),
            dump_field(end, "focusowner"),
            dump_field(end, "selected")
        ),
        ("false", "0", "3"),
        "Esc with the keyboard on Open folder did not close the dialog and \
         hand the keyboard back: {end}"
    );

    // --- 3. Export greyed: the ring passes over it both ways (QE's P1) -----
    // Its own prefix: the shared one carries the folder destination.
    let script = format!(
        "1400:wait:load settled gen 0;1500:select-all;1700:clipdest:{dest_file};\
         1900:key:ctrl+shift+e;2300:dump.plan;\
         2500:key:tab;2800:dump.g1;3000:key:tab;3300:dump.g2;3500:key:tab;3800:dump.g3;\
         4000:key:y;4300:dump.gy;\
         4500:key:shift+tab;4800:dump.g4;5000:key:shift+tab;5300:dump.g5;\
         5500:key:escape;5800:dump.closed",
        dest_file = dest_file.display()
    );
    let grey = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out_dir().join("tabring-clip-grey.jpg"),
    );
    held(&grey, &["plan", "g1", "g2", "g3", "gy", "g4", "g5"]);
    assert!(
        grey.contains("wait:load settled gen 0 (satisfied"),
        "the selection was made before the folder settled:\n{grey}"
    );
    // The premise names the refusal: Export greyed because the destination
    // is not a folder, not because the selection gave the plan nothing to
    // export.
    let gplan = qedump(&grey, "plan");
    assert_eq!(
        (
            dump_field(gplan, "clipstate"),
            dump_text(gplan, "cliperror"),
            dump_field(gplan, "selected")
        ),
        ("0", "the destination is not a folder", "3"),
        "the premise is a plan the destination refuses, where Export is \
         greyed: {gplan}"
    );
    assert!(
        dump_text(gplan, "status").contains("★0 ✕0"),
        "the premise is a session with no marks: {gplan}"
    );
    landings(
        &grey,
        &[
            ("g1", "tab", "clip choose"),
            ("g2", "tab", "clip cancel"),
            // The wrap, over the greyed Export.
            ("g3", "tab", "clip choose"),
            ("g4", "shift+tab", "clip cancel"),
            ("g5", "shift+tab", "clip choose"),
        ],
    );
    let gy = qedump(&grey, "gy");
    assert_eq!(
        (
            dump_text(gy, "status"),
            dump_field(gy, "cursor"),
            dump_field(gy, "selected")
        ),
        (
            dump_text(gplan, "status"),
            dump_field(gplan, "cursor"),
            dump_field(gplan, "selected")
        ),
        "the Y on the focused Choose… reached the grid behind the dialog:\n{grey}"
    );
    assert_eq!(
        landing(&mark_labels(&grey), "gy").1,
        Vec::<&str>::new(),
        "the Y moved the keyboard:\n{grey}"
    );
    let gclosed = qedump(&grey, "closed");
    assert_eq!(
        (
            dump_field(gclosed, "clip"),
            dump_field(gclosed, "focusowner"),
            dump_field(gclosed, "selected")
        ),
        ("false", "0", "3"),
        "Esc with the keyboard on Choose… did not close the dialog, hand the \
         keyboard back and keep the selection: {gclosed}"
    );

    // --- 4. the running ring and the finish (QE's P3) ---------------------
    let running = run_held(
        "tabring-clip-run.jpg",
        &format!(
            "1400:wait:load settled gen 0;1500:select-all;1700:clipdest:{dest3};\
             1900:key:ctrl+shift+e;2300:dump.plan4;\
             2500:key:return;2700:key:tab;3000:dump.run;3200:key:y;3500:dump.inrun;\
             3700:wait:clip export finished run 1;4000:dump.fin;\
             4200:key:tab;4500:dump.f1;4700:key:escape;5000:dump.closed4",
            dest3 = dest3.display()
        ),
    );
    held(&running, &["plan4", "run", "inrun", "fin", "f1"]);
    assert!(
        running.contains("fastcull: FASTCULL_CLIP_HOLD_MS=3000 — every video export is held"),
        "the writer's hold was not read, so this launch's run is not the held \
         one it is about:\n{running}"
    );
    let plan4 = qedump(&running, "plan4");
    assert!(
        dump_field(plan4, "clipstate") == "0"
            && dump_field(plan4, "selected") == "3"
            && dump_text(plan4, "clipsummary").starts_with("3 frames · 400×300 ·")
            && dump_text(plan4, "cliperror").is_empty(),
        "the premise is a clean plan, where Return from home exports: {plan4}"
    );
    // The launch's premise (the copy test's): still running, held before
    // its first frame, when the Tab lands — and the Tab on its Cancel.
    let run4 = qedump(&running, "run");
    assert_eq!(
        (
            dump_field(run4, "clipstate"),
            dump_text(run4, "clipprogress")
        ),
        ("1", "Starting…"),
        "the export was not running, held before its first frame, when the \
         Tab landed: {run4}"
    );
    landings(&running, &[("run", "tab", "clip cancel")]);
    // A Y on the running Cancel marks nothing, moves nothing and keeps the
    // selection, with the run still running at the dump after it.
    let inrun = qedump(&running, "inrun");
    assert_eq!(
        (
            dump_field(inrun, "clipstate"),
            dump_text(inrun, "status"),
            dump_field(inrun, "cursor"),
            dump_field(inrun, "selected")
        ),
        (
            "1",
            dump_text(plan4, "status"),
            dump_field(plan4, "cursor"),
            dump_field(plan4, "selected")
        ),
        "the Y on the running Cancel reached the grid, or the run had ended \
         before it:\n{running}"
    );
    assert_eq!(
        landing(&mark_labels(&running), "inrun"),
        ("drive: key:y", Vec::new()),
        "the Y moved the keyboard:\n{running}"
    );
    assert!(
        running.contains("wait:clip export finished run 1 (satisfied"),
        "the export never finished — the report dump was timed, not gated:\n{running}"
    );
    let fin = qedump(&running, "fin");
    assert!(
        dump_field(fin, "clipstate") == "2"
            && dump_text(fin, "clipreport").contains("a-c.mov")
            && dump_text(fin, "clipreport").contains("all checksums verified"),
        "the held export did not finish with its file verified: {fin}"
    );
    assert_the_finish_brings_the_keyboard_home(&running, "clip export finished run 1", "clip");
    landings(&running, &[("f1", "tab", "clip open-folder")]);
    let closed4 = qedump(&running, "closed4");
    assert_eq!(
        (
            dump_field(closed4, "clip"),
            dump_field(closed4, "focusowner"),
            dump_field(closed4, "selected")
        ),
        ("false", "0", "3"),
        "Esc with the keyboard on Open folder did not close the report, hand \
         the keyboard back and keep the selection: {closed4}"
    );

    // --- 5. Space on the running Cancel (QE's P3) --------------------------
    // Cancel is reached by launch 4's own path, which launch 4 has
    // asserted: the Space can only press Cancel.
    let cancelling = run_held(
        "tabring-clip-cancel.jpg",
        &format!(
            "1400:wait:load settled gen 0;1500:select-all;1700:clipdest:{dest4};\
             1900:key:ctrl+shift+e;2300:dump.plan5;\
             2500:key:return;2700:key:tab;3000:dump.runb;3200:key:space;\
             3300:wait:clip export finished run 1;3600:dump.cancelled;\
             3800:key:tab;4100:dump.cb1;4300:key:escape;4600:dump.closed5",
            dest4 = dest4.display()
        ),
    );
    held(&cancelling, &["plan5", "runb", "cancelled", "cb1"]);
    let runb = qedump(&cancelling, "runb");
    assert_eq!(
        (
            dump_field(runb, "clipstate"),
            dump_text(runb, "clipprogress")
        ),
        ("1", "Starting…"),
        "the export was not running, held before its first frame, when the \
         Tab landed: {runb}"
    );
    landings(&cancelling, &[("runb", "tab", "clip cancel")]);
    assert_eq!(
        last_gained_before(&mark_labels(&cancelling), "drive: key:space"),
        "focus: clip cancel gained",
        "the Space was not dispatched with the keyboard on the running Cancel \
         — the outcome read after it is not that press's:\n{cancelling}"
    );
    assert!(
        cancelling.contains("wait:clip export finished run 1 (satisfied"),
        "the cancelled export never put its report up — the dump was timed, \
         not gated:\n{cancelling}"
    );
    let cancelled = qedump(&cancelling, "cancelled");
    assert!(
        dump_field(cancelled, "clipstate") == "2"
            && dump_text(cancelled, "clipreport").contains("Cancelled — nothing was written"),
        "Space on the running Cancel did not cancel the export: {cancelled}"
    );
    assert_the_finish_brings_the_keyboard_home(&cancelling, "clip export finished run 1", "clip");
    landings(&cancelling, &[("cb1", "tab", "clip open-folder")]);
    let closed5 = qedump(&cancelling, "closed5");
    assert_eq!(
        (
            dump_field(closed5, "clip"),
            dump_field(closed5, "focusowner"),
            dump_field(closed5, "selected")
        ),
        ("false", "0", "3"),
        "Esc with the keyboard on Open folder did not close the report, hand \
         the keyboard back and keep the selection: {closed5}"
    );
    assert_eq!(
        std::fs::read_dir(&dest4).map(|d| d.count()).unwrap_or(0),
        0,
        "an export cancelled during its hold left a file at the destination"
    );
    for d in [&src, &dest, &dest3, &dest4] {
        std::fs::remove_dir_all(d).ok();
    }
    std::fs::remove_file(&dest_file).ok();
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

/// The file-manager companions of the collapse rule (brief 002, user
/// decision 2026-09-06, AC3 and AC4): Ctrl+arrows, Ctrl+PgUp/PgDn/Home/End
/// and Ctrl+`[`/`]` move the cursor exactly where the plain key would and
/// leave the selection alone, and Ctrl+Space adds or removes the frame
/// under the cursor without moving it.
///
/// Driven with REAL chords (`key:ctrl+…` synthesises a held Control the way
/// a keyboard does), because that is the whole claim: these keys were dead
/// before this change — the main scope's Ctrl block rejected everything but
/// O/Q/A/E/Shift+E/Shift+B — so a `nav` token would prove the handler and
/// not the binding.
///
/// The route through the burst pattern (`SYNTHETIC_BURST_RUNS`, single 0,
/// A = 1..=5, single 6, B = 7..=9, C = 10..=17, … G = 34..=39) is the
/// user's own "two non-adjacent bursts" sequence: Ctrl+Shift+B on A,
/// Ctrl+`]` three times, Ctrl+Shift+B on C.
#[test]
fn ctrl_navigation_keeps_the_selection_and_ctrl_space_toggles() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("ctrl-nav.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "600:key:];800:key:ctrl+shift+b;1000:dump.a;\
                 1200:key:ctrl+];1400:dump.h1;1600:key:ctrl+];1700:key:ctrl+];1900:dump.h3;\
                 2100:key:ctrl+shift+b;2300:dump.two;\
                 2500:key:ctrl+right;2700:dump.cr;2900:key:ctrl+pgdn;3100:dump.cpd;\
                 3300:key:ctrl+home;3500:dump.ch;3700:key:ctrl+end;3900:dump.ce;\
                 4100:key:ctrl+[;4300:dump.cb;\
                 4500:key:ctrl+space;4700:dump.t1;4900:key:ctrl+left;5000:key:ctrl+space;5200:dump.t2;\
                 5400:key:ctrl+space;5600:dump.t3;5800:key:ctrl+up;6000:dump.cu;\
                 6200:key:];6400:dump.plain;\
                 6600:key:escape;6800:key:home;7000:key:ctrl+space;7200:key:ctrl+right;7300:key:ctrl+right;\
                 7500:key:ctrl+space;7700:key:ctrl+right;7900:key:ctrl+space;8100:dump.three;\
                 8300:key:ctrl+space;8500:dump.two2;\
                 8700:key:escape;8900:filter:picked;9100:dump.empty;\
                 9300:key:ctrl+space;9500:filter:all;9700:dump.ghost;\
                 9900:key:escape;10100:key:home;10300:key:];\
                 10500:key:ctrl+shift+b;10700:key:ctrl+];\
                 10900:key:shift+right;11100:dump.hopfresh",
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
    assert_eq!(at("a"), (1, 5), "Ctrl+Shift+B on A's opener takes A whole");
    // --- Ctrl+`]` walks with the selection in hand (AC3) -------------------
    assert_eq!(at("h1"), (6, 5), "Ctrl+`]` lands on the single, keeps A");
    assert_eq!(at("h3"), (10, 5), "three hops: B's opener, then C's");
    assert_eq!(
        at("two"),
        (10, 13),
        "the second Ctrl+Shift+B adds C to A — two non-adjacent bursts (AC3)"
    );
    // --- and so does every other Ctrl-move --------------------------------
    assert_eq!(at("cr"), (11, 13), "Ctrl+Right moves, keeps");
    let (cpd_cursor, cpd_sel) = at("cpd");
    assert!(
        cpd_cursor > 11,
        "Ctrl+PgDn did not move the cursor (it was at 11, it is at {cpd_cursor})"
    );
    assert_eq!(cpd_sel, 13, "Ctrl+PgDn keeps the selection");
    assert_eq!(at("ch"), (0, 13), "Ctrl+Home");
    assert_eq!(at("ce"), (39, 13), "Ctrl+End");
    assert_eq!(
        at("cb"),
        (34, 13),
        "Ctrl+`[` from mid-G re-anchors on G's opener, like `[`"
    );
    // --- Ctrl+Space toggles the frame under the cursor (AC4) --------------
    assert_eq!(at("t1"), (34, 14), "Ctrl+Space adds 34, cursor unmoved");
    assert_eq!(at("t2"), (33, 15), "Ctrl+Left then Ctrl+Space adds 33");
    assert_eq!(at("t3"), (33, 14), "Ctrl+Space again removes it");
    let (cu_cursor, cu_sel) = at("cu");
    // The row width follows the window and the zoom, so what is pinned is
    // that the cursor MOVED, never where to.
    assert_ne!(cu_cursor, 33, "Ctrl+Up did not move the cursor");
    assert_eq!(cu_sel, 14, "Ctrl+Up keeps the selection");
    // --- Ctrl+Space builds a discontiguous selection from nothing (AC4) ---
    assert_eq!(
        at("three"),
        (3, 3),
        "Ctrl+Space on three frames, walked between with Ctrl+Right"
    );
    assert_eq!(at("two2"), (3, 2), "and one press takes one away");
    // The contrast this test exists to draw: the same `]`, without Ctrl,
    // ends the selection (rule 1). It read 14 before the rule landed.
    let (_, plain_sel) = at("plain");
    assert_eq!(
        plain_sel, 0,
        "a plain `]` kept the selection that Ctrl+`]` is for"
    );
    // The status bar counts what the dump counts, and goes silent when
    // there is nothing to count.
    assert!(
        dump_text(qedump(&stderr, "two"), "status").contains("· 13 selected"),
        "{}",
        qedump(&stderr, "two")
    );
    assert!(
        !dump_text(qedump(&stderr, "plain"), "status").contains("selected"),
        "an empty selection is silent: {}",
        qedump(&stderr, "plain")
    );
    // --- and a cursor the filter has hidden toggles nothing ---------------
    // `select-toggle`'s guard (`nav.rs`, `cursor_pos().is_some()`), which
    // is only reachable while the view is empty and was review-verified
    // until the harness learned `filter:` (senior-developer review F2).
    // Nothing is marked in this session, so the Picked filter empties the
    // view; the cursor id survives as a stale one, and a Ctrl+Space there
    // must not select it. Widening the filter again is what makes the
    // difference visible — a ghost selected member is invisible in
    // `selected=` while the view that would count it is empty.
    let empty = qedump(&stderr, "empty");
    assert!(
        dump_text(empty, "status").contains("(0/0)"),
        "the Picked filter did not empty the view of an unmarked session, \
         so the guard below is not being exercised: {empty}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "ghost"), "selected"),
        "0",
        "Ctrl+Space selected a frame the filter had hidden — what you see \
         is what you stamp (this reads 1 without the guard):\n{stderr}"
    );
    // --- Ctrl+`[` / Ctrl+`]` RESET THE ANCHOR, like Ctrl+arrows ----------
    // The other half of the spec's Ctrl+`[`/`]` row (ui-grid.md: "and the
    // anchor reset of Ctrl+arrows"), which had no test until the
    // senior developer's re-review found a mutant surviving it (N-2,
    // 2026-09-06): making the burst hop KEEP the anchor left T1-T4 green.
    //
    // `]` to A's opener, Ctrl+Shift+B takes A (anchor armed at 1),
    // Ctrl+`]` hops to the single at 6 — and must drop that anchor, so
    // the Shift+Right after it is a FRESH span: {6, 7}, two frames. With
    // the anchor kept it would continue from 1 and read 1..=7, seven.
    assert_eq!(
        at("hopfresh"),
        (7, 2),
        "the Shift+Right after a Ctrl+`]` continued the burst's anchor \
         instead of starting fresh — Ctrl-navigation resets it (this \
         reads (7, 7) when the hop keeps the anchor):\n{stderr}"
    );
}

/// Rule 1 of the selection rule (brief 002, the user's decision of
/// 2026-09-06): EVERY unmodified cursor move empties the selection — an
/// arrow, `]`, PgDn, End, and the advance a `Y` performs — while `U`,
/// which does not move, leaves it alone.
///
/// Every strand here was RED before the rule landed (the counts it read
/// then are named beside each assertion): a plain move used to fold the
/// live span into the selection and keep it, which is what carried a
/// finished export's frames into the next one.
#[test]
fn a_plain_move_collapses_the_selection_in_the_grid() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("collapse-grid.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "600:key:right;700:key:right;800:key:right;1000:key:ctrl+shift+b;1200:dump.sel;\
                 1400:key:right;1600:dump.arrow;1800:key:ctrl+shift+b;2000:dump.sel2;\
                 2200:key:];2400:dump.bracket;2600:key:shift+];2800:dump.span;\
                 3000:key:pgdn;3200:dump.pgdn;3400:key:end;3600:key:ctrl+shift+b;3800:dump.sel3;\
                 4000:key:right;4200:dump.edge;\
                 4400:key:home;4600:key:right;4700:key:right;4800:key:right;\
                 5000:key:ctrl+shift+b;5200:dump.sel4;\
                 5400:key:y;5600:dump.y;5800:key:left;6000:dump.marked;\
                 6200:key:right;6300:key:right;6500:dump.next;\
                 6700:key:ctrl+shift+b;6900:dump.sel5;7100:key:u;7300:dump.u;\
                 7500:key:home;7700:key:y;7800:key:y;7900:key:y;8100:dump.picks;\
                 8300:filter:picked;8500:dump.filtered;8700:key:home;\
                 8900:select-all;9100:dump.all;9300:key:u;9500:dump.removed;\
                 9700:filter:all;9900:select-all;10100:dump.all40;\
                 10300:filter:rejected;10500:dump.emptyview;10700:key:right;\
                 10900:filter:all;11100:dump.after",
            ),
        ],
        &out,
    );
    let at = |label: &str| -> (usize, usize) {
        let d = qedump(&stderr, label);
        (
            dump_field(d, "cursor").parse().unwrap(),
            dump_field(d, "selected").parse().unwrap(),
        )
    };
    let status = |label: &str| dump_text(qedump(&stderr, label), "status").to_string();

    // --- an arrow ---------------------------------------------------------
    assert_eq!(at("sel"), (3, 5), "Ctrl+Shift+B on frame 3 takes A whole");
    assert_eq!(
        at("arrow"),
        (4, 0),
        "a plain Right left the selection standing (it read 5 before the rule)"
    );
    // --- `[` / `]` --------------------------------------------------------
    assert_eq!(at("sel2"), (4, 5));
    assert_eq!(
        at("bracket"),
        (6, 0),
        "a plain `]` left the selection standing (it read 5 before the rule)"
    );
    // And the Shift+`]` that follows an emptied selection is a fresh span:
    // from the single at 6 it takes 6 plus B, and nothing else.
    assert_eq!(at("span"), (7, 4), "a fresh burst span: the single plus B");
    // --- PgDn -------------------------------------------------------------
    let (pgdn_cursor, pgdn_sel) = at("pgdn");
    assert!(
        pgdn_cursor > 7,
        "PgDn did not move the cursor (it was at 7, it is at {pgdn_cursor})"
    );
    assert_eq!(pgdn_sel, 0, "PgDn left the selection standing");
    // --- a key that moves NOTHING still collapses --------------------------
    // End lands on the last frame, Ctrl+Shift+B takes G, and the Right
    // after it has nowhere to go: the selection goes anyway, because the
    // intent is the same (ui-grid.md, rule 1, "whether or not the move
    // changed the cursor").
    assert_eq!(at("sel3"), (39, 6), "End, then Ctrl+Shift+B on G");
    assert_eq!(
        at("edge"),
        (39, 0),
        "a Right at the last frame moved nothing and kept the selection"
    );
    // --- the Y advance is a cursor move (AC5) ------------------------------
    assert_eq!(at("sel4"), (3, 5), "A selected again, cursor on frame 3");
    assert_eq!(
        at("y"),
        (4, 0),
        "the pick advance left the selection standing (it read 5 before)"
    );
    assert!(
        status("y").contains("· unmarked"),
        "the advance landed on 4, which is unmarked: {}",
        status("y")
    );
    let (marked_cursor, _) = at("marked");
    assert_eq!(marked_cursor, 3);
    assert!(
        status("marked").contains("★ picked"),
        "the mark did not land on frame 3: {}",
        status("marked")
    );
    // The mark landed on the CURSOR frame only, never on the five that
    // were selected when the key was pressed: 5 was one of them.
    let (next_cursor, _) = at("next");
    assert_eq!(next_cursor, 5);
    assert!(
        status("next").contains("· unmarked"),
        "the Y marked a frame that was merely SELECTED — marks are not \
         batch operations (ui-grid.md): {}",
        status("next")
    );
    // --- `U` moves nothing, so it takes nothing ---------------------------
    assert_eq!(at("sel5"), (5, 5), "A selected once more, cursor on 5");
    assert_eq!(
        at("u"),
        (5, 5),
        "`U` collapsed the selection — it does not advance, so it must not \
         (ui-grid.md rule 1, Manager 2026-09-06)"
    );
    // --- ...unless its mark takes the frame OUT of the view ---------------
    // The other half of rule 1's `U` clause, and the reason the harness
    // learned `filter:` (senior-developer review F1): under a Picked
    // filter, clearing a mark removes the frame from the view, the
    // live-removal rule moves the cursor on, and THAT is a cursor move
    // like any other. Unreachable without a filter — which is why the
    // clause had no driven proof until this strand.
    //
    // Note the order: `home` comes BEFORE `select-all`, because `home` is
    // itself a plain move and would otherwise empty the selection this
    // strand needs the `U` to take.
    assert_eq!(
        at("picks"),
        (3, 0),
        "three picks from the head, cursor on 3"
    );
    let filtered = qedump(&stderr, "filtered");
    assert!(
        dump_text(filtered, "status").contains("showing 4 of 40"),
        "the Picked filter did not narrow the view to the four picked \
         frames (three from this strand, plus the one the `Y` above left on \
         frame 3), so the `U` below takes nothing out of anything: \
         {filtered}"
    );
    assert_eq!(at("all"), (0, 4), "select-all over the picked view");
    let (removed_cursor, removed_sel) = at("removed");
    assert_eq!(
        removed_sel, 0,
        "the `U` took its frame out of the Picked view and the cursor moved \
         on, which is a cursor move — the selection must go with it \
         (ui-grid.md rule 1). Without the cursor-moved half of that rule \
         this reads 3."
    );
    assert_ne!(
        removed_cursor, 0,
        "the cleared frame is still under the cursor, so nothing moved and \
         this strand proves nothing:\n{stderr}"
    );
    // --- and a plain move collapses even where there is nowhere to move --
    // Rule 1 says the clear happens "whether or not the move changed the
    // cursor", and an EMPTY filtered view is the hardest case of that: no
    // frame to move to, so the move does nothing visible, and the
    // selection used to survive it and come back the moment the filter
    // widened again (QE 2026-09-06, M-2 — reachable with the mouse alone:
    // click a chip that matches nothing, press an arrow, click All).
    //
    // The Rejected view is the empty one here: this session has picks by
    // now, so Picked would not be empty (QE's strand used an unmarked
    // session; this test is no longer one).
    assert_eq!(
        dump_field(qedump(&stderr, "all40"), "selected"),
        "40",
        "select-all did not take the whole view back:\n{stderr}"
    );
    let empty_view = qedump(&stderr, "emptyview");
    assert!(
        dump_text(empty_view, "status").contains("(0/0)")
            && dump_text(empty_view, "status").contains("showing 0 of 40"),
        "the Rejected filter did not empty the view, so the arrow below \
         had somewhere to go: {empty_view}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "after"), "selected"),
        "0",
        "a plain arrow in an empty view left the selection standing, and \
         widening the filter brought all 40 back (it read 40 before the \
         fix):\n{stderr}"
    );
}

/// Rule 1 in the LOUPE, where no wash shows a selection and the status
/// count is its only sign — and rule 2 there too (Ctrl+Right keeps it).
/// The `zoom` field is read at every dump: the run never left the loupe,
/// so none of this is the grid's behaviour in disguise.
#[test]
fn a_plain_move_collapses_the_selection_in_the_loupe() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("collapse-loupe.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts", "--start-loupe"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "600:key:];800:key:shift+];1000:dump.sel;1200:key:right;1400:dump.arrow;\
                 1600:key:ctrl+shift+b;1800:dump.sel2;2000:key:];2200:dump.bracket;\
                 2400:key:ctrl+shift+b;2600:dump.sel3;2800:key:ctrl+right;3000:dump.keep;\
                 3200:key:y;3400:dump.y;3600:key:left;3800:dump.marked",
            ),
        ],
        &out,
    );
    let at = |label: &str| -> (usize, usize) {
        let d = qedump(&stderr, label);
        (
            dump_field(d, "cursor").parse().unwrap(),
            dump_field(d, "selected").parse().unwrap(),
        )
    };
    let status = |label: &str| dump_text(qedump(&stderr, label), "status").to_string();
    let labels = [
        "sel", "arrow", "sel2", "bracket", "sel3", "keep", "y", "marked",
    ];

    assert_eq!(
        at("sel"),
        (6, 6),
        "Shift+`]` in the loupe: A plus the single"
    );
    assert_eq!(
        at("arrow"),
        (7, 0),
        "a plain Right in the loupe left the selection standing (it read 6)"
    );
    assert_eq!(at("sel2"), (7, 3), "Ctrl+Shift+B on B");
    assert_eq!(
        at("bracket"),
        (10, 0),
        "a plain `]` in the loupe left the selection standing (it read 3)"
    );
    assert_eq!(at("sel3"), (10, 8), "Ctrl+Shift+B on C");
    assert_eq!(
        at("keep"),
        (11, 8),
        "Ctrl+Right must keep the selection in the loupe too"
    );
    assert_eq!(at("y"), (12, 0), "the pick advance kept the selection");
    assert!(status("y").contains("· unmarked"), "{}", status("y"));
    let (marked_cursor, _) = at("marked");
    assert_eq!(marked_cursor, 11);
    assert!(
        status("marked").contains("★ picked"),
        "the mark did not land on 11: {}",
        status("marked")
    );
    // The whole run happened at one zoom: nothing above is a grid gesture.
    let zoom = dump_field(qedump(&stderr, "sel"), "zoom").to_string();
    for label in labels {
        assert_eq!(
            dump_field(qedump(&stderr, label), "zoom"),
            zoom,
            "dump.{label} is at another zoom — the run left the loupe:\n{stderr}"
        );
    }
}

/// Rule 3 (brief 002, the user's answer 3): a Shift-span whose anchor arms
/// on THIS press is the whole selection — the frames a Ctrl+Space added
/// included — while a span that continues a live anchor still shrinks and
/// flips.
#[test]
fn a_fresh_span_after_ctrl_navigation_replaces_the_selection() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("fresh-span.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "600:key:home;800:key:shift+right;900:key:shift+right;1000:key:shift+right;\
                 1200:dump.four;\
                 1400:key:ctrl+right;1500:key:ctrl+right;1700:dump.walked;\
                 1900:key:shift+right;2100:dump.fresh;\
                 2300:key:ctrl+right;2500:key:ctrl+space;2700:dump.added;\
                 2900:key:ctrl+right;3000:key:ctrl+right;3200:key:shift+right;3400:dump.fresh2;\
                 3600:key:shift+left;3800:dump.shrink;4000:key:shift+left;4200:dump.flip;\
                 4400:key:ctrl+left;4500:key:ctrl+left;4600:key:ctrl+left;4700:key:ctrl+left;\
                 4900:key:shift+[;5100:dump.burstfresh",
            ),
        ],
        &out,
    );
    let at = |label: &str| -> (usize, usize) {
        let d = qedump(&stderr, label);
        (
            dump_field(d, "cursor").parse().unwrap(),
            dump_field(d, "selected").parse().unwrap(),
        )
    };
    assert_eq!(at("four"), (3, 4), "Shift+Right x3 from the head");
    assert_eq!(at("walked"), (5, 4), "Ctrl+Right x2 keeps the four");
    assert_eq!(
        at("fresh"),
        (6, 2),
        "the fresh span is 5..6 and nothing else — before the rule it read \
         6, the old four unioned with the new two"
    );
    assert_eq!(at("added"), (7, 3), "Ctrl+Space adds 7 to the span");
    assert_eq!(
        at("fresh2"),
        (10, 2),
        "the next fresh span replaces the Ctrl-added frame too (answer 3) \
         — before the rule it read 4"
    );
    // A span that CONTINUES its anchor still replaces only itself.
    assert_eq!(at("shrink"), (9, 1), "Shift+Left shrinks the live span");
    assert_eq!(at("flip"), (8, 2), "and flips past its anchor");
    assert_eq!(
        at("burstfresh"),
        (1, 5),
        "a fresh Shift+`[` from mid-A takes A alone — before the rule it \
         read 7, A plus the abandoned {{8, 9}}"
    );
}

/// AC1 and AC6, end to end: the user's own report of 2026-09-06, as a
/// test. Four frames exported, Esc on the report, two arrows, a new
/// Shift-span — and the second video holds the NEW two frames only.
///
/// Before the rule this run produced `a-g.mov` with six frames and the
/// dialog said "4 of 6 frames are already in a-d.mov" (the earlier-export
/// hint of issue #56, which is what told the user something was wrong
/// after the fact).
///
/// Eight tiny synthetic RAWs rather than real camera files: two exports
/// have to fit inside one driven run, and `write_synthetic_raw` gives each
/// frame a distinct length so a wrong frame set is visible in the file's
/// size as well as in its sample count.
#[test]
fn the_second_video_holds_only_the_new_span() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let src = out_dir().join("collapse-clip-src");
    let dest = out_dir().join("collapse-clip-dest");
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
        std::fs::create_dir_all(d).unwrap();
    }
    for (i, name) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        write_synthetic_raw(&src.join(format!("{name}.ARW")), 400, 300, 1, 4096 + i * 64);
    }
    // ADR 0003 guard: this run marks nothing, so the whole source listing
    // — names and lengths — must come back identical.
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

    let script = format!(
        "1500:clipdest:{dest};1700:wait:load settled gen 0;1800:home;\
         2000:key:shift+right;2100:key:shift+right;2200:key:shift+right;2400:dump.four;\
         2600:key:ctrl+shift+e;2900:dump.plan1;3100:key:return;\
         3200:wait:clip export finished run 1;4400:dump.done1;\
         4700:key:escape;5000:dump.closed1;5200:key:right;5300:key:right;5500:dump.moved;\
         5700:key:shift+right;5900:dump.span;6100:key:ctrl+shift+e;6400:dump.plan2;\
         6600:key:return;6700:wait:clip export finished run 2;7900:dump.done2;\
         8200:key:escape;8500:dump.closed2;8700:key:escape;9000:dump.cleared",
        dest = dest.display()
    );
    let out = out_dir().join("collapse-clip.jpg");
    let stderr = shoot_env_stderr(
        &[src.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let after = listing(&src);
    let mut landed: Vec<String> = std::fs::read_dir(&dest)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    landed.sort();
    let first = dest
        .join("a-d.mov")
        .is_file()
        .then(|| read_movie_at(&dest.join("a-d.mov")));
    let second = dest
        .join("f-g.mov")
        .is_file()
        .then(|| read_movie_at(&dest.join("f-g.mov")));
    for d in [&src, &dest] {
        std::fs::remove_dir_all(d).ok();
    }

    assert_eq!(before, after, "the export wrote to a source RAW (ADR 0003)");
    for run in [1, 2] {
        assert!(
            stderr.contains(&format!("wait:clip export finished run {run} (satisfied")),
            "the `wait:` for export {run} never fired — the dumps were \
             timed, not gated:\n{stderr}"
        );
    }
    let sel = |label: &str| dump_field(qedump(&stderr, label), "selected").to_string();
    let status = |label: &str| dump_text(qedump(&stderr, label), "status").to_string();

    // --- the first export, four frames ------------------------------------
    assert_eq!(sel("four"), "4");
    assert!(
        status("four").starts_with("d.ARW (4/8)"),
        "the span is a..d: {}",
        status("four")
    );
    let plan1 = qedump(&stderr, "plan1");
    assert_eq!(dump_field(plan1, "clipstate"), "0", "{plan1}");
    assert!(
        dump_text(plan1, "clipsummary").starts_with("4 frames")
            && dump_text(plan1, "clipsummary").contains("a-d.mov"),
        "{plan1}"
    );
    assert!(
        dump_text(qedump(&stderr, "done1"), "clipreport").contains("a-d.mov"),
        "{}",
        qedump(&stderr, "done1")
    );
    // AC6, first half: Esc closes the dialog and the selection is INTACT.
    let closed1 = qedump(&stderr, "closed1");
    assert_eq!(dump_field(closed1, "clip"), "false", "{closed1}");
    assert_eq!(
        dump_field(closed1, "selected"),
        "4",
        "a dialog's Esc took the selection with it — it must only close \
         the dialog (video-export.md, brief 002): {closed1}"
    );

    // --- two arrows, and the old selection is gone (AC1) -------------------
    assert_eq!(
        sel("moved"),
        "0",
        "the two arrows left the first export's four frames selected — the \
         user's report (it read 4 before the rule)"
    );
    assert!(
        status("moved").starts_with("f.ARW (6/8)"),
        "{}",
        status("moved")
    );
    assert_eq!(
        sel("span"),
        "2",
        "the new span carried the old four along (it read 6 before the rule)"
    );
    let plan2 = qedump(&stderr, "plan2");
    assert_eq!(dump_field(plan2, "clipstate"), "0", "{plan2}");
    assert!(
        dump_text(plan2, "clipsummary").starts_with("2 frames")
            && dump_text(plan2, "clipsummary").contains("f-g.mov"),
        "the second plan is not the new span alone (it read \"6 frames … \
         a-g.mov\" before the rule): {plan2}"
    );
    assert_eq!(
        dump_text(plan2, "cliphint"),
        "",
        "the second export overlaps the first (the hint read \"4 of 6 \
         frames are already in a-d.mov\" before the rule): {plan2}"
    );
    assert!(
        dump_text(qedump(&stderr, "done2"), "clipreport").contains("f-g.mov"),
        "{}",
        qedump(&stderr, "done2")
    );
    let closed2 = qedump(&stderr, "closed2");
    assert_eq!(dump_field(closed2, "clip"), "false", "{closed2}");
    assert_eq!(dump_field(closed2, "selected"), "2", "{closed2}");
    // AC6, second half: the Esc after it, on the grid, clears.
    assert_eq!(
        sel("cleared"),
        "0",
        "the second Esc did not clear the selection"
    );
    // Neither export ever met the clash question: two different names.
    for label in [
        "four", "plan1", "done1", "closed1", "moved", "span", "plan2", "done2", "closed2",
        "cleared",
    ] {
        assert_ne!(
            dump_field(qedump(&stderr, label), "clipstate"),
            "3",
            "dump.{label}: the export asked to replace a file, so the two \
             runs collided on one name:\n{stderr}"
        );
    }

    // --- and the files on disk say the same thing --------------------------
    assert_eq!(
        landed,
        vec!["a-d.mov".to_string(), "f-g.mov".to_string()],
        "the destination holds the wrong files"
    );
    assert_eq!(
        first.expect("a-d.mov").samples.len(),
        4,
        "the first video is not four frames"
    );
    assert_eq!(
        second.expect("f-g.mov").samples.len(),
        2,
        "the second video holds more than the two frames that were selected"
    );
}

/// AC2: caption a burst, hop to the next with `]`, caption again — and the
/// second commit lands on the second burst only. The revert slot counts
/// the batch it wrote, which is what makes "5, then 3, never 8" readable.
///
/// This is the IPTC half of the user's report: before the rule the `]`
/// kept the first burst selected and the second commit stamped both.
#[test]
fn caption_then_hop_then_caption_lands_on_the_second_burst_only() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("caption-hop.jpg");
    let script = format!(
        "{PIN_WINDOW};1200:key:];1400:key:ctrl+shift+b;1600:dump.sel;\
         1800:key:i;1900:wait:iptc field 0 laid out at 1150;\
         2300:click:iptc field 0;2500:key:t;2700:key:return;3000:dump.first;\
         3200:key:];3400:dump.hop;3600:key:];3800:key:ctrl+shift+b;4000:dump.sel2;\
         4200:click:iptc field 0;4400:key:u;4600:key:return;4900:dump.second"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    assert!(
        stderr.contains("wait:iptc field 0 laid out at 1150 (satisfied"),
        "the `wait:` never fired — the clicks were timed, not gated:\n{stderr}"
    );
    assert_click_resolved(&stderr, "iptc field 0");
    let at = |label: &str| -> (usize, usize) {
        let d = qedump(&stderr, label);
        (
            dump_field(d, "cursor").parse().unwrap(),
            dump_field(d, "selected").parse().unwrap(),
        )
    };
    assert_eq!(at("sel"), (1, 5), "Ctrl+Shift+B on A");
    let first = qedump(&stderr, "first");
    assert!(
        dump_text(first, "revert").contains("on 5 image(s)"),
        "the first commit did not stamp the five-frame burst — the field \
         click missed, or nothing committed: {first}"
    );
    assert_eq!(
        dump_field(first, "focusowner"),
        "0",
        "Enter did not return the keyboard to the grid, so the `]` below \
         would be typed into the field: {first}"
    );
    assert_eq!(dump_field(first, "selected"), "5");
    // The hop. This is the rule: the caption is on the sidecars, the
    // selection has no job left, and the `]` ends it.
    assert_eq!(
        at("hop"),
        (6, 0),
        "the `]` after a commit left the burst selected (it read 5 before \
         the rule)"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "hop"), "focusowner"),
        "0",
        "the `]` never reached the grid:\n{stderr}"
    );
    assert_eq!(at("sel2"), (7, 3), "the second burst, alone (it read 8)");
    let second = qedump(&stderr, "second");
    assert!(
        dump_text(second, "revert").contains("on 3 image(s)")
            && !dump_text(second, "revert").contains("8 image(s)"),
        "the second commit did not land on the second burst alone — it \
         read \"on 8 image(s)\" before the rule: {second}"
    );
}

/// AC7 (brief 002 R5, persona A1): "· N selected" is painted in the
/// selection's blue, in the grid and in the loupe, and an empty selection
/// says nothing at all.
///
/// Measured, never assumed: the fragment reports its own rectangle
/// (`status selected laid out at X,Y size WxH`) and so does the grey half
/// before it, and this reads the PIXELS inside each in the same shot. That
/// is what makes it a colour assertion rather than a font one — no
/// coordinate is written here, and a face that moves the fragment 40 px
/// along the bar moves the rectangle with it.
///
/// Blue bias is mean (B − R) over the rectangle, the same measure the wash
/// tests use. Both rectangles are mostly the bar's own #202024, whose own
/// bias is +4, with glyphs on top, so the numbers are small in absolute
/// terms and only the DIFFERENCE means anything. Measured on this seat at
/// 1440x900 over JPEG q92, identical in both runs: fragment 18.0, grey
/// head 4.1 — a difference of 13.9. The two mutants this must catch,
/// measured the same way: the fragment painted in the grey #a8a8b0 gives
/// 4.1 (difference 0.0) and the fragment painted in the 25 % wash blend
/// instead of the hue at full opacity gives 7.4 (difference 3.3).
///
/// The threshold is 8.0: 2.4x the strongest mutant and 0.58x the real
/// signal. Font sensitivity, since CI draws in DejaVu Sans and Segoe UI
/// and this seat in Noto Sans: the bias is coverage x 178 (the accent's
/// own B − R) plus background, so 18.0 means the glyphs ink about 8 % of
/// the rectangle, and 8.0 would need that to fall under 2.2 % — a face
/// three and a half times lighter than this one. Do not raise it to pass
/// on a seat; a red here is a paint that changed.
#[test]
fn the_selection_count_is_drawn_in_the_accent() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    const T: f64 = 8.0;
    for (name, args) in [
        ("grid", vec!["--synthetic", "40", "--bursts"]),
        (
            "loupe",
            vec!["--synthetic", "40", "--bursts", "--start-loupe"],
        ),
    ] {
        let out = out_dir().join(format!("status-accent-{name}.jpg"));
        let stderr = shoot_env_stderr(
            &args,
            &[
                ("FASTCULL_TRACE", "1"),
                (
                    "FASTCULL_DRIVE",
                    &format!(
                        "{PIN_WINDOW};1300:wait:window geometry 1440x900;\
                         1500:key:];1700:key:shift+];1900:dump.sel"
                    ),
                ),
            ],
            &out,
        );
        assert!(
            stderr.contains("wait:window geometry 1440x900 (satisfied"),
            "{name}: the window never reached 1440x900, so the fractions \
             below address the wrong pixels:\n{stderr}"
        );
        let sel = qedump(&stderr, "sel");
        // Anti-vacuity: there IS a count to paint.
        assert_eq!(
            dump_field(sel, "selected"),
            "6",
            "{name}: nothing is selected, so the fragment is empty and the \
             pixels below are the bar's: {sel}"
        );
        assert!(
            dump_text(sel, "status").contains("· 6 selected"),
            "{name}: {sel}"
        );
        if name == "loupe" {
            assert_eq!(
                dump_field(sel, "one2one"),
                "false",
                "{name}: the loupe run is not at fit: {sel}"
            );
        }
        // The two rectangles, as the app reported them at the shutter.
        let (sx, sy, sw, sh) = laid_out_rect(&stderr, "status selected", "status at shutter: ");
        let (hx, hy, hw, hh) = laid_out_rect(&stderr, "status head", "status at shutter: ");
        assert!(
            sw > 0.0 && sh > 0.0,
            "{name}: the count fragment has no rectangle ({sw}x{sh}) — it \
             was never laid out:\n{stderr}"
        );
        assert!(hw > 0.0 && hh > 0.0, "{name}: no head rectangle:\n{stderr}");
        // Logical px over the pinned window is the frame fraction at any
        // scale factor, which is what keeps this readable on a HiDPI seat.
        let bias = |(x, y, w, h): (f32, f32, f32, f32)| {
            region_blue_bias(
                &out,
                x as f64 / 1440.0,
                y as f64 / 900.0,
                (x + w) as f64 / 1440.0,
                (y + h) as f64 / 900.0,
            )
        };
        let sel_bias = bias((sx, sy, sw, sh));
        let head_bias = bias((hx, hy, hw, hh));
        eprintln!("{name}: fragment blue bias {sel_bias:.1}, head {head_bias:.1}");
        assert!(
            head_bias < 6.0,
            "{name}: the grey half of the status line reads {head_bias:.1} \
             of blue bias — the control is not grey, so the comparison \
             below means nothing"
        );
        assert!(
            sel_bias - head_bias > T,
            "{name}: the selection count is not drawn in the accent — blue \
             bias {head_bias:.1} (grey head) vs {sel_bias:.1} (the \
             fragment), a difference of {:.1} against the {T} this \
             requires:\n{stderr}",
            sel_bias - head_bias
        );
    }
}

/// Shift+PgUp, Shift+PgDn, Shift+Home and Shift+End EXTEND the selection,
/// exactly as Shift+arrows do (Manager ruling 2026-09-06 on QE's M-1: the
/// file-manager convention this whole unit is built on).
///
/// They were unbound when the collapse rule landed, and an unbound Shift
/// chord fell through to its PLAIN form — so for one commit these four
/// keys moved the cursor and threw the selection away, which is worse than
/// the nothing they did before. A Shift-modified key is never silently the
/// plain key.
///
/// The spans are asserted as arithmetic, never as a page size: a synthetic
/// session's ids are its view positions, so "the span runs from the anchor
/// to wherever the key landed" is `selected == |cursor − anchor| + 1` for
/// any window, any row width and any page height.
#[test]
fn shift_page_keys_extend_the_selection() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("shift-page.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                &format!(
                    "{PIN_WINDOW};1300:wait:window geometry 1440x900;\
                     1500:key:home;1700:key:right;1800:key:right;1900:key:right;2100:dump.start;\
                     2300:key:shift+end;2500:dump.send;2700:key:shift+home;2900:dump.shome;\
                     3100:key:escape;3300:key:home;3500:key:down;3700:dump.mid;\
                     3900:key:shift+pgdn;4100:dump.spgdn;4300:key:shift+pgup;4500:dump.spgup;\
                     4700:key:escape;4900:key:home;5100:key:ctrl+a;5300:dump.all;\
                     5500:key:shift+space;5700:dump.sspace"
                ),
            ),
        ],
        &out,
    );
    assert!(
        stderr.contains("wait:window geometry 1440x900 (satisfied"),
        "the window never reached 1440x900:\n{stderr}"
    );
    let at = |label: &str| -> (usize, usize) {
        let d = qedump(&stderr, label);
        (
            dump_field(d, "cursor").parse().unwrap(),
            dump_field(d, "selected").parse().unwrap(),
        )
    };
    // --- Shift+End / Shift+Home, where the landing is not in doubt -------
    assert_eq!(at("start"), (3, 0), "three plain rights from the head");
    assert_eq!(
        at("send"),
        (39, 37),
        "Shift+End must EXTEND from the anchor at 3 to the last frame — \
         before the fix it read (39, 0), the plain End's collapse"
    );
    assert_eq!(
        at("shome"),
        (0, 4),
        "Shift+Home continues the live anchor at 3, so the span flips to \
         0..=3 — before the fix it read (0, 0)"
    );
    // --- the page keys, whose landing depends on the window --------------
    assert_eq!(at("mid"), (8, 0), "one row down from the head at 8 columns");
    let (down, down_sel) = at("spgdn");
    assert!(
        down > 8,
        "Shift+PgDn did not move the cursor (8 -> {down}), so the span \
         below proves nothing:\n{stderr}"
    );
    assert_eq!(
        down_sel,
        down - 8 + 1,
        "Shift+PgDn must span from the anchor at 8 to where the page \
         landed ({down}) — before the fix it read 0"
    );
    let (up, up_sel) = at("spgup");
    assert!(
        up < down,
        "Shift+PgUp did not move the cursor back ({down} -> {up}):\n{stderr}"
    );
    assert_eq!(
        up_sel,
        8_usize.abs_diff(up) + 1,
        "Shift+PgUp must re-span from the SAME anchor at 8 (a continued \
         span, not a fresh one) to {up} — before the fix it read 0"
    );
    // --- and Shift+Space is inert, not a pick ----------------------------
    // The same fall-through, with a harmless-looking meaning: nothing in
    // the spec gives Shift+Space a job, so it must do nothing rather than
    // quietly mark a frame and advance (which also collapsed the
    // selection, 40 -> 0).
    assert_eq!(at("all"), (0, 40), "Ctrl+A over the whole view");
    assert_eq!(
        at("sspace"),
        (0, 40),
        "Shift+Space moved the cursor or ate the selection — it is inert \
         (before the fix it picked frame 0 and advanced, reading (1, 0))"
    );
    assert!(
        dump_text(qedump(&stderr, "sspace"), "status").contains("★0"),
        "Shift+Space marked a frame: {}",
        qedump(&stderr, "sspace")
    );
}

/// Ctrl-navigation CLAIMS THE CURSOR (`ui-grid.md`'s key table: "claims
/// the cursor"), so the view rules stop moving it afterwards.
///
/// `filter::cursor_after_recompute` snaps an UNTOUCHED cursor to the new
/// view's head whenever the user asks for a different view (issue #4: a
/// folder never opens with the cursor stranded mid-grid) or while the
/// metadata is still streaming; a cursor the user has moved keeps its
/// image instead. A Ctrl-move is a deliberate act on the cursor, so it
/// must switch that off — and nothing in the suite noticed when the eleven
/// Ctrl/toggle tokens were deleted from the claim (QE 2026-09-06, M-5).
///
/// The discriminator here is a FILTER CHANGE, not the load-settled
/// re-sort the sibling test uses: `user_changed_query || !metadata_complete`
/// is one condition, and only the first half can be driven without a race.
/// Measured on this seat, three real RAWs settle in under 600 ms — earlier
/// than any key a script can send — so the re-sort form of this test was
/// vacuous (its own ordering guard said so, which is why it is not the
/// form that shipped).
///
/// `Ctrl+Space` sits in the same `matches!` and is not pinned separately:
/// it never moves the cursor, so its claim cannot be observed on its own —
/// any gesture that would make it visible has already claimed the cursor
/// itself.
#[test]
fn ctrl_navigation_claims_the_cursor() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("ctrl-claim.jpg");
    let stderr = shoot_env_stderr(
        &["--synthetic", "40", "--bursts"],
        &[
            ("FASTCULL_TRACE", "1"),
            (
                "FASTCULL_DRIVE",
                "600:key:ctrl+right;700:key:ctrl+right;800:key:ctrl+right;1000:dump.walked;\
                 1200:filter:unmarked;1400:dump.filtered",
            ),
        ],
        &out,
    );
    let walked = qedump(&stderr, "walked");
    assert_eq!(
        dump_field(walked, "cursor"),
        "3",
        "three Ctrl+Rights did not reach frame 3, so nothing below is \
         about a cursor the user moved: {walked}"
    );
    let filtered = qedump(&stderr, "filtered");
    // Anti-vacuity: the filter really changed, so the cursor rule really
    // re-ran with `user_changed_query` set. Every synthetic frame is
    // unmarked, so the Unmarked view holds all forty — the membership is
    // the same, the QUESTION asked of the cursor is not.
    assert!(
        dump_text(filtered, "status").contains("showing 40 of 40"),
        "the Unmarked filter never engaged, so no cursor rule was \
         re-applied: {filtered}"
    );
    assert_eq!(
        dump_field(filtered, "cursor"),
        "3",
        "the filter change snapped the cursor back to the head of the \
         view, off the frame the user had walked to with Ctrl+Right — a \
         Ctrl-move claims the cursor (ui-grid.md). Without that claim this \
         reads 0:\n{stderr}"
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
/// the shortcuts popup (549) y 182..731, About (348) y 283..631, and the
/// Settings dialog (506 on every tab — one height per open, its tallest
/// tab's, brief 009; measured 2026-10-03) y 204..710.
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
/// dialog is up (`copy` / `clip` / `settings`).
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

/// The centre of the Settings dialog's wash field on its UI tab at the
/// pinned 1440x900: `settings wash laid out at 630,313 size 80x32`,
/// measured on this seat (Noto Sans, 2026-10-03; it was 630,433 until brief
/// 009 gave the card one height per open, its tallest tab's, which put the
/// card's top 120 px higher on every tab). The x is arithmetic — the
/// 560 px card centred in 1440, then the padding and the 160 px label
/// column — but the y sits under the title and the tab strip, whose heights
/// are font metrics: the strand that uses this runs only where
/// `menu_clicks_are_calibrated()`, like `RENAME_FIELD_Y`'s, and checks
/// before it wheels that the point really is on the field. The wheel has no
/// by-name token; this is the only coordinate the test needs, and the
/// click at it is that check, not a way to reach a named control.
const SETTINGS_WASH_FIELD: (u32, u32) = (670, 329);

/// The Settings dialog, the fifth scrim (ui-grid.md: "ALL FIVE scrims
/// swallow the wheel"; issue #49): a wheel over the card's centre, over bare
/// scrim and — on the calibrated runners — over the wash field, a child
/// that owns a `TextInput`, never scrolls the grid behind the dialog.
/// `--synthetic 300` for the copy test's reason: the contract is about the
/// scrim, and 300 cells leave the grid room to scroll. At 1440x900 the card
/// is centred in the area under the menu bar (`settings card laid out at
/// 440,204 size 560x506` on every tab since brief 009, the notice line
/// reserved — measured here), so (700,400) is on the card and (100,400) is
/// bare scrim whatever the face.
///
/// The child strand is the copy test's rename-field shape, gated for its
/// reason (the field's y is a font metric): UI tab, a click at the field's
/// measured centre, a `9` typed — the field's own `settings wash shows`
/// mark must carry it, or the point missed the field and the strand stops
/// loudly instead of wheeling over the wrong element — then the wheel at
/// the same point. The closing Esc discards the half-typed `9`.
///
/// Mutant (2026-10-01): the `scroll-event` arm taken out of the Settings
/// scrim's `TouchArea` → the wheel over the card scrolls the grid,
/// `dump.wheeled` reads `vpy=-360.0` — red (QE 2026-10-01, D27: no test
/// wheeled this scrim, and QE measured the grid at -1800 after three wheels
/// under that mutant with every settings test green).
#[test]
fn a_wheel_over_the_settings_dialog_never_scrolls_the_grid_behind_it() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("i49-settings-wheel.jpg");
    let over_the_field = menu_clicks_are_calibrated();
    let (fx, fy) = SETTINGS_WASH_FIELD;
    let child_steps = if over_the_field {
        format!(
            "4900:key:right;5200:click.{fx},{fy};5400:key:9;\
             5700:wheel.{fx},{fy},-180;6400:dump.overfield;"
        )
    } else {
        String::new()
    };
    let script = format!(
        "{PIN_WINDOW};1600:{w};2100:dump.prewheel;\
         2400:key:ctrl+,;2800:dump.open;\
         3000:{w};3700:dump.wheeled;\
         4000:wheel.100,400,-180;4700:dump.scrim;\
         {child_steps}\
         6900:key:escape;7200:dump.closed;\
         7500:{w};8200:dump.control",
        w = THREE_NOTCHES_DOWN
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    // Over the card first: the primary contract, so a scrim that leaks
    // fails HERE rather than in the child case below.
    assert_wheel_over_the_dialog_is_swallowed(&stderr, "settings");
    assert_eq!(
        dump_field(qedump(&stderr, "scrim"), "vpy"),
        "-180.0",
        "a wheel over the bare scrim beside the Settings card scrolled the grid \
         behind it (issue #49):\n{stderr}"
    );
    if !over_the_field {
        eprintln!("over-the-field strand skipped: uncalibrated card geometry");
        return;
    }
    // The calibration guard: the click at the measured point focused the
    // wash field and the `9` landed in it, before the wheel at that point.
    let labels = mark_labels(&stderr);
    let wheel = format!("drive: wheel.{fx},{fy},-180");
    let wheeled_at = labels
        .iter()
        .position(|l| *l == wheel)
        .unwrap_or_else(|| panic!("the wheel over the field never ran:\n{stderr}"));
    let shown = labels[..wheeled_at]
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("settings wash shows "));
    assert!(
        shown.is_some_and(|text| text.contains('9')),
        "the click at {SETTINGS_WASH_FIELD:?} missed the wash field (it shows {shown:?}), \
         so the wheel below would be over the wrong element:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "overfield"), "vpy"),
        "-180.0",
        "a wheel over the Settings dialog's wash field scrolled the grid behind it \
         (issue #49):\n{stderr}"
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

// ---------------------------------------------------------------------------
// The Settings dialog (settings.md, brief 008, issue #39). Every run below is
// FASTCULL_NO_CONFIG like every other (the harness sets it); the ones that
// must read or write a settings file point FASTCULL_CONFIG_DIR at their own
// scratch dir under the shot dir (test-harness.md), so no run ever touches
// the real ~/.config/fastcull.
// ---------------------------------------------------------------------------

/// A fresh config dir for one test, holding `settings.toml` with `text`
/// when there is any.
fn settings_scratch(tag: &str, text: Option<&str>) -> PathBuf {
    let dir = out_dir().join(format!("settings-{tag}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(text) = text {
        std::fs::write(dir.join("settings.toml"), text).unwrap();
    }
    dir
}

/// AC1 (settings.md): `Ctrl+,` and File › Settings… open the dialog; `Esc`
/// and Close close it; a click on the scrim does NOT; the keyboard is back
/// on the grid afterwards — asserted by ACTING (`+` zooms), never by
/// `keysfocus`.
///
/// The menu strand is Linux-only, like About's (`menu_clicks_are_
/// calibrated`): File is at x 22 in the in-window bar and Settings… is its
/// fourth item, y = 61 + 3 × 32.
///
/// Mutant (2026-10-01): the `Ctrl+,` arm deleted from the main key scope →
/// the dialog never opens, so `click:settings close` finds no layout mark
/// and the run aborts loudly (exit 1) — red.
#[test]
fn settings_opens_from_the_chord_and_the_menu_and_closes_with_esc_keeping_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-open-close.jpg");
    let menu = if menu_clicks_are_calibrated() {
        "6200:click.22,19;6600:click.80,157;7000:dump.menu;7300:key:escape;7700:dump.menuclosed"
    } else {
        "7000:dump.menu"
    };
    let script = format!(
        "{PIN_WINDOW};900:key:ctrl+,;1300:dump.opened;1600:click.20,300;2000:dump.scrim;\
         2300:key:escape;2700:dump.closed;3000:key:+;3300:dump.zoomed;\
         3700:key:ctrl+,;4300:click:settings close;4700:dump.closebutton;\
         5000:key:-;5300:dump.minus;{menu}"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let opened = qedump(&stderr, "opened");
    assert_eq!(
        dump_field(opened, "settings"),
        "true",
        "Ctrl+, did not open the Settings dialog:\n{stderr}"
    );
    assert_eq!(
        dump_field(opened, "focusowner"),
        "-1",
        "the dialog is up but does not own the keyboard (the `-1` token):\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "scrim"), "settings"),
        "true",
        "a click on the scrim closed the dialog — it is a form, like Copy \
         Picks, and closes on Esc or Close only:\n{stderr}"
    );
    let closed = qedump(&stderr, "closed");
    assert_eq!(
        dump_field(closed, "settings"),
        "false",
        "Esc did not close it:\n{stderr}"
    );
    assert_eq!(
        dump_field(closed, "focusowner"),
        "0",
        "Esc closed the dialog but the keyboard did not come back to the grid:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "zoomed"), "zoom"),
        "2",
        "the `+` after the dialog closed was dead — the keyboard is stranded:\n{stderr}"
    );
    assert_click_resolved(&stderr, "settings close");
    let closebutton = qedump(&stderr, "closebutton");
    assert_eq!(
        dump_field(closebutton, "settings"),
        "false",
        "the Close button did not close the dialog:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "minus"), "zoom"),
        "1",
        "the `-` after Close was dead — the keyboard did not come back:\n{stderr}"
    );
    if menu_clicks_are_calibrated() {
        assert_eq!(
            dump_field(qedump(&stderr, "menu"), "settings"),
            "true",
            "File › Settings… did not open the dialog (the menu click missed?):\n{stderr}"
        );
        assert_eq!(
            dump_field(qedump(&stderr, "menuclosed"), "settings"),
            "false",
            "Esc did not close the dialog the menu opened:\n{stderr}"
        );
    }
}

/// AC2 (settings.md, "Stacking"): under the dialog every grid key dies —
/// `Y`/`N` mark nothing, `Ctrl+E` and `Ctrl+Shift+E` open nothing, a driven
/// nav token is swallowed — About over it closes topmost-first, and the
/// menu bar stays live (About is opened from the Help menu on the
/// calibrated runners, by its token elsewhere). The control at the end
/// proves the `N` was contained rather than dead.
///
/// Mutant (2026-10-01): the harness's nav mirror without its
/// `get_settings_visible()` term → the driven `reject` reaches the grid,
/// `dump.contained` reads ✕1 and this goes red.
#[test]
fn settings_contains_every_grid_key_and_stacks_under_about() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-contained.jpg");
    let about = if menu_clicks_are_calibrated() {
        "3200:click.115,19;3600:click.180,93"
    } else {
        "3600:about"
    };
    let script = format!(
        "900:key:ctrl+,;1300:dump.opened;1600:key:y;1800:key:n;2000:key:ctrl+e;\
         2300:key:ctrl+shift+e;2600:reject;2900:dump.contained;{about};\
         4000:dump.about;4200:key:n;4400:key:escape;4800:dump.esc1;5000:key:escape;\
         5400:dump.esc2;5700:key:n;6100:dump.control"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    assert_eq!(
        dump_field(qedump(&stderr, "opened"), "settings"),
        "true",
        "the dialog never opened:\n{stderr}"
    );
    let contained = qedump(&stderr, "contained");
    assert!(
        dump_text(contained, "status").contains("★0 ✕0"),
        "a mark leaked through the Settings dialog: {contained}"
    );
    assert_eq!(
        dump_field(contained, "cursor"),
        "0",
        "a key moved the cursor: {contained}"
    );
    assert_eq!(
        dump_field(contained, "copy"),
        "false",
        "Ctrl+E opened Copy Picks: {contained}"
    );
    assert_eq!(
        dump_field(contained, "clip"),
        "false",
        "Ctrl+Shift+E opened the export dialog: {contained}"
    );
    assert_eq!(
        dump_field(contained, "settings"),
        "true",
        "a key closed the dialog: {contained}"
    );
    assert!(
        stderr.contains("drive swallowed by modal: reject"),
        "the driven nav token was not swallowed by the dialog:\n{stderr}"
    );
    let about = qedump(&stderr, "about");
    assert!(
        about.contains("about=true") && about.contains("settings=true"),
        "About did not open over the Settings dialog (the menu bar is not \
         live under it?): {about}"
    );
    let esc1 = qedump(&stderr, "esc1");
    assert!(
        esc1.contains("about=false") && esc1.contains("settings=true"),
        "the first Esc did not close About alone — topmost first (issue #42): {esc1}"
    );
    assert!(
        dump_text(esc1, "status").contains("★0 ✕0"),
        "the N pressed under About over Settings marked a photo: {esc1}"
    );
    let esc2 = qedump(&stderr, "esc2");
    assert!(
        esc2.contains("settings=false") && dump_field(esc2, "focusowner") == "0",
        "the second Esc did not close the dialog and return the keyboard: {esc2}"
    );
    assert!(
        dump_text(qedump(&stderr, "control"), "status").contains("★0 ✕1"),
        "the N after the dialogs closed did not reject — the containment \
         above is vacuous:\n{stderr}"
    );
}

/// AC2's other half (settings.md, "Stacking"): the keyboard shortcuts card
/// over the dialog closes topmost-first. Under the card a key neither
/// marks a photo nor reaches the dialog's strip; Esc closes the CARD and
/// leaves the dialog up; `?` from the dialog's scope closes the card too;
/// with the card gone the strip answers again (a Left switches tabs); Esc
/// then closes the dialog and the keyboard is back on the grid, where the
/// control `N` rejects.
///
/// The card is opened from the real Help menu on the calibrated runners
/// (Linux: Help at x 115 in the bar, Keyboard Shortcuts its first item at
/// y 61), by its `shortcuts` token elsewhere — About's two-path shape in
/// `settings_contains_every_grid_key_and_stacks_under_about`. A real `?`
/// cannot be the opener: the dialog swallows it, so the menu is the only
/// real path.
///
/// Mutant (2026-10-01): `|| root.shortcuts-visible` taken out of the
/// dialog scope's `capture-key-pressed` → under the card the keyboard sits
/// in the dialog's scope, the Esc bubbles to its `key-pressed` Esc arm and
/// closes SETTINGS under the card, `dump.esc1` reads `settings=false
/// shortcuts=true` — red (QE 2026-10-01, D27: until this test that mutant
/// stayed green).
#[test]
fn settings_stacks_under_the_shortcuts_card_and_closes_topmost_first() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-under-shortcuts.jpg");
    let card = |at: u32| -> String {
        if menu_clicks_are_calibrated() {
            format!("{}:click.115,19;{at}:click.180,61", at - 400)
        } else {
            format!("{at}:shortcuts")
        }
    };
    let script = format!(
        "900:key:ctrl+,;1200:key:right;{first};2400:dump.sc;2700:key:n;3000:key:left;\
         3300:dump.under;3600:key:escape;3900:dump.esc1;{second};4800:dump.sc2;5000:key:?;\
         5300:dump.q;\
         5600:key:left;5900:dump.alive;6200:key:escape;6500:dump.closed;6800:key:n;\
         7100:dump.control",
        first = card(2000),
        second = card(4600),
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    let sc = qedump(&stderr, "sc");
    assert!(
        dump_field(sc, "shortcuts") == "true"
            && dump_field(sc, "settings") == "true"
            && dump_field(sc, "settingstab") == "1",
        "the shortcuts card did not open over the Settings dialog on its UI tab \
         (the menu bar is not live under it?): {sc}"
    );
    let under = qedump(&stderr, "under");
    assert!(
        dump_text(under, "status").contains("★0 ✕0"),
        "an N under the shortcuts card over Settings marked a photo: {under}"
    );
    assert_eq!(
        dump_field(under, "settingstab"),
        "1",
        "a Left under the shortcuts card reached the dialog's strip behind it:\n{stderr}"
    );
    // The contract: topmost first.
    let esc1 = qedump(&stderr, "esc1");
    assert!(
        dump_field(esc1, "shortcuts") == "false" && dump_field(esc1, "settings") == "true",
        "the first Esc did not close the shortcuts card alone — topmost first \
         (issue #42): {esc1}"
    );
    // The premise of the `?` below: the card really is up again, or the
    // `?` would close nothing and the assertion after it would be vacuous.
    let sc2 = qedump(&stderr, "sc2");
    assert!(
        dump_field(sc2, "shortcuts") == "true" && dump_field(sc2, "settings") == "true",
        "the shortcuts card did not open over the dialog a second time, so the `?` \
         below would prove nothing: {sc2}"
    );
    let q = qedump(&stderr, "q");
    assert!(
        dump_field(q, "shortcuts") == "false" && dump_field(q, "settings") == "true",
        "`?` from the Settings dialog did not close the shortcuts card over it, or \
         closed the dialog too: {q}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "alive"), "settingstab"),
        "0",
        "with the card gone a Left did not switch tabs — the keyboard did not come \
         back to the dialog:\n{stderr}"
    );
    let closed = qedump(&stderr, "closed");
    assert!(
        dump_field(closed, "settings") == "false" && dump_field(closed, "focusowner") == "0",
        "Esc did not close the dialog and give the keyboard back to the grid: {closed}"
    );
    assert!(
        dump_text(qedump(&stderr, "control"), "status").contains("✕1"),
        "the N after the dialogs closed did not reject — the containment above is \
         vacuous:\n{stderr}"
    );
}

/// AC3 (settings.md, "Keyboard"): the strip switches tabs on Left/Right
/// (wrapping) and everywhere on Ctrl+Tab/Ctrl+Shift+Tab; a digit never
/// switches; Tab walks the active tab's controls — the strip, the
/// controls, Reset, Close — and wraps back to the strip without ever
/// leaving the dialog (asserted by acting: an Enter that lands on Close
/// closes it, and a Right after the wrap switches tabs); Reset resets the
/// ACTIVE tab only.
///
/// The dialog opens on General at launch (`dump.open`) and REOPENS on the
/// tab it was last closed on (settings.md, "Opening and closing"): the
/// last strand closes it on UI and opens it again. The reopen earlier in
/// the script closes on General, which cannot tell the two rules apart —
/// hence a strand that closes on a tab other than the first.
///
/// Mutants (2026-10-01): the Tab arm deleted from the dialog's scope →
/// Slint's window navigation walks the whole item tree, the fourth Tab
/// leaves the dialog for the grid, and `dump.wrapped` reads `cursor=1`
/// with the tab unswitched — red; `on_settings_open` setting the tab to 0
/// instead of the one it was closed on → `dump.reopened` reads
/// `settingstab=0` — red (QE 2026-10-01, D27: until this strand that
/// mutant stayed green).
///
/// Reset NAMES the active tab (settings.md, "The card"; brief 010 R3,
/// AC29): read from the button's own label mark, `settings reset shows
/// <text>` (test-harness.md), last before each of the first three dumps —
/// `Reset General to defaults` at `open`, `Reset UI to defaults` at `right`,
/// `Reset Performance to defaults` at `ctrltab`. Mutant (2026-10-03): the
/// label built from `settings-tabs[0]` instead of the active tab's entry →
/// `dump.right` reads `Reset General to defaults` — red.
#[test]
fn settings_tabs_switch_by_keys_and_never_by_digits() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-tabs.jpg");
    let script = "900:key:ctrl+,;1300:dump.open;1500:key:right;1800:dump.right;\
                  2000:key:ctrl+tab;2300:dump.ctrltab;2500:key:2;2800:dump.digit;\
                  3000:key:ctrl+shift+tab;3300:dump.back;3500:key:left;3700:key:left;\
                  4000:dump.wrapleft;4200:key:right;4500:dump.general;\
                  4700:key:tab;4900:key:tab;5100:key:tab;5300:key:return;5700:dump.enterclose;\
                  6000:key:ctrl+,;6400:key:tab;6600:key:tab;6800:key:tab;7000:key:tab;\
                  7200:key:right;7500:dump.wrapped;\
                  7800:key:left;8100:click:settings auto-advance;8500:dump.aaoff;\
                  8800:key:ctrl+tab;9200:click:settings wash;9500:key:ctrl+a;9700:key:1;\
                  9900:key:0;10100:key:return;10500:dump.wash;\
                  10800:click:settings reset;11200:dump.reset;\
                  11500:key:escape;11900:dump.closed;12100:key:ctrl+,;12500:dump.reopened";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    let tab = |label: &str| dump_field(qedump(&stderr, label), "settingstab").to_string();
    assert_eq!(
        tab("open"),
        "0",
        "the dialog did not open on General:\n{stderr}"
    );
    assert_eq!(
        tab("right"),
        "1",
        "Right on the strip did not switch to UI:\n{stderr}"
    );
    assert_eq!(
        tab("ctrltab"),
        "2",
        "Ctrl+Tab did not switch to Performance:\n{stderr}"
    );
    assert_eq!(
        tab("digit"),
        "2",
        "a digit switched tabs — 1–5 are reserved and never switch (settings.md):\n{stderr}"
    );
    assert_eq!(
        tab("back"),
        "1",
        "Ctrl+Shift+Tab did not switch back:\n{stderr}"
    );
    assert_eq!(
        tab("wrapleft"),
        "2",
        "Left twice from UI did not wrap round to Performance:\n{stderr}"
    );
    assert_eq!(
        tab("general"),
        "0",
        "Right from Performance did not wrap to General:\n{stderr}"
    );
    // Reset names the active tab: what the BUTTON says, its own last label
    // mark before each dump (AC29).
    let labels = mark_labels(&stderr);
    for (dump, said) in [
        ("open", "Reset General to defaults"),
        ("right", "Reset UI to defaults"),
        ("ctrltab", "Reset Performance to defaults"),
    ] {
        let step = format!("drive: dump.{dump}");
        let at = labels
            .iter()
            .position(|l| *l == step)
            .unwrap_or_else(|| panic!("no `{step}` in the trace:\n{stderr}"));
        let shown = labels[..at]
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix("settings reset shows "));
        assert_eq!(
            shown,
            Some(said),
            "at dump.{dump} the Reset button does not name the active tab (its last \
             `settings reset shows` mark before the dump; None means it reported no \
             label at all) — settings.md, \"The card\":\n{stderr}"
        );
    }
    // General's ring: strip → auto-advance → Reset → Close. Three Tabs land
    // on Close, and Enter there closes the dialog.
    let enter = qedump(&stderr, "enterclose");
    assert_eq!(
        dump_field(enter, "settings"),
        "false",
        "three Tabs and an Enter did not close the dialog — the ring is not \
         strip, auto-advance, Reset, Close:\n{stderr}"
    );
    // Four Tabs from the strip wrap back to it: the Right that follows
    // switches tabs, and nothing moved the grid behind.
    let wrapped = qedump(&stderr, "wrapped");
    assert_eq!(
        dump_field(wrapped, "settingstab"),
        "1",
        "after four Tabs the keyboard was not back on the strip:\n{stderr}"
    );
    assert_eq!(
        dump_field(wrapped, "focusowner"),
        "-1",
        "Tab left the dialog:\n{stderr}"
    );
    assert_eq!(
        dump_field(wrapped, "cursor"),
        "0",
        "a key reached the grid behind the dialog — Tab walked out of it:\n{stderr}"
    );
    assert_click_resolved(&stderr, "settings auto-advance");
    assert_eq!(
        dump_field(qedump(&stderr, "aaoff"), "autoadvance"),
        "false",
        "the auto-advance checkbox did not apply on click:\n{stderr}"
    );
    assert_click_resolved(&stderr, "settings wash");
    let wash = qedump(&stderr, "wash");
    assert_eq!(
        dump_field(wash, "wash"),
        "10",
        "the wash field did not commit 10:\n{stderr}"
    );
    assert_eq!(
        dump_field(wash, "washprop"),
        "0.100",
        "the committed wash never reached the window's property:\n{stderr}"
    );
    assert_click_resolved(&stderr, "settings reset");
    let reset = qedump(&stderr, "reset");
    assert_eq!(
        dump_field(reset, "wash"),
        "25",
        "Reset UI did not reset the wash:\n{stderr}"
    );
    assert_eq!(
        dump_field(reset, "washprop"),
        "0.250",
        "Reset never reached the window:\n{stderr}"
    );
    assert_eq!(
        dump_field(reset, "autoadvance"),
        "false",
        "Reset on the UI tab reset General's setting too — it resets the \
         ACTIVE tab only:\n{stderr}"
    );
    // The reopen. The close FIRST: a `Ctrl+,` over a dialog still open is
    // inert, and `settingstab=1` would then hold for the wrong reason.
    assert_eq!(
        dump_field(qedump(&stderr, "closed"), "settings"),
        "false",
        "Esc on the UI tab did not close the dialog, so the reopen below proves \
         nothing:\n{stderr}"
    );
    let reopened = qedump(&stderr, "reopened");
    assert_eq!(
        dump_field(reopened, "settings"),
        "true",
        "Ctrl+, did not reopen the dialog:\n{stderr}"
    );
    assert_eq!(
        dump_field(reopened, "settingstab"),
        "1",
        "the dialog closed on UI did not reopen on UI — it reopens on the tab it \
         was last closed on (settings.md):\n{stderr}"
    );
}

/// AC3 (settings.md, "Keyboard"; QE 2026-10-01, D33): `Tab` or `Shift+Tab`
/// into a number field SELECTS its text, so what is typed replaces the value
/// the field shows — keyboard only, the way a user walks the dialog: no
/// click into a field, no Ctrl+A anywhere. Four fields in turn: Selection
/// highlight (shows 25, `1`,`0` → 10), Loupe memory (`2 GB`, `3` → 3 GB),
/// Thumbnail cache cap (`2 GB`, `1` → 1 GB), the read workers' Limit (`4`,
/// `2` → 2), and the wash field again reached BACKWARDS by Shift+Tab (`2`,`0`
/// → 20). The one click is the Adaptive checkbox, which frees the Limit
/// field; its `toggled` takes the keyboard (`self.focus()`), so ONE Tab then
/// lands on Limit. Nothing is written: the harness's own FASTCULL_NO_CONFIG,
/// no FASTCULL_CONFIG_DIR, and `settings written` never traced is the
/// hermetic premise.
///
/// Slint's TextInput selects all only on a Tab-NAVIGATION focus (i-slint-core
/// 1.17.1 `items/text.rs:1180`, `FocusReason::TabNavigation`), and the
/// dialog's ring takes Tab itself and focuses each field with `focus()` from
/// code — a programmatic focus, which selects nothing and leaves the caret
/// where it was (Cargo.toml, the fourth canary's sixth fact). So the ring
/// selects the field itself, in `focus-slot`, on arrival.
///
/// RED on 13a904e, the head before the fix (QE round 3 of brief 008, D33):
/// nothing was selected and each typed digit went in at the caret, beside
/// the value shown — the wash field showed `125` then `1025` and committed
/// 50, the maximum; the loupe memory committed `32 GB` (loupemem=
/// 33377808384, this seat's RAM, where the clamp held it); the cache cap
/// `12 GB`; the Limit `24`; and the wash field reached by Shift+Tab showed
/// `502`, `5020` and committed 50 again. Every other Settings test clicks a
/// field and presses Ctrl+A before typing, which is why the suite could not
/// see it. When this fails that way it is that defect; do not quiet it, and
/// do not add a Ctrl+A to this script.
///
/// Mutant (2026-10-01): the four `select-all()` calls taken out of
/// `focus-slot` — which IS 13a904e — → red on all five dumps.
///
/// The SPACE strand, at the end (settings.md, "Apply on commit": a checkbox
/// applies on click or `Space`; brief 010 R2, AC24): `Ctrl+Shift+Tab` back
/// to General, one `Tab` from the strip to the Auto-advance box, `Space` —
/// the box commits `false` once and shows it (`dump.aaspace`); then
/// `Ctrl+Shift+Tab` round to Performance, three `Tab`s from the strip to the
/// Adaptive box (cleared above, a limit of 2), `Space` — it commits
/// `max_readers = 0` once and shows `true` (`dump.adspace`). Each commit is
/// counted over the whole run and must come after its own `Space`; what the
/// box SHOWS is its own `shows` mark between that `Space` and the dump.
/// Mutant (2026-10-03): the dialog scope's `capture-key-pressed` accepting
/// `" "` before any control sees it → neither box commits, `dump.aaspace`
/// reads `autoadvance=true` — red.
#[test]
fn a_number_typed_after_tab_replaces_the_value_in_the_field() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-tab-typed.jpg");
    let script = "400:key:ctrl+,;700:key:right;1000:key:tab;1200:key:1;1300:key:0;\
                  1500:key:return;1800:dump.wash;2100:key:ctrl+tab;2400:key:tab;2600:key:3;\
                  2800:key:return;3100:dump.loupe;3400:key:tab;3600:key:1;3800:key:return;\
                  4100:dump.cap;4400:click:settings readers-adaptive;4700:key:tab;4900:key:2;\
                  5100:key:return;5400:dump.limit;5700:key:ctrl+shift+tab;6000:key:shift+tab;\
                  6200:key:shift+tab;6400:key:shift+tab;6600:key:2;6700:key:0;6900:key:return;\
                  7200:dump.washrev;7500:key:ctrl+shift+tab;7800:key:tab;8000:key:space;\
                  8300:dump.aaspace;8600:key:ctrl+shift+tab;8900:key:tab;9100:key:tab;\
                  9300:key:tab;9500:key:space;9800:dump.adspace";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    let wash = qedump(&stderr, "wash");
    assert!(
        dump_field(wash, "wash") == "10" && dump_field(wash, "washprop") == "0.100",
        "Tab into Selection highlight (showing 25), then 1, 0, Enter did not commit \
         10 — the typed number did not replace the value shown, so the field was \
         not selected on arrival (QE 2026-10-01, D33): {wash}\n{stderr}"
    );
    // What the field SHOWED when the dump was taken (the dump's `wash=` is
    // the model's): the last `settings wash shows` mark before the step.
    let labels = mark_labels(&stderr);
    let dump_at = labels
        .iter()
        .position(|l| *l == "drive: dump.wash")
        .unwrap_or_else(|| panic!("no `drive: dump.wash` step in the trace:\n{stderr}"));
    let shown = labels[..dump_at]
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("settings wash shows "));
    assert_eq!(
        shown,
        Some("10"),
        "the wash field does not show 10 after Tab, 1, 0, Enter — what was typed \
         did not replace the value shown:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "loupe"), "loupemem"),
        "3221225472",
        "Tab into Loupe memory (showing 2 GB), then 3, Enter did not commit 3 GB:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "cap"), "cachecap"),
        "1073741824",
        "Tab into the Thumbnail cache cap (showing 2 GB), then 1, Enter did not \
         commit 1 GB:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "limit"), "readers"),
        "limit:2",
        "Tab into the read workers' Limit (showing 4), then 2, Enter did not commit \
         a limit of 2:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "washrev"), "wash"),
        "20",
        "Shift+Tab back into Selection highlight (showing 10), then 2, 0, Enter did \
         not commit 20 — the field is not selected when the ring arrives \
         backwards:\n{stderr}"
    );
    // `Space` on a checkbox commits it (AC24): the Auto-advance box reached
    // by one Tab from General's strip, then the Adaptive box by three Tabs
    // from Performance's.
    let spaces = label_positions(&labels, "drive: key:space");
    assert_eq!(
        spaces.len(),
        2,
        "the two Space steps did not both run:\n{stderr}"
    );
    for (n, (space, dump, tab, field, model, commit, shows)) in [
        (
            spaces[0],
            "aaspace",
            "0",
            "autoadvance",
            "false",
            "settings committed general.auto_advance = false",
            "settings auto-advance shows false",
        ),
        (
            spaces[1],
            "adspace",
            "2",
            "readers",
            "adaptive",
            "settings committed performance.max_readers = 0",
            "settings readers-adaptive shows true",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let step = format!("drive: dump.{dump}");
        let at = labels
            .iter()
            .position(|l| *l == step)
            .unwrap_or_else(|| panic!("no `{step}` in the trace:\n{stderr}"));
        let line = qedump(&stderr, dump);
        assert_eq!(
            dump_field(line, "settingstab"),
            tab,
            "Space {}: not on the tab its box lives on: {line}",
            n + 1
        );
        assert_eq!(
            dump_field(line, field),
            model,
            "Space {} on the checkbox the ring reached did not commit it (`{field}=` \
             at dump.{dump}) — settings.md, \"Apply on commit\": a checkbox applies on \
             click or Space:\n{stderr}",
            n + 1
        );
        let commits = label_positions(&labels, commit);
        assert!(
            commits.len() == 1 && commits[0] > space && commits[0] < at,
            "`{commit}` was traced at {commits:?}: once, after Space {} (at {space}) and \
             before dump.{dump} (at {at}), expected:\n{stderr}",
            n + 1
        );
        assert!(
            labels[space..at].contains(&shows),
            "the box does not SHOW the state Space {} gave it — no `{shows}` between the \
             Space and dump.{dump}:\n{stderr}",
            n + 1
        );
    }
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        0,
        "a settings file was written by a run under FASTCULL_NO_CONFIG — the run is \
         not hermetic:\n{stderr}"
    );
}

/// AC4 and AC9 (settings.md, "Writing" and "Apply on commit"): a commit
/// writes settings.toml at once, keeping the user's comment, the trailing
/// comment on the key and an unknown table; the window's wash takes the
/// committed value; and `Esc` in a field DISCARDS its half-typed text and
/// closes — a `2` on the way to `20` never lands as 2 %.
///
/// Mutants (2026-10-01): the `settings::write` call taken out of the
/// bridge's `save` → the file still says 40 and this goes red; the
/// `root.settings-visible` guard taken out of the wash field's blur → the
/// deferred blur runs before the closed dialog is torn down, commits the
/// half-typed `2`, `dump.discarded` reads `wash=2` and this goes red.
#[test]
fn a_settings_commit_writes_the_file_and_esc_discards_a_half_typed_field() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let users = "# my hand-written settings\n[ui]\nselection_wash = 40 # too strong\n\n\
                 [mine]\nkeep = \"me\"\n";
    let dir = settings_scratch("write", Some(users));
    let out = out_dir().join("settings-write.jpg");
    let script = "900:dump.start;1100:key:ctrl+,;1500:key:right;1900:click:settings wash;\
                  2200:key:ctrl+a;2400:key:1;2600:key:5;2800:key:return;3200:dump.committed;\
                  3500:key:ctrl+a;3700:key:2;3900:key:escape;4300:dump.discarded";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out,
    );
    let file = dir.join("settings.toml");
    assert!(
        stderr.contains(&format!("FASTCULL_CONFIG_DIR={}", dir.display())),
        "the config-dir override did not announce itself on stderr:\n{stderr}"
    );
    let start = qedump(&stderr, "start");
    assert_eq!(
        dump_field(start, "wash"),
        "40",
        "the file's wash was not read:\n{stderr}"
    );
    assert_eq!(
        dump_field(start, "washprop"),
        "0.400",
        "the file's wash never reached the window:\n{stderr}"
    );
    // The path as the dump quotes it (Debug, so a Windows path's
    // backslashes come doubled): its tail is enough to say which file.
    assert!(
        dump_text(start, "settingsfile").ends_with("settings.toml"),
        "the dialog's file is not the scratch settings.toml:\n{stderr}"
    );
    assert_click_resolved(&stderr, "settings wash");
    let committed = qedump(&stderr, "committed");
    assert_eq!(
        dump_field(committed, "wash"),
        "15",
        "the commit did not apply:\n{stderr}"
    );
    assert_eq!(
        dump_field(committed, "washprop"),
        "0.150",
        "the commit reached the model but not the window's property:\n{stderr}"
    );
    let discarded = qedump(&stderr, "discarded");
    assert_eq!(
        dump_field(discarded, "settings"),
        "false",
        "Esc in the field did not close:\n{stderr}"
    );
    assert_eq!(
        dump_field(discarded, "wash"),
        "15",
        "Esc COMMITTED the half-typed `2` instead of discarding it:\n{stderr}"
    );
    assert_eq!(
        dump_field(discarded, "washprop"),
        "0.150",
        "Esc changed the window's wash:\n{stderr}"
    );
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        1,
        "the file was written other than once (once for the one commit):\n{stderr}"
    );
    let text = std::fs::read_to_string(&file).expect("settings.toml after the run");
    for kept in [
        "# my hand-written settings\n[ui]\n",
        "selection_wash = 15 # too strong\n",
        "[mine]\nkeep = \"me\"\n",
        "auto_advance = true",
        "loupe_memory = \"2 GB\"",
    ] {
        assert!(
            text.contains(kept),
            "the written file lacks {kept:?}:\n{text}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// AC4 (settings.md, "Apply on commit": "after a commit … the field shows
/// the value IN FORCE — parsed, clamped, normalised — never the raw text"):
/// a commit that leaves the value in force UNCHANGED, and a value the field
/// refuses, re-show the value in force too. Four strands in one launch,
/// each typed over a field and committed with Enter:
///   - Selection highlight: 50 committed first (it shows 25), then `60` —
///     clamped to 50, the value in force unchanged — and then `abc`, which
///     the field refuses (`set_from_text` fails and the model is untouched);
///   - Loupe memory and the Thumbnail cache cap: `2gb` over `2 GB` — the
///     same 2 GB, normalised;
///   - the read workers' Limit, Adaptive cleared (a limit of 4): `04` —
///     the integer 4.
///
/// Each strand reads what the field SHOWED, from its own `settings <field>
/// shows` mark: the raw text last before its Enter (the PREMISE that the
/// typing reached the field) and the value in force last before its dump
/// (the contract), with the model's value in the dump beside it. Only the
/// field's own `accepted` handler can do this re-show: the value in force
/// did not change, so `changed shown` does not fire, and the first
/// keystroke broke the field's binding, so the bridge's `present` cannot
/// reach it (QE 2026-10-02, round 5: with the re-show taken out of any
/// field's `accepted`, the field went on showing `60`, `abc`, `2gb` or
/// `04` after Enter, and the whole suite stayed green). No config dir: the
/// harness's own FASTCULL_NO_CONFIG, `settings written` 0 the hermetic
/// premise. With the dialog deleted the first field click finds no layout
/// mark and the run aborts.
///
/// Mutants (2026-10-02), each alone: `self.text = root.settings-wash;` taken
/// out of the wash field's `accepted` → the wash field shows `60` at
/// `dump.wash` (and `abc` at `dump.refused`) — red; the same line out of
/// Loupe memory's → it shows `2gb` — red; out of the cap's → `2gb` — red;
/// out of the Limit's → `04` — red.
#[test]
fn a_commit_that_leaves_the_value_in_force_unchanged_still_reshows_it() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-reshow.jpg");
    let script = "400:key:ctrl+,;700:key:right;1000:click:settings wash;1300:key:ctrl+a;\
                  1500:key:5;1700:key:0;1900:key:return;2200:key:ctrl+a;2400:key:6;2600:key:0;\
                  2800:key:return;3100:dump.wash;3400:key:ctrl+a;3600:key:a;3800:key:b;\
                  4000:key:c;4200:key:return;4500:dump.refused;4800:key:ctrl+tab;\
                  5200:click:settings loupe-memory;5500:key:ctrl+a;5700:key:2;5900:key:g;\
                  6100:key:b;6300:key:return;6600:dump.loupe;6900:click:settings cache-cap;\
                  7200:key:ctrl+a;7400:key:2;7600:key:g;7800:key:b;8000:key:return;\
                  8300:dump.cap;8600:click:settings readers-adaptive;\
                  9000:click:settings readers-limit;9300:key:ctrl+a;9500:key:0;9700:key:4;\
                  9900:key:return;10200:dump.limit";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    for element in [
        "settings wash",
        "settings loupe-memory",
        "settings cache-cap",
        "settings readers-adaptive",
        "settings readers-limit",
    ] {
        assert_click_resolved(&stderr, element);
    }
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        0,
        "a settings file was written by a run under FASTCULL_NO_CONFIG — the run is \
         not hermetic:\n{stderr}"
    );
    let labels = mark_labels(&stderr);
    // What the field showed last before position `at`.
    let shown = |field: &str, at: usize| -> Option<&str> {
        let tag = format!("settings {field} shows ");
        labels[..at]
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix(tag.as_str()))
    };
    for (field, raw, dump, model, value, in_force) in [
        ("wash", "60", "wash", "wash", "50", "50"),
        ("wash", "abc", "refused", "wash", "50", "50"),
        (
            "loupe-memory",
            "2gb",
            "loupe",
            "loupemem",
            "2147483648",
            "2 GB",
        ),
        ("cache-cap", "2gb", "cap", "cachecap", "2147483648", "2 GB"),
        ("readers-limit", "04", "limit", "readers", "limit:4", "4"),
    ] {
        let step = format!("drive: dump.{dump}");
        let at = labels
            .iter()
            .position(|l| *l == step)
            .unwrap_or_else(|| panic!("no `{step}` in the trace:\n{stderr}"));
        let enter = labels[..at]
            .iter()
            .rposition(|l| *l == "drive: key:return")
            .unwrap_or_else(|| panic!("no Enter before `{step}`:\n{stderr}"));
        assert_eq!(
            shown(field, enter),
            Some(raw),
            "the premise: the {field} field did not show the typed `{raw}` when Enter \
             was pressed, so the re-show below proves nothing:\n{stderr}"
        );
        assert_eq!(
            dump_field(qedump(&stderr, dump), model),
            value,
            "`{model}=` is not the value in force after `{raw}` and Enter:\n{stderr}"
        );
        assert_eq!(
            shown(field, at),
            Some(in_force),
            "after `{raw}` and Enter the {field} field still shows the raw text, not \
             the value in force `{in_force}` (settings.md, \"Apply on commit\"):\n{stderr}"
        );
    }
}

/// The labels of a run's trace marks, in the order they were emitted
/// (`fastcull-trace: [<ms>] <label>`, the one emit site — `mark_lines`).
fn mark_labels(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("fastcull-trace: ["))
        .filter_map(|r| r.split_once("] "))
        .map(|(_, label)| label)
        .collect()
}

/// The `[<ms>]` stamps of a run's trace marks — milliseconds since the app
/// began tracing (trace.rs `emit`) — in [`mark_labels`]'s order: the same
/// marks, so an index into one is an index into the other.
fn mark_stamps(stderr: &str) -> Vec<u64> {
    stderr
        .lines()
        .filter_map(|l| l.strip_prefix("fastcull-trace: ["))
        .filter_map(|r| r.split_once("] "))
        .map(|(ms, label)| {
            ms.parse()
                .unwrap_or_else(|_| panic!("a trace mark with no `[<ms>]` stamp: {ms:?} {label:?}"))
        })
        .collect()
}

/// AC3 and AC4 (settings.md, "Apply on commit"): a click on Reset is a
/// click-away like any other, so the text the user was typing commits
/// first — ONCE — and then the Reset resets the tab, that field included:
/// the model, the window's wash and what the field SHOWS all end on the
/// default. The UI tab with `35` typed into the wash field, then the
/// Performance tab with `0.5` typed into the loupe memory, no Enter either
/// time. What a field shows is read from its own `settings <field> shows`
/// mark (test-harness.md); the dump's `wash=`/`loupemem=` are the model's.
///
/// RED on 3c0599a, the head before the fix (senior-developer review F1):
/// the click commits 35 and resets, and then the wash field's deferred
/// blur commits the flushed `35` a second time — its `changed shown`
/// re-sync never fired, because the value in force went 25 → 35 → 25
/// inside one event-loop iteration and a Slint `changed` handler fires
/// only for a value that differs from the one it last saw (Cargo.toml,
/// the fourth canary's fact 5) — so `dump.ui` read `wash=35`. When this
/// fails that way it is that defect; do not quiet it.
///
/// Mutants (2026-10-01): the `self.dirty` test dropped from the wash
/// field's blur → the stale `35` is committed again after the Reset and
/// `dump.ui` reads `wash=35` — red; the blur's re-show of the value in
/// force dropped → the model is 25 but the field still shows `35` — red
/// on the `settings wash shows` mark.
///
/// Two more rows, appended (QE 2026-10-02, round 5: each field carries its
/// own `dirty` test, and only the wash's had a guard): `1` typed into the
/// Thumbnail cache cap (it shows `2 GB`), then Reset → `cachecap` back to 2
/// GB and the field showing `2 GB`; Adaptive cleared, `6` typed into the
/// Limit, then Reset → `readers=adaptive` and the field showing nothing —
/// each commit traced once, before its tab's Reset. A row's commit and
/// Reset are looked for after the dump before it: Performance's first
/// Reset in the run is the loupe row's. Mutants (2026-10-02): `self.dirty
/// &&` dropped from the cap's blur → the stale `1 GB` commits again after
/// the Reset, `dump.cap` reads `cachecap=1073741824` — red; dropped from
/// the Limit's blur → `max_readers = 6` traced twice, `dump.limit` reads
/// `readers=limit:6` — red. (The Limit shows `4` before and nothing after,
/// so its `changed shown` re-sync does fire — but Slint runs the trackers
/// last-dirtied first, and the field's blur was dirtied after it, by the
/// click's `focus()`: the blur reads the flushed `6` against the reset
/// value and, without `dirty`, commits it.)
#[test]
fn reset_with_a_half_typed_field_commits_it_then_resets() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-reset-typed.jpg");
    let script = "900:key:ctrl+,;1300:key:right;1700:click:settings wash;2000:key:ctrl+a;\
                  2200:key:3;2400:key:5;2800:click:settings reset;3300:dump.ui;\
                  3600:key:ctrl+tab;4000:click:settings loupe-memory;4300:key:ctrl+a;\
                  4500:key:0;4700:key:.;4900:key:5;5300:click:settings reset;5800:dump.perf;\
                  6100:click:settings cache-cap;6400:key:ctrl+a;6600:key:1;\
                  7000:click:settings reset;7500:dump.cap;7800:click:settings readers-adaptive;\
                  8200:click:settings readers-limit;8500:key:ctrl+a;8700:key:6;\
                  9100:click:settings reset;9600:dump.limit";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    let labels = mark_labels(&stderr);
    // What the field showed last before the dump `label`.
    let shown_before = |field: &str, label: &str| -> Option<String> {
        let dump = format!("QEDUMP {label} ");
        let at = labels.iter().position(|l| l.starts_with(&dump))?;
        let tag = format!("settings {field} shows ");
        labels[..at]
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix(tag.as_str()))
            .map(str::to_string)
    };
    // `after`: the step a row's own commit and Reset are looked for after —
    // the run's start for the first two; for the rows appended after them,
    // the dump before, since the Performance tab's earlier Reset is the
    // loupe row's.
    for (tab, field, commit, reset, dump, model, in_force, default, after) in [
        (
            "UI",
            "wash",
            "settings committed ui.selection_wash = 35",
            "settings reset ui",
            "ui",
            "wash",
            "25",
            "25",
            None,
        ),
        (
            "Performance",
            "loupe-memory",
            "settings committed performance.loupe_memory = 0.5 GB",
            "settings reset performance",
            "perf",
            "loupemem",
            "2147483648",
            "2 GB",
            None,
        ),
        (
            "Performance",
            "cache-cap",
            "settings committed performance.cache_cap = 1 GB",
            "settings reset performance",
            "cap",
            "cachecap",
            "2147483648",
            "2 GB",
            Some("drive: dump.perf"),
        ),
        (
            "Performance",
            "readers-limit",
            "settings committed performance.max_readers = 6",
            "settings reset performance",
            "limit",
            "readers",
            "adaptive",
            "",
            Some("drive: dump.cap"),
        ),
    ] {
        assert_click_resolved(&stderr, &format!("settings {field}"));
        let line = qedump(&stderr, dump);
        assert_eq!(
            dump_field(line, model),
            in_force,
            "Reset {tab} did not reset the {field} field the user was typing in — \
             the half-typed text was committed AFTER the Reset (senior-developer \
             review F1):\n{stderr}"
        );
        let commits = labels.iter().filter(|l| **l == commit).count();
        assert_eq!(
            commits, 1,
            "`{commit}` was traced {commits} time(s): the click-away commit runs \
             exactly once, before the Reset:\n{stderr}"
        );
        let base = after.map_or(0, |step| {
            labels
                .iter()
                .position(|l| *l == step)
                .unwrap_or_else(|| panic!("no `{step}` in the trace:\n{stderr}"))
        });
        let committed_at = labels[base..].iter().position(|l| *l == commit);
        let reset_at = labels[base..].iter().position(|l| *l == reset);
        assert!(
            reset_at.is_some() && committed_at < reset_at,
            "`{commit}` must come before `{reset}` (the click commits first, \
             then resets):\n{stderr}"
        );
        assert_eq!(
            shown_before(field, dump).as_deref(),
            Some(default),
            "the {field} field does not show the value in force after the Reset \
             (settings.md: a field shows the value in force, never stale text):\n{stderr}"
        );
    }
    assert_eq!(
        dump_field(qedump(&stderr, "ui"), "washprop"),
        "0.250",
        "the window's wash is not the default after Reset UI:\n{stderr}"
    );
}

/// AC4 (settings.md, "Apply on commit"): every control that takes the
/// keyboard from a number field holding typed, uncommitted text is a
/// click-away — the field's text commits FIRST, exactly once, and then the
/// control does its own work; only `Esc` discards. One launch per row. The
/// controls run over one dirty field, Loupe memory (showing `2 GB`, `3`
/// typed, no Enter), so those rows read one commit; and the three rules
/// each field carries its OWN copy of — Close's commit (the field's arm of
/// `flush()`), a click on the scrim's commit (the field's blur) and Esc's
/// discard (its blur's `settings-visible` guard) — run over all four fields
/// (QE 2026-10-02, round 5: the matrix read Loupe memory only, and taking
/// any of the other three fields' copies out left the suite green). The
/// next unit's control joins by adding a row — on a tab that has a number
/// field — and its number field by adding the three.
///
/// The rows are the matrix of the senior developer's diagnosis of QE round 4
/// (brief 008 D39), numbered as there, with the field after a dash where it
/// is not Loupe memory: 1 Close; 3 Clear (Linux only: the default cache
/// sandboxed under HOME and XDG_CACHE_HOME, brief 008 D24); 4 the Adaptive
/// checkbox; 4b the Adaptive checkbox with the Limit field dirty instead; 6
/// a click into another field; 7 a click on a tab; 8 Ctrl+Tab; 9
/// Ctrl+Shift+Tab; 11 Tab; 12 Shift+Tab; 14 Esc, the one discard; 15 a click
/// on the scrim; 16 About over the dialog by its token; 17 About and 18 the
/// shortcuts card from the Help menu, and 19 View › IPTC Panel (Linux only:
/// the in-window menu bar, `menu_clicks_are_calibrated`); 20 a folder opened
/// under the dialog; and per field 1-, 15- and 14-wash (Selection highlight,
/// `3` typed over 25), -cap (the Thumbnail cache cap, `1` over `2 GB`) and
/// -limit (the read workers' Limit: Adaptive cleared, a limit of 4, `6`
/// typed — row 4b's setup). 14-wash is pinned too by
/// `a_settings_commit_writes_the_file_and_esc_discards_a_half_typed_field`
/// and kept here so the table has no hole. Pinned by other tests and not
/// re-run here: 2 Reset (`reset_with_a_half_typed_field_commits_it_then_resets`,
/// over all four fields) and 13 Enter
/// (`a_number_typed_after_tab_replaces_the_value_in_the_field`).
/// Unreachable today: 5, the auto-advance checkbox — General has no number
/// field, and a field dirty on another tab is committed by the tab switch
/// before General shows; its `toggled` reads its state before the flush as
/// the Adaptive box's does, review-verified by that shape until General
/// gains a number field and the row can be added — and 10, Left/Right on
/// the strip: the dirty field holds the keyboard, and the strip is reached
/// only by row 12 or by a click, each of which commits first. Not shipped:
/// a click on the strip's empty background, which reports no rectangle, so
/// its point would be a coordinate measured on one platform (issue #70).
/// Window deactivation with a dirty field cannot be driven; settings.md's
/// click-away rule covers it, source-verified (brief 008 D39).
///
/// Each row reads: the commit, counted over the whole run by its EXACT mark
/// (`= 4` must never match `= 40`) — once and after the control's first
/// step, or never for an Esc row; `loupemem=`; the row's own dump fields;
/// and — where the matrix names one — an ORDER on the one trace stream after
/// that step. The order is the guard wherever a `flush()` in the control's
/// handler is what commits: without it the field's own deferred blur still
/// commits, but one event-loop iteration later, after the gaining element's
/// focus mark (Cargo.toml, the second canary's fact 1: the gainer's `changed
/// has-focus` runs before the loser's). Every Esc row carries a PREMISE,
/// checked first: the field's last `settings <field> shows` mark before the
/// Esc is the typed text — without it an Esc row would be green when the
/// typing never reached the field. With the dialog or a field deleted
/// nothing of a row survives: its click finds no layout mark and the run
/// aborts. Every row runs even when one is red, and the test fails naming
/// each red row.
///
/// RED on a377405, the head before the fix (QE round 4, D39, deterministic;
/// reproduced by the senior developer 5/5 and 2/2): row 4 read
/// `readers=adaptive`, the trace saying `settings committed
/// performance.loupe_memory = 3 GB` and then `… max_readers = 0` — the
/// click undone — and row 4b read `readers=limit:6` with `… max_readers =
/// 6` traced twice. The fluent CheckBox flips `checked` and then calls
/// `toggled`; the handler flushed first, the flush's commit ran
/// `present()`, which wrote the model's old value into
/// `settings-readers-adaptive`, and `checked <=>` carried it back into the
/// box before the handler read `self.checked` (Cargo.toml, the fourth
/// canary's fact 7). When row 4 or 4b fails that way it is that defect; do
/// not quiet it.
///
/// Mutants (2026-10-02), each applied alone to main.slint, rebuilt, run and
/// restored byte for byte:
/// - both checkboxes reading `self.checked` after the flush again, no
///   `want` (a377405's shape) → rows 4 and 4b red, as on a377405;
/// - `flush()` out of Close's `clicked` → row 1: the commit traced 0
///   times — `settings-close` hides the dialog before the blur runs, so the
///   blur took the `Esc` path and discarded;
/// - `flush()` out of `go-to-tab` → rows 7, 8 and 9, by order: the strip's
///   `gained` first, the blur's late commit after it;
/// - `flush()` out of `walk` → row 12, by order. Row 11 stays GREEN under
///   it: the ring lands on a field, which traces no focus mark, and the
///   field's blur commits 7 ms later with the same outcome — so row 11
///   guards that Tab commits at all, and is red only with `walk`'s flush
///   AND the field's blur commit both gone;
/// - `flush()` out of Clear's `clicked` → row 3, by order;
/// - the Loupe memory field's own blur commit removed (the belt) → rows 6,
///   15, 16, 17, 18, 19 and 20, the commit traced 0 times: the rows the
///   belt alone guarantees.
///
/// Mutants (2026-10-02, QE round 5), the same way, each red on its row and
/// no other, the commit's count the message: the wash's arm of `flush()`
/// removed → 1-wash (0 times); the cap's (QE's S8) → 1-cap; the Limit's →
/// 1-limit and 4b (Adaptive's own flush commits nothing either); the
/// wash's, the cap's and the Limit's blur commit removed (S10, S11, S12) →
/// 15-wash, 15-cap, 15-limit (0 times); the `settings-visible` guard on
/// Loupe memory's blur (S9) → 14, `3 GB` committed (1 time, 0 expected);
/// the same guard on the wash, the cap and the Limit → 14-wash, 14-cap,
/// 14-limit, each commit traced once.
#[test]
fn every_control_that_leaves_a_dirty_settings_field_commits_it_first() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();

    /// A trace mark, matched EXACTLY, or by its contractual prefix
    /// (`load settled gen N: …`, whose tail is free to differ).
    #[derive(Clone, Copy, Debug)]
    enum Mark {
        Is(&'static str),
        Starts(&'static str),
    }
    impl Mark {
        fn matches(self, label: &str) -> bool {
            match self {
                Mark::Is(mark) => label == mark,
                Mark::Starts(mark) => label.starts_with(mark),
            }
        }
    }
    /// One control acted on while a number field holds typed text.
    struct Row {
        /// The row's number in the matrix (see the doc above).
        row: &'static str,
        control: &'static str,
        /// The script up to the dirty field.
        setup: &'static str,
        /// The control's own steps, then `dump.after`.
        steps: String,
        /// The control's first step as the harness echoes it (its LAST
        /// echo: a setup may hold the same step): the commit must come
        /// after it, and every order is read after it.
        from: Mark,
        /// The named elements the row clicks; each must resolve inside its
        /// rectangle.
        clicks: &'static [&'static str],
        /// The dirty field's commit: traced `commits` times — once, after
        /// `from`, or never for an `Esc` row.
        commit: &'static str,
        commits: usize,
        /// An `Esc` row's PREMISE: the dirty field and the text it showed
        /// last before `from` — the typed text, or the discard proves
        /// nothing.
        typed: Option<(&'static str, &'static str)>,
        /// Other commits the row expects exactly once.
        once: &'static [&'static str],
        /// `loupemem=` at `dump.after`.
        loupemem: &'static str,
        /// The row's own fields at `dump.after`.
        fields: &'static [(&'static str, &'static str)],
        /// Marks that must come in this order after `from`, each pair.
        order: &'static [(Mark, Mark)],
        /// With the default thumbnail cache, sandboxed — Linux only.
        cache: bool,
        /// Through the in-window menu bar — Linux only.
        menu: bool,
    }

    // Settings open on Performance, `3` typed into Loupe memory (it shows
    // `2 GB`), no Enter.
    const DIRTY_LOUPE: &str = "400:key:ctrl+,;700:key:ctrl+shift+tab;\
                               1000:click:settings loupe-memory;1300:key:ctrl+a;1500:key:3";
    const LOUPE_3GB: &str = "settings committed performance.loupe_memory = 3 GB";
    const GB3: &str = "3221225472";
    const GB2: &str = "2147483648";
    const STRIP_GAINED: Mark = Mark::Is("focus: settings strip gained");
    const DIALOG_GAINED: Mark = Mark::Is("focus: settings dialog gained");
    const CLOSED: Mark = Mark::Is("settings closed");
    // The field dimension (QE 2026-10-02, round 5): the same Close, scrim and
    // Esc over the other three fields. Settings open on UI (Right from the
    // strip), `3` typed into Selection highlight (it shows 25).
    const DIRTY_WASH: &str = "400:key:ctrl+,;700:key:right;1000:click:settings wash;\
                              1300:key:ctrl+a;1500:key:3";
    const WASH_3: &str = "settings committed ui.selection_wash = 3";
    // Settings open on Performance, `1` typed into the Thumbnail cache cap
    // (it shows `2 GB`).
    const DIRTY_CAP: &str = "400:key:ctrl+,;700:key:ctrl+shift+tab;\
                             1000:click:settings cache-cap;1300:key:ctrl+a;1500:key:1";
    const CAP_1GB: &str = "settings committed performance.cache_cap = 1 GB";
    // Settings open on Performance, Adaptive cleared (a limit of 4), then
    // `6` typed into the Limit field — row 4b's setup.
    const DIRTY_LIMIT: &str = "400:key:ctrl+,;700:key:ctrl+shift+tab;\
                               1000:click:settings readers-adaptive;\
                               1400:click:settings readers-limit;1700:key:ctrl+a;1900:key:6";
    const LIMIT_6: &str = "settings committed performance.max_readers = 6";
    const LIMIT_4: &str = "settings committed performance.max_readers = 4";
    let row = |row: &'static str, control: &'static str, steps: String, from: Mark| Row {
        row,
        control,
        setup: DIRTY_LOUPE,
        steps,
        from,
        clicks: &["settings loupe-memory"],
        commit: LOUPE_3GB,
        commits: 1,
        typed: None,
        once: &[],
        loupemem: GB3,
        fields: &[],
        order: &[],
        cache: false,
        menu: false,
    };
    let folder = out_dir().join("click-away-folder");
    std::fs::remove_dir_all(&folder).ok();
    std::fs::create_dir_all(&folder).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &folder.join("one.ARW"),
    );
    let home = out_dir().join("click-away-cache-home");
    std::fs::remove_dir_all(&home).ok();
    // Gone however the test ends, a red row included.
    struct RemoveOnDrop(Vec<PathBuf>);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            for dir in &self.0 {
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }
    let _cleanup = RemoveOnDrop(vec![folder.clone(), home.clone()]);

    let rows = vec![
        Row {
            clicks: &["settings loupe-memory", "settings close"],
            fields: &[("settings", "false")],
            order: &[(Mark::Is(LOUPE_3GB), Mark::Is("settings closed"))],
            ..row(
                "1",
                "Close",
                "1900:click:settings close;2300:dump.after".into(),
                Mark::Is("drive: click:settings close"),
            )
        },
        Row {
            clicks: &["settings loupe-memory", "settings clear-cache"],
            order: &[(Mark::Is(LOUPE_3GB), Mark::Is("settings cache clearing"))],
            cache: true,
            ..row(
                "3",
                "Clear",
                "1900:click:settings clear-cache;2000:wait:settings cache cleared;2300:dump.after"
                    .into(),
                Mark::Is("drive: click:settings clear-cache"),
            )
        },
        Row {
            clicks: &["settings loupe-memory", "settings readers-adaptive"],
            once: &["settings committed performance.max_readers = 4"],
            fields: &[("readers", "limit:4")],
            order: &[
                (
                    Mark::Is(LOUPE_3GB),
                    Mark::Is("settings committed performance.max_readers = 4"),
                ),
                (
                    Mark::Is("settings committed performance.max_readers = 4"),
                    Mark::Is("settings readers-adaptive shows false"),
                ),
            ],
            ..row(
                "4",
                "the Adaptive checkbox",
                "1900:click:settings readers-adaptive;2300:dump.after".into(),
                Mark::Is("drive: click:settings readers-adaptive"),
            )
        },
        Row {
            // Adaptive cleared first (a limit of 4), then `6` typed into the
            // Limit field, then Adaptive clicked again to turn it back on.
            setup: DIRTY_LIMIT,
            clicks: &["settings readers-adaptive", "settings readers-limit"],
            commit: LIMIT_6,
            once: &["settings committed performance.max_readers = 0"],
            loupemem: GB2,
            fields: &[("readers", "adaptive")],
            order: &[(
                Mark::Is(LIMIT_6),
                Mark::Is("settings committed performance.max_readers = 0"),
            )],
            ..row(
                "4b",
                "the Adaptive checkbox, the Limit field dirty",
                "2300:click:settings readers-adaptive;2700:dump.after".into(),
                Mark::Is("drive: click:settings readers-adaptive"),
            )
        },
        Row {
            clicks: &["settings loupe-memory", "settings cache-cap"],
            fields: &[("cachecap", "1073741824")],
            ..row(
                "6",
                "a click into another field",
                "1900:click:settings cache-cap;2200:key:ctrl+a;2400:key:1;2600:key:return;\
                 2900:dump.after"
                    .into(),
                Mark::Is("drive: click:settings cache-cap"),
            )
        },
        Row {
            clicks: &["settings loupe-memory", "settings tab ui"],
            fields: &[("settingstab", "1")],
            order: &[(Mark::Is(LOUPE_3GB), STRIP_GAINED)],
            ..row(
                "7",
                "a click on a tab",
                "1900:click:settings tab ui;2300:dump.after".into(),
                Mark::Is("drive: click:settings tab ui"),
            )
        },
        Row {
            fields: &[("settingstab", "0")],
            order: &[(Mark::Is(LOUPE_3GB), STRIP_GAINED)],
            ..row(
                "8",
                "Ctrl+Tab",
                "1900:key:ctrl+tab;2300:dump.after".into(),
                Mark::Is("drive: key:ctrl+tab"),
            )
        },
        Row {
            fields: &[("settingstab", "1")],
            order: &[(Mark::Is(LOUPE_3GB), STRIP_GAINED)],
            ..row(
                "9",
                "Ctrl+Shift+Tab",
                "1900:key:ctrl+shift+tab;2300:dump.after".into(),
                Mark::Is("drive: key:ctrl+shift+tab"),
            )
        },
        Row {
            // The ring lands on the Thumbnail cache cap and selects it, so
            // the `1` replaces `2 GB`.
            fields: &[("cachecap", "1073741824")],
            ..row(
                "11",
                "Tab",
                "1900:key:tab;2200:key:1;2400:key:return;2700:dump.after".into(),
                Mark::Is("drive: key:tab"),
            )
        },
        Row {
            // Back to the strip; the Right that follows switches tabs.
            fields: &[("settingstab", "0")],
            order: &[(Mark::Is(LOUPE_3GB), STRIP_GAINED)],
            ..row(
                "12",
                "Shift+Tab",
                "1900:key:shift+tab;2200:key:right;2600:dump.after".into(),
                Mark::Is("drive: key:shift+tab"),
            )
        },
        Row {
            // The scrim reports no rectangle; x 20 is outside the 560 px
            // card on every window the app supports. The dialog's scope
            // takes the keyboard (brief 008 D19) and the field's blur
            // commits.
            fields: &[("settings", "true")],
            order: &[(
                Mark::Is("focus: settings dialog gained"),
                Mark::Is(LOUPE_3GB),
            )],
            ..row(
                "15",
                "a click on the scrim",
                "1900:click.20,300;2300:dump.after".into(),
                Mark::Is("drive: click.20,300"),
            )
        },
        Row {
            fields: &[("about", "true"), ("settings", "true")],
            order: &[(STRIP_GAINED, Mark::Is(LOUPE_3GB))],
            ..row(
                "16",
                "About over the dialog, by its token",
                "1900:about;2300:dump.after".into(),
                Mark::Is("drive: about"),
            )
        },
        Row {
            // Help at x 115 in the bar, About its second item: the menu's
            // popup takes the keyboard first.
            fields: &[("about", "true"), ("settings", "true")],
            order: &[(Mark::Is(LOUPE_3GB), STRIP_GAINED)],
            menu: true,
            ..row(
                "17",
                "About from the Help menu",
                "1900:click.115,19;2300:click.180,93;2700:dump.after".into(),
                Mark::Is("drive: click.115,19"),
            )
        },
        Row {
            fields: &[("shortcuts", "true")],
            menu: true,
            ..row(
                "18",
                "the shortcuts card from the Help menu",
                "1900:click.115,19;2300:click.180,61;2700:dump.after".into(),
                Mark::Is("drive: click.115,19"),
            )
        },
        Row {
            // The panel opens under the modal: the menu bar is live by spec.
            fields: &[("iptc", "true")],
            menu: true,
            ..row(
                "19",
                "View › IPTC Panel",
                "1900:click.72,19;2300:click.130,125;2700:dump.after".into(),
                Mark::Is("drive: click.72,19"),
            )
        },
        Row {
            fields: &[("settings", "true")],
            order: &[(Mark::Is(LOUPE_3GB), Mark::Starts("load settled gen 1:"))],
            ..row(
                "20",
                "a folder opened under the dialog",
                format!(
                    "1900:open:{};2000:wait:load settled gen 1;2300:dump.after",
                    folder.display()
                ),
                Mark::Starts("drive: open:"),
            )
        },
        // Esc over Loupe memory: the one discard.
        Row {
            commits: 0,
            typed: Some(("loupe-memory", "3")),
            loupemem: GB2,
            fields: &[("settings", "false")],
            ..row(
                "14",
                "Esc",
                "1900:key:escape;2300:dump.after".into(),
                Mark::Is("drive: key:escape"),
            )
        },
        // The field dimension: Close, the scrim and Esc over the other three.
        Row {
            setup: DIRTY_WASH,
            clicks: &["settings wash", "settings close"],
            commit: WASH_3,
            loupemem: GB2,
            fields: &[("settings", "false"), ("wash", "3"), ("washprop", "0.030")],
            order: &[(Mark::Is(WASH_3), CLOSED)],
            ..row(
                "1-wash",
                "Close, Selection highlight dirty",
                "1900:click:settings close;2300:dump.after".into(),
                Mark::Is("drive: click:settings close"),
            )
        },
        Row {
            setup: DIRTY_WASH,
            clicks: &["settings wash"],
            commit: WASH_3,
            loupemem: GB2,
            fields: &[("settings", "true"), ("wash", "3")],
            order: &[(DIALOG_GAINED, Mark::Is(WASH_3))],
            ..row(
                "15-wash",
                "a click on the scrim, Selection highlight dirty",
                "1900:click.20,300;2300:dump.after".into(),
                Mark::Is("drive: click.20,300"),
            )
        },
        Row {
            setup: DIRTY_WASH,
            clicks: &["settings wash"],
            commit: WASH_3,
            commits: 0,
            typed: Some(("wash", "3")),
            loupemem: GB2,
            fields: &[("settings", "false"), ("wash", "25"), ("washprop", "0.250")],
            ..row(
                "14-wash",
                "Esc, Selection highlight dirty",
                "1900:key:escape;2300:dump.after".into(),
                Mark::Is("drive: key:escape"),
            )
        },
        Row {
            setup: DIRTY_CAP,
            clicks: &["settings cache-cap", "settings close"],
            commit: CAP_1GB,
            loupemem: GB2,
            fields: &[("settings", "false"), ("cachecap", "1073741824")],
            order: &[(Mark::Is(CAP_1GB), CLOSED)],
            ..row(
                "1-cap",
                "Close, the cache cap dirty",
                "1900:click:settings close;2300:dump.after".into(),
                Mark::Is("drive: click:settings close"),
            )
        },
        Row {
            setup: DIRTY_CAP,
            clicks: &["settings cache-cap"],
            commit: CAP_1GB,
            loupemem: GB2,
            fields: &[("settings", "true"), ("cachecap", "1073741824")],
            order: &[(DIALOG_GAINED, Mark::Is(CAP_1GB))],
            ..row(
                "15-cap",
                "a click on the scrim, the cache cap dirty",
                "1900:click.20,300;2300:dump.after".into(),
                Mark::Is("drive: click.20,300"),
            )
        },
        Row {
            setup: DIRTY_CAP,
            clicks: &["settings cache-cap"],
            commit: CAP_1GB,
            commits: 0,
            typed: Some(("cache-cap", "1")),
            loupemem: GB2,
            fields: &[("settings", "false"), ("cachecap", GB2)],
            ..row(
                "14-cap",
                "Esc, the cache cap dirty",
                "1900:key:escape;2300:dump.after".into(),
                Mark::Is("drive: key:escape"),
            )
        },
        Row {
            setup: DIRTY_LIMIT,
            clicks: &[
                "settings readers-adaptive",
                "settings readers-limit",
                "settings close",
            ],
            commit: LIMIT_6,
            once: &[LIMIT_4],
            loupemem: GB2,
            fields: &[("settings", "false"), ("readers", "limit:6")],
            order: &[(Mark::Is(LIMIT_6), CLOSED)],
            ..row(
                "1-limit",
                "Close, the Limit dirty",
                "2300:click:settings close;2700:dump.after".into(),
                Mark::Is("drive: click:settings close"),
            )
        },
        Row {
            setup: DIRTY_LIMIT,
            clicks: &["settings readers-adaptive", "settings readers-limit"],
            commit: LIMIT_6,
            once: &[LIMIT_4],
            loupemem: GB2,
            fields: &[("settings", "true"), ("readers", "limit:6")],
            order: &[(DIALOG_GAINED, Mark::Is(LIMIT_6))],
            ..row(
                "15-limit",
                "a click on the scrim, the Limit dirty",
                "2300:click.20,300;2700:dump.after".into(),
                Mark::Is("drive: click.20,300"),
            )
        },
        Row {
            setup: DIRTY_LIMIT,
            clicks: &["settings readers-adaptive", "settings readers-limit"],
            commit: LIMIT_6,
            commits: 0,
            typed: Some(("readers-limit", "6")),
            once: &[LIMIT_4],
            loupemem: GB2,
            fields: &[("settings", "false"), ("readers", "limit:4")],
            ..row(
                "14-limit",
                "Esc, the Limit dirty",
                "2300:key:escape;2700:dump.after".into(),
                Mark::Is("drive: key:escape"),
            )
        },
    ];

    let check = |row: &Row| {
        let script = format!("{};{}", row.setup, row.steps);
        let shot = out_dir().join(format!("settings-click-away-row-{}.jpg", row.row));
        let stderr = if row.cache {
            let cache_home = home.join(".cache");
            std::fs::create_dir_all(&cache_home).unwrap();
            shoot_with_sandboxed_cache(
                &["--synthetic", "24"],
                &[
                    ("FASTCULL_TRACE", "1"),
                    ("HOME", home.to_str().unwrap()),
                    ("XDG_CACHE_HOME", cache_home.to_str().unwrap()),
                    ("FASTCULL_DRIVE", script.as_str()),
                ],
                &shot,
            )
        } else {
            shoot_env_stderr(
                &["--synthetic", "24"],
                &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
                &shot,
            )
        };
        let trace = shot.with_extension("trace.log");
        let trace = trace.display();
        for element in row.clicks {
            assert_click_resolved(&stderr, element);
        }
        // The hermetic premise: the harness's own FASTCULL_NO_CONFIG.
        assert_eq!(
            mark_lines(&stderr, "settings written "),
            0,
            "a settings file was written by a run under FASTCULL_NO_CONFIG; trace: {trace}"
        );
        let labels = mark_labels(&stderr);
        let from = labels
            .iter()
            .rposition(|l| row.from.matches(l))
            .unwrap_or_else(|| panic!("no {:?} step in the trace: {trace}", row.from));
        if let Some((field, typed)) = row.typed {
            let tag = format!("settings {field} shows ");
            let shown = labels[..from]
                .iter()
                .rev()
                .find_map(|l| l.strip_prefix(tag.as_str()));
            assert_eq!(
                shown,
                Some(typed),
                "the premise: the {field} field did not show the typed `{typed}` when \
                 the control acted — a discard of text that never reached the field \
                 proves nothing; trace: {trace}"
            );
        }
        let commits = labels.iter().filter(|l| **l == row.commit).count();
        assert_eq!(
            commits,
            row.commits,
            "`{}` was traced {commits} time(s), {} expected: {}; trace: {trace}",
            row.commit,
            row.commits,
            if row.commits == 0 {
                "Esc DISCARDS the half-typed text, it never commits"
            } else {
                "the half-typed text commits exactly once when the control takes the \
                 keyboard from it"
            }
        );
        if row.commits == 1 {
            let committed = labels.iter().position(|l| *l == row.commit);
            assert!(
                committed > Some(from),
                "`{}` came before the control acted — not a click-away commit; trace: \
                 {trace}",
                row.commit
            );
        }
        let after = qedump(&stderr, "after");
        assert_eq!(
            dump_field(after, "loupemem"),
            row.loupemem,
            "the dirty field's value is not in force after the control: {after}\ntrace: {trace}"
        );
        for (field, want) in row.fields {
            assert_eq!(
                dump_field(after, field),
                *want,
                "`{field}=` after the control: {after}\ntrace: {trace}"
            );
        }
        for commit in row.once {
            let n = labels.iter().filter(|l| *l == commit).count();
            assert_eq!(
                n, 1,
                "`{commit}` was traced {n} time(s), once expected; trace: {trace}"
            );
        }
        let tail = &labels[from..];
        for (first, second) in row.order {
            let a = tail.iter().position(|l| first.matches(l));
            let b = tail.iter().position(|l| second.matches(l));
            assert!(
                a.is_some() && b.is_some() && a < b,
                "{first:?} (at {a:?}) must come before {second:?} (at {b:?}) after the \
                 control's step; trace: {trace}"
            );
        }
    };

    let mut red = Vec::new();
    for row in &rows {
        if row.menu && !menu_clicks_are_calibrated() {
            eprintln!(
                "row {} ({}): skipped — the menu bar is the OS's here, outside the window",
                row.row, row.control
            );
            continue;
        }
        if row.cache && !cfg!(target_os = "linux") {
            eprintln!(
                "row {} ({}): skipped — the default cache cannot be sandboxed off Linux",
                row.row, row.control
            );
            continue;
        }
        // Every row runs, so one red row never hides another; each row's own
        // message is printed above by the panic hook, whole.
        if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(row))) {
            let why = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let first = why.lines().next().unwrap_or("").to_string();
            red.push(format!("row {} ({}): {first}", row.row, row.control));
        }
    }
    assert!(
        red.is_empty(),
        "A CONTROL TOOK THE KEYBOARD FROM A HALF-TYPED FIELD WITHOUT COMMITTING IT FIRST, \
         OR ESC COMMITTED WHAT IT MUST DISCARD (settings.md, \"Apply on commit\") — {} of \
         {} rows red, each one's whole message printed above:\n{}",
        red.len(),
        rows.len(),
        red.join("\n")
    );
}

/// AC5 (settings.md, "Reading" and "Writing"): a file that does not parse
/// gives the defaults, says so on stderr and on the status line, shows the
/// whole error in the dialog's notice — and is never overwritten in place:
/// the first commit moves it aside, byte for byte, to
/// `settings.toml.broken`, writes a fresh file, and names where it went.
///
/// Mutant (2026-10-01): core's `write` overwriting a broken file instead of
/// moving it aside → `settings.toml.broken` never exists and this goes red.
#[test]
fn a_malformed_settings_file_yields_defaults_and_is_moved_aside_on_the_first_write() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let broken = "[general\nauto_advance = false\n";
    let dir = settings_scratch("broken", Some(broken));
    let out = out_dir().join("settings-broken.jpg");
    let script = "900:dump.start;1100:key:ctrl+,;1500:dump.opened;1700:key:right;\
                  2100:click:settings wash;2400:key:ctrl+a;2600:key:1;2800:key:5;3000:key:return;\
                  3400:dump.after;3700:key:escape;4100:dump.closed";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out,
    );
    let file = dir.join("settings.toml");
    assert!(
        stderr.contains(&format!(
            "fastcull: {} could not be read (TOML parse error at line 1, column 9) — defaults in force",
            file.display()
        )),
        "no stderr line naming the file and the error:\n{stderr}"
    );
    let start = qedump(&stderr, "start");
    assert_eq!(
        dump_field(start, "autoadvance"),
        "true",
        "the broken file's `false` was applied — the defaults must be in force:\n{stderr}"
    );
    assert!(
        dump_text(start, "status")
            .contains("⚠ settings.toml could not be read (defaults in force)"),
        "the status line does not say the file could not be read: {start}"
    );
    let opened = qedump(&stderr, "opened");
    let note = dump_text(opened, "settingsnote");
    assert!(
        note.contains("could not be read") && note.contains("invalid table header"),
        "the notice does not show the whole error: {note:?}"
    );
    let after = qedump(&stderr, "after");
    assert_eq!(dump_field(after, "wash"), "15");
    let moved = "settings.toml rewritten — the file that would not read is settings.toml.broken";
    assert_eq!(
        dump_text(after, "settingsnote"),
        moved,
        "the notice does not name where it went"
    );
    assert!(
        dump_text(qedump(&stderr, "closed"), "status").contains(moved),
        "the status line does not name where the broken file went"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("settings.toml.broken"))
            .ok()
            .as_deref(),
        Some(broken),
        "the broken file was not moved aside byte for byte — it was \
         overwritten in place (brief 008 D5):\n{stderr}"
    );
    let fresh = std::fs::read_to_string(&file).expect("a fresh settings.toml");
    assert!(
        fresh.parse::<toml::Table>().is_ok() && fresh.contains("selection_wash = 15"),
        "the fresh file does not parse or lacks the commit:\n{fresh}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC4 and settings.md, "Reading": the file is read again at every open of
/// the dialog and what it says is APPLIED at that open — docs/settings.md:
/// "A hand edit takes effect when you next open the dialog". The file says
/// `selection_wash = 40` at launch; the dialog is opened and closed; a
/// helper thread then rewrites the file by hand to 10 — anchored on the
/// app's own `settings closed` line (the issue #50 way, as the D26 test
/// does), a second before the reopen — and the reopen applies it: the
/// WINDOW's `selection-wash-opacity` reads 0.100 (`washprop=`), not only the
/// model. `wash=10` at the reopen is the PREMISE that the re-read saw the
/// edit, so a hand edit that lost its race fails on that, with its own
/// message, never as a false pass; and the reopen traced `settings loaded
/// from <path>` after its own `Ctrl+,` — the re-read happened (QE
/// 2026-10-02, round 5: with the open's apply taken out, the grid kept its
/// old tint beside the new value in the dialog, and the suite stayed
/// green).
///
/// Mutant (2026-10-02): `apply_instant` taken out of the bridge's
/// `on_settings_open` → `dump.reopened` reads `washprop=0.400` beside
/// `wash=10` — red.
#[test]
fn a_hand_edit_is_applied_when_the_dialog_next_opens() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("reopen-edit", Some("[ui]\nselection_wash = 40\n"));
    let file = dir.join("settings.toml");
    // The helper waits for the drain thread's signal; when the run ends the
    // sender goes with the drain thread, so a mark that never came ends the
    // wait at once rather than at a timeout.
    let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();
    let editor = {
        let file = file.clone();
        std::thread::spawn(move || {
            if closed_rx.recv().is_ok() {
                std::fs::write(&file, "[ui]\nselection_wash = 10\n").unwrap();
            }
        })
    };
    let out = out_dir().join("settings-reopen-edit.jpg");
    let script = "900:key:ctrl+,;1300:dump.first;1600:key:escape;2600:key:ctrl+,;\
                  3000:dump.reopened";
    let mut signalled = false;
    let stderr = shoot_env_stderr_watching(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out,
        // The FIRST close only.
        move |line| {
            if !signalled && line.contains("] settings closed") {
                signalled = true;
                let _ = closed_tx.send(());
            }
        },
    );
    editor.join().unwrap();
    let first = qedump(&stderr, "first");
    assert!(
        dump_field(first, "wash") == "40" && dump_field(first, "washprop") == "0.400",
        "the file read at launch was not applied: {first}"
    );
    let reopened = qedump(&stderr, "reopened");
    assert_eq!(
        dump_field(reopened, "wash"),
        "10",
        "the premise: the reopen's re-read did not see the hand edit (it landed late, \
         or not at all), so the window's wash below would prove nothing:\n{stderr}"
    );
    let labels = mark_labels(&stderr);
    let reopen = labels
        .iter()
        .rposition(|l| *l == "drive: key:ctrl+,")
        .unwrap_or_else(|| panic!("no second `drive: key:ctrl+,`:\n{stderr}"));
    let loaded = format!("settings loaded from {}", file.display());
    assert!(
        labels[reopen..].contains(&loaded.as_str()),
        "the reopen did not re-read the file — no `{loaded}` after its Ctrl+,:\n{stderr}"
    );
    assert_eq!(
        dump_field(reopened, "washprop"),
        "0.100",
        "the hand edit reached the dialog's model but not the WINDOW — the grid kept \
         its old tint (settings.md, \"Reading\": what the file says is applied at that \
         open):\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC25 (settings.md, "Writing": "every save writes EVERY known key with the
/// value the saving window holds, so … a hand edit made while the dialog is
/// open, loses to the last save" — the user's option A, brief 008 D42; brief
/// 010 R2, issue #100's fifth NOW guard). The file says `selection_wash = 40`
/// at launch and the open reads it (`wash=40` at `dump.opened`). While the
/// dialog is open a helper thread rewrites the file by hand — `selection_wash
/// = 10` and a key of the user's own, `hand_edit = 1` — anchored on the app's
/// own `settings opened` line (the issue #50 way: the drain thread only
/// signals, through an unbounded channel), 1.7 s ahead of the click on the
/// script's clock. A commit of ANOTHER setting (the Auto-advance box) then
/// saves every key as the dialog holds it: the file reads `selection_wash =
/// 40` again, beside the user's surviving `hand_edit = 1`, and the model
/// never saw 10 (`wash=40` at `dump.saved`).
///
/// The `edited` premise (the integrity review's): `hand_edit = 1` in the
/// written file proves the hand edit reached the file, and `auto_advance =
/// false` beside it proves the save came AFTER it and merged into it — a
/// hand edit that lost its race and landed after the save leaves a file with
/// no `auto_advance` line at all, which fails there, never as a pass; and
/// `wash=40` at the open proves the edit followed the open's re-read. Each
/// premise is asserted before the claim, with its own message.
///
/// Mutant (2026-10-03): the bridge's `save` writing only the CHANGED key —
/// the file re-read and the committed key alone set over it (D42's option B)
/// → the file keeps the hand edit's `selection_wash = 10` beside `auto_advance
/// = false` — red.
#[test]
fn a_hand_edit_made_while_the_dialog_is_open_loses_to_the_next_save() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("open-edit", Some("[ui]\nselection_wash = 40\n"));
    let file = dir.join("settings.toml");
    // The helper waits for the drain thread's signal; when the run ends the
    // sender goes with the drain thread, so a mark that never came ends the
    // wait at once rather than at a timeout.
    let (opened_tx, opened_rx) = std::sync::mpsc::channel::<()>();
    let editor = {
        let file = file.clone();
        std::thread::spawn(move || {
            if opened_rx.recv().is_ok() {
                std::fs::write(&file, "[ui]\nselection_wash = 10\nhand_edit = 1\n").unwrap();
            }
        })
    };
    let out = out_dir().join("settings-open-edit.jpg");
    let script = "900:key:ctrl+,;1300:dump.opened;2600:click:settings auto-advance;\
                  3000:dump.saved";
    let mut signalled = false;
    let stderr = shoot_env_stderr_watching(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out,
        // The open's own line, once.
        move |line| {
            if !signalled && line.contains("] settings opened") {
                signalled = true;
                let _ = opened_tx.send(());
            }
        },
    );
    editor.join().unwrap();
    let text = std::fs::read_to_string(&file).expect("settings.toml after the run");
    let has = |line: &str| text.lines().any(|l| l.trim() == line);
    // The premises.
    let opened = qedump(&stderr, "opened");
    assert!(
        dump_field(opened, "wash") == "40" && dump_field(opened, "washprop") == "0.400",
        "the premise: the open did not read the launch file's 40, so the hand edit's \
         place relative to the re-read is unknown: {opened}"
    );
    assert_click_resolved(&stderr, "settings auto-advance");
    assert_eq!(
        mark_lines(&stderr, "settings committed general.auto_advance = false"),
        1,
        "the premise: the click on Auto-advance did not commit once:\n{stderr}"
    );
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        1,
        "the premise: the commit was not saved, once:\n{stderr}"
    );
    assert!(
        has("hand_edit = 1"),
        "the premise: the hand edit never reached the file (the anchor never fired), \
         so nothing below is about a hand edit: {text:?}"
    );
    assert!(
        has("auto_advance = false"),
        "the premise: the save did not come after the hand edit — the file is the hand \
         edit's alone, the save's `auto_advance = false` missing — the edit lost its \
         race, so nothing below proves the save wins: {text:?}"
    );
    // The claim: the save wrote every key as the dialog held it.
    assert_eq!(
        dump_field(qedump(&stderr, "saved"), "wash"),
        "40",
        "the dialog's model took the hand edit's 10 while it was open — only an open \
         re-reads the file:\n{stderr}"
    );
    assert!(
        has("selection_wash = 40") && !has("selection_wash = 10"),
        "A HAND EDIT MADE WHILE THE DIALOG WAS OPEN SURVIVED THE NEXT SAVE: the file \
         keeps the hand edit's wash where the save must write the 40 the dialog holds \
         (settings.md, \"Writing\"; the user, brief 008 D42, option A): {text:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC5, a read error NEWER than the move-aside (settings.md, "Writing"; QE
/// 2026-10-01, D26): a file broken at launch is moved aside by the first
/// commit and a fresh one written; a hand edit then breaks the FRESH file
/// while the session runs, and the next open's re-read fails. The notice
/// and the status line must say the file could not be read — every
/// setting just went back to its default — and still name the file moved
/// aside earlier, instead of `rewritten`.
///
/// The hand edit is anchored to the app's own `settings written` mark (the
/// issue #50 shape): the drain thread only signals, a helper thread
/// rewrites the file. The reopen comes ~1.7 s later on the script's clock,
/// and the verdict does not lean on that margin: the reopen's own read mark
/// must carry the hand edit's error (line 2) before anything is read off
/// the notice, so a hand edit that lost the race fails on THAT assertion,
/// never as a false D26.
///
/// The reopen's re-read also prints the stderr line startup prints for a
/// file that will not read (settings.md, "Reading"; QE 2026-10-01, D31) —
/// the line-2 error, which only that re-read can produce.
///
/// RED on 6f20679, the head before the fix: the `reread` notice read
/// `settings.toml rewritten — the file that would not read is
/// settings.toml.broken` while the defaults had taken over. When this
/// fails that way it is that defect; do not quiet it. RED on 16abebd, the
/// head before the D31 fix: no stderr line for the line-2 error.
///
/// Mutants (2026-10-01): the old arm order restored in `notice` (the
/// `moved_aside` arm above the read error) → red on the `reread` notice;
/// the bridge's `report_on_stderr()` call after the re-read removed → red
/// on the stderr line.
#[test]
fn a_hand_edit_that_breaks_the_fresh_file_is_shown_not_masked_by_rewritten() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let at_launch = "[general\nauto_advance = false\n";
    let by_hand = "[general]\nauto_advance = maybe\n";
    let dir = settings_scratch("broken-again", Some(at_launch));
    let file = dir.join("settings.toml");
    // The helper waits for the drain thread's signal; when the run ends the
    // sender goes with the drain thread, so a mark that never came ends the
    // wait at once rather than at a timeout.
    let (written_tx, written_rx) = std::sync::mpsc::channel::<()>();
    let editor = {
        let file = file.clone();
        std::thread::spawn(move || {
            if written_rx.recv().is_ok() {
                std::fs::write(&file, by_hand).unwrap();
            }
        })
    };
    let out = out_dir().join("settings-broken-again.jpg");
    let script = "900:key:ctrl+,;1300:key:right;1700:click:settings wash;2000:key:ctrl+a;\
                  2200:key:1;2400:key:5;2600:key:return;3000:dump.after;3300:key:escape;\
                  4300:key:ctrl+,;4700:dump.reread";
    let mut signalled = false;
    let stderr = shoot_env_stderr_watching(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out,
        // The FIRST write only — the one that moved the launch file aside.
        move |line| {
            if !signalled && line.contains("] settings written ") {
                signalled = true;
                let _ = written_tx.send(());
            }
        },
    );
    editor.join().unwrap();
    // The premises, each with its own message: the first commit moved the
    // launch file aside, and the reopen's read saw the hand edit.
    let after = qedump(&stderr, "after");
    assert_eq!(
        dump_text(after, "settingsnote"),
        "settings.toml rewritten — the file that would not read is settings.toml.broken",
        "the first commit did not move the launch file aside, so nothing below \
         is about a read error AFTER a move:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "settings: {} could not be read: TOML parse error at line 2, column 16",
            file.display()
        )),
        "the reopen did not read the hand edit (line 2) — it landed late, or \
         not at all, and the notice below would prove nothing:\n{stderr}"
    );
    // The stderr line, from the reopen's re-read (D31): the line-2 error
    // exists only in the file the hand edit wrote.
    assert!(
        stderr.contains(&format!(
            "fastcull: {} could not be read (TOML parse error at line 2, column 16) — \
             defaults in force",
            file.display()
        )),
        "the dialog's re-read found the file unreadable and printed no stderr line \
         naming it (settings.md, \"Reading\"; QE 2026-10-01, D31):\n{stderr}"
    );
    // The contract.
    let reread = qedump(&stderr, "reread");
    let note = dump_text(reread, "settingsnote");
    assert!(
        note.starts_with(
            "settings.toml could not be read (defaults in force): TOML parse error at line 2, \
             column 16"
        ) && note.ends_with(" — the earlier one is settings.toml.broken"),
        "the notice does not say the hand-edited file could not be read, naming the \
         earlier aside — `rewritten` masked the read error (QE 2026-10-01, D26): {note:?}"
    );
    assert!(
        dump_text(reread, "status").contains(
            "⚠ settings.toml could not be read (defaults in force) — the earlier one is \
             settings.toml.broken"
        ),
        "the status line does not say the hand-edited file could not be read, naming \
         the earlier aside (QE 2026-10-01, D26): {reread}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// settings.md, "Reading" and "Writing": a write that fails keeps the
/// commit in force in memory and says so on the notice line and stderr —
/// and while that error stands, opening the dialog does NOT re-read the
/// file, because the read would silently take the commit back; the trace
/// says no read happened instead of naming a file it did not read. The
/// write is made to fail by a read-only `settings.toml` (the file's
/// permissions on unix, its read-only attribute on Windows).
///
/// Mutants (2026-10-01): the open's `write_error.is_none()` guard taken out
/// of the bridge → the second open re-reads the file's 40 over the
/// committed 15 and `dump.reopened` reads `wash=40` — red; the open's
/// else-branch mark taken out → no `settings: not re-read` line — red
/// (senior-developer review F5 of brief 008: the open used to trace
/// `settings loaded from <path>` there).
///
/// The STATUS LINE of this plain failed write — no read error standing,
/// nothing moved aside — reads ` — ⚠ settings.toml could not be written`
/// at both dumps, and never `rewritten` nor `(defaults in force)`
/// (settings.md, "Writing"; brief 010 R2, AC23: the line had no driven
/// reader). Mutant (2026-10-03): the write-error arm of the bridge's
/// `status_note` taken out → the status line carries no settings words at
/// `dump.committed` — red.
#[test]
fn a_failed_settings_write_keeps_the_commit_and_the_next_open_does_not_reread() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let users = "[ui]\nselection_wash = 40\n";
    let dir = settings_scratch("readonly", Some(users));
    let file = dir.join("settings.toml");
    let writable = std::fs::metadata(&file).unwrap().permissions();
    let mut readonly = writable.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&file, readonly).unwrap();
    // The premise, asserted rather than assumed: a seat where this file is
    // still writable (a root shell ignores the mode) cannot make the write
    // fail, and the rest of the test would prove nothing.
    assert!(
        std::fs::OpenOptions::new().write(true).open(&file).is_err(),
        "the read-only settings.toml is still writable on this seat — the \
         write cannot be made to fail here"
    );
    let out = out_dir().join("settings-readonly.jpg");
    let script = "900:key:ctrl+,;1300:key:right;1700:click:settings wash;2000:key:ctrl+a;\
                  2200:key:1;2400:key:5;2600:key:return;3000:dump.committed;3300:key:escape;\
                  3700:key:ctrl+,;4100:dump.reopened";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out,
    );
    std::fs::set_permissions(&file, writable).unwrap();
    let committed = qedump(&stderr, "committed");
    assert_eq!(
        dump_field(committed, "wash"),
        "15",
        "the commit did not stay in force when the write failed:\n{stderr}"
    );
    assert!(
        dump_text(committed, "settingsnote").starts_with("Could not write settings.toml: "),
        "the notice does not say the write failed: {committed}"
    );
    assert!(
        stderr.contains(&format!("fastcull: could not write {}: ", file.display())),
        "no stderr line for the failed write:\n{stderr}"
    );
    let reopened = qedump(&stderr, "reopened");
    assert_eq!(
        dump_field(reopened, "wash"),
        "15",
        "the second open re-read the file's 40 over the commit the failed \
         write left only in memory:\n{stderr}"
    );
    assert!(
        dump_text(reopened, "settingsnote").starts_with("Could not write settings.toml: "),
        "the notice stopped naming the write error at the second open: {reopened}"
    );
    // The status line says the write failed, and nothing it is not
    // (settings.md, "Writing"; AC23): the commit is in force, so never
    // `(defaults in force)`, and nothing was rewritten.
    for (label, dump) in [("committed", committed), ("reopened", reopened)] {
        let status = dump_text(dump, "status");
        assert!(
            status.contains(" — ⚠ settings.toml could not be written"),
            "dump.{label}: the status line does not say the write failed: {status:?}"
        );
        assert!(
            !status.contains("rewritten") && !status.contains("defaults in force"),
            "dump.{label}: the status line claims a rewrite or the defaults beside a \
             commit in force: {status:?}"
        );
    }
    // Startup and the first open read the file; the second open did not,
    // and says so.
    assert_eq!(
        mark_lines(&stderr, "settings loaded from "),
        2,
        "`settings loaded from` was traced other than at startup and the \
         first open:\n{stderr}"
    );
    assert_eq!(
        mark_lines(
            &stderr,
            "settings: not re-read (a write failed and none has succeeded since)"
        ),
        1,
        "the second open did not say it read nothing:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        users,
        "the read-only file changed"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC6 (settings.md, "Writing" — hermetic): under FASTCULL_NO_CONFIG the
/// dialog still works, in memory — the wash applies — writes nothing, and
/// says `Not saved`.
///
/// Mutant (2026-10-01): the bridge's notice without its no-path arm →
/// `settingsnote` is empty and this goes red.
#[test]
fn settings_under_no_config_applies_in_memory_and_writes_nothing() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-noconfig.jpg");
    let script = "900:key:ctrl+,;1300:key:right;1700:click:settings wash;2000:key:ctrl+a;\
                  2200:key:1;2400:key:5;2600:key:return;3000:dump.committed";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    let committed = qedump(&stderr, "committed");
    assert_eq!(
        dump_text(committed, "settingsfile"),
        "none",
        "a settings path resolved under NO_CONFIG"
    );
    assert_eq!(
        dump_text(committed, "settingsnote"),
        "Not saved: FASTCULL_NO_CONFIG is set",
        "the dialog does not say it is not saving"
    );
    assert_eq!(
        dump_field(committed, "wash"),
        "15",
        "the commit did not apply in memory:\n{stderr}"
    );
    assert_eq!(
        dump_field(committed, "washprop"),
        "0.150",
        "the commit never reached the window:\n{stderr}"
    );
    assert!(
        stderr.contains("settings not written: FASTCULL_NO_CONFIG is set"),
        "the commit did not report that it was not written:\n{stderr}"
    );
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        0,
        "a file was written under NO_CONFIG"
    );
}

/// AC6 and brief 008 D11 (settings.md, "The file"; test-harness.md,
/// `FASTCULL_NO_CONFIG` and `FASTCULL_CONFIG_DIR`): `templates.toml` and
/// `ui.toml` are read through core's ONE config-dir resolver, the one
/// `settings.toml` uses — `FASTCULL_CONFIG_DIR` moves both, and
/// `FASTCULL_NO_CONFIG` hides both. Read off the marks each read emits from
/// the very path it used (`templates loaded from <path>`, `templates: <path>
/// could not be read: …`, `ui prefs read from <path>`; test-harness.md).
///
/// Run A points FASTCULL_CONFIG_DIR at a scratch dir holding a valid
/// templates.toml (one template) and a ui.toml remembering a Copy Picks
/// destination; `I` opens the IPTC panel (a templates read) and `Ctrl+E`
/// Copy Picks (a ui.toml read) — every such mark must name the scratch dir,
/// and each kind must appear. Run B is the same script under the harness's
/// own FASTCULL_NO_CONFIG: no read, so no mark at all.
///
/// Why marks and not behaviour (QE 2026-10-01, D37): a revert of either
/// path to the per-user dir stayed green by construction — the real config
/// dir is empty on CI and on the development seat, so a run that read it
/// looked exactly like a hermetic one, and every driven run would silently
/// have read the user's real templates.toml again.
///
/// Mutants (2026-10-01), each run with XDG_CONFIG_HOME pointed into scratch
/// so that even the mutant never reads the user's real config dir:
/// `iptc::default_templates_path` resolving the `directories` crate's
/// per-user dir itself → run A's templates mark names that dir — red;
/// `session::ui_prefs_path` taking the per-user dir directly
/// (`config_dir_from` with no environment) → run A's ui prefs mark names
/// it, and run B emits one — red.
#[test]
fn templates_and_ui_prefs_are_read_from_the_one_config_dir() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("one-resolver", None);
    let dest = dir.join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(
        dir.join("templates.toml"),
        "[templates.qe]\ntitle = \"one resolver\"\n",
    )
    .unwrap();
    // A TOML string, escaped by the TOML crate: a Windows path's
    // backslashes are escapes in a basic string.
    std::fs::write(
        dir.join("ui.toml"),
        format!(
            "copy_dest = {}\n",
            toml::Value::String(dest.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let script = "900:key:i;1400:key:ctrl+e;1900:dump.open";
    let templates = dir.join("templates.toml").display().to_string();
    let ui_prefs = dir.join("ui.toml").display().to_string();

    // A — the config dir moved: both reads name it, and nothing else.
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out_dir().join("one-resolver-moved.jpg"),
    );
    let labels = mark_labels(&stderr);
    // The path a config-read mark names, or None for any other mark.
    let named = |label: &str| -> Option<String> {
        if let Some(path) = label.strip_prefix("templates loaded from ") {
            return Some(path.to_string());
        }
        if let Some(rest) = label.strip_prefix("templates: ") {
            return Some(
                rest.split_once(" could not be read: ")
                    .map_or(rest, |(path, _)| path)
                    .to_string(),
            );
        }
        label
            .strip_prefix("ui prefs read from ")
            .map(str::to_string)
    };
    let loaded = format!("templates loaded from {templates}");
    let read = format!("ui prefs read from {ui_prefs}");
    assert!(
        labels.iter().any(|l| *l == loaded),
        "no `{loaded}` mark — the IPTC panel's templates.toml read did not go \
         through FASTCULL_CONFIG_DIR:\n{stderr}"
    );
    assert!(
        labels.iter().any(|l| *l == read),
        "no `{read}` mark — Copy Picks' ui.toml read did not go through \
         FASTCULL_CONFIG_DIR:\n{stderr}"
    );
    for label in &labels {
        if let Some(path) = named(label) {
            assert!(
                path == templates || path == ui_prefs,
                "a config read named a file outside FASTCULL_CONFIG_DIR ({}): \
                 `{label}` — that read bypassed the one resolver:\n{stderr}",
                dir.display()
            );
        }
    }

    // B — FASTCULL_NO_CONFIG (the harness's own): no read, no mark.
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out_dir().join("one-resolver-hidden.jpg"),
    );
    let reads: Vec<&str> = mark_labels(&stderr)
        .into_iter()
        .filter(|l| named(l).is_some())
        .collect();
    assert!(
        reads.is_empty(),
        "under FASTCULL_NO_CONFIG a config file was still read — {reads:?}; the \
         resolver hides templates.toml and ui.toml as it hides settings.toml:\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC7 (settings.md, "Environment precedence"): FASTCULL_MAX_READERS wins
/// over the file's `max_readers`, the field shows the variable's value and
/// cannot be changed — and an unparsable variable is ignored, the file
/// governing. Two runs over the same file (`max_readers = 7`), one per
/// value of the variable, and a third with no file at all.
///
/// The dump's `readers=` is the BRIDGE's own resolution; what the read pool
/// ADOPTED is read from the pool's mark, `read pool started floor F cap C`
/// (`Pipeline::read_pool_bounds()`), at a folder open: env 3 pins (3, 3),
/// an ignored `abc` over the file's 7 gives (4, 7), and no file gives floor
/// 4 with the core count as the cap, never pinned. Each run opens an empty
/// folder by `open:` — File › Open Folder…'s own path — before it waits on
/// that mark, because neither launch can satisfy the wait: a `--synthetic`
/// session starts no pipeline, and a folder given at launch is opened
/// before the harness registers its waits (trace.rs: "past" starts at
/// `harness::install`; measured 2026-10-01 — the launch folder's mark at
/// 0 ms, the wait never satisfied, exit 1). The third run's variable is
/// EMPTY, ignored like any unparsable value, so a shell's own
/// FASTCULL_MAX_READERS cannot leak into it.
///
/// The third run also proves the Limit field is a **Limit** only "when the
/// checkbox is off" (settings.md, "Performance › Read workers"; brief 008
/// R10): with Adaptive ticked, a click on the field and a typed `6` and
/// Enter commit nothing and `readers=` stays adaptive — and, the control in
/// the same run, the same click and keys commit `max_readers = 6` once
/// Adaptive is cleared, so the inert half cannot pass because the click
/// resolved to nothing or the field is gone. It is a click on purpose: the
/// Tab ring never lands on the field while Adaptive is ticked (its own
/// `slot-ok` skips the slot), so no keyboard test can see the gate. Green
/// on a377405, a guard and not a bug fix (the senior developer measured
/// `readers=adaptive`, no commit, no file, `focusowner=-1`: the click fell
/// through to the dialog's scope, a disabled LineEdit being a disabled
/// TextInput, widgets/common/lineedit-base.slint:11). That run's scratch
/// dir now receives a written file (`= 4`, then `= 6`); the byte-equality
/// check above concerns the OTHER dir.
///
/// The locked run also reads the row's NOTE and clicks its Adaptive box
/// (QE 2026-10-02, round 5: AC7's "and its note" had no reader, and the
/// box's lock no guard — with the note presented empty, or the lock taken
/// out of the box, the whole suite stayed green). The note is read from the
/// mark its own Text emits when it is created, `settings note readers-env
/// shows <text>` — once, before `dump.perf` — and compared with core's
/// `environment_note`, whose wording `the_environment_note_is_the_specs_sentence`
/// pins to the spec's sentence (this test, comparing with the same
/// function, cannot see a reworded note; core can). The `abc` run, its
/// variable ignored, shows no such line. And the Adaptive box is locked like
/// the field: a click on it commits nothing — no `settings committed
/// performance.max_readers`, no `settings written` — and the box does not
/// end up flipped (no `settings readers-adaptive shows` after the click).
/// It is a click because the Tab ring never lands on the locked box (its
/// `slot-ok` skips the slot). `readers=env:3` at `dump.typed` is the
/// PREMISE, not the guard: the dump resolves the environment over whatever
/// the model holds, so it reads `env:3` even after a click that rewrote the
/// file — what turns red is the commit, the write and the file.
///
/// A fourth run proves the environment never reaches the file (settings.md,
/// "Writing"; the user, 2026-10-02, brief 008 D42: "make sure that
/// environment variables don't rewrite settings"): under
/// FASTCULL_MAX_READERS=3, over its own file holding `max_readers = 7`, a
/// commit of ANOTHER setting (the wash, 15) is saved with the file's own
/// `max_readers = 7` beside it, never the 3 in force. Green before the
/// commit that adds it — a guard. Mutants (2026-10-02): core's writer
/// emitting the value in force for `max_readers` → the file reads
/// `max_readers = 3` — red; the bridge's `save` writing a copy whose
/// `max_readers` is the value in force → the same — red (the core test
/// `the_environment_never_reaches_the_settings_file` cannot see this one).
///
/// Mutants (2026-10-01): `resolve_max_readers` ignoring the environment →
/// the first run reads `readers=limit:7` and this goes red; session.rs
/// passing `None` to `Pipeline::start` with the mark untouched → the pool
/// starts adaptive, the first run's wait is never satisfied, the run exits
/// 1 and this goes red (QE 2026-10-01, D27: until the mark read the pool,
/// nothing did, and that mutant stayed green). Mutant (2026-10-02, QE round
/// 4's G2): the `!root.settings-readers-adaptive &&` term dropped from the
/// Limit field's `enabled` → the first click focuses the field, `6` and
/// Enter commit, and `dump.limitoff` reads `readers=limit:6` — red.
/// Mutants (2026-10-02, QE round 5), each alone: the bridge's `present`
/// writing an empty environment note → no `settings note readers-env shows`
/// mark — red (traced 0 times); the warning Text taken out of `SettingRow`
/// → the same — red; `enabled: !root.settings-readers-locked` taken out of
/// the Adaptive box → the click commits `max_readers = 0`, `settings
/// written` follows and the file reads `max_readers = 0` — red at the
/// commit. Under that mutant no `shows` mark fires either way: the box goes
/// false → true → false inside the click's one event-loop iteration (the
/// commit re-presents the environment's state through `<=>`), which a
/// `changed` handler cannot see (Cargo.toml, the fourth canary's fact 5) —
/// so the `shows` check holds the box to its state only against a flip that
/// STAYS, and the commit, the write and the file are the lock's guards.
///
/// The locked field SHOWS the environment's value (settings.md, AC7 —
/// "read-only with the environment's value"; brief 010, issue #100's third
/// NOW guard): the Limit field's own creation mark, `settings readers-limit
/// shows <text>` (test-harness.md), is the last before `dump.perf` and reads
/// `3` in the first run, where the file says 7 — and `7` in the `abc` run,
/// the variable ignored. The dump's `readers=` is the bridge's resolution,
/// which cannot see what the field displays. Mutant (2026-10-03): the
/// `Environment(n)` arm of the bridge's `present` showing the file's value
/// (`s.max_readers`) → the first run's field shows `7` — red.
#[test]
fn the_environment_wins_over_the_settings_file_for_read_workers() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("readers", Some("[performance]\nmax_readers = 7\n"));
    let photos = out_dir().join("settings-readers-photos");
    std::fs::remove_dir_all(&photos).ok();
    std::fs::create_dir_all(&photos).unwrap();
    let open_folder = format!("300:open:{}", photos.display());
    let open = "900:key:ctrl+,;1300:key:ctrl+tab;1500:key:ctrl+tab;1900:dump.perf";
    let run = |config: &Path, env: &str, script: &str, shot: &str| {
        shoot_env_stderr(
            &["--synthetic", "24"],
            &[
                ("FASTCULL_TRACE", "1"),
                ("FASTCULL_CONFIG_DIR", config.to_str().unwrap()),
                ("FASTCULL_MAX_READERS", env),
                ("FASTCULL_DRIVE", script),
            ],
            &out_dir().join(shot),
        )
    };
    // The locked run also tries to type a limit into the field, and then
    // clicks the Adaptive box.
    let typing = format!(
        "{open_folder};600:wait:read pool started floor 3 cap 3;{open};\
         2200:click:settings readers-limit;2500:key:ctrl+a;2700:key:9;\
         2900:key:return;3200:click:settings readers-adaptive;3600:dump.typed"
    );
    let stderr = run(&dir, "3", &typing, "settings-readers-env.jpg");
    assert!(
        stderr.contains("wait:read pool started floor 3 cap 3 (satisfied"),
        "the read pool did not adopt FASTCULL_MAX_READERS=3 as floor 3 cap 3 — the \
         environment never reached the pool (QE 2026-10-01, D27):\n{stderr}"
    );
    let perf = qedump(&stderr, "perf");
    assert_eq!(
        dump_field(perf, "settingstab"),
        "2",
        "not on the Performance tab:\n{stderr}"
    );
    assert_eq!(
        dump_field(perf, "readers"),
        "env:3",
        "the file's 7 governs although FASTCULL_MAX_READERS=3 is set:\n{stderr}"
    );
    assert_eq!(dump_text(perf, "readersenv"), "3");
    // The environment's note is ON SCREEN, core's sentence byte for byte:
    // the row's own line reports itself when it is created, once per open.
    let labels = mark_labels(&stderr);
    let perf_at = labels
        .iter()
        .position(|l| *l == "drive: dump.perf")
        .unwrap_or_else(|| panic!("no `drive: dump.perf` step:\n{stderr}"));
    let note = format!(
        "settings note readers-env shows {}",
        fastcull_core::settings::environment_note(fastcull_core::settings::MAX_READERS_VAR)
    );
    let shown = labels[..perf_at].iter().filter(|l| **l == note).count();
    assert_eq!(
        shown, 1,
        "the read workers row did not show the environment's note once before \
         `dump.perf` — `{note}` traced {shown} time(s) (settings.md, \"Environment \
         precedence\"):\n{stderr}"
    );
    // What the locked Limit field DISPLAYS: its own last mark before the dump
    // — the environment's 3, never the file's 7 (AC7).
    let limit_shown = |labels: &[&str], at: usize| -> Option<String> {
        labels[..at]
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix("settings readers-limit shows "))
            .map(str::to_string)
    };
    assert_eq!(
        limit_shown(&labels, perf_at).as_deref(),
        Some("3"),
        "the read-only Limit field does not SHOW the environment's value — its last \
         `settings readers-limit shows` before `dump.perf` (None: it reported \
         nothing) is not `3` while FASTCULL_MAX_READERS=3 governs over the file's 7 \
         (settings.md, \"Environment precedence\"):\n{stderr}"
    );
    let typed = qedump(&stderr, "typed");
    assert_eq!(
        dump_field(typed, "readers"),
        "env:3",
        "the read-only field took a value while the environment governs it:\n{stderr}"
    );
    // The Adaptive box is locked too: its click resolved on the box, and
    // the box neither flipped nor committed anything.
    assert_click_resolved(&stderr, "settings readers-adaptive");
    assert!(
        !labels
            .iter()
            .any(|l| l.starts_with("settings committed performance.max_readers")),
        "a read workers control committed while FASTCULL_MAX_READERS governs the row \
         — the file's own `max_readers = 7` would be rewritten:\n{stderr}"
    );
    let clicked = labels
        .iter()
        .rposition(|l| *l == "drive: click:settings readers-adaptive")
        .unwrap_or_else(|| panic!("no click on the Adaptive box:\n{stderr}"));
    assert!(
        !labels[clicked..]
            .iter()
            .any(|l| l.starts_with("settings readers-adaptive shows ")),
        "the Adaptive box flipped under a click while FASTCULL_MAX_READERS governs \
         the row:\n{stderr}"
    );
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        0,
        "a read-only field wrote the file"
    );

    let stderr = run(
        &dir,
        "abc",
        &format!("{open_folder};600:wait:read pool started floor 4 cap 7;{open}"),
        "settings-readers-abc.jpg",
    );
    assert!(
        stderr.contains("wait:read pool started floor 4 cap 7 (satisfied"),
        "the read pool did not adopt the file's max_readers = 7 as floor 4 cap 7 \
         under an unparsable FASTCULL_MAX_READERS — the file never reached the \
         pool (QE 2026-10-01, D27):\n{stderr}"
    );
    let perf = qedump(&stderr, "perf");
    assert_eq!(
        dump_field(perf, "readers"),
        "limit:7",
        "an unparsable FASTCULL_MAX_READERS was not ignored — the file must govern:\n{stderr}"
    );
    assert_eq!(dump_text(perf, "readersenv"), "abc");
    assert!(
        !mark_labels(&stderr)
            .iter()
            .any(|l| l.starts_with("settings note readers-env shows ")),
        "the environment's note is on screen though the variable is ignored:\n{stderr}"
    );
    // The variable ignored, the field shows the file's 7.
    let labels = mark_labels(&stderr);
    let perf_at = labels
        .iter()
        .position(|l| *l == "drive: dump.perf")
        .unwrap_or_else(|| panic!("no `drive: dump.perf` step in the abc run:\n{stderr}"));
    assert_eq!(
        limit_shown(&labels, perf_at).as_deref(),
        Some("7"),
        "under an ignored FASTCULL_MAX_READERS the Limit field does not show the file's \
         7 before `dump.perf`:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("settings.toml")).unwrap(),
        "[performance]\nmax_readers = 7\n",
        "the file changed: the only run that typed was the locked one"
    );

    // No file, the variable ignored: the pool is adaptive — floor 4, the
    // cap the core count (never pinned). Then the Limit field, inert while
    // Adaptive is ticked, and the control: live once it is cleared.
    let none = settings_scratch("readers-none", None);
    let limit = format!(
        "{open_folder};900:key:ctrl+,;1300:key:ctrl+tab;1500:key:ctrl+tab;\
         1900:click:settings readers-limit;2200:key:6;2400:key:return;2800:dump.limitoff;\
         3100:click:settings readers-adaptive;3500:click:settings readers-limit;\
         3800:key:ctrl+a;4000:key:6;4200:key:return;4600:dump.limiton;4900:key:escape"
    );
    let stderr = run(&none, "", &limit, "settings-readers-none.jpg");
    assert!(
        stderr.contains("] read pool started floor 4 cap "),
        "with no settings file the read pool did not start adaptive (floor 4) \
         (QE 2026-10-01, D27):\n{stderr}"
    );
    // Both clicks on the field resolved inside it: the last through the
    // shared check, the first by its own echo against the rectangle the
    // app reported before it.
    assert_click_resolved(&stderr, "settings readers-limit");
    assert_click_resolved(&stderr, "settings readers-adaptive");
    let first = stderr
        .lines()
        .find(|l| l.ends_with("] drive: click:settings readers-limit"))
        .unwrap_or_else(|| panic!("no first click on the Limit field:\n{stderr}"));
    let (x, y, w, h) = laid_out_rect(&stderr, "settings readers-limit", first);
    let echo = stderr
        .split_once(first)
        .and_then(|(_, after)| {
            after.lines().find(|l| {
                l.contains("] drive ptr click ") && l.ends_with(" (settings readers-limit)")
            })
        })
        .and_then(|l| l.split_once("drive ptr click "))
        .and_then(|(_, at)| at.split_once(' '))
        .and_then(|(xy, _)| xy.split_once(','))
        .and_then(|(cx, cy)| Some((cx.parse::<f32>().ok()?, cy.parse::<f32>().ok()?)));
    assert!(
        echo.is_some_and(|(cx, cy)| cx >= x && cx <= x + w && cy >= y && cy <= y + h),
        "the first click on the Limit field did not resolve inside it ({echo:?} against \
         {x},{y} {w}x{h}) — the inert half below would prove nothing:\n{stderr}"
    );
    let labels = mark_labels(&stderr);
    let limitoff = labels
        .iter()
        .position(|l| *l == "drive: dump.limitoff")
        .unwrap_or_else(|| panic!("no `drive: dump.limitoff` step:\n{stderr}"));
    assert_eq!(
        dump_field(qedump(&stderr, "limitoff"), "readers"),
        "adaptive",
        "with Adaptive ticked the Limit field took a limit — it is a Limit only when \
         the checkbox is off (settings.md, R10; QE round 4, D41):\n{stderr}"
    );
    assert!(
        !labels[..limitoff]
            .iter()
            .any(|l| l.starts_with("settings committed performance.max_readers")),
        "with Adaptive ticked a click and `6`, Enter on the Limit field committed a \
         limit:\n{stderr}"
    );
    // The control: the same click and keys, Adaptive cleared, commit.
    assert_eq!(
        dump_field(qedump(&stderr, "limiton"), "readers"),
        "limit:6",
        "with Adaptive cleared the same click and `6`, Enter did not commit a limit of \
         6 — the inert half above proves nothing:\n{stderr}"
    );
    assert!(
        labels[limitoff..].contains(&"settings committed performance.max_readers = 6"),
        "no `settings committed performance.max_readers = 6` after `dump.limitoff`:\n{stderr}"
    );

    // The environment never reaches the file (settings.md, "Writing"; the
    // user, brief 008 D42): under FASTCULL_MAX_READERS=3, a commit of
    // ANOTHER setting saves the file's own `max_readers = 7`, never the 3
    // in force. Its own dir: the first run's file must stay byte-identical.
    let saved = settings_scratch("readers-env-save", Some("[performance]\nmax_readers = 7\n"));
    let stderr = run(
        &saved,
        "3",
        "900:key:ctrl+,;1300:key:right;1700:click:settings wash;2000:key:ctrl+a;2200:key:1;\
         2400:key:5;2600:key:return;3000:dump.saved",
        "settings-readers-env-save.jpg",
    );
    assert_click_resolved(&stderr, "settings wash");
    let dump = qedump(&stderr, "saved");
    assert_eq!(
        dump_field(dump, "readers"),
        "env:3",
        "the premise: FASTCULL_MAX_READERS=3 governs what is in force:\n{stderr}"
    );
    assert_eq!(
        dump_field(dump, "wash"),
        "15",
        "the other setting's commit did not apply:\n{stderr}"
    );
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        1,
        "the commit was not saved, once:\n{stderr}"
    );
    let text = std::fs::read_to_string(saved.join("settings.toml")).unwrap();
    assert!(
        text.lines().any(|line| line.trim() == "max_readers = 7")
            && text
                .lines()
                .any(|line| line.trim() == "selection_wash = 15"),
        "the save did not keep the file's own `max_readers = 7` beside the commit — \
         the environment reached the file: {text:?}"
    );
    assert!(
        !text.lines().any(|line| line.trim() == "max_readers = 3"),
        "the environment's value was written to the file: {text:?}"
    );
    std::fs::remove_dir_all(&saved).ok();
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&none).ok();
    std::fs::remove_dir_all(&photos).ok();
}

/// AC8 (settings.md, "General › Auto-advance"; brief 008 D7): with
/// auto-advance off, `Y` marks and the cursor STAYS, the selection left
/// alone exactly as `U` would; under a filter that the mark takes the frame
/// out of, the live-removal rule moves the cursor and that move ends the
/// selection; turned back on, `Y` advances as it always has.
///
/// Mutant (2026-10-01): `advance` in the mark handler ignoring the setting
/// → the first `Y` advances the cursor and collapses the selection, and
/// this goes red.
#[test]
fn auto_advance_off_keeps_the_cursor_and_the_selection_like_u() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-autoadvance.jpg");
    let script = "900:key:ctrl+space;1100:key:ctrl+right;1300:key:ctrl+space;1600:dump.sel;\
                  1900:key:ctrl+,;2400:click:settings auto-advance;2800:key:escape;3200:dump.off;\
                  3500:key:y;3900:dump.y;4200:filter:unmarked;4600:dump.filtered;\
                  4900:key:y;5300:dump.removed;5600:filter:all;5900:key:ctrl+,;\
                  6400:click:settings auto-advance;6800:key:escape;7100:key:ctrl+space;\
                  7300:key:ctrl+right;7500:key:ctrl+space;7800:dump.on;8100:key:y;8500:dump.advanced";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    let sel = qedump(&stderr, "sel");
    assert_eq!(
        dump_field(sel, "selected"),
        "2",
        "the seed selection did not build:\n{stderr}"
    );
    assert_eq!(dump_field(sel, "cursor"), "1");
    let off = qedump(&stderr, "off");
    assert_eq!(
        dump_field(off, "autoadvance"),
        "false",
        "auto-advance was not turned off:\n{stderr}"
    );
    assert_eq!(
        dump_field(off, "selected"),
        "2",
        "opening Settings ended the selection:\n{stderr}"
    );
    let y = qedump(&stderr, "y");
    assert!(
        dump_text(y, "status").contains("★1 ✕0"),
        "the Y did not mark: {y}"
    );
    assert_eq!(
        dump_field(y, "cursor"),
        "1",
        "with auto-advance off the Y moved the cursor:\n{stderr}"
    );
    assert_eq!(
        dump_field(y, "selected"),
        "2",
        "with auto-advance off the Y ended the selection — it must leave it \
         alone exactly as U does:\n{stderr}"
    );
    // Under Unmarked the picked frame left the view and the filter change
    // moved the cursor to a survivor (an engine move: the selection stays).
    let filtered = qedump(&stderr, "filtered");
    let survivor = dump_field(filtered, "cursor").to_string();
    assert_ne!(
        survivor, "1",
        "the filter change kept the cursor on a hidden frame:\n{stderr}"
    );
    let removed = qedump(&stderr, "removed");
    assert_ne!(
        dump_field(removed, "cursor"),
        survivor,
        "the Y took the frame out of the view and the cursor did not move on \
         (the live-removal exception):\n{stderr}"
    );
    assert_eq!(
        dump_field(removed, "selected"),
        "0",
        "the live-removal move did not end the selection:\n{stderr}"
    );
    let on = qedump(&stderr, "on");
    assert_eq!(
        dump_field(on, "autoadvance"),
        "true",
        "auto-advance did not come back on:\n{stderr}"
    );
    let before = dump_field(on, "cursor").parse::<u32>().unwrap();
    let advanced = qedump(&stderr, "advanced");
    assert_eq!(
        dump_field(advanced, "cursor").parse::<u32>().unwrap(),
        before + 1,
        "with auto-advance on the Y did not advance:\n{stderr}"
    );
    assert_eq!(
        dump_field(advanced, "selected"),
        "0",
        "the advance did not end the selection:\n{stderr}"
    );
}

/// AC30 (settings.md, "General › Auto-advance": on, `Y`/`N` moves the
/// cursor "at every zoom"; off, the cursor STAYS; brief 010 R3 — a launch of
/// its own, the integrity review's shape): IN THE LOUPE AT 1:1, on a real
/// folder of three distinct frames (`--start-11`, the full-res of the first
/// on screen before anything is pressed — `wait:loupe idx 0 factor`, after
/// the settle). Auto-advance turned off in the dialog, `Y` marks the frame
/// and the cursor stays — `★1` on the status line, the same cursor, still at
/// 1:1 (`one2one=true`, `zf=inf`); turned back on, `Y` advances to the next
/// frame, still at 1:1 (`zf=inf`, the 1:1 desire carried; the shutter of a
/// `--start-11` run then waits for that frame's full-res).
///
/// The mark path has no zoom branch today, so its only red mutant is the
/// grid test's (`auto_advance_off_keeps_the_cursor_and_the_selection_like_u`):
/// this pins "at every zoom" against a future one. Mutant (2026-10-03):
/// `advance` in the mark handler ignoring the setting (`key != "clear"`
/// alone, nav.rs) → the first `Y` moves the cursor at 1:1 — red.
#[test]
fn auto_advance_off_holds_the_cursor_in_the_loupe_at_one_to_one() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("settings-autoadvance-one2one");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    place_three_distinct(&dir);
    let out = out_dir().join("settings-autoadvance-one2one.jpg");
    let script = "1500:wait:load settled gen 0;1600:wait:loupe idx 0 factor;\
                  2000:key:ctrl+,;2400:click:settings auto-advance;2800:key:escape;\
                  3200:dump.before;3500:key:y;3900:dump.held;4200:key:ctrl+,;\
                  4600:click:settings auto-advance;5000:key:escape;5400:dump.on;\
                  5700:key:y;6100:dump.advanced";
    let stderr = shoot_env_stderr(
        &["--start-11", dir.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    for wait in [
        "wait:load settled gen 0 (satisfied",
        "wait:loupe idx 0 factor (satisfied",
    ] {
        assert!(stderr.contains(wait), "`{wait}` never fired:\n{stderr}");
    }
    assert_click_resolved(&stderr, "settings auto-advance");
    // The premises: auto-advance off, at 1:1, the dialog closed.
    let before = qedump(&stderr, "before");
    assert!(
        dump_field(before, "autoadvance") == "false"
            && dump_field(before, "settings") == "false"
            && dump_field(before, "one2one") == "true"
            && dump_field(before, "zf") == "inf",
        "the premise: not at 1:1 with auto-advance off and the dialog closed: {before}"
    );
    let cursor = dump_field(before, "cursor");
    // Off: Y marks, and the cursor stays at 1:1.
    let held = qedump(&stderr, "held");
    assert!(
        dump_text(held, "status").contains("★1 ✕0"),
        "the Y at 1:1 did not mark: {held}"
    );
    assert_eq!(
        dump_field(held, "cursor"),
        cursor,
        "WITH AUTO-ADVANCE OFF THE Y MOVED THE CURSOR AT 1:1 — off, the cursor stays on \
         the frame marked, at every zoom (settings.md, \"General › Auto-advance\"):\n{stderr}"
    );
    assert!(
        dump_field(held, "one2one") == "true" && dump_field(held, "zf") == "inf",
        "the Y with auto-advance off left 1:1: {held}"
    );
    // On: Y advances, at 1:1 too.
    let on = qedump(&stderr, "on");
    assert!(
        dump_field(on, "autoadvance") == "true" && dump_field(on, "cursor") == cursor,
        "the premise: auto-advance not back on, or the cursor moved before the Y: {on}"
    );
    let advanced = qedump(&stderr, "advanced");
    assert_ne!(
        dump_field(advanced, "cursor"),
        cursor,
        "with auto-advance on the Y at 1:1 did not advance:\n{stderr}"
    );
    assert!(
        dump_text(advanced, "status").contains("(2/3)"),
        "the advance at 1:1 did not land on the next frame of the three: {advanced}"
    );
    assert_eq!(
        dump_field(advanced, "zf"),
        "inf",
        "the advance left 1:1 — the 1:1 desire is carried to the next frame: {advanced}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC10 (settings.md, "Performance › Loupe memory"): the figure committed in
/// the dialog shows its hint at once and reaches the loupe engine at the
/// NEXT folder open — the same folder is fine. The wait is on the engine's
/// own mark with the exact budget, so a run whose engine started with any
/// other number never satisfies it and fails loudly at the wait's cap.
///
/// And it reaches the RING (raw-pipeline.md, "The ring fits the budget";
/// brief 012 AC4): `Z` in the 0.5 GB session is the session's first loupe
/// focus, at 1:1; the first header the engine parses sizes the ring, and
/// the `loupe ring` mark must name the window three A1 frames allow — the
/// focused frame and its two nearest neighbours, `rest 1/1` — with the held
/// arrow's ring of mids left whole, `transit 2/8`. The mark comes from the
/// engine's own report, never from the app (brief 012 D5).
///
/// Mutants: (2026-10-01) `session.rs` starting the engine with
/// `DEFAULT_BUDGET_BYTES` again → the second mark says 2147483648, the wait
/// is never satisfied and the run exits 1 — red; (2026-10-05) the cap
/// bypassed in `ring_within_budget` → the mark says `rest 2/2`, the ring
/// wait is never satisfied and the run exits 1 — red; the pump never
/// printing the mark → the same.
#[test]
fn loupe_memory_takes_effect_at_the_next_folder_open() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("settings-loupe");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("settings-loupe.jpg");
    // 0.5 GiB holds three decoded A1 frames (149,299,200 bytes each, a
    // fixture here: the engine learns the size from the header).
    let ring = "loupe ring budget 536870912 frame 149299200 rest 1/1 transit 2/8";
    let script = format!(
        "1500:wait:load settled gen 0;1600:key:ctrl+,;1900:key:ctrl+tab;2100:key:ctrl+tab;\
         2500:click:settings loupe-memory;2800:key:ctrl+a;3000:key:0;3200:key:.;3400:key:5;\
         3600:key:return;4000:dump.set;4200:key:escape;4500:open:{dir};\
         4600:wait:loupe engine started budget 536870912;4800:key:z;5000:wait:{ring};\
         5200:dump.after",
        dir = dir.display()
    );
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    assert!(
        stderr.contains("loupe engine started budget 2147483648"),
        "the first folder did not start the engine with the 2 GB default:\n{stderr}"
    );
    assert!(
        stderr.contains("wait:loupe engine started budget 536870912 (satisfied"),
        "the engine never started with the committed 0.5 GB:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("wait:{ring} (satisfied")),
        "the 0.5 GB budget never reached the loupe's prefetch ring — no `{ring}` \
         mark after the session's first 1:1 focus (raw-pipeline.md, \"The ring fits \
         the budget\"):\n{stderr}"
    );
    let wide: Vec<&str> = mark_labels(&stderr)
        .into_iter()
        .filter(|l| l.starts_with("loupe ring budget 536870912 ") && l.contains(" rest 2/2 "))
        .collect();
    assert!(
        wide.is_empty(),
        "at 0.5 GB the engine reported the whole ±2 window for full frames, \
         which the budget cannot hold: {wide:?}"
    );
    assert_click_resolved(&stderr, "settings loupe-memory");
    let set = qedump(&stderr, "set");
    assert_eq!(
        dump_field(set, "loupemem"),
        "536870912",
        "0.5 GB is not 2^29 bytes in force:\n{stderr}"
    );
    let hint = dump_text(set, "loupehint");
    assert!(
        hint.starts_with("= 512.0 MB") && hint.contains("≈ 3 A1 frames"),
        "the hint does not show the figure in force and the A1 frames: {hint:?}"
    );
}

/// AC12, the cache off (settings.md, "Performance › Thumbnail cache"; brief
/// 008 D13): under FASTCULL_NO_CACHE — the harness's own — the row says the
/// cache is off and Clear does nothing. The live clear is
/// `the_cache_cap_and_clear_cache_reach_the_default_cache`'s, on Linux, and
/// core's (`cache::tests::clear_leaves_the_file_present_…`); this sentence
/// called the worker, the `Clearing…` state and the re-measured readout
/// review-verified until brief 010, though that test had driven them since
/// QE's round 5 of brief 008.
///
/// And the button is DISABLED (brief 010, issue #100's second NOW guard;
/// QE 2026-10-02, SC-4: until then only the row's text and the absence of a
/// clear were read): its own mark, `settings clear-cache enabled false`
/// from its creation, is the last before `dump.perf`, and none reads `true`
/// from the open on.
///
/// Mutants: (2026-10-01) the readout ignoring FASTCULL_NO_CACHE → the row
/// shows the real cache's size (a stat, nothing more) and this goes red;
/// (2026-10-03) the bridge's `present` enabling Clear on `clear_rx.is_none()`
/// alone, the cache-off term dropped → `settings clear-cache enabled true` at
/// the open — red (the click then still clears nothing: the handler's own
/// guard holds, so only the button's mark sees it).
#[test]
fn clear_cache_is_off_under_no_cache() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-clear-off.jpg");
    let script = "900:key:ctrl+,;1300:key:ctrl+tab;1500:key:ctrl+tab;1900:dump.perf;\
                  2200:click:settings clear-cache;2700:dump.clicked";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    let off = "Thumbnail cache: off (FASTCULL_NO_CACHE is set)";
    assert_eq!(dump_text(qedump(&stderr, "perf"), "cachereadout"), off);
    assert_click_resolved(&stderr, "settings clear-cache");
    assert_eq!(dump_text(qedump(&stderr, "clicked"), "cachereadout"), off);
    assert_eq!(
        mark_lines(&stderr, "settings cache cleared"),
        0,
        "Clear ran with the cache off:\n{stderr}"
    );
    // The button's own state (AC12): disabled from the open, never offered.
    let labels = mark_labels(&stderr);
    let opened = labels
        .iter()
        .position(|l| *l == "drive: key:ctrl+,")
        .unwrap_or_else(|| panic!("no `drive: key:ctrl+,` step:\n{stderr}"));
    let perf_at = labels
        .iter()
        .position(|l| *l == "drive: dump.perf")
        .unwrap_or_else(|| panic!("no `drive: dump.perf` step:\n{stderr}"));
    let enabled = labels[opened..perf_at]
        .iter()
        .rev()
        .find_map(|l| l.strip_prefix("settings clear-cache enabled "));
    assert_eq!(
        enabled,
        Some("false"),
        "the Clear button is not DISABLED with the cache off — its last `settings \
         clear-cache enabled` mark before `dump.perf` (None: it reported nothing) \
         (settings.md, \"Performance › Thumbnail cache\"):\n{stderr}"
    );
    assert!(
        !labels[opened..].contains(&"settings clear-cache enabled true"),
        "the Clear button was offered with the cache off:\n{stderr}"
    );
}

/// `hover:<element>` landed inside the rectangle the app reported for the
/// element — `assert_click_resolved`'s check for a hover: the echo is the
/// first `drive ptr hover … (<element>)` after the step, and the rectangle
/// the last one reported before it.
fn assert_hover_resolved(stderr: &str, element: &str) {
    let step = format!("] drive: hover:{element}");
    let step_line = stderr
        .lines()
        .rfind(|l| l.ends_with(&step))
        .unwrap_or_else(|| panic!("no `drive: hover:{element}` step in the trace:\n{stderr}"));
    let after = stderr
        .rfind(step_line)
        .map(|at| &stderr[at + step_line.len()..])
        .unwrap_or("");
    let tag = format!(" ({element})");
    let line = after
        .lines()
        .find(|l| l.contains("] drive ptr hover ") && l.ends_with(&tag))
        .unwrap_or_else(|| panic!("the hover:{element} step never resolved:\n{stderr}"));
    let point = || -> Option<(f32, f32)> {
        let at = line.split_once("drive ptr hover ")?.1;
        let (x, y) = at.split_once(' ')?.0.split_once(',')?;
        Some((x.parse().ok()?, y.parse().ok()?))
    };
    let (px, py) = point().unwrap_or_else(|| panic!("malformed hover echo: {line:?}"));
    let (x, y, w, h) = laid_out_rect(stderr, element, step_line);
    assert!(
        px >= x && px <= x + w && py >= y && py <= y + h,
        "the hover resolved to ({px}, {py}), outside the {element} rectangle \
         (x {x}..{}, y {y}..{}):\n{stderr}",
        x + w,
        y + h
    );
}

/// AC13 (settings.md; ui-grid.md, "Visual language" — promised since M2,
/// built in brief 008): the Failed badge shows its reason on HOVER —
/// Slint's built-in tooltip raised by a real pointer move onto the badge,
/// read from the `failed tooltip shown:` mark its popup emits when it is
/// created — and the status line carries the same words while the cursor
/// stands on the failed frame, and none while it stands on a healthy one.
/// The reason is the pipeline's own, asked of core for the same bytes.
///
/// The cursor starts on `broken.ARW`: it is first in name order, and the
/// load-settled re-sort that puts it last (no capture time) does not move
/// an untouched cursor once the folder has loaded (issue #25).
///
/// Mutant (2026-10-01): the pump recording a failure without its reason
/// (`or_insert(String::new())`) → the status reads `⚠ failed: ` with
/// nothing after it and this goes red.
#[test]
fn the_failed_badge_shows_its_reason_on_hover_and_in_the_status_line() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("failed-tooltip");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("good.ARW"),
    );
    let broken = dir.join("broken.ARW");
    std::fs::write(&broken, vec![0xAB; 2048]).unwrap();
    // The words the pipeline gives this file — the same function its
    // workers run (`process_job` sends exactly this string as Failed).
    let reason = fastcull_core::pipeline::make_grid_thumb(&fastcull_core::pipeline::JobSpec {
        path: broken.clone(),
        size: 2048,
        mtime: None,
    })
    .expect_err("2 KB of 0xAB is not a RAW");
    let out = out_dir().join("failed-tooltip.jpg");
    // The tail clicks the badge itself with the cursor elsewhere: the
    // tooltip's hover tracker must take no press (it observes and forwards,
    // i-slint-core's `TooltipArea`), so the cell under it still claims the
    // cursor — the pointer contract's click, untouched by the tooltip.
    let script = "1500:wait:load settled gen 0;1600:dump.broken;1900:key:left;2200:dump.good;\
                  2500:key:right;2800:dump.again;3100:hover:failed badge 0;\
                  3200:wait:failed tooltip shown:;3600:dump.tip;3800:key:left;\
                  4100:click:failed badge 0;4500:dump.clicked";
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out,
    );
    let words = format!("broken.ARW (2/2) · unmarked · ⚠ failed: {reason}");
    for label in ["broken", "again"] {
        let status = dump_text(qedump(&stderr, label), "status").to_string();
        assert!(
            status.starts_with(&words),
            "dump.{label}: the status line does not name the failed cursor's \
             reason after its mark words — wanted {words:?}, got {status:?}"
        );
    }
    let good = dump_text(qedump(&stderr, "good"), "status").to_string();
    assert!(
        good.starts_with("good.ARW (1/2)") && !good.contains("failed:"),
        "on the healthy frame the status line still talks of a failure: {good:?}"
    );
    assert_hover_resolved(&stderr, "failed badge 0");
    assert!(
        stderr.contains("wait:failed tooltip shown: (satisfied"),
        "the hover never raised the badge's tooltip:\n{stderr}"
    );
    // Every time the popup was created it carried the pipeline's words
    // (the click at the tail raises it once more: a click begins with a
    // pointer move, which is a hover).
    let shown = mark_lines(&stderr, "failed tooltip shown: ");
    assert!(shown >= 1, "the tooltip never showed:\n{stderr}");
    assert_eq!(
        mark_lines(&stderr, &format!("failed tooltip shown: {reason}")),
        shown,
        "the tooltip did not show the pipeline's reason:\n{stderr}"
    );
    assert_click_resolved(&stderr, "failed badge 0");
    let clicked = dump_text(qedump(&stderr, "clicked"), "status").to_string();
    assert!(
        clicked.starts_with(&words),
        "a click on the badge did not reach the cell under it — the \
         tooltip's hover tracker took the press: {clicked:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The luma variance inside a window-logical rectangle `(x, y, w, h)` of a
/// shot taken at scale factor 1 — the fraction `region_stats` wants is the
/// rectangle over the shot's own size.
fn rect_variance(shot: &Path, (x, y, w, h): (f32, f32, f32, f32)) -> f64 {
    let (sw, sh, _) = analyze(shot);
    assert_eq!(
        sw, 1440,
        "rect_variance assumes scale factor 1 (a 1440 px shot of the 1440 px window); got {sw} px"
    );
    let (sw, sh) = (sw as f64, sh as f64);
    let (x, y, w, h) = (f64::from(x), f64::from(y), f64::from(w), f64::from(h));
    region_stats(shot, x / sw, y / sh, (x + w) / sw, (y + h) / sh).1
}

/// How many pixel ROWS of a window-logical rectangle `(x, y, w, h)` of a
/// shot taken at scale factor 1 read as blue: rows whose mean (B − R)
/// across the rectangle's width is above `floor` — the blue bias
/// [`region_blue_bias`] measures, one row at a time.
///
/// Rows are counted, never averaged into one band, because a `laid out at`
/// mark is its position rounded to whole px and the pixels can sit a row
/// off it (test-harness.md, "Layout"): a 3 px band flush with a mark's edge
/// held both rows of a 2 px line on one runner and one of them on another,
/// where a window that straddles the edge holds the same rows on both.
fn blue_rows(shot: &Path, (x, y, w, h): (f32, f32, f32, f32), floor: f64) -> usize {
    let bytes = std::fs::read(shot).expect("snapshot file");
    let mut dec = zune_jpeg::JpegDecoder::new(&bytes);
    let px = dec.decode().expect("decode snapshot");
    let (sw, sh) = dec.dimensions().expect("dims");
    assert_eq!(
        sw, 1440,
        "blue_rows assumes scale factor 1 (a 1440 px shot of the 1440 px window); got {sw} px"
    );
    // The marks print whole px (`{:.0}`), so these casts drop nothing; a
    // rectangle past the shot's floor is cut at it.
    let (x0, x1) = (x as usize, ((x + w) as usize).min(sw));
    let (y0, y1) = (y as usize, ((y + h) as usize).min(sh));
    assert!(x0 < x1, "blue_rows: an empty rectangle {x},{y} {w}x{h}");
    (y0..y1)
        .filter(|&row| {
            let bias: f64 = (x0..x1)
                .map(|col| {
                    let i = (row * sw + col) * 3;
                    f64::from(px[i + 2]) - f64::from(px[i])
                })
                .sum();
            bias / (x1 - x0) as f64 > floor
        })
        .count()
}

/// AC3, the notes (settings.md, "The card"; QE 2026-10-01, D22): every row
/// of every tab carries its one-line note, and the note is CORE's sentence
/// byte for byte — `Key::note()` and `CLEAR_CACHE_NOTE`, the notes' one
/// home — read from the `settings note <name> shows` mark each note Text
/// emits itself, never from the bridge that feeds it. And it is DRAWN: at
/// the shutter, on the Performance tab, the loupe memory note's rectangle
/// holds text — its luma variance stands far above the bare card's (a strip
/// of the card's top padding, above the title, in the same shot). Text was
/// drawn there; a bound but hidden note would read like the bare card.
///
/// Mutants (2026-10-01): `set_settings_note_wash` taken out of
/// `settings_bridge::wire` → the wash note shows an empty text and this goes
/// red; the note Text taken out of `SettingRow` → no `settings note` mark at
/// all — red.
///
/// The active tab's ACCENT UNDERLINE, in the same shot (settings.md, "The
/// card": the active tab is marked by a 2 px accent underline — brief 010
/// R3, AC28; the underline only, the integrity review having refused a check
/// of the label's brightness). Each tab cell's rectangle comes from its own
/// `settings tab <name>` mark at `dump.perf`, and the shot's pixel ROWS are
/// counted: a row reads as the accent when its mean blue bias (B − R; the
/// accent `#4da3ff` is strongly blue, the card `#202028` hardly) across the
/// cell, inset 3 px from its sides, is above 100. In a window of six rows
/// straddling the active Performance cell's BOTTOM edge there are at least
/// 2 — the 2 px underline; in the same window at its TOP edge at most 1 —
/// the 1 px accent ring the strip draws on every edge of the active cell
/// while it holds the keyboard, whose bottom edge shares the underline's
/// lower row, so with the underline gone the bottom edge holds that one row
/// and no more; at the inactive cells' bottom edges none. The windows
/// straddle the edges because a mark is its position rounded to whole px
/// and the pixels can sit a row off it (test-harness.md, "Layout").
/// Measured 2026-10-03, the accent rows reading 176–179 and every other row
/// in the windows 5–14:
///   * windows-latest (CI run 37150692393, debug; the mark `578,244 size
///     103x32`): the ring's top at y+1, the underline at y+h−1 and y+h —
///     the cell drawn one row below its mark;
///   * ubuntu-latest (the same run, release, DejaVu Sans; `584,258 size
///     111x32`) and this seat (Noto Sans; `580,256 size 108x32`): the
///     ring's top at y, the underline at y+h−2 and y+h−1.
///
/// Rows of one shot counted against the cell's own mark, no font metric:
/// the cell is 32 px tall by the code and its label is centred far from
/// both windows. Mutants (2026-10-03), each alone: the underline's
/// `background` always `transparent` → the bottom window holds the ring's
/// row alone, 1 — red; always `#4da3ff`, an underline under every tab →
/// the inactive cells' bottom windows hold `[2, 2]` — red.
/// Until the senior developer's review F1 (2026-10-03) the strand averaged
/// 3 px bands flush with the mark's edges, which held both underline rows
/// on this seat and only one on windows-latest: red there on a correct
/// tree (the bottom band 65.0 against the top band's 65.1).
#[test]
fn every_settings_note_is_the_core_text() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    use fastcull_core::settings::{Key, CLEAR_CACHE_NOTE};
    let out = out_dir().join("settings-notes.jpg");
    let script = format!(
        "{PIN_WINDOW};900:key:ctrl+,;1300:dump.general;1500:key:right;1800:dump.ui;\
         2000:key:right;2400:dump.perf"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    for (label, tab) in [("general", "0"), ("ui", "1"), ("perf", "2")] {
        assert_eq!(
            dump_field(qedump(&stderr, label), "settingstab"),
            tab,
            "dump.{label} is not on tab {tab}:\n{stderr}"
        );
    }
    let labels = mark_labels(&stderr);
    for (name, note) in [
        ("auto-advance", Key::AutoAdvance.note()),
        ("wash", Key::SelectionWash.note()),
        ("loupe-memory", Key::LoupeMemory.note()),
        ("cache-cap", Key::CacheCap.note()),
        ("readers", Key::MaxReaders.note()),
        ("clear-cache", CLEAR_CACHE_NOTE),
    ] {
        let tag = format!("settings note {name} shows ");
        let shown = labels
            .iter()
            .rev()
            .find_map(|l| l.strip_prefix(tag.as_str()));
        assert_eq!(
            shown,
            Some(note),
            "the {name} row's note is not core's sentence (`settings note {name} \
             shows` is its last mark; None means the row reported no note at \
             all):\n{stderr}"
        );
    }
    let note = laid_out_at(&stderr, "settings note loupe-memory", "perf");
    let (cx, cy, cw, _) = laid_out_at(&stderr, "settings card", "perf");
    // Inside the card's 18 px top padding, clear of the border, the rounded
    // corners and the title below it: bare #202028.
    let bare = (cx + 24.0, cy + 4.0, cw - 48.0, 10.0);
    let (drawn, empty) = (rect_variance(&out, note), rect_variance(&out, bare));
    assert!(
        drawn > 100.0 && drawn > 20.0 * empty.max(1.0),
        "the loupe memory note's rectangle {note:?} reads like bare card — luma \
         variance {drawn:.1} against {empty:.1} for the card's padding: the \
         note is bound but not drawn:\n{stderr}"
    );
    // The active tab's accent underline (AC28), counted in accent ROWS: a
    // row of a tab cell, inset 3 px from its sides (clear of the ring's side
    // edges and rounded corners), whose mean blue bias is above 100 — the
    // accent reads 176–179 there, the card and the labels 5–14. Each window
    // is six rows straddling one edge of the cell, so a cell drawn a row
    // off its mark (windows-latest) holds the same rows as one drawn inside
    // it (this seat).
    let accent = |tab: &str, at_top: bool| {
        let (x, y, w, h) = laid_out_at(&stderr, &format!("settings tab {tab}"), "perf");
        let from = if at_top { y - 2.0 } else { y + h - 4.0 };
        blue_rows(&out, (x + 3.0, from, w - 6.0, 6.0), 100.0)
    };
    let inactive = [accent("general", false), accent("ui", false)];
    assert_eq!(
        inactive,
        [0, 0],
        "an INACTIVE tab's bottom edge holds accent rows (General, UI) — the underline \
         marks the active tab alone (settings.md, \"The card\"):\n{stderr}"
    );
    // The premise of the count below: the ring the strip draws on every edge
    // of the active cell while it holds the keyboard is 1 px, so its bottom
    // edge accounts for at most ONE accent row there.
    let ring = accent("performance", true);
    assert!(
        ring <= 1,
        "the active tab's TOP edge holds {ring} accent rows — more than the 1 px focus \
         ring, so the rows at its bottom edge could be a thicker ring rather than the \
         underline, and the count below proves nothing:\n{stderr}"
    );
    let underline = accent("performance", false);
    assert!(
        underline >= 2,
        "THE ACTIVE TAB HAS NO ACCENT UNDERLINE: its bottom edge holds {underline} accent \
         row(s) where the 2 px underline makes 2 — one row there is the focus ring's \
         bottom edge alone (settings.md, \"The card\"; measured 2 on this seat, on \
         ubuntu-latest and on windows-latest, 1 with the underline transparent):\n{stderr}"
    );
}

/// AC2, stacking (settings.md, "Stacking"; brief 008 D12): the Settings
/// dialog and the export dialogs never stack. On every runner, the keyboard
/// half: with Copy Picks up, `Ctrl+,` opens nothing (the chord lives in the
/// main key scope, which the copy dialog's scope stands in front of). On
/// the calibrated runners, the menu half: with Settings up, File › Copy
/// Picks… is greyed and a click on it opens nothing; with Copy Picks up,
/// File › Settings… is greyed likewise. Each greyed click is followed by a
/// CONTROL — the same click with nothing up opens that dialog — so a click
/// that missed its item cannot pass for a greyed one. The menu strand is
/// Linux-only, like About's (`menu_clicks_are_calibrated`): on Windows the
/// menu bar is the OS's, outside the client area, so the greying there is
/// review-verified. (The pick made first is the approved script's; Copy
/// Picks opens with or without one — a synthetic session has no files to
/// plan, and its summary says so.)
///
/// The Export Frames as Video half, both ways, is a SECOND launch on the
/// calibrated runners (QE 2026-10-02, round 5: only the Copy Picks and
/// Settings items' greying was driven, and taking either export term out
/// left the suite green). It needs a REAL folder: a `--synthetic` session
/// has no files behind its cells, so its export item is greyed for that
/// reason alone (presenter.rs, `clip_frames`). Two frames, both selected,
/// make the export available — `clipavail=true` is read first, the premise.
/// With Settings up, File › Export Frames as Video… is greyed; with the
/// export dialog up, File › Settings… is greyed; each greyed click is
/// followed by its control, the same click with nothing up opening that
/// dialog. With the greying deleted the greyed dumps invert; a click that
/// missed its item fails its control first — neither half can pass
/// vacuously.
///
/// Mutant (2026-10-01): both `enabled:` conditions of the File menu's Copy
/// Picks… and Settings… items set to `true` → Copy Picks opens over
/// Settings (`dump.greyed1` reads `copy=true`) and Settings over Copy Picks
/// (`dump.greyed2` reads `settings=true`) — red on Linux. Mutants
/// (2026-10-02), each alone: `&& !root.settings-visible` taken out of the
/// Export item → the export dialog opens over Settings, `dump.greyed3`
/// reads `clip=true` — red; `!root.clip-visible` taken out of the Settings
/// item → Settings opens over the export dialog, `dump.greyed4` reads
/// `settings=true` — red.
#[test]
fn settings_and_the_export_dialogs_never_stack() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let out = out_dir().join("settings-never-stack.jpg");
    let menu = if menu_clicks_are_calibrated() {
        "3100:key:ctrl+,;3500:click.22,19;3900:click.80,93;4300:dump.greyed1;\
         4500:key:escape;4800:key:escape;5200:dump.closed1;\
         5500:click.22,19;5900:click.80,93;6300:dump.ctrl1;6500:key:escape;\
         6900:key:ctrl+e;7300:click.22,19;7700:click.80,157;8100:dump.greyed2;\
         8300:key:escape;8600:key:escape;9000:dump.closed2;\
         9300:click.22,19;9700:click.80,157;10100:dump.ctrl2"
    } else {
        "3100:dump.nomenu"
    };
    let script = format!(
        "900:key:y;1200:key:ctrl+e;1600:dump.copy;1800:key:ctrl+,;2200:dump.chord;\
         2400:key:escape;2800:dump.copyclosed;{menu}"
    );
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    assert_eq!(
        dump_field(qedump(&stderr, "copy"), "copy"),
        "true",
        "Ctrl+E did not open Copy Picks with a pick made — the premise:\n{stderr}"
    );
    let chord = qedump(&stderr, "chord");
    assert!(
        dump_field(chord, "settings") == "false" && dump_field(chord, "copy") == "true",
        "Ctrl+, under Copy Picks opened Settings over it: {chord}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "copyclosed"), "copy"),
        "false",
        "Esc did not close Copy Picks:\n{stderr}"
    );
    if !menu_clicks_are_calibrated() {
        return;
    }
    let greyed1 = qedump(&stderr, "greyed1");
    assert!(
        dump_field(greyed1, "settings") == "true" && dump_field(greyed1, "copy") == "false",
        "File › Copy Picks… opened over the Settings dialog — it is greyed while \
         Settings is up (brief 008 D12): {greyed1}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "closed1"), "settings"),
        "false",
        "two Escs (the menu, then the dialog) did not close Settings:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "ctrl1"), "copy"),
        "true",
        "CONTROL: the File › Copy Picks… click opened nothing with nothing up — \
         the coordinate missed the item, so the greyed check above is vacuous:\n{stderr}"
    );
    let greyed2 = qedump(&stderr, "greyed2");
    assert!(
        dump_field(greyed2, "copy") == "true" && dump_field(greyed2, "settings") == "false",
        "File › Settings… opened over Copy Picks — it is greyed while Copy Picks \
         is up (brief 008 D12): {greyed2}"
    );
    let closed2 = qedump(&stderr, "closed2");
    assert!(
        dump_field(closed2, "copy") == "false" && dump_field(closed2, "settings") == "false",
        "two Escs (the menu, then the dialog) did not close Copy Picks: {closed2}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "ctrl2"), "settings"),
        "true",
        "CONTROL: the File › Settings… click opened nothing with nothing up — the \
         coordinate missed the item, so the greyed check above is vacuous:\n{stderr}"
    );

    // The export half, both ways, on a REAL folder: a `--synthetic` session
    // has no files behind its cells, so its Export Frames as Video item is
    // greyed for that reason alone (presenter.rs, `clip_frames`) and could
    // never show the greying under test. Two frames selected make the
    // export available — the premise, read first.
    let folder = out_dir().join("settings-never-stack-folder");
    std::fs::remove_dir_all(&folder).ok();
    std::fs::create_dir_all(&folder).unwrap();
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    let _cleanup = RemoveOnDrop(folder.clone());
    for name in ["one.ARW", "two.ARW"] {
        place_fixture(
            &raws_dir().join("A1_full_compressed.ARW"),
            &folder.join(name),
        );
    }
    // File's items sit at y = 61 + 32k: Export Frames as Video… is the
    // third (125), Settings… the fourth (157).
    let script = "1500:wait:load settled gen 0;1600:key:ctrl+space;1800:key:ctrl+right;\
                  2000:key:ctrl+space;2300:dump.avail;2500:key:ctrl+,;2900:click.22,19;\
                  3300:click.80,125;3700:dump.greyed3;3900:key:escape;4200:key:escape;\
                  4600:dump.closed3;4900:click.22,19;5300:click.80,125;5700:dump.ctrl3;\
                  5900:key:escape;6300:key:ctrl+shift+e;6700:dump.clip;7000:click.22,19;\
                  7400:click.80,157;7800:dump.greyed4;8000:key:escape;8300:key:escape;\
                  8700:dump.closed4;9000:click.22,19;9400:click.80,157;9800:dump.ctrl4";
    let stderr = shoot_env_stderr(
        &[folder.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out_dir().join("settings-never-stack-export.jpg"),
    );
    assert_eq!(
        dump_field(qedump(&stderr, "avail"), "clipavail"),
        "true",
        "the premise: two frames selected on a real folder make Export Frames as \
         Video available — without it the item below is greyed for another \
         reason:\n{stderr}"
    );
    let greyed3 = qedump(&stderr, "greyed3");
    assert!(
        dump_field(greyed3, "settings") == "true" && dump_field(greyed3, "clip") == "false",
        "File › Export Frames as Video… opened over the Settings dialog — it is \
         greyed while Settings is up (brief 008 D12): {greyed3}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "closed3"), "settings"),
        "false",
        "two Escs (the menu, then the dialog) did not close Settings:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "ctrl3"), "clip"),
        "true",
        "CONTROL: the File › Export Frames as Video… click opened nothing with \
         nothing up — the coordinate missed the item, so the greyed check above is \
         vacuous:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "clip"), "clip"),
        "true",
        "the premise: Ctrl+Shift+E did not open the export dialog:\n{stderr}"
    );
    let greyed4 = qedump(&stderr, "greyed4");
    assert!(
        dump_field(greyed4, "clip") == "true" && dump_field(greyed4, "settings") == "false",
        "File › Settings… opened over the export dialog — it is greyed while \
         Export Frames as Video is up (brief 008 D12): {greyed4}"
    );
    let closed4 = qedump(&stderr, "closed4");
    assert!(
        dump_field(closed4, "clip") == "false" && dump_field(closed4, "settings") == "false",
        "two Escs (the menu, then the dialog) did not close the export dialog: {closed4}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "ctrl4"), "settings"),
        "true",
        "CONTROL: the File › Settings… click opened nothing with nothing up — the \
         coordinate missed the item, so the greyed check above is vacuous:\n{stderr}"
    );
}

/// settings.md, "Writing": a commit or a Reset that changes nothing writes
/// only a file that already exists — a missing settings.toml stays missing
/// until the first CHANGE. Into an empty config dir: Reset General (all
/// defaults already) and an Enter on the untouched wash field change nothing
/// and write nothing — read off the trace's order, every `settings written`
/// line coming after the one real commit — then typing 15 writes, once.
/// (`Ctrl+Tab`, not the approved script's `Right`, switches to the UI tab:
/// the Reset click leaves the keyboard on Reset, where Right does nothing.)
///
/// Mutant (2026-10-01): the `!changed && !path.exists()` guard taken out of
/// `settings_bridge::save` → the Reset alone creates the file, a
/// `settings written` line precedes the typed commit and this goes red.
#[test]
fn a_no_change_commit_or_reset_never_creates_the_file() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("nochange", None);
    let out = out_dir().join("settings-nochange.jpg");
    let script = "900:key:ctrl+,;1300:click:settings reset;1700:key:ctrl+tab;\
                  2100:click:settings wash;2400:key:return;2800:dump.untouched;\
                  3000:key:ctrl+a;3200:key:1;3400:key:5;3600:key:return;4000:dump.changed";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out,
    );
    let file = dir.join("settings.toml");
    assert_click_resolved(&stderr, "settings reset");
    assert_click_resolved(&stderr, "settings wash");
    let labels = mark_labels(&stderr);
    let count = |l: &str| labels.iter().filter(|x| **x == l).count();
    assert_eq!(
        count("settings reset general"),
        1,
        "the Reset of General did not run once — the premise:\n{stderr}"
    );
    assert_eq!(
        count("settings committed ui.selection_wash = 25"),
        1,
        "the Enter on the untouched field did not commit — the premise:\n{stderr}"
    );
    let untouched = qedump(&stderr, "untouched");
    assert_eq!(dump_field(untouched, "settingstab"), "1");
    assert_eq!(
        dump_text(untouched, "settingsnote"),
        "",
        "the notice is not empty after changes that changed nothing: {untouched}"
    );
    let typed = labels
        .iter()
        .position(|l| *l == "settings committed ui.selection_wash = 15")
        .unwrap_or_else(|| panic!("the typed 15 was never committed:\n{stderr}"));
    assert!(
        !labels[..typed]
            .iter()
            .any(|l| l.starts_with("settings written ")),
        "settings.toml was written before anything changed — a no-change Reset \
         or commit created the file (settings.md, \"Writing\"):\n{stderr}"
    );
    assert_eq!(
        mark_lines(&stderr, "settings written "),
        1,
        "the file was written other than once (once, for the one change):\n{stderr}"
    );
    let text = std::fs::read_to_string(&file).expect("settings.toml after the change");
    assert!(
        text.contains("selection_wash = 15"),
        "the written file lacks the change:\n{text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC1 (settings.md, "The dialog"; focus continuity): opening Settings over
/// a FOCUSED keyword field holding typed text commits the field like a
/// click-away — the keyword lands in the sidecar, the Revert slot names it
/// — and the dialog owns the keyboard (the `-1` token): `Y`/`N` under it
/// mark nothing; `Esc` closes it and hands the keyboard back to the grid,
/// proven by acting (`+` zooms). Opened from the real File menu on the
/// calibrated runners — the menu's own focus restore is what bit issue #41
/// — and by the `settings` token elsewhere, which runs the same
/// `settings-open` body.
///
/// What this pins is the OUTCOME, which several belts hold up at once: on
/// the menu path the field commits when the File menu opens, and the
/// dialog takes the `-1` token at its creation, before the menu's deferred
/// reassert reads it; the open's `focus-keys()`, its deferred refocus,
/// focus.rs's covered term and the field's bounce are each redundant with
/// those (measured: each taken out alone, and the last three together,
/// leave this green).
///
/// Mutant (2026-10-01): the Settings branch taken out of `focus-keys()` →
/// every claim while the dialog is up routes the keyboard to the grid
/// behind it, `dump.opened` reads `focusowner=0` and this goes red — on the
/// menu path and on the token path (the latter measured on Linux with the
/// token forced).
///
/// Before the open, `Ctrl+,` is INERT while the keyword field holds the
/// keyboard (settings.md, "Opening and closing": the chord lives in the main
/// key scope and is inert while a field holds the keyboard; brief 010 R3,
/// AC27): with `bird` typed, `dump.typed` is the premise — the field's own
/// token (`focusowner` neither 0, the grid, nor −1, a dialog), no dialog and
/// nothing committed yet (`revert=""`: the IPTC Revert slot fills only when
/// a commit lands) — and after the chord `dump.inert` reads no dialog, the
/// same token and still `revert=""`: the chord committed nothing. That is
/// what "the field keeps its text" rests on: with nothing committed before
/// the open, the commit `dump.opened` reports (the slot filled) is the
/// open's own, and the sidecar's `>bird<` says it committed the word whole.
/// The sidecar alone cannot say WHEN the word was committed — a chord that
/// committed and cleared the field writes the same `>bird<` (QE 2026-10-03,
/// D2: this doc had credited the sidecar with "keeps its text"). The strand
/// sits between the last typed key and the open: the open and every step
/// after it run 500 ms later than before brief 010, every gap between them
/// kept. Mutants (2026-10-03): the root's `content` made a FocusScope whose
/// `capture-key-pressed` opens the dialog on `Ctrl+,` before any field sees
/// the key → `dump.inert` reads `settings=true focusowner=-1` — red at the
/// dialog check, which comes first; G1, the keyword field's own
/// `key-pressed` committing its text and clearing it on `Ctrl+,` →
/// `dump.inert` reads `revert="Revert: keywords on 1 image(s)"` — red at the
/// `revert` check, and green before that check existed.
#[test]
fn settings_over_a_focused_keyword_field_commits_it_and_owns_the_keyboard() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = out_dir().join("settings-over-keyword");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &dir.join("one.ARW"),
    );
    let out = out_dir().join("settings-over-keyword.jpg");
    let open = if menu_clicks_are_calibrated() {
        "4100:click.22,19;4500:click.80,157"
    } else {
        "4500:settings"
    };
    let script = format!(
        "2400:wait:load settled gen 0;2500:key:k;3000:key:b;3100:key:i;3200:key:r;\
         3300:key:d;3500:dump.typed;3700:key:ctrl+,;3900:dump.inert;{open};\
         4900:dump.opened;5100:key:y;5300:key:n;5600:dump.under;\
         5800:key:escape;6200:dump.closed;6400:key:+;6700:dump.zoomed"
    );
    let stderr = shoot_env_stderr(
        &[dir.to_str().unwrap()],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script.as_str())],
        &out,
    );
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the `wait:load settled gen 0` step never fired — the panel opened on \
         the clock:\n{stderr}"
    );
    // Ctrl+, inert while the keyword field holds the keyboard (AC27): the
    // premise — the field's own token, no dialog — then the same after it.
    let typed = qedump(&stderr, "typed");
    let field = dump_field(typed, "focusowner");
    assert!(
        field != "0" && field != "-1" && dump_field(typed, "settings") == "false",
        "the premise: the typed keyword field does not hold the keyboard (focusowner \
         {field}, neither the grid's 0 nor a dialog's -1, expected) or a dialog is \
         up: {typed}"
    );
    let inert = qedump(&stderr, "inert");
    assert_eq!(
        dump_field(inert, "settings"),
        "false",
        "Ctrl+, opened Settings while the keyword field held the keyboard — the chord \
         is inert while a field holds it (settings.md, \"Opening and closing\"): {inert}"
    );
    assert_eq!(
        dump_field(inert, "focusowner"),
        field,
        "Ctrl+, took the keyboard from the keyword field it should be inert over: \
         {inert}"
    );
    // ... and the field kept its text: nothing was committed before the
    // open. `revert=` is the IPTC Revert slot, which fills only when a
    // commit lands, so it reads "" at `typed` (the premise: the typing
    // committed nothing) and at `inert` (the chord committed nothing), and
    // the commit `opened` reports below is then the open's own (QE
    // 2026-10-03, D2). After the two checks above, so a chord that opened
    // the dialog — which commits the field too — keeps its own message.
    assert_eq!(
        dump_text(typed, "revert"),
        "",
        "the premise: something was committed before Ctrl+, — the Revert slot is not \
         empty at `dump.typed`: {typed}"
    );
    assert_eq!(
        dump_text(inert, "revert"),
        "",
        "Ctrl+, committed the keyword field it should be inert over — the field must keep \
         its text for the open to commit (settings.md AC27): {inert}"
    );
    let opened = qedump(&stderr, "opened");
    assert_eq!(
        dump_field(opened, "settings"),
        "true",
        "the Settings dialog never opened (the menu click missed?): {opened}"
    );
    assert_eq!(
        dump_field(opened, "focusowner"),
        "-1",
        "the dialog is up but does not own the keyboard (the `-1` token) — \
         the keyword field or the grid holds it behind the scrim: {opened}"
    );
    assert!(
        opened.contains("revert=\"Revert: keywords on 1 image(s)\""),
        "the typed keyword was not committed when Settings opened over the \
         field — a click-away commits it: {opened}"
    );
    let under = qedump(&stderr, "under");
    assert!(
        dump_text(under, "status").contains("★0 ✕0") && dump_field(under, "settings") == "true",
        "Y/N under the dialog marked a frame or closed the dialog: {under}"
    );
    let closed = qedump(&stderr, "closed");
    assert!(
        dump_field(closed, "settings") == "false" && dump_field(closed, "focusowner") == "0",
        "Esc did not close the dialog and give the keyboard back to the grid: {closed}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "zoomed"), "zoom"),
        "2",
        "the `+` after the dialog closed was dead:\n{stderr}"
    );
    let sidecar = dir.join("one.ARW.xmp");
    let xmp = std::fs::read_to_string(&sidecar)
        .unwrap_or_else(|e| panic!("no sidecar written for the committed keyword: {e}"));
    assert!(
        xmp.contains(">bird<"),
        "the sidecar does not hold the committed keyword: {xmp}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// settings.md, "The card": the Performance tab in its TALLEST state — the
/// FASTCULL_MAX_READERS note on the read workers row, a read error's whole
/// text on the notice line and, with the cache on, the Thumbnail cache row
/// showing a long path in full — fits whole in the smallest supported
/// window, 1000x700 (ui-grid.md). Since brief 009 the card has one height
/// per open, its tallest tab's (settings.md, "The card holds still"), so
/// this "tallest state" is every tab's state: the run still opens on
/// Performance, where the long cache row lives, and the fit it measures
/// holds on every tab (AC20). Measured as SLACK, never as a height: the
/// card is `min(content, layer − 40)`, so a clamped card sits exactly 20 px
/// above the modal layer's floor (the status bar's top, `window − 26`) and an
/// unclamped one more; Close and Reset lie inside the card. The slack is
/// printed in the failure, never asserted as a number — a height is a sum of
/// text line boxes and belongs to the face (the rule ui-grid.md gives the
/// shortcuts card). The premises are asserted, so the state really is the
/// tallest: the tab, the environment's value, the long notice.
///
/// Two runs of the same script over the same broken settings file. The
/// first is the harness's FASTCULL_NO_CACHE, where the cache row is one short
/// line. The second (QE 2026-10-01, D38) has the cache ON, through
/// `shoot_with_sandboxed_cache` — HOME and XDG_CACHE_HOME inside the shots
/// dir, Linux only, Windows' known-folder lookup ignoring both (the rule of
/// settings.md AC11/AC12), so it is skipped at run time elsewhere and the
/// `--list` halves stay the same on every runner — with XDG_CACHE_HOME
/// nested so the row prints a path of 85–100 characters,
/// `~/.cache/nas-mount/…/fastcull/previews.db`, which wraps. Its one extra
/// premise is that the row shows that path in full. Why 85–100 (evidence,
/// brief 008 D38): the row is as tall as its Clear button until the readout
/// reaches three lines; a path under 100 characters is two lines on Noto
/// Sans and DejaVu Sans and never four on any face, the one shape the 20 px
/// could not take — Noto at 1000x700 measured 47 px of slack without the
/// cache, 45 px at 93 characters, 35 px at 118 (three lines), 27 px at 150.
///
/// Mutants (2026-10-01): the card layout's padding raised by 80 px → the
/// card clamps, Close lands below its floor and the slack reads 20 — red;
/// the cache row's readout at 24 px instead of 13 → the cache-off run still
/// fits (575 px, 29 px of slack) while the cache-on run clamps at 594 with
/// Close outside the card — red: the second run sees what the first cannot.
#[test]
fn the_settings_card_fits_its_smallest_window_in_its_tallest_state() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("tallest", Some("[general\n"));
    let script = "200:resize:1000x700;600:wait:window geometry 1000x700;900:key:ctrl+,;\
                  1300:key:ctrl+tab;1600:key:ctrl+tab;2000:dump.perf";
    // The premises and the fit, for one run.
    let assert_fits = |stderr: &str, strand: &str| {
        assert!(
            stderr.contains("wait:window geometry 1000x700 (satisfied"),
            "{strand}: the window never reached 1000x700 — the premise:\n{stderr}"
        );
        let perf = qedump(stderr, "perf");
        assert_eq!(
            dump_field(perf, "settingstab"),
            "2",
            "{strand}: not on Performance: {perf}"
        );
        assert_eq!(
            dump_field(perf, "readers"),
            "env:3",
            "{strand}: the environment note row is not up: {perf}"
        );
        let note = dump_text(perf, "settingsnote");
        assert!(
            note.contains("could not be read") && note.contains("invalid table header"),
            "{strand}: the notice is not the whole parse error — not the tallest \
             state: {note:?}"
        );
        let (wx, wy) = (1000.0f32, 700.0f32);
        let (cx, cy, cw, ch) = laid_out_at(stderr, "settings card", "perf");
        for control in ["settings close", "settings reset"] {
            let (x, y, w, h) = laid_out_at(stderr, control, "perf");
            assert!(
                x >= cx && x + w <= cx + cw + 0.5 && y >= cy && y + h <= cy + ch + 0.5,
                "{strand}: {control} ({x},{y} {w}x{h}) is not inside the card ({cx},{cy} \
                 {cw}x{ch}) at 1000x700 in the tallest state:\n{stderr}"
            );
        }
        let floor = wy - 26.0;
        assert!(
            cx >= 0.0 && cx + cw <= wx && cy >= 0.0 && cy + ch <= floor,
            "{strand}: the card ({cx},{cy} {cw}x{ch}) is not inside the modal layer of a \
             1000x700 window (which ends at y={floor}, the status bar's top):\n{stderr}"
        );
        let slack = floor - (cy + ch);
        assert!(
            slack > 20.0,
            "{strand}: THE SETTINGS CARD OUTGREW ITS SMALLEST WINDOW: {ch} px tall at \
             1000x700 in its tallest state, leaving {slack} px above the status bar — the \
             clamp's own 20 px, so the card is clamped and its content cut. A row was \
             added, or this seat's face is far taller than the ones it was measured \
             on:\n{stderr}"
        );
    };
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_MAX_READERS", "3"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out_dir().join("settings-tallest.jpg"),
    );
    assert_fits(&stderr, "cache off");

    if !cfg!(target_os = "linux") {
        eprintln!(
            "skipped the cache-on strand: the default cache cannot be sandboxed off Linux \
             (Windows' known-folder lookup ignores HOME and XDG_CACHE_HOME); the wrapped \
             readout is review-verified there"
        );
        std::fs::remove_dir_all(&dir).ok();
        return;
    }
    let home = out_dir().join("cache-home");
    std::fs::remove_dir_all(&home).ok();
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    let _cleanup = RemoveOnDrop(home.clone());
    let cache_home = home
        .join(".cache")
        .join("nas-mount")
        .join("photo-studio")
        .join("cull-sessions")
        .join("a-cache-path-long-enough");
    // The path the row prints, `~/` and all — 91 characters, in the band
    // the evidence above sets.
    let readout_path = format!(
        "~/{}",
        cache_home
            .join("fastcull")
            .join("previews.db")
            .strip_prefix(&home)
            .unwrap()
            .display()
    );
    assert!(
        (85..=100).contains(&readout_path.chars().count()),
        "the long path is {} characters, outside the 85–100 the strand is built for: \
         {readout_path}",
        readout_path.chars().count()
    );
    let stderr = shoot_with_sandboxed_cache(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_MAX_READERS", "3"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("HOME", home.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache_home.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out_dir().join("settings-tallest-cache.jpg"),
    );
    let readout = dump_text(qedump(&stderr, "perf"), "cachereadout");
    assert!(
        readout.contains(&readout_path),
        "the cache-on strand's row does not show the long path in full — not the \
         long-path state (expected `{readout_path}`): {readout:?}"
    );
    assert_fits(&stderr, "cache on, a long path");
    std::fs::remove_dir_all(&dir).ok();
}

/// How many marks whose label starts with `mark` lie in `labels[from..to]`
/// — a stretch of [`mark_labels`] between two anchors the caller located.
/// Counted as emitted lines: a relayout of the Settings card emits its mark
/// twice (`changed absolute-position` and `changed height`), so a count is
/// a count of layouts plus their echoes, never a substring tally.
fn marks_between(labels: &[&str], from: usize, to: usize, mark: &str) -> usize {
    labels[from..to]
        .iter()
        .filter(|l| l.starts_with(mark))
        .count()
}

/// Where each `label` sits in [`mark_labels`], in order — the anchors of
/// [`marks_between`].
fn label_positions(labels: &[&str], label: &str) -> Vec<usize> {
    labels
        .iter()
        .enumerate()
        .filter(|(_, l)| **l == label)
        .map(|(i, _)| i)
        .collect()
}

/// settings.md, "The card holds still" (brief 009 R1–R5; AC17, AC18's first
/// half, AC19). The user, 2026-10-03: "The settings screen is bumping
/// depending on its size … get its size fixed." At each open the card takes
/// ONE height — its TALLEST tab's — and a tab switch moves nothing: no edge,
/// no button, no label.
///
/// Three launches of one script: open on General, `Ctrl+Tab` through UI and
/// Performance back to General with a dump on each tab, on to Performance,
/// `Esc`, and reopen there (it reopens on the tab it closed on):
///   1. the harness's FASTCULL_NO_CONFIG — the notice line says `Not saved`
///      from the open's first frame;
///   2. FASTCULL_CONFIG_DIR into an empty scratch dir — no file, no notice;
///   3. a broken file (`[general`) and FASTCULL_MAX_READERS=3 — the whole
///      parse error, two lines, on the notice line and the environment's
///      line on Read workers, both there at the open.
///
/// Each launch asserts, from the app's own layout marks (test-harness.md):
/// exactly ONE `settings card laid out` mark from each `Ctrl+,` to the close
/// or the run's end — the open's `init`, whose geometry is already final —
/// counted as lines, never read off Close (its `init` mark is a 32x32
/// placeholder); the card one height at every dump, and an open on General
/// as tall as the reopen on Performance (R1: the tallest tab is the height);
/// Close and Reset at one x,y at all four dumps of the walk (R2 — Reset's
/// WIDTH follows its label, "Reset General…" against "Reset Performance…",
/// and is not compared); every tab's x and width the same at all four (R4);
/// and the body host ending above Reset (the slack is empty card); the
/// active tab's first control starting at the body's top at every dump (R2:
/// the body is laid out from the top under the rule); and, to the pixel, no
/// new layout mark from the card, the body host, the notice line, the
/// footer or a tab between the first switch and the close. Across
/// launches 1 and 2 the card and the notice line are the same height (R3:
/// the line is reserved, so a notice moves nothing). Every comparison is
/// between two measurements on one seat; no height is pinned.
///
/// RED on bef5b5e, the head before brief 009 (the brief's table, measured
/// again 2026-10-03, debug, this seat): launch 1's first open laid the card
/// out 13 times — 238 px at `init`, 267 two milliseconds later, in the same
/// key event, when the notice line (then an `if`) was created — three
/// marks, nothing painted short — then two marks per switch, 266 / 506 /
/// 267 / 266 / 506 — Close at y 526 → 540 → 660 → 540, the tabs 458/78 →
/// 458/76, 538/42 → 536/43, 582/108 → 580/111; launch 2 11 times (238 / 237
/// / 477 / 238 / 237 / 477); launch 3 13 times — 238 → 283 at the open, the
/// two-line notice created after the card's `init` read its height, and 477
/// → 541 at the reopen on Performance, the environment's line created after
/// it too (each in the same key event as the `init`, never painted short).
/// When this fails that way it is that defect; do not quiet it.
///
/// Mutants (2026-10-03), each alone: the body host's height back to the
/// ACTIVE body's (bef5b5e's rule) → launch 1 lays the card out 3 times, the
/// growth on the switch to Performance — red; the notice line back to an
/// `if` → 3 card marks at launch 1's open — red; the active tab's
/// `font-weight` back → "the ui tab moved from x 538 width 42 at the open to
/// x 536 width 43 at dump.ui" — red; the environment's line back to an `if`
/// → 3 card marks at launch 3's open, launches 1 and 2 green — red; the
/// notice line kept but 0 px when blank → each launch holds still on its
/// own, and the card is 17 px shorter without a notice than with one — red
/// at the comparison across launches 1 and 2. And (the test-integrity
/// review, 2026-10-03; QE D3) General's body centred in the host, `y:
/// (parent.height - self.height) / 2` → "the active tab's first control
/// starts 119 px below the body's top at dump.open" — red, where T3, the
/// tabs test and the open/close test all stayed green.
#[test]
fn the_settings_card_holds_still_across_its_tabs() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let script = "900:key:ctrl+,;1400:dump.open;1700:key:ctrl+tab;2100:dump.ui;\
                  2400:key:ctrl+tab;2800:dump.perf;3100:key:ctrl+tab;3500:dump.general;\
                  3800:key:ctrl+tab;4100:key:ctrl+tab;4400:key:escape;4800:key:ctrl+,;\
                  5300:dump.reopened";
    let walk = ["open", "ui", "perf", "general"];
    let run = |extra: &[(&str, &str)], shot: &str| {
        let mut envs = vec![("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)];
        envs.extend_from_slice(extra);
        shoot_env_stderr(&["--synthetic", "24"], &envs, &out_dir().join(shot))
    };
    // The claims of one launch, the premises first.
    let assert_still = |stderr: &str, strand: &str| {
        for (label, tab) in [
            ("open", "0"),
            ("ui", "1"),
            ("perf", "2"),
            ("general", "0"),
            ("reopened", "2"),
        ] {
            assert_eq!(
                dump_field(qedump(stderr, label), "settingstab"),
                tab,
                "{strand}: dump.{label} is not on tab {tab} — the walk did not happen:\n{stderr}"
            );
        }
        let labels = mark_labels(stderr);
        assert_eq!(
            label_positions(&labels, "drive: key:ctrl+tab").len(),
            5,
            "{strand}: the five Ctrl+Tab steps did not all run:\n{stderr}"
        );
        // R5: the card is laid out once per open. The anchors are the two
        // `Ctrl+,` steps and the close between them.
        let opens = label_positions(&labels, "drive: key:ctrl+,");
        let closed = labels
            .iter()
            .position(|l| *l == "settings closed")
            .unwrap_or_else(|| panic!("{strand}: the dialog never closed:\n{stderr}"));
        assert!(
            opens.len() == 2 && opens[0] < closed && closed < opens[1],
            "{strand}: not one open, a close and a reopen, in that order:\n{stderr}"
        );
        let card = "settings card laid out at ";
        let first = marks_between(&labels, opens[0], closed, card);
        assert_eq!(
            first, 1,
            "{strand}: THE SETTINGS CARD LAID OUT {first} TIMES in an open that switched \
             tabs four times — it takes one height per open, its tallest tab's, and \
             neither a line created after the card's `init` nor a switch may change it \
             (settings.md, \"The card holds still\"; brief 009):\n{stderr}"
        );
        let second = marks_between(&labels, opens[1], labels.len(), card);
        assert_eq!(
            second, 1,
            "{strand}: the card laid out {second} times at the reopen on Performance — \
             once per open:\n{stderr}"
        );
        // R1: one height, every tab; the open on General is as tall as the
        // reopen on Performance, the tallest body.
        let height = |label: &str| laid_out_at(stderr, "settings card", label).3;
        for label in walk {
            assert_eq!(
                height(label),
                height("open"),
                "{strand}: the card is {} px tall at dump.{label} and {} px at the open — \
                 a tab switch changed its height:\n{stderr}",
                height(label),
                height("open")
            );
        }
        assert_eq!(
            height("reopened"),
            height("open"),
            "{strand}: the card opened {} px tall on General and {} px on Performance — \
             its height is not the tallest tab's:\n{stderr}",
            height("open"),
            height("reopened")
        );
        // R2: the footer is pinned. x AND y, at every dump of the walk.
        for control in ["settings close", "settings reset"] {
            let (x0, y0, _, _) = laid_out_at(stderr, control, "open");
            for label in walk {
                let (x, y, _, _) = laid_out_at(stderr, control, label);
                assert!(
                    x == x0 && y == y0,
                    "{strand}: {control} moved from {x0},{y0} at the open to {x},{y} at \
                     dump.{label} — the footer is not pinned (brief 009 R2):\n{stderr}"
                );
            }
        }
        // R4: the strip holds still.
        for tab in ["general", "ui", "performance"] {
            let what = format!("settings tab {tab}");
            let (x0, _, w0, _) = laid_out_at(stderr, &what, "open");
            for label in walk {
                let (x, _, w, _) = laid_out_at(stderr, &what, label);
                assert!(
                    x == x0 && w == w0,
                    "{strand}: the {tab} tab moved from x {x0} width {w0} at the open to x {x} \
                     width {w} at dump.{label} — the strip shifted on a switch (brief 009 \
                     R4):\n{stderr}"
                );
            }
        }
        // The slack between the body and the footer is empty card.
        for label in walk {
            let (_, by, _, bh) = laid_out_at(stderr, "settings body", label);
            let (_, ry, _, _) = laid_out_at(stderr, "settings reset", label);
            assert!(
                by + bh <= ry,
                "{strand}: the body host ends at {} but Reset starts at {ry} at \
                 dump.{label}:\n{stderr}",
                by + bh
            );
        }
        // R2's other half: the active body is laid out from the top under the
        // rule, so the tab's first control starts at the body's top. Bounded
        // by the control's OWN height, never by a pixel figure: on every face
        // measured the two are equal (Noto Sans 313 = 313 on this seat; DejaVu
        // Sans 314 = 314 and Segoe UI 302 = 302 on PR #102's runners), but a
        // face whose label is taller than the control may seat the control
        // lower in its row — by less than the control's own height. A body
        // centred in the host, or one left scrolled, is out by far more.
        for (label, control) in [
            ("open", "settings auto-advance"),
            ("ui", "settings wash"),
            ("perf", "settings loupe-memory"),
            ("general", "settings auto-advance"),
            ("reopened", "settings loupe-memory"),
        ] {
            let (_, top, _, _) = laid_out_at(stderr, "settings body", label);
            let (_, y, _, h) = laid_out_at(stderr, control, label);
            assert!(
                y >= top && y - top < h,
                "{strand}: the active tab's first control starts {} px below the body's \
                 top at dump.{label} ({control} at y {y}, {h} px tall; the body at y \
                 {top}) — the body is not laid out from the top (settings.md, \"The \
                 card holds still\"):\n{stderr}",
                y - top
            );
        }
        // And nothing moved AT ALL from the first switch to the close: no new
        // mark from the card, the body host, the notice line, Reset, Close or
        // a tab. Exact where the comparisons above are not: a mark prints its
        // position rounded half to even, so a 1 px move from y 660.5 to 659.5
        // prints `660` both times (measured under the body-host mutant in
        // `the_settings_card_never_shrinks_while_it_is_open`'s doc).
        let first_switch = label_positions(&labels, "drive: key:ctrl+tab")[0];
        for mark in [
            card,
            "settings body laid out at ",
            "settings notice laid out at ",
            "settings reset laid out at ",
            "settings close laid out at ",
            "settings tab ",
        ] {
            let moved = marks_between(&labels, first_switch, closed, mark);
            assert_eq!(
                moved, 0,
                "{strand}: `{mark}…` reported {moved} new layout(s) during the tab walk — \
                 a switch moved it (brief 009 R2, R4):\n{stderr}"
            );
        }
    };

    let notice = run(&[], "settings-still-notice.jpg");
    let said = dump_text(qedump(&notice, "open"), "settingsnote").to_string();
    assert!(
        said.starts_with("Not saved"),
        "launch 1 has no notice at the open, so it proves nothing about one: {said:?}"
    );
    assert_still(&notice, "a notice at the open");

    let empty = settings_scratch("still-quiet", None);
    let quiet = run(
        &[("FASTCULL_CONFIG_DIR", empty.to_str().unwrap())],
        "settings-still-quiet.jpg",
    );
    assert_eq!(
        dump_text(qedump(&quiet, "open"), "settingsnote"),
        "",
        "launch 2 has a notice — it must have none:\n{quiet}"
    );
    assert_still(&quiet, "no notice");
    // R3: the notice line is reserved — the same card, and the same line,
    // with and without a notice.
    let notice_line = |stderr: &str| laid_out_at(stderr, "settings notice", "open").3;
    let card_h = |stderr: &str| laid_out_at(stderr, "settings card", "open").3;
    assert_eq!(
        card_h(&notice),
        card_h(&quiet),
        "the card is {} px tall with a notice and {} px without one — the notice \
         line is not reserved (brief 009 R3)",
        card_h(&notice),
        card_h(&quiet)
    );
    assert_eq!(
        notice_line(&notice),
        notice_line(&quiet),
        "the notice line is {} px saying `Not saved` and {} px blank — a blank \
         line must reserve its one line (brief 009 R3)",
        notice_line(&notice),
        notice_line(&quiet)
    );

    let broken = settings_scratch("still-tallest", Some("[general\n"));
    let tallest = run(
        &[
            ("FASTCULL_CONFIG_DIR", broken.to_str().unwrap()),
            ("FASTCULL_MAX_READERS", "3"),
        ],
        "settings-still-tallest.jpg",
    );
    // The premises: the environment governs Read workers and its line is on
    // screen at the open's first layout, and the notice is the parse error,
    // taller than launch 1's one line — it wraps.
    let open = qedump(&tallest, "open");
    assert_eq!(
        dump_field(open, "readers"),
        "env:3",
        "launch 3: the environment does not govern Read workers: {open}"
    );
    assert!(
        dump_text(open, "settingsnote").contains("could not be read"),
        "launch 3: the notice is not the read error: {open}"
    );
    let (_, _, _, env_h) = laid_out_at(&tallest, "settings note readers-env", "open");
    assert!(
        env_h > 0.0,
        "launch 3: the environment's line on Read workers is 0 px at the open:\n{tallest}"
    );
    assert!(
        notice_line(&tallest) > notice_line(&notice),
        "launch 3: the read error's notice ({} px) is not taller than a one-line \
         notice ({} px) — it must wrap, or this launch is not the two-line \
         case:\n{tallest}",
        notice_line(&tallest),
        notice_line(&notice)
    );
    assert_still(&tallest, "a two-line notice and the environment's line");
    std::fs::remove_dir_all(&empty).ok();
    std::fs::remove_dir_all(&broken).ok();
}

/// settings.md, "The card holds still" (brief 009 R1; AC18's second half):
/// while the dialog is open its height is a HIGH-WATER MARK — a text that
/// grows grows the card, and nothing shrinks it. The text of launches 1 and
/// 2 is Loupe memory's hint: `100` is clamped to this machine's RAM and the
/// hint reads `= 31.1 GB (all of this machine's RAM) ≈ 223 A1 frames`,
/// which wraps to a second line in its cell on every face measured; `2`
/// puts back the one-line `= 2.0 GB of … ≈ 14 A1 frames`. The wrapped hint
/// moves the card only where its two lines are TALLER than the 32 px field
/// beside them: Noto Sans, this seat (two 12 px lines, 33 px). On DejaVu
/// Sans (28 px) and Segoe UI they fit the field's row, and on PR #102's
/// runners the card stayed 497 px (ubuntu) and 483 px (windows) through both
/// commits (QE 2026-10-03, D2). Three launches:
///   1. open on the defaults, commit `100`, then `2`: after the second commit
///      the card stays the height the wrap gave it, Close does not move, and
///      no `settings card laid out` mark lies between the second Enter and
///      the dump;
///   2. open on a file that already says `loupe_memory = "100 GB"` — the
///      hint is wrapped in the open's first layout — and commit `2`: the
///      card keeps the height it OPENED with, again with no card mark. This
///      is the launch that reaches the open's own capture of the mark (the
///      card's `init`): in launch 1 the content grows before it shrinks, and
///      the `changed` handler alone would hold it. Then the GROWTH, across
///      the two launches (the test-integrity review, 2026-10-03; QE D1): a
///      commit that wraps the hint while the dialog is open grows the card
///      to the height an open already in that state takes — launch 1's card
///      at `wrapped` equals launch 2's at its open. Two measurements on one
///      seat, no height pinned; the notice line is reserved, so launch 1's
///      `Not saved` and launch 2's blank line measure the same (R3, pinned
///      by `the_settings_card_holds_still_across_its_tabs`);
///   3. Linux only — the cache strand, the one with power on the ubuntu
///      runner's face (the test-integrity review, 2026-10-03; QE D2). The
///      default cache is sandboxed under the shots dir through HOME and
///      XDG_CACHE_HOME (`shoot_with_sandboxed_cache`, which refuses to run
///      otherwise; Windows' known-folder lookup ignores both variables, so
///      the strand skips itself there AT RUN TIME and the `--list` stays the
///      same on every runner), nested so the Thumbnail cache row prints a
///      path of 221 characters in hyphenated words,
///      `~/.cache/<63>/<63>/<63>/fastcull/previews.db`: four lines on every
///      face QE measured (rows of 71, 61 and 63 px on Noto Sans, DejaVu Sans
///      and Selawik, Segoe UI's open twin), where the fit test's
///      91-character path is two lines and the row stays as tall as its
///      Clear button. Clear turns the row into the one-line `Clearing…`, and
///      the content shrinks by the difference — the shrink the card must not
///      follow: between `settings cache clearing` and `settings cache
///      cleared` the card, the body host, the notice line, Reset and Close
///      report NO new layout, and the card at `dump.cleared` is at least as
///      tall as at `dump.perf`. A driven click is a timer step, and Slint
///      runs the change trackers right after the timers, before the clear
///      worker's posted completion can land (Cargo.toml, the fourth canary's
///      fact 8), so a card that followed the shrink would report it inside
///      that window however fast the clear. The window ends at `cleared` on
///      purpose: the row then shows the path again with a re-measured size,
///      and a longer figure may grow the card once, as a text that grows
///      does.
///
/// Growth at `100` is NOT asserted within launch 1 (`≥`, never `>`):
/// whether the wrapped hint outgrows its field is the face's business. So
/// launches 1 and 2, and the growth check across them, go red under the
/// mutants below only where it does — Noto Sans, this seat — and are green
/// but powerless on both runners' faces: their verdict does not depend on
/// the seat, their power does. Launch 3 is what gives the ubuntu runner
/// power on the never-shrinks rule; growth has its own launch, with power
/// on every face, in
/// `the_settings_card_grows_by_a_write_error_that_wraps_while_it_is_open`.
/// The premises are asserted: the click resolved on the field, both commits
/// traced, and the hint said the clamp's words at the wrapped dump and not
/// at the other — read from its text, never from a height. (A machine with
/// 100 GB of RAM or more would not clamp, and the premise says so loudly.)
/// After each commit the body host and the footer report NO new layout: a
/// mark prints its position rounded half to even, so comparing printed y
/// values cannot see a 1 px move between two halves. Launch 3's premises:
/// the click on Clear resolved; at `dump.perf` the row showed the long path
/// in full and was taller than its one-line Clear button (the button's cell
/// stretches with its row, so its reported height is the row's — the path
/// wrapped); and `settings cache clearing` and the row's `Clearing…` came
/// between the click and `settings cache cleared`.
///
/// RED on bef5b5e (2026-10-03, debug, this seat): launch 1 went 506 → 507 →
/// 506, the card re-laid out twice after the second Enter; launch 2 opened
/// at 478 and fell to 477.
///
/// Mutants (2026-10-03), each alone: the card's height without the mark
/// (`min(self.content, …)`) → launch 1, "THE CARD SHRANK WHILE OPEN — 506
/// px after `2` against 507 px after `100`" — red; the card's `changed
/// content` handler deleted → the same — red; the open's capture
/// (`self.high-water = self.content` in the card's `init`) deleted → launch
/// 1 GREEN and launch 2 "THE CARD SHRANK BELOW THE HEIGHT IT OPENED WITH —
/// 506 px … against 507 px" — red, which is why launch 2 exists; the body
/// host's `min-height` made a bound `height` (a fixed cell, which cannot
/// take the slack) → the card holds 507, the host falls to 306 and the
/// notice, Reset and Close rise 1 px, Close's mark printing `660` before
/// and after (660.5 and 659.5) — the y comparison green, the no-new-layout
/// check red: "`settings body laid out at …` reported 2 new layout(s)".
/// Mutants (the test-integrity review, 2026-10-03), each alone: the card's
/// `changed content` handler deleted AND its height `min(self.high-water,
/// …)` — a card that never grows → launches 1 and 2 green, the growth check
/// red, "THE CARD DID NOT FOLLOW A TEXT THAT GREW WHILE OPEN — 506 px after
/// the commit of `100` … against 507 px" — and on DejaVu Sans (the ubuntu
/// runner's face, through `SLINT_DEFAULT_FONT`) the whole test GREEN, 497 =
/// 497: the power limit above, which is why the growth has its own test;
/// the card's height without the mark (`min(self.content, …)`) on DejaVu
/// Sans → launches 1 and 2 green, launch 3 red, "`settings card laid out at
/// …` reported 2 new layout(s) while the row read `Clearing…`" (526 → 497
/// px inside the window, back to 526 after `cleared`) — on Noto Sans the
/// same mutant is red at launch 1, above.
#[test]
fn the_settings_card_never_shrinks_while_it_is_open() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let clamped = "all of this machine's RAM";
    let card = "settings card laid out at ";

    let script = "900:key:ctrl+,;1300:key:ctrl+tab;1600:key:ctrl+tab;2000:dump.perf;\
                  2300:click:settings loupe-memory;2600:key:ctrl+a;2800:key:1;3000:key:0;\
                  3200:key:0;3400:key:return;3900:dump.wrapped;4200:key:ctrl+a;4400:key:2;\
                  4600:key:return;5100:dump.unwrapped";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[("FASTCULL_TRACE", "1"), ("FASTCULL_DRIVE", script)],
        &out_dir().join("settings-never-shrinks.jpg"),
    );
    assert_click_resolved(&stderr, "settings loupe-memory");
    let labels = mark_labels(&stderr);
    let committed = |value: &str| {
        let mark = format!("settings committed performance.loupe_memory = {value}");
        labels.iter().position(|l| *l == mark)
    };
    assert!(
        matches!((committed("100 GB"), committed("2 GB")), (Some(a), Some(b)) if a < b),
        "launch 1: Loupe memory was not committed as 100 GB and then 2 GB:\n{stderr}"
    );
    let hint =
        |stderr: &str, label: &str| dump_text(qedump(stderr, label), "loupehint").contains(clamped);
    assert!(
        hint(&stderr, "wrapped") && !hint(&stderr, "unwrapped"),
        "launch 1: the hint did not read the clamp at `wrapped` and the plain figure at \
         `unwrapped` — the premise:\n{stderr}"
    );
    let height = |stderr: &str, label: &str| laid_out_at(stderr, "settings card", label).3;
    assert!(
        height(&stderr, "wrapped") >= height(&stderr, "perf"),
        "launch 1: the card SHRANK at a commit that can only grow it:\n{stderr}"
    );
    assert_eq!(
        height(&stderr, "unwrapped"),
        height(&stderr, "wrapped"),
        "launch 1: THE CARD SHRANK WHILE OPEN — {} px after `2` against {} px after \
         `100`; its height is a high-water mark for the open (settings.md, \"The card \
         holds still\"; brief 009 R1):\n{stderr}",
        height(&stderr, "unwrapped"),
        height(&stderr, "wrapped")
    );
    let enters = label_positions(&labels, "drive: key:return");
    let dumped = labels
        .iter()
        .position(|l| l.starts_with("QEDUMP unwrapped "))
        .unwrap_or_else(|| panic!("no `dump.unwrapped` mark:\n{stderr}"));
    assert_eq!(enters.len(), 2, "launch 1: not two Enters:\n{stderr}");
    let after = marks_between(&labels, enters[1], dumped, card);
    assert_eq!(
        after, 0,
        "launch 1: the card laid out {after} time(s) after the commit of `2` — it \
         moved when it must hold still:\n{stderr}"
    );
    let close_y = |stderr: &str, label: &str| laid_out_at(stderr, "settings close", label).1;
    assert_eq!(
        close_y(&stderr, "unwrapped"),
        close_y(&stderr, "wrapped"),
        "launch 1: Close moved after the commit of `2`:\n{stderr}"
    );
    // The footer held still to the pixel: the slack the shrink left went to
    // the body host, which keeps its height. Read as "no new mark", because a
    // mark prints a rounded position — under the mutant that binds the host's
    // height the footer rose 1 px, 660.5 → 659.5, and both print `660`.
    let footer = [
        "settings body laid out at ",
        "settings notice laid out at ",
        "settings reset laid out at ",
        "settings close laid out at ",
    ];
    for mark in footer {
        let moved = marks_between(&labels, enters[1], dumped, mark);
        assert_eq!(
            moved, 0,
            "launch 1: `{mark}…` reported {moved} new layout(s) after the commit of `2` \
             — the footer moved while the card held (the slack goes to the body host, \
             brief 009 R2):\n{stderr}"
        );
    }

    // Launch 1's trace, kept for the growth check after launch 2.
    let launch1 = stderr;

    let dir = settings_scratch(
        "never-shrinks",
        Some("[performance]\nloupe_memory = \"100 GB\"\n"),
    );
    let script = "900:key:ctrl+,;1300:key:ctrl+tab;1600:key:ctrl+tab;2000:dump.perf;\
                  2300:click:settings loupe-memory;2600:key:ctrl+a;2800:key:2;\
                  3000:key:return;3500:dump.unwrapped";
    let stderr = shoot_env_stderr(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out_dir().join("settings-never-shrinks-opened-wrapped.jpg"),
    );
    assert_click_resolved(&stderr, "settings loupe-memory");
    let labels = mark_labels(&stderr);
    let enter = label_positions(&labels, "drive: key:return");
    assert!(
        enter.len() == 1
            && labels[enter[0]..].contains(&"settings committed performance.loupe_memory = 2 GB"),
        "launch 2: Loupe memory was not committed as 2 GB:\n{stderr}"
    );
    assert!(
        hint(&stderr, "perf") && !hint(&stderr, "unwrapped"),
        "launch 2: the hint did not open on the clamp and end on the plain figure — \
         the premise:\n{stderr}"
    );
    assert_eq!(
        height(&stderr, "unwrapped"),
        height(&stderr, "perf"),
        "launch 2: THE CARD SHRANK BELOW THE HEIGHT IT OPENED WITH — {} px after `2` \
         against {} px at the open; the open's own height is the mark's start (brief \
         009 R1):\n{stderr}",
        height(&stderr, "unwrapped"),
        height(&stderr, "perf")
    );
    let dumped = labels
        .iter()
        .position(|l| l.starts_with("QEDUMP unwrapped "))
        .unwrap_or_else(|| panic!("no `dump.unwrapped` mark:\n{stderr}"));
    let after = marks_between(&labels, enter[0], dumped, card);
    assert_eq!(
        after, 0,
        "launch 2: the card laid out {after} time(s) after the commit of `2`:\n{stderr}"
    );
    assert_eq!(
        close_y(&stderr, "unwrapped"),
        close_y(&stderr, "perf"),
        "launch 2: Close moved after the commit of `2`:\n{stderr}"
    );
    for mark in footer {
        let moved = marks_between(&labels, enter[0], dumped, mark);
        assert_eq!(
            moved, 0,
            "launch 2: `{mark}…` reported {moved} new layout(s) after the commit of `2` \
             — the footer moved while the card held:\n{stderr}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
    // The growth, across launches 1 and 2 (QE 2026-10-03, D1): the commit of
    // `100` grows the card to the height an open with the hint already
    // wrapped takes. Powerless where the wrapped hint fits its field (the doc
    // above), never false there.
    assert_eq!(
        height(&launch1, "wrapped"),
        height(&stderr, "perf"),
        "THE CARD DID NOT FOLLOW A TEXT THAT GREW WHILE OPEN — {} px after the commit \
         of `100` wrapped the hint (launch 1) against {} px for an open with the hint \
         already wrapped (launch 2); a commit that grows the content grows the card to \
         the height an open in that state takes (settings.md, \"The card holds still\"; \
         brief 009 R1). The two traces: settings-never-shrinks.trace.log and \
         settings-never-shrinks-opened-wrapped.trace.log in {}",
        height(&launch1, "wrapped"),
        height(&stderr, "perf"),
        out_dir().display()
    );

    // 3 — the cache strand: Linux only, decided at run time (the doc above).
    if !cfg!(target_os = "linux") {
        eprintln!(
            "skipped the cache strand: the default cache cannot be sandboxed off Linux \
             (Windows' known-folder lookup ignores HOME and XDG_CACHE_HOME, and \
             shoot_with_sandboxed_cache refuses without the sandbox); launches 1 and 2 ran"
        );
        return;
    }
    let home = out_dir().join("cache-home-never-shrinks");
    std::fs::remove_dir_all(&home).ok();
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
    let _cleanup = RemoveOnDrop(home.clone());
    // Three directory names of 63 characters each, in hyphenated words — the
    // row's word wrap breaks after a hyphen, never inside a word — so the
    // path the row prints is 221 characters: four lines on every face
    // measured.
    let cache_home = home
        .join(".cache")
        .join("nas-mount-of-the-photo-studio-holding-every-cull-of-this-season")
        .join("weddings-and-sports-and-wildlife-sessions-shot-on-the-a1-bodies")
        .join("a-cache-path-long-enough-to-wrap-the-row-to-four-lines-anywhere");
    let readout_path = format!(
        "~/{}",
        cache_home
            .join("fastcull")
            .join("previews.db")
            .strip_prefix(&home)
            .unwrap()
            .display()
    );
    let script = "900:key:ctrl+,;1200:key:ctrl+tab;1400:key:ctrl+tab;1700:dump.perf;\
                  1900:click:settings clear-cache;2000:wait:settings cache cleared;\
                  3000:dump.cleared";
    let stderr = shoot_with_sandboxed_cache(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("HOME", home.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache_home.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out_dir().join("settings-never-shrinks-clearing.jpg"),
    );
    // The premises: the click landed on Clear, and the row was the wrapped
    // path — shown in full, its row taller than the one-line button whose
    // cell stretches with it — so `Clearing…` shrinks the content.
    assert_click_resolved(&stderr, "settings clear-cache");
    let readout = dump_text(qedump(&stderr, "perf"), "cachereadout");
    assert!(
        readout.contains(&readout_path),
        "launch 3: the row does not show the long path in full — not the wrapped \
         state (expected `{readout_path}`): {readout:?}"
    );
    let (_, _, _, row) = laid_out_at(&stderr, "settings clear-cache", "perf");
    assert!(
        row > 32.0,
        "launch 3: the Clear row is {row} px tall at dump.perf, no taller than its \
         one-line button — the path did not wrap, so `Clearing…` shrinks nothing and \
         this launch would prove nothing:\n{stderr}"
    );
    let labels = mark_labels(&stderr);
    let clicked = labels
        .iter()
        .rposition(|l| *l == "drive: click:settings clear-cache")
        .unwrap_or_else(|| panic!("launch 3: no click on Clear:\n{stderr}"));
    let clearing = labels.iter().position(|l| *l == "settings cache clearing");
    let cleared = labels
        .iter()
        .position(|l| l.starts_with("settings cache cleared "));
    let (clearing, cleared) = match (clearing, cleared) {
        (Some(a), Some(b)) if clicked < a && a < b => (a, b),
        _ => panic!(
            "launch 3: the trace does not show the click, then `settings cache clearing` \
             (the `Clearing…` row), then `settings cache cleared`:\n{stderr}"
        ),
    };
    assert!(
        labels[clicked..cleared].contains(&"settings cache readout shows Clearing…"),
        "launch 3: the row never read `Clearing…` between the click on Clear and \
         `settings cache cleared`, so nothing shrank:\n{stderr}"
    );
    // The claim: while the row read `Clearing…` the card held still.
    for mark in [
        card,
        "settings body laid out at ",
        "settings notice laid out at ",
        "settings reset laid out at ",
        "settings close laid out at ",
    ] {
        let moved = marks_between(&labels, clearing, cleared, mark);
        assert_eq!(
            moved, 0,
            "launch 3: `{mark}…` reported {moved} new layout(s) while the row read \
             `Clearing…` — THE CARD FOLLOWED A TEXT THAT SHRANK WHILE OPEN; its height is \
             a high-water mark for the open (settings.md, \"The card holds still\"; \
             brief 009 R1):\n{stderr}"
        );
    }
    assert!(
        height(&stderr, "cleared") >= height(&stderr, "perf"),
        "launch 3: THE CARD SHRANK WHILE OPEN — {} px after the clear against {} px \
         before it:\n{stderr}",
        height(&stderr, "cleared"),
        height(&stderr, "perf")
    );
}

/// settings.md, "The card holds still" (brief 009 R1 and R3; AC18): while
/// the dialog is open the card GROWS when a text that affects its height
/// changes — here a write error arriving — by exactly the notice line's
/// extra lines, once (the test-integrity review, 2026-10-03; QE D1: with the
/// card made never to grow, a two-line write error on Performance pushed the
/// Clear note out of the visible body, and every test stayed green).
///
/// One launch over a scratch config dir holding a broken file (`[general`):
/// the first open's click on Auto-advance commits and writes — the broken
/// file goes aside as `settings.toml.broken`, a fresh `settings.toml` is
/// written — and `Esc` closes; on the app's own `settings closed` line a
/// helper thread makes the fresh file read-only (the issue #50 anchoring,
/// as the cache test's run 3 does: the drain thread only signals, through
/// an unbounded channel, and never waits; a full second ahead of the
/// reopen on the script's clock, and the verdict does not lean on it). The
/// reopen reads the fresh file and says `settings.toml rewritten — the
/// file that would not read is settings.toml.broken`, one line on every
/// face measured (14 px on both runners in PR #102's traces, 17 here); the
/// second click commits, the write fails, and the notice becomes `Could not
/// write settings.toml: <the OS's words> — the file that would not read is
/// settings.toml.broken`, 117 characters (116 on Windows, whose OS says
/// `Access is denied. (os error 5)`) that wrap in the 524 px cell at 12 px
/// on every face by width. The card's growth between `dump.before` and
/// `dump.after` must equal the notice's, in whole rounded pixels: two
/// measurements on one seat, no height pinned. Close's move is NOT pinned —
/// the card is centred, so a growth moves it by half (brief 009's plan,
/// residual 1).
///
/// The premises, each loud: the clicks reached the box (a pointer echo on
/// it per click) and committed (`settings committed general.auto_advance =
/// false`, then `= true`, then `= false`); the first open's commit moved the
/// broken file aside (the notice says `rewritten`); the notice at
/// `dump.after` says the write failed (a chmod that lost its race, or a seat
/// whose read-only file is still writable, fails THERE, never as a false
/// pass); and that notice is TALLER than the one at `dump.before` — it
/// wrapped, or the launch proves nothing. The file is made writable again
/// before anything is removed: Windows will not delete a read-only file.
///
/// Then the card so grown KEEPS that height (settings.md, "The card holds
/// still": the height is a high-water mark for the open; brief 009's TP-1,
/// landed in brief 010 — AC18): on the app's own `QEDUMP after` line the
/// helper makes the file writable again (a second signal from the drain
/// thread, the first's way), and a third click on Auto-advance commits and
/// WRITES — the notice un-wraps to the one-line `settings.toml rewritten —
/// …`, shorter than the write error. The card at `dump.held` is as tall as
/// at `dump.after`, Close has not moved, and from that third click to the
/// dump neither the card, Reset nor Close reports a new layout. The body
/// host and the notice line are not in that set: the notice shrinks and the
/// slack goes to the host, both by design. This strand is the never-shrinks
/// rule with power on EVERY face — the write error wraps by width on every
/// runner — where `the_settings_card_never_shrinks_while_it_is_open`'s
/// Loupe memory strand has none on DejaVu Sans or Segoe UI and its cache
/// strand runs on Linux only.
///
/// On bef5b5e, the head before brief 009, this test cannot run as written:
/// the `settings notice laid out` mark it reads is brief 009's — this doc
/// called it GREEN there until brief 010 (QE 2026-10-03, D6 in issue #100).
/// What is recorded instead: there the card's height FOLLOWED its content,
/// so the growth held (267 → 283 px, QE's trace of bef5b5e); a card that
/// follows its content follows a text that shrinks too, and that rule in
/// today's tree is the second mutant below — red at the third commit's
/// strand, the card back down by the notice's lost line.
///
/// Mutants (2026-10-03): the card's `changed content` handler deleted AND
/// its height `min(self.high-water, …)` — a card that never grows → "THE
/// CARD DID NOT GROW BY THE NOTICE'S EXTRA LINE: the card went 506 → 506 px
/// while the notice line went 17 → 33 px" — red on Noto Sans; and 497 → 497
/// against 14 → 28 on DejaVu Sans (the ubuntu runner's face, through
/// `SLINT_DEFAULT_FONT`), where the never-shrinks test stays green under it.
/// The card's height without the mark, `min(self.content, …)` — the card
/// following its content, bef5b5e's rule → the growth half green and the
/// third commit's strand red: "THE CARD SHRANK WHILE OPEN — 506 px after
/// the third commit un-wrapped the notice against 522 px with the write
/// error" (Noto Sans, this seat).
#[test]
fn the_settings_card_grows_by_a_write_error_that_wraps_while_it_is_open() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("grows", Some("[general\n"));
    let file = dir.join("settings.toml");
    // The helper waits for the drain thread's signals; when the run ends the
    // senders go with the drain thread, so a mark that never came ends the
    // wait at once rather than at a timeout. It makes the file read-only on
    // the first close and writable again once the write error has been
    // dumped (TP-1's third commit must succeed), and hands back the
    // permissions it replaced, so the test can restore them whatever
    // happened.
    let (closed_tx, closed_rx) = std::sync::mpsc::channel::<()>();
    let (after_tx, after_rx) = std::sync::mpsc::channel::<()>();
    let locker = {
        let file = file.clone();
        std::thread::spawn(move || -> Option<std::fs::Permissions> {
            closed_rx.recv().ok()?;
            let writable = std::fs::metadata(&file).ok()?.permissions();
            let mut readonly = writable.clone();
            readonly.set_readonly(true);
            std::fs::set_permissions(&file, readonly).ok()?;
            if after_rx.recv().is_ok() {
                std::fs::set_permissions(&file, writable.clone()).ok()?;
            }
            Some(writable)
        })
    };
    let script = "900:key:ctrl+,;1300:click:settings auto-advance;1700:dump.first;\
                  2000:key:escape;3000:key:ctrl+,;3400:dump.before;\
                  3700:click:settings auto-advance;4100:dump.after;\
                  4500:click:settings auto-advance;4900:dump.held";
    let (mut closed, mut dumped) = (false, false);
    let stderr = shoot_env_stderr_watching(
        &["--synthetic", "24"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out_dir().join("settings-grows.jpg"),
        // The FIRST close only, and the write error's dump.
        move |line| {
            if !closed && line.contains("] settings closed") {
                closed = true;
                let _ = closed_tx.send(());
            }
            if !dumped && line.contains("] QEDUMP after ") {
                dumped = true;
                let _ = after_tx.send(());
            }
        },
    );
    if let Some(writable) = locker.join().unwrap() {
        std::fs::set_permissions(&file, writable).expect("settings.toml writable again");
    }
    // The premises.
    assert_click_resolved(&stderr, "settings auto-advance");
    let labels = mark_labels(&stderr);
    let clicks = labels
        .iter()
        .filter(|l| l.starts_with("drive ptr click ") && l.ends_with(" (settings auto-advance)"))
        .count();
    let commits: Vec<&str> = labels
        .iter()
        .filter_map(|l| l.strip_prefix("settings committed general.auto_advance = "))
        .collect();
    assert!(
        clicks == 3 && commits == ["false", "true", "false"],
        "the three clicks on Auto-advance did not all land and commit (`false`, `true`, \
         `false`) — {clicks} pointer echo(es), commits {commits:?}:\n{stderr}"
    );
    let notice = |label: &str| dump_text(qedump(&stderr, label), "settingsnote").to_string();
    assert!(
        notice("first").starts_with("settings.toml rewritten"),
        "the first commit did not move the broken file aside, so the write error below \
         would not name it and would not wrap: {:?}",
        notice("first")
    );
    assert!(
        notice("after").starts_with("Could not write settings.toml: "),
        "the second commit's write did not fail — the file was not read-only when it ran \
         (the injection lost its race, or this seat ignores the mode), so nothing below is \
         about a write error: {:?}\n{stderr}",
        notice("after")
    );
    let line = |label: &str| laid_out_at(&stderr, "settings notice", label).3;
    let card = |label: &str| laid_out_at(&stderr, "settings card", label).3;
    assert!(
        line("after") > line("before"),
        "the write error's notice ({} px) is no taller than the one before it ({} px) — it \
         did not wrap, so this launch proves nothing about a growth: {:?}\n{stderr}",
        line("after"),
        line("before"),
        notice("after")
    );
    // The claim.
    assert_eq!(
        card("after") - card("before"),
        line("after") - line("before"),
        "THE CARD DID NOT GROW BY THE NOTICE'S EXTRA LINE: the card went {} → {} px while \
         the notice line went {} → {} px; a text that grows while the dialog is open grows \
         the card by exactly its growth (settings.md, \"The card holds still\"; brief 009 \
         R1, R3):\n{stderr}",
        card("before"),
        card("after"),
        line("before"),
        line("after")
    );
    // TP-1 (AC18): the third commit writes, the notice un-wraps — and the
    // card keeps the height the write error gave it. The premises first.
    assert!(
        notice("held").starts_with("settings.toml rewritten"),
        "the third commit's write did not succeed — the file was not writable again \
         when it ran (the restore lost its race), so nothing below is about a notice \
         that shrank: {:?}\n{stderr}",
        notice("held")
    );
    assert!(
        line("held") < line("after"),
        "the notice at dump.held ({} px) is no shorter than the write error's ({} px) — \
         it did not un-wrap, so this strand proves nothing about a shrink: {:?}\n{stderr}",
        line("held"),
        line("after"),
        notice("held")
    );
    assert_eq!(
        card("held"),
        card("after"),
        "THE CARD SHRANK WHILE OPEN — {} px after the third commit un-wrapped the notice \
         against {} px with the write error; its height is a high-water mark for the open \
         (settings.md, \"The card holds still\"; brief 009 R1, TP-1):\n{stderr}",
        card("held"),
        card("after")
    );
    let close_y = |label: &str| laid_out_at(&stderr, "settings close", label).1;
    assert_eq!(
        close_y("held"),
        close_y("after"),
        "Close moved after the third commit:\n{stderr}"
    );
    let third = label_positions(&labels, "drive: click:settings auto-advance");
    let held_at = labels
        .iter()
        .position(|l| l.starts_with("QEDUMP held "))
        .unwrap_or_else(|| panic!("no `dump.held` mark:\n{stderr}"));
    assert_eq!(third.len(), 3, "not three click steps:\n{stderr}");
    for mark in [
        "settings card laid out at ",
        "settings reset laid out at ",
        "settings close laid out at ",
    ] {
        let moved = marks_between(&labels, third[2], held_at, mark);
        assert_eq!(
            moved, 0,
            "`{mark}…` reported {moved} new layout(s) after the third commit un-wrapped the \
             notice — the card followed a text that shrank (settings.md, \"The card holds \
             still\"):\n{stderr}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// settings.md, "The card holds still", its last rule (brief 009 D4 and D5,
/// the senior developer's call; AC22): below the supported minimum window
/// the BODY gives, never the footer. At 1000x400 — under the 1000x700 floor
/// — in the tallest state (a broken file's two-line notice, the
/// FASTCULL_MAX_READERS line on Read workers), on Performance:
///   * the card is clamped inside the window's modal layer, and Close and
///     Reset lie inside the card at the open and after a wheel;
///   * a wheel over the body scrolls it: the Loupe memory field's mark
///     rises by the wheel's 180 px or less (the body's floor may stop it
///     short), while the grid behind holds `vpy=0.0`. The same wheel moving
///     the body is the control: the grid's stillness is the scrim's
///     containment, not a wheel that went nowhere;
///   * a tab switch puts the body back at its top: on General the
///     auto-advance box is at or below the body's top edge again.
///
/// The wheel is the one coordinate step — the harness has no by-name wheel
/// — so (500,200) is asserted inside the body's own reported rectangle
/// before anything is read from it, loudly. `--synthetic 300` gives the
/// grid room to scroll, so its `vpy=0.0` is a claim and not a floor. The
/// window's landing is its own mark (`wait:window geometry 1000x400`), and
/// the layer is the window less the 26 px status bar, its top read as 0 so
/// the bound holds on Windows, whose menu bar is outside the client area.
///
/// RED on bef5b5e (2026-10-03, debug, this seat): the card clamps to
/// `220,60 560x294` while Close is laid out at `700,551`, 197 px below the
/// card's bottom edge, and the wheel moves nothing — the host was as tall
/// as the active body and could not give.
///
/// Mutants (2026-10-03), each alone: the host's `min-height: 0px` back to
/// `self.preferred-height` → Close outside the card at `dump.perf` — red;
/// the Flickable made a clipping Rectangle (and the viewport reset with it)
/// → the wheel moves nothing — red; the viewport reset taken out of
/// `go-to-tab` → General shows its auto-advance box 180 px above the body's
/// top — red.
///
/// The geometry premise names a REVERTED window as such (brief 009's TP-3,
/// landed in brief 010; issue #100): a satisfied `wait:window geometry
/// 1000x400` promises the layout reached that size, not that it stayed
/// there (test-harness.md) — under a cold build's load the compositor
/// restored 1440x900 some 30 ms after the landing, and the verdicts below
/// then read the restored window as the body/footer defect. So from the
/// LANDING — the first `window geometry 1000x400` mark after the
/// `resize:1000x400` step — to the last dump no `window geometry WxH` other
/// than `1000x400` may be traced, the WxH prefix compared only; its message
/// names the revert. Diagnostic quality: a revert is red either way.
///
/// The range starts at the landing, never at the wait's echo (corrected
/// 2026-10-03, QE round 1 D1): a `wait:` answers "has this happened yet",
/// so a window restored right after the landing still satisfies it, and the
/// restore is traced BEFORE the echo. Under `taskset -c 0,1` and six pinned
/// spinners the restore came 13–31 ms after the landing and some 370 ms
/// before the echo in 16 of 21 of QE's runs, and a range from the echo
/// named none of them — each read as the card outside the modal layer.
#[test]
fn below_the_minimum_window_the_settings_body_gives_before_the_footer() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    let _s = serial();
    let dir = settings_scratch("below-minimum", Some("[general\n"));
    let script = "200:resize:1000x400;600:wait:window geometry 1000x400;900:key:ctrl+,;\
                  1300:key:ctrl+tab;1600:key:ctrl+tab;2000:dump.perf;\
                  2300:wheel.500,200,-180;2800:dump.wheeled;3100:key:ctrl+tab;\
                  3500:dump.general";
    let stderr = shoot_env_stderr(
        &["--synthetic", "300"],
        &[
            ("FASTCULL_TRACE", "1"),
            ("FASTCULL_MAX_READERS", "3"),
            ("FASTCULL_CONFIG_DIR", dir.to_str().unwrap()),
            ("FASTCULL_DRIVE", script),
        ],
        &out_dir().join("settings-below-minimum.jpg"),
    );
    // The premises: the window landed, the tab and the tallest state.
    assert!(
        stderr.contains("wait:window geometry 1000x400 (satisfied"),
        "the window never reached 1000x400 — the premise:\n{stderr}"
    );
    // ... and STAYED there from the landing to the last dump (TP-3). The
    // landing is the first `window geometry 1000x400` mark after the resize
    // step's echo — not the wait's echo, which a revert precedes (the doc).
    let labels = mark_labels(&stderr);
    let resized = labels
        .iter()
        .position(|l| *l == "drive: resize:1000x400")
        .unwrap_or_else(|| panic!("no `resize:1000x400` step echo:\n{stderr}"));
    let landed = resized
        + labels[resized..]
            .iter()
            .position(|l| l.starts_with("window geometry 1000x400 "))
            .unwrap_or_else(|| {
                panic!("no `window geometry 1000x400` landing after the resize step:\n{stderr}")
            });
    let last = labels
        .iter()
        .position(|l| l.starts_with("QEDUMP general "))
        .unwrap_or_else(|| panic!("no `dump.general` mark:\n{stderr}"));
    let reverted: Vec<&str> = labels[landed..last]
        .iter()
        .filter(|l| {
            l.starts_with("window geometry ") && !l.starts_with("window geometry 1000x400 ")
        })
        .copied()
        .collect();
    assert!(
        reverted.is_empty(),
        "THE WINDOW DID NOT STAY AT 1000x400: after it landed (the first `window geometry \
         1000x400` mark after the resize step) it was traced at {reverted:?} before the last \
         dump — the compositor reverted it, so the verdicts below would read a restored \
         window as the body/footer defect (brief 009's TP-3; test-harness.md: a satisfied \
         geometry wait promises the landing, not that the window stays):\n{stderr}"
    );
    let perf = qedump(&stderr, "perf");
    assert_eq!(
        dump_field(perf, "settingstab"),
        "2",
        "not on Performance: {perf}"
    );
    assert_eq!(
        dump_field(perf, "readers"),
        "env:3",
        "the environment's line is not up: {perf}"
    );
    assert!(
        dump_text(perf, "settingsnote").contains("could not be read"),
        "the notice is not the read error — not the tallest state: {perf}"
    );
    // The card in the layer, and the footer in the card — at the open, then
    // again after the wheel.
    let (wx, floor) = (1000.0f32, 400.0f32 - 26.0);
    let (cx, cy, cw, ch) = laid_out_at(&stderr, "settings card", "perf");
    assert!(
        cx >= 0.0 && cx + cw <= wx && cy >= 0.0 && cy + ch <= floor,
        "the card ({cx},{cy} {cw}x{ch}) is not inside the modal layer of a 1000x400 \
         window (which ends at y={floor}):\n{stderr}"
    );
    let footer_inside = |label: &str| {
        let (cx, cy, cw, ch) = laid_out_at(&stderr, "settings card", label);
        for control in ["settings close", "settings reset"] {
            let (x, y, w, h) = laid_out_at(&stderr, control, label);
            assert!(
                x >= cx && x + w <= cx + cw + 0.5 && y >= cy && y + h <= cy + ch + 0.5,
                "dump.{label}: {control} ({x},{y} {w}x{h}) is not inside the card \
                 ({cx},{cy} {cw}x{ch}) at 1000x400 — the footer gave instead of the \
                 body (brief 009 D4):\n{stderr}"
            );
        }
    };
    footer_inside("perf");
    let (bx, by, bw, bh) = laid_out_at(&stderr, "settings body", "perf");
    assert!(
        (bx..=bx + bw).contains(&500.0) && (by..=by + bh).contains(&200.0),
        "the wheel's point (500,200) is not over the body ({bx},{by} {bw}x{bh}), so \
         the wheel below would test something else:\n{stderr}"
    );
    footer_inside("wheeled");
    // The wheel scrolled the body, and only the body.
    let (_, before, _, _) = laid_out_at(&stderr, "settings loupe-memory", "perf");
    let (_, after, _, _) = laid_out_at(&stderr, "settings loupe-memory", "wheeled");
    let rose = before - after;
    assert!(
        rose > 0.0 && rose <= 180.0,
        "a 180 px wheel over the clamped body moved the Loupe memory field by {rose} px \
         ({before} → {after}) — the body does not scroll, so what no longer fits is \
         out of reach:\n{stderr}"
    );
    assert_eq!(
        dump_field(qedump(&stderr, "wheeled"), "vpy"),
        "0.0",
        "the wheel over the Settings body scrolled the grid behind the dialog:\n{stderr}"
    );
    // A switch puts the body back at its top.
    assert_eq!(
        dump_field(qedump(&stderr, "general"), "settingstab"),
        "0",
        "the Ctrl+Tab after the wheel did not wrap to General:\n{stderr}"
    );
    let (_, top, _, _) = laid_out_at(&stderr, "settings body", "general");
    let (_, first, _, _) = laid_out_at(&stderr, "settings auto-advance", "general");
    assert!(
        first >= top,
        "General's first row is at y {first}, above the body's top at {top} — the body \
         kept the scroll from Performance and hides the tab it switched to:\n{stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AC11 and AC12, the app's half (settings.md, "Performance › Thumbnail
/// cache cap" and "Performance › Thumbnail cache"; QE 2026-10-01, D24): a
/// folder open trims the DEFAULT cache to the file's cap, and Clear empties
/// that cache through a connection of its own, off the UI thread, never
/// unlinking it — the row reading `Clearing…` meanwhile and the size
/// re-measured from disk after. Every other driven run but three (the
/// Settings card fit test's cache-on run, the click-away matrix's Clear row
/// and the never-shrinks test's cache strand) is FASTCULL_NO_CACHE; these
/// point the default cache into the shots dir through HOME and
/// XDG_CACHE_HOME (`shoot_with_sandboxed_cache` refuses to run otherwise).
/// That redirect exists on Linux only — Windows' known-folder lookup ignores
/// the environment — so the test skips itself elsewhere, at run time, which
/// keeps the `--list` halves the same on every runner; AC12's Windows half
/// stays review-verified.
///
/// Four runs over one seeded cache (300 thumbnails of 1 MiB each) and one
/// settings file (`cache_cap = "0.1"`, held at the 0.25 GB floor =
/// 268,435,456 bytes, room for 256 of them):
///   1. the folder open alone — afterwards at most 256 seeded rows remain
///      (read after the run: the clear in run 2 empties the table);
///   2. Settings › Performance › Clear — afterwards the table is empty and
///      `previews.db` is the same file, its inode unchanged since the
///      seeding; the row named `~/.cache/fastcull/previews.db` before and a
///      re-measured size in KB after, never `0 B`; and on the one trace
///      stream `settings cache clearing` (the `Clearing…` state) came before
///      `settings cache cleared B -> A`, with A < B. Between the two the
///      worker says where it ran, `settings cache clear ran on
///      settings-clear` (the thread's own name); and the ELEMENTS say what
///      they showed: after the click and before `cleared` the row's
///      `settings cache readout shows Clearing…` and `settings clear-cache
///      enabled false` — the one visible guard against a second VACUUM —
///      and after it `enabled true` and the row showing the re-measured KB.
///      Those two marks cannot be hidden by a fast VACUUM: the click is a
///      timer step, and Slint runs the change trackers right after the
///      timers and before the worker's posted completion can land
///      (Cargo.toml, the fourth canary's fact 8; measured: a 4 ms failed
///      clear still traced both).
///   3. Clear with the database made read-only — previews.db alone, its
///      mode set by a helper thread when the app traces `settings opened`,
///      1.2 s ahead of the click (the issue #50 anchoring), never before
///      launch: the session opens, scans and stores as ever, its connection
///      opened read-write before — and the row then says the clear failed:
///      `Thumbnail cache: could not be cleared (…) — … KB in
///      ~/.cache/fastcull/previews.db` (measured on this seat: `(cache
///      database error: attempt to write a readonly database)`). Its
///      premises come first, each with its own message: the row was fine
///      before the click, and the table still holds the thumbnail the
///      folder open stored — a chmod that lost its race would let the clear
///      empty it and fail THERE, never as a false pass. The mode is restored
///      before the table is read (QE 2026-10-02, round 5: the worker
///      thread, the `Clearing…` row, the disabled button and a failed
///      clear's wording had no guard — each taken out, the suite stayed
///      green).
///   4. Clear HELD, reached by the keyboard (brief 010 R3; settings.md
///      AC12): `FASTCULL_CLEAR_HOLD_MS=3000` holds the worker 3 s before it
///      starts (test-harness.md — announced on stderr, which is asserted).
///      The thumb of `one.ARW` is in memory first (`wait:thumb landed idx
///      0`), and `dump.perf` counts it (`thumbtex` ≥ 1). Four Tabs from the
///      strip on Performance land on Clear and Return starts the clear —
///      `settings cache clearing` after the Return, with neither a Reset
///      nor a close between them: the Tab ring reaches Clear when the cache
///      is on. One second into the hold `dump.during` answers — on the UI
///      thread, while the row reads `Clearing…` — and its line comes BEFORE
///      the worker's own `settings cache clear ran on settings-clear` on the
///      one trace: the clear never blocks the UI thread. A dump after the
///      worker's line is that verdict only if the row's own `settings cache
///      readout shows Clearing…` mark — traced as soon as the Return's
///      handler returns — came after the worker's line too; with that mark
///      before it the UI thread was free and the drive step itself was
///      late, which the run reports as such, red, with no verdict on the UI
///      thread (the senior developer's review F2). After `settings cache
///      cleared`, `dump.cleared` counts as many thumb textures as
///      `dump.perf` did: the open session keeps its painted thumbs.
///
/// The worker's NAME (run 2) proves the worker, not that the UI thread never
/// waits for it — a `join()` right after the spawn would block the UI and
/// still trace `settings-clear`; run 4's held dump is that proof (until
/// brief 010 it was review-verified, and the kept thumbs too, no dump field
/// reading textures).
///
/// Mutants (2026-10-01): session.rs trimming the default cache to
/// `DEFAULT_CAP_BYTES` → all 300 seeded rows survive run 1 — red; the
/// clear worker unlinking the file and opening a fresh one → the inode
/// changes — red. Mutants (2026-10-02), each alone: the clear's closure
/// called inline instead of spawned → `settings cache clear ran on main` —
/// red; the bridge's `Clearing…` assignment removed → no `settings cache
/// readout shows Clearing…` — red; Clear enabled whenever the cache is on
/// (`clear_rx.is_none()` dropped from `present`) → no `settings clear-cache
/// enabled false` — red; the completion re-measuring with no error
/// (`cache_readout(None)`) → run 3's row reads `Thumbnail cache: 92.3 KB
/// in …` as if the clear had worked — red. Mutants (2026-10-03, brief 010),
/// each alone, on run 4: the UI thread `join()`ing the worker right after
/// the spawn → `dump.during` lands after the worker's line, and the row's
/// `Clearing…` mark after it too — red, THE CLEAR BLOCKED THE UI THREAD
/// (re-shown at review F2: the worker's line 3001 ms after the Return, the
/// row's mark after it at 3001–3002, the dump at 3009; a premise on the
/// dump's delay alone, ≥ 3000 there, would have called this defect a late
/// drive step); the premise's probe (review F2, never committed: the
/// script's `dump.during` moved to 3.5 s after the Return) → the row's mark
/// 1 ms after the Return, the dump after the worker's line — red, THE DRIVE
/// STEP ITSELF WAS LATE, where the verdict alone had read THE CLEAR BLOCKED
/// THE UI THREAD; the session's thumb textures dropped when the clear
/// completes (`st.textures.images.clear()` in `on_settings_cache_cleared`)
/// → `thumbtex=0` at `dump.cleared` — red; Clear's ring slot never ok
/// (`slot-ok(5)` false) → the fourth Tab lands on Reset and the Return
/// resets Performance instead — red.
#[test]
fn the_cache_cap_and_clear_cache_reach_the_default_cache() {
    if !has_display() {
        eprintln!("screenshot smoke skipped: no display server");
        return;
    }
    if !cfg!(target_os = "linux") {
        eprintln!("skipped: the default cache cannot be sandboxed off Linux");
        return;
    }
    let _s = serial();
    let home = out_dir().join("cache-home");
    std::fs::remove_dir_all(&home).ok();
    // The seeded cache is 300 MiB: gone however the test ends, a red run
    // included — the shots dir is uploaded as CI's evidence, and the trace
    // logs beside it are what a reader needs, not the database.
    struct RemoveOnDrop(Vec<PathBuf>);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            for dir in &self.0 {
                std::fs::remove_dir_all(dir).ok();
            }
        }
    }
    let _cleanup = RemoveOnDrop(vec![
        home.clone(),
        out_dir().join("settings-cachecap"),
        out_dir().join("cache-folder"),
    ]);
    let cache_home = home.join(".cache");
    let db = cache_home.join("fastcull").join("previews.db");
    let mtime = Some(std::time::SystemTime::now());
    let seeded = |i: usize| PathBuf::from(format!("/seed/{i:03}.ARW"));
    {
        let mut cache = fastcull_core::cache::PreviewCache::open(&db).expect("seed the cache");
        let blob = vec![0u8; 1 << 20];
        for i in 0..300 {
            cache
                .store(
                    &seeded(i),
                    1,
                    mtime,
                    &fastcull_core::exif::ExifSummary::default(),
                    &blob,
                )
                .expect("seed a row");
        }
    }
    let inode = || -> u64 {
        #[cfg(unix)]
        {
            std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&db).expect("previews.db"))
        }
        #[cfg(not(unix))]
        {
            0
        }
    };
    let seeded_inode = inode();
    let config = settings_scratch("cachecap", Some("[performance]\ncache_cap = \"0.1\"\n"));
    let folder = out_dir().join("cache-folder");
    std::fs::remove_dir_all(&folder).ok();
    std::fs::create_dir_all(&folder).unwrap();
    place_fixture(
        &raws_dir().join("A1_full_compressed.ARW"),
        &folder.join("one.ARW"),
    );
    let run = |script: &str, shot: &str| {
        shoot_with_sandboxed_cache(
            &[folder.to_str().unwrap()],
            &[
                ("FASTCULL_TRACE", "1"),
                ("HOME", home.to_str().unwrap()),
                ("XDG_CACHE_HOME", cache_home.to_str().unwrap()),
                ("FASTCULL_CONFIG_DIR", config.to_str().unwrap()),
                ("FASTCULL_DRIVE", script),
            ],
            &out_dir().join(shot),
        )
    };

    // 1 — the folder open trims the cache to the cap.
    let stderr = run(
        "1500:wait:load settled gen 0;1600:dump.loaded",
        "settings-cache-cap.jpg",
    );
    assert!(
        stderr.contains("wait:load settled gen 0 (satisfied"),
        "the folder never settled:\n{stderr}"
    );
    let remaining = {
        let mut cache = fastcull_core::cache::PreviewCache::open(&db).expect("reopen the cache");
        (0..300)
            .filter(|i| matches!(cache.lookup(&seeded(*i), 1, mtime), Ok(Some(_))))
            .count()
    };
    assert!(
        remaining <= 256,
        "{remaining} of the 300 seeded 1 MiB thumbnails survived a folder open \
         under a 0.25 GB cap — the cap was not enforced on the default cache:\n{stderr}"
    );

    // 2 — Clear.
    let stderr = run(
        "1500:wait:load settled gen 0;1600:key:ctrl+,;1900:key:ctrl+tab;2100:key:ctrl+tab;\
         2500:dump.perf;2800:click:settings clear-cache;2900:wait:settings cache cleared;\
         3300:dump.cleared",
        "settings-cache-clear.jpg",
    );
    assert_click_resolved(&stderr, "settings clear-cache");
    let before = dump_text(qedump(&stderr, "perf"), "cachereadout").to_string();
    assert!(
        before.starts_with("Thumbnail cache: ")
            && before.ends_with(" in ~/.cache/fastcull/previews.db"),
        "the row does not name the sandboxed default cache in the spec's `~/` \
         form: {before:?}"
    );
    let labels = mark_labels(&stderr);
    let clearing = labels.iter().position(|l| *l == "settings cache clearing");
    let cleared = labels
        .iter()
        .position(|l| l.starts_with("settings cache cleared "));
    assert!(
        clearing.is_some() && cleared.is_some() && clearing < cleared,
        "the trace does not show `settings cache clearing` (the `Clearing…` \
         row) before `settings cache cleared`:\n{stderr}"
    );
    // The clear ran on its own named worker, between the two: a clear run
    // inline on the UI thread reads `… ran on main`.
    let ran = labels
        .iter()
        .position(|l| *l == "settings cache clear ran on settings-clear");
    assert!(
        ran.is_some() && clearing < ran && ran < cleared,
        "the trace does not say the clear ran on the `settings-clear` worker between \
         `settings cache clearing` and `settings cache cleared` (at {ran:?}; the \
         clear's own line: {:?}):\n{stderr}",
        labels
            .iter()
            .find(|l| l.starts_with("settings cache clear ran on "))
    );
    // What the ELEMENTS showed (their own marks): between the click and the
    // worker's completion the row read `Clearing…` and Clear was disabled;
    // after it, Clear was offered again and the row read the re-measured
    // size.
    let clicked = labels
        .iter()
        .rposition(|l| *l == "drive: click:settings clear-cache")
        .unwrap_or_else(|| panic!("no click on Clear:\n{stderr}"));
    let cleared_at = cleared.unwrap_or(labels.len());
    let during = &labels[clicked.min(cleared_at)..cleared_at];
    for mark in [
        "settings cache readout shows Clearing…",
        "settings clear-cache enabled false",
    ] {
        assert!(
            during.contains(&mark),
            "no `{mark}` between the click on Clear and `settings cache cleared` — the \
             row did not say Clearing…, or Clear stayed offered while it ran:\n{stderr}"
        );
    }
    let after_clear = &labels[cleared_at..];
    assert!(
        after_clear.contains(&"settings clear-cache enabled true"),
        "Clear was not offered again after the clear:\n{stderr}"
    );
    assert!(
        after_clear.iter().any(|l| {
            l.strip_prefix("settings cache readout shows Thumbnail cache: ")
                .and_then(|rest| rest.strip_suffix(" KB in ~/.cache/fastcull/previews.db"))
                .is_some_and(|kb| kb.parse::<f64>().is_ok_and(|kb| kb > 0.0))
        }),
        "after the clear the row did not SHOW a re-measured size in KB:\n{stderr}"
    );
    let (b, a) = cleared
        .and_then(|i| labels[i].strip_prefix("settings cache cleared "))
        .and_then(|s| s.split_once(" -> "))
        .and_then(|(b, a)| Some((b.parse::<u64>().ok()?, a.parse::<u64>().ok()?)))
        .unwrap_or_else(|| panic!("malformed `settings cache cleared` mark:\n{stderr}"));
    assert!(a < b, "Clear did not shrink the cache: {b} -> {a} bytes");
    let after = dump_text(qedump(&stderr, "cleared"), "cachereadout").to_string();
    let size = after
        .strip_prefix("Thumbnail cache: ")
        .and_then(|s| s.strip_suffix(" KB in ~/.cache/fastcull/previews.db"))
        .and_then(|n| n.parse::<f64>().ok());
    assert!(
        size.is_some_and(|kb| kb > 0.0),
        "after Clear the row does not read a re-measured size in KB (an empty \
         database's few tens of KB, never `0 B`): {after:?}"
    );
    let rows = fastcull_core::cache::PreviewCache::open(&db)
        .and_then(|cache| cache.len())
        .expect("reopen the cache");
    assert_eq!(rows, 0, "Clear left rows in the table");
    assert_eq!(
        inode(),
        seeded_inode,
        "previews.db is a different file after Clear — it was unlinked and \
         recreated under the live session (catalog-cache.md's lock rule)"
    );

    // 3 — a clear that FAILS says so in the row. The database alone (never
    // its -wal or -shm, never before launch: the session must open, scan
    // and store as ever) is made read-only once the dialog is open — anchored
    // on the app's own `settings opened` line, 1.2 s ahead of the click, the
    // issue #50 way — so the clear's own connection cannot write and the
    // session is untouched: its connection was opened read-write before.
    let writable = std::fs::metadata(&db).expect("previews.db").permissions();
    let (opened_tx, opened_rx) = std::sync::mpsc::channel::<()>();
    let locker = {
        let db = db.clone();
        std::thread::spawn(move || {
            if opened_rx.recv().is_ok() {
                let mut readonly = std::fs::metadata(&db).unwrap().permissions();
                readonly.set_readonly(true);
                std::fs::set_permissions(&db, readonly).unwrap();
            }
        })
    };
    let mut signalled = false;
    let stderr = shoot_with_sandboxed_cache_watching(
        &[folder.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            ("HOME", home.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache_home.to_str().unwrap()),
            ("FASTCULL_CONFIG_DIR", config.to_str().unwrap()),
            (
                "FASTCULL_DRIVE",
                "1500:wait:load settled gen 0;1600:key:ctrl+,;1900:key:ctrl+tab;\
                 2100:key:ctrl+tab;2500:dump.perf;2800:click:settings clear-cache;\
                 2900:wait:settings cache cleared;3300:dump.failed",
            ),
        ],
        &out_dir().join("settings-cache-clear-fails.jpg"),
        move |line| {
            if !signalled && line.contains("] settings opened") {
                signalled = true;
                let _ = opened_tx.send(());
            }
        },
    );
    locker.join().unwrap();
    // Writable again before anything reads it (the table check opens it).
    std::fs::set_permissions(&db, writable).expect("previews.db writable again");
    assert_click_resolved(&stderr, "settings clear-cache");
    // The premises, each with its own message: the row was fine before the
    // click — the failure is the clear's own — and the clear really was
    // refused: the folder open stored one.ARW's thumbnail, and a chmod that
    // lost the race would have let the clear empty the table.
    let fine = dump_text(qedump(&stderr, "perf"), "cachereadout").to_string();
    assert!(
        fine.starts_with("Thumbnail cache: ") && !fine.contains("could not be cleared"),
        "the row already said something was wrong before Clear: {fine:?}"
    );
    let rows = fastcull_core::cache::PreviewCache::open(&db)
        .and_then(|cache| cache.len())
        .expect("reopen the cache");
    assert!(
        rows >= 1,
        "the clear emptied the table: the database was not read-only when it ran \
         (the injection lost its race), so the row below would prove nothing:\n{stderr}"
    );
    // The contract (settings.md, "Thumbnail cache": a clear that fails says
    // so in the row) — and the size beside it is still re-measured.
    let failed = dump_text(qedump(&stderr, "failed"), "cachereadout").to_string();
    assert!(
        failed.starts_with("Thumbnail cache: could not be cleared (")
            && failed.ends_with(" KB in ~/.cache/fastcull/previews.db"),
        "a clear that failed does not say so in the row (settings.md, \"Thumbnail \
         cache\"): {failed:?}"
    );
    assert_eq!(
        inode(),
        seeded_inode,
        "previews.db is a different file after the failed clear"
    );

    // 4 — Clear HELD on its worker (FASTCULL_CLEAR_HOLD_MS), reached by the
    // Tab ring: the UI thread answers during the hold, and the session keeps
    // its painted thumbs (settings.md AC12; brief 010). The hold is three
    // times the dump's offset from the Return.
    let stderr = shoot_with_sandboxed_cache(
        &[folder.to_str().unwrap()],
        &[
            ("FASTCULL_TRACE", "1"),
            ("HOME", home.to_str().unwrap()),
            ("XDG_CACHE_HOME", cache_home.to_str().unwrap()),
            ("FASTCULL_CONFIG_DIR", config.to_str().unwrap()),
            ("FASTCULL_CLEAR_HOLD_MS", "3000"),
            (
                "FASTCULL_DRIVE",
                "1500:wait:load settled gen 0;1600:wait:thumb landed idx 0;1700:key:ctrl+,;\
                 2000:key:ctrl+tab;2200:key:ctrl+tab;2600:dump.perf;2900:key:tab;\
                 3100:key:tab;3300:key:tab;3500:key:tab;3700:key:return;4700:dump.during;\
                 4800:wait:settings cache cleared;5200:dump.cleared",
            ),
        ],
        &out_dir().join("settings-cache-clear-held.jpg"),
    );
    for wait in [
        "wait:load settled gen 0 (satisfied",
        "wait:thumb landed idx 0 (satisfied",
        "wait:settings cache cleared (satisfied",
    ] {
        assert!(
            stderr.contains(wait),
            "run 4: `{wait}` never fired:\n{stderr}"
        );
    }
    assert!(
        stderr.contains("fastcull: FASTCULL_CLEAR_HOLD_MS=3000 — every cache clear is held"),
        "run 4: the hold knob did not announce itself on stderr (test-harness.md), so \
         the worker may not have been held at all:\n{stderr}"
    );
    let perf = qedump(&stderr, "perf");
    assert_eq!(
        dump_field(perf, "settingstab"),
        "2",
        "run 4: not on Performance: {perf}"
    );
    let labels = mark_labels(&stderr);
    // The Tab ring reached Clear: the four Tabs ran, and the Return started
    // the clear — no Reset, no close, between them.
    let tabs = label_positions(&labels, "drive: key:tab");
    let ret = labels
        .iter()
        .rposition(|l| *l == "drive: key:return")
        .unwrap_or_else(|| panic!("run 4: no Return:\n{stderr}"));
    let clearing = labels
        .iter()
        .position(|l| *l == "settings cache clearing")
        .unwrap_or_else(|| {
            panic!(
                "run 4: the Return after four Tabs started no clear — the Tab ring did not \
                 reach Clear with the cache on (settings.md AC12):\n{stderr}"
            )
        });
    assert!(
        tabs.len() == 4 && tabs[3] < ret && ret < clearing,
        "run 4: not four Tabs, then the Return, then `settings cache clearing` (Tabs at \
         {tabs:?}, the Return at {ret}, the clearing at {clearing}):\n{stderr}"
    );
    assert!(
        !labels[ret..clearing]
            .iter()
            .any(|l| *l == "settings reset performance" || *l == "settings closed"),
        "run 4: the Return reset or closed before the clear began — the ring was not on \
         Clear:\n{stderr}"
    );
    // The clear never blocks the UI thread: one second into the hold the UI
    // thread answered a dump while the row read `Clearing…`, BEFORE the
    // worker's own line.
    let during = labels
        .iter()
        .position(|l| l.starts_with("QEDUMP during "))
        .unwrap_or_else(|| panic!("run 4: no `dump.during` mark:\n{stderr}"));
    let ran = labels
        .iter()
        .position(|l| *l == "settings cache clear ran on settings-clear")
        .unwrap_or_else(|| panic!("run 4: the worker never said where it ran:\n{stderr}"));
    // The premise of that verdict (the senior developer's review F2): a dump
    // that comes after the worker's line convicts the UI thread only if the
    // UI thread was silent from the Return to that line, and the dump's own
    // delay cannot say so — a UI thread that waits for the worker makes the
    // drive step late by the whole hold too (measured under the `join()`
    // mutant: the dump 3009 ms after the Return, the worker's line at 3001).
    // The witness is the row's own `settings cache readout shows Clearing…`
    // mark: Slint traces it in the same loop turn, as soon as the timer
    // callback that ran the Return has returned (Cargo.toml, the fourth
    // canary's fact 8). It comes after the worker's line when that callback
    // waited for the worker (the `join()` mutant: in the worker's own
    // millisecond or the next, after its line), and about 3 s before it when
    // the UI thread was free and only the dump's timer fired late (measured
    // with the dump moved 3.5 s after the Return: 1 ms after the Return). So
    // a dump after the worker's line with that mark before it is a late
    // drive step — red, with no verdict on the UI thread. One shape this
    // cannot name: a UI thread blocked by the clear LATER than the Return's
    // handler (a blocking poll, say) shows that mark before the worker's
    // line and is reported as a late drive step — red either way, as it
    // would be under a premise on the dump's delay.
    let stamps = mark_stamps(&stderr);
    let after_return = |i: usize| stamps[i].saturating_sub(stamps[ret]);
    let shown = labels[clearing..]
        .iter()
        .position(|l| *l == "settings cache readout shows Clearing…")
        .map(|i| clearing + i);
    assert!(
        !(ran < during && shown.is_some_and(|shown| shown < ran)),
        "run 4: THE DRIVE STEP ITSELF WAS LATE — no verdict on the UI thread. The dump due \
         1000 ms after the Return came {} ms after it, after the worker's own line ({} ms \
         after it); but the row's own `settings cache readout shows Clearing…` mark, which \
         Slint traces as soon as the timer callback that ran the Return returns (Cargo.toml, \
         the fourth canary's fact 8), came {} ms after the Return — before the worker's line, \
         so the UI thread was free once the Return was handled and it was the drive's timer \
         that fired late, not the clear that blocked:\n{stderr}",
        after_return(during),
        after_return(ran),
        shown.map_or(0, after_return),
    );
    assert!(
        clearing < during && during < ran,
        "THE CLEAR BLOCKED THE UI THREAD: the dump due one second into the worker's \
         3 s hold came at {during}, after the worker's own line at {ran} (the clearing \
         at {clearing}) — the UI thread waited for the worker (settings.md, \
         \"Performance › Thumbnail cache\": never the UI thread):\n{stderr}"
    );
    assert_eq!(
        dump_text(qedump(&stderr, "during"), "cachereadout"),
        "Clearing…",
        "run 4: the row did not read `Clearing…` during the hold:\n{stderr}"
    );
    // The open session keeps its painted thumbs.
    let held = dump_field(perf, "thumbtex")
        .parse::<usize>()
        .unwrap_or_else(|e| panic!("run 4: unreadable thumbtex at dump.perf ({e}): {perf}"));
    let cleared = qedump(&stderr, "cleared");
    assert!(
        held >= 1,
        "run 4: the premise: no thumb texture was in memory before the clear, so the \
         count after it proves nothing: {perf}"
    );
    assert_eq!(
        dump_field(cleared, "thumbtex"),
        held.to_string(),
        "run 4: the open session lost painted thumbs to Clear cache — it keeps them \
         (settings.md, \"Performance › Thumbnail cache\"): {cleared}"
    );
}
