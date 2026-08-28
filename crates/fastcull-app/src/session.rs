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
use crate::state::{AppState, BurstIndex, SessionState};
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
    /// `--synthetic N`; with `--bursts` the frames carry a fixed burst
    /// pattern (see `SessionState::seed_synthetic_bursts`).
    Synthetic {
        n: usize,
        bursts: bool,
    },
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
        Launch::Synthetic { n, bursts } => {
            let mut st = state.borrow_mut();
            // Built through the same constructors a real folder uses, so
            // the per-image vectors are sized in ONE place. Not through
            // `begin_session`: nothing is being swapped away here (this is
            // the launch), and the kitchen has no work to retarget.
            st.session = SessionState::synthetic(n);
            st.bursts = BurstIndex::new(n);
            if bursts {
                st.session.seed_synthetic_bursts();
                // The grouping a real folder gets when its metadata
                // lands — through the same function, so the badge, the
                // status position and the bracket keys see one truth.
                crate::copy_bridge::recompute_bursts(&mut st);
            }
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
    let paths: Vec<std::path::PathBuf> = jobs.iter().map(|j| j.path.clone()).collect();
    // The whole session swap, in one call: every group that describes one
    // folder is replaced, the survivors (the kitchen worker, the bound
    // cell model, the zoom step, the two panel visibilities, the
    // remembered copy destination) are named in `begin_session` itself.
    // This is where the previous session's sidecar writer is dropped, and
    // dropping it flushes its pending marks — the barrier the new writer
    // below is started on the far side of.
    st.begin_session(labels, paths.clone());
    // templates.toml: read at session open (spec: live-reload = re-read
    // here and on panel toggle, no watcher). Errors/warnings surface in
    // the panel warning strip.
    reload_templates(&mut st);
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
    // Read BEFORE the swap: `load_folder` replaces the session, which
    // drops the export handle — cancelling and JOINING its worker — and
    // clears both of these. A worker that committed in that window left a
    // real file, and the report below must not call that "nothing".
    //
    // The FLAG, not the path: under an Overwrite answer the destination
    // is occupied from the moment the export starts, so "the file is
    // there" would have reported yesterday's file as this export's
    // (validator finding, 2026-08-28). The flag is shared with the
    // worker and is final once the join has happened.
    let (swap_dst, swap_landed) = {
        let st = state.borrow();
        (
            st.clip.running_dst.clone(),
            st.clip.handle.as_ref().map(|h| h.landed_flag()),
        )
    };
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
            // The Copy dialog survives a session swap on screen, and
            // everything it was showing belonged to the OLD session.
            //
            // * A clash question belongs to the session it was asked
            //   about: the menu bar stays live while the dialog is up, so
            //   a folder can be opened underneath the question — and the
            //   answer is a policy that gets REPLANNED (fileops.md rule
            //   3), which would apply "overwrite everything" to a set of
            //   picks the user never saw named.
            // * A copy that was RUNNING was cancelled by the swap (the
            //   handle is dropped, which cancels and joins), and its
            //   events died with it — the dialog would sit at "running"
            //   for ever with a Cancel button that does nothing.
            // * The plan preview's counts describe the old folder.
            if win.get_copy_visible() {
                if win.get_copy_state() == 1 {
                    win.set_copy_report(
                        "Cancelled — the folder was changed. Files that finished remain.".into(),
                    );
                    win.set_copy_state(2);
                } else {
                    win.set_copy_state(0);
                    win.set_copy_confirm("".into());
                    let mut st = state.borrow_mut();
                    crate::copy_bridge::copy_replan(win, &mut st);
                }
            }
            // The video export dialog, for exactly the same three
            // reasons — with one difference in the running case: this
            // operation produces ONE file and never commits it until it
            // has been verified, so a swap that cancels it usually leaves
            // nothing behind at all.
            //
            // USUALLY, not always, and the message says which (validator
            // finding 2026-08-28): the swap cancels by DROPPING the
            // handle, and a worker that had already passed its last
            // cancel check finishes and commits. Its report dies with the
            // receiver, so the only honest way to know is to look at the
            // destination — which is why the running destination is kept.
            if win.get_clip_visible() {
                if win.get_clip_state() == 1 {
                    let landed =
                        swap_landed.is_some_and(|f| f.load(std::sync::atomic::Ordering::Relaxed));
                    let name = swap_dst.as_deref().map(crate::clip_bridge::file_name_of);
                    win.set_clip_report(
                        crate::clip_bridge::swap_report(landed, name.as_deref()).into(),
                    );
                    win.set_clip_state(2);
                } else {
                    win.set_clip_state(0);
                    win.set_clip_confirm("".into());
                    let mut st = state.borrow_mut();
                    crate::clip_bridge::clip_replan(win, &mut st);
                }
            }
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

/// The remembered preferences as they are on disk, or an empty table.
///
/// Every writer below goes through this and then [`write_ui_prefs`], so a
/// preference one dialog never touches survives the other dialog saving
/// (the video export's `clip_dest` used to be erased by any Copy Picks
/// save, because that path rebuilt the whole file from two keys).
fn read_ui_prefs() -> toml::Table {
    ui_prefs_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|c| c.parse().ok())
        .unwrap_or_default()
}

fn write_ui_prefs(table: &toml::Table) {
    let Some(path) = ui_prefs_path() else { return };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, toml::to_string_pretty(table).unwrap_or_default()).ok();
}

fn stored_path(table: &toml::Table, key: &str) -> Option<std::path::PathBuf> {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
}

pub(crate) fn load_ui_prefs() -> (Option<std::path::PathBuf>, String) {
    let table = read_ui_prefs();
    let template = table
        .get("copy_template")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    (stored_path(&table, "copy_dest"), template)
}

/// Where "Export Frames as Video" should land, remembered across
/// sessions — and SEEDED from the Copy Picks destination until a video
/// folder has actually been chosen (video-export.md, persona decision
/// 2026-08-27). Once chosen it is remembered on its own and the two
/// never move together again.
pub(crate) fn load_clip_dest() -> Option<std::path::PathBuf> {
    let table = read_ui_prefs();
    stored_path(&table, "clip_dest").or_else(|| stored_path(&table, "copy_dest"))
}

pub(crate) fn save_clip_dest(dest: &std::path::Path) {
    let mut table = read_ui_prefs();
    table.insert(
        "clip_dest".into(),
        toml::Value::String(dest.to_string_lossy().into_owned()),
    );
    write_ui_prefs(&table);
}

pub(crate) fn save_ui_prefs(dest: Option<&std::path::Path>, template: &str) {
    // Read-modify-write, not rebuild: the file also holds the video
    // export's own destination, and rebuilding it from two keys erased
    // that every time the user picked a Copy Picks folder.
    let mut table = read_ui_prefs();
    // Persist the last NON-EMPTY template (gate N1: the field now opens
    // empty by design, so a template-less copy — or just picking a
    // destination — must not erase yesterday's remembered template).
    let template = if template.trim().is_empty() {
        load_ui_prefs().1
    } else {
        template.to_string()
    };
    if let Some(d) = dest {
        table.insert(
            "copy_dest".into(),
            toml::Value::String(d.to_string_lossy().into_owned()),
        );
    }
    table.insert("copy_template".into(), toml::Value::String(template));
    write_ui_prefs(&table);
}
