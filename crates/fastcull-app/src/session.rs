//! Session lifecycle: what a folder being opened means (CLI argument,
//! File > Open Folder, the drive harness's `open:` token all land here),
//! the startup launch dispatch, templates.toml re-reads, and the remembered
//! UI preferences that survive across sessions.

use std::cell::RefCell;
use std::rc::Rc;

use fastcull_core::catalog::Session;
use fastcull_core::pipeline::{JobSpec, Pipeline};
use slint::ComponentHandle;

use crate::focus::refocus_topmost_deferred;
use crate::nav::recompute_view;
use crate::presenter::refresh;
use crate::state::AppState;
use crate::MainWindow;

/// Wire File > Open Folder (the native picker; the action itself is
/// [`open_folder_at`], shared with the drive harness).
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_open_folder(move || {
            let Some(win) = win.upgrade() else { return };
            let Some(folder) = rfd::FileDialog::new().pick_folder() else {
                return; // user cancelled
            };
            open_folder_at(&win, &state, &folder);
        });
    }
}

pub(crate) enum Launch {
    /// No arguments (desktop launcher / double-clicked binary, issue
    /// #5): open the normal window in the empty state — NEVER a usage
    /// error printed to a terminal nobody sees.
    Empty,
    Synthetic(usize),
    Folder(std::path::PathBuf),
}

/// Apply the parsed command line to the fresh state: empty window,
/// synthetic session, or a real folder.
pub(crate) fn dispatch(state: &Rc<RefCell<AppState>>, launch: Launch, start_11: bool) {
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
            st.session.labels = (0..n).map(|i| format!("SYN{i:05}.ARW")).collect();
            st.session.picks = vec![fastcull_core::catalog::PickState::Unmarked; n];
            st.session.capture_keys = vec![None; n];
            st.session.frame_meta = vec![fastcull_core::burst::FrameMeta::default(); n];
            st.bursts = crate::state::BurstIndex::new(n);
            st.session.iptc = vec![fastcull_core::iptc::IptcData::default(); n];
            st.session.synthetic = true;
            st.session.session_open = true;
            // No pipeline runs in synthetic mode, so no job ever completes:
            // without this the session is permanently "still loading" and
            // the status bar would claim "0/N loaded - sorting by name"
            // forever, on the very frames the screenshot suite captures.
            st.session.thumbs_done = n;
            recompute_view(&mut st);
        }
        Launch::Folder(path) => {
            if let Err(e) = load_folder(state, &path) {
                eprintln!("fastcull: {e}");
                std::process::exit(1);
            }
            // load_folder resets the zoom; --start-11 wants 1:1 back on.
            if start_11 {
                state.borrow_mut().loupe_view.zoom_factor = f32::INFINITY;
            }
        }
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
    st.session.pipeline = None;
    // The whole loupe view dies with the session it was looking at: the
    // engine and its receiver (stopped here, before the new ones start),
    // the desired factor and pan, and every scrap of "what did the overlay
    // last draw" bookkeeping. Replacing the struct rather than clearing
    // ten fields by hand is the point of the A2 sub-structs: three of
    // those fields (last_pan_write, last_overlay_cursor, and the grid's
    // last_view_geometry) were NOT in the old hand-written list.
    st.loupe_view = crate::state::LoupeViewState::default();
    st.session.pipeline_rx = None;
    drop(st.session.writer.take()); // flush barrier for the previous session's marks
    st.session.sidecar_errs = None;

    let count = labels.len();
    let paths: Vec<std::path::PathBuf> = jobs.iter().map(|j| j.path.clone()).collect();
    st.session.labels = labels;
    st.session.paths = paths.clone();
    st.session.picks = vec![fastcull_core::catalog::PickState::Unmarked; count];
    st.session.touched.clear();
    st.session.sidecar_failures = 0;
    st.grid.cursor = 0;
    st.grid.cursor_touched = false;
    st.textures.thumb_jpegs.clear();
    st.textures.images.clear();
    st.textures.failed.clear();
    st.session.thumbs_done = 0;
    // New session: drop queued kitchen work and orphan late completions
    // (their generation dies with the old session).
    st.kitchen.retarget();
    // Re-arm issue #25's one-shot re-sort edge for the new session. It
    // happens to re-arm anyway because every open path refreshes
    // synchronously before the pump can deliver an event, but that is an
    // implicit invariant one reordered call away from a second folder
    // silently losing its re-anchor (validator concern, 2026-07-31).
    st.grid.last_metadata_complete = false;
    st.grid.last_cursor_visible = true;
    st.session.synthetic = false;
    st.textures.fullres.clear();
    st.textures.terminal_native.clear();
    st.textures.mids.clear();
    st.textures.va = fastcull_core::viewassets::ViewAssets::default();
    st.session.capture_keys = vec![None; count];
    st.session.frame_meta = vec![fastcull_core::burst::FrameMeta::default(); count];
    st.bursts = crate::state::BurstIndex::new(count);
    st.session.iptc = vec![fastcull_core::iptc::IptcData::default(); count];
    st.session.touched_iptc.clear();
    st.iptc_panel.cache = Default::default();
    st.copy.plan = None;
    st.copy.handle = None;
    st.copy.rx = None;
    st.copy.copied_to.clear();
    st.grid.selection.reset();
    st.iptc_panel.revert = Default::default();
    st.iptc_panel.revert_ids.clear();
    st.iptc_panel.revert_label.clear();
    // templates.toml: read at session open (spec: live-reload = re-read
    // here and on panel toggle, no watcher). Errors/warnings surface in
    // the panel warning strip.
    reload_templates(&mut st);
    // A new folder starts unfiltered: a hidden active filter on a fresh
    // session would look like missing files.
    st.grid.query = fastcull_core::filter::ViewQuery::default();

    let (writer, errs) = fastcull_core::sidecar_writer::SidecarWriter::start();
    st.session.writer = Some(writer);
    st.session.sidecar_errs = Some(errs);
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
    st.session.pipeline = Some(pipeline);
    st.loupe_view.engine = Some(loupe);
    st.session.pipeline_rx = Some(rx);
    st.loupe_view.rx = Some(loupe_rx);
    st.session.session_open = true;
    recompute_view(&mut st);
    Ok(())
}

/// The Open Folder ACTION — everything the menu entry does after the native
/// dialog has produced a path: session swap via [`load_folder`], fresh grid
/// zoom, viewport at the top, error surfaced in the status bar. Shared by
/// the menu callback and the `open:PATH` drive token (issue #34) so the
/// scripted swap exercises the exact code path a real Open Folder takes —
/// a parallel test-only path would bypass the very wiring under test.
pub(crate) fn open_folder_at(
    win: &MainWindow,
    state: &Rc<RefCell<AppState>>,
    folder: &std::path::Path,
) {
    match load_folder(state, folder) {
        Ok(()) => {
            // Menu-open behaves like the CLI argument (spec): fresh
            // grid zoom, cursor at the first image.
            let mut st = state.borrow_mut();
            st.grid.zoom = 1;
            st.grid.last_grid_zoom = 1;
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

/// Re-read templates.toml (session open + panel toggle = the spec's
/// read-on-open live-reload). Parse errors and CLEAR warnings both land in
/// the panel warning strip.
pub(crate) fn reload_templates(st: &mut AppState) {
    st.session.templates.clear();
    st.session.template_warnings.clear();
    let Some(path) = fastcull_core::iptc::default_templates_path() else {
        return;
    };
    match fastcull_core::iptc::load_templates(&path) {
        Ok(load) => {
            st.session.templates = load.templates;
            st.session.template_warnings = load.entry_errors;
            st.session.template_warnings.extend(load.warnings);
        }
        Err(e) => st.session.template_warnings.push(e.to_string()),
    }
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

pub(crate) fn load_ui_prefs() -> (Option<std::path::PathBuf>, String) {
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

pub(crate) fn save_ui_prefs(dest: Option<&std::path::Path>, template: &str) {
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
