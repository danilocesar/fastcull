//! Transit policy: WHICH rung the loupe shows, whether the "◌ loading" cue
//! pill is lit, and which texture the rings give up — as pure decision
//! functions.
//!
//! The render ladder (`specs/modules/ui-grid.md`, "The render ladder", as
//! revised by issues #21, #46 and #60) is: full-res (sharp) → the cursor's
//! SCREEN RUNG (soft above fit: the fit-size decode) → the cursor's mid rung
//! (soft) → the cursor's own 320 px grid THUMB (soft) → a bounded residual
//! HOLD of the previous image's pixels → the honest drop to fit. Two bounds
//! guard the hold: a decode FAILURE of the cursor image drops immediately
//! (the strip owns the failed badge), and a cap (`OVERLAY_HOLD_CAP` in the
//! app, `hold_cap` here) ends a wedged decode's hold. At FIT the fit cell
//! shows the best rung in hand ([`fit_cell`]) and the pill flags it whenever
//! it does not serve the fit box (brief 008 R8: "never show upscaled pixels
//! UNFLAGGED" holds at fit too); while travelling a lit pill holds for a
//! minimum on-time ([`cue_pill`]).
//!
//! This module owns that ladder as [`render_rung`], the fit cell's one order
//! as [`fit_cell`], the pill as [`cue_pill`], the texture rings' victim choice
//! as [`evict_ring`], and the order the kitchen cooks its queued full-res
//! fills in as [`next_fill`]. It speaks in rungs, holds and decisions only —
//! never textures, properties or Slint (01-architecture.md: if a piece of code
//! can live in `fastcull-core`, it must). The app gathers the plain-data
//! inputs (texture lookups, the clock read, the zoom factor, the engine's
//! windows and travel time), calls in, and does the property writes its
//! answer names.
//!
//! **Why it lives here** (ui-grid.md's own recorded deferral, gate
//! 2026-08-09): every #46-class bug so far lived exactly in untestable
//! app-side state. `elapsed` and `now` are INPUTS rather than clock reads,
//! which is what makes the cap and the pill's minimum testable as tables
//! instead of stopwatches.

use std::fmt;
use std::time::{Duration, Instant};

use crate::loupe::RingWindow;

/// Where the loupe is — the first thing the render decision asks
/// (ui-grid.md, "The render ladder"): off it, at its fit view, or above fit
/// in the zoom overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoupeWhere {
    /// Not at the loupe (a grid zoom), or at the loupe's fit view with an
    /// empty view: nothing on the ladder applies, and there is no cursor to
    /// cue.
    Off,
    /// The loupe's fit view: the fit cell shows the best rung in hand and
    /// the cue decides whether it is flagged.
    Fit,
    /// Any factor above fit: the zoom overlay and its ladder.
    AboveFit,
}

/// Where the loupe is, from the three facts the app has: whether the view
/// is at one column, the factor it renders (the desire clamped to the known
/// 1:1 ceiling — an unresolved 1:1 pin is infinite, so above fit), and how
/// many images the view holds. Above fit the factor decides first, as the
/// overlay's own `factor > 1.0 && at_loupe` always did; at fit an empty view
/// is Off, there being no cursor to cue (the app's `at-fit` surface is down
/// there as well).
pub fn loupe_where(at_loupe: bool, factor: f32, view_len: usize) -> LoupeWhere {
    if !at_loupe {
        LoupeWhere::Off
    } else if factor > 1.0 {
        LoupeWhere::AboveFit
    } else if view_len == 0 {
        LoupeWhere::Off
    } else {
        LoupeWhere::Fit
    }
}

/// What the loupe should do this refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderDecision {
    /// Render the cursor's TOP rung: sharp, no cue pill.
    Sharp,
    /// Render the cursor's SCREEN RUNG above fit, at the carried factor and
    /// pan centre, flagged by the cue pill: it is the fit-size decode, so any
    /// factor above fit upscales it (ui-grid.md, the ladder above fit).
    Rung,
    /// Render a sub-top rung of the cursor's OWN image at the carried
    /// factor and pan centre, flagged by the cue pill. `is_thumb`
    /// distinguishes the mid rung from the 320 px grid-thumb rescue —
    /// same extent math, different source and different trace line.
    Soft { is_thumb: bool },
    /// Keep the PREVIOUS image's pixels at the carried geometry (residual
    /// HOLD). `start` is true on the refresh that BEGINS a hold for this
    /// cursor image — the app then stamps the hold's clock. It is false
    /// while a hold for the same cursor continues, so the cap measures one
    /// photograph's misrepresentation, not the pixels' total tenure.
    Hold { start: bool },
    /// Take the overlay down to the fit view.
    Drop { reason: DropReason },
    /// The loupe is at fit: the fit cell shows the best rung in hand
    /// ([`fit_cell`]), and `cue` says that rung does not serve the fit box,
    /// so the pill must flag it (ui-grid.md, "At fit"; brief 008 R8).
    Fit { cue: bool },
}

/// Why the overlay came down. The two *traced* reasons are the ones that
/// excuse a drop while the desire is still above fit — an unexcused drop
/// there is the M1 fit-flash the transit contract outlaws, which is why the
/// trace distinguishes them and the regression tests grep for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The loupe is not on screen (a grid zoom, or an empty view): the
    /// overlay simply does not apply. Not traced — nothing was lost.
    BelowLadder,
    /// The cursor image's decode FAILED. Traced `(decode failed)`.
    DecodeFailed,
    /// A residual hold outlived `hold_cap`. Traced `(hold cap)`; the
    /// overlay re-raises the moment any rung of the cursor image lands.
    HoldCap,
    /// A cold ENTRY into zoom: the overlay was not up and there are no
    /// pixels of this image to hold. Not traced — the overlay stays down
    /// until the first rung lands (the pre-existing honest behavior).
    NothingToHold,
}

/// An in-flight residual hold, as the decision needs to see it: whether it
/// belongs to the CURSOR image, and how long it has run.
///
/// The elapsed time is passed in rather than read here so the cap is a
/// table row instead of a stopwatch. `same_cursor` is false while a hold
/// stamped for the previous image is still recorded — the case that
/// re-times the cap on a hold-arrow run across cold frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldState {
    pub same_cursor: bool,
    pub elapsed: Duration,
}

/// Everything the ladder decides from — plain data the app gathers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RungInputs {
    /// A TOP-rung texture for the cursor is in hand (`loupe::is_top_rung`
    /// over the full-res slot, terminality included): the file's best,
    /// sharp at every factor.
    pub has_sharp: bool,
    /// The cursor's SCREEN RUNG texture is in hand — the embedded full JPEG
    /// decoded at N/8 to the fit box (brief 008).
    pub has_rung: bool,
    /// SOME texture of the cursor's own image below the sharp arm is in
    /// hand: its mid rung, or a warm texture the engine re-announced into
    /// the full-res slot (the pruned-and-revisited path). The app gathers
    /// it as "mid rung, else full-res slot", so when `has_sharp` is also
    /// true this may be that same TOP-rung texture — harmless, because the
    /// ladder tests `has_sharp` first and `has_mid` is only ever read below
    /// it. Do not read this field as "strictly sub-top".
    pub has_mid: bool,
    /// The cursor's own 320 px grid thumb is in hand.
    pub has_thumb: bool,
    /// The screen rung in hand serves the CURRENT fit box
    /// (`loupe::serves_box`, the 1.25 rule's one home): one cached for a
    /// smaller display does not. False with no box.
    pub rung_serves_fit: bool,
    /// The mid in hand serves the current fit box: on viewports up to ~2K,
    /// not on a wide one. False with no box.
    pub mid_serves_fit: bool,
    /// The cursor image's decode has FAILED (the strip shows its badge).
    pub cursor_failed: bool,
    /// Where the loupe is ([`loupe_where`]).
    pub loupe: LoupeWhere,
    /// The overlay was up on the PREVIOUS refresh: there are previous
    /// pixels on screen that a hold would be keeping.
    pub overlay_was_up: bool,
    /// The hold recorded by an earlier refresh, if any.
    pub hold: Option<HoldState>,
    /// Longest one photograph may be misrepresented by a hold.
    pub hold_cap: Duration,
}

/// The render ladder, as one total function: every combination of inputs
/// yields a decision (`render_rung_is_total_and_reaches_every_decision`
/// sweeps them).
///
/// Off the loupe nothing applies. At fit the answer is always the fit view,
/// cued unless a rung in hand serves the fit box — the full-res (or a
/// terminal rung, which the app gathers as sharp), a screen rung or a mid
/// that serves it — or the cursor has failed (the strip owns the badge).
/// "Serves" is the ladder's 1.25 tolerance, so a ≤ 25 % upscale is
/// unflagged at fit as everywhere on the ladder (Manager M2, 2026-09-26).
///
/// Above fit the order is the ladder's own, top rung first. Two rules are
/// easy to state backwards and are therefore spelled out here:
///
/// * the thumb RESCUE is skipped for a failed cursor image — a file whose
///   320 px thumb survived while every loupe rung is corrupt would sit at
///   1:1 behind a "loading" pill that can never complete, hiding the
///   strip's failed badge (validator finding, #46). One transient is
///   causally unavoidable and accepted: the first focus of a freshly dead
///   file MAY render its thumb for the milliseconds until the decode
///   attempt fails, because the failure does not exist as knowledge yet.
///   Accepted, not required — at the app level either order can win (the
///   thumb texture or the failure, issue #50); here the rule is simply
///   `cursor_failed: false`, and the gate binds from the Failed event on.
/// * the cap is PER CURSOR IMAGE: a hold stamped for a different image
///   does not cap this one, it re-starts (`Hold { start: true }`). So the
///   same stale pixels can exceed the cap in aggregate across a hold-arrow
///   run over consecutively cold frames — the bound is on how long any ONE
///   photograph can be misrepresented (recorded in ui-grid.md).
pub fn render_rung(i: &RungInputs) -> RenderDecision {
    match i.loupe {
        LoupeWhere::Off => {
            return RenderDecision::Drop {
                reason: DropReason::BelowLadder,
            };
        }
        LoupeWhere::Fit => {
            let served =
                i.has_sharp || (i.has_rung && i.rung_serves_fit) || (i.has_mid && i.mid_serves_fit);
            return RenderDecision::Fit {
                cue: !(served || i.cursor_failed),
            };
        }
        LoupeWhere::AboveFit => {}
    }
    if i.has_sharp {
        return RenderDecision::Sharp;
    }
    if i.has_rung {
        return RenderDecision::Rung;
    }
    if i.has_mid {
        return RenderDecision::Soft { is_thumb: false };
    }
    if i.has_thumb && !i.cursor_failed {
        return RenderDecision::Soft { is_thumb: true };
    }
    // No rung of the cursor's own image. Hold the previous pixels, unless
    // one of the two bounds forbids it.
    let capped = i
        .hold
        .is_some_and(|h| h.same_cursor && h.elapsed >= i.hold_cap);
    if i.overlay_was_up && !i.cursor_failed && !capped {
        return RenderDecision::Hold {
            start: i.hold.is_none_or(|h| !h.same_cursor),
        };
    }
    RenderDecision::Drop {
        reason: if !i.overlay_was_up {
            // Nothing on screen to keep: no drop happened, the overlay
            // simply never came up for this image.
            DropReason::NothingToHold
        } else if i.cursor_failed {
            DropReason::DecodeFailed
        } else {
            // The only remaining way past the hold arm.
            DropReason::HoldCap
        },
    }
}

/// Which rung the fit cell draws — and the rung its trace mark names
/// (test-harness.md, `loupe fit idx N rung K`; the dump's `rung=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitRung {
    None,
    Thumb,
    Mid,
    Screen,
    Full,
}

impl fmt::Display for FitRung {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The words are a contract: driven tests wait on and read them.
        f.write_str(match self {
            FitRung::None => "none",
            FitRung::Thumb => "thumb",
            FitRung::Mid => "mid",
            FitRung::Screen => "screen",
            FitRung::Full => "full",
        })
    }
}

/// The texture the fit cell DRAWS and the rung its mark NAMES, from one
/// order — full > screen > mid > thumb (ui-grid.md, "At fit": "the fit cell
/// shows the best rung in hand — full-res, then the screen rung, the mid, the
/// thumb"). The app gathers the four textures it holds for an index and draws
/// exactly what this returns, so the mark names the drawn texture by
/// construction: an order of the app's own beside this one could draw the 2×
/// mid while every gate read `rung screen` (brief 008's plan, fix round 3).
/// Generic so core owns the order without knowing `slint::Image`.
pub fn fit_cell<T>(
    full: Option<T>,
    rung: Option<T>,
    mid: Option<T>,
    thumb: Option<T>,
) -> (FitRung, Option<T>) {
    match (full, rung, mid, thumb) {
        (Some(t), _, _, _) => (FitRung::Full, Some(t)),
        (None, Some(t), _, _) => (FitRung::Screen, Some(t)),
        (None, None, Some(t), _) => (FitRung::Mid, Some(t)),
        (None, None, None, Some(t)) => (FitRung::Thumb, Some(t)),
        (None, None, None, None) => (FitRung::None, None),
    }
}

/// [`fit_cell`]'s label over presence alone, for the decision's inputs —
/// one definition, so the two cannot disagree.
pub fn fit_rung_shown(i: &RungInputs) -> FitRung {
    fit_cell(
        i.has_sharp.then_some(()),
        i.has_rung.then_some(()),
        i.has_mid.then_some(()),
        i.has_thumb.then_some(()),
    )
    .0
}

/// The "◌ loading" pill for one render, and the clock the app keeps for the
/// next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CuePill {
    /// The pill is lit.
    pub on: bool,
    /// Re-evaluate the pill after this long although nothing may land. Set
    /// ONLY while the pill is lit over a frame that is not soft — held by the
    /// minimum while travelling — to the earlier of the minimum's end and the
    /// end of travel, the two instants at which the held pill must clear.
    /// None while the pill is off or the frame is soft: whatever makes a soft
    /// frame sharp is a landing, which refreshes anyway. Without it a pill
    /// held over a sharp frame at a hold's last refresh stays lit at rest,
    /// where nothing may land to clear it (brief 008's plan, fix round 2).
    pub recheck_in: Option<Duration>,
    /// The last soft frame on screen, which the minimum counts from: the app
    /// keeps it and hands it back at its next render. Forgotten for a failed
    /// cursor, whose pill is off.
    pub last_soft: Option<Instant>,
}

/// The pill (ui-grid.md, "The pill never flickers"; the pill rule, Manager
/// M2 on the persona's redesign check, 2026-09-26), decided whole here so the
/// re-check can never disagree with the pill and no rule of it lives in the
/// app:
/// * a soft frame lights it, however brief — a minimum on-time errs toward
///   flagging a sharp frame, never toward hiding a soft one;
/// * while travelling (`travel_left` is `Some`, the engine's `travel_left()`)
///   a lit pill stays on until `min_on` has passed since the LAST soft frame,
///   so soft and sharp frames alternating in a hold keep it lit rather than
///   blinking — counted from the last soft frame, not from the lighting, so a
///   second soft frame restarts the minimum (Manager ruling 2026-09-26, brief
///   008 Q-B);
/// * not travelling, a sharp frame clears it at once ("it clears the moment a
///   sharp frame is on screen after the key is released");
/// * the minimum never holds it for a failed cursor: the strip owns the
///   failed badge, and "◌ loading" must not linger over it — the clock is
///   forgotten there. A failed cursor's own soft pixels (its mid or screen
///   rung above fit) still light it, as any soft frame does: they are
///   upscaled pixels, which are never shown unflagged (ui-grid.md, the
///   quality rule), and at fit the cue itself is off for a failed cursor.
///
/// `soft` is whether the frame now on screen is below what the view needs
/// (the app's per-decision value), `last_soft` the clock the previous call
/// returned, `now` this render's instant.
pub fn cue_pill(
    soft: bool,
    cursor_failed: bool,
    travel_left: Option<Duration>,
    last_soft: Option<Instant>,
    now: Instant,
    min_on: Duration,
) -> CuePill {
    if soft {
        return CuePill {
            on: true,
            recheck_in: None,
            last_soft: Some(now),
        };
    }
    if cursor_failed {
        return CuePill {
            on: false,
            recheck_in: None,
            last_soft: None,
        };
    }
    let since_last_soft = last_soft.map(|t| now.saturating_duration_since(t));
    match (travel_left, since_last_soft) {
        (Some(left), Some(since)) if since < min_on => CuePill {
            on: true,
            recheck_in: Some((min_on - since).min(left)),
            last_soft,
        },
        _ => CuePill {
            on: false,
            recheck_in: None,
            last_soft,
        },
    }
}

/// Which slot of a texture ring to give up, or `None` while the ring holds
/// no more than its `window`'s capacity (ui-grid.md, "The render ladder").
///
/// `held` is the image ids in slot order (most recently inserted last),
/// `view` the current view order, and `window` the ring's window around the
/// cursor, LEANED by the engine's travel latch
/// (`LoupeEngine::texture_windows`, never re-derived here or in the app).
/// Eviction is by VIEW distance from the cursor, not insertion age (issue
/// #46): age is view-order-blind — the provisional-order startup window
/// legitimately decodes filename-order neighbors, and once the capture sort
/// lands those are strangers occupying slots; age eviction then discarded
/// exactly the view neighbor the next tap needed while keeping a frame seven
/// positions away (observed as an 81 ms thumb blink on a warm frame).
///
/// Four rules the caller must not re-derive:
/// * the CURSOR's own texture is never the victim (it is what the user is
///   looking at; a prefetch evicting it was seen as back-arrow quality
///   degradation);
/// * an entry no longer in the view (or any entry when the cursor itself
///   is not in the view) is at maximum distance and goes first;
/// * then any entry OUTSIDE the window goes before any entry inside it, the
///   farthest first within each class — a symmetric distance cannot hold an
///   asymmetric ring: after a forward hold it kept the frames just passed
///   and evicted the runway's far end as it landed (brief 008). With a
///   symmetric window the rule is plain distance eviction;
/// * on a TIE the LATER slot goes — the freshly inserted texture loses to
///   an equally distant older one, which is what keeps a back-and-forth
///   walk from thrashing the neighbor it just came from.
///
/// The capacity is the window's size, so a ring holds its whole window, and
/// a landing outside the window is kept while the ring has room and is the
/// first victim when it has none.
pub fn evict_ring(
    held: &[usize],
    cursor: usize,
    view: &[usize],
    window: RingWindow,
) -> Option<usize> {
    if held.len() <= window.capacity() {
        return None;
    }
    let pos_of = |id: usize| view.iter().position(|v| *v == id);
    let cursor_pos = pos_of(cursor);
    Some(
        held.iter()
            .enumerate()
            .filter(|(_, id)| **id != cursor)
            .max_by_key(|(_, id)| match (cursor_pos, pos_of(**id)) {
                (Some(c), Some(p)) => (!window.contains(c, p), p.abs_diff(c)),
                // Not in the view (or no view): first out.
                _ => (true, usize::MAX),
            })
            .map(|(slot, _)| slot)
            // Only reachable if every slot holds the cursor, which the
            // caller's dedupe makes impossible — stay total anyway.
            .unwrap_or(0),
    )
}

/// Which of the kitchen's queued full-res fills to cook next — the slot in
/// `queued` (image ids in queue order) — or `None` when nothing is queued
/// (01-architecture.md, the kitchen; ui-grid.md, "The render ladder"): the
/// mirror of [`evict_ring`]'s victim rule. The cursor's fill first; then
/// fills inside `window` (the full-res texture window, leaned by the
/// engine's latch) by view distance from the cursor, at equal distance the
/// one toward the window's lean first (`after > before` leans forward, and a
/// symmetric window reads forward); then fills outside the window, by
/// distance; fills for images out of the view (or all of them when the
/// cursor has left it) last; first queued first among equals. So the member
/// a tap reaches first is never cooked last, which the kitchen's old
/// latest-first pop did to the nearest member of a ring fifteen deep.
pub fn next_fill(
    queued: &[usize],
    cursor: usize,
    view: &[usize],
    window: RingWindow,
) -> Option<usize> {
    let pos_of = |id: usize| view.iter().position(|v| *v == id);
    let cursor_pos = pos_of(cursor);
    let forward = window.after >= window.before;
    queued
        .iter()
        .enumerate()
        // `min_by_key` returns the FIRST minimum: first queued among equals.
        .min_by_key(|(_, id)| {
            if **id == cursor {
                return (0, 0, false);
            }
            match (cursor_pos, pos_of(**id)) {
                (Some(c), Some(p)) => {
                    let class = if window.contains(c, p) { 1 } else { 2 };
                    let against_lean = if forward { p < c } else { p > c };
                    (class, p.abs_diff(c), against_lean)
                }
                _ => (3, 0, false),
            }
        })
        .map(|(slot, _)| slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loupe::PREFETCH;

    const CAP: Duration = Duration::from_millis(250);

    /// The symmetric ±`PREFETCH` window — the app's full-res texture ring
    /// before brief 008, under which every eviction row below was written and
    /// keeps its value: with a symmetric window the rule is plain distance.
    const SYMMETRIC: RingWindow = RingWindow::symmetric(PREFETCH);

    /// The inputs, with everything absent and the loupe above fit: rows
    /// name only what they are about.
    fn cold() -> RungInputs {
        RungInputs {
            has_sharp: false,
            has_rung: false,
            has_mid: false,
            has_thumb: false,
            rung_serves_fit: false,
            mid_serves_fit: false,
            cursor_failed: false,
            loupe: LoupeWhere::AboveFit,
            overlay_was_up: false,
            hold: None,
            hold_cap: CAP,
        }
    }

    fn held_for_this_cursor(elapsed_ms: u64) -> Option<HoldState> {
        Some(HoldState {
            same_cursor: true,
            elapsed: Duration::from_millis(elapsed_ms),
        })
    }

    fn held_for_the_previous_image(elapsed_ms: u64) -> Option<HoldState> {
        Some(HoldState {
            same_cursor: false,
            elapsed: Duration::from_millis(elapsed_ms),
        })
    }

    // ---------------------------------------------------------------
    // The ladder, rung by rung — each row is a sentence of ui-grid.md
    // with its expected decision written out, not computed.
    // ---------------------------------------------------------------

    #[test]
    fn the_top_rung_renders_sharp() {
        let i = RungInputs {
            has_sharp: true,
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Sharp);
        // ...and it wins over every lower rung, and over a running hold.
        let i = RungInputs {
            has_sharp: true,
            has_mid: true,
            has_thumb: true,
            overlay_was_up: true,
            hold: held_for_this_cursor(10),
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Sharp);
    }

    #[test]
    fn a_failed_decode_does_not_veto_a_sharp_texture_in_hand() {
        // The failure gate guards the THUMB rescue and the hold, not a
        // real rung: pixels of the cursor's own image at top-rung size are
        // the truth whatever a later decode attempt said.
        let i = RungInputs {
            has_sharp: true,
            cursor_failed: true,
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Sharp);
        let i = RungInputs {
            has_mid: true,
            cursor_failed: true,
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Soft { is_thumb: false });
    }

    #[test]
    fn below_the_top_rung_the_mid_renders_soft() {
        let i = RungInputs {
            has_mid: true,
            has_thumb: true,
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Soft { is_thumb: false });
    }

    #[test]
    fn below_the_mid_the_cursors_own_thumb_is_the_rescue() {
        let i = RungInputs {
            has_thumb: true,
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Soft { is_thumb: true });
    }

    #[test]
    fn a_failed_cursor_skips_the_thumb_rescue_and_drops_honestly() {
        // The recorded reason: a live 320 px thumb with every loupe rung
        // corrupt would sit at 1:1 behind a pill that can never complete,
        // hiding the strip's failed badge.
        let i = RungInputs {
            has_thumb: true,
            cursor_failed: true,
            overlay_was_up: true,
            ..cold()
        };
        assert_eq!(
            render_rung(&i),
            RenderDecision::Drop {
                reason: DropReason::DecodeFailed
            }
        );
    }

    #[test]
    fn the_causally_unavoidable_thumb_transient_is_preserved() {
        // The first focus of a freshly dead file: its thumb reached memory
        // before the file died, and the decode attempt has not failed YET.
        // The recorded residual is that the thumb DOES render here — the
        // gate binds from the Failed event on, not before it exists.
        let before_the_failure_is_known = RungInputs {
            has_thumb: true,
            cursor_failed: false,
            overlay_was_up: true,
            ..cold()
        };
        assert_eq!(
            render_rung(&before_the_failure_is_known),
            RenderDecision::Soft { is_thumb: true }
        );
        // ...and the very next refresh, once the Failed event landed:
        let after = RungInputs {
            cursor_failed: true,
            ..before_the_failure_is_known
        };
        assert_eq!(
            render_rung(&after),
            RenderDecision::Drop {
                reason: DropReason::DecodeFailed
            }
        );
    }

    // ---------------------------------------------------------------
    // The residual hold and its two bounds.
    // ---------------------------------------------------------------

    #[test]
    fn no_rung_at_all_holds_the_previous_pixels() {
        let i = RungInputs {
            overlay_was_up: true,
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Hold { start: true });
    }

    #[test]
    fn a_running_hold_for_the_same_cursor_continues_without_restamping() {
        let i = RungInputs {
            overlay_was_up: true,
            hold: held_for_this_cursor(100),
            ..cold()
        };
        assert_eq!(render_rung(&i), RenderDecision::Hold { start: false });
    }

    #[test]
    fn the_cap_ends_a_wedged_hold() {
        let at_the_cap = RungInputs {
            overlay_was_up: true,
            hold: held_for_this_cursor(250),
            ..cold()
        };
        assert_eq!(
            render_rung(&at_the_cap),
            RenderDecision::Drop {
                reason: DropReason::HoldCap
            }
        );
        // The boundary is inclusive on the drop side (`elapsed >= cap`):
        // one millisecond less still holds.
        let just_inside = RungInputs {
            hold: held_for_this_cursor(249),
            ..at_the_cap
        };
        assert_eq!(
            render_rung(&just_inside),
            RenderDecision::Hold { start: false }
        );
    }

    #[test]
    fn the_cap_re_times_at_each_cursor_change() {
        // The recorded residual: a hold-arrow run across consecutively
        // cold frames re-stamps the cap at every cursor change, so the
        // stale pixels' TOTAL tenure can exceed the cap — the bound is on
        // how long any ONE photograph can be misrepresented.
        let long_past_the_cap_but_for_the_previous_image = RungInputs {
            overlay_was_up: true,
            hold: held_for_the_previous_image(10_000),
            ..cold()
        };
        assert_eq!(
            render_rung(&long_past_the_cap_but_for_the_previous_image),
            RenderDecision::Hold { start: true }
        );
    }

    #[test]
    fn a_failed_cursor_ends_the_hold_immediately_however_young() {
        let i = RungInputs {
            cursor_failed: true,
            overlay_was_up: true,
            hold: held_for_this_cursor(1),
            ..cold()
        };
        assert_eq!(
            render_rung(&i),
            RenderDecision::Drop {
                reason: DropReason::DecodeFailed
            }
        );
    }

    #[test]
    fn failure_outranks_the_cap_in_the_traced_reason() {
        // Both bounds true at once: the strip's badge is the honest
        // explanation, so `(decode failed)` is what the trace must say.
        let i = RungInputs {
            cursor_failed: true,
            overlay_was_up: true,
            hold: held_for_this_cursor(9_999),
            ..cold()
        };
        assert_eq!(
            render_rung(&i),
            RenderDecision::Drop {
                reason: DropReason::DecodeFailed
            }
        );
    }

    #[test]
    fn a_cold_entry_with_nothing_to_hold_keeps_the_overlay_down() {
        // The overlay was NOT up: there are no previous pixels on screen,
        // so nothing is being kept and nothing was lost — untraced.
        let i = RungInputs {
            overlay_was_up: false,
            ..cold()
        };
        assert_eq!(
            render_rung(&i),
            RenderDecision::Drop {
                reason: DropReason::NothingToHold
            }
        );
        // Same with a failed cursor: still nothing on screen to lose, so
        // the drop is the untraced kind (the strip owns the badge).
        let i = RungInputs {
            cursor_failed: true,
            ..i
        };
        assert_eq!(
            render_rung(&i),
            RenderDecision::Drop {
                reason: DropReason::NothingToHold
            }
        );
    }

    #[test]
    fn the_overlay_re_raises_the_moment_any_rung_lands() {
        // After a capped drop the hold record is cleared by the app; what
        // matters for policy is that a rung in hand outranks any memory of
        // the cap — the same inputs that dropped now render.
        let capped = RungInputs {
            overlay_was_up: true,
            hold: held_for_this_cursor(400),
            ..cold()
        };
        assert_eq!(
            render_rung(&capped),
            RenderDecision::Drop {
                reason: DropReason::HoldCap
            }
        );
        assert_eq!(
            render_rung(&RungInputs {
                has_thumb: true,
                ..capped
            }),
            RenderDecision::Soft { is_thumb: true }
        );
        assert_eq!(
            render_rung(&RungInputs {
                has_mid: true,
                ..capped
            }),
            RenderDecision::Soft { is_thumb: false }
        );
        assert_eq!(
            render_rung(&RungInputs {
                has_sharp: true,
                ..capped
            }),
            RenderDecision::Sharp
        );
    }

    /// Renamed from `at_or_below_fit_nothing_on_the_ladder_applies`, whose
    /// promise — nothing applies at or below fit — brief 008 changes: at fit
    /// the cue applies (ui-grid.md, "At fit"). Every input off the loupe is
    /// the untraced drop; every input at fit is the fit view, whatever rungs,
    /// hold or failure it carries.
    #[test]
    fn off_the_loupe_nothing_applies_and_at_fit_only_the_cue_does() {
        let (mut off, mut fit) = (0, 0);
        for i in every_input_combination() {
            match i.loupe {
                LoupeWhere::Off => {
                    off += 1;
                    assert_eq!(
                        render_rung(&i),
                        RenderDecision::Drop {
                            reason: DropReason::BelowLadder
                        },
                        "off the loupe: {i:?}"
                    );
                }
                LoupeWhere::Fit => {
                    fit += 1;
                    assert!(
                        matches!(render_rung(&i), RenderDecision::Fit { .. }),
                        "at fit only the cue applies: {i:?}"
                    );
                }
                LoupeWhere::AboveFit => {}
            }
        }
        assert_eq!(
            (off, fit),
            (1280, 1280),
            "each position is a third of the sweep"
        );
    }

    /// Brief 008 A3 (ui-grid.md, "At fit"), the rows written out: the pill
    /// shows whenever the rung the fit cell shows does not serve the fit box.
    #[test]
    fn at_fit_the_cue_follows_the_serving_rung() {
        let fit = RungInputs {
            loupe: LoupeWhere::Fit,
            ..cold()
        };
        // A screen rung that serves the box: the frame at the size the
        // screen draws it — no cue.
        assert_eq!(
            render_rung(&RungInputs {
                has_rung: true,
                rung_serves_fit: true,
                has_mid: true,
                has_thumb: true,
                ..fit
            }),
            RenderDecision::Fit { cue: false }
        );
        // THE G6 ROW: the mid alone on a box it does not serve (a 4K
        // viewport) — before brief 008 this was the 2× upscaled mid, unflagged.
        assert_eq!(
            render_rung(&RungInputs {
                has_mid: true,
                has_thumb: true,
                ..fit
            }),
            RenderDecision::Fit { cue: true },
            "an upscaled mid at fit must be flagged"
        );
        // The mid alone on a box it serves (~2K and below): nothing changes
        // there — no cue.
        assert_eq!(
            render_rung(&RungInputs {
                has_mid: true,
                mid_serves_fit: true,
                has_thumb: true,
                ..fit
            }),
            RenderDecision::Fit { cue: false }
        );
        // Nothing in hand yet — a cold frame's placeholder: cued.
        assert_eq!(render_rung(&fit), RenderDecision::Fit { cue: true });
        // A failed cursor: never cued — the strip owns the failed badge.
        for i in [
            RungInputs {
                cursor_failed: true,
                ..fit
            },
            RungInputs {
                cursor_failed: true,
                has_thumb: true,
                has_mid: true,
                ..fit
            },
        ] {
            assert_eq!(render_rung(&i), RenderDecision::Fit { cue: false }, "{i:?}");
        }
        // A screen rung cached for a smaller display (the window moved to a
        // bigger screen): a soft rung, cued — never presented as sharp.
        assert_eq!(
            render_rung(&RungInputs {
                has_rung: true,
                rung_serves_fit: false,
                has_mid: true,
                has_thumb: true,
                ..fit
            }),
            RenderDecision::Fit { cue: true }
        );
        // The full-res — or a terminal rung, the file's best, which the app
        // gathers as sharp: never cued, over any other rung.
        for i in [
            RungInputs {
                has_sharp: true,
                ..fit
            },
            RungInputs {
                has_sharp: true,
                has_rung: true,
                has_mid: true,
                has_thumb: true,
                ..fit
            },
        ] {
            assert_eq!(render_rung(&i), RenderDecision::Fit { cue: false }, "{i:?}");
        }
        // The thumb alone: cued.
        assert_eq!(
            render_rung(&RungInputs {
                has_thumb: true,
                ..fit
            }),
            RenderDecision::Fit { cue: true }
        );
    }

    /// "Never show upscaled pixels UNFLAGGED" at fit, as an invariant over the
    /// whole sweep, stated independently of how the arm is written: a cue off
    /// at fit means a sharp or terminal rung, a screen rung or mid that
    /// serves the box, or a failed cursor — nothing else.
    #[test]
    fn at_fit_a_cue_off_means_a_serving_rung_or_a_sharp_or_a_failure() {
        let (mut off, mut on) = (0, 0);
        for i in every_input_combination() {
            if i.loupe != LoupeWhere::Fit {
                continue;
            }
            match render_rung(&i) {
                RenderDecision::Fit { cue: false } => {
                    off += 1;
                    assert!(
                        i.has_sharp
                            || (i.has_rung && i.rung_serves_fit)
                            || (i.has_mid && i.mid_serves_fit)
                            || i.cursor_failed,
                        "upscaled pixels unflagged at fit: {i:?}"
                    );
                }
                RenderDecision::Fit { cue: true } => on += 1,
                other => panic!("at fit the decision is the fit view, not {other:?}: {i:?}"),
            }
        }
        assert!(off > 0 && on > 0, "the sweep reached both cues");
    }

    /// Above fit the screen rung is the fit-size decode, so any factor above
    /// fit upscales it: it renders, below the full-res and above the mid and
    /// the thumb, and the app lights the pill for it (ui-grid.md, the ladder
    /// above fit) — whether or not it serves the fit box, since above fit the
    /// rule is strict.
    #[test]
    fn above_fit_a_screen_rung_renders_cued() {
        for i in [
            RungInputs {
                has_rung: true,
                ..cold()
            },
            RungInputs {
                has_rung: true,
                rung_serves_fit: true,
                ..cold()
            },
            RungInputs {
                has_rung: true,
                has_mid: true,
                mid_serves_fit: true,
                has_thumb: true,
                ..cold()
            },
            // A real rung of the cursor's own image, like the mid: a failure
            // gates the thumb rescue and the hold, not this.
            RungInputs {
                has_rung: true,
                cursor_failed: true,
                ..cold()
            },
        ] {
            assert_eq!(render_rung(&i), RenderDecision::Rung, "{i:?}");
        }
        // The full-res wins over it.
        assert_eq!(
            render_rung(&RungInputs {
                has_sharp: true,
                has_rung: true,
                rung_serves_fit: true,
                ..cold()
            }),
            RenderDecision::Sharp
        );
    }

    /// Brief 008 (ui-grid.md, "The render ladder": "an empty view at fit is
    /// off, there being no cursor to cue"): where the loupe is.
    #[test]
    fn the_fit_cue_needs_a_cursor_in_the_view() {
        assert_eq!(loupe_where(true, 1.0, 0), LoupeWhere::Off);
        assert_eq!(loupe_where(true, 1.0, 5), LoupeWhere::Fit);
        assert_eq!(loupe_where(true, 2.0, 5), LoupeWhere::AboveFit);
        assert_eq!(
            loupe_where(true, 2.0, 0),
            LoupeWhere::AboveFit,
            "above fit the factor decides first, as the overlay's own test did"
        );
        assert_eq!(loupe_where(false, 1.0, 5), LoupeWhere::Off);
        assert_eq!(loupe_where(false, 2.0, 5), LoupeWhere::Off);
        // `Z` before the ceiling is known: the infinite pin is above fit.
        assert_eq!(loupe_where(true, f32::INFINITY, 5), LoupeWhere::AboveFit);
    }

    // ---------------------------------------------------------------
    // The exhaustive table: every reachable input combination.
    // ---------------------------------------------------------------

    /// The pre-A3 app ladder, transcribed from `presenter.rs` as it stood
    /// at `cd236e6` — the SHAPE it had there (a nested match on the two
    /// texture Options with the thumb fallback, then the hold/drop
    /// catch-all), not the shape [`render_rung`] has.
    ///
    /// This is the equivalence obligation as an executable oracle: if the
    /// extraction lost or inverted a condition, the sweep below finds the
    /// input that shows it. Deliberately written the OLD way — a
    /// transcription that mirrored the new early-return chain would prove
    /// nothing. It can speak only where the old ladder had an answer: off
    /// the fit view (the old ladder left it) and without a screen rung
    /// (brief 008 added it), so its one input line reads the loupe's
    /// position where it read `overlay_wanted`.
    fn the_old_app_ladder(i: &RungInputs) -> RenderDecision {
        // `let sharp = fullres.filter(is_top_rung)` — the Option the old
        // match scrutinised. `overlay` is `factor > 1.0 && at_loupe`.
        let sharp = i.has_sharp;
        assert!(i.loupe != LoupeWhere::Fit);
        let overlay = i.loupe == LoupeWhere::AboveFit;
        // `let soft = if sharp.is_none() && overlay { mids.get(cursor)
        //     .or_else(|| fullres_for(cursor)) } else { None };`
        let soft = if !sharp && overlay { i.has_mid } else { false };
        // `let (soft, soft_is_thumb) = match soft { Some => (soft, false),
        //     None if sharp.is_none() && overlay && !failed =>
        //         (images.get(cursor), true), None => (None, false) };`
        let (soft, soft_is_thumb) = if soft {
            (true, false)
        } else if !sharp && overlay && !i.cursor_failed {
            (i.has_thumb, true)
        } else {
            (false, false)
        };
        // `match (sharp, soft) { (Some, _) if overlay => …, (None, Some)
        //     if overlay => …, _ => … }`
        if sharp && overlay {
            RenderDecision::Sharp
        } else if !sharp && soft && overlay {
            RenderDecision::Soft {
                is_thumb: soft_is_thumb,
            }
        } else {
            // The catch-all: `let capped = matches!(overlay_hold,
            //     Some((c, since)) if c == cursor && now - since >= CAP);`
            let capped = matches!(i.hold, Some(h) if h.same_cursor && h.elapsed >= i.hold_cap);
            let failed = i.cursor_failed;
            // `if overlay && win.get_one2one() && !failed && !capped {`
            if overlay && i.overlay_was_up && !failed && !capped {
                //     `if !matches!(overlay_hold, Some((c, _)) if c == cursor) {`
                RenderDecision::Hold {
                    start: !matches!(i.hold, Some(h) if h.same_cursor),
                }
            } else {
                // `if win.get_one2one() && overlay { trace "(… )" }`, with
                // `if failed { "decode failed" } else { "hold cap" }`.
                RenderDecision::Drop {
                    reason: if !overlay {
                        DropReason::BelowLadder
                    } else if !i.overlay_was_up {
                        DropReason::NothingToHold
                    } else if failed {
                        DropReason::DecodeFailed
                    } else {
                        DropReason::HoldCap
                    },
                }
            }
        }
    }

    /// Every combination of the inputs that vary: 2^8 booleans (the four
    /// rungs in hand, the two "serves the fit box" facts, the failure, the
    /// overlay's previous state) × 3 loupe positions × 5 hold states = 3,840
    /// rows (ui-grid.md: "2^8 booleans × 3 loupe positions × 5 hold states").
    /// `hold_cap` is pinned at production's `OVERLAY_HOLD_CAP` on purpose —
    /// only `elapsed >= cap` is ever asked, and the 249/250 ms hold states
    /// above already sweep both sides of that comparison. Varying the cap
    /// itself would re-test the same boundary in different units.
    fn every_input_combination() -> Vec<RungInputs> {
        let holds = [
            None,
            held_for_this_cursor(0),
            held_for_this_cursor(249),
            held_for_this_cursor(250),
            held_for_the_previous_image(10_000),
        ];
        let mut rows = Vec::new();
        for bits in 0u16..256 {
            let bit = |n: u16| bits & (1 << n) != 0;
            for loupe in [LoupeWhere::Off, LoupeWhere::Fit, LoupeWhere::AboveFit] {
                for hold in holds {
                    rows.push(RungInputs {
                        has_sharp: bit(0),
                        has_rung: bit(1),
                        has_mid: bit(2),
                        has_thumb: bit(3),
                        rung_serves_fit: bit(4),
                        mid_serves_fit: bit(5),
                        cursor_failed: bit(6),
                        loupe,
                        overlay_was_up: bit(7),
                        hold,
                        hold_cap: CAP,
                    });
                }
            }
        }
        rows
    }

    /// Renamed from `render_rung_reproduces_the_old_app_ladder_on_every_input`:
    /// the equivalence with the pre-move ladder holds on the rows without a
    /// screen rung and off the fit view (ui-grid.md), the only rows the old
    /// ladder could answer; the new rows are sentences of the spec with their
    /// decisions written out (the tests above).
    #[test]
    fn render_rung_reproduces_the_old_app_ladder_where_it_can_speak() {
        let rows = every_input_combination();
        assert_eq!(rows.len(), 3840, "the sweep must be the full cross product");
        let mut spoken = 0;
        for i in rows
            .iter()
            .filter(|i| !i.has_rung && i.loupe != LoupeWhere::Fit)
        {
            spoken += 1;
            assert_eq!(
                render_rung(i),
                the_old_app_ladder(i),
                "extraction changed behavior for {i:?}"
            );
        }
        assert_eq!(
            spoken, 1280,
            "a third of the rows lack both the rung and the fit"
        );
    }

    #[test]
    fn render_rung_is_total_and_reaches_every_decision() {
        // Totality is the type system's, but "every arm is live" is not:
        // a decision no input can produce is a branch that lost its cause.
        let mut seen = Vec::new();
        for i in every_input_combination() {
            let d = render_rung(&i);
            if !seen.contains(&d) {
                seen.push(d);
            }
        }
        for expected in [
            RenderDecision::Sharp,
            RenderDecision::Rung,
            RenderDecision::Soft { is_thumb: false },
            RenderDecision::Soft { is_thumb: true },
            RenderDecision::Hold { start: true },
            RenderDecision::Hold { start: false },
            RenderDecision::Drop {
                reason: DropReason::BelowLadder,
            },
            RenderDecision::Drop {
                reason: DropReason::DecodeFailed,
            },
            RenderDecision::Drop {
                reason: DropReason::HoldCap,
            },
            RenderDecision::Drop {
                reason: DropReason::NothingToHold,
            },
            RenderDecision::Fit { cue: true },
            RenderDecision::Fit { cue: false },
        ] {
            assert!(seen.contains(&expected), "no input produces {expected:?}");
        }
        assert_eq!(seen.len(), 12, "an unexpected decision appeared: {seen:?}");
    }

    #[test]
    fn the_overlay_never_drops_to_fit_with_pixels_of_the_cursor_in_hand() {
        // The transit contract as an invariant over the whole sweep,
        // stated independently of how the ladder is written: while the
        // loupe is above fit and the cursor image is not known dead, a
        // rung of the CURSOR's own image always renders — never fit. The
        // screen rung counts as pixels in hand (brief 008).
        for i in every_input_combination() {
            if i.loupe != LoupeWhere::AboveFit || i.cursor_failed {
                continue;
            }
            if i.has_sharp || i.has_rung || i.has_mid || i.has_thumb {
                assert!(
                    matches!(
                        render_rung(&i),
                        RenderDecision::Sharp | RenderDecision::Rung | RenderDecision::Soft { .. }
                    ),
                    "fit-flash with pixels in hand: {i:?}"
                );
            }
        }
    }

    #[test]
    fn a_drop_above_fit_always_carries_an_excuse() {
        // The M1 fit-flash rule: an overlay that was UP coming down while
        // the loupe is still above fit must name failure or the cap — the
        // excuse-less `(no rung in hand)` form is outlawed.
        for i in every_input_combination() {
            if i.loupe != LoupeWhere::AboveFit || !i.overlay_was_up {
                continue;
            }
            if let RenderDecision::Drop { reason } = render_rung(&i) {
                assert!(
                    matches!(reason, DropReason::DecodeFailed | DropReason::HoldCap),
                    "unexcused drop above fit: {i:?}"
                );
            }
        }
    }

    // ---------------------------------------------------------------
    // The fit cell and the pill.
    // ---------------------------------------------------------------

    /// The fit cell draws what its mark names (brief 008's plan, fix round
    /// 3): over all 16 combinations of held textures, each with a distinct
    /// marker, `fit_cell` returns the highest present in the order full >
    /// screen > mid > thumb AND the texture its label names, and
    /// `fit_rung_shown` agrees with the label on every combination. Red with
    /// the screen rung and the mid swapped in the order (the {rung, mid} row
    /// labels `mid`), and with `Screen` handing back the mid's texture (the
    /// label right, the texture wrong).
    #[test]
    fn the_fit_cell_draws_the_rung_it_names() {
        // The order as data, scanned — not a mirror of the match.
        let order = [
            (FitRung::Full, "full"),
            (FitRung::Screen, "screen"),
            (FitRung::Mid, "mid"),
            (FitRung::Thumb, "thumb"),
        ];
        for mask in 0u8..16 {
            let has = |n: u8| mask & (1 << n) != 0;
            let (label, drawn) = fit_cell(
                has(0).then_some("full"),
                has(1).then_some("screen"),
                has(2).then_some("mid"),
                has(3).then_some("thumb"),
            );
            let best = (0u8..4).find(|n| has(*n)).map(|n| order[usize::from(n)]);
            assert_eq!(
                label,
                best.map_or(FitRung::None, |(rung, _)| rung),
                "the highest rung held, mask {mask:04b}"
            );
            assert_eq!(
                drawn,
                best.map(|(_, marker)| marker),
                "the texture drawn is the one the label names, mask {mask:04b}"
            );
            // The texture a label names reads the same in the mark.
            assert_eq!(label.to_string(), drawn.unwrap_or("none"));
            let inputs = RungInputs {
                has_sharp: has(0),
                has_rung: has(1),
                has_mid: has(2),
                has_thumb: has(3),
                ..cold()
            };
            assert_eq!(fit_rung_shown(&inputs), label, "mask {mask:04b}");
        }
    }

    /// The pill never flickers (ui-grid.md; the pill rule, Manager M2
    /// 2026-09-26; Q-B): while travelling a lit pill stays on until
    /// `CUE_MIN_ON` has passed since the LAST soft frame, a sharp frame after
    /// the key is released clears it at once, any soft frame lights it, the
    /// minimum never holds it over a failed cursor — and the re-check is due
    /// at the earlier of the minimum's end and the end of travel. Red with no
    /// minimum (the
    /// 100 ms row), with the minimum held when not travelling (the "off at
    /// once" row), with the failed clause dropped (the failed rows), with the
    /// re-check ignoring the travel left (the first re-check row reads 150 ms)
    /// or always `None` (both `Some` rows), and with the minimum counted from
    /// the lighting (the sequence's 400 ms row: a second soft frame must
    /// restart it).
    #[test]
    fn the_cue_pill_keeps_its_minimum_while_travelling() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let at = |n: u64| t0 + ms(n);
        let min = ms(250);
        // Travel left never exceeds `SETTLE_DEBOUNCE` (150 ms).
        let travelling = Some(ms(120));
        // Travelling: a soft frame lights it, and the minimum counts from it.
        let lit = cue_pill(true, false, travelling, None, at(0), min);
        assert!(lit.on, "a soft frame lights the pill");
        assert_eq!(lit.last_soft, Some(at(0)));
        // A sharp frame 100 ms after the last soft one, still travelling:
        // held lit.
        assert!(cue_pill(false, false, travelling, Some(at(0)), at(100), min).on);
        // 250 ms after it: cleared.
        assert!(!cue_pill(false, false, travelling, Some(at(0)), at(250), min).on);
        // Not travelling: a sharp frame clears it at once, however recent
        // the soft one.
        assert!(
            !cue_pill(false, false, None, Some(at(0)), at(10), min).on,
            "the key is released: a sharp frame clears the pill at once"
        );
        // A single soft frame lights it, travelling or not.
        assert!(cue_pill(true, false, None, None, at(0), min).on);
        assert!(cue_pill(true, false, None, Some(at(0)), at(900), min).on);
        // A failed cursor: the minimum never holds the pill over its badge —
        // travelling or not, inside the minimum or not — and the clock is
        // forgotten.
        for (travel, since) in [(travelling, 100), (None, 100), (travelling, 900)] {
            let failed = cue_pill(false, true, travel, Some(at(0)), at(since), min);
            assert_eq!(
                failed,
                CuePill {
                    on: false,
                    recheck_in: None,
                    last_soft: None
                },
                "a failed cursor's pill: travel {travel:?}, {since} ms"
            );
        }
        // Its own soft pixels still light it, as any soft frame does: they
        // are upscaled, never shown unflagged.
        assert!(cue_pill(true, true, travelling, Some(at(0)), at(100), min).on);
        // The re-check: lit over a sharp frame, 100 ms since the last soft
        // one, 120 ms of travel left — travel ends first.
        assert_eq!(
            cue_pill(false, false, Some(ms(120)), Some(at(0)), at(100), min).recheck_in,
            Some(ms(120))
        );
        // 200 ms since the last soft frame, 120 ms left — the minimum ends
        // first.
        assert_eq!(
            cue_pill(false, false, Some(ms(120)), Some(at(0)), at(200), min).recheck_in,
            Some(ms(50))
        );
        // Lit over a soft frame: none (its landing refreshes).
        assert_eq!(
            cue_pill(true, false, Some(ms(120)), Some(at(0)), at(100), min).recheck_in,
            None
        );
        // Off: none.
        assert_eq!(
            cue_pill(false, false, None, Some(at(0)), at(100), min).recheck_in,
            None
        );
        assert_eq!(
            cue_pill(false, false, travelling, Some(at(0)), at(300), min).recheck_in,
            None
        );
        // Q-B: soft, sharp, soft, sharp, sharp while travelling, the clock
        // threaded through as the app threads it. The second soft frame, at
        // 200 ms, restarts the minimum, so the sharp frame at 400 ms is still
        // held lit (200 ms since the last soft frame) — counted from the
        // lighting at 0 it would read 400 ms and clear. At 450 ms the minimum
        // has passed.
        let mut last_soft = None;
        for (t, soft, on) in [
            (0, true, true),
            (100, false, true),
            (200, true, true),
            (400, false, true),
            (450, false, false),
        ] {
            let pill = cue_pill(soft, false, travelling, last_soft, at(t), min);
            assert_eq!(pill.on, on, "the frame at {t} ms (soft {soft})");
            last_soft = pill.last_soft;
        }
    }

    // ---------------------------------------------------------------
    // The texture rings.
    // ---------------------------------------------------------------

    /// The texture rings take their windows from the engine (renamed from
    /// `the_ring_is_the_prefetch_ring`, whose promise — 5, the literal the
    /// app used to carry — became the engine's windows, brief 008): one lean
    /// for the engine's ring and both texture rings, `RingWindow::leaning`,
    /// and the symmetric ±`PREFETCH` window the app's full-res ring had.
    #[test]
    fn the_texture_rings_are_the_engines_windows() {
        let forward = RingWindow::leaning(2, 15, true);
        assert_eq!(
            forward,
            RingWindow {
                before: 2,
                after: 15
            }
        );
        assert_eq!(
            forward.capacity(),
            18,
            "the ring: 2 behind, 15 ahead, the cursor"
        );
        assert_eq!(
            RingWindow::leaning(2, 15, false),
            RingWindow {
                before: 15,
                after: 2
            },
            "a backward lean swaps the sides"
        );
        assert_eq!(
            SYMMETRIC.capacity(),
            5,
            "the full-res ring before brief 008"
        );
        assert!(forward.contains(400, 398) && forward.contains(400, 415));
        assert!(!forward.contains(400, 397) && !forward.contains(400, 416));
        assert!(
            forward.contains(1, 0),
            "clamped at the start, never negative"
        );
    }

    #[test]
    fn a_ring_within_capacity_evicts_nothing() {
        let view: Vec<usize> = (0..20).collect();
        for n in 0..=SYMMETRIC.capacity() {
            let held: Vec<usize> = (0..n).collect();
            assert_eq!(evict_ring(&held, 0, &view, SYMMETRIC), None, "held {n}");
        }
    }

    #[test]
    fn the_victim_is_the_farthest_in_view_order() {
        let view: Vec<usize> = (0..20).collect();
        // Cursor at 10; the farthest held id is 3 (distance 7), in slot 1.
        let held = [9, 3, 10, 11, 12, 8];
        assert_eq!(evict_ring(&held, 10, &view, SYMMETRIC), Some(1));
    }

    #[test]
    fn the_cursors_own_texture_is_never_the_victim() {
        // The cursor sits at one END of the ring, so it IS the farthest
        // entry by distance — and must still survive.
        let view: Vec<usize> = (0..20).collect();
        let held = [10, 9, 8, 7, 6, 5];
        assert_eq!(evict_ring(&held, 10, &view, SYMMETRIC), Some(5)); // id 5, not id 10
    }

    #[test]
    fn distance_is_view_order_not_id_order() {
        // The capture sort interleaves two camera bodies: id 19 is the
        // cursor's immediate view NEIGHBOUR while id 11 is five positions
        // away. Age or id order would evict exactly the frame the next tap
        // needs (issue #46, the 81 ms thumb blink).
        let view = vec![0, 10, 1, 11, 2, 12, 3, 13, 9, 19];
        let held = [9, 19, 11, 12, 13, 3];
        // Cursor id 9 sits at view position 8. Distances: 19→1, 11→5,
        // 12→3, 13→1, 3→2. The farthest is id 11 in slot 2.
        assert_eq!(evict_ring(&held, 9, &view, SYMMETRIC), Some(2));
    }

    #[test]
    fn an_entry_no_longer_in_the_view_goes_first() {
        // A filter removed id 4 from the view: it is unreachable by any
        // arrow, so it outranks even the farthest live entry.
        let view = vec![0, 1, 2, 3, 5, 6, 7, 8, 9, 10];
        let held = [0, 4, 9, 10, 8, 7];
        assert_eq!(evict_ring(&held, 7, &view, SYMMETRIC), Some(1));
    }

    #[test]
    fn a_cursor_outside_the_view_makes_every_entry_maximal() {
        // Nothing has a distance, so the tie rule decides: the LAST
        // non-cursor slot.
        let view = vec![0, 1, 2, 3, 4, 5];
        let held = [0, 1, 2, 3, 4, 99];
        assert_eq!(evict_ring(&held, 42, &view, SYMMETRIC), Some(5));
        // ...and with the cursor among them it is still spared.
        let held = [0, 1, 2, 3, 4, 42];
        assert_eq!(evict_ring(&held, 42, &view, SYMMETRIC), Some(4));
    }

    #[test]
    fn ties_go_to_the_later_slot() {
        // ids 8 and 12 are both 2 away from the cursor at 10. The freshly
        // inserted one (the later slot) loses — a back-and-forth walk then
        // keeps the neighbour it just came from.
        let view: Vec<usize> = (0..20).collect();
        let held = [10, 9, 11, 8, 12, 13];
        // Distances: 9→1, 11→1, 8→2, 12→2, 13→3. Farthest is 13 (slot 5).
        assert_eq!(evict_ring(&held, 10, &view, SYMMETRIC), Some(5));
        // Remove it and the 8/12 tie decides: slot 4 (id 12), the later.
        let held = [10, 9, 11, 8, 12, 7];
        // 7 is 3 away, so it goes first...
        assert_eq!(evict_ring(&held, 10, &view, SYMMETRIC), Some(5));
        // SYNTHETIC ring, deliberately: id 11 appears twice, which
        // `insert_fullres`'s retain-then-push dedupe forbids in the app.
        // The duplicate is NOT the only way to tie at max distance — a
        // duplicate-free linear ring ties too (e.g. [10,9,11,8,7,13] at
        // cursor 10: ids 7 and 13 are both 3 away) — it is just the
        // smallest state that pins the last-maximum rule on a value pair
        // the surrounding rows already use. The tie rule itself is
        // app-reachable (equidistant neighbors on any view).
        let held = [10, 9, 11, 8, 12, 11];
        // ...with the maximum shared by 8 (slot 3) and 12 (slot 4).
        assert_eq!(evict_ring(&held, 10, &view, SYMMETRIC), Some(4));
    }

    #[test]
    fn ties_go_to_the_later_slot_on_a_view_the_app_can_reach() {
        // The row above forces its tie with a duplicate id. This one uses
        // six DISTINCT ids and a view order the app really produces (a
        // filtered / capture-time-sorted view, where neighbours in id
        // space are not neighbours in view space): ids 20 and 31 are both
        // three positions from the cursor, so the tie rule alone decides.
        let view = [20, 30, 9, 10, 11, 21, 31];
        let held = [10, 9, 11, 30, 20, 31];
        // View positions: 20@0, 30@1, 9@2, cursor 10@3, 11@4, 21@5, 31@6.
        // Distances: 9→1, 11→1, 30→2, 20→3, 31→3. Farthest is the 20/31
        // tie, and the LATER slot (5, id 31) loses.
        assert_eq!(evict_ring(&held, 10, &view, SYMMETRIC), Some(5));
    }

    #[test]
    fn an_empty_view_evicts_the_last_non_cursor_slot() {
        // No view at all (a session being swapped): every entry is at
        // maximum distance, so the tie rule alone decides.
        let held = [1, 2, 3, 4, 5, 6];
        assert_eq!(evict_ring(&held, 3, &[], SYMMETRIC), Some(5));
    }

    #[test]
    fn repeated_eviction_walks_the_ring_down_to_capacity() {
        // The caller's loop: evict until `None`. Pinning the SEQUENCE,
        // because that is what the app does and a single-shot victim can
        // be right while the walk is wrong.
        let view: Vec<usize> = (0..20).collect();
        let mut held = vec![10, 3, 11, 17, 9, 12, 8];
        let mut evicted = Vec::new();
        while let Some(victim) = evict_ring(&held, 10, &view, SYMMETRIC) {
            evicted.push(held.remove(victim));
        }
        assert_eq!(evicted, vec![17, 3]);
        assert_eq!(held, vec![10, 11, 9, 12, 8]);
        assert_eq!(held.len(), SYMMETRIC.capacity());
    }

    /// The app's loop over one ring (`insert_fullres`): re-inserting an id
    /// moves it to the end, then slots are given up until `evict_ring` says
    /// the ring fits.
    fn insert_all(
        ids: impl IntoIterator<Item = usize>,
        cursor: usize,
        window: RingWindow,
    ) -> Vec<usize> {
        let view: Vec<usize> = (0..1000).collect();
        let mut held: Vec<usize> = Vec::new();
        for id in ids {
            held.retain(|h| *h != id);
            held.push(id);
            while let Some(victim) = evict_ring(&held, cursor, &view, window) {
                held.remove(victim);
            }
        }
        held.sort_unstable();
        held
    }

    /// Brief 008 (ui-grid.md, "The render ladder"): a ring leaning forward
    /// holds its whole runway. With the cursor at 400 and the window 2
    /// behind / 15 ahead, ids 391..=415 land in order and the ring ends
    /// 398..=415, every frame of the runway 401..=415 held — an entry inside
    /// the window is never evicted while one outside it is held. With plain
    /// distance it kept the frames just passed and evicted the runway's far
    /// end as each landed (391..=408).
    #[test]
    fn a_leaning_ring_keeps_its_runway() {
        let held = insert_all(391..=415, 400, RingWindow::leaning(2, 15, true));
        assert_eq!(held, (398..=415).collect::<Vec<_>>());
    }

    /// The mirror: leaning backward, ids 409 down to 385 land and the ring
    /// ends 385..=402, the runway behind the cursor held.
    #[test]
    fn a_backward_lean_keeps_the_runway_behind() {
        let held = insert_all((385..=409).rev(), 400, RingWindow::leaning(2, 15, false));
        assert_eq!(held, (385..=402).collect::<Vec<_>>());
    }

    /// The rule stated in WORDS rather than in `max_by_key`: out-of-view is
    /// maximal, an entry outside the window goes before any entry inside it,
    /// farthest by view distance first within each class, the cursor is
    /// spared, and a tie goes to the LATER slot.
    ///
    /// Written as an explicit scan on purpose. The app's version leaned on
    /// `max_by_key` returning the LAST maximum — documented std behavior,
    /// but nothing in the app ever said the tie rule mattered. Re-deriving
    /// it by hand here is what makes the sweep below a check rather than a
    /// mirror. The outside-first class is its own pass over the entries,
    /// not a key.
    fn farthest_by_hand(
        held: &[usize],
        cursor: usize,
        view: &[usize],
        window: RingWindow,
    ) -> Option<usize> {
        if held.len() <= window.capacity() {
            return None;
        }
        let pos_of = |id: usize| view.iter().position(|v| *v == id);
        let distance_of = |id: usize| match (pos_of(cursor), pos_of(id)) {
            (Some(c), Some(p)) => Some(p.abs_diff(c)),
            _ => None,
        };
        let outside = |id: usize| match (pos_of(cursor), pos_of(id)) {
            (Some(c), Some(p)) => p + window.before < c || p > c + window.after,
            _ => true,
        };
        // The class the victim comes from: outside the window, if any entry
        // other than the cursor is.
        let any_outside = held.iter().any(|id| *id != cursor && outside(*id));
        let mut best: Option<(usize, usize)> = None; // (distance, slot)
        for (slot, id) in held.iter().enumerate() {
            if *id == cursor || outside(*id) != any_outside {
                continue;
            }
            let distance = distance_of(*id).unwrap_or(usize::MAX);
            // `>=`, not `>`: an equal distance later in the ring replaces
            // the earlier one — the tie goes to the fresher slot.
            if best.is_none_or(|(d, _)| distance >= d) {
                best = Some((distance, slot));
            }
        }
        Some(best.map_or(0, |(_, slot)| slot))
    }

    #[test]
    fn the_victim_rule_holds_over_a_generated_sweep() {
        // Deterministic LCG (Numerical Recipes): no dependency, same rows
        // every run. 2,000 rings over views that shrink under a filter,
        // cursors that fall out of the view, ties by construction (the id
        // pool is small), windows that lean either way or none (each side
        // 0..=4), and lengths on both sides of the ring capacity.
        let mut seed: u64 = 0x5DEE_CE66;
        let mut next = move |n: usize| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as usize % n
        };
        let mut evicted_something = 0;
        let mut cursor_out_of_view = 0;
        let mut leaning = 0;
        for _ in 0..2_000 {
            let view: Vec<usize> = (0..12).filter(|_| next(4) > 0).collect();
            let window = RingWindow {
                before: next(5),
                after: next(5),
            };
            if window.before != window.after {
                leaning += 1;
            }
            let mut held: Vec<usize> = Vec::new();
            let want = 1 + next(9);
            while held.len() < want {
                let id = next(14); // 12..13 are ids no view ever contains
                if !held.contains(&id) {
                    held.push(id);
                }
            }
            let cursor = next(14);
            if !view.contains(&cursor) {
                cursor_out_of_view += 1;
            }
            let victim = evict_ring(&held, cursor, &view, window);
            assert_eq!(
                victim,
                farthest_by_hand(&held, cursor, &view, window),
                "held {held:?} cursor {cursor} view {view:?} window {window:?}"
            );
            if let Some(slot) = victim {
                evicted_something += 1;
                assert!(held.len() > window.capacity(), "evicted inside capacity");
                assert_ne!(held[slot], cursor, "the cursor was evicted");
            }
        }
        // Non-vacuity: the sweep really exercised the interesting shapes.
        assert!(
            evicted_something > 200,
            "only {evicted_something} evictions"
        );
        assert!(
            cursor_out_of_view > 100,
            "only {cursor_out_of_view} stray cursors"
        );
        assert!(leaning > 1000, "only {leaning} leaning windows");
    }

    /// Brief 008, the redesign's G4 (ui-grid.md, "The render ladder";
    /// 01-architecture.md, the kitchen): the kitchen cooks its queued
    /// full-res fills in the order the cursor meets them — the cursor's
    /// first, then by view distance inside the window, at equal distance
    /// toward the window's lean (a symmetric window reads forward), then the
    /// fills outside the window — whatever order they were queued in. Red
    /// when the lean is ignored (the backward row).
    #[test]
    fn full_fills_cook_in_the_order_the_cursor_meets_them() {
        let view: Vec<usize> = (0..40).collect();
        let pops = |window: RingWindow| {
            let mut queued = vec![7, 3, 5, 12, 4, 6, 30];
            let mut order = Vec::new();
            while let Some(slot) = next_fill(&queued, 5, &view, window) {
                order.push(queued.remove(slot));
            }
            order
        };
        assert_eq!(
            pops(RingWindow::leaning(2, 15, true)),
            [5, 6, 4, 7, 3, 12, 30],
            "leaning forward"
        );
        assert_eq!(
            pops(RingWindow::leaning(2, 15, false)),
            [5, 4, 6, 3, 7, 12, 30],
            "leaning backward: 12 is outside the window, 30 farther outside"
        );
        assert_eq!(
            pops(RingWindow::symmetric(2)),
            [5, 6, 4, 7, 3, 12, 30],
            "a symmetric window reads forward on ties"
        );
        // Out of the view last, first queued first among equals.
        let queued = [99, 98, 6];
        assert_eq!(
            next_fill(&queued, 5, &view, RingWindow::symmetric(2)),
            Some(2)
        );
        assert_eq!(
            next_fill(&[99, 98], 5, &view, RingWindow::symmetric(2)),
            Some(0)
        );
        assert_eq!(next_fill(&[], 5, &view, RingWindow::symmetric(2)), None);
    }
}
