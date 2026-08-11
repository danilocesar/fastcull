//! Screenshot shutter and shutdown: the readiness gate that decides WHEN a
//! `--screenshot` run may photograph the window, the JPEG writer, and the
//! bounded sidecar flush the process exits through.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use fastcull_core::grid::GridLayout;
use fastcull_core::loupe::is_top_rung;
use slint::ComponentHandle;

use crate::state::AppState;
use crate::trace::trace_mark;
use crate::MainWindow;

/// Arm the screenshot shutter: a 250 ms poll that waits for content
/// readiness (and for the drive script to finish), then snapshots and quits.
/// Returns the timer — dropping it cancels the poll, so main.rs holds it —
/// and the "the file was written" flag [`finish`] checks.
pub(crate) fn arm(
    window: &MainWindow,
    state: &Rc<RefCell<AppState>>,
    screenshot: Option<std::path::PathBuf>,
    drives_pending: &Rc<Cell<usize>>,
) -> (slint::Timer, Rc<Cell<bool>>) {
    // Screenshot mode: wait for content readiness, then snapshot and quit.
    // Deterministic (validator finding: a fixed delay captured the fit view
    // as the "1:1" frame on slow/debug runs): thumbs settle >=1.5 s, and in
    // --start-11 mode the cursor's FULL-RES texture must be adopted before
    // the shutter fires. If it never is, FAIL LOUDLY instead of capturing
    // the fit frame as the "1:1" (CI diagnosis 2026-07-25: the old
    // fire-anyway 15 s cap produced a confusing diff-is-zero test failure on
    // slow Windows debug runners); the cap is generous because a debug-build
    // 50 MP decode on a virtualized runner is legitimately slow.
    let shot_timer = slint::Timer::default();
    let shot_written = Rc::new(std::cell::Cell::new(false));
    if let Some(out) = screenshot {
        let win = window.as_weak();
        let state_rc = Rc::clone(state);
        let shot_written = Rc::clone(&shot_written);
        let started = std::time::Instant::now();
        let drives_pending = Rc::clone(drives_pending);
        shot_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                let Some(win) = win.upgrade() else { return };
                let elapsed = started.elapsed();
                // The DRIVE script must have fully executed: a fast
                // release build reaches readiness before late-scheduled
                // actions fire, and capturing a half-driven state makes
                // the same test mean different things per profile.
                if drives_pending.get() > 0 {
                    return;
                }
                let (one2one_ready, fit_ready) = {
                    let st = state_rc.borrow();
                    let one2one = st.loupe_view.zoom_factor <= 1.0
                        || st.textures.fullres.iter().any(|(i, img)| {
                            // A terminal small texture IS the top rung
                            // (bare JPEGs, issue #8 — QE D2: the 60s
                            // refusal hit every small-JPEG --start-11 run),
                            // which is why the predicate is shared.
                            *i == st.grid.cursor
                                && is_top_rung(
                                    img.size().width.max(img.size().height),
                                    st.textures.terminal_native.contains(&st.grid.cursor),
                                )
                        });
                    // At LOUPE FIT the old gate was vacuous (zoom_factor is
                    // 1.0), so the shutter fired on the bare 1.5 s floor —
                    // racing the loupe decodes. Two consecutive Windows CI
                    // runs ~60% slower than usual lost that race two
                    // DIFFERENT ways (PR #29, 2026-08-01): one snapshot
                    // caught the PLACEHOLDER ("no pillarbox bars"), the
                    // rerun caught a blurry UPSCALED THUMB (photo variance
                    // 99.2 against the suite's 100 floor, ~3300 normal). So
                    // the gate waits for the MID-or-better tier — the
                    // texture fit actually settles on — not merely any
                    // texture; a thumb stretched across the loupe is still
                    // the wrong state to photograph. Scoped: synthetic
                    // sessions never produce textures, a failed cursor
                    // never will (the loupe's Failed event fills
                    // `st.textures.failed`), a terminal small file's whole-file rung
                    // is adopted into the fullres slot (issue #8), and an
                    // empty view has no cursor — all keep the old
                    // behaviour or they would hang into the 60 s cap.
                    let at_loupe = st.at_loupe();
                    let fit = !at_loupe
                        || st.session.synthetic
                        || st.grid.view.is_empty()
                        || st.textures.failed.contains(&st.grid.cursor)
                        || st
                            .textures
                            .fullres
                            .iter()
                            .any(|(i, _)| *i == st.grid.cursor)
                        || st.textures.mids.contains_key(&st.grid.cursor);
                    (one2one, fit)
                };
                let ready = one2one_ready && fit_ready;
                if !ready && elapsed > std::time::Duration::from_secs(60) {
                    eprintln!(
                        "screenshot: the loupe frame's texture never arrived \
                         within 60 s ({}) — refusing to capture the wrong state",
                        if one2one_ready {
                            "fit view still on the placeholder"
                        } else {
                            "full-res never adopted for the 1:1 frame"
                        }
                    );
                    slint::quit_event_loop().ok();
                    std::process::exit(1);
                }
                if elapsed < std::time::Duration::from_millis(1500) || !ready {
                    return;
                }
                // The status line at shutter time, for text-level test
                // assertions — status strings were otherwise untestable
                // and the "(0/1)" fabrication (issue #19) survived two
                // human reviews because nothing could assert them.
                trace_mark(&format!("status at shutter: {}", win.get_status()));
                // Geometry at shutter, in LOGICAL px. Pixel measurements of
                // the rendered frame are resolution- and DPI-dependent and
                // have twice broken on the Windows runner while the app was
                // behaving correctly; the "whole frame is on screen"
                // requirement is a statement about numbers, so state the
                // numbers and let tests assert them.
                {
                    let st = state_rc.borrow();
                    let layout = GridLayout::new(
                        st.grid.zoom,
                        win.get_grid_width(),
                        win.get_grid_height(),
                        st.grid.view.len(),
                    );
                    let scroll = (-win.get_vp_y()).max(0.0);
                    let cursor_top = st.cursor_pos().map(|p| layout.position(p).1).unwrap_or(0.0);
                    trace_mark(&format!(
                        "geometry at shutter: columns {} cell {:.0}x{:.0} grid {:.0}x{:.0} \
                         scroll {scroll:.0} cursor-top {cursor_top:.0}",
                        layout.columns,
                        layout.cell_width,
                        layout.cell_height,
                        win.get_grid_width(),
                        win.get_grid_height(),
                    ));
                }
                match win.window().take_snapshot() {
                    Ok(buf) => {
                        let ok = write_snapshot_jpeg(&out, &buf);
                        shot_written.set(ok);
                        slint::quit_event_loop().ok();
                        if !ok {
                            eprintln!("screenshot: failed to write {}", out.display());
                            std::process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("screenshot: {e}");
                        slint::quit_event_loop().ok();
                        std::process::exit(1);
                    }
                }
            },
        );
    }
    (shot_timer, shot_written)
}

/// Fail a `--screenshot` run that reached the end of the event loop
/// without producing its file.
pub(crate) fn finish(screenshot_requested: bool, shot_written: &Rc<Cell<bool>>) {
    // Screenshot mode must NEVER exit 0 without its file: if anything ends
    // the event loop before the shutter fires (window closed under load —
    // validator-observed flake), the harness would otherwise see a clean
    // exit and fail later with a bare file-not-found.
    if screenshot_requested && !shot_written.get() {
        eprintln!(
            "screenshot: event loop ended before the snapshot was captured \
             (window closed early?) — failing instead of exiting clean"
        );
        std::process::exit(2);
    }
}

/// The shutdown path (recorded, 01-architecture.md): flush the sidecar
/// writer under a watchdog, then exit — never join the read workers.
pub(crate) fn shutdown(state: &Rc<RefCell<AppState>>) {
    // Shutdown policy (recorded, 01-architecture.md): the ONLY thing that
    // must complete is the sidecar flush. Pipeline/loupe workers are
    // read-only and the cache is crash-safe (WAL), so we exit without
    // joining them - 32 readers stuck in kernel I/O on a dying card once
    // held the process un-killable for minutes (user report; the process
    // even survived SIGKILL until the card responded). A watchdog bounds
    // the flush itself in case the sidecars live on that same dead card.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(8));
        eprintln!("fastcull: shutdown watchdog fired - storage not responding; exiting");
        std::process::exit(1);
    });
    let writer = state.borrow_mut().session.writer.take();
    drop(writer); // drains every pending sidecar write, then joins
    std::process::exit(0);
}

/// Snapshot writer: always JPEG q92 regardless of the output extension
/// (recorded in ui-grid.md — lossless PNG would need an extra dependency and
/// smoke comparisons don't require it). RGBA in, RGB JPEG out.
fn write_snapshot_jpeg(
    out: &std::path::Path,
    buf: &slint::SharedPixelBuffer<slint::Rgba8Pixel>,
) -> bool {
    let rgb: Vec<u8> = buf
        .as_slice()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b])
        .collect();
    let mut data = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut data, 92);
    if enc
        .encode(
            &rgb,
            buf.width() as u16,
            buf.height() as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .is_err()
    {
        return false;
    }
    std::fs::write(out, data).is_ok()
}
