//! FastCull desktop application: thin Slint bridge over `fastcull-core`
//! (specs/modules/ui-grid.md). All layout math lives in `fastcull_core::grid`;
//! this crate only moves data between the engine and the declarative UI.
//!
//! Usage: `fastcull-app <folder>` or `fastcull-app --synthetic 2000`
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
    /// The one VecModel the window binds; refresh mutates it in place.
    cells: Rc<VecModel<CellData>>,
    /// Full-res loupe assets (real sessions only).
    loupe: Option<fastcull_core::loupe::LoupeEngine>,
    /// UI-side textures for the focused image ± neighbors: sized to the
    /// prefetch ring (5) and cursor-protected on eviction (see
    /// insert_fullres); the core LRU holds the pixel data for rebuilds.
    fullres: Vec<(usize, slint::Image)>,
    one2one: bool,
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
    filter_bar_visible: bool,
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
    if let Some(id) =
        fastcull_core::filter::cursor_after_filter_change(&old_view, old_cursor, &st.view)
    {
        st.cursor = id;
    }
    let loupe_step = grid::ZOOM_COLUMNS.len() - 1;
    if st.view.is_empty() && st.zoom == loupe_step {
        st.one2one = false;
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
    st.thumb_jpegs.clear();
    st.images.clear();
    st.failed.clear();
    st.thumbs_done = 0;
    st.synthetic = false;
    st.fullres.clear();
    st.one2one = false;
    st.mids.clear();
    st.va = fastcull_core::viewassets::ViewAssets::default();
    st.capture_keys = vec![None; count];
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
        Synthetic(usize),
        Folder(std::path::PathBuf),
    }
    let launch = match args.as_slice() {
        [flag, n] if flag == "--synthetic" => {
            let Ok(n) = n.parse::<usize>() else {
                eprintln!("usage: fastcull-app <folder> | --synthetic <count>");
                std::process::exit(2);
            };
            Launch::Synthetic(n)
        }
        [folder] => Launch::Folder(folder.into()),
        _ => {
            eprintln!("usage: fastcull-app <folder> | --synthetic <count>");
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
        cells,
        loupe: None,
        fullres: Vec::new(),
        one2one: start_11,
        last_grid_zoom: 1,
        mids: HashMap::new(),
        va: fastcull_core::viewassets::ViewAssets::default(),
        query: fastcull_core::filter::ViewQuery::default(),
        view: Vec::new(),
        capture_keys: Vec::new(),
        filter_bar_visible: true,
        pipeline_rx: None,
        loupe_rx: None,
        sidecar_errs: None,
    }));

    match launch {
        Launch::Synthetic(n) => {
            let mut st = state.borrow_mut();
            st.labels = (0..n).map(|i| format!("SYN{i:05}.ARW")).collect();
            st.picks = vec![fastcull_core::catalog::PickState::Unmarked; n];
            st.capture_keys = vec![None; n];
            st.synthetic = true;
            recompute_view(&mut st);
        }
        Launch::Folder(path) => {
            if let Err(e) = load_folder(&state, &path) {
                eprintln!("fastcull: {e}");
                std::process::exit(1);
            }
            // load_folder resets one2one; --start-11 wants it back on.
            if start_11 {
                state.borrow_mut().one2one = true;
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
                recompute_view(&mut st);
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
                            }
                            SessionEvent::Sidecar { index, pick } => {
                                // Picks from a previous session/tool — never
                                // override what the user changed just now.
                                if !st.touched.contains(&index) {
                                    if let Some(slot) = st.picks.get_mut(index) {
                                        *slot = pick;
                                        dirty = true;
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
                    let loupe_events: Vec<_> = st
                        .loupe_rx
                        .as_ref()
                        .map(|rx| rx.try_iter().collect())
                        .unwrap_or_default();
                    for event in loupe_events {
                        match event {
                            fastcull_core::loupe::LoupeEvent::Ready { index, image } => {
                                let long = image.width.max(image.height);
                                trace_mark(&format!("loupe ready idx {index} long {long}"));
                                if long <= fastcull_core::loupe::MID_RUNG_MAX_LONG {
                                    // Mid rung (~5 MB copy): grid-cell quality
                                    // for intermediate zooms; cheap, always keep.
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
                    !st.one2one
                        || st.fullres.iter().any(|(i, img)| {
                            *i == st.cursor
                                && img.size().width.max(img.size().height)
                                    > fastcull_core::loupe::MID_RUNG_MAX_LONG
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
                            st.one2one = false;
                            st.zoom = st.last_grid_zoom.min(loupe_step - 1);
                        }
                    }
                }
            }
        }
        // One seamless zoom axis (spec): columns -> loupe fit -> 1:1.
        "one2one" => {
            if st.loupe.is_some() {
                if st.zoom < loupe_step {
                    st.last_grid_zoom = st.zoom;
                    st.zoom = loupe_step; // Z from grid jumps to loupe 1:1
                }
                st.one2one = !st.one2one;
            }
        }
        "grid" => {
            st.one2one = false;
            if st.zoom == loupe_step {
                st.zoom = st.last_grid_zoom.min(loupe_step - 1);
            }
        }
        "zoom-in" => {
            if st.zoom == loupe_step {
                st.one2one = st.loupe.is_some(); // fit -> 1:1
            } else {
                if st.zoom + 1 == loupe_step {
                    st.last_grid_zoom = st.zoom;
                }
                st.zoom = grid::zoom_step(st.zoom, 1);
            }
        }
        "zoom-out" => {
            if st.one2one {
                st.one2one = false; // 1:1 -> fit, not straight to the grid
            } else {
                st.zoom = grid::zoom_step(st.zoom, -1);
            }
        }
        nav => {
            let nav = match nav {
                "left" => Nav::Left,
                "right" => Nav::Right,
                "up" => Nav::Up,
                "down" => Nav::Down,
                "pgup" => Nav::PageUp,
                "pgdn" => Nav::PageDown,
                "home" => Nav::Home,
                "end" => Nav::End,
                _ => return,
            };
            // Navigation happens over VIEW positions; the cursor stays an
            // image id (M5 filter model).
            if !st.view.is_empty() {
                let rows_per_page =
                    ((viewport_h / (layout.cell_height + grid::CELL_GAP)) as usize).max(1);
                let pos = st.cursor_pos().unwrap_or(0);
                let new_pos =
                    grid::navigate(pos, st.view.len(), layout.columns, rows_per_page, nav);
                st.cursor = st.view[new_pos];
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
        if !cur_visible {
            let center_row =
                ((scroll_y + viewport_h * 0.5) / (layout.cell_height + grid::CELL_GAP)) as usize;
            st.cursor = st.view[center_row.min(view_len - 1)];
        }
        if let Some(loupe) = &st.loupe {
            // focus() returns the cached image on a warm hit: the rebuild
            // path for textures evicted UI-side (validator finding — going
            // backwards previously degraded to the thumb forever).
            let focus_index = st.cursor;
            // Ladder target: fit view needs the viewport in physical pixels;
            // 1:1 always demands the top rung.
            let display_long = if st.one2one {
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
    // 1:1 overlay: only shown when the CURSOR's texture exists (a stale
    // previous image must never pose as the current one), and sized in
    // logical pixels divided by the scale factor so 1:1 means device pixels
    // on HiDPI (validator finding — sharpness judging must not upsample).
    // 1:1 requires the TOP rung: showing the mid rung at native 1616 px
    // would be a misleading scale jump (validator finding) — hold the fit
    // view until the full-res texture lands.
    let overlay = st.one2one && at_loupe;
    match fullres_for(&st, cursor).filter(|img| img.size().width.max(img.size().height) > 2048) {
        Some(img) if overlay => {
            let size = img.size();
            let sf = win.window().scale_factor();
            win.set_loupe_w(size.width as f32 / sf);
            win.set_loupe_h(size.height as f32 / sf);
            win.set_loupe_image(img);
            win.set_one2one(true);
        }
        _ => win.set_one2one(false),
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
    win.set_status(
        format!(
            "{} ({}/{}){} — {} thumbs loaded — ★{} ✕{}{} — {} column{}",
            if cursor_in_view {
                st.labels.get(cursor).cloned().unwrap_or_default()
            } else {
                String::new()
            },
            cursor_pos.map_or(0, |p| p + 1),
            view_len.max(1),
            showing,
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
/// evict the focused image itself (validator HIGH finding; the user saw it as
/// back-arrow quality degradation).
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
