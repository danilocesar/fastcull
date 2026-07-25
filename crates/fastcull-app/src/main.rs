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

struct AppState {
    labels: Vec<String>,
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
}

impl AppState {
    fn count(&self) -> usize {
        self.labels.len()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (labels, jobs): (Vec<String>, Option<Vec<JobSpec>>) = match args.as_slice() {
        [flag, n] if flag == "--synthetic" => {
            let Ok(n) = n.parse::<usize>() else {
                eprintln!("usage: fastcull-app <folder> | --synthetic <count>");
                std::process::exit(2);
            };
            ((0..n).map(|i| format!("SYN{i:05}.ARW")).collect(), None)
        }
        [folder] => {
            let session = Session::open(std::path::Path::new(folder)).unwrap_or_else(|e| {
                eprintln!("fastcull: {e}");
                std::process::exit(1);
            });
            let labels = session
                .images
                .iter()
                .map(|i| i.file_name().into_owned())
                .collect();
            let jobs = session
                .images
                .iter()
                .map(|i| JobSpec {
                    path: i.path.clone(),
                    size: i.size,
                    mtime: i.mtime,
                })
                .collect();
            (labels, Some(jobs))
        }
        _ => {
            eprintln!("usage: fastcull-app <folder> | --synthetic <count>");
            std::process::exit(2);
        }
    };

    let synthetic = jobs.is_none();
    let window = MainWindow::new().expect("creating window");
    let cells = Rc::new(VecModel::from(Vec::<CellData>::new()));
    window.set_cells(slint::ModelRc::from(Rc::clone(&cells)));
    let state = Rc::new(RefCell::new(AppState {
        labels,
        zoom: 1, // 8 columns
        cursor: 0,
        thumb_jpegs: HashMap::new(),
        images: HashMap::new(),
        failed: HashSet::new(),
        pipeline: None,
        thumbs_done: 0,
        synthetic,
        cells,
        loupe: None,
        fullres: Vec::new(),
        one2one: false,
        last_grid_zoom: 1,
    }));

    // Start the engines for real folders; events polled on a UI timer.
    let event_rx = jobs.map(|jobs| {
        let paths: Vec<std::path::PathBuf> = jobs.iter().map(|j| j.path.clone()).collect();
        let (pipeline, rx) = Pipeline::start(
            jobs,
            fastcull_core::cache::default_cache_path(),
            std::thread::available_parallelism().map_or(4, |n| n.get()),
        );
        let (loupe, loupe_rx) = fastcull_core::loupe::LoupeEngine::start(
            paths,
            fastcull_core::loupe::DEFAULT_BUDGET_BYTES,
        );
        let mut st = state.borrow_mut();
        st.pipeline = Some(pipeline);
        st.loupe = Some(loupe);
        (rx, loupe_rx)
    });

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

    // Pipeline event pump: drain pending events every 33 ms; refresh once if
    // anything relevant arrived.
    let timer = slint::Timer::default();
    if let Some((rx, loupe_rx)) = event_rx {
        let state = Rc::clone(&state);
        let win = window.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            std::time::Duration::from_millis(33),
            move || {
                let mut dirty = false;
                {
                    let mut st = state.borrow_mut();
                    for event in rx.try_iter() {
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
                            SessionEvent::MetadataReady { .. } => {}
                        }
                    }
                    let at_loupe = st.zoom == grid::ZOOM_COLUMNS.len() - 1;
                    for event in loupe_rx.try_iter() {
                        match event {
                            fastcull_core::loupe::LoupeEvent::Ready { index, image } => {
                                // Skip the 150 MB texture copy for prefetches
                                // arriving after the user left the loupe; the
                                // core LRU keeps the pixels for peek-rebuild.
                                if at_loupe {
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
    window.run().expect("running event loop");
}

fn handle_nav(win: &MainWindow, state: &Rc<RefCell<AppState>>, key: &str) {
    let (layout, viewport_h, scroll_y) = current_geometry(win, state);
    let mut st = state.borrow_mut();
    let loupe_step = grid::ZOOM_COLUMNS.len() - 1;
    match key {
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
            let rows_per_page =
                ((viewport_h / (layout.cell_height + grid::CELL_GAP)) as usize).max(1);
            st.cursor = grid::navigate(st.cursor, st.count(), layout.columns, rows_per_page, nav);
        }
    }
    // Keep the cursor visible under the (possibly new) layout. Order matters
    // (spec, cursor contract): the Flickable clamps viewport-y against its
    // CURRENT viewport-height, so the new virtual height must land first or
    // the reveal gets clamped against stale bounds and the cursor scrolls
    // out of view.
    let layout = GridLayout::new(st.zoom, win.get_grid_width(), st.count());
    let new_scroll = layout.scroll_to_reveal(st.cursor, scroll_y, viewport_h);
    drop(st);
    win.set_virtual_height(layout.total_height);
    win.set_vp_y(-new_scroll);
    refresh(win, state);
}

fn current_geometry(win: &MainWindow, state: &Rc<RefCell<AppState>>) -> (GridLayout, f32, f32) {
    let st = state.borrow();
    let layout = GridLayout::new(st.zoom, win.get_grid_width(), st.count());
    let viewport_h = win.get_grid_height();
    let scroll_y = (-win.get_vp_y()).max(0.0);
    (layout, viewport_h, scroll_y)
}

/// Rebuild the windowed model for the current viewport.
fn refresh(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (layout, viewport_h, scroll_y) = current_geometry(win, state);
    let mut st = state.borrow_mut();
    let count = st.count();
    let range = layout.visible_range(count, scroll_y, viewport_h, MARGIN_ROWS);

    // Tell the engine what is on screen (priority promotion).
    if let Some(pipeline) = &st.pipeline {
        pipeline.set_visible(range.clone());
    }

    // Decode encoded thumbs entering the window, bounded per refresh so a
    // page jump never stalls a frame; leftovers get a follow-up refresh.
    let to_decode: Vec<usize> = range
        .clone()
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
    if at_loupe && count > 0 {
        // Scroll moves the cursor ONLY when the cursor's cell left the
        // viewport: unconditionally snapping to the center row made arrow
        // keys a no-op on tall windows where >2 rows fit (validator
        // finding — move, no scroll needed, snap-back to center).
        let (_, cur_top) = layout.position(st.cursor);
        let cur_visible =
            cur_top < scroll_y + viewport_h && cur_top + layout.cell_height > scroll_y;
        if !cur_visible {
            let center_row =
                ((scroll_y + viewport_h * 0.5) / (layout.cell_height + grid::CELL_GAP)) as usize;
            st.cursor = center_row.min(count - 1);
        }
        if let Some(loupe) = &st.loupe {
            // focus() returns the cached image on a warm hit: the rebuild
            // path for textures evicted UI-side (validator finding — going
            // backwards previously degraded to the thumb forever).
            let focus_index = st.cursor;
            let hit = loupe.focus(focus_index);
            let missing = !st.fullres.iter().any(|(i, _)| *i == focus_index);
            if let (Some(image), true) = (hit, missing) {
                let texture = fullres_texture(&image);
                insert_fullres(&mut st, focus_index, texture);
            }
        }
    }
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
    let overlay = st.one2one && at_loupe;
    match fullres_for(&st, cursor) {
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
    for index in range.clone() {
        let (x, y) = layout.position(index);
        let full = if at_loupe {
            fullres_for(&st, index)
        } else {
            None
        };
        let image = full.as_ref().or(st.images.get(&index));
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
    win.set_status(
        format!(
            "{} ({}/{}) — {} images, {} thumbs loaded — {} column{}",
            st.labels.get(cursor).cloned().unwrap_or_default(),
            cursor + 1,
            count.max(1),
            count,
            st.thumbs_done.min(count),
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
