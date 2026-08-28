//! The FASTCULL_DRIVE test harness: the scripted-action interpreter that
//! makes the app drivable headlessly (timed nav keys, real pointer and key
//! events, session swaps, window resizes, and the QEDUMP state dumps).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use slint::ComponentHandle;

use crate::nav::handle_nav;
use crate::presenter::refresh;
use crate::session::open_folder_at;
use crate::state::AppState;
use crate::trace::trace_mark;
use crate::MainWindow;

/// Schedule every step of the FASTCULL_DRIVE script (a no-op when the
/// variable is unset). Returns the not-yet-fired counter the screenshot
/// shutter waits on, so a scripted run means the same thing in every
/// build profile.
pub(crate) fn install(window: &MainWindow, state: &Rc<RefCell<AppState>>) -> Rc<Cell<usize>> {
    // FASTCULL_DRIVE="6000:one2one;12000:grid;15000:quit": timed nav
    // injection for headless hang debugging (companion to FASTCULL_TRACE —
    // no display-automation tooling needed on Wayland). The screenshot
    // shutter WAITS for the whole script (drives_pending below): on a
    // fast release build the readiness gate can otherwise open before
    // late-scheduled actions fire, capturing a half-driven state — the
    // same script must mean the same shot in every profile.
    let drives_pending = Rc::new(std::cell::Cell::new(0usize));
    if let Ok(script) = std::env::var("FASTCULL_DRIVE") {
        for step in script.split(';') {
            let Some((ms, key)) = step.split_once(':') else {
                continue;
            };
            let Ok(ms) = ms.trim().parse::<u64>() else {
                continue;
            };
            let key = key.trim().to_string();
            let win = window.as_weak();
            let state = Rc::clone(state);
            drives_pending.set(drives_pending.get() + 1);
            let pending = Rc::clone(&drives_pending);
            slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
                pending.set(pending.get().saturating_sub(1));
                let Some(win) = win.upgrade() else { return };
                trace_mark(&format!("drive: {key}"));
                if key == "quit" {
                    slint::quit_event_loop().ok();
                    return;
                }
                if key == "iptc" {
                    // Panel toggle for the screenshot harness (issue #12:
                    // the docking bug shipped because no automated run
                    // could reach the panel-open state).
                    win.invoke_iptc_toggle();
                    return;
                }
                if key == "about" || key == "shortcuts" {
                    // Modal toggles for the containment tests (issue #23:
                    // a stray N while a popup is up must never reject —
                    // untestable headlessly without a way to open them).
                    let visible = if key == "about" {
                        let v = !win.get_about_visible();
                        win.set_about_visible(v);
                        v
                    } else {
                        let v = !win.get_shortcuts_visible();
                        win.set_shortcuts_visible(v);
                        v
                    };
                    if visible {
                        // The SHIPPED steal path, same as the menu items
                        // (issue #41: these toggles used to call a bare
                        // focus-keys(), replicating the intended menu
                        // behavior rather than the real one — the
                        // fidelity trap issue #13 records; menu-restore
                        // fidelity itself needs the click. token).
                        win.invoke_modal_opened();
                    }
                    trace_mark(&format!("{key} toggled to {visible}"));
                    return;
                }
                if let Some(at) = key.strip_prefix("dblclick:") {
                    // dblclick:X,Y (view-area logical px) — replays Slint's
                    // REAL dispatch order for a double-click above fit: a
                    // `clicked` that re-centers, then `double-clicked` on
                    // the SAME release. That ordering is the whole bug
                    // class — the shipped proximity guard compared two
                    // image fractions taken either side of the recenter and
                    // silently vetoed every double-click above fit
                    // (validator FAIL-1 / QE D1, 2026-07-30). No core test
                    // could see it: the machine was always right, the
                    // bridge was not. This does NOT inject real pointer
                    // events — which Slint surface receives a press is
                    // still review-verified only (issue #13).
                    if let Some((x, y)) = at.split_once(',') {
                        if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
                            if win.get_one2one() {
                                // BOTH releases fire `clicked` (Slint calls
                                // it per release and adds `double_clicked`
                                // when click_count is odd), and the second
                                // press is the SAME physical screen point —
                                // which by then maps to a different image
                                // fraction, because the first click already
                                // re-centred and refreshed. Replaying only
                                // one click leaves the trap unarmed and the
                                // test vacuous (caught by mutation).
                                let frac = |w: &MainWindow| {
                                    let (lw, lh) = (w.get_loupe_w(), w.get_loupe_h());
                                    let (ox, oy) = (w.get_loupe_vx(), w.get_loupe_vy());
                                    (
                                        ((x - ox) / lw.max(1.0)).clamp(0.0, 1.0),
                                        ((y - oy) / lh.max(1.0)).clamp(0.0, 1.0),
                                    )
                                };
                                let (fx, fy) = frac(&win);
                                win.invoke_loupe_clicked(fx, fy);
                                let (fx2, fy2) = frac(&win); // AFTER the recentre
                                win.invoke_loupe_clicked(fx2, fy2);
                                win.invoke_zoom_double_clicked(x, y);
                            } else {
                                win.invoke_fit_clicked();
                                win.invoke_fit_clicked();
                                win.invoke_fit_double_clicked(x, y);
                            }
                        }
                    }
                    return;
                }
                if let Some(px) = key.strip_prefix("scroll:") {
                    // scroll:N — browse the grid to offset N logical px
                    // WITHOUT claiming the cursor, i.e. what the wheel does
                    // (the Flickable handles it natively, so Rust never
                    // hears about it and `cursor_touched` stays false).
                    // Added 2026-07-31: the harness had no scroll-without-
                    // claim action, which is exactly why a re-anchor that
                    // yanked a browsing user's viewport got through review.
                    if let Ok(y) = px.parse::<f32>() {
                        win.set_vp_y(-y.max(0.0));
                        refresh(&win, &state);
                    }
                    return;
                }
                if let Some(path) = key.strip_prefix("open:") {
                    // open:PATH — the Open Folder menu action minus the
                    // native rfd dialog (issue #34: an app-level session
                    // swap mid-operation was untestable headlessly — the
                    // kitchen's generation fence, the pipeline/loupe
                    // restart and the marks flush were unit- or
                    // review-verified only). Same shared function as the
                    // menu path, so this drives the REAL swap, and like
                    // the menu bar it stays live while a modal is up
                    // (harness plumbing, not a nav key). The path is
                    // everything after the first colon, so drive scripts
                    // cannot open a path containing `;` — fine for a test
                    // harness, recorded in ui-grid.md.
                    open_folder_at(&win, &state, std::path::Path::new(path));
                    return;
                }
                if let Some(text) = key.strip_prefix("copytemplate:") {
                    // copytemplate:TEXT — type a rename template without
                    // the pointer gymnastics of focusing the LineEdit and
                    // sending one key event per character (the `copydest:`
                    // reasoning). Replans exactly as the field's `edited`
                    // callback does, so the preview and the plan are the
                    // ones a real keystroke would produce.
                    win.set_copy_template(text.into());
                    win.invoke_copy_replan();
                    return;
                }
                if let Some(path) = key.strip_prefix("copydest:") {
                    // copydest:PATH — the Copy Picks destination picker
                    // minus the native rfd dialog (the `open:` reasoning):
                    // a driven copy → hand-delete → copy run was otherwise
                    // unreachable headlessly, which is exactly where the
                    // 2026-08-21 re-run bug shipped. Sets the destination
                    // the dialog shows on its next Ctrl+E (the open path
                    // keeps an already-chosen destination over ui.toml).
                    state.borrow_mut().copy.dest = Some(std::path::PathBuf::from(path));
                    return;
                }
                if let Some(path) = key.strip_prefix("clipdest:") {
                    // clipdest:PATH — the video export's destination
                    // picker minus the native rfd dialog, exactly like
                    // `copydest:` and for the same reason: without it the
                    // whole export flow (plan, clash question, the file on
                    // disk) is unreachable headlessly, and this is the one
                    // operation that writes a new kind of file.
                    state.borrow_mut().clip.dest = Some(std::path::PathBuf::from(path));
                    return;
                }
                if let Some(spec) = key.strip_prefix("key:") {
                    // key:<k> / key:ctrl+<k> — dispatch a REAL key press +
                    // release through `slint::Window::dispatch_event`, i.e.
                    // through the true focus system. The nav tokens above
                    // call handle_nav directly and BYPASS focus entirely,
                    // which is why the issue #41 class (keyboard stranded
                    // when a focused editor is destroyed or covered) stayed
                    // green under every driven test: only a dispatched
                    // event can land on no element. Named keys cover what
                    // the regression tests need; anything else is sent as
                    // literal text (so `key:k` types k, `key:+` zooms).
                    // `ctrl+` synthesizes a held Control around the press,
                    // which Slint folds into `event.modifiers` (issue #13).
                    use slint::platform::{Key, WindowEvent};
                    let (ctrl, rest) = match spec.strip_prefix("ctrl+") {
                        Some(rest) if !rest.is_empty() => (true, rest),
                        _ => (false, spec),
                    };
                    // `shift+` after `ctrl+`, so `key:ctrl+shift+e` is a
                    // real chord: Ctrl+Shift+E (the video export) has to
                    // be drivable, and it must reach the app the way the
                    // keyboard sends it — the letter plus BOTH modifiers,
                    // which is exactly what makes it distinguishable from
                    // Ctrl+E (Copy Picks).
                    let (shift, name) = match rest.strip_prefix("shift+") {
                        Some(rest) if !rest.is_empty() => (true, rest),
                        _ => (false, rest),
                    };
                    let text: slint::SharedString = match name {
                        "escape" => char::from(Key::Escape).to_string().into(),
                        "return" => char::from(Key::Return).to_string().into(),
                        "tab" => char::from(Key::Tab).to_string().into(),
                        "left" => char::from(Key::LeftArrow).to_string().into(),
                        "right" => char::from(Key::RightArrow).to_string().into(),
                        "up" => char::from(Key::UpArrow).to_string().into(),
                        "down" => char::from(Key::DownArrow).to_string().into(),
                        s => s.into(),
                    };
                    let ctrl_text: slint::SharedString =
                        char::from(Key::Control).to_string().into();
                    let shift_text: slint::SharedString = char::from(Key::Shift).to_string().into();
                    if ctrl {
                        win.window().dispatch_event(WindowEvent::KeyPressed {
                            text: ctrl_text.clone(),
                        });
                    }
                    if shift {
                        win.window().dispatch_event(WindowEvent::KeyPressed {
                            text: shift_text.clone(),
                        });
                    }
                    win.window()
                        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                    win.window()
                        .dispatch_event(WindowEvent::KeyReleased { text });
                    if shift {
                        win.window()
                            .dispatch_event(WindowEvent::KeyReleased { text: shift_text });
                    }
                    if ctrl {
                        win.window()
                            .dispatch_event(WindowEvent::KeyReleased { text: ctrl_text });
                    }
                    return;
                }
                if let Some(at) = key.strip_prefix("click.") {
                    // click.X,Y — a REAL pointer move + press + release at
                    // window-logical coordinates, hit-tested by Slint like
                    // a physical click. This is what makes the menu bar
                    // drivable headlessly (menus render in-window on this
                    // backend), including the menu's own focus save/restore
                    // — the exact machinery the about/shortcuts toggles
                    // above cannot exercise (issue #13's fidelity note).
                    // Spelled with a dot so the step's `MS:ACTION` split
                    // stays visually unambiguous in scripts.
                    if let Some((x, y)) = at.split_once(',') {
                        if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
                            use slint::platform::{PointerEventButton, WindowEvent};
                            let pos = slint::LogicalPosition::new(x, y);
                            win.window()
                                .dispatch_event(WindowEvent::PointerMoved { position: pos });
                            win.window().dispatch_event(WindowEvent::PointerPressed {
                                position: pos,
                                button: PointerEventButton::Left,
                            });
                            win.window().dispatch_event(WindowEvent::PointerReleased {
                                position: pos,
                                button: PointerEventButton::Left,
                            });
                        }
                    }
                    return;
                }
                if let Some((coords, kind)) = key
                    .strip_prefix("press.")
                    .map(|a| (a, 0u8))
                    .or_else(|| key.strip_prefix("move.").map(|a| (a, 1u8)))
                    .or_else(|| key.strip_prefix("release.").map(|a| (a, 2u8)))
                {
                    // press.X,Y / move.X,Y / release.X,Y — the three phases
                    // of `click.` split into separately SCHEDULABLE steps
                    // (issue #46, promoted from the reproduction's QE
                    // instrumentation like `key:`/`click.` before it, PR
                    // #43). Spread over timed steps they carry real
                    // inter-event timing, which is what a drag needs to BE
                    // a drag: `click.`'s move+press+release in one tick has
                    // zero displacement and zero velocity, so no drag
                    // gesture — and no drag-derived defect class — was
                    // drivable headlessly. A `press.` dispatches a move
                    // first so hover state is coherent, exactly like
                    // `click.`; a lone `move.` while pressed extends the
                    // drag. Same dot spelling, same reason.
                    if let Some((x, y)) = coords.split_once(',') {
                        if let (Ok(x), Ok(y)) = (x.parse::<f32>(), y.parse::<f32>()) {
                            use slint::platform::{PointerEventButton, WindowEvent};
                            let pos = slint::LogicalPosition::new(x, y);
                            match kind {
                                0 => {
                                    win.window().dispatch_event(WindowEvent::PointerMoved {
                                        position: pos,
                                    });
                                    win.window().dispatch_event(WindowEvent::PointerPressed {
                                        position: pos,
                                        button: PointerEventButton::Left,
                                    });
                                }
                                1 => {
                                    win.window().dispatch_event(WindowEvent::PointerMoved {
                                        position: pos,
                                    });
                                }
                                _ => {
                                    win.window().dispatch_event(WindowEvent::PointerReleased {
                                        position: pos,
                                        button: PointerEventButton::Left,
                                    });
                                }
                            }
                            trace_mark(&format!(
                                "drive ptr {} {x:.0},{y:.0}",
                                ["press", "move", "release"][kind as usize]
                            ));
                        }
                    }
                    return;
                }
                if let Some(at) = key.strip_prefix("wheel.") {
                    // wheel.X,Y,DY — a REAL scroll event at window-logical
                    // coordinates, DY in logical px (60 = one notch-
                    // equivalent per the pointer contract's accumulator;
                    // positive = wheel up). Promoted for the same reason as
                    // press./move./release. (issue #46, QE gap): the
                    // overlay's scroll wiring — surfaces, accumulators, the
                    // post-Flickable coordinate terms — was reachable by no
                    // test and no Wayland automation, i.e. review-verified
                    // only. A move precedes the scroll so hover targeting
                    // is coherent, like click./press. .
                    if let Some((x, rest)) = at.split_once(',') {
                        if let Some((y, dy)) = rest.split_once(',') {
                            if let (Ok(x), Ok(y), Ok(dy)) =
                                (x.parse::<f32>(), y.parse::<f32>(), dy.parse::<f32>())
                            {
                                use slint::platform::WindowEvent;
                                let pos = slint::LogicalPosition::new(x, y);
                                win.window()
                                    .dispatch_event(WindowEvent::PointerMoved { position: pos });
                                win.window().dispatch_event(WindowEvent::PointerScrolled {
                                    position: pos,
                                    delta_x: 0.0,
                                    delta_y: dy,
                                });
                                trace_mark(&format!("drive ptr wheel {x:.0},{y:.0} dy {dy:.0}"));
                            }
                        }
                    }
                    return;
                }
                if let Some(label) = key.strip_prefix("dump.") {
                    // dump.<label> — trace the focus/surface state for test
                    // assertions. `keysfocus` observes the main key scope's
                    // real has-focus (the dbg-keys-focus debug property):
                    // focus was otherwise INVISIBLE to every headless run,
                    // which is how a stranded keyboard shipped (issue #41).
                    // The loupe pan block (soft/vx/vy/pan/zf, issue #46)
                    // makes the overlay's position observable at a scripted
                    // instant: mid-transit geometry and the carried pan
                    // centre could otherwise only be inferred from renders
                    // that trace on CHANGE, and a wrong-position frame is
                    // exactly a state nothing re-renders.
                    let st = state.borrow();
                    trace_mark(&format!(
                        "QEDUMP {label} keysfocus={} one2one={} zoom={} iptc={} about={} \
                         shortcuts={} copy={} summary={:?} template={:?} revert={:?} status={:?} \
                         soft={} vx={:.1} vy={:.1} pan={:.4},{:.4} zf={:.3} \
                         copynote={:?} report={:?} copystate={} confirm={:?} \
                         clip={} clipstate={} clipavail={} clipsummary={:?} clipskipped={:?} \
                         cliperror={:?} clipreport={:?} clipconfirm={:?} clipprogress={:?} \
                         cursor={} selected={}",
                        win.get_dbg_keys_focus(),
                        win.get_one2one(),
                        st.grid.zoom,
                        win.get_iptc_visible(),
                        win.get_about_visible(),
                        win.get_shortcuts_visible(),
                        win.get_copy_visible(),
                        win.get_copy_summary().as_str(),
                        win.get_copy_template().as_str(),
                        win.get_iptc_revert_label().as_str(),
                        win.get_status().as_str(),
                        win.get_loupe_soft(),
                        win.get_loupe_vx(),
                        win.get_loupe_vy(),
                        st.loupe_view.pan_center.0,
                        st.loupe_view.pan_center.1,
                        st.loupe_view.zoom_factor,
                        win.get_copy_collisions().as_str(),
                        win.get_copy_report().as_str(),
                        // The clash question is a STATE of the copy
                        // dialog (fileops.md), so `copy=true` alone cannot
                        // tell a plan preview from the question: without
                        // these two fields a driven run can see that the
                        // dialog is up but not what it is asking, and the
                        // one irreversible operation in the app would be
                        // assertable only down to "a dialog exists".
                        win.get_copy_state(),
                        win.get_copy_confirm().as_str(),
                        // The video export (M9): the same reasoning as
                        // the copy block above — this is the second
                        // operation in the app that writes files the user
                        // cannot undo, and a driven run must be able to
                        // see WHICH state its dialog is in, what the plan
                        // line promised, and what the report said.
                        win.get_clip_visible(),
                        win.get_clip_state(),
                        win.get_clip_available(),
                        win.get_clip_summary().as_str(),
                        win.get_clip_skipped().as_str(),
                        win.get_clip_error().as_str(),
                        win.get_clip_report().as_str(),
                        win.get_clip_confirm().as_str(),
                        // "Writing 3 / 30 — DSC05012.ARW" and "Verifying
                        // 137 / 400": the only part of a running export a
                        // user sees, and no driven test could see it at
                        // all until this line (QE finding 2026-08-28).
                        win.get_clip_progress().as_str(),
                        // The cursor (an image id) and the selection count
                        // the status bar shows: the burst keys (issue
                        // #55) are a cursor move plus a selection change,
                        // and neither was observable to a driven run
                        // except by parsing the status text.
                        st.grid.cursor,
                        st.grid.selection.count_in_view(&st.grid.view),
                    ));
                    return;
                }
                if let Some(dims) = key.strip_prefix("resize:") {
                    // resize:WxH (logical px) — the user's reported bug
                    // class (issue #16) needs REAL window resizes to be
                    // drivable, or it ships regression-blind.
                    if let Some((w, h)) = dims.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>()) {
                            win.window().set_size(slint::WindowSize::Logical(
                                slint::LogicalSize::new(w, h),
                            ));
                        }
                    }
                    return;
                }
                // Modal keyboard containment, mirrored for driven keys
                // (issue #23): a driven nav action must die exactly like
                // a real keypress while a popup is up — the FocusScope
                // guard only sees real keyboard events, and without this
                // mirror the containment tests would test nothing.
                if win.get_about_visible() || win.get_shortcuts_visible() {
                    trace_mark(&format!("drive swallowed by modal: {key}"));
                    return;
                }
                handle_nav(&win, &state, &key);
            });
        }
    }
    drives_pending
}
