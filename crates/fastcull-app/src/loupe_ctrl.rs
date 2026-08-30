//! Loupe and pointer control: every pointer gesture (grid cells and the
//! loupe surfaces) folded through the core pointer machine, the loupe's
//! geometry helpers (aspect, the 1:1 ceiling, the rendered factor), the
//! warm-decode routing rule, and the full-res texture ring.
//!
//! Not here: WHICH rung the loupe ends up showing (sharp / soft mid / thumb
//! rescue / hold / fit) is decided by `fastcull_core::transit::render_rung`
//! and walked by the refresh pass in `presenter.rs` — this module supplies
//! the geometry and the textures it chooses among. The ring's victim choice
//! is core's too (`transit::evict_fullres`); `insert_fullres` below just
//! carries it out.

use std::cell::RefCell;
use std::rc::Rc;

use fastcull_core::grid::{self, GridLayout};
use fastcull_core::loupe::is_top_rung;
use slint::ComponentHandle;

use crate::presenter::refresh;
use crate::state::AppState;
use crate::MainWindow;

/// Wire every pointer gesture: clicks and drags on the loupe surfaces, the
/// wheel, and the grid cells' click/double-click.
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        // Click in the zoom overlay: "center HERE", FACTOR UNCHANGED
        // (issue #11 transition table, superseding the earlier "below the
        // ceiling a click jumps to 1:1" default — double-click owns the
        // 1:1 jump now). Fractions arrive image-relative from Slint, so
        // this IS the machine's (Zoomed, Click) → Recenter row without a
        // lossy coords round-trip.
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_loupe_clicked(move |fx, fy| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                // Clicking to pixel-peep is as much a claim as any other
                // click (validator: a capture-key re-sort could otherwise
                // swap the image under an active 1:1 inspection).
                st.grid.cursor_touched = true;
                st.loupe_view.pan_center = (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0));
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
        let state = Rc::clone(state);
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

    {
        // Pointer wheel (issue #11): one notch-equivalent = one ladder
        // stop, anchored under the pointer. Arrives from the fit surface
        // or the zoom overlay; the machine decides from the actual state.
        let state = Rc::clone(state);
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
        let state = Rc::clone(state);
        window.on_fit_clicked(move || {
            state.borrow_mut().grid.cursor_touched = true;
        });
    }
    {
        // Double-clicks in the loupe (fit or zoomed): 1:1 with the
        // clicked point centered — IF the two presses were close (spec:
        // farther apart = two independent clicks, already handled).
        {
            let state = Rc::clone(state);
            let win = window.as_weak();
            window.on_fit_double_clicked(move |x, y| {
                let Some(win) = win.upgrade() else { return };
                handle_loupe_double_click(&win, &state, x, y);
            });
        }
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_zoom_double_clicked(move |x, y| {
            let Some(win) = win.upgrade() else { return };
            handle_loupe_double_click(&win, &state, x, y);
        });
    }
    {
        // Grid double-click: open that image in the loupe at fit (the
        // first click already moved and claimed the cursor).
        let state = Rc::clone(state);
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
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_cell_clicked(move |id, _lx, _ly, ctrl, shift| {
            let Some(win) = win.upgrade() else { return };
            {
                let mut st = state.borrow_mut();
                let id = id as usize;
                if !st.grid.view.contains(&id) {
                    return;
                }
                st.grid.cursor_touched = true; // clicks claim (untouched-cursor rule)
                if ctrl {
                    // Ctrl+click: toggle membership; cursor moves too.
                    st.grid.selection.toggle(id);
                    st.grid.cursor = id;
                } else if shift {
                    // Shift+click: span cursor..clicked (view order).
                    let view = st.grid.view.clone();
                    let from = st.grid.cursor;
                    st.grid.selection.extend_to(&view, from, id);
                    st.grid.cursor = id;
                } else {
                    // Plain click: collapse any selection (gate finding:
                    // after Ctrl+A there was NO deselect gesture), move
                    // the cursor.
                    st.grid.selection.clear();
                    st.grid.cursor = id;
                }
            }
            refresh(&win, &state);
        });
    }
}

/// Width/height aspect of the best texture held for an image (any rung —
/// aspect is rung-invariant). None while only the placeholder exists.
fn aspect_for(st: &AppState, index: usize) -> Option<f32> {
    let size = st
        .textures
        .fullres
        .iter()
        .find(|(i, _)| *i == index)
        .map(|(_, img)| img.size())
        .or_else(|| st.textures.mids.get(&index).map(|img| img.size()))
        .or_else(|| st.textures.images.get(&index).map(|img| img.size()))?;
    (size.height > 0).then(|| size.width as f32 / size.height as f32)
}

/// Which kitchen lane a WARM image takes — one already-decoded image the
/// app must hand to the kitchen, at the two places that happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WarmJob {
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
pub(crate) enum WarmCtx {
    /// The engine ANNOUNCED a fresh decode (`LoupeEvent::Ready`), for any
    /// index in the prefetch ring, not just the cursor's.
    Announced,
    /// `focus()` handed back a CACHED image for the cursor because the
    /// UI-side texture had been evicted (the rebuild path).
    FocusHit,
}

/// The warm-hit routing rule, both contexts, one function.
///
/// `mid_held` = a mid-rung texture for this index is already in `st.textures.mids`;
/// `at_loupe` = the loupe is on screen right now.
pub(crate) fn route_warm(
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
                // window) goes through Wrap into st.textures.mids — where the
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
pub(crate) fn max_factor(win: &MainWindow, st: &AppState) -> Option<f32> {
    let img = st
        .textures
        .fullres
        .iter()
        .find(|(i, _)| *i == st.grid.cursor)
        .map(|(_, img)| img)?;
    let size = img.size();
    if !is_top_rung(
        size.width.max(size.height),
        st.textures.terminal_native.contains(&st.grid.cursor),
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
pub(crate) fn clamped_factor(win: &MainWindow, st: &AppState) -> f32 {
    match max_factor(win, st) {
        Some(max) => st.loupe_view.zoom_factor.clamp(1.0, max.max(1.0)),
        None => st.loupe_view.zoom_factor.max(1.0),
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
        .textures
        .fullres
        .iter()
        .find(|(i, _)| *i == st.grid.cursor)
        .map(|(_, img)| {
            let s = img.size();
            (s.width as f32 / sf, s.height as f32 / sf)
        })
        .or_else(|| aspect_for(st, st.grid.cursor).map(|aspect| (vh * aspect, vh)))
        .unwrap_or((vw, vh));
    // The COUNT is the Grid arm's payload, so it is still derived here;
    // the PREDICATE is `at_loupe()` (they agree — a single 1 sits at the
    // last index and zoom is clamped to the ladder).
    let columns = grid::ZOOM_COLUMNS[st.grid.zoom.min(grid::ZOOM_COLUMNS.len() - 1)];
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
        let layout = GridLayout::new(st.grid.zoom, vw, win.get_grid_height(), st.grid.view.len());
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
        pan_center: st.loupe_view.pan_center,
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
/// `double-clicked` cannot fire for distant presses at all. Anchored to a
/// test since 2026-08-29 (issue #13): the grid half of that dependency is
/// driven with real clicks in
/// `two_distant_clicks_are_two_clicks_not_a_double_click` — two presses
/// 600 px apart do not pair, the same two on one point do — and the drag
/// half in `a_grid_drag_scrolls_without_clicking_the_cell_under_it`.
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
        st.grid.cursor_touched = true;
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
                st.loupe_view.pan_center = center;
                st.loupe_view.zoom_factor = factor;
                if factor <= 1.0 {
                    st.loupe_view.pan_center = (0.5, 0.5); // fit forgets the pan spot
                }
            }
            Action::Recenter { center } => {
                st.loupe_view.pan_center = center;
            }
            Action::EnterLoupe => {
                st.enter_loupe(1.0);
                // Also from INSIDE the loupe (a double-click while zoomed
                // is the way back to fit), so these are unconditional.
                st.loupe_view.zoom_factor = 1.0;
                st.loupe_view.pan_center = (0.5, 0.5);
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

/// Keep a full-res texture, giving up slots until the ring is back within
/// [`FULLRES_RING`].
///
/// The victim CHOICE — protect the cursor's own texture, farthest by view
/// distance first, out-of-view entries ahead of everything, ties to the
/// later slot — lives in `fastcull_core::transit::evict_fullres` and is
/// table-tested there (A3). Here: re-inserting the texture (a re-announced
/// index moves to the end of the ring, as it always did) and removing the
/// slots core names. `evict_fullres` returns None once the ring fits, so
/// even the capacity test is not re-derived at this end — the literal `5`
/// that used to sit in this loop is gone, and the ring is now `2·PREFETCH+1`
/// by construction rather than by comment.
pub(crate) fn insert_fullres(st: &mut AppState, index: usize, texture: slint::Image) {
    st.textures.fullres.retain(|(i, _)| *i != index);
    st.textures.fullres.push((index, texture));
    let cursor = st.grid.cursor;
    loop {
        let held: Vec<usize> = st.textures.fullres.iter().map(|(i, _)| *i).collect();
        match fastcull_core::transit::evict_fullres(&held, cursor, &st.grid.view) {
            Some(victim) => {
                st.textures.fullres.remove(victim);
            }
            None => break,
        }
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
