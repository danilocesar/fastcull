// GUI subsystem on Windows (issue #40, specs/01-architecture.md): without
// this, a double-clicked fastcull-app.exe drags a console window along, and
// closing that console kills the app (CTRL_CLOSE_EVENT). No-op on Linux.
// fastcull-cli deliberately stays a console-subsystem terminal tool.
#![windows_subsystem = "windows"]

//! FastCull desktop application: thin Slint bridge over `fastcull-core`
//! (specs/modules/ui-grid.md). All layout math lives in `fastcull_core::grid`;
//! this crate only moves data between the engine and the declarative UI.
//!
//! Usage: `fastcull-app [<folder>]` or `fastcull-app --synthetic 2000` —
//! no arguments opens the empty window (desktop-launcher start, issue #5)
//! (colored placeholder cells, no RAW files needed — the M2 60 fps spike).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

mod copy_bridge;
mod harness;
mod kitchen;
mod pump;
mod shutter;
mod state;
mod trace;

use crate::state::{
    clamp_wash_opacity, AppState, MARGIN_ROWS, MIDS_CAP, OVERLAY_HOLD_CAP, SELECTION_WASH_OPACITY,
    SELECTION_WASH_RGB,
};
use crate::trace::{trace_mark, trace_slow, trace_start};
use fastcull_core::catalog::Session;
use fastcull_core::grid::{self, GridLayout, Nav};
use fastcull_core::iptc::IptcField;
use fastcull_core::loupe::is_top_rung;
use fastcull_core::pipeline::{JobSpec, Pipeline};
use slint::{ComponentHandle, Model, VecModel};

slint::include_modules!();

fn recompute_view(st: &mut AppState) {
    let complete = st.metadata_complete();
    st.view =
        fastcull_core::filter::view(&st.picks, &st.labels, &st.capture_keys, &st.query, complete);
    // Every membership/order change bumps the generation: a cursor
    // displaced by a view RE-SORT (capture keys streaming in during
    // load) is not scrolling, and the follow-scroll claim must not
    // fire on it (issue #22 — the cursor moved during folder load with
    // no input, and the load-race flaked CI).
    st.view_generation = st.view_generation.wrapping_add(1);
    // Re-key the loupe prefetch ring in the same tick (issue #46): the
    // ring walks VIEW order — what arrows actually reach — and a stale
    // ring after a filter/sort change would warm ghosts. Every view
    // change funnels through here (load_folder recomputes after the
    // engine starts, so a fresh session is keyed too).
    if let Some(loupe) = &st.loupe {
        loupe.set_view(&st.view);
    }
}

/// Recompute the view AND re-apply the cursor rules. Every membership
/// change — a filter switch, but also pump-driven ones (sidecar picks
/// landing under an active filter, progressive capture keys) — must leave
/// the cursor on a view member (nearest survivor), and an emptied view has
/// no loupe to be in (persona G2). Validator finding: the pump previously
/// recomputed membership alone, leaving a cursor no cell owned.
///
/// `user_changed_query` distinguishes the USER asking for a different view
/// (a filter chip, the sort control) from the ENGINE talking (streaming
/// metadata, the load-settled re-sort, a decode landing, a sidecar
/// arriving). Pre-touch the first still snaps to the new view's head per
/// issue #4; the second must stop moving the photograph once the folder has
/// loaded (user decision 2026-07-31). The semantics live in
/// `filter::cursor_after_recompute` — see it for why this is a state and not
/// an edge.
fn recompute_view_keep_cursor(st: &mut AppState, user_changed_query: bool) {
    let old_view = std::mem::take(&mut st.view);
    let old_cursor = old_view.contains(&st.cursor).then_some(st.cursor);
    recompute_view(st);
    if let Some(id) = fastcull_core::filter::cursor_after_recompute(
        &old_view,
        old_cursor,
        &st.view,
        st.cursor_touched,
        st.metadata_complete(),
        user_changed_query,
    ) {
        st.cursor = id;
    }
    if st.view.is_empty() {
        st.exit_loupe(); // nothing to look at: the empty state is a grid
    }
}

/// Swap the session to `folder` (startup CLI path and File > Open Folder
/// share this — spec: identical behavior). Drops the old engines first:
/// pipeline/loupe workers stop, the old sidecar writer flushes on drop.
fn load_folder(state: &Rc<RefCell<AppState>>, folder: &std::path::Path) -> Result<(), String> {
    let session = Session::open(folder).map_err(|e| e.to_string())?;
    let labels: Vec<String> = session
        .images
        .iter()
        .map(|i| i.file_name().into_owned())
        .collect();
    let jobs: Vec<JobSpec> = session
        .images
        .iter()
        .map(|i| JobSpec {
            path: i.path.clone(),
            size: i.size,
            mtime: i.mtime,
        })
        .collect();
    let mut st = state.borrow_mut();
    st.pipeline = None;
    st.loupe = None;
    st.pipeline_rx = None;
    st.loupe_rx = None;
    drop(st.writer.take()); // flush barrier for the previous session's marks
    st.sidecar_errs = None;

    let count = labels.len();
    let paths: Vec<std::path::PathBuf> = jobs.iter().map(|j| j.path.clone()).collect();
    st.labels = labels;
    st.paths = paths.clone();
    st.picks = vec![fastcull_core::catalog::PickState::Unmarked; count];
    st.touched.clear();
    st.sidecar_failures = 0;
    st.cursor = 0;
    st.cursor_touched = false;
    st.thumb_jpegs.clear();
    st.images.clear();
    st.failed.clear();
    st.thumbs_done = 0;
    // New session: drop queued kitchen work and orphan late completions
    // (their generation dies with the old session).
    st.kitchen.retarget();
    // Re-arm issue #25's one-shot re-sort edge for the new session. It
    // happens to re-arm anyway because every open path refreshes
    // synchronously before the pump can deliver an event, but that is an
    // implicit invariant one reordered call away from a second folder
    // silently losing its re-anchor (validator concern, 2026-07-31).
    st.last_metadata_complete = false;
    st.last_cursor_visible = true;
    st.synthetic = false;
    st.fullres.clear();
    st.terminal_native.clear();
    st.last_resolved_factor = None; // magnification never carries across sessions
    st.last_badge = None; // indexes mean a different image now
    st.overlay_hold = None; // a hold must not straddle a session swap
    st.last_soft_rung = None;
    st.zoom_factor = 1.0;
    st.pan_center = (0.5, 0.5);
    st.mids.clear();
    st.va = fastcull_core::viewassets::ViewAssets::default();
    st.capture_keys = vec![None; count];
    st.frame_meta = vec![fastcull_core::burst::FrameMeta::default(); count];
    st.burst_of = vec![None; count];
    st.burst_badge = vec![0; count];
    st.burst_pos = vec![None; count];
    st.burst_dirty = false;
    st.iptc = vec![fastcull_core::iptc::IptcData::default(); count];
    st.touched_iptc.clear();
    st.panel_cache = Default::default();
    st.copy_plan = None;
    st.copy_handle = None;
    st.copy_rx = None;
    st.copied_to.clear();
    st.selection.reset();
    st.revert = Default::default();
    st.revert_ids.clear();
    st.revert_label.clear();
    // templates.toml: read at session open (spec: live-reload = re-read
    // here and on panel toggle, no watcher). Errors/warnings surface in
    // the panel warning strip.
    reload_templates(&mut st);
    // A new folder starts unfiltered: a hidden active filter on a fresh
    // session would look like missing files.
    st.query = fastcull_core::filter::ViewQuery::default();

    let (writer, errs) = fastcull_core::sidecar_writer::SidecarWriter::start();
    st.writer = Some(writer);
    st.sidecar_errs = Some(errs);
    // FASTCULL_NO_CACHE: hermetic test runs must not touch the user's
    // real per-user cache DB (validator/QE finding).
    let cache_path = if std::env::var_os("FASTCULL_NO_CACHE").is_some() {
        None
    } else {
        fastcull_core::cache::default_cache_path()
    };
    let (pipeline, rx) = Pipeline::start(
        jobs,
        cache_path,
        std::thread::available_parallelism().map_or(4, |n| n.get()),
    );
    let (loupe, loupe_rx) =
        fastcull_core::loupe::LoupeEngine::start(paths, fastcull_core::loupe::DEFAULT_BUDGET_BYTES);
    st.pipeline = Some(pipeline);
    st.loupe = Some(loupe);
    st.pipeline_rx = Some(rx);
    st.loupe_rx = Some(loupe_rx);
    st.session_open = true;
    recompute_view(&mut st);
    Ok(())
}

/// THE focus-continuity rule (issue #41): whenever the focused editor is
/// destroyed or covered — IPTC panel close, session swap, a modal opening
/// over a focused field — keyboard focus must deterministically return to
/// the topmost surface's key scope, or the keyboard dies (no element has
/// focus and nothing reclaims it; at 1:1 there is no discoverable
/// recovery) or, worse, keystrokes land invisibly in a field hidden
/// behind a modal's scrim and get committed as metadata.
///
/// Deferred via a zero-length timer ON PURPOSE: Slint's MenuBar restores
/// focus to the previously-focused element AFTER the item activation
/// callback runs, inside the same event dispatch — QE proved a
/// synchronous `focus-keys()` inside an activation is overridden by that
/// restore. A timer scheduled during the dispatch cannot fire until the
/// dispatch (activation + menu close + focus restore) has fully
/// unwound, so the queued claim always lands last. The Slint side adds a
/// synchronous belt-and-braces bounce on the editors themselves (a focus
/// gain that arrives behind a modal is handed straight to the modal's
/// scope) so the dangerous surfaces never hold the keyboard even
/// mid-dispatch.
fn refocus_topmost_deferred(win: &MainWindow) {
    let win = win.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(win) = win.upgrade() {
            win.invoke_focus_keys();
        }
    });
}

/// The Open Folder ACTION — everything the menu entry does after the native
/// dialog has produced a path: session swap via [`load_folder`], fresh grid
/// zoom, viewport at the top, error surfaced in the status bar. Shared by
/// the menu callback and the `open:PATH` drive token (issue #34) so the
/// scripted swap exercises the exact code path a real Open Folder takes —
/// a parallel test-only path would bypass the very wiring under test.
fn open_folder_at(win: &MainWindow, state: &Rc<RefCell<AppState>>, folder: &std::path::Path) {
    match load_folder(state, folder) {
        Ok(()) => {
            // Menu-open behaves like the CLI argument (spec): fresh
            // grid zoom, cursor at the first image.
            let mut st = state.borrow_mut();
            st.zoom = 1;
            st.last_grid_zoom = 1;
            drop(st);
            win.set_vp_y(0.0);
            // Invalidate every in-flight edit BEFORE any focus movement
            // (issue #41 D3): editors stamp this generation on focus
            // gain, and a blur commit from a stale stamp discards — the
            // structural guarantee that the old session's half-typed
            // text can never be committed against the new session's
            // images (user decision: swap mid-edit discards).
            win.set_session_gen(win.get_session_gen().wrapping_add(1));
            refresh(win, state);
            // The swap rebuilt the panel's field rows and dropped any
            // editor focus; without this claim the first keystroke on
            // the fresh session is dead.
            refocus_topmost_deferred(win);
        }
        Err(e) => {
            eprintln!("fastcull: {e}");
            win.set_status(format!("Open folder failed: {e}").into());
        }
    }
}

/// Reconnect stderr/stdout to the parent's console (issue #40). A GUI-
/// subsystem process launched from cmd/PowerShell WITHOUT redirection starts
/// with NULL std handles, so every `eprintln!` — the FASTCULL_TRACE marks the
/// FAQ tells bug reporters to capture, usage errors, the drive harness —
/// would silently vanish. Attaching to the parent's console makes Windows
/// replace NULL std handles with console handles (GetStdHandle docs,
/// "Attach/detach behavior"), and Rust's std re-queries the handle on every
/// write rather than caching it, so no further rebinding is needed.
/// Redirected handles (`2> trace.txt`, test pipes) arrive via
/// STARTF_USESTDHANDLES and are never replaced. Failure (no parent console —
/// the Explorer double-click) is the normal GUI launch: ignore it.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: no pointers, no preconditions; the call either attaches the
    // process to its parent's console or fails harmlessly.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn main() {
    // Must run before ANY output so the first trace/usage line already has a
    // console to land on.
    #[cfg(windows)]
    attach_parent_console();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // --screenshot <out.png>: render, snapshot after a settle delay, save,
    // exit 0. The screenshot smoke-test hook (ui-grid.md acceptance).
    let screenshot: Option<std::path::PathBuf> = match args.iter().position(|a| a == "--screenshot")
    {
        Some(i) if i + 1 < args.len() => {
            let path = args.remove(i + 1);
            args.remove(i);
            Some(path.into())
        }
        Some(_) => {
            eprintln!("usage: --screenshot <out.jpg>");
            std::process::exit(2);
        }
        None => None,
    };
    if screenshot.is_some() {
        // take_snapshot() yields black frames on the GPU renderer; the
        // software renderer supports it and is fine for smoke tests.
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }
    // --start-loupe / --start-11: open directly at loupe zoom (fit or 1:1) —
    // used by the screenshot smoke tests to capture those states.
    let start_11 = args
        .iter()
        .position(|a| a == "--start-11")
        .map(|i| args.remove(i))
        .is_some();
    let start_loupe = args
        .iter()
        .position(|a| a == "--start-loupe")
        .map(|i| args.remove(i))
        .is_some();
    enum Launch {
        /// No arguments (desktop launcher / double-clicked binary, issue
        /// #5): open the normal window in the empty state — NEVER a usage
        /// error printed to a terminal nobody sees.
        Empty,
        Synthetic(usize),
        Folder(std::path::PathBuf),
    }
    let launch = match args.as_slice() {
        [] => Launch::Empty,
        [flag, n] if flag == "--synthetic" => {
            let Ok(n) = n.parse::<usize>() else {
                eprintln!("usage: fastcull-app [<folder> | --synthetic <count>]");
                std::process::exit(2);
            };
            Launch::Synthetic(n)
        }
        [folder] => Launch::Folder(folder.into()),
        _ => {
            eprintln!("usage: fastcull-app [<folder> | --synthetic <count>]");
            std::process::exit(2);
        }
    };

    let window = MainWindow::new().expect("creating window");
    // About-dialog version (issue #23): X.Y.Z on a release-tag build,
    // X.Y.Z-devel-YYYYMMDD-<hash> otherwise (suffix composed by build.rs — a bug
    // report from a dev build must pin the commit). Traced so headless
    // runs can assert the composition without pixel-reading the dialog.
    let about_version = format!(
        "{}{}",
        fastcull_core::VERSION,
        env!("FASTCULL_VERSION_SUFFIX")
    );
    trace_mark(&format!("about version {about_version}"));
    window.set_about_version(about_version.into());
    // Selection wash defaults. The UI only ever READS these two properties,
    // so promoting the strength to a user setting later is a write here.
    window.set_selection_wash(slint::Color::from_rgb_u8(
        SELECTION_WASH_RGB[0],
        SELECTION_WASH_RGB[1],
        SELECTION_WASH_RGB[2],
    ));
    window.set_selection_wash_opacity(clamp_wash_opacity(SELECTION_WASH_OPACITY));
    let cells = Rc::new(VecModel::from(Vec::<CellData>::new()));
    window.set_cells(slint::ModelRc::from(Rc::clone(&cells)));
    let start_at_loupe = start_11 || start_loupe;
    let state = Rc::new(RefCell::new(AppState {
        labels: Vec::new(),
        paths: Vec::new(),
        picks: Vec::new(),
        touched: HashSet::new(),
        writer: None,
        sidecar_failures: 0,
        zoom: if start_at_loupe {
            grid::ZOOM_COLUMNS.len() - 1
        } else {
            1 // 8 columns
        },
        cursor: 0,
        thumb_jpegs: HashMap::new(),
        images: HashMap::new(),
        failed: HashSet::new(),
        pipeline: None,
        thumbs_done: 0,
        synthetic: false,
        session_open: false,
        cells,
        loupe: None,
        terminal_native: HashSet::new(),
        view_generation: 0,
        last_view_generation: 0,
        last_cursor_visible: true,
        last_metadata_complete: false,
        last_resolved_factor: None,
        last_badge: None,
        last_view_geometry: None,
        fullres: Vec::new(),
        zoom_factor: if start_11 { f32::INFINITY } else { 1.0 },
        pan_center: (0.5, 0.5),
        last_pan_write: None,
        overlay_hold: None,
        last_soft_rung: None,
        kitchen: {
            // Completion nudge: the worker pokes the event loop so a
            // finished texture is adopted as soon as the UI is idle —
            // the 33 ms pump is the fallback, not the design point
            // (persona condition: the one-tick cost must not be a
            // trickle-in).
            let win = window.as_weak();
            kitchen::Kitchen::start(Box::new(move || {
                let win = win.clone();
                slint::invoke_from_event_loop(move || {
                    if let Some(win) = win.upgrade() {
                        win.invoke_kitchen_ready();
                    }
                })
                .ok();
            }))
        },
        last_overlay_cursor: None,
        cursor_touched: false,
        last_grid_zoom: 1,
        mids: HashMap::new(),
        va: fastcull_core::viewassets::ViewAssets::default(),
        query: fastcull_core::filter::ViewQuery::default(),
        view: Vec::new(),
        capture_keys: Vec::new(),
        frame_meta: Vec::new(),
        burst_of: Vec::new(),
        burst_badge: Vec::new(),
        burst_pos: Vec::new(),
        burst_dirty: false,
        iptc: Vec::new(),
        touched_iptc: HashSet::new(),
        panel_cache: Default::default(),
        selection: Default::default(),
        revert: Default::default(),
        revert_ids: Vec::new(),
        revert_label: String::new(),
        templates: Vec::new(),
        template_warnings: Vec::new(),
        iptc_visible: false,
        filter_bar_visible: true,
        copy_plan: None,
        copy_dest: None,
        copy_handle: None,
        copy_rx: None,
        copied_to: HashMap::new(),
        pipeline_rx: None,
        loupe_rx: None,
        sidecar_errs: None,
    }));

    match launch {
        Launch::Empty => {
            // Folderless start: the window opens with the "No folder
            // open" empty state; the session begins when the user picks
            // a folder (File > Open Folder — the existing session-swap
            // path handles it).
            recompute_view(&mut state.borrow_mut());
        }
        Launch::Synthetic(n) => {
            let mut st = state.borrow_mut();
            st.labels = (0..n).map(|i| format!("SYN{i:05}.ARW")).collect();
            st.picks = vec![fastcull_core::catalog::PickState::Unmarked; n];
            st.capture_keys = vec![None; n];
            st.frame_meta = vec![fastcull_core::burst::FrameMeta::default(); n];
            st.burst_of = vec![None; n];
            st.burst_badge = vec![0; n];
            st.burst_pos = vec![None; n];
            st.iptc = vec![fastcull_core::iptc::IptcData::default(); n];
            st.synthetic = true;
            st.session_open = true;
            // No pipeline runs in synthetic mode, so no job ever completes:
            // without this the session is permanently "still loading" and
            // the status bar would claim "0/N loaded - sorting by name"
            // forever, on the very frames the screenshot suite captures.
            st.thumbs_done = n;
            recompute_view(&mut st);
        }
        Launch::Folder(path) => {
            if let Err(e) = load_folder(&state, &path) {
                eprintln!("fastcull: {e}");
                std::process::exit(1);
            }
            // load_folder resets the zoom; --start-11 wants 1:1 back on.
            if start_11 {
                state.borrow_mut().zoom_factor = f32::INFINITY;
            }
        }
    }

    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_viewport_changed(move || {
            if let Some(win) = win.upgrade() {
                refresh(&win, &state);
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_nav(move |key| {
            let Some(win) = win.upgrade() else { return };
            handle_nav(&win, &state, key.as_str());
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_set_filter(move |name| {
            let Some(win) = win.upgrade() else { return };
            let filter = match name.as_str() {
                "picked" => fastcull_core::filter::PickFilter::Picked,
                "rejected" => fastcull_core::filter::PickFilter::Rejected,
                "unmarked" => fastcull_core::filter::PickFilter::Unmarked,
                _ => fastcull_core::filter::PickFilter::All,
            };
            apply_filter_change(&win, &state, filter);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_cycle_sort(move || {
            let Some(win) = win.upgrade() else { return };
            {
                use fastcull_core::filter::SortKey;
                let mut st = state.borrow_mut();
                // Cycle: Capture ↑ → Capture ↓ → Name ↑ → Name ↓ → …
                let q = &mut st.query;
                (q.sort, q.ascending) = match (q.sort, q.ascending) {
                    (SortKey::CaptureTime, true) => (SortKey::CaptureTime, false),
                    (SortKey::CaptureTime, false) => (SortKey::Filename, true),
                    (SortKey::Filename, true) => (SortKey::Filename, false),
                    (SortKey::Filename, false) => (SortKey::CaptureTime, true),
                };
                // Through the cursor-aware recompute (validator: plain
                // recompute here skipped the pre-touch snap, making the
                // cursor after a sort click timing-dependent again).
                recompute_view_keep_cursor(&mut st, true); // the USER re-sorted
            }
            reveal_cursor(&win, &state);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_toggle_filter_bar(move || {
            let Some(win) = win.upgrade() else { return };
            let hide_resets = {
                let mut st = state.borrow_mut();
                st.filter_bar_visible = !st.filter_bar_visible;
                win.set_filter_bar_visible(st.filter_bar_visible);
                // Persona G6: a filter must never be active while invisible.
                !st.filter_bar_visible && st.query.filter != fastcull_core::filter::PickFilter::All
            };
            if hide_resets {
                apply_filter_change(&win, &state, fastcull_core::filter::PickFilter::All);
            } else {
                reveal_cursor(&win, &state);
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_open_folder(move || {
            let Some(win) = win.upgrade() else { return };
            let Some(folder) = rfd::FileDialog::new().pick_folder() else {
                return; // user cancelled
            };
            open_folder_at(&win, &state, &folder);
        });
    }
    window.on_quit(|| {
        slint::quit_event_loop().ok();
    });
    {
        // Help > About / Keyboard Shortcuts: steal the keyboard from any
        // focused editor now COVERED by the modal (issue #41 D2). The
        // immediate claim handles the non-menu callers; the deferred one
        // survives the MenuBar's post-activation focus restore, which
        // otherwise hands the keys back to the field hidden behind the
        // scrim — an un-dismissable modal, with every keystroke landing
        // invisibly in the field and committable as metadata.
        let win = window.as_weak();
        window.on_modal_opened(move || {
            let Some(win) = win.upgrade() else { return };
            win.invoke_focus_keys();
            refocus_topmost_deferred(&win);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_iptc_toggle(move || {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                st.iptc_visible = !st.iptc_visible;
                if st.iptc_visible {
                    reload_templates(&mut st); // read-on-open live-reload
                }
                // Publish the new dock state BEFORE any geometry read:
                // grid-width is a binding on it, and revealing against
                // the STALE width mis-anchored the viewport and let the
                // follow-scroll claim swap the photo (issue #16).
                win.set_iptc_visible(st.iptc_visible);
            }
            // The dock reflows the grid: anchor on the cursor so the
            // viewport doesn't land somewhere new (persona gap 1).
            reveal_cursor(&win, &state);
            // Closing the panel DESTROYS its editors; if one was focused,
            // focus lands on no element and the keyboard dies (issue #41
            // D1 — the user's live hit, via View > IPTC Panel; the menu's
            // own restore targets the destroyed editor and strands the
            // keys). The mid-edit text is discarded with the editor (user
            // decision). Close only: on OPEN the K path may just have
            // landed focus in the keyword field, which must keep it.
            if !win.get_iptc_visible() {
                refocus_topmost_deferred(&win);
            }
        });
    }
    {
        // Manual field commit: same tri-state as templates, but in the
        // PANEL bare emptiness PRESERVES (persona IN-MY-WAY rule) — an
        // empty commit is a no-op; clearing is the explicit control.
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_iptc_field_committed(move |i, text, return_focus| {
            let Some(win) = win.upgrade() else { return };
            // Sanitize at the commit boundary (NFC + control-strip + trim:
            // raw controls make the XMP packet invalid, QE-proven).
            let text = fastcull_core::iptc::sanitize_text(text.as_str());
            if !text.is_empty() {
                let mut st = state.borrow_mut();
                let batch = st.selection.batch(&st.view, st.cursor);
                // No-op guard (gate finding): a value-unchanged commit —
                // Enter as "back to the grid", or the G7 click-away
                // double-fire — must not clobber the shared revert slot
                // or rewrite sidecars.
                let unchanged = batch.iter().all(|id| {
                    st.iptc
                        .get(*id)
                        .is_some_and(|d| iptc_field_get(d, i as usize) == Some(&text))
                });
                if !batch.is_empty() && !unchanged {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.iptc.get_mut(*id) {
                            iptc_field_set(d, i as usize, Some(text.clone()));
                        }
                    }
                    let label = format!(
                        "{} on {} image(s)",
                        iptc_field_label(i as usize),
                        batch.len()
                    );
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            if return_focus {
                win.invoke_focus_grid(); // G4: cursor stays, grid gets keys
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_iptc_field_clear(move |i| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let batch = st.selection.batch(&st.view, st.cursor);
                let all_unset = batch.iter().all(|id| {
                    st.iptc
                        .get(*id)
                        .is_some_and(|d| iptc_field_get(d, i as usize).is_none())
                });
                if !batch.is_empty() && !all_unset {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.iptc.get_mut(*id) {
                            iptc_field_set(d, i as usize, None);
                        }
                    }
                    let label = format!(
                        "clear {} on {} image(s)",
                        iptc_field_label(i as usize),
                        batch.len()
                    );
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_iptc_keyword_added(move |text| {
            let Some(win) = win.upgrade() else { return };
            let kws: Vec<String> = text
                .split(',')
                .map(|k| k.trim().to_string())
                .filter(|k| !k.is_empty())
                .collect();
            if !kws.is_empty() {
                let mut st = state.borrow_mut();
                let batch = st.selection.batch(&st.view, st.cursor);
                // No-op guard (gate N2): re-entering an already-present
                // keyword — easy via the G7 click-away — must not clobber
                // the shared revert slot or rewrite sidecars. Dry-run on
                // clones; commit only when something actually changes.
                let changed = batch.iter().any(|id| {
                    st.iptc.get(*id).is_some_and(|d| {
                        let mut probe = d.clone();
                        probe.add_keywords(kws.iter().cloned());
                        probe.keywords != d.keywords
                    })
                });
                if !batch.is_empty() && changed {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.iptc.get_mut(*id) {
                            d.add_keywords(kws.iter().cloned());
                        }
                    }
                    let label = format!("keywords on {} image(s)", batch.len());
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            refresh(&win, &state);
        });
    }
    {
        // Chip X: removes the keyword from EVERY batch image — revert-
        // covered (persona: never un-revertible batch destruction).
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_iptc_keyword_removed(move |chip_index| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let batch = st.selection.batch(&st.view, st.cursor);
                // Rebuild the chip order exactly as the panel shows it
                // (first-seen across the batch in view order).
                let mut order: Vec<String> = Vec::new();
                for id in &batch {
                    if let Some(d) = st.iptc.get(*id) {
                        for kw in &d.keywords {
                            if !order.contains(kw) {
                                order.push(kw.clone());
                            }
                        }
                    }
                }
                if let Some(kw) = order.get(chip_index as usize).cloned() {
                    let snaps: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.iptc.get(*id).cloned())
                        .collect();
                    for id in &batch {
                        if let Some(d) = st.iptc.get_mut(*id) {
                            d.keywords.retain(|k| *k != kw);
                        }
                    }
                    let label = format!("remove '{kw}' from {} image(s)", batch.len());
                    commit_batch_mutation(&mut st, &batch, snaps, &label);
                }
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_iptc_apply_template(move |tpl_index| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let batch = st.selection.batch(&st.view, st.cursor);
                let Some(tpl) = st.templates.get(tpl_index as usize).cloned() else {
                    return;
                };
                if !batch.is_empty() {
                    // Contexts in batch (= view) order: the {seq} contract.
                    let ctxs: Vec<_> = batch
                        .iter()
                        .map(|id| {
                            let name = st.labels.get(*id).cloned().unwrap_or_default();
                            let mtime = st
                                .paths
                                .get(*id)
                                .and_then(|p| std::fs::metadata(p).ok())
                                .and_then(|m| m.modified().ok())
                                .unwrap_or(std::time::UNIX_EPOCH);
                            // Camera model is not plumbed yet: {camera}
                            // expands empty (recorded in the spec ledger).
                            fastcull_core::iptc::ExpandContext::from_sort_key(
                                st.capture_keys.get(*id).and_then(|k| k.as_deref()),
                                mtime,
                                &name,
                                None,
                            )
                        })
                        .collect();
                    let mut images: Vec<_> = batch
                        .iter()
                        .filter_map(|id| st.iptc.get(*id).cloned())
                        .collect();
                    match fastcull_core::iptc::apply_template(&tpl, &mut images, &ctxs) {
                        Ok(snaps) => {
                            for (id, data) in batch.iter().zip(images) {
                                if let Some(slot) = st.iptc.get_mut(*id) {
                                    *slot = data;
                                }
                            }
                            let label = format!("apply '{}' to {} image(s)", tpl.name, batch.len());
                            commit_batch_mutation(&mut st, &batch, snaps, &label);
                        }
                        Err(e) => {
                            // All-or-nothing: nothing changed; surface it.
                            st.template_warnings = vec![e.to_string()];
                        }
                    }
                }
            }
            refresh(&win, &state);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_iptc_revert(move || {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let ids = std::mem::take(&mut st.revert_ids);
                let mut images: Vec<_> = ids
                    .iter()
                    .filter_map(|id| st.iptc.get(*id).cloned())
                    .collect();
                if st.revert.revert_into(&mut images) {
                    for (id, data) in ids.iter().zip(images) {
                        if let Some(slot) = st.iptc.get_mut(*id) {
                            *slot = data.clone();
                        }
                        if let (Some(path), Some(writer)) = (st.paths.get(*id), &st.writer) {
                            writer.iptc(path.clone(), data);
                        }
                    }
                }
                st.revert_label.clear();
            }
            refresh(&win, &state);
        });
    }
    copy_bridge::wire(&window, &state);
    {
        // Click in the zoom overlay: "center HERE", FACTOR UNCHANGED
        // (issue #11 transition table, superseding the earlier "below the
        // ceiling a click jumps to 1:1" default — double-click owns the
        // 1:1 jump now). Fractions arrive image-relative from Slint, so
        // this IS the machine's (Zoomed, Click) → Recenter row without a
        // lossy coords round-trip.
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_loupe_clicked(move |fx, fy| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                // Clicking to pixel-peep is as much a claim as any other
                // click (validator: a capture-key re-sort could otherwise
                // swap the image under an active 1:1 inspection).
                st.cursor_touched = true;
                st.pan_center = (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0));
            }
            refresh(&win, &state);
        });
    }
    {
        // Overlay drag-pan (issue #46): dx/dy from the overlay TouchArea,
        // folded through the pointer machine's Zoomed×Drag row — the
        // table cell that used to be implemented OUTSIDE the machine via
        // the Flickable's kinetic pan plus offset read-back. Rust
        // recenters `pan_center` and refresh() rewrites the offsets
        // synchronously: single writer, and the drag itself is the
        // positive signal the #16/#22 doctrine demands — no displacement
        // inference anywhere.
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_loupe_dragged(move |dx, dy| {
            let Some(win) = win.upgrade() else { return };
            let action = {
                let st = state.borrow();
                let (ms, geo) = machine_ctx(&win, &st);
                fastcull_core::pointer::step(
                    ms,
                    fastcull_core::pointer::PointerInput::Drag { dx, dy },
                    &geo,
                )
                .1
            };
            apply_pointer_action(&win, &state, action);
        });
    }
    pump::wire(&window, &state);
    {
        // Pointer wheel (issue #11): one notch-equivalent = one ladder
        // stop, anchored under the pointer. Arrives from the fit surface
        // or the zoom overlay; the machine decides from the actual state.
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_pointer_wheel(move |up, ctrl, x, y| {
            let Some(win) = win.upgrade() else { return };
            let action = {
                let st = state.borrow();
                let (ms, geo) = machine_ctx(&win, &st);
                fastcull_core::pointer::step(
                    ms,
                    fastcull_core::pointer::PointerInput::Wheel {
                        up,
                        ctrl,
                        pos: (x, y),
                    },
                    &geo,
                )
                .1
            };
            apply_pointer_action(&win, &state, action);
        });
    }
    {
        // A click at fit does nothing to the view (spec Q5) — it only
        // claims the cursor, like every other click on an image
        // (untouched-cursor rule: a capture-key re-sort must not swap the
        // image under the user). It used to also feed a double-click
        // proximity trace; that guard is gone — Slint's own 10 px repeat
        // gate enforces the rule, see `handle_loupe_double_click`.
        let state = Rc::clone(&state);
        window.on_fit_clicked(move || {
            state.borrow_mut().cursor_touched = true;
        });
    }
    {
        // Double-clicks in the loupe (fit or zoomed): 1:1 with the
        // clicked point centered — IF the two presses were close (spec:
        // farther apart = two independent clicks, already handled).
        {
            let state = Rc::clone(&state);
            let win = window.as_weak();
            window.on_fit_double_clicked(move |x, y| {
                let Some(win) = win.upgrade() else { return };
                handle_loupe_double_click(&win, &state, x, y);
            });
        }
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_zoom_double_clicked(move |x, y| {
            let Some(win) = win.upgrade() else { return };
            handle_loupe_double_click(&win, &state, x, y);
        });
    }
    {
        // Grid double-click: open that image in the loupe at fit (the
        // first click already moved and claimed the cursor).
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_cell_double_clicked(move |_id| {
            let Some(win) = win.upgrade() else { return };
            let action = {
                let st = state.borrow();
                let (ms, geo) = machine_ctx(&win, &st);
                fastcull_core::pointer::step(
                    ms,
                    fastcull_core::pointer::PointerInput::DoubleClick { pos: (0.0, 0.0) },
                    &geo,
                )
                .1
            };
            apply_pointer_action(&win, &state, action);
        });
    }
    {
        // Grid cell click: cursor + selection semantics (issue #7). The
        // old "at loupe fit a click zooms to the point" branch is GONE —
        // superseded by the pointer contract (issue #11: click at fit
        // does nothing; double-click owns 1:1), and at fit the fit-ta
        // surface swallows presses before cells anyway.
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_cell_clicked(move |id, _lx, _ly, ctrl, shift| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let id = id as usize;
                if !st.view.contains(&id) {
                    return;
                }
                st.cursor_touched = true; // clicks claim (untouched-cursor rule)
                if ctrl {
                    // Ctrl+click: toggle membership; cursor moves too.
                    st.selection.toggle(id);
                    st.cursor = id;
                } else if shift {
                    // Shift+click: span cursor..clicked (view order).
                    let view = st.view.clone();
                    let from = st.cursor;
                    st.selection.extend_to(&view, from, id);
                    st.cursor = id;
                } else {
                    // Plain click: collapse any selection (gate finding:
                    // after Ctrl+A there was NO deselect gesture), move
                    // the cursor.
                    st.selection.clear();
                    st.cursor = id;
                }
            }
            refresh(&win, &state);
        });
    }

    // The pump timer must outlive the event loop: dropping it stops the tick.
    let _timer = pump::start(&window, &state);

    refresh(&window, &state);

    let drives_pending = harness::install(&window, &state);

    let screenshot_requested = screenshot.is_some();
    // The shutter timer must outlive the event loop — dropping it cancels
    // the poll — so main keeps the binding even though it never touches it.
    let (_shot_timer, shot_written) = shutter::arm(&window, &state, screenshot, &drives_pending);

    window.run().expect("running event loop");
    shutter::finish(screenshot_requested, &shot_written);
    shutter::shutdown(&state);
}

/// Compute the reveal that keeps the cursor visible under the CURRENT
/// state, and mark that geometry as consumed. Callable with the state
/// borrow held — handle_nav holds it across its whole body and used to
/// re-inline this whole computation because of that.
///
/// Returns the (virtual height, viewport-y) pair for `apply_reveal`; it
/// deliberately does NOT write them itself. Keeping the writes outside
/// the borrow is what lets ONE copy of this serve both callers: handle_nav
/// cannot write them while it holds `st`.
///
/// ORDER NOTE (gate finding, two reviewers): the two copies this replaced
/// disagreed about when the consumed-geometry mark lands. handle_nav
/// marked BEFORE writing (forced: it holds the borrow); the old
/// reveal_cursor marked AFTER. Unifying on handle_nav's order is safe
/// because the mark can only be observed by a `refresh` re-entered from
/// the vp-y write, and that re-entry does not exist: Slint's `changed`
/// callbacks are DEFERRED, not synchronous. `set_vp_y` only appends the
/// change tracker to a thread-local list (i-slint-core-1.17.1
/// properties/change_tracker.rs `mark_dirty`); the notify functions run
/// later, when the event loop calls `run_change_handlers()` (via
/// `WindowInner::ensure_tree_instantiated`, window.rs, and
/// `platform::update_timers_and_animations`, platform.rs — the only two
/// callers). So no refresh can interleave between `apply_reveal`'s writes
/// and this mark in either order, and mark-then-write is observationally
/// identical to write-then-mark here.
///
/// Re-verified for OUR window, not just for the library: `changed vp-y =>
/// root.viewport-changed()` (main.slint) compiles to `change_tracker2` on
/// `InnerMainWindow`, initialized with `ChangeTracker::init(eval = flick
/// viewport_y, notify = call viewport_changed)` in the generated
/// `.../out/main.rs`. `viewport-changed` is the callback whose Rust
/// handler is `refresh` — so the one re-entry that could observe this mark
/// is that tracker, and that tracker is queued, never called from `set`.
fn reveal_scroll(
    win: &MainWindow,
    st: &mut AppState,
    viewport_h: f32,
    scroll_y: f32,
) -> (f32, f32) {
    let width = win.get_grid_width();
    let layout = GridLayout::new(st.zoom, width, viewport_h, st.view.len());
    let pos = st.cursor_pos().unwrap_or(0);
    let new_scroll = layout.scroll_to_reveal(pos, scroll_y, viewport_h);
    // This reveal IS the relayout correction for its geometry: mark it
    // consumed so refresh doesn't re-anchor on top of an already
    // consistent (geometry, offset) pair (the grid resize branch
    // double-corrected panel toggles with mixed old/new frames, and a nav
    // key racing a resize would double-correct the same way).
    st.last_view_geometry = Some((width, viewport_h));
    (layout.total_height, -new_scroll)
}

/// Write a computed reveal to the window. Order matters (spec, cursor
/// contract): the Flickable clamps viewport-y against its CURRENT
/// viewport height, so the new virtual height must land FIRST or the
/// reveal is clamped against stale bounds and the cursor scrolls out of
/// view. Call with no state borrow held (see `reveal_scroll`).
fn apply_reveal(win: &MainWindow, (virtual_height, vp_y): (f32, f32)) {
    win.set_virtual_height(virtual_height);
    win.set_vp_y(vp_y);
}

/// Re-anchor the scroll so the cursor is visible, then refresh.
fn reveal_cursor(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (viewport_h, scroll_y) = viewport_metrics(win);
    let reveal = {
        let mut st = state.borrow_mut();
        reveal_scroll(win, &mut st, viewport_h, scroll_y)
    };
    apply_reveal(win, reveal);
    refresh(win, state);
}

/// Remembered UI preferences (fileops.md: destination and rename template
/// survive across sessions). Tiny TOML in the fastcull config dir.
fn ui_prefs_path() -> Option<std::path::PathBuf> {
    // FASTCULL_NO_CONFIG: hermetic test runs must never read or write the
    // user's real ~/.config/fastcull/ui.toml (issue #13 gap, found by the
    // issue #41 sweep: a driven copy dialog displayed the user's real
    // remembered destination — FASTCULL_NO_CACHE sandboxes only the
    // cache). Gating the PATH covers both load and save in one place.
    if std::env::var_os("FASTCULL_NO_CONFIG").is_some() {
        return None;
    }
    let dirs = directories::ProjectDirs::from("org", "fastcull", "fastcull")?;
    Some(dirs.config_dir().join("ui.toml"))
}

fn load_ui_prefs() -> (Option<std::path::PathBuf>, String) {
    let Some(path) = ui_prefs_path() else {
        return (None, String::new());
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return (None, String::new());
    };
    let table: toml::Table = content.parse().unwrap_or_default();
    let dest = table
        .get("copy_dest")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from);
    let template = table
        .get("copy_template")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    (dest, template)
}

fn save_ui_prefs(dest: Option<&std::path::Path>, template: &str) {
    let Some(path) = ui_prefs_path() else { return };
    // Persist the last NON-EMPTY template (gate N1: the field now opens
    // empty by design, so a template-less copy — or just picking a
    // destination — must not erase yesterday's remembered template).
    let template = if template.trim().is_empty() {
        load_ui_prefs().1
    } else {
        template.to_string()
    };
    let template = template.as_str();
    let mut table = toml::Table::new();
    if let Some(d) = dest {
        table.insert(
            "copy_dest".into(),
            toml::Value::String(d.to_string_lossy().into_owned()),
        );
    }
    table.insert(
        "copy_template".into(),
        toml::Value::String(template.to_string()),
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, toml::to_string_pretty(&table).unwrap_or_default()).ok();
}

/// Re-read templates.toml (session open + panel toggle = the spec's
/// read-on-open live-reload). Parse errors and CLEAR warnings both land in
/// the panel warning strip.
fn reload_templates(st: &mut AppState) {
    st.templates.clear();
    st.template_warnings.clear();
    let Some(path) = fastcull_core::iptc::default_templates_path() else {
        return;
    };
    match fastcull_core::iptc::load_templates(&path) {
        Ok(load) => {
            st.templates = load.templates;
            st.template_warnings = load.entry_errors;
            st.template_warnings.extend(load.warnings);
        }
        Err(e) => st.template_warnings.push(e.to_string()),
    }
}

/// The panel field rows: the core table, in its declaration order, which
/// IS the display order. The row index is the callback contract with the
/// UI (iptc-field-committed / iptc-field-clear), so it must stay the
/// core order — hence indexing `IptcField::ALL` rather than keeping a
/// parallel list here.
///
/// An out-of-range index (a UI/core disagreement) reads as "no value" and
/// writes nowhere, exactly as the hand-written match arms did.
fn iptc_field_label(i: usize) -> &'static str {
    IptcField::ALL.get(i).map_or("field", |f| f.label())
}

fn iptc_field_get(d: &fastcull_core::iptc::IptcData, i: usize) -> Option<&String> {
    IptcField::ALL.get(i).and_then(|f| f.get(d))
}

fn iptc_field_set(d: &mut fastcull_core::iptc::IptcData, i: usize, v: Option<String>) {
    if let Some(f) = IptcField::ALL.get(i) {
        f.set(d, v);
    }
}

/// Width/height aspect of the best texture held for an image (any rung —
/// aspect is rung-invariant). None while only the placeholder exists.
fn aspect_for(st: &AppState, index: usize) -> Option<f32> {
    let size = st
        .fullres
        .iter()
        .find(|(i, _)| *i == index)
        .map(|(_, img)| img.size())
        .or_else(|| st.mids.get(&index).map(|img| img.size()))
        .or_else(|| st.images.get(&index).map(|img| img.size()))?;
    (size.height > 0).then(|| size.width as f32 / size.height as f32)
}

/// Which kitchen lane a WARM image takes — one already-decoded image the
/// app must hand to the kitchen, at the two places that happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WarmJob {
    /// Top-priority full-res fill: the sharp 1:1 source.
    Full,
    /// Native-size copy (never downscaled) into the mid rung. `terminal`
    /// also teaches the app the file's zoom ceiling (issue #8).
    Wrap { terminal: bool },
}

/// Where the warm image came from. The two sites route DIFFERENTLY and
/// always did — this enum states the difference once instead of leaving
/// two hand-copies to embody it and drift (they did, on 2026-08-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WarmCtx {
    /// The engine ANNOUNCED a fresh decode (`LoupeEvent::Ready`), for any
    /// index in the prefetch ring, not just the cursor's.
    Announced,
    /// `focus()` handed back a CACHED image for the cursor because the
    /// UI-side texture had been evicted (the rebuild path).
    FocusHit,
}

/// The warm-hit routing rule, both contexts, one function.
///
/// `mid_held` = a mid-rung texture for this index is already in `st.mids`;
/// `at_loupe` = the loupe is on screen right now.
fn route_warm(
    long: u32,
    terminal: bool,
    at_loupe: bool,
    mid_held: bool,
    ctx: WarmCtx,
) -> Option<WarmJob> {
    match ctx {
        WarmCtx::Announced => {
            // SIZE alone decides here, deliberately: a terminal image at
            // or below mid class goes through Wrap, which is native size
            // and never downscaled, so its pixels ARE the zoom ceiling
            // (issue #8) — routing it to Full instead would resize the
            // one rung the file can never re-cook. `mid_held` is ignored:
            // a freshly announced decode supersedes whatever is held.
            if is_top_rung(long, false) {
                // A Full is a large fill on the kitchen worker: only
                // worth cooking where it can be shown.
                at_loupe.then_some(WarmJob::Full)
            } else {
                Some(WarmJob::Wrap { terminal })
            }
        }
        WarmCtx::FocusHit => {
            // Route by RUNG (validator finding, 2026-08-02): only a real
            // top rung earns the Full lane — the old code parked warm
            // MIDS in the fullres slot too, which burned the top-priority
            // lane on a texture the sharp filter can never accept. Here
            // terminality DOES promote: the caller is the cursor's own
            // rebuild, and a terminal texture is that cursor's top rung.
            // `at_loupe` does not gate it either — this path only runs
            // inside the at-loupe branch of refresh.
            if is_top_rung(long, terminal) {
                Some(WarmJob::Full)
            } else if !mid_held {
                // A warm sub-top hit (the pruned-and-revisited path: the
                // engine re-announces a cached mid beyond the retained
                // window) goes through Wrap into st.mids — where the
                // soft-transit renderer looks FIRST.
                Some(WarmJob::Wrap { terminal })
            } else {
                None
            }
        }
    }
}

/// The 1:1 zoom ceiling relative to fit for the cursor image, derived from
/// its full-res texture (`ui-grid.md` zoom ladder: 1:1 means device
/// pixels). None until the top rung is adopted — the ceiling is unknowable
/// before the native dimensions are.
fn max_factor(win: &MainWindow, st: &AppState) -> Option<f32> {
    let img = st
        .fullres
        .iter()
        .find(|(i, _)| *i == st.cursor)
        .map(|(_, img)| img)?;
    let size = img.size();
    if !is_top_rung(
        size.width.max(size.height),
        st.terminal_native.contains(&st.cursor),
    ) {
        return None; // not the top rung: native size unknown
    }
    let sf = win.window().scale_factor();
    let (nw, nh) = (size.width as f32 / sf, size.height as f32 / sf);
    let s = fastcull_core::zoompan::fit_scale(win.get_grid_width(), win.get_loupe_area_h(), nw, nh);
    Some(1.0 / s)
}

/// The factor actually rendered: desired, clamped to the known 1:1
/// ceiling (an INFINITY desire = "1:1 as soon as we know where that is").
fn clamped_factor(win: &MainWindow, st: &AppState) -> f32 {
    match max_factor(win, st) {
        Some(max) => st.zoom_factor.clamp(1.0, max.max(1.0)),
        None => st.zoom_factor.max(1.0),
    }
}

/// Pointer state machine bridge (issue #11): current machine state +
/// per-call geometry from the live window numbers. The machine itself
/// lives in fastcull-core (rule 5) — this is pure normalization.
fn machine_ctx(
    win: &MainWindow,
    st: &AppState,
) -> (
    fastcull_core::pointer::ViewState,
    fastcull_core::pointer::Geometry,
) {
    use fastcull_core::pointer as pm;
    let (vw, vh) = (win.get_grid_width(), win.get_loupe_area_h());
    // Native dims from the full-res texture when present (same source as
    // max_factor); otherwise the viewport — extents degenerate to the
    // fit view, which is exactly what is on screen in that case.
    // Native dims: full-res texture first; else any lower rung via
    // aspect_for's chain (WRONG magnitude but CORRECT aspect — enough
    // for the fit frame and letterbox rejection during decode gaps);
    // else the viewport.
    let sf = win.window().scale_factor();
    let native = st
        .fullres
        .iter()
        .find(|(i, _)| *i == st.cursor)
        .map(|(_, img)| {
            let s = img.size();
            (s.width as f32 / sf, s.height as f32 / sf)
        })
        .or_else(|| aspect_for(st, st.cursor).map(|aspect| (vh * aspect, vh)))
        .unwrap_or((vw, vh));
    // The COUNT is the Grid arm's payload, so it is still derived here;
    // the PREDICATE is `at_loupe()` (they agree — a single 1 sits at the
    // last index and zoom is clamped to the ladder).
    let columns = grid::ZOOM_COLUMNS[st.zoom.min(grid::ZOOM_COLUMNS.len() - 1)];
    let state = if !st.at_loupe() {
        pm::ViewState::Grid {
            columns: columns as u8,
        }
    } else {
        let f = clamped_factor(win, st);
        if f <= 1.0 {
            pm::ViewState::Fit
        } else {
            pm::ViewState::Zoomed { factor: f }
        }
    };
    // Where the fit view really renders: the cursor's N=1 grid cell in
    // keys-space (validator MAJOR: the fit view is a grid strip cell,
    // scroll-dependent — not an image centered in the viewport).
    let fit_cell = st.at_loupe().then(|| {
        // The layout is bounded by the GRID area's height (below the filter
        // bar), not by `vh`/`loupe_area_h` — the zoom overlay covers the bar
        // but the fit view does not.
        let layout = GridLayout::new(st.zoom, vw, win.get_grid_height(), st.view.len());
        let pos = st.cursor_pos().unwrap_or(0);
        let (cx, cy) = layout.position(pos);
        let scroll_y = (-win.get_vp_y()).max(0.0);
        let bar_h = vh - win.get_grid_height(); // filter bar height (0 when hidden)
        (
            cx,
            bar_h + cy - scroll_y,
            layout.cell_width,
            layout.cell_height,
        )
    });
    let geo = pm::Geometry {
        viewport_w: vw,
        viewport_h: vh,
        native_w: native.0,
        native_h: native.1,
        max_factor: max_factor(win, st),
        pan_center: st.pan_center,
        fit_cell,
    };
    (state, geo)
}

/// Loupe double-click (fit or zoomed surface): straight to the machine.
///
/// There is deliberately NO app-level proximity check here. The persona
/// rule it was meant to serve — scanning eye, then beak, then wingtip in
/// quick succession is three independent re-centers, never a jump to 1:1 —
/// is already enforced by Slint itself: `check_repeat` restarts the click
/// count unless the second press lands within 10 logical px of the first
/// (`i-slint-core-1.17.1/input.rs`, `square_length() < 100`), so
/// `double-clicked` cannot fire for distant presses at all.
///
/// The check that used to live here was not merely redundant, it VETOED
/// the gesture it guarded (validator FAIL-1 / QE D1, 2026-07-30). It
/// compared the two clicks as FRACTIONAL IMAGE coordinates — but Slint
/// fires `clicked` before `double-clicked`, and the first click's handler
/// (`on_loupe_clicked`) re-centers the view and refreshes, moving the image
/// under a stationary pointer. The second press therefore lands on the same
/// screen pixel but a DIFFERENT image fraction, so the measured "distance"
/// was really the recenter displacement: at 1:1 on a 1440x900 window a
/// double-click 200 px right of centre measured 520 px and was rejected.
/// Above fit the gesture never reached 1:1 at all; only from fit (where a
/// click re-centers nothing) did it work — which is why it passed review.
fn handle_loupe_double_click(win: &MainWindow, state: &Rc<RefCell<AppState>>, x: f32, y: f32) {
    use fastcull_core::pointer as pm;
    let action = {
        let mut st = state.borrow_mut();
        let (ms, geo) = machine_ctx(win, &st);
        st.cursor_touched = true;
        pm::step(ms, pm::PointerInput::DoubleClick { pos: (x, y) }, &geo).1
    };
    apply_pointer_action(win, state, action);
}

/// Apply a machine action. Grid-routing actions (GridClick/GridScroll/
/// NativeDrag) never reach this — the grid keeps its native paths; the
/// machine covers them for the table's completeness.
fn apply_pointer_action(
    win: &MainWindow,
    state: &Rc<RefCell<AppState>>,
    action: fastcull_core::pointer::Action,
) {
    use fastcull_core::pointer::Action;
    {
        let mut st = state.borrow_mut();
        match action {
            Action::SetZoom { factor, center } => {
                st.pan_center = center;
                st.zoom_factor = factor;
                if factor <= 1.0 {
                    st.pan_center = (0.5, 0.5); // fit forgets the pan spot
                }
            }
            Action::Recenter { center } => {
                st.pan_center = center;
            }
            Action::EnterLoupe => {
                st.enter_loupe(1.0);
                // Also from INSIDE the loupe (a double-click while zoomed
                // is the way back to fit), so these are unconditional.
                st.zoom_factor = 1.0;
                st.pan_center = (0.5, 0.5);
            }
            Action::None
            | Action::Reserved(_)
            | Action::GridScroll { .. }
            | Action::GridNativeDrag
            | Action::GridClick => return,
        }
    }
    refresh(win, state);
}

// `capture_pan` — the read-back that folded Flickable offset deltas into
// `pan_center` — is GONE (issue #46). It inferred a drag from
// displacement, and a fling's physics binding fed it animated offsets
// nobody dragged, corrupting the carried centre on every refresh of the
// decay. Per the #16/#22 doctrine (intent only ever from a POSITIVE
// signal), the drag itself is now the signal: the overlay's TouchArea
// reports it through `loupe-dragged`, Rust recenters through the pointer
// machine, and the offsets have exactly one writer. There is nothing
// left to read back.

/// Populate the panel models for the current batch (selection in view
/// order, or the cursor). Field rows get the tri-state UI mapping: common
/// value across the batch = shown; differing values = `mixed`; unset
/// everywhere = untouched. Keyword chips show the batch UNION with
/// coverage counts on multi-selections (persona: an un-revertible
/// batch-destructive X is unacceptable — removal arms the shared slot).
fn refresh_iptc_panel(win: &MainWindow, st: &mut AppState) {
    win.set_iptc_visible(st.iptc_visible);
    if !st.iptc_visible {
        return;
    }
    let batch = st.selection.batch(&st.view, st.cursor);
    win.set_iptc_batch_label(
        match batch.len() {
            0 => "No image".to_string(),
            1 => st.labels.get(batch[0]).cloned().unwrap_or_default(),
            n => format!("{n} images selected"),
        }
        .into(),
    );
    win.set_iptc_warning(st.template_warnings.join("\n").into());
    // Build plain-data snapshots first; the Slint models are rebuilt ONLY
    // when content changed (gate finding: unconditional rebuilds tore the
    // field editors down mid-typing on every 33 ms engine tick).
    let rows: Vec<(String, String, bool)> = (0..IptcField::ALL.len())
        .map(|i| {
            let mut vs = batch
                .iter()
                .filter_map(|id| st.iptc.get(*id).map(|d| iptc_field_get(d, i).cloned()));
            let head = vs.next().flatten();
            let mixed = {
                let mut vs = batch
                    .iter()
                    .filter_map(|id| st.iptc.get(*id).map(|d| iptc_field_get(d, i).cloned()));
                let h = vs.next().flatten();
                vs.any(|v| v != h)
            };
            (
                iptc_field_label(i).to_string(),
                if mixed {
                    String::new()
                } else {
                    head.unwrap_or_default()
                },
                mixed,
            )
        })
        .collect();
    let mut chip_data: Vec<(String, usize)> = Vec::new();
    for id in &batch {
        if let Some(d) = st.iptc.get(*id) {
            for kw in &d.keywords {
                match chip_data.iter_mut().find(|(t, _)| t == kw) {
                    Some((_, n)) => *n += 1,
                    None => chip_data.push((kw.clone(), 1)),
                }
            }
        }
    }
    let total = batch.len();
    let chips: Vec<(String, String)> = chip_data
        .into_iter()
        .map(|(text, n)| {
            let cov = if total > 1 {
                format!("{n}/{total}")
            } else {
                String::new()
            };
            (text, cov)
        })
        .collect();
    let names: Vec<String> = st.templates.iter().map(|t| t.name.clone()).collect();

    if st.panel_cache.rows != rows {
        win.set_iptc_fields(slint::ModelRc::new(VecModel::from(
            rows.iter()
                .map(|(label, value, mixed)| IptcFieldRow {
                    label: label.clone().into(),
                    value: value.clone().into(),
                    mixed: *mixed,
                })
                .collect::<Vec<_>>(),
        )));
        st.panel_cache.rows = rows;
    }
    if st.panel_cache.chips != chips {
        win.set_iptc_keywords(slint::ModelRc::new(VecModel::from(
            chips
                .iter()
                .map(|(text, cov)| KeywordChip {
                    text: text.clone().into(),
                    coverage: cov.clone().into(),
                })
                .collect::<Vec<_>>(),
        )));
        st.panel_cache.chips = chips;
    }
    if st.panel_cache.names != names {
        win.set_iptc_templates(slint::ModelRc::new(VecModel::from(
            names
                .iter()
                .map(|n| slint::SharedString::from(n.as_str()))
                .collect::<Vec<_>>(),
        )));
        st.panel_cache.names = names;
    }
    win.set_iptc_revert_enabled(!st.revert_ids.is_empty());
    win.set_iptc_revert_label(st.revert_label.clone().into());
}

/// Persist + arm the shared revert slot after a batch mutation (template
/// Apply, manual field commit, keyword add/chip removal — every one, per
/// the user decision). `snapshots` are pre-mutation states parallel to
/// `ids`; writes go through the serialized writer thread.
fn commit_batch_mutation(
    st: &mut AppState,
    ids: &[usize],
    snapshots: Vec<fastcull_core::iptc::IptcData>,
    label: &str,
) {
    st.revert.store(snapshots);
    st.revert_ids = ids.to_vec();
    st.revert_label = format!("Revert: {label}");
    st.touched_iptc.extend(ids.iter().copied());
    if let Some(writer) = &st.writer {
        for id in ids {
            if let (Some(path), Some(data)) = (st.paths.get(*id), st.iptc.get(*id)) {
                writer.iptc(path.clone(), data.clone());
            }
        }
    }
}

/// Switch the single-choice filter chip (spec cursor rule + persona G2).
fn apply_filter_change(
    win: &MainWindow,
    state: &Rc<RefCell<AppState>>,
    filter: fastcull_core::filter::PickFilter,
) {
    {
        let mut st = state.borrow_mut();
        st.query.filter = filter;
        recompute_view_keep_cursor(&mut st, true); // the USER re-filtered
    }
    reveal_cursor(win, state);
}

fn handle_nav(win: &MainWindow, state: &Rc<RefCell<AppState>>, key: &str) {
    let t0 = trace_start();
    handle_nav_inner(win, state, key);
    trace_slow(&format!("handle_nav({key})"), t0);
}

fn handle_nav_inner(win: &MainWindow, state: &Rc<RefCell<AppState>>, key: &str) {
    let (layout, viewport_h, scroll_y) = current_geometry(win, state);
    let mut st = state.borrow_mut();
    // Marks and navigation claim the cursor (issue #4); zoom keys do not
    // move it and stay neutral.
    if matches!(
        key,
        "pick"
            | "reject"
            | "clear"
            | "left"
            | "right"
            | "up"
            | "down"
            | "pgup"
            | "pgdn"
            | "home"
            | "end"
            | "burst-prev"
            | "burst-next"
    ) {
        st.cursor_touched = true;
    }
    match key {
        "pick" | "reject" | "clear" => {
            let pick = match key {
                "pick" => fastcull_core::catalog::PickState::Picked,
                "reject" => fastcull_core::catalog::PickState::Rejected,
                _ => fastcull_core::catalog::PickState::Unmarked,
            };
            let cursor = st.cursor;
            // Marks land on the cursor image only while it is in the view
            // (an empty filtered view has nothing to mark).
            if let (Some(old_pos), Some(slot)) = (st.cursor_pos(), st.picks.get_mut(cursor)) {
                *slot = pick;
                st.touched.insert(cursor);
                if let (Some(writer), Some(path)) = (&st.writer, st.paths.get(cursor)) {
                    writer.mark(path.clone(), pick);
                }
                // Advance/removal composition (spec, persona G1): net
                // movement is exactly one image. Auto-advance after Y/N
                // (user decision 2026-07-25; future config option); U stays.
                recompute_view(&mut st);
                match fastcull_core::filter::cursor_after_mark(
                    cursor,
                    old_pos,
                    &st.view,
                    key != "clear",
                ) {
                    Some(id) => st.cursor = id,
                    None => {
                        // Inbox zero (persona G2): leaving the loupe — the
                        // empty state is a grid-level view.
                        st.exit_loupe();
                    }
                }
            }
        }
        // One seamless zoom axis (spec): columns -> loupe fit -> x1.5
        // ladder -> 1:1 (ui-grid.md Loupe zoom ladder, 2026-07-25).
        "one2one" => {
            // Z: fit -> 1:1; zoomed (1:1 or intermediate) -> back to fit;
            // from a grid zoom: jump straight to loupe 1:1.
            if st.loupe.is_some() && !st.enter_loupe(f32::INFINITY) {
                if st.zoom_factor > 1.0 {
                    st.zoom_factor = 1.0;
                    st.pan_center = (0.5, 0.5); // fit forgets the pan spot
                } else if max_factor(win, &st).is_none_or(|max| max > 1.0) {
                    // Small-file guard (validator L1): a known ceiling at
                    // or below fit has no 1:1 to jump to; leaving the
                    // desire at fit keeps the next `-` meaningful.
                    st.zoom_factor = f32::INFINITY;
                }
            }
        }
        "grid" => {
            if !st.at_loupe() && st.zoom_factor <= 1.0 {
                // Already at a grid zoom: Esc/G collapses the selection
                // (the deselect gesture — gate finding).
                st.selection.clear();
            }
            st.exit_loupe();
            // At a grid zoom there is no loupe to leave, but a carried
            // factor (and its pan) is still dropped here.
            st.zoom_factor = 1.0;
            st.pan_center = (0.5, 0.5);
        }
        "zoom-in" => {
            if st.at_loupe() {
                if st.loupe.is_some() {
                    // Climb one x1.5 stop from the CLAMPED factor (the
                    // desired one may be INFINITY from an earlier Z). An
                    // unknown ceiling (full-res not decoded yet) climbs
                    // optimistically; the render clamp lands it at 1:1.
                    let actual = clamped_factor(win, &st);
                    st.zoom_factor = match max_factor(win, &st) {
                        Some(max) => fastcull_core::zoompan::ladder_up(actual, max),
                        None => actual * fastcull_core::zoompan::ZOOM_STEP,
                    };
                }
            } else {
                if st.zoom + 1 == grid::ZOOM_COLUMNS.len() - 1 {
                    st.remember_grid_zoom(); // this step crosses INTO the loupe
                }
                st.zoom = grid::zoom_step(st.zoom, 1);
            }
        }
        "zoom-out" => {
            if st.zoom_factor > 1.0 {
                // Retrace the x1.5 stops down to fit, never straight to
                // the grid. Unknown ceiling: nothing above fit was ever
                // rendered, so fit is the only honest stop.
                let actual = clamped_factor(win, &st);
                st.zoom_factor = if max_factor(win, &st).is_some() {
                    fastcull_core::zoompan::ladder_down(actual)
                } else {
                    1.0
                };
                if st.zoom_factor <= 1.0 {
                    st.pan_center = (0.5, 0.5); // fit forgets the pan spot
                }
            } else {
                st.zoom = grid::zoom_step(st.zoom, -1);
            }
        }
        // [ / ]: previous/next burst boundary over the FILTERED view
        // (burst-grouping.md UI contract): first visible frame of the
        // adjacent group; singles are their own territory; clamps.
        "burst-prev" | "burst-next" => {
            if !st.view.is_empty() {
                let pos = st.cursor_pos().unwrap_or(0);
                let view = st.view.clone();
                let group_of = |p: usize| st.burst_of.get(view[p]).copied().flatten();
                let new_pos = fastcull_core::burst::next_boundary(
                    pos,
                    view.len(),
                    group_of,
                    key == "burst-next",
                );
                st.cursor = view[new_pos];
                // Plain navigation resets the Shift-span anchor, same as
                // the arrow keys (selection contract, ui-grid.md).
                st.selection.reset_anchor();
            }
        }
        "select-all" => {
            st.cursor_touched = true;
            let view = st.view.clone();
            st.selection.select_all(&view);
        }
        nav => {
            let (nav, extends) = match nav {
                "left" => (Nav::Left, false),
                "right" => (Nav::Right, false),
                "up" => (Nav::Up, false),
                "down" => (Nav::Down, false),
                "shift-left" => (Nav::Left, true),
                "shift-right" => (Nav::Right, true),
                "shift-up" => (Nav::Up, true),
                "shift-down" => (Nav::Down, true),
                "pgup" => (Nav::PageUp, false),
                "pgdn" => (Nav::PageDown, false),
                "home" => (Nav::Home, false),
                "end" => (Nav::End, false),
                _ => return,
            };
            if extends {
                st.cursor_touched = true; // shift-nav claims like plain nav
            }
            // Navigation happens over VIEW positions; the cursor stays an
            // image id (M5 filter model).
            if !st.view.is_empty() {
                let rows_per_page =
                    ((viewport_h / (layout.cell_height + grid::CELL_GAP)) as usize).max(1);
                let pos = st.cursor_pos().unwrap_or(0);
                let new_pos =
                    grid::navigate(pos, st.view.len(), layout.columns, rows_per_page, nav);
                let from = st.cursor;
                st.cursor = st.view[new_pos];
                if extends {
                    // Shift+arrow: span anchor..cursor (core model).
                    let view = st.view.clone();
                    let to = st.cursor;
                    st.selection.extend_to(&view, from, to);
                } else {
                    st.selection.reset_anchor();
                }
            }
        }
    }
    // Keep the cursor visible under the (possibly new) layout — the same
    // reveal reveal_cursor performs, with the borrow still held.
    let reveal = reveal_scroll(win, &mut st, viewport_h, scroll_y);
    drop(st);
    apply_reveal(win, reveal);
    refresh(win, state);
}

/// The (viewport height, scroll offset) pair, read straight off the
/// window. Split out of `current_geometry` because `reveal_cursor` needs
/// only these two — it built a `GridLayout` here and threw it away, then
/// `reveal_scroll` built an identical one (gate finding).
fn viewport_metrics(win: &MainWindow) -> (f32, f32) {
    (win.get_grid_height(), (-win.get_vp_y()).max(0.0))
}

fn current_geometry(win: &MainWindow, state: &Rc<RefCell<AppState>>) -> (GridLayout, f32, f32) {
    let st = state.borrow();
    let (viewport_h, scroll_y) = viewport_metrics(win);
    let layout = GridLayout::new(st.zoom, win.get_grid_width(), viewport_h, st.view.len());
    (layout, viewport_h, scroll_y)
}

/// Rebuild the windowed model for the current viewport.
fn refresh(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let t0 = trace_start();
    refresh_inner(win, state);
    trace_slow("refresh", t0);
}

fn refresh_inner(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (layout, viewport_h, mut scroll_y) = current_geometry(win, state);
    let mut st = state.borrow_mut();
    // Relayout detection (issue #16): a geometry change since the last
    // refresh is chrome/window movement (panel toggle, resize), never
    // user scrolling.
    let geom_now = (win.get_grid_width(), viewport_h);
    let prev_geom = st.last_view_geometry;
    let relayout = prev_geom.is_some_and(|g| g != geom_now);
    st.last_view_geometry = Some(geom_now);
    // A view that mutated since the last refresh (metadata re-sort
    // during load, live filter removal) displaces the cursor without
    // any scrolling — the follow-scroll claim must re-anchor instead
    // (issue #22).
    let view_mutated = st.last_view_generation != st.view_generation;
    st.last_view_generation = st.view_generation;
    let view_len = st.view.len();
    // Issue #25's one re-sort: the instant the last metadata job lands, the
    // WHOLE grid reorders from the provisional filename order into the
    // user's sort. Every other view mutation moves a few cells; this one
    // moves all of them, so keeping the raw scroll offset lands the
    // viewport on unrelated content and can leave the cursor cell
    // off-screen entirely — the next arrow key then teleports the view.
    // The N=1 strip re-anchors through its own block below; the multi-
    // column grid had no path for it, because the relayout branch is gated
    // on GEOMETRY changes and this is a content change (validator FAIL,
    // 2026-07-30). Reveal once, on the edge only — not on every mutation,
    // which would fight live filter removal and per-mark recomputes.
    // The edge is consumed only when this refresh can actually act on it: a
    // pre-layout pass (no height yet, minimized window) would otherwise
    // swallow it and the re-sort would silently never re-anchor for the rest
    // of the session (validator concern, 2026-07-31).
    let can_anchor = viewport_h > 0.0 && view_len > 0;
    let load_settled = st.metadata_complete() && !st.last_metadata_complete && can_anchor;
    if can_anchor {
        st.last_metadata_complete = st.metadata_complete();
    }

    // GRID-level resize anchoring (user report: shrink the window and
    // the list "scrolls up", grow and it "scrolls down"). Row pitch is
    // a pure function of the grid width, so keeping the raw pixel
    // offset across a relayout lands it on DIFFERENT content. Anchor
    // CONTENT instead: keep the top-visible row's position (fractional,
    // so partial-row offsets survive), pin the bottom clamp to the
    // bottom (growing at End must not strand the viewport mid-list),
    // and keep the cursor visible if it was. The N=1 strip has its own
    // re-anchor in the loupe block below (issue #16).
    // The load-settled re-sort (issue #25) at MULTI-COLUMN zoom: content,
    // not geometry, so the relayout branch below cannot see it. Put the
    // cursor's cell back on screen and let the scroll follow it.
    // ...but only for a cursor the user was actually looking at. Wheel and
    // scrollbar browsing do NOT claim the cursor, and the cursor contract
    // says an off-screen cursor stays off-screen until the next arrow key —
    // so yanking the viewport back would be the very "it moved with no
    // input" defect this change exists to remove, and would regress a
    // browsing user against the old behaviour (validator FAIL, 2026-07-31).
    // Same guard the relayout branch below already applies.
    if load_settled && layout.columns > 1 {
        let cur_pos = st.cursor_pos().unwrap_or(0);
        // The guard itself lives in core (rule 5) and is unit-tested there:
        // grid::scroll_after_resort. It shipped into review MISSING from the
        // app-level version, so it does not belong in the app.
        let corrected = grid::scroll_after_resort(
            &layout,
            cur_pos,
            scroll_y,
            viewport_h,
            st.last_cursor_visible,
        );
        // Unconditional marker: a test must be able to see that the flip
        // HAPPENED, not only that it moved something (validator: the old
        // trace fired solely inside the >=0.5px branch, so a run where the
        // re-sort never occurred was indistinguishable from one where it
        // occurred and correctly changed nothing).
        trace_mark(&format!(
            "load settled: cursor pos {cur_pos}, scroll {scroll_y:.0} -> {corrected:.0}  (cursor was {})",
            if st.last_cursor_visible {
                "visible"
            } else {
                "off-screen; offset kept"
            }
        ));
        win.set_virtual_height(layout.total_height);
        win.set_vp_y(-corrected);
        scroll_y = corrected;
    }
    if relayout && layout.columns > 1 && view_len > 0 && viewport_h > 0.0 {
        if let Some((old_width, old_viewport_h)) = prev_geom {
            let old_layout = GridLayout::new(st.zoom, old_width, old_viewport_h, view_len);
            let old_pitch = old_layout.cell_height + grid::CELL_GAP;
            let new_pitch = layout.cell_height + grid::CELL_GAP;
            let old_max = (old_layout.total_height - old_viewport_h).max(0.0);
            let new_max = (layout.total_height - viewport_h).max(0.0);
            let mut corrected = if old_max >= 1.0 && scroll_y >= old_max - 1.0 {
                // At the bottom CLAMP: the bottom stays the bottom. The
                // old_max > 0 gate keeps fits-the-viewport views (where
                // scroll 0 is vacuously "at the clamp") pinned to the
                // TOP instead (validator+QE D1: a fits-to-overflow grow
                // jumped scroll 0 to new_max).
                new_max
            } else if old_pitch > 0.0 {
                (scroll_y / old_pitch * new_pitch).clamp(0.0, new_max)
            } else {
                scroll_y
            };
            // Cursor visibility carries across the relayout (the panel
            // toggle already honors this via reveal_cursor).
            let cur_pos = st.cursor_pos().unwrap_or(0);
            let (_, old_cur_top) = old_layout.position(cur_pos);
            let cursor_was_visible = old_cur_top < scroll_y + old_viewport_h
                && old_cur_top + old_layout.cell_height > scroll_y;
            if cursor_was_visible {
                corrected = layout.scroll_to_reveal(cur_pos, corrected, viewport_h);
            }
            if (corrected - scroll_y).abs() >= 0.5 {
                trace_mark(&format!(
                    "grid relayout re-anchor: scroll {scroll_y:.0} -> {corrected:.0} \
                     (pitch {old_pitch:.1} -> {new_pitch:.1})"
                ));
            }
            win.set_virtual_height(layout.total_height);
            win.set_vp_y(-corrected);
            scroll_y = corrected; // this very pass renders the anchored view
        }
    }
    // The scroll offset is final for this pass: record whether the cursor is
    // on screen, for the next refresh's load-settled decision (above).
    // NOTE: this is after both multi-column writes to `vp_y`, but BEFORE the
    // loupe block's own write and follow-scroll claim — so on the N=1 path
    // the recorded value is one pass stale. Harmless only because the
    // consumer is gated on `columns > 1`; revisit if that gate is relaxed.
    if can_anchor {
        st.last_cursor_visible = st
            .cursor_pos()
            .is_some_and(|p| layout.is_visible(p, scroll_y, viewport_h));
    }
    // Visible VIEW positions; `ids` are the image ids shown there (the two
    // coincide only with filter=All + capture sort before keys arrive).
    let range = layout.visible_range(view_len, scroll_y, viewport_h, MARGIN_ROWS);
    let ids: Vec<usize> = range.clone().map(|pos| st.view[pos]).collect();

    // Tell the engine what is on screen (priority promotion).
    if let Some(pipeline) = &st.pipeline {
        pipeline.promote(
            ids.iter().copied(),
            fastcull_core::pipeline::Priority::Visible,
        );
    }

    // Thumbs entering the window go to the texture kitchen — the UI
    // thread never decodes (01-architecture.md). No budget and no
    // follow-up timer: submission is O(pending) and the kitchen's
    // completion nudge drives adoption. Encoded bytes are MOVED into the
    // job (the SQLite cache keeps the encoded copy); a submitted index
    // vanishes from `thumb_jpegs`, which is what makes this loop
    // naturally idempotent across refreshes.
    let mut to_prep: Vec<usize> = ids
        .iter()
        .copied()
        .filter(|i| st.thumb_jpegs.contains_key(i) && !st.images.contains_key(i))
        .collect();
    // Cursor first (issue #46): at the loupe the cursor's thumb is the
    // transit rescue rung, and the kitchen is FIFO — cooking a margin
    // cell ahead of it extends a residual hold pointlessly (observed:
    // the margin thumb took the first cook slot and the hold cap fired
    // 9 ms before the cursor's own thumb landed).
    if let Some(pos) = to_prep.iter().position(|i| *i == st.cursor) {
        let c = to_prep.remove(pos);
        to_prep.insert(0, c);
    }
    for index in to_prep {
        if let Some(jpeg) = st.thumb_jpegs.remove(&index) {
            st.kitchen.submit_thumb(index, jpeg);
        }
    }

    // POSITIVE claim gate (issues #16/#22 family, final form): the
    // follow-scroll claim fires only on actual scrollbar activity — the
    // one gesture the cursor contract names. Inferring "scrolled" from
    // displacement alone kept misfiring through timing windows
    // (folder-load re-sorts, panel-toggle reflows, Windows DPI clamp
    // races) that no elimination list closes. Consumed EVERY refresh —
    // grid included — so it always means "since the last refresh": a
    // flag armed by the GRID scrollbar (or a loupe drag too small to
    // displace) must never claim minutes later (gate finding M1).
    let scrolled = win.get_sb_activity();
    if scrolled {
        win.set_sb_activity(false);
    }
    // Loupe: at 1-column zoom the visible image IS the cursor (spec, cursor
    // contract): scrolling moves the cursor, and full-res always targets
    // what the user is looking at.
    let at_loupe = st.at_loupe();
    if at_loupe && view_len > 0 {
        // Scroll moves the cursor ONLY when the cursor's cell left the
        // viewport: unconditionally snapping to the center row made arrow
        // keys a no-op on tall windows where >2 rows fit (validator
        // finding — move, no scroll needed, snap-back to center).
        let cur_pos = st.cursor_pos().unwrap_or(0);
        let (_, cur_top) = layout.position(cur_pos);
        let cur_visible = layout.is_visible(cur_pos, scroll_y, viewport_h);
        // PARTLY visible is not good enough at one column (validator
        // 2026-07-30). Now that the N=1 cell is bounded by the viewport
        // HEIGHT, a vertical resize changes the row pitch — so keeping the
        // raw pixel offset lands mid-strip and the loupe shows the bottom
        // of one photo with the top of the next below it. That is strictly
        // worse than the crop this bound was added to fix, so a geometry or
        // view change re-anchors whenever the cell is not WHOLLY on screen,
        // not only when it has left entirely. (Plain scrolling is left
        // alone: a scrollbar drag legitimately parks a cell half-way, and
        // re-anchoring there would fight the user's hand.)
        let fully_visible = cur_top - grid::CELL_GAP >= scroll_y - 0.5
            && cur_top + layout.cell_height + grid::CELL_GAP <= scroll_y + viewport_h + 0.5;
        let geometry_moved = relayout || view_mutated;
        // Guard against pre-layout geometry (issue #4 debugging: refreshes
        // before the window lays out see a NEGATIVE viewport height, made
        // the cursor look "scrolled away", and spuriously claimed it —
        // killing the untouched-snap and leaving the final cursor racy).
        if viewport_h > 0.0 && (!cur_visible || (geometry_moved && !fully_visible)) {
            if !cur_visible && scrolled && !geometry_moved {
                let center_row = ((scroll_y + viewport_h * 0.5)
                    / (layout.cell_height + grid::CELL_GAP))
                    as usize;
                let claimed = center_row.min(view_len - 1);
                trace_mark(&format!(
                    "follow-scroll claim: cursor pos {cur_pos} -> {claimed}"
                ));
                st.cursor = st.view[claimed];
                st.cursor_touched = true; // scrolling the loupe IS cursor movement
            } else {
                // Geometry changed under the cursor (panel toggle, window
                // RESIZE — the user's reported bug): this is NOT
                // scrolling. Keep the cursor, move the viewport back to
                // it; a follow-up refresh renders the corrected window
                // (issue #16 — the claim above used to swap the photo).
                let corrected = layout.scroll_to_reveal(cur_pos, scroll_y, viewport_h);
                win.set_virtual_height(layout.total_height);
                win.set_vp_y(-corrected);
                trace_mark(&format!(
                    "relayout re-anchor: cursor kept at pos {cur_pos}, scroll {scroll_y:.0} -> {corrected:.0}"
                ));
                let win_weak = win.as_weak();
                let state_rc = Rc::clone(state);
                slint::Timer::single_shot(std::time::Duration::from_millis(0), move || {
                    if let Some(win) = win_weak.upgrade() {
                        refresh(&win, &state_rc);
                    }
                });
            }
        }
        if let Some(loupe) = &st.loupe {
            // focus() returns the cached image on a warm hit: the rebuild
            // path for textures evicted UI-side (validator finding — going
            // backwards previously degraded to the thumb forever).
            let focus_index = st.cursor;
            // Ladder target: fit view needs the viewport in physical pixels;
            // any factor above fit demands the top rung (quality rule as
            // revised by #21: the top rung is still ALWAYS requested;
            // until it lands the view renders soft-flagged, never
            // unflagged).
            let display_long = if st.zoom_factor > 1.0 {
                u32::MAX
            } else {
                (win.get_grid_width() * win.window().scale_factor()) as u32
            };
            let hit = loupe.focus(focus_index, display_long);
            let missing = !st.fullres.iter().any(|(i, _)| *i == focus_index);
            if let (Some(image), true) = (hit, missing) {
                // Kitchen fills the buffer off-thread; the pending guards
                // absorb the refresh loop re-asking every frame while it
                // cooks. The routing rule itself (and why this context
                // differs from the pump's) lives in `route_warm`.
                let long = image.width.max(image.height);
                match route_warm(
                    long,
                    st.terminal_native.contains(&focus_index),
                    at_loupe,
                    st.mids.contains_key(&focus_index),
                    WarmCtx::FocusHit,
                ) {
                    Some(WarmJob::Full) => st.kitchen.submit_full(focus_index, image),
                    Some(WarmJob::Wrap { terminal }) => {
                        st.kitchen.submit_wrap(focus_index, image, terminal)
                    }
                    None => {}
                }
            }
        }
    }
    // Intermediate-zoom ladder (user bug report: cells looked bad between
    // 8-column and loupe): cells wider than 320*1.25 physical px outgrow the
    // thumb — climb them to the mid rung via the same 25% rule.
    let cell_phys = layout.cell_width * win.window().scale_factor();
    let want_mid = !at_loupe && cell_phys > 320.0 * 1.25;
    if want_mid {
        let stx = &mut *st;
        if let Some(loupe) = &stx.loupe {
            // ensure() also returns cached images no event will announce
            // (the zoom-walk bug: pruned-and-revisited cells stayed
            // thumbs). Downscales cook on the kitchen worker; stale ones
            // for scrolled-away cells are culled here, at the submission
            // wave (spec rule) — landed ones are still adopted.
            stx.kitchen.cull_mids(&ids);
            for (index, image) in stx.va.ensure(&ids, cell_phys as u32, loupe) {
                if stx.mids.len() >= MIDS_CAP && !stx.mids.contains_key(&index) {
                    break;
                }
                stx.kitchen.submit_mid(index, image);
            }
        }
    }
    st.mids.retain(|i, _| ids.contains(i));
    st.va.prune(&ids);

    let cursor = st.cursor;
    let fullres_for = |st: &AppState, index: usize| -> Option<slint::Image> {
        st.fullres
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, img)| img.clone())
    };
    // Zoom overlay (any factor above fit, capped at 1:1): only shown when
    // a texture of the CURSOR's own exists (a stale previous image must
    // never pose as the current one), sized as fit-extent × factor in
    // logical pixels so the capped factor means device pixels on HiDPI.
    // Quality rule as revised by issue #21: the top rung renders sharp;
    // below it the view stays at the carried factor rendered SOFT from
    // the cursor's mid rung, FLAGGED by the cue pill — never unflagged
    // upscaling, and fit only when even the mid is missing.
    let factor = clamped_factor(win, &st);
    let overlay = factor > 1.0 && at_loupe;
    // Issue #21 (user-approved contract): above fit, always show the
    // CURRENT image at the carried factor and pan center — rung QUALITY
    // may degrade (mid rung upscaled, flagged by the cue pill), position
    // and identity may not. Extended below the mid by issue #46: the
    // ladder's last rung with pixels is the cursor's own grid THUMB —
    // the overlay NEVER drops to fit while the desire is above it (the
    // transit contract), however cold the neighbor.
    let sharp = fullres_for(&st, cursor).filter(|img| {
        is_top_rung(
            img.size().width.max(img.size().height),
            st.terminal_native.contains(&cursor),
        )
    });
    let soft = if sharp.is_none() && overlay {
        // The cursor's own mid — or a warm sub-top texture the engine
        // re-announced into the fullres slot (the pruned-and-revisited
        // path: validator M3, held-left beyond the mids window used to
        // re-strobe fit with the pixels literally in hand).
        st.mids
            .get(&cursor)
            .cloned()
            .or_else(|| fullres_for(&st, cursor))
    } else {
        None
    };
    // Thumb rung (issue #46): below the mid, the cursor's own 320 px grid
    // thumb, upscaled to the carried geometry — colored mush at 1:1, and
    // exactly right during transit (persona: "what my eye needs is that
    // the blob stays where the blob was"). Identity is intact: it is the
    // CURRENT image's own thumb, flagged by the cue pill like every
    // sub-top rung. A decode-FAILED cursor skips the rescue (validator
    // finding): a file whose 320 px thumb survived while every loupe
    // rung is corrupt would otherwise sit at 1:1 behind a "loading"
    // pill that can never complete, hiding the strip's failed badge —
    // fit plus the badge is the honest floor there.
    let (soft, soft_is_thumb) = match soft {
        Some(img) => (Some(img), false),
        None if sharp.is_none() && overlay && !st.failed.contains(&cursor) => {
            (st.images.get(&cursor).cloned(), true)
        }
        None => (None, false),
    };
    // The soft view needs a FINITE factor: an INFINITY pin (Z) resolves
    // against native dims we don't have yet — carry the last resolved
    // magnification (visual continuity across the transit). A VIRGIN
    // pin (nothing ever resolved this session) renders the mid at its
    // own native size: the most zoom the data truthfully supports right
    // now, flagged soft; the sharp landing then resolves the real 1:1
    // (QE D2: the old None-guard left FIT showing for 11 debug-seconds
    // with a usable mid in hand).
    let soft_factor = if factor.is_finite() {
        Some(factor)
    } else {
        st.last_resolved_factor
    };
    match (sharp, soft) {
        (Some(img), _) if overlay => {
            let size = img.size();
            let sf = win.window().scale_factor();
            let (nw, nh) = (size.width as f32 / sf, size.height as f32 / sf);
            let (vw, vh) = (win.get_grid_width(), win.get_loupe_area_h());
            let s = fastcull_core::zoompan::fit_scale(vw, vh, nw, nh);
            // Texture present => max_factor was known => factor is finite.
            let (ew, eh) = (nw * s * factor, nh * s * factor);
            let ox = fastcull_core::zoompan::offset_centering(vw, ew, st.pan_center.0);
            let oy = fastcull_core::zoompan::offset_centering(vh, eh, st.pan_center.1);
            win.set_loupe_w(ew);
            win.set_loupe_h(eh);
            win.set_loupe_image(img);
            win.set_loupe_vx(ox);
            win.set_loupe_vy(oy);
            // Trace on any offset, visibility or CURSOR change (QE: silent
            // same-size persistence made cross-image forensics blind).
            if st.last_pan_write != Some((ox, oy))
                || !win.get_one2one()
                || win.get_loupe_soft()
                || st.last_overlay_cursor != Some(cursor)
            {
                trace_mark(&format!(
                    "loupe idx {cursor} factor {factor:.3} extent {ew:.0}x{eh:.0} \
                     center {:.3},{:.3} off {ox:.0},{oy:.0}",
                    st.pan_center.0, st.pan_center.1
                ));
            }
            st.last_resolved_factor = Some(factor);
            st.last_pan_write = Some((ox, oy));
            st.last_overlay_cursor = Some(cursor);
            st.overlay_hold = None;
            st.last_soft_rung = None;
            win.set_loupe_soft(false);
            win.set_one2one(true);
        }
        (None, Some(img)) if overlay => {
            // SOFT transit render: the mid rung — or, below it, the grid
            // thumb (issue #46) — upscaled to the carried factor. Same
            // extent math for both — only the aspect matters at a given
            // factor (dims x fit_scale = the fit extent regardless of
            // rung resolution).
            let size = img.size();
            let sf = win.window().scale_factor();
            let (mw, mh) = (size.width as f32 / sf, size.height as f32 / sf);
            let (vw, vh) = (win.get_grid_width(), win.get_loupe_area_h());
            let s = fastcull_core::zoompan::fit_scale(vw, vh, mw, mh);
            // Virgin pin: the mid at its native resolution (factor 1/s),
            // floored at fit.
            let f = soft_factor.unwrap_or_else(|| (1.0 / s.max(1e-6)).max(1.0));
            let (ew, eh) = (mw * s * f, mh * s * f);
            let ox = fastcull_core::zoompan::offset_centering(vw, ew, st.pan_center.0);
            let oy = fastcull_core::zoompan::offset_centering(vh, eh, st.pan_center.1);
            win.set_loupe_w(ew);
            win.set_loupe_h(eh);
            win.set_loupe_image(img);
            win.set_loupe_vx(ox);
            win.set_loupe_vy(oy);
            if st.last_soft_rung != Some((cursor, soft_is_thumb)) {
                trace_mark(&format!(
                    "loupe {} idx {cursor} factor {f:.3} extent {ew:.0}x{eh:.0}",
                    if soft_is_thumb { "thumb" } else { "soft" }
                ));
            }
            st.last_soft_rung = Some((cursor, soft_is_thumb));
            st.last_pan_write = Some((ox, oy));
            st.last_overlay_cursor = Some(cursor);
            st.overlay_hold = None;
            win.set_loupe_soft(true);
            win.set_one2one(true);
        }
        _ => {
            // Not even the cursor's own THUMB exists (cold-start edge —
            // the thumb pipeline has not served this image yet), or the
            // desire is at/below fit. Two very different situations:
            //
            // Residual HOLD (issue #46, recorded in ui-grid.md): with the
            // overlay up and the desire still above fit, keep the
            // PREVIOUS image's pixels at the carried geometry, flagged by
            // the cue pill — a fit-drop is the strobe the transit
            // contract forbids, and a black frame is retinal pumping at
            // 9pm (persona). BOUNDED, never an open-ended lie: a decode
            // FAILURE of the cursor image drops to fit immediately (the
            // strip owns the failed badge), and OVERLAY_HOLD_CAP caps a
            // wedged decode. The mark badge and status bar name the NEW
            // image during the hold; the accepted, capped cost is
            // recorded in the spec.
            let now = std::time::Instant::now();
            let failed = st.failed.contains(&cursor);
            let capped = matches!(st.overlay_hold, Some((c, since)) if c == cursor
                && now.duration_since(since) >= OVERLAY_HOLD_CAP);
            if overlay && win.get_one2one() && !failed && !capped {
                if !matches!(st.overlay_hold, Some((c, _)) if c == cursor) {
                    st.overlay_hold = Some((cursor, now));
                    trace_mark(&format!(
                        "loupe hold idx {cursor} (not even a thumb; previous pixels kept)"
                    ));
                }
                // Geometry, pixels, last_pan_write and last_overlay_cursor
                // all stay on the PREVIOUS image — the pixels still belong
                // to it, and the first rung of the new image lands in the
                // branches above.
                win.set_loupe_soft(true);
            } else {
                // Honest drop: leaving the ladder (desire at/below fit or
                // not at the loupe), a failed cursor image, or a hold that
                // outlived its cap. An overlay dropping while the desire
                // is still above fit is the M1 fit-flash unless excused by
                // failure/cap — trace it (the regression tests grep this).
                if win.get_one2one() && overlay {
                    trace_mark(&format!(
                        "loupe overlay dropped idx {cursor} ({})",
                        if failed { "decode failed" } else { "hold cap" }
                    ));
                }
                win.set_one2one(false);
                win.set_loupe_soft(false);
                st.last_pan_write = None;
                st.last_overlay_cursor = None;
                st.overlay_hold = None;
                st.last_soft_rung = None;
            }
        }
    }
    // Fit pointer surface (issue #11): active at 1 column with no zoom
    // overlay up — the wheel zooms there (browsing is keyboard-only).
    win.set_at_fit(at_loupe && !win.get_one2one() && view_len > 0);

    // Loupe state badge (issue #20): the cursor's mark, written in the
    // same refresh pass that swaps the image/cells — never a frame where
    // the badge belongs to the previous photo (the issue #6 stale class).
    let cursor_mark = match st.picks.get(cursor) {
        Some(fastcull_core::catalog::PickState::Picked) => 1,
        Some(fastcull_core::catalog::PickState::Rejected) => 2,
        _ => 0,
    };
    win.set_loupe_mark(cursor_mark);
    if at_loupe && view_len > 0 {
        if st.last_badge != Some((cursor, cursor_mark)) {
            trace_mark(&format!(
                "loupe badge idx {cursor} mark {}",
                match cursor_mark {
                    1 => "picked",
                    2 => "rejected",
                    _ => "none",
                }
            ));
            st.last_badge = Some((cursor, cursor_mark));
        }
    } else {
        // Reset on leaving the loupe so RE-ENTERING traces a fresh line
        // even on an unchanged (cursor, mark) — trace forensics must
        // show what the badge said each time the loupe came up
        // (validator m3).
        st.last_badge = None;
    }

    // Mutate the one bound VecModel in place (spec: reuse, don't recreate).
    let model = Rc::clone(&st.cells);
    let mut row = 0usize;
    for pos in range.clone() {
        let index = st.view[pos];
        let (x, y) = layout.position(pos);
        let full = if at_loupe {
            fullres_for(&st, index)
        } else {
            None
        };
        // Quality ladder per cell: loupe full-res > mid rung (large cells
        // or loupe fallback) > 320px thumb > placeholder.
        let image = full
            .as_ref()
            .or_else(|| {
                if want_mid || at_loupe {
                    st.mids.get(&index)
                } else {
                    None
                }
            })
            .or(st.images.get(&index));
        let cell = CellData {
            x,
            y,
            w: layout.cell_width,
            h: layout.cell_height,
            image: image.cloned().unwrap_or_default(),
            has_image: image.is_some(),
            failed: st.failed.contains(&index),
            label: st.labels.get(index).cloned().unwrap_or_default().into(),
            is_cursor: index == st.cursor,
            selected: st.selection.is_selected(index),
            id: index as i32,
            copied: st.copied_to.contains_key(&index),
            burst_count: st.burst_badge.get(index).copied().unwrap_or(0) as i32,
            seed: if st.synthetic { index as i32 } else { -1 },
            // At the loupe (N=1) the #20 badge pill owns state display:
            // bare cells keep the grid's 40% reject dim out of the loupe
            // (a reject may be re-judged for rescue at full brightness)
            // and stop the cell glyph double-rendering under the pill.
            pick: if at_loupe {
                0
            } else {
                match st.picks.get(index) {
                    Some(fastcull_core::catalog::PickState::Picked) => 1,
                    Some(fastcull_core::catalog::PickState::Rejected) => 2,
                    _ => 0,
                }
            },
        };
        if row < model.row_count() {
            model.set_row_data(row, cell);
        } else {
            model.push(cell);
        }
        row += 1;
    }
    while model.row_count() > row {
        model.remove(model.row_count() - 1);
    }

    win.set_virtual_height(layout.total_height);

    // Filter bar + status (M5): live counts, active chip, sort label,
    // inbox-zero empty state (persona G2).
    let counts = fastcull_core::filter::counts(&st.picks);
    win.set_counts_all(counts.all as i32);
    win.set_counts_picked(counts.picked as i32);
    win.set_counts_rejected(counts.rejected as i32);
    win.set_counts_unmarked(counts.unmarked as i32);
    win.set_filter_bar_visible(st.filter_bar_visible);
    use fastcull_core::filter::{PickFilter, SortKey};
    win.set_active_filter(
        match st.query.filter {
            PickFilter::All => "all",
            PickFilter::Picked => "picked",
            PickFilter::Rejected => "rejected",
            PickFilter::Unmarked => "unmarked",
        }
        .into(),
    );
    win.set_sort_label(
        match (st.query.sort, st.query.ascending) {
            (SortKey::CaptureTime, true) => "Capture ↑",
            (SortKey::CaptureTime, false) => "Capture ↓",
            (SortKey::Filename, true) => "Name ↑",
            (SortKey::Filename, false) => "Name ↓",
        }
        .into(),
    );
    let count = st.count();
    win.set_empty_message(if view_len == 0 && count > 0 {
        let what = match st.query.filter {
            PickFilter::All => "images",
            PickFilter::Picked => "picked",
            PickFilter::Rejected => "rejected",
            PickFilter::Unmarked => "unmarked",
        };
        format!(
            "0 {what} — ★{} picked, ✕{} rejected, {} unmarked of {}",
            counts.picked, counts.rejected, counts.unmarked, counts.all
        )
        .into()
    } else if count == 0 && !st.session_open {
        // Folderless launch (issue #5, ui-grid.md): distinct from a
        // folder that opened but contained nothing.
        "No folder open — File > Open Folder… (Ctrl+O)".into()
    } else if count == 0 {
        "No images — File > Open Folder… (Ctrl+O)".into()
    } else {
        slint::SharedString::default()
    });

    let cursor_pos = st.cursor_pos();
    let cursor_in_view = cursor_pos.is_some();
    let showing = if st.query.filter == PickFilter::All {
        String::new()
    } else {
        format!(" — showing {view_len} of {count}")
    };
    refresh_iptc_panel(win, &mut st);
    win.set_scroll_hint(
        if st.view.is_empty() {
            String::new()
        } else {
            // "795 / 1450 · 15:42" — capture time appended when sorting by
            // capture (persona: a day is navigated by light, not by frame
            // numbers); filename sorts get numbers only.
            let mut hint = format!("{} / {}", range.start + 1, st.view.len());
            if st.query.sort == fastcull_core::filter::SortKey::CaptureTime {
                if let Some(key) = st
                    .view
                    .get(range.start)
                    .and_then(|id| st.capture_keys.get(*id))
                    .and_then(|k| k.as_deref())
                {
                    // Sort key "YYYY:MM:DD HH:MM:SS.mmm" -> "HH:MM".
                    if let Some(t) = key.split(' ').nth(1) {
                        if t.len() >= 5 {
                            hint.push_str(&format!(" · {}", &t[..5]));
                        }
                    }
                }
            }
            hint
        }
        .into(),
    );
    // M7: "· burst 7/23" when the cursor sits inside a group.
    let burst_note = st
        .burst_pos
        .get(cursor)
        .copied()
        .flatten()
        .map(|(p, n)| format!(" · burst {p}/{n}"))
        .unwrap_or_default();
    // Selection size (persona MUST-HAVE alongside the wash): the wash shows
    // WHICH images the IPTC batch covers, but a selection can scroll
    // off-screen — only a number tells the whole truth. Counted in core by
    // `count_in_view` so it can never drift from `Selection::batch` (rule 5:
    // the semantics live in fastcull-core, the app only renders them). An
    // empty selection stays silent: the batch is then just the cursor, and
    // "1 selected" on every image would be noise.
    let sel_note = match st.selection.count_in_view(&st.view) {
        0 => String::new(),
        n => format!(" · {n} selected"),
    };
    // Issue #20 backstop: the status bar always spells the cursor's mark
    // in words — in the loupe "no badge" needs a textual "unmarked", and
    // the words disambiguate the glyph everywhere else.
    let mark_words = if cursor_in_view {
        match cursor_mark {
            1 => " · ★ picked",
            2 => " · ✕ rejected",
            _ => " · unmarked",
        }
    } else {
        ""
    };
    // Load progress (persona ask, issue #25): a bare "1847 thumbs loaded"
    // makes you hunt for the total to know whether to start now or get a
    // beer, and while the order is provisional the status bar is the only
    // honest place to say so — the grid looks identical either way.
    let loaded = st.thumbs_done.min(count);
    let load_note = if st.metadata_complete() {
        format!("{loaded} thumbs loaded")
    } else {
        format!("{loaded}/{count} loaded · sorting by name until loaded")
    };
    win.set_status(
        format!(
            "{} ({}/{}){}{}{}{} — {} — ★{} ✕{}{} — {} column{}",
            if cursor_in_view {
                st.labels.get(cursor).cloned().unwrap_or_default()
            } else {
                String::new()
            },
            cursor_pos.map_or(0, |p| p + 1),
            // Honest count (issue #19): an empty view reads "(0/0)" — a
            // fabricated "/1" made the whole counter untrustworthy ("a
            // counter that can invent 1 can't be trusted to report
            // 3,100" — persona).
            view_len,
            mark_words,
            showing,
            burst_note,
            sel_note,
            load_note,
            counts.picked,
            counts.rejected,
            if st.sidecar_failures > 0 {
                format!(" — ⚠{} sidecar write failures", st.sidecar_failures)
            } else {
                String::new()
            },
            layout.columns,
            if layout.columns == 1 { "" } else { "s" }
        )
        .into(),
    );
}

/// Keep a full-res texture, protecting the cursor's and capping at the
/// prefetch ring size (5 = cursor ±2): the old 3-slot FIFO let the prefetch
/// evict the focused image itself (validator HIGH finding; the user saw it
/// as back-arrow quality degradation).
///
/// Eviction is by VIEW distance from the cursor, not insertion age
/// (issue #46): age is view-order-blind — the provisional-order startup
/// window legitimately decodes filename-order neighbors, and once the
/// capture sort lands those are strangers occupying slots; age eviction
/// then discarded exactly the view neighbor the NEXT tap needed while
/// keeping a frame seven positions away (observed as an 81 ms thumb
/// blink on a warm frame). Entries no longer in the view evict first.
fn insert_fullres(st: &mut AppState, index: usize, texture: slint::Image) {
    st.fullres.retain(|(i, _)| *i != index);
    st.fullres.push((index, texture));
    let cursor = st.cursor;
    while st.fullres.len() > 5 {
        let pos_of = |id: usize| st.view.iter().position(|v| *v == id);
        let cursor_pos = pos_of(cursor);
        let victim = st
            .fullres
            .iter()
            .enumerate()
            .filter(|(_, (i, _))| *i != cursor)
            .max_by_key(|(_, (i, _))| match (cursor_pos, pos_of(*i)) {
                (Some(c), Some(p)) => p.abs_diff(c),
                _ => usize::MAX, // not in the view (or no view): first out
            })
            .map(|(slot, _)| slot)
            .unwrap_or(0);
        st.fullres.remove(victim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two warm-hit contexts DIFFER, on purpose, in exactly two ways.
    /// Pinning both so the next change to one of them is a visible edit to
    /// this table rather than a silent divergence (which is how the
    /// 2026-08-02 bug happened).
    #[test]
    fn warm_routing_differs_by_context_only_where_intended() {
        const BIG: u32 = fastcull_core::loupe::MID_RUNG_MAX_LONG + 1;
        const SMALL: u32 = 640;

        // 1. A terminal SMALL image: the announcement Wraps it (native
        // size is its ceiling, issue #8); the cursor's own rebuild treats
        // it as the top rung and Fulls it.
        assert_eq!(
            route_warm(SMALL, true, true, false, WarmCtx::Announced),
            Some(WarmJob::Wrap { terminal: true })
        );
        assert_eq!(
            route_warm(SMALL, true, true, false, WarmCtx::FocusHit),
            Some(WarmJob::Full)
        );

        // 2. A held mid: the announcement still Wraps (a fresh decode
        // supersedes what is held); the rebuild leaves it alone.
        assert_eq!(
            route_warm(SMALL, false, true, true, WarmCtx::Announced),
            Some(WarmJob::Wrap { terminal: false })
        );
        assert_eq!(
            route_warm(SMALL, false, true, true, WarmCtx::FocusHit),
            None
        );

        // Away from the loupe, an announced top rung is not cooked at all,
        // while a sub-top one still fills the mid rung for the grid.
        assert_eq!(
            route_warm(BIG, false, false, false, WarmCtx::Announced),
            None
        );
        assert_eq!(
            route_warm(SMALL, false, false, false, WarmCtx::Announced),
            Some(WarmJob::Wrap { terminal: false })
        );
        assert_eq!(
            route_warm(BIG, false, true, false, WarmCtx::Announced),
            Some(WarmJob::Full)
        );
    }
}
