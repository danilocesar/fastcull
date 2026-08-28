//! The presenter: the single refresh pass that turns `AppState` into what
//! the window shows. Every controller ends by calling [`refresh`].
//!
//! The pass runs in eight phases, in this order, under ONE state borrow:
//!
//! 1. [`detect_drift`] — what changed since the last refresh: the grid
//!    geometry (relayout, issue #16), the view's membership or order
//!    (issue #22), the load-settled edge (issue #25).
//! 2. [`anchor_the_scroll`] — put the viewport back where the content the
//!    user was looking at is, after a whole-grid re-sort or a relayout.
//! 3. [`visible_window`] — which view positions are on screen, what the
//!    engine should prioritise, which thumbs go to the texture kitchen.
//! 4. [`claim_cursor_at_loupe`] — at one column the visible image IS the
//!    cursor, so a scroll can move it (cursor contract); then the warm
//!    decode request for what it now looks at.
//! 5. [`climb_mid_rung`] — cells that outgrew the 320 px thumb.
//! 6. [`render_loupe_rung`] — sharp / soft / thumb rescue / bounded hold /
//!    honest drop, plus the state badge. The ui-grid.md ladder, DECIDED by
//!    `fastcull_core::transit::render_rung` and only rendered here.
//! 7. [`fill_grid_cells`] — the windowed cell model, mutated in place.
//! 8. [`write_status_and_chrome`] — filter bar, empty state, IPTC panel,
//!    scroll hint, status line.
//!
//! What one phase computes for the next travels in [`Pass`] — including
//! `scroll_y`, which phase 2 may correct and everything after must render
//! against. It is deliberately NOT a field of `AppState`: it is a fact
//! about this pass, and the window owns the real scroll position.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use fastcull_core::grid::{self, GridLayout};
use fastcull_core::loupe::is_top_rung;
use fastcull_core::transit::{DropReason, RenderDecision};
use slint::{ComponentHandle, Model};

use crate::iptc_bridge::refresh_iptc_panel;
use crate::loupe_ctrl::{clamped_factor, route_warm, WarmCtx, WarmJob};
use crate::nav::current_geometry;
use crate::state::{AppState, MARGIN_ROWS, MIDS_CAP, OVERLAY_HOLD_CAP};
use crate::trace::{trace_mark, trace_slow, trace_start};
use crate::{CellData, MainWindow};

/// Wire the viewport-changed callback: the Flickable's scroll position
/// changing is a request to re-render the window.
pub(crate) fn wire(window: &MainWindow, state: &Rc<RefCell<AppState>>) {
    {
        let state = Rc::clone(state);
        let win = window.as_weak();
        window.on_viewport_changed(move || {
            if let Some(win) = win.upgrade() {
                refresh(&win, &state);
            }
        });
    }
}

/// Rebuild the windowed model for the current viewport.
pub(crate) fn refresh(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let t0 = trace_start();
    refresh_inner(win, state);
    trace_slow("refresh", t0);
}

/// What one refresh pass established about the geometry it renders into,
/// before it changes anything: the layout, the scroll offset, and the four
/// "what drifted since last time" answers every later phase asks.
///
/// `scroll_y` lives here rather than in `AppState` on purpose: it is a
/// fact about THIS pass (the anchoring phase may correct it), not a
/// property of the app — the window owns the real scroll position.
struct Pass {
    layout: GridLayout,
    viewport_h: f32,
    scroll_y: f32,
    view_len: usize,
    /// At one column, the visible image IS the cursor (spec, cursor
    /// contract). Fixed for the pass: no phase changes the zoom step.
    at_loupe: bool,
    /// The grid geometry the PREVIOUS refresh saw (None on the first).
    prev_geom: Option<(f32, f32)>,
    /// Geometry changed since the last refresh: chrome or window movement,
    /// never user scrolling (issue #16).
    relayout: bool,
    /// The view's membership or order changed under the cursor (issue #22).
    view_mutated: bool,
    /// This pass can actually act on an anchoring decision (it has a
    /// viewport height and something to show).
    can_anchor: bool,
    /// Issue #25's one-shot edge: the last metadata job just landed.
    load_settled: bool,
}

/// PHASE 1 — what drifted since the last refresh: geometry (relayout),
/// view membership/order, and the load-settled edge. Consumes those edges
/// (recording this pass's values) so each is answered once per refresh.
fn detect_drift(win: &MainWindow, st: &mut AppState, geom: (GridLayout, f32, f32)) -> Pass {
    let (layout, viewport_h, scroll_y) = geom;
    // Relayout detection (issue #16): a geometry change since the last
    // refresh is chrome/window movement (panel toggle, resize), never
    // user scrolling.
    let geom_now = (win.get_grid_width(), viewport_h);
    let prev_geom = st.grid.last_view_geometry;
    let relayout = prev_geom.is_some_and(|g| g != geom_now);
    st.grid.last_view_geometry = Some(geom_now);
    // A view that mutated since the last refresh (metadata re-sort
    // during load, live filter removal) displaces the cursor without
    // any scrolling — the follow-scroll claim must re-anchor instead
    // (issue #22).
    let view_mutated = st.grid.last_view_generation != st.grid.view_generation;
    st.grid.last_view_generation = st.grid.view_generation;
    let view_len = st.grid.view.len();
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
    let load_settled = st.metadata_complete() && !st.grid.last_metadata_complete && can_anchor;
    if can_anchor {
        st.grid.last_metadata_complete = st.metadata_complete();
    }
    Pass {
        layout,
        viewport_h,
        scroll_y,
        view_len,
        at_loupe: st.at_loupe(),
        prev_geom,
        relayout,
        view_mutated,
        can_anchor,
        load_settled,
    }
}

/// PHASE 2 — anchoring: put the viewport where the content the user was
/// looking at still is, after a whole-grid re-sort (issue #25) or a
/// relayout (issue #16). Writes `pass.scroll_y` so the rest of the pass
/// renders the corrected view, not the stale one.
fn anchor_the_scroll(win: &MainWindow, st: &mut AppState, pass: &mut Pass) {
    let (layout, viewport_h, view_len) = (&pass.layout, pass.viewport_h, pass.view_len);
    let mut scroll_y = pass.scroll_y;
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
    if pass.load_settled && layout.columns > 1 {
        let cur_pos = st.cursor_pos().unwrap_or(0);
        // The guard itself lives in core (rule 5) and is unit-tested there:
        // grid::scroll_after_resort. It shipped into review MISSING from the
        // app-level version, so it does not belong in the app.
        let corrected = grid::scroll_after_resort(
            layout,
            cur_pos,
            scroll_y,
            viewport_h,
            st.grid.last_cursor_visible,
        );
        // Unconditional marker: a test must be able to see that the flip
        // HAPPENED, not only that it moved something (validator: the old
        // trace fired solely inside the >=0.5px branch, so a run where the
        // re-sort never occurred was indistinguishable from one where it
        // occurred and correctly changed nothing).
        trace_mark(&format!(
            "load settled: cursor pos {cur_pos}, scroll {scroll_y:.0} -> {corrected:.0}  (cursor was {})",
            if st.grid.last_cursor_visible {
                "visible"
            } else {
                "off-screen; offset kept"
            }
        ));
        win.set_virtual_height(layout.total_height);
        win.set_vp_y(-corrected);
        scroll_y = corrected;
    }
    if pass.relayout && layout.columns > 1 && view_len > 0 && viewport_h > 0.0 {
        if let Some((old_width, old_viewport_h)) = pass.prev_geom {
            let old_layout = GridLayout::new(st.grid.zoom, old_width, old_viewport_h, view_len);
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
    if pass.can_anchor {
        st.grid.last_cursor_visible = st
            .cursor_pos()
            .is_some_and(|p| layout.is_visible(p, scroll_y, viewport_h));
    }
    pass.scroll_y = scroll_y;
}

/// PHASE 3 — the visible window: which view positions are on screen (plus
/// the margin rows), what the engine should prioritise, and which thumbs
/// go to the texture kitchen.
fn visible_window(st: &mut AppState, pass: &Pass) -> (Range<usize>, Vec<usize>) {
    let (layout, viewport_h, scroll_y, view_len) =
        (&pass.layout, pass.viewport_h, pass.scroll_y, pass.view_len);
    // Visible VIEW positions; `ids` are the image ids shown there (the two
    // coincide only with filter=All + capture sort before keys arrive).
    let range = layout.visible_range(view_len, scroll_y, viewport_h, MARGIN_ROWS);
    let ids: Vec<usize> = range.clone().map(|pos| st.grid.view[pos]).collect();

    // Tell the engine what is on screen (priority promotion).
    if let Some(pipeline) = &st.session.pipeline {
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
        .filter(|i| st.textures.thumb_jpegs.contains_key(i) && !st.textures.images.contains_key(i))
        .collect();
    // Cursor first (issue #46): at the loupe the cursor's thumb is the
    // transit rescue rung, and the kitchen is FIFO — cooking a margin
    // cell ahead of it extends a residual hold pointlessly (observed:
    // the margin thumb took the first cook slot and the hold cap fired
    // 9 ms before the cursor's own thumb landed).
    if let Some(pos) = to_prep.iter().position(|i| *i == st.grid.cursor) {
        let c = to_prep.remove(pos);
        to_prep.insert(0, c);
    }
    for index in to_prep {
        if let Some(jpeg) = st.textures.thumb_jpegs.remove(&index) {
            st.kitchen.submit_thumb(index, jpeg);
        }
    }
    (range, ids)
}

/// The UI-side full-res texture held for `index`, if any. Used by the rung
/// choice (is there a sharp one for the cursor?) and by the cell fill.
fn fullres_for(st: &AppState, index: usize) -> Option<slint::Image> {
    st.textures
        .fullres
        .iter()
        .find(|(i, _)| *i == index)
        .map(|(_, img)| img.clone())
}

/// PHASE 4 — the cursor claim at the loupe, and the warm-decode request
/// for what it is now looking at. At one column the visible image IS the
/// cursor (cursor contract), so a scroll that carries the cursor's cell off
/// screen MOVES the cursor — but only a POSITIVE scrollbar signal counts
/// (issues #16/#22); geometry moving under the cursor re-anchors instead.
///
/// Takes the `Rc` as well as the borrow: the re-anchor arm schedules a
/// zero-delay follow-up refresh, and that closure needs its own handle.
fn claim_cursor_at_loupe(
    win: &MainWindow,
    state: &Rc<RefCell<AppState>>,
    st: &mut AppState,
    pass: &Pass,
) {
    let layout = &pass.layout;
    let (viewport_h, scroll_y, view_len) = (pass.viewport_h, pass.scroll_y, pass.view_len);
    let (at_loupe, relayout, view_mutated) = (pass.at_loupe, pass.relayout, pass.view_mutated);
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
                st.grid.cursor = st.grid.view[claimed];
                st.grid.cursor_touched = true; // scrolling the loupe IS cursor movement
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
        if let Some(loupe) = &st.loupe_view.engine {
            // focus() returns the cached image on a warm hit: the rebuild
            // path for textures evicted UI-side (validator finding — going
            // backwards previously degraded to the thumb forever).
            let focus_index = st.grid.cursor;
            // Ladder target: fit view needs the viewport in physical pixels;
            // any factor above fit demands the top rung (quality rule as
            // revised by #21: the top rung is still ALWAYS requested;
            // until it lands the view renders soft-flagged, never
            // unflagged).
            let display_long = if st.loupe_view.zoom_factor > 1.0 {
                u32::MAX
            } else {
                (win.get_grid_width() * win.window().scale_factor()) as u32
            };
            let hit = loupe.focus(focus_index, display_long);
            let missing = !st.textures.fullres.iter().any(|(i, _)| *i == focus_index);
            if let (Some(image), true) = (hit, missing) {
                // Kitchen fills the buffer off-thread; the pending guards
                // absorb the refresh loop re-asking every frame while it
                // cooks. The routing rule itself (and why this context
                // differs from the pump's) lives in `route_warm`.
                let long = image.width.max(image.height);
                match route_warm(
                    long,
                    st.textures.terminal_native.contains(&focus_index),
                    at_loupe,
                    st.textures.mids.contains_key(&focus_index),
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
}

/// PHASE 5 — the intermediate-zoom ladder: cells that have outgrown the
/// 320 px thumb climb to the mid rung, and mids for cells that scrolled
/// away are dropped. Returns whether this zoom wants mids at all — the
/// cell fill needs the same answer.
fn climb_mid_rung(win: &MainWindow, st: &mut AppState, pass: &Pass, ids: &[usize]) -> bool {
    let layout = &pass.layout;
    let at_loupe = pass.at_loupe;
    // Intermediate-zoom ladder (user bug report: cells looked bad between
    // 8-column and loupe): cells wider than 320*1.25 physical px outgrow the
    // thumb — climb them to the mid rung via the same 25% rule.
    let cell_phys = layout.cell_width * win.window().scale_factor();
    let want_mid = !at_loupe && cell_phys > 320.0 * 1.25;
    if want_mid {
        let stx = &mut *st;
        if let Some(loupe) = &stx.loupe_view.engine {
            // ensure() also returns cached images no event will announce
            // (the zoom-walk bug: pruned-and-revisited cells stayed
            // thumbs). Downscales cook on the kitchen worker; stale ones
            // for scrolled-away cells are culled here, at the submission
            // wave (spec rule) — landed ones are still adopted.
            stx.kitchen.cull_mids(ids);
            for (index, image) in stx.textures.va.ensure(ids, cell_phys as u32, loupe) {
                if stx.textures.mids.len() >= MIDS_CAP && !stx.textures.mids.contains_key(&index) {
                    break;
                }
                stx.kitchen.submit_mid(index, image);
            }
        }
    }
    st.textures.mids.retain(|i, _| ids.contains(i));
    st.textures.va.prune(ids);
    want_mid
}

/// The honest floor: the overlay comes down to the fit view, and every
/// piece of bookkeeping about what was on it is cleared.
///
/// The trace is the M1 fit-flash pin — an overlay dropping while the desire
/// is still above fit must NAME its excuse (the regression tests grep these
/// two strings). The old code re-derived "is this the traced case?" from
/// `one2one && overlay`; it is now exactly "the reason is one of the two
/// traced ones", which is the same set by construction: `render_rung` emits
/// `DecodeFailed`/`HoldCap` only when the overlay was up and still wanted.
fn honest_drop(win: &MainWindow, st: &mut AppState, cursor: usize, reason: DropReason) {
    let excuse = match reason {
        DropReason::DecodeFailed => Some("decode failed"),
        DropReason::HoldCap => Some("hold cap"),
        // Leaving the ladder, or a cold entry with nothing on screen:
        // nothing was lost, so there is nothing to excuse.
        DropReason::BelowLadder | DropReason::NothingToHold => None,
    };
    if let Some(excuse) = excuse {
        trace_mark(&format!("loupe overlay dropped idx {cursor} ({excuse})"));
    }
    win.set_one2one(false);
    win.set_loupe_soft(false);
    st.loupe_view.last_pan_write = None;
    st.loupe_view.last_overlay_cursor = None;
    st.loupe_view.overlay_hold = None;
    st.loupe_view.last_soft_rung = None;
}

/// PHASE 6 — which rung the loupe overlay shows, and the render of it:
/// sharp (the top rung) / soft (the cursor's mid) / thumb rescue / a
/// bounded residual hold / the honest drop to fit — plus the state badge.
///
/// The ladder and its rules are ui-grid.md's, and since A3 the DECISION is
/// `fastcull_core::transit::render_rung` — table-tested there over every
/// input combination. What is left here is what only the app can do: look
/// the textures up, read the clock, do the extent math, and write the
/// properties each decision names. Returns the cursor's mark, which the
/// status line spells out.
fn render_loupe_rung(win: &MainWindow, st: &mut AppState, pass: &Pass) -> i32 {
    let (at_loupe, view_len) = (pass.at_loupe, pass.view_len);
    let cursor = st.grid.cursor;
    // Zoom overlay (any factor above fit, capped at 1:1): sized as
    // fit-extent × factor in logical pixels so the capped factor means
    // device pixels on HiDPI.
    let factor = clamped_factor(win, st);
    let overlay = factor > 1.0 && at_loupe;
    // The three rungs of the CURSOR's own image, looked up but not yet
    // judged. Which one (if any) renders is `render_rung`'s call.
    let sharp = fullres_for(st, cursor).filter(|img| {
        is_top_rung(
            img.size().width.max(img.size().height),
            st.textures.terminal_native.contains(&cursor),
        )
    });
    // The mid rung — or a warm sub-top texture the engine re-announced
    // into the fullres slot (the pruned-and-revisited path: validator M3,
    // held-left beyond the mids window used to re-strobe fit with the
    // pixels literally in hand).
    let mid = st
        .textures
        .mids
        .get(&cursor)
        .cloned()
        .or_else(|| fullres_for(st, cursor));
    // The cursor's own 320 px grid thumb (issue #46's bottom rung).
    let thumb = st.textures.images.get(&cursor).cloned();
    // One clock read for the whole decision: the same `now` measures the
    // hold and stamps a new one, as it always did.
    let now = std::time::Instant::now();
    let decision = fastcull_core::transit::render_rung(&fastcull_core::transit::RungInputs {
        has_sharp: sharp.is_some(),
        has_mid: mid.is_some(),
        has_thumb: thumb.is_some(),
        cursor_failed: st.textures.failed.contains(&cursor),
        overlay_wanted: overlay,
        // The overlay's own visibility property IS the "were previous
        // pixels on screen" memory the hold arm consults.
        overlay_was_up: win.get_one2one(),
        hold: st.loupe_view.overlay_hold.map(|(held_for, since)| {
            fastcull_core::transit::HoldState {
                same_cursor: held_for == cursor,
                elapsed: now.duration_since(since),
            }
        }),
        hold_cap: OVERLAY_HOLD_CAP,
    });
    // The texture each decision names. `render_rung` decided FROM these
    // being present, so Sharp and Soft always find theirs; the match below
    // still covers the pairing for totality.
    let chosen = match decision {
        RenderDecision::Sharp => sharp,
        RenderDecision::Soft { is_thumb: false } => mid,
        RenderDecision::Soft { is_thumb: true } => thumb,
        RenderDecision::Hold { .. } | RenderDecision::Drop { .. } => None,
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
        st.loupe_view.last_resolved_factor
    };
    match (decision, chosen) {
        (RenderDecision::Sharp, Some(img)) => {
            let size = img.size();
            let sf = win.window().scale_factor();
            let (nw, nh) = (size.width as f32 / sf, size.height as f32 / sf);
            let (vw, vh) = (win.get_grid_width(), win.get_loupe_area_h());
            let s = fastcull_core::zoompan::fit_scale(vw, vh, nw, nh);
            // Texture present => max_factor was known => factor is finite.
            let (ew, eh) = (nw * s * factor, nh * s * factor);
            let ox = fastcull_core::zoompan::offset_centering(vw, ew, st.loupe_view.pan_center.0);
            let oy = fastcull_core::zoompan::offset_centering(vh, eh, st.loupe_view.pan_center.1);
            win.set_loupe_w(ew);
            win.set_loupe_h(eh);
            win.set_loupe_image(img);
            win.set_loupe_vx(ox);
            win.set_loupe_vy(oy);
            // Trace on any offset, visibility or CURSOR change (QE: silent
            // same-size persistence made cross-image forensics blind).
            if st.loupe_view.last_pan_write != Some((ox, oy))
                || !win.get_one2one()
                || win.get_loupe_soft()
                || st.loupe_view.last_overlay_cursor != Some(cursor)
            {
                trace_mark(&format!(
                    "loupe idx {cursor} factor {factor:.3} extent {ew:.0}x{eh:.0} \
                     center {:.3},{:.3} off {ox:.0},{oy:.0}",
                    st.loupe_view.pan_center.0, st.loupe_view.pan_center.1
                ));
            }
            st.loupe_view.last_resolved_factor = Some(factor);
            st.loupe_view.last_pan_write = Some((ox, oy));
            st.loupe_view.last_overlay_cursor = Some(cursor);
            st.loupe_view.overlay_hold = None;
            st.loupe_view.last_soft_rung = None;
            win.set_loupe_soft(false);
            win.set_one2one(true);
        }
        (RenderDecision::Soft { is_thumb }, Some(img)) => {
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
            let ox = fastcull_core::zoompan::offset_centering(vw, ew, st.loupe_view.pan_center.0);
            let oy = fastcull_core::zoompan::offset_centering(vh, eh, st.loupe_view.pan_center.1);
            win.set_loupe_w(ew);
            win.set_loupe_h(eh);
            win.set_loupe_image(img);
            win.set_loupe_vx(ox);
            win.set_loupe_vy(oy);
            if st.loupe_view.last_soft_rung != Some((cursor, is_thumb)) {
                trace_mark(&format!(
                    "loupe {} idx {cursor} factor {f:.3} extent {ew:.0}x{eh:.0}",
                    if is_thumb { "thumb" } else { "soft" }
                ));
            }
            st.loupe_view.last_soft_rung = Some((cursor, is_thumb));
            st.loupe_view.last_pan_write = Some((ox, oy));
            st.loupe_view.last_overlay_cursor = Some(cursor);
            st.loupe_view.overlay_hold = None;
            win.set_loupe_soft(true);
            win.set_one2one(true);
        }
        (RenderDecision::Hold { start }, _) => {
            // Residual HOLD (issue #46, recorded in ui-grid.md): not even
            // the cursor's own thumb exists yet, so keep the PREVIOUS
            // image's pixels at the carried geometry, flagged by the cue
            // pill — a fit-drop is the strobe the transit contract
            // forbids, and a black frame is retinal pumping at 9pm
            // (persona). The bounds that make this honest are the core
            // decision's; what happens here is the clock STAMP.
            if start {
                st.loupe_view.overlay_hold = Some((cursor, now));
                trace_mark(&format!(
                    "loupe hold idx {cursor} (not even a thumb; previous pixels kept)"
                ));
            }
            // Geometry, pixels, last_pan_write and last_overlay_cursor
            // all stay on the PREVIOUS image — the pixels still belong
            // to it, and the first rung of the new image lands in the
            // branches above.
            win.set_loupe_soft(true);
        }
        (RenderDecision::Drop { reason }, _) => honest_drop(win, st, cursor, reason),
        // Unreachable: `render_rung` named a rung from these very Options
        // being Some. The honest floor is the right answer anyway — it is
        // what the old ladder's catch-all did with no pixels in hand.
        (RenderDecision::Sharp | RenderDecision::Soft { .. }, None) => {
            honest_drop(win, st, cursor, DropReason::NothingToHold)
        }
    }
    // Fit pointer surface (issue #11): active at 1 column with no zoom
    // overlay up — the wheel zooms there (browsing is keyboard-only).
    win.set_at_fit(at_loupe && !win.get_one2one() && view_len > 0);

    // Loupe state badge (issue #20): the cursor's mark, written in the
    // same refresh pass that swaps the image/cells — never a frame where
    // the badge belongs to the previous photo (the issue #6 stale class).
    let cursor_mark = match st.session.picks.get(cursor) {
        Some(fastcull_core::catalog::PickState::Picked) => 1,
        Some(fastcull_core::catalog::PickState::Rejected) => 2,
        _ => 0,
    };
    win.set_loupe_mark(cursor_mark);
    if at_loupe && view_len > 0 {
        if st.loupe_view.last_badge != Some((cursor, cursor_mark)) {
            trace_mark(&format!(
                "loupe badge idx {cursor} mark {}",
                match cursor_mark {
                    1 => "picked",
                    2 => "rejected",
                    _ => "none",
                }
            ));
            st.loupe_view.last_badge = Some((cursor, cursor_mark));
        }
    } else {
        // Reset on leaving the loupe so RE-ENTERING traces a fresh line
        // even on an unchanged (cursor, mark) — trace forensics must
        // show what the badge said each time the loupe came up
        // (validator m3).
        st.loupe_view.last_badge = None;
    }
    cursor_mark
}

/// PHASE 7 — the cell model: the windowed rows the grid binds to, mutated
/// in place (spec: reuse the model, don't recreate it). Per cell the
/// quality ladder is full-res > mid > 320 px thumb > placeholder.
fn fill_grid_cells(
    win: &MainWindow,
    st: &AppState,
    pass: &Pass,
    range: Range<usize>,
    want_mid: bool,
) {
    let layout = &pass.layout;
    let at_loupe = pass.at_loupe;
    // Mutate the one bound VecModel in place (spec: reuse, don't recreate).
    let model = Rc::clone(&st.cells);
    let mut row = 0usize;
    for pos in range.clone() {
        let index = st.grid.view[pos];
        let (x, y) = layout.position(pos);
        let full = if at_loupe {
            fullres_for(st, index)
        } else {
            None
        };
        // Quality ladder per cell: loupe full-res > mid rung (large cells
        // or loupe fallback) > 320px thumb > placeholder.
        let image = full
            .as_ref()
            .or_else(|| {
                if want_mid || at_loupe {
                    st.textures.mids.get(&index)
                } else {
                    None
                }
            })
            .or(st.textures.images.get(&index));
        let cell = CellData {
            x,
            y,
            w: layout.cell_width,
            h: layout.cell_height,
            image: image.cloned().unwrap_or_default(),
            has_image: image.is_some(),
            failed: st.textures.failed.contains(&index),
            label: st
                .session
                .labels
                .get(index)
                .cloned()
                .unwrap_or_default()
                .into(),
            is_cursor: index == st.grid.cursor,
            selected: st.grid.selection.is_selected(index),
            id: index as i32,
            copied: st.copy.copies.is_copied(index),
            burst_count: st.bursts.badge.get(index).copied().unwrap_or(0) as i32,
            seed: if st.session.synthetic {
                index as i32
            } else {
                -1
            },
            // At the loupe (N=1) the #20 badge pill owns state display:
            // bare cells keep the grid's 40% reject dim out of the loupe
            // (a reject may be re-judged for rescue at full brightness)
            // and stop the cell glyph double-rendering under the pill.
            pick: if at_loupe {
                0
            } else {
                match st.session.picks.get(index) {
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
}

/// PHASE 8 — the chrome: filter bar counts and labels, the empty-state
/// message, the IPTC panel, the scroll hint and the status line. Nothing
/// here decides anything; it is the pass's findings, in words.
fn write_status_and_chrome(
    win: &MainWindow,
    st: &mut AppState,
    pass: &Pass,
    range: Range<usize>,
    cursor: usize,
    cursor_mark: i32,
) {
    let layout = &pass.layout;
    let view_len = pass.view_len;
    // Filter bar + status (M5): live counts, active chip, sort label,
    // inbox-zero empty state (persona G2).
    let counts = fastcull_core::filter::counts(&st.session.picks);
    win.set_counts_all(counts.all as i32);
    win.set_counts_picked(counts.picked as i32);
    win.set_counts_rejected(counts.rejected as i32);
    win.set_counts_unmarked(counts.unmarked as i32);
    win.set_filter_bar_visible(st.grid.filter_bar_visible);
    use fastcull_core::filter::{PickFilter, SortKey};
    win.set_active_filter(
        match st.grid.query.filter {
            PickFilter::All => "all",
            PickFilter::Picked => "picked",
            PickFilter::Rejected => "rejected",
            PickFilter::Unmarked => "unmarked",
        }
        .into(),
    );
    win.set_sort_label(
        match (st.grid.query.sort, st.grid.query.ascending) {
            (SortKey::CaptureTime, true) => "Capture ↑",
            (SortKey::CaptureTime, false) => "Capture ↓",
            (SortKey::Filename, true) => "Name ↑",
            (SortKey::Filename, false) => "Name ↓",
        }
        .into(),
    );
    let count = st.count();
    win.set_empty_message(if view_len == 0 && count > 0 {
        let what = match st.grid.query.filter {
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
    } else if count == 0 && !st.session.session_open {
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
    let showing = if st.grid.query.filter == PickFilter::All {
        String::new()
    } else {
        format!(" — showing {view_len} of {count}")
    };
    refresh_iptc_panel(win, st);
    win.set_scroll_hint(
        if st.grid.view.is_empty() {
            String::new()
        } else {
            // "795 / 1450 · 15:42" — capture time appended when sorting by
            // capture (persona: a day is navigated by light, not by frame
            // numbers); filename sorts get numbers only.
            let mut hint = format!("{} / {}", range.start + 1, st.grid.view.len());
            if st.grid.query.sort == fastcull_core::filter::SortKey::CaptureTime {
                if let Some(key) = st
                    .grid
                    .view
                    .get(range.start)
                    .and_then(|id| st.session.capture_keys.get(*id))
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
        .bursts
        .pos
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
    let sel_note = match st.grid.selection.count_in_view(&st.grid.view) {
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
    let loaded = st.session.thumbs_done.min(count);
    let load_note = if st.metadata_complete() {
        format!("{loaded} thumbs loaded")
    } else {
        format!("{loaded}/{count} loaded · sorting by name until loaded")
    };
    // Is there anything to export as video right now? The menu item's
    // enabled state follows this; the KEY works either way and explains
    // itself below (video-export.md: never a silent grey item).
    // The CHEAP count (core's `scope_len`), not the list: this runs on
    // every repaint, and building the list means scanning the whole
    // session's burst grouping for a number the status line has already
    // computed. The selection count is `sel_note`'s own; the burst size
    // is the badge's own.
    // A synthetic session (`--synthetic`, the render benchmark) has cells
    // but no files behind them, so there is nothing to export from it —
    // and without this the menu item would offer an export whose dialog
    // then says there is nothing to export.
    let clip_frames = if st.session.paths.is_empty() {
        0
    } else {
        fastcull_core::clip::scope_len(
            st.grid.selection.count_in_view(&st.grid.view),
            st.bursts
                .pos
                .get(cursor)
                .copied()
                .flatten()
                .map_or(0, |(_, n)| n),
        )
    };
    win.set_clip_available(fastcull_core::clip::unavailable_reason(clip_frames).is_none());
    // A refused export explains itself HERE, where the user is already
    // looking after a keystroke that appeared to do nothing. It expires
    // on its own (the pump drops it and asks for this repaint).
    // ...and it disappears the moment it stops being true: selecting the
    // frames the message asked for must not leave the complaint on
    // screen (found driving the real app, 2026-08-27).
    let clip_notice = match &st.clip.notice {
        Some((text, at))
            if at.elapsed() < crate::state::CLIP_NOTICE_LIFE && !win.get_clip_available() =>
        {
            format!(" — {text}")
        }
        _ => String::new(),
    };
    win.set_status(
        format!(
            "{} ({}/{}){}{}{}{} — {} — ★{} ✕{}{} — {} column{}{}",
            if cursor_in_view {
                st.session.labels.get(cursor).cloned().unwrap_or_default()
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
            if st.session.sidecar_failures > 0 {
                format!(" — ⚠{} sidecar write failures", st.session.sidecar_failures)
            } else {
                String::new()
            },
            layout.columns,
            if layout.columns == 1 { "" } else { "s" },
            clip_notice
        )
        .into(),
    );
}

fn refresh_inner(win: &MainWindow, state: &Rc<RefCell<AppState>>) {
    let geom = current_geometry(win, state);
    let mut st = state.borrow_mut();
    // One borrow, eight phases, in this order — each named after what it
    // decides. Everything they hand each other travels in `Pass` or in
    // these locals; nothing is smuggled through AppState.
    let mut pass = detect_drift(win, &mut st, geom);
    anchor_the_scroll(win, &mut st, &mut pass);
    let (range, ids) = visible_window(&mut st, &pass);
    claim_cursor_at_loupe(win, state, &mut st, &pass);
    let want_mid = climb_mid_rung(win, &mut st, &pass, &ids);
    let cursor_mark = render_loupe_rung(win, &mut st, &pass);
    // Read AFTER the claim phase, the only thing in the pass that can move
    // the cursor: the cells and the status line must name the same image
    // the rung choice just rendered.
    let cursor = st.grid.cursor;
    fill_grid_cells(win, &st, &pass, range.clone(), want_mid);
    write_status_and_chrome(win, &mut st, &pass, range, cursor, cursor_mark);
}
