//! Navigation and the view query: the keyboard nav/mark/zoom keys, the
//! filter and sort controls, the view recompute that keeps the cursor on a
//! member, and the reveal math that keeps the cursor's cell on screen.

use std::cell::RefCell;
use std::rc::Rc;

use fastcull_core::grid::{self, GridLayout, Nav};
use slint::ComponentHandle;

use crate::loupe_ctrl::{clamped_factor, max_factor};
use crate::presenter::refresh;
use crate::state::AppState;
use crate::trace::{trace_slow, trace_start};
use crate::MainWindow;

/// Wire the keyboard nav callback and the filter-bar controls (chip,
/// sort cycle, bar visibility).
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_nav(move |key| {
            let Some(win) = win.upgrade() else { return };
            handle_nav(&win, &state, key.as_str());
        });
    }
    {
        let state = Rc::clone(state);
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
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_cycle_sort(move || {
            let Some(win) = win.upgrade() else { return };
            {
                use fastcull_core::filter::SortKey;
                let mut st = state.borrow_mut();
                // Cycle: Capture ↑ → Capture ↓ → Name ↑ → Name ↓ → …
                let q = &mut st.grid.query;
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
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_toggle_filter_bar(move || {
            let Some(win) = win.upgrade() else { return };
            let hide_resets = {
                let mut st = state.borrow_mut();
                st.grid.filter_bar_visible = !st.grid.filter_bar_visible;
                win.set_filter_bar_visible(st.grid.filter_bar_visible);
                // Persona G6: a filter must never be active while invisible.
                !st.grid.filter_bar_visible
                    && st.grid.query.filter != fastcull_core::filter::PickFilter::All
            };
            if hide_resets {
                apply_filter_change(&win, &state, fastcull_core::filter::PickFilter::All);
            } else {
                reveal_cursor(&win, &state);
            }
        });
    }
}

pub(crate) fn recompute_view(st: &mut AppState) {
    let complete = st.metadata_complete();
    st.grid.view = fastcull_core::filter::view(
        &st.picks,
        &st.labels,
        &st.capture_keys,
        &st.grid.query,
        complete,
    );
    // Every membership/order change bumps the generation: a cursor
    // displaced by a view RE-SORT (capture keys streaming in during
    // load) is not scrolling, and the follow-scroll claim must not
    // fire on it (issue #22 — the cursor moved during folder load with
    // no input, and the load-race flaked CI).
    st.grid.view_generation = st.grid.view_generation.wrapping_add(1);
    // Re-key the loupe prefetch ring in the same tick (issue #46): the
    // ring walks VIEW order — what arrows actually reach — and a stale
    // ring after a filter/sort change would warm ghosts. Every view
    // change funnels through here (load_folder recomputes after the
    // engine starts, so a fresh session is keyed too).
    if let Some(loupe) = &st.loupe_view.engine {
        loupe.set_view(&st.grid.view);
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
pub(crate) fn recompute_view_keep_cursor(st: &mut AppState, user_changed_query: bool) {
    let old_view = std::mem::take(&mut st.grid.view);
    let old_cursor = old_view.contains(&st.grid.cursor).then_some(st.grid.cursor);
    recompute_view(st);
    if let Some(id) = fastcull_core::filter::cursor_after_recompute(
        &old_view,
        old_cursor,
        &st.grid.view,
        st.grid.cursor_touched,
        st.metadata_complete(),
        user_changed_query,
    ) {
        st.grid.cursor = id;
    }
    if st.grid.view.is_empty() {
        st.exit_loupe(); // nothing to look at: the empty state is a grid
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
        st.grid.query.filter = filter;
        recompute_view_keep_cursor(&mut st, true); // the USER re-filtered
    }
    reveal_cursor(win, state);
}

pub(crate) fn handle_nav(win: &MainWindow, state: &Rc<RefCell<AppState>>, key: &str) {
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
        st.grid.cursor_touched = true;
    }
    match key {
        "pick" | "reject" | "clear" => {
            let pick = match key {
                "pick" => fastcull_core::catalog::PickState::Picked,
                "reject" => fastcull_core::catalog::PickState::Rejected,
                _ => fastcull_core::catalog::PickState::Unmarked,
            };
            let cursor = st.grid.cursor;
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
                    &st.grid.view,
                    key != "clear",
                ) {
                    Some(id) => st.grid.cursor = id,
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
            if st.loupe_view.engine.is_some() && !st.enter_loupe(f32::INFINITY) {
                if st.loupe_view.zoom_factor > 1.0 {
                    st.loupe_view.zoom_factor = 1.0;
                    st.loupe_view.pan_center = (0.5, 0.5); // fit forgets the pan spot
                } else if max_factor(win, &st).is_none_or(|max| max > 1.0) {
                    // Small-file guard (validator L1): a known ceiling at
                    // or below fit has no 1:1 to jump to; leaving the
                    // desire at fit keeps the next `-` meaningful.
                    st.loupe_view.zoom_factor = f32::INFINITY;
                }
            }
        }
        "grid" => {
            if !st.at_loupe() && st.loupe_view.zoom_factor <= 1.0 {
                // Already at a grid zoom: Esc/G collapses the selection
                // (the deselect gesture — gate finding).
                st.grid.selection.clear();
            }
            st.exit_loupe();
            // At a grid zoom there is no loupe to leave, but a carried
            // factor (and its pan) is still dropped here.
            st.loupe_view.zoom_factor = 1.0;
            st.loupe_view.pan_center = (0.5, 0.5);
        }
        "zoom-in" => {
            if st.at_loupe() {
                if st.loupe_view.engine.is_some() {
                    // Climb one x1.5 stop from the CLAMPED factor (the
                    // desired one may be INFINITY from an earlier Z). An
                    // unknown ceiling (full-res not decoded yet) climbs
                    // optimistically; the render clamp lands it at 1:1.
                    let actual = clamped_factor(win, &st);
                    st.loupe_view.zoom_factor = match max_factor(win, &st) {
                        Some(max) => fastcull_core::zoompan::ladder_up(actual, max),
                        None => actual * fastcull_core::zoompan::ZOOM_STEP,
                    };
                }
            } else {
                if st.grid.zoom + 1 == grid::ZOOM_COLUMNS.len() - 1 {
                    st.remember_grid_zoom(); // this step crosses INTO the loupe
                }
                st.grid.zoom = grid::zoom_step(st.grid.zoom, 1);
            }
        }
        "zoom-out" => {
            if st.loupe_view.zoom_factor > 1.0 {
                // Retrace the x1.5 stops down to fit, never straight to
                // the grid. Unknown ceiling: nothing above fit was ever
                // rendered, so fit is the only honest stop.
                let actual = clamped_factor(win, &st);
                st.loupe_view.zoom_factor = if max_factor(win, &st).is_some() {
                    fastcull_core::zoompan::ladder_down(actual)
                } else {
                    1.0
                };
                if st.loupe_view.zoom_factor <= 1.0 {
                    st.loupe_view.pan_center = (0.5, 0.5); // fit forgets the pan spot
                }
            } else {
                st.grid.zoom = grid::zoom_step(st.grid.zoom, -1);
            }
        }
        // [ / ]: previous/next burst boundary over the FILTERED view
        // (burst-grouping.md UI contract): first visible frame of the
        // adjacent group; singles are their own territory; clamps.
        "burst-prev" | "burst-next" => {
            if !st.grid.view.is_empty() {
                let pos = st.cursor_pos().unwrap_or(0);
                let view = st.grid.view.clone();
                let group_of = |p: usize| st.bursts.group_of.get(view[p]).copied().flatten();
                let new_pos = fastcull_core::burst::next_boundary(
                    pos,
                    view.len(),
                    group_of,
                    key == "burst-next",
                );
                st.grid.cursor = view[new_pos];
                // Plain navigation resets the Shift-span anchor, same as
                // the arrow keys (selection contract, ui-grid.md).
                st.grid.selection.reset_anchor();
            }
        }
        "select-all" => {
            st.grid.cursor_touched = true;
            let view = st.grid.view.clone();
            st.grid.selection.select_all(&view);
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
                st.grid.cursor_touched = true; // shift-nav claims like plain nav
            }
            // Navigation happens over VIEW positions; the cursor stays an
            // image id (M5 filter model).
            if !st.grid.view.is_empty() {
                let rows_per_page =
                    ((viewport_h / (layout.cell_height + grid::CELL_GAP)) as usize).max(1);
                let pos = st.cursor_pos().unwrap_or(0);
                let new_pos =
                    grid::navigate(pos, st.grid.view.len(), layout.columns, rows_per_page, nav);
                let from = st.grid.cursor;
                st.grid.cursor = st.grid.view[new_pos];
                if extends {
                    // Shift+arrow: span anchor..cursor (core model).
                    let view = st.grid.view.clone();
                    let to = st.grid.cursor;
                    st.grid.selection.extend_to(&view, from, to);
                } else {
                    st.grid.selection.reset_anchor();
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

pub(crate) fn current_geometry(
    win: &MainWindow,
    state: &Rc<RefCell<AppState>>,
) -> (GridLayout, f32, f32) {
    let st = state.borrow();
    let (viewport_h, scroll_y) = viewport_metrics(win);
    let layout = GridLayout::new(
        st.grid.zoom,
        win.get_grid_width(),
        viewport_h,
        st.grid.view.len(),
    );
    (layout, viewport_h, scroll_y)
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
    let layout = GridLayout::new(st.grid.zoom, width, viewport_h, st.grid.view.len());
    let pos = st.cursor_pos().unwrap_or(0);
    let new_scroll = layout.scroll_to_reveal(pos, scroll_y, viewport_h);
    // This reveal IS the relayout correction for its geometry: mark it
    // consumed so refresh doesn't re-anchor on top of an already
    // consistent (geometry, offset) pair (the grid resize branch
    // double-corrected panel toggles with mixed old/new frames, and a nav
    // key racing a resize would double-correct the same way).
    st.grid.last_view_geometry = Some((width, viewport_h));
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
pub(crate) fn reveal_cursor(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let (viewport_h, scroll_y) = viewport_metrics(win);
    let reveal = {
        let mut st = state.borrow_mut();
        reveal_scroll(win, &mut st, viewport_h, scroll_y)
    };
    apply_reveal(win, reveal);
    refresh(win, state);
}
