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

use fastcull_core::catalog::Session;
use fastcull_core::grid::{self, GridLayout, Nav};
use fastcull_core::pipeline::{JobSpec, Pipeline, SessionEvent};
use slint::{ComponentHandle, Model, VecModel};

slint::include_modules!();

/// Rows kept alive around the viewport in the windowed model.
const MARGIN_ROWS: usize = 1;

/// UI-thread decode budget per refresh (~0.5 ms per 320 px thumb): bounds the
/// stall of a Home/End jump into a fully-thumbed region; the remainder is
/// decoded on follow-up refreshes (deviation from the never-decode-on-UI
/// rule recorded in ui-grid.md — slint::Image itself is not Send).
const DECODES_PER_REFRESH: usize = 32;

/// Full-res→mid adoptions per refresh (~35 ms each): bounds the stall when
/// leaving 1:1 after a burst walk (validator finding); leftovers follow up.
const ADOPTS_PER_REFRESH: usize = 2;

/// Belt-and-braces cap on mid textures (~5 MB each) beyond the prune-to-
/// visible-window bound (recorded decision: 4K + 6 columns worst case).
const MIDS_CAP: usize = 64;

/// Last-set IPTC panel model contents (field rows, keyword chips,
/// template names): models rebuild ONLY when these change.
#[derive(Default, PartialEq)]
struct PanelCache {
    rows: Vec<(String, String, bool)>,
    chips: Vec<(String, String)>,
    names: Vec<String>,
}

struct AppState {
    labels: Vec<String>,
    /// RAW paths for real sessions (empty for --synthetic).
    paths: Vec<std::path::PathBuf>,
    /// Pick state per image (mirrors sidecars; synthetic = in-memory only).
    picks: Vec<fastcull_core::catalog::PickState>,
    /// Images whose pick the user changed this session: sidecar-at-open
    /// events must not overwrite fresh user intent.
    touched: HashSet<usize>,
    writer: Option<fastcull_core::sidecar_writer::SidecarWriter>,
    /// Failed sidecar writes this session (surfaced in the status bar).
    sidecar_failures: usize,
    zoom: usize,
    cursor: usize,
    /// Encoded thumbs by index (30–60 KB each); decoded lazily per window,
    /// bytes dropped after decode (the SQLite cache keeps the encoded copy).
    thumb_jpegs: HashMap<usize, Vec<u8>>,
    /// Decoded images, kept for the session (spec: thumbs are cheap).
    images: HashMap<usize, slint::Image>,
    failed: HashSet<usize>,
    pipeline: Option<Pipeline>,
    thumbs_done: usize,
    /// True for --synthetic sessions: cells get distinct placeholder hues;
    /// real folders use the spec's neutral gray.
    synthetic: bool,
    /// False until a session exists (folder opened or --synthetic). The
    /// folderless launch (issue #5) shows "No folder open" — a different
    /// message from "folder opened but it has no images".
    session_open: bool,
    /// The one VecModel the window binds; refresh mutates it in place.
    cells: Rc<VecModel<CellData>>,
    /// Full-res loupe assets (real sessions only).
    loupe: Option<fastcull_core::loupe::LoupeEngine>,
    /// Images whose best rung is mid-class-or-smaller but TERMINAL (the
    /// file's native size — bare JPEGs, issue #8): their small texture
    /// counts as the top rung for the zoom ceiling.
    terminal_native: HashSet<usize>,
    /// Grid-area geometry (grid_width, viewport_h) at the last refresh:
    /// a change means RELAYOUT (panel toggle, window resize), not user
    /// scrolling — the loupe follow-scroll claim must not fire (issue
    /// #16: marks landed on a photo the user already left).
    last_view_geometry: Option<(f32, f32)>,
    /// UI-side textures for the focused image ± neighbors: sized to the
    /// prefetch ring (5) and cursor-protected on eviction (see
    /// insert_fullres); the core LRU holds the pixel data for rebuilds.
    fullres: Vec<(usize, slint::Image)>,
    /// DESIRED loupe zoom factor relative to fit (ui-grid.md zoom ladder):
    /// 1.0 = fit, `f32::INFINITY` = 1:1 wanted before the full-res texture
    /// (and thus the real ceiling) is known. Clamped to the 1:1 ceiling at
    /// render time; the overlay shows only when the clamped factor > 1.
    zoom_factor: f32,
    /// Pan anchor as a fractional image coordinate (0..1). Persists across
    /// image navigation (contract: lock 1:1 on the eye, arrow through the
    /// burst); resets to center when returning to fit.
    pan_center: (f32, f32),
    /// The loupe offsets we last WROTE to the Flickable: when the read-back
    /// differs, the user dragged, and the drag wins over `pan_center` (a
    /// mid-drag engine refresh must not yank the view back). None while the
    /// overlay is hidden.
    last_pan_write: Option<(f32, f32)>,
    /// Double-click proximity bookkeeping (issue #11, spec: the second
    /// press must land near the first or it is two independent clicks).
    /// Image fracs of the last two loupe-surface clicks.
    loupe_click_prev: Option<(f32, f32)>,
    loupe_click_last: Option<(f32, f32)>,
    /// Which image the overlay last showed (trace bookkeeping only).
    last_overlay_cursor: Option<usize>,
    /// False until the user first moves the cursor or marks (issue #4):
    /// while untouched, the cursor tracks the view's FIRST image through
    /// the progressive metadata re-sorts, so a folder never opens with
    /// the cursor stranded mid-grid (name order vs capture order).
    cursor_touched: bool,
    /// Grid zoom to return to when leaving the loupe with G/Esc.
    last_grid_zoom: usize,
    /// Mid-rung textures (1616x1080, ~5 MB each) for intermediate zooms
    /// whose cells outgrow the 320 px thumb; pruned to the visible window.
    mids: HashMap<usize, slint::Image>,
    /// Bookkeeping for `mids` (core-side, tested by tests/zoom_walk.rs):
    /// which rung each held texture is, and what must be adopted from the
    /// engine cache when no event will fire.
    va: fastcull_core::viewassets::ViewAssets,
    /// M5 filter/sort state: the grid binds to `view` (image ids passing the
    /// filter, in sort order); `cursor` remains an IMAGE id throughout.
    query: fastcull_core::filter::ViewQuery,
    view: Vec<usize>,
    /// EXIF capture sort keys, filled by MetadataReady events (None until
    /// metadata loads; keyless images sort after keyed ones by name).
    capture_keys: Vec<Option<String>>,
    /// Burst inputs per image (M7), from MetadataReady summaries.
    frame_meta: Vec<fastcull_core::burst::FrameMeta>,
    /// Burst outputs by IMAGE id: group membership, badge count on the
    /// group's first frame (0 = no badge), and "7/23" position. Rebuilt
    /// at most once per pump tick while metadata streams (burst_dirty).
    burst_of: Vec<Option<usize>>,
    burst_badge: Vec<usize>,
    burst_pos: Vec<Option<(usize, usize)>>,
    burst_dirty: bool,
    /// Per-image IPTC state (M5 panel): seeded from sidecars at open,
    /// edited by the panel, persisted via SidecarWriter::iptc.
    iptc: Vec<fastcull_core::iptc::IptcData>,
    /// Images whose IPTC the user edited this session: a stale sidecar
    /// read racing the debounced write must not revert fresh intent
    /// (same guard as `touched` for picks — gate finding).
    touched_iptc: HashSet<usize>,
    /// Last-set panel model contents: the models are ONLY rebuilt when
    /// these differ (gate finding: rebuilding on every engine event tore
    /// down the field editors mid-typing).
    panel_cache: PanelCache,
    /// Multi-selection (Shift+arrows, Ctrl+A; batch = selection in view
    /// order or the cursor — core model, tested).
    selection: fastcull_core::selection::Selection,
    /// ONE shared single-level revert slot (user decision): armed by every
    /// batch mutation from the panel; the ids the snapshots belong to ride
    /// alongside so revert lands on the right images even after re-sorts.
    revert: fastcull_core::iptc::RevertSlot,
    revert_ids: Vec<usize>,
    revert_label: String,
    /// Templates + load warnings (templates.toml, read at session open —
    /// live-reload is read-on-open per spec).
    templates: Vec<fastcull_core::iptc::IptcTemplate>,
    template_warnings: Vec<String>,
    iptc_visible: bool,
    filter_bar_visible: bool,
    /// M6 Copy Picks: the previewed plan (rebuilt by replan), the running
    /// worker, and which ids were copied WHERE this session (the re-run
    /// skip default + the copied badge).
    copy_plan: Option<fastcull_core::fileops::CopyPlan>,
    copy_dest: Option<std::path::PathBuf>,
    copy_handle: Option<fastcull_core::fileops::CopyHandle>,
    copy_rx: Option<std::sync::mpsc::Receiver<fastcull_core::fileops::CopyEvent>>,
    copied_to: HashMap<usize, std::path::PathBuf>,
    /// Engine event receivers live in state so File > Open Folder can swap
    /// the whole session without restarting the event pump.
    pipeline_rx: Option<std::sync::mpsc::Receiver<SessionEvent>>,
    loupe_rx: Option<std::sync::mpsc::Receiver<fastcull_core::loupe::LoupeEvent>>,
    sidecar_errs: Option<std::sync::mpsc::Receiver<fastcull_core::sidecar_writer::WriteFailure>>,
}

/// FASTCULL_TRACE=1: log UI-thread stalls to stderr (any handle_nav /
/// refresh phase over the trace threshold). Debug facility for hang
/// reports — zero cost when off.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FASTCULL_TRACE").is_some())
}

fn trace_clock() -> u128 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis()
}

fn trace_slow(label: &str, t0: Option<std::time::Instant>) {
    if let Some(t0) = t0 {
        let ms = t0.elapsed().as_millis();
        if ms > 20 {
            eprintln!("fastcull-trace: [{}] {label} took {ms} ms", trace_clock());
        }
    }
}

fn trace_mark(label: &str) {
    if trace_enabled() {
        eprintln!("fastcull-trace: [{}] {label}", trace_clock());
    }
}

impl AppState {
    fn count(&self) -> usize {
        self.labels.len()
    }

    /// The cursor's position in the current view (None = cursor image is
    /// filtered out or the view is empty).
    fn cursor_pos(&self) -> Option<usize> {
        self.view.iter().position(|id| *id == self.cursor)
    }
}

fn recompute_view(st: &mut AppState) {
    st.view = fastcull_core::filter::view(&st.picks, &st.labels, &st.capture_keys, &st.query);
}

/// Recompute the view AND re-apply the cursor rules. Every membership
/// change — a filter switch, but also pump-driven ones (sidecar picks
/// landing under an active filter, progressive capture keys) — must leave
/// the cursor on a view member (nearest survivor), and an emptied view has
/// no loupe to be in (persona G2). Validator finding: the pump previously
/// recomputed membership alone, leaving a cursor no cell owned.
fn recompute_view_keep_cursor(st: &mut AppState) {
    let old_view = std::mem::take(&mut st.view);
    let old_cursor = old_view.contains(&st.cursor).then_some(st.cursor);
    recompute_view(st);
    if !st.cursor_touched {
        // Issue #4: before the first user interaction the cursor is "the
        // first image", not a pinned id — capture keys stream in and
        // re-sort the view under it.
        if let Some(first) = st.view.first() {
            st.cursor = *first;
        }
    } else if let Some(id) =
        fastcull_core::filter::cursor_after_filter_change(&old_view, old_cursor, &st.view)
    {
        st.cursor = id;
    }
    let loupe_step = grid::ZOOM_COLUMNS.len() - 1;
    if st.view.is_empty() && st.zoom == loupe_step {
        st.zoom_factor = 1.0;
        st.pan_center = (0.5, 0.5);
        st.zoom = st.last_grid_zoom.min(loupe_step - 1);
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
    st.synthetic = false;
    st.fullres.clear();
    st.terminal_native.clear();
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

fn main() {
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
        last_view_geometry: None,
        fullres: Vec::new(),
        zoom_factor: if start_11 { f32::INFINITY } else { 1.0 },
        pan_center: (0.5, 0.5),
        last_pan_write: None,
        loupe_click_prev: None,
        loupe_click_last: None,
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
                recompute_view_keep_cursor(&mut st);
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
            match load_folder(&state, &folder) {
                Ok(()) => {
                    // Menu-open behaves like the CLI argument (spec): fresh
                    // grid zoom, cursor at the first image.
                    let mut st = state.borrow_mut();
                    st.zoom = 1;
                    st.last_grid_zoom = 1;
                    drop(st);
                    win.set_vp_y(0.0);
                    refresh(&win, &state);
                }
                Err(e) => {
                    eprintln!("fastcull: {e}");
                    win.set_status(format!("Open folder failed: {e}").into());
                }
            }
        });
    }
    window.on_quit(|| {
        slint::quit_event_loop().ok();
    });
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
                        IPTC_FIELD_LABELS.get(i as usize).unwrap_or(&"field"),
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
                        IPTC_FIELD_LABELS.get(i as usize).unwrap_or(&"field"),
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
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_copy_open(move || {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                if st.copy_handle.is_some() {
                    // A copy is running: just re-show the dialog.
                    win.set_copy_visible(true);
                    return;
                }
                // THE BARRIER, part 1 (gate HIGH finding: planning before
                // flushing froze `sidecar exists?` answers from BEFORE
                // the debounced write landed — a fresh first-ever pick
                // shipped its RAW without the sidecar while reporting
                // verified). Flush here so the PREVIEW is truthful;
                // copy_start flushes AND replans again.
                if let Some(writer) = &st.writer {
                    writer.flush();
                }
                let (dest, template) = load_ui_prefs();
                if st.copy_dest.is_none() {
                    st.copy_dest = dest;
                }
                // The remembered template is OFFERED, never pre-applied
                // (fileops.md "never silently pre-applied"; gate finding).
                win.set_copy_last_template(template.into());
                win.set_copy_template("".into());
                win.set_copy_dest(
                    st.copy_dest
                        .as_ref()
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default()
                        .into(),
                );
                win.set_copy_state(0);
                win.set_copy_report("".into());
                win.set_copy_visible(true);
                copy_replan(&win, &mut st);
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_copy_pick_dest(move || {
            let Some(win) = win.upgrade() else { return };
            // Blocking rfd picker (same recorded limitation as Open
            // Folder); the native dialog allows creating a folder.
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                let mut st = state.borrow_mut();
                st.copy_dest = Some(dir.clone());
                save_ui_prefs(Some(&dir), win.get_copy_template().as_str());
                win.set_copy_dest(dir.to_string_lossy().into_owned().into());
                copy_replan(&win, &mut st);
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_copy_replan(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            save_ui_prefs(st.copy_dest.as_deref(), win.get_copy_template().as_str());
            copy_replan(&win, &mut st);
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_copy_start(move || {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            // THE BARRIER, part 2: flush FIRST, then rebuild the plan
            // fresh so sidecar existence, refresh mtimes and free space
            // are decided AFTER every pending write landed (gate HIGH
            // finding — a frozen at-open plan is never executed).
            if let Some(writer) = &st.writer {
                writer.flush();
            }
            copy_replan(&win, &mut st);
            let Some(plan) = st.copy_plan.take() else {
                return; // replan surfaced an error; the dialog shows it
            };
            let (handle, rx) = fastcull_core::fileops::execute(plan);
            st.copy_handle = Some(handle);
            st.copy_rx = Some(rx);
            win.set_copy_state(1);
            win.set_copy_progress("Starting…".into());
        });
    }
    {
        let state = Rc::clone(&state);
        window.on_copy_cancel(move || {
            let st = state.borrow();
            if let Some(handle) = &st.copy_handle {
                handle.cancel();
            }
        });
    }
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_copy_close(move || {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                if st.copy_handle.is_none() {
                    // keep plan state tidy between opens
                    st.copy_plan = None;
                }
            }
            win.set_copy_visible(false);
            win.invoke_focus_grid();
        });
    }
    {
        let state = Rc::clone(&state);
        window.on_copy_open_dest_folder(move || {
            let st = state.borrow();
            if let Some(dest) = &st.copy_dest {
                #[cfg(target_os = "windows")]
                let cmd = "explorer";
                #[cfg(not(target_os = "windows"))]
                let cmd = "xdg-open";
                std::process::Command::new(cmd).arg(dest).spawn().ok();
            }
        });
    }
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
                capture_pan(&win, &mut st);
                // Clicking to pixel-peep is as much a claim as any other
                // click (validator: a capture-key re-sort could otherwise
                // swap the image under an active 1:1 inspection).
                st.cursor_touched = true;
                // Double-click proximity trace (spec): remember the last
                // two loupe clicks.
                st.loupe_click_prev = st.loupe_click_last;
                st.loupe_click_last = Some((fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0)));
                st.pan_center = (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0));
            }
            refresh(&win, &state);
        });
    }
    {
        // Pointer wheel (issue #11): one notch-equivalent = one ladder
        // stop, anchored under the pointer. Arrives from the fit surface
        // or the zoom overlay; the machine decides from the actual state.
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_pointer_wheel(move |up, ctrl, x, y| {
            let Some(win) = win.upgrade() else { return };
            let action = {
                let mut st = state.borrow_mut();
                capture_pan(&win, &mut st); // fold a pending drag first
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
        // Fit-surface clicks only feed the double-click proximity trace
        // (a click at fit does nothing — spec Q5).
        let state = Rc::clone(&state);
        let win = window.as_weak();
        window.on_fit_clicked(move |x, y| {
            let Some(win) = win.upgrade() else { return };
            let mut st = state.borrow_mut();
            // A fit click claims the cursor like every other image click
            // (untouched-cursor rule — validator: a capture-key re-sort
            // must not swap the image under the user).
            st.cursor_touched = true;
            let (_, geo) = machine_ctx(&win, &st);
            let frac = fastcull_core::pointer::view_to_frac(&geo, 1.0, (x, y));
            st.loupe_click_prev = st.loupe_click_last;
            st.loupe_click_last = Some(frac);
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

    // Engine event pump: drain pending events every 33 ms; refresh once if
    // anything relevant arrived. Receivers live in AppState so File > Open
    // Folder can swap the session under a running pump.
    let timer = slint::Timer::default();
    {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(33),
            move || {
                let mut dirty = false;
                {
                    let mut st = state.borrow_mut();
                    let events: Vec<SessionEvent> = st
                        .pipeline_rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in events {
                        match event {
                            SessionEvent::ThumbReady {
                                index, thumb_jpeg, ..
                            } => {
                                st.thumb_jpegs.insert(index, thumb_jpeg);
                                st.thumbs_done += 1;
                                dirty = true;
                            }
                            SessionEvent::Failed { index, .. } => {
                                st.failed.insert(index);
                                st.thumbs_done += 1;
                                dirty = true;
                            }
                            SessionEvent::MetadataReady { index, exif, .. } => {
                                // Capture-time sort keys arrive progressively;
                                // the view re-sorts as they land (spec:
                                // keyless images sort after keyed ones).
                                if let Some(slot) = st.capture_keys.get_mut(index) {
                                    *slot = exif.sort_key();
                                    dirty = true;
                                }
                                if let Some(slot) = st.frame_meta.get_mut(index) {
                                    *slot = fastcull_core::burst::FrameMeta::from_summary(&exif);
                                    st.burst_dirty = true;
                                }
                            }
                            SessionEvent::Sidecar { index, pick, iptc } => {
                                // Picks from a previous session/tool — never
                                // override what the user changed just now.
                                if !st.touched.contains(&index) {
                                    if let Some(slot) = st.picks.get_mut(index) {
                                        *slot = pick;
                                        dirty = true;
                                    }
                                }
                                // Same guard as picks: a sidecar read that
                                // raced the debounced writer must not
                                // revert a fresh panel edit (gate finding).
                                if !st.touched_iptc.contains(&index) {
                                    if let Some(slot) = st.iptc.get_mut(index) {
                                        *slot = *iptc;
                                    }
                                }
                            }
                        }
                    }
                    let at_loupe = st.zoom == grid::ZOOM_COLUMNS.len() - 1;
                    let failures: Vec<_> = st
                        .sidecar_errs
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for _failure in failures {
                        // Count for the status bar only: the writer's drain
                        // already eprintlns the path+reason (QE finding —
                        // logging here too printed every failure twice).
                        st.sidecar_failures += 1;
                        dirty = true;
                    }
                    // Copy Picks progress/report (M6).
                    let copy_events: Vec<_> = st
                        .copy_rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in copy_events {
                        use fastcull_core::fileops::CopyEvent;
                        match event {
                            CopyEvent::File { index, total, name } => {
                                if let Some(win) = win.upgrade() {
                                    win.set_copy_progress(
                                        format!("{index} / {total} — {name}").into(),
                                    );
                                }
                            }
                            CopyEvent::Failed { .. } => {} // in the report
                            CopyEvent::Finished(report) => {
                                if let Some(dest) = st.copy_dest.clone() {
                                    for id in &report.copied_ids {
                                        st.copied_to.insert(*id, dest.clone());
                                    }
                                }
                                st.copy_handle = None;
                                st.copy_rx = None;
                                if let Some(win) = win.upgrade() {
                                    // The green light to format a card
                                    // appears ONLY when this run actually
                                    // verified copies (gate finding: an
                                    // all-skipped run verified nothing).
                                    let verified_line = report.copied > 0
                                        && report.all_verified
                                        && report.failed.is_empty()
                                        && !report.cancelled;
                                    let mut lines = vec![if report.copied == 0 {
                                        "Nothing needed copying".to_string()
                                    } else {
                                        format!(
                                            "{} copied{}",
                                            report.copied,
                                            if verified_line {
                                                ", all checksums verified"
                                            } else {
                                                ""
                                            }
                                        )
                                    }];
                                    if report.skipped > 0 {
                                        lines.push(format!("{} skipped", report.skipped));
                                    }
                                    if report.refreshed > 0 {
                                        lines.push(format!(
                                            "{} sidecars refreshed",
                                            report.refreshed
                                        ));
                                    }
                                    if report.cancelled {
                                        lines.push("cancelled — finished files remain".into());
                                    }
                                    for (name, reason) in &report.failed {
                                        lines.push(format!("FAILED {name}: {reason}"));
                                    }
                                    win.set_copy_report(lines.join("\n").into());
                                    win.set_copy_state(2);
                                }
                                dirty = true; // copied badges
                            }
                        }
                    }
                    let loupe_events: Vec<_> = st
                        .loupe_rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in loupe_events {
                        match event {
                            fastcull_core::loupe::LoupeEvent::Ready {
                                index,
                                image,
                                terminal,
                            } => {
                                let long = image.width.max(image.height);
                                trace_mark(&format!("loupe ready idx {index} long {long}"));
                                if long <= fastcull_core::loupe::MID_RUNG_MAX_LONG {
                                    // Mid rung (~5 MB copy): grid-cell quality
                                    // for intermediate zooms; cheap, always keep.
                                    if terminal {
                                        // The file's BEST rung (bare JPEG,
                                        // issue #8): this IS native — keep it
                                        // as the top rung so the zoom ceiling
                                        // is knowable (validator MAJOR: small
                                        // JPEGs dead-ended the zoom path).
                                        st.terminal_native.insert(index);
                                        let texture = fullres_texture(&image);
                                        insert_fullres(&mut st, index, texture);
                                    }
                                    if st.mids.len() < MIDS_CAP || st.mids.contains_key(&index) {
                                        st.mids.insert(index, fullres_texture(&image));
                                        st.va.note_held(index, long);
                                    }
                                } else if at_loupe {
                                    // Full rung: 150 MB copy only while the
                                    // loupe can use it; core LRU keeps pixels.
                                    let texture = fullres_texture(&image);
                                    insert_fullres(&mut st, index, texture);
                                }
                                dirty = true;
                            }
                            fastcull_core::loupe::LoupeEvent::Failed { index, .. } => {
                                st.failed.insert(index); // badge; core won't retry
                                dirty = true;
                            }
                        }
                    }
                    if dirty {
                        // Picks/keys may have changed membership or order.
                        recompute_view_keep_cursor(&mut st);
                    }
                    if st.burst_dirty {
                        st.burst_dirty = false;
                        recompute_bursts(&mut st);
                    }
                }
                if dirty {
                    if let Some(win) = win.upgrade() {
                        refresh(&win, &state);
                    }
                }
            },
        );
    }

    refresh(&window, &state);

    // FASTCULL_DRIVE="6000:one2one;12000:grid;15000:quit": timed nav
    // injection for headless hang debugging (companion to FASTCULL_TRACE —
    // no display-automation tooling needed on Wayland).
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
            let state = Rc::clone(&state);
            slint::Timer::single_shot(std::time::Duration::from_millis(ms), move || {
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
                handle_nav(&win, &state, &key);
            });
        }
    }

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
    // Screenshot mode must NEVER exit 0 without its file: if anything ends
    // the event loop before the shutter fires (window closed under load —
    // validator-observed flake), the harness would otherwise see a clean
    // exit and fail later with a bare file-not-found.
    let screenshot_requested = screenshot.is_some();
    let shot_written = Rc::new(std::cell::Cell::new(false));
    if let Some(out) = screenshot {
        let win = window.as_weak();
        let state_rc = Rc::clone(&state);
        let shot_written = Rc::clone(&shot_written);
        let started = std::time::Instant::now();
        shot_timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(250),
            move || {
                let Some(win) = win.upgrade() else { return };
                let elapsed = started.elapsed();
                let one2one_ready = {
                    let st = state_rc.borrow();
                    st.zoom_factor <= 1.0
                        || st.fullres.iter().any(|(i, img)| {
                            *i == st.cursor
                                && (img.size().width.max(img.size().height)
                                    > fastcull_core::loupe::MID_RUNG_MAX_LONG
                                    // A terminal small texture IS the top
                                    // rung (bare JPEGs, issue #8 — QE D2:
                                    // the 60s refusal hit every small-JPEG
                                    // --start-11 run).
                                    || st.terminal_native.contains(&st.cursor))
                        })
                };
                if !one2one_ready && elapsed > std::time::Duration::from_secs(60) {
                    eprintln!(
                        "screenshot: full-res texture never adopted for the 1:1 \
                         frame within 60 s — refusing to capture the wrong state"
                    );
                    slint::quit_event_loop().ok();
                    std::process::exit(1);
                }
                if elapsed < std::time::Duration::from_millis(1500) || !one2one_ready {
                    return;
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
    window.run().expect("running event loop");
    if screenshot_requested && !shot_written.get() {
        eprintln!(
            "screenshot: event loop ended before the snapshot was captured \
             (window closed early?) — failing instead of exiting clean"
        );
        std::process::exit(2);
    }
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
    let writer = state.borrow_mut().writer.take();
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

/// Re-anchor the scroll so the cursor is visible, then refresh. Order per
/// the cursor contract: virtual height BEFORE viewport-y.
fn reveal_cursor(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (layout, viewport_h, scroll_y) = current_geometry(win, state);
    let pos = state.borrow().cursor_pos().unwrap_or(0);
    let new_scroll = layout.scroll_to_reveal(pos, scroll_y, viewport_h);
    win.set_virtual_height(layout.total_height);
    win.set_vp_y(-new_scroll);
    refresh(win, state);
}

/// Remembered UI preferences (fileops.md: destination and rename template
/// survive across sessions). Tiny TOML in the fastcull config dir.
fn ui_prefs_path() -> Option<std::path::PathBuf> {
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

/// Picked images in SESSION SORT ORDER (fileops.md: scope is "everything
/// with a star", filter-independent; `{seq}` follows the session sort).
fn plan_sources(st: &AppState) -> Vec<fastcull_core::fileops::PlanSource> {
    let all_query = fastcull_core::filter::ViewQuery {
        filter: fastcull_core::filter::PickFilter::All,
        ..st.query
    };
    let ordered = fastcull_core::filter::view(&st.picks, &st.labels, &st.capture_keys, &all_query);
    ordered
        .into_iter()
        .filter(|id| {
            matches!(
                st.picks.get(*id),
                Some(fastcull_core::catalog::PickState::Picked)
            )
        })
        .filter_map(|id| {
            let path = st.paths.get(id)?.clone();
            let meta = std::fs::metadata(&path).ok();
            let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            let name = st.labels.get(id).cloned().unwrap_or_default();
            Some(fastcull_core::fileops::PlanSource {
                id,
                path,
                size,
                ctx: fastcull_core::iptc::ExpandContext::from_sort_key(
                    st.capture_keys.get(id).and_then(|k| k.as_deref()),
                    mtime,
                    &name,
                    None,
                ),
            })
        })
        .collect()
}

fn human_bytes(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{b} B")
    }
}

/// Rebuild the copy plan from the dialog's current inputs and publish the
/// preview properties (fileops.md dialog minimums).
fn copy_replan(win: &MainWindow, st: &mut AppState) {
    use fastcull_core::fileops::{plan, ExistsMode, PlanError};
    let sources = plan_sources(st);
    win.set_copy_error("".into());
    win.set_copy_ready(false);
    win.set_copy_preview("".into());
    win.set_copy_collisions("".into());
    win.set_copy_show_skip_toggle(false);
    st.copy_plan = None;
    if sources.is_empty() {
        win.set_copy_summary("No picked images — nothing to copy.".into());
        return;
    }
    let Some(dest) = st.copy_dest.clone() else {
        win.set_copy_summary(
            format!("{} picked images. Choose a destination.", sources.len()).into(),
        );
        return;
    };
    let template_raw = win.get_copy_template().to_string();
    let template = (!template_raw.trim().is_empty()).then_some(template_raw.as_str());
    let mode = if win.get_copy_skip_existing() {
        ExistsMode::Skip
    } else {
        ExistsMode::Rename
    };
    // Canonicalized comparison: the same destination reached via a
    // different path spelling must not lose the re-run skip default.
    let dest_canon = dest.canonicalize().unwrap_or_else(|_| dest.clone());
    let already: std::collections::HashSet<usize> = st
        .copied_to
        .iter()
        .filter(|(_, d)| d.canonicalize().unwrap_or_else(|_| (*d).clone()) == dest_canon)
        .map(|(id, _)| *id)
        .collect();
    match plan(&sources, &dest, template, mode, &already) {
        Ok(p) => {
            if template.is_some() {
                let preview: Vec<String> = p
                    .jobs
                    .iter()
                    .take(3)
                    .map(|j| {
                        j.dst_raw
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect();
                win.set_copy_preview(format!("→ {}", preview.join(", ")).into());
            }
            win.set_copy_summary(
                format!(
                    "{} picked · {} to copy · {}",
                    sources.len(),
                    human_bytes(p.total_bytes),
                    match p.free_bytes {
                        Some(free) => format!("{} free", human_bytes(free)),
                        None => "free space unknown".to_string(),
                    }
                )
                .into(),
            );
            let mut notes = Vec::new();
            if p.renamed > 0 {
                notes.push(format!("{} will be renamed (name collisions)", p.renamed));
            }
            if p.skipped > 0 {
                notes.push(format!("{} already at destination (skipped)", p.skipped));
            }
            if p.refreshed > 0 {
                notes.push(format!("{} sidecars will be refreshed", p.refreshed));
            }
            let collided = p.renamed > 0 || p.skipped > 0 || p.refreshed > 0;
            win.set_copy_collisions(notes.join(" · ").into());
            win.set_copy_show_skip_toggle(collided);
            win.set_copy_ready(true);
            st.copy_plan = Some(p);
        }
        Err(
            e @ (PlanError::InsufficientSpace { .. }
            | PlanError::DestEqualsSource
            | PlanError::DestInsideSource
            | PlanError::DestExists(_)
            | PlanError::TemplateCollision { .. }
            | PlanError::Template(_)
            | PlanError::Io(_)),
        ) => {
            win.set_copy_summary(format!("{} picked images.", sources.len()).into());
            win.set_copy_error(e.to_string().into());
        }
    }
}

/// Rebuild burst grouping (M7, burst-grouping.md): always over CAPTURE
/// order of the WHOLE session (the spec's input contract) regardless of
/// the UI's filter/sort; results are indexed by image id for the grid
/// badge, the status position, and the `[`/`]` boundary keys.
fn recompute_bursts(st: &mut AppState) {
    let capture_query = fastcull_core::filter::ViewQuery {
        filter: fastcull_core::filter::PickFilter::All,
        sort: fastcull_core::filter::SortKey::CaptureTime,
        ascending: true,
    };
    let order =
        fastcull_core::filter::view(&st.picks, &st.labels, &st.capture_keys, &capture_query);
    let frames: Vec<fastcull_core::burst::FrameMeta> = order
        .iter()
        .map(|id| st.frame_meta.get(*id).cloned().unwrap_or_default())
        .collect();
    let grouping =
        fastcull_core::burst::group(&frames, &fastcull_core::burst::BurstConfig::default());
    let n = st.labels.len();
    st.burst_of = vec![None; n];
    st.burst_badge = vec![0; n];
    st.burst_pos = vec![None; n];
    let positions = grouping.positions(); // one O(n) pass, not per-frame
    for (pos_in_order, id) in order.iter().enumerate() {
        st.burst_of[*id] = grouping.group[pos_in_order];
        // Badge goes on the group's FIRST frame (position 1) — with
        // interleaved bodies members need not be contiguous.
        if let Some((1, size)) = positions[pos_in_order] {
            st.burst_badge[*id] = size;
        }
        st.burst_pos[*id] = positions[pos_in_order];
    }
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

/// The 11 panel field rows, in display order. Index = callback contract
/// with the UI (iptc-field-committed / iptc-field-clear).
const IPTC_FIELD_LABELS: [&str; 11] = [
    "Title",
    "Description",
    "Creator",
    "Copyright",
    "Headline",
    "City",
    "Country",
    "Credit",
    "Source",
    "Job ID",
    "Location",
];

fn iptc_field_get(d: &fastcull_core::iptc::IptcData, i: usize) -> Option<&String> {
    match i {
        0 => d.title.as_ref(),
        1 => d.description.as_ref(),
        2 => d.creator.as_ref(),
        3 => d.rights.as_ref(),
        4 => d.headline.as_ref(),
        5 => d.city.as_ref(),
        6 => d.country.as_ref(),
        7 => d.credit.as_ref(),
        8 => d.source.as_ref(),
        9 => d.job_id.as_ref(),
        10 => d.location.as_ref(),
        _ => None,
    }
}

fn iptc_field_set(d: &mut fastcull_core::iptc::IptcData, i: usize, v: Option<String>) {
    match i {
        0 => d.title = v,
        1 => d.description = v,
        2 => d.creator = v,
        3 => d.rights = v,
        4 => d.headline = v,
        5 => d.city = v,
        6 => d.country = v,
        7 => d.credit = v,
        8 => d.source = v,
        9 => d.job_id = v,
        10 => d.location = v,
        _ => {}
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
    if size.width.max(size.height) <= fastcull_core::loupe::MID_RUNG_MAX_LONG
        && !st.terminal_native.contains(&st.cursor)
    {
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
    let columns = grid::ZOOM_COLUMNS[st.zoom.min(grid::ZOOM_COLUMNS.len() - 1)];
    let state = if columns > 1 {
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
    let fit_cell = (columns == 1).then(|| {
        let layout = GridLayout::new(st.zoom, vw, st.view.len());
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

/// Loupe double-click (fit or zoomed surface): route through the machine
/// after the proximity check — the second press must land near the first
/// or it was two independent clicks whose re-centers already happened
/// (spec, persona finding: eye-beak-wingtip scan clicks must not slam to
/// 1:1).
fn handle_loupe_double_click(win: &MainWindow, state: &Rc<RefCell<AppState>>, x: f32, y: f32) {
    use fastcull_core::pointer as pm;
    let action = {
        let mut st = state.borrow_mut();
        capture_pan(win, &mut st);
        let (ms, geo) = machine_ctx(win, &st);
        let factor = match ms {
            pm::ViewState::Zoomed { factor } => factor,
            _ => 1.0,
        };
        if let (Some(a), Some(b)) = (st.loupe_click_prev, st.loupe_click_last) {
            // Compare the two recorded clicks in ON-SCREEN pixels at the
            // current extents.
            let s = fastcull_core::zoompan::fit_scale(
                geo.viewport_w,
                geo.viewport_h,
                geo.native_w,
                geo.native_h,
            ) * factor;
            let d = ((a.0 - b.0) * geo.native_w * s).hypot((a.1 - b.1) * geo.native_h * s);
            if d > 12.0 {
                return; // two re-centers, not a double-click
            }
        }
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
    let loupe_step = grid::ZOOM_COLUMNS.len() - 1;
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
                if st.zoom < loupe_step {
                    st.last_grid_zoom = st.zoom;
                    st.zoom = loupe_step;
                }
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

/// Fold a drag-pan back into the fractional pan center: drags move the
/// Flickable viewport without Rust hearing about it, so every mutation
/// path reads the offsets back before acting. Programmatic writes are
/// recognized via `last_pan_write` and never misread as drags.
fn capture_pan(win: &MainWindow, st: &mut AppState) {
    if !win.get_one2one() {
        return;
    }
    // Issue #6 guard: only fold offsets back for the image we last drove
    // the overlay FOR. Mid-navigation, a readback delta is a Flickable
    // clamp/init artifact, never a user drag — the hand is on the arrow
    // key, not the mouse.
    if st.last_overlay_cursor != Some(st.cursor) {
        return;
    }
    let (vx, vy) = (win.get_loupe_vx(), win.get_loupe_vy());
    let Some((wx, wy)) = st.last_pan_write else {
        return; // overlay not yet driven by us: nothing to fold back
    };
    if (vx - wx).abs() < 0.5 && (vy - wy).abs() < 0.5 {
        return; // no drag since our last write
    }
    st.pan_center = (
        fastcull_core::zoompan::frac_at_center(win.get_grid_width(), win.get_loupe_w(), vx),
        fastcull_core::zoompan::frac_at_center(win.get_loupe_area_h(), win.get_loupe_h(), vy),
    );
    // Forensics for issue #6: a "drag" fold nobody dragged (e.g. a
    // recreated/clamped Flickable writing back through the two-way
    // binding) corrupts pan_center mid-navigation.
    trace_mark(&format!(
        "pan fold: read {vx:.0},{vy:.0} (last write {wx:.0},{wy:.0}) -> center {:.3},{:.3}",
        st.pan_center.0, st.pan_center.1
    ));
    st.last_pan_write = Some((vx, vy));
}

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
    let rows: Vec<(String, String, bool)> = (0..IPTC_FIELD_LABELS.len())
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
                IPTC_FIELD_LABELS[i].to_string(),
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
        recompute_view_keep_cursor(&mut st);
    }
    reveal_cursor(win, state);
}

fn handle_nav(win: &MainWindow, state: &Rc<RefCell<AppState>>, key: &str) {
    let t0 = trace_enabled().then(std::time::Instant::now);
    handle_nav_inner(win, state, key);
    trace_slow(&format!("handle_nav({key})"), t0);
}

fn handle_nav_inner(win: &MainWindow, state: &Rc<RefCell<AppState>>, key: &str) {
    let (layout, viewport_h, scroll_y) = current_geometry(win, state);
    let mut st = state.borrow_mut();
    // A drag since the last render moved the pan; fold it in before any
    // action reads or resets the pan center.
    capture_pan(win, &mut st);
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
        // Leaving an image invalidates the double-click proximity trace
        // (a click on the OLD image must not veto a double-click here).
        st.loupe_click_prev = None;
        st.loupe_click_last = None;
    }
    let loupe_step = grid::ZOOM_COLUMNS.len() - 1;
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
                        if st.zoom == loupe_step {
                            st.zoom_factor = 1.0;
                            st.pan_center = (0.5, 0.5);
                            st.zoom = st.last_grid_zoom.min(loupe_step - 1);
                        }
                    }
                }
            }
        }
        // One seamless zoom axis (spec): columns -> loupe fit -> x1.5
        // ladder -> 1:1 (ui-grid.md Loupe zoom ladder, 2026-07-25).
        "one2one" => {
            // Z: fit -> 1:1; zoomed (1:1 or intermediate) -> back to fit;
            // from a grid zoom: jump straight to loupe 1:1.
            if st.loupe.is_some() {
                if st.zoom < loupe_step {
                    st.last_grid_zoom = st.zoom;
                    st.zoom = loupe_step;
                    st.zoom_factor = f32::INFINITY;
                } else if st.zoom_factor > 1.0 {
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
            if st.zoom != loupe_step && st.zoom_factor <= 1.0 {
                // Already at a grid zoom: Esc/G collapses the selection
                // (the deselect gesture — gate finding).
                st.selection.clear();
            }
            st.zoom_factor = 1.0;
            st.pan_center = (0.5, 0.5);
            if st.zoom == loupe_step {
                st.zoom = st.last_grid_zoom.min(loupe_step - 1);
            }
        }
        "zoom-in" => {
            if st.zoom == loupe_step {
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
                if st.zoom + 1 == loupe_step {
                    st.last_grid_zoom = st.zoom;
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
    // Keep the cursor visible under the (possibly new) layout. Order matters
    // (spec, cursor contract): the Flickable clamps viewport-y against its
    // CURRENT viewport-height, so the new virtual height must land first or
    // the reveal gets clamped against stale bounds and the cursor scrolls
    // out of view.
    let layout = GridLayout::new(st.zoom, win.get_grid_width(), st.view.len());
    let pos = st.cursor_pos().unwrap_or(0);
    let new_scroll = layout.scroll_to_reveal(pos, scroll_y, viewport_h);
    drop(st);
    win.set_virtual_height(layout.total_height);
    win.set_vp_y(-new_scroll);
    refresh(win, state);
}

fn current_geometry(win: &MainWindow, state: &Rc<RefCell<AppState>>) -> (GridLayout, f32, f32) {
    let st = state.borrow();
    let layout = GridLayout::new(st.zoom, win.get_grid_width(), st.view.len());
    let viewport_h = win.get_grid_height();
    let scroll_y = (-win.get_vp_y()).max(0.0);
    (layout, viewport_h, scroll_y)
}

/// Rebuild the windowed model for the current viewport.
fn refresh(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let t0 = trace_enabled().then(std::time::Instant::now);
    refresh_inner(win, state);
    trace_slow("refresh", t0);
}

fn refresh_inner(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (layout, viewport_h, scroll_y) = current_geometry(win, state);
    let mut st = state.borrow_mut();
    // Relayout detection (issue #16): a geometry change since the last
    // refresh is chrome/window movement (panel toggle, resize), never
    // user scrolling.
    let geom_now = (win.get_grid_width(), viewport_h);
    let relayout = st.last_view_geometry.is_some_and(|g| g != geom_now);
    st.last_view_geometry = Some(geom_now);
    let view_len = st.view.len();
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

    // Decode encoded thumbs entering the window, bounded per refresh so a
    // page jump never stalls a frame; leftovers get a follow-up refresh.
    let to_decode: Vec<usize> = ids
        .iter()
        .copied()
        .filter(|i| st.thumb_jpegs.contains_key(i) && !st.images.contains_key(i))
        .collect();
    let leftovers = to_decode.len() > DECODES_PER_REFRESH;
    for index in to_decode.into_iter().take(DECODES_PER_REFRESH) {
        // Encoded bytes are dropped after decode: the SQLite cache keeps
        // the encoded copy, no need for a third one in RAM.
        if let Some(image) = st.thumb_jpegs.remove(&index).and_then(|b| decode_image(&b)) {
            st.images.insert(index, image);
        }
    }
    if leftovers {
        let win_weak = win.as_weak();
        let state_rc = Rc::clone(state);
        slint::Timer::single_shot(std::time::Duration::from_millis(16), move || {
            if let Some(win) = win_weak.upgrade() {
                refresh(&win, &state_rc);
            }
        });
    }

    // Loupe: at 1-column zoom the visible image IS the cursor (spec, cursor
    // contract): scrolling moves the cursor, and full-res always targets
    // what the user is looking at.
    let at_loupe = layout.columns == 1;
    if at_loupe && view_len > 0 {
        // Scroll moves the cursor ONLY when the cursor's cell left the
        // viewport: unconditionally snapping to the center row made arrow
        // keys a no-op on tall windows where >2 rows fit (validator
        // finding — move, no scroll needed, snap-back to center).
        let cur_pos = st.cursor_pos().unwrap_or(0);
        let (_, cur_top) = layout.position(cur_pos);
        let cur_visible =
            cur_top < scroll_y + viewport_h && cur_top + layout.cell_height > scroll_y;
        // Guard against pre-layout geometry (issue #4 debugging: refreshes
        // before the window lays out see a NEGATIVE viewport height, made
        // the cursor look "scrolled away", and spuriously claimed it —
        // killing the untouched-snap and leaving the final cursor racy).
        if !cur_visible && viewport_h > 0.0 {
            if relayout {
                // Geometry changed under the cursor (panel toggle, window
                // RESIZE — the user's reported bug): this is NOT
                // scrolling. Keep the cursor, move the viewport back to
                // it; a follow-up refresh renders the corrected window
                // (issue #16 — the claim below used to swap the photo).
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
            } else {
                let center_row = ((scroll_y + viewport_h * 0.5)
                    / (layout.cell_height + grid::CELL_GAP))
                    as usize;
                let claimed = center_row.min(view_len - 1);
                trace_mark(&format!(
                    "follow-scroll claim: cursor pos {cur_pos} -> {claimed}"
                ));
                st.cursor = st.view[claimed];
                st.cursor_touched = true; // scrolling the loupe IS cursor movement
            }
        }
        if let Some(loupe) = &st.loupe {
            // focus() returns the cached image on a warm hit: the rebuild
            // path for textures evicted UI-side (validator finding — going
            // backwards previously degraded to the thumb forever).
            let focus_index = st.cursor;
            // Ladder target: fit view needs the viewport in physical pixels;
            // any factor above fit demands the top rung (quality rule:
            // intermediate zooms render from full-res, never upscaled mid).
            let display_long = if st.zoom_factor > 1.0 {
                u32::MAX
            } else {
                (win.get_grid_width() * win.window().scale_factor()) as u32
            };
            let hit = loupe.focus(focus_index, display_long);
            let missing = !st.fullres.iter().any(|(i, _)| *i == focus_index);
            if let (Some(image), true) = (hit, missing) {
                let t1 = trace_enabled().then(std::time::Instant::now);
                let texture = fullres_texture(&image);
                insert_fullres(&mut st, focus_index, texture);
                trace_slow("refresh: fullres_texture copy", t1);
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
            // (the zoom-walk bug: pruned-and-revisited cells stayed thumbs).
            let adopts = stx.va.ensure(&ids, cell_phys as u32, loupe);
            let leftover = adopts.len() > ADOPTS_PER_REFRESH;
            for (index, image) in adopts.into_iter().take(ADOPTS_PER_REFRESH) {
                if stx.mids.len() >= MIDS_CAP && !stx.mids.contains_key(&index) {
                    break;
                }
                let t1 = trace_enabled().then(std::time::Instant::now);
                let (held_long, texture) = adopt_texture(&image);
                stx.va.note_held(index, held_long);
                stx.mids.insert(index, texture);
                trace_slow("refresh: mid-rung adopt/downscale", t1);
            }
            if leftover {
                let win_weak = win.as_weak();
                let state_rc = Rc::clone(state);
                slint::Timer::single_shot(std::time::Duration::from_millis(16), move || {
                    if let Some(win) = win_weak.upgrade() {
                        refresh(&win, &state_rc);
                    }
                });
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
    // the CURSOR's texture exists (a stale previous image must never pose
    // as the current one), sized as fit-extent × factor in logical pixels
    // so the capped factor means device pixels on HiDPI (validator
    // finding — sharpness judging must not upsample). Every factor above
    // fit requires the TOP rung (ui-grid.md quality rule: never upscale
    // the mid rung for a sharpness-critical view) — hold the fit view
    // until the full-res texture lands.
    capture_pan(win, &mut st); // drag since last render wins over pan_center
    let factor = clamped_factor(win, &st);
    let overlay = factor > 1.0 && at_loupe;
    match fullres_for(&st, cursor).filter(|img| {
        img.size().width.max(img.size().height) > fastcull_core::loupe::MID_RUNG_MAX_LONG
            || st.terminal_native.contains(&cursor)
    }) {
        Some(img) if overlay => {
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
                || st.last_overlay_cursor != Some(cursor)
            {
                trace_mark(&format!(
                    "loupe idx {cursor} factor {factor:.3} extent {ew:.0}x{eh:.0} \
                     center {:.3},{:.3} off {ox:.0},{oy:.0}",
                    st.pan_center.0, st.pan_center.1
                ));
            }
            st.last_pan_write = Some((ox, oy));
            st.last_overlay_cursor = Some(cursor);
            win.set_one2one(true);
        }
        _ => {
            win.set_one2one(false);
            st.last_pan_write = None;
            st.last_overlay_cursor = None;
        }
    }
    // Fit pointer surface (issue #11): active at 1 column with no zoom
    // overlay up — the wheel zooms there (browsing is keyboard-only).
    win.set_at_fit(at_loupe && !win.get_one2one() && view_len > 0);

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
            pick: match st.picks.get(index) {
                Some(fastcull_core::catalog::PickState::Picked) => 1,
                Some(fastcull_core::catalog::PickState::Rejected) => 2,
                _ => 0,
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
    win.set_status(
        format!(
            "{} ({}/{}){}{} — {} thumbs loaded — ★{} ✕{}{} — {} column{}",
            if cursor_in_view {
                st.labels.get(cursor).cloned().unwrap_or_default()
            } else {
                String::new()
            },
            cursor_pos.map_or(0, |p| p + 1),
            view_len.max(1),
            showing,
            burst_note,
            st.thumbs_done.min(count),
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
fn insert_fullres(st: &mut AppState, index: usize, texture: slint::Image) {
    st.fullres.retain(|(i, _)| *i != index);
    st.fullres.push((index, texture));
    let cursor = st.cursor;
    while st.fullres.len() > 5 {
        let victim = st
            .fullres
            .iter()
            .position(|(i, _)| *i != cursor)
            .unwrap_or(0);
        st.fullres.remove(victim);
    }
}

/// Adopt an engine-cached image as a grid-cell texture: mid rungs directly,
/// full-res downscaled to mid size first (rare, ~30 ms — only for images
/// visited at 1:1 and then viewed at an intermediate zoom) so the mids layer
/// stays ~5 MB per texture.
fn adopt_texture(image: &fastcull_core::loupe::FullImage) -> (u32, slint::Image) {
    use fastcull_core::loupe::{MID_RUNG_MAX_LONG, MID_RUNG_TARGET};
    let long = image.width.max(image.height);
    if long <= MID_RUNG_MAX_LONG {
        return (long, fullres_texture(image));
    }
    let t = u64::from(MID_RUNG_TARGET);
    let (dst_w, dst_h) = if image.width >= image.height {
        (
            MID_RUNG_TARGET,
            (u64::from(image.height) * t / u64::from(image.width)).max(1) as u32,
        )
    } else {
        (
            (u64::from(image.width) * t / u64::from(image.height)).max(1) as u32,
            MID_RUNG_TARGET,
        )
    };
    // Borrowed source: no 150 MB clone of the full-res pixels (validator).
    let src = fast_image_resize::images::ImageRef::new(
        image.width,
        image.height,
        image.rgb.as_ref(),
        fast_image_resize::PixelType::U8x3,
    )
    .expect("valid source image");
    let mut dst =
        fast_image_resize::images::Image::new(dst_w, dst_h, fast_image_resize::PixelType::U8x3);
    fast_image_resize::Resizer::new()
        .resize(&src, &mut dst, None)
        .expect("resize");
    let buffer =
        slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(dst.buffer(), dst_w, dst_h);
    (dst_w.max(dst_h), slint::Image::from_rgb8(buffer))
}

/// Build a slint texture from a decoded full-res image (one 150 MB copy —
/// slint owns its pixel buffers; the core LRU keeps the original).
fn fullres_texture(image: &fastcull_core::loupe::FullImage) -> slint::Image {
    let buffer = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
        &image.rgb,
        image.width,
        image.height,
    );
    slint::Image::from_rgb8(buffer)
}

fn decode_image(jpeg: &[u8]) -> Option<slint::Image> {
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(jpeg, options);
    let pixels = decoder.decode().ok()?;
    let (w, h) = decoder.dimensions()?;
    let buffer =
        slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(&pixels, w as u32, h as u32);
    Some(slint::Image::from_rgb8(buffer))
}
