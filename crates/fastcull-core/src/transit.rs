//! Transit policy: WHICH rung the loupe overlay shows, and which full-res
//! texture the ring gives up — as pure decision functions.
//!
//! The render ladder (`specs/modules/ui-grid.md`, quality rule as revised
//! by issues #21 and #46) is: full-res (sharp) → the cursor's mid rung
//! (soft) → the cursor's own 320 px grid THUMB (soft) → a bounded residual
//! HOLD of the previous image's pixels → the honest drop to fit. Two bounds
//! guard the hold: a decode FAILURE of the cursor image drops immediately
//! (the strip owns the failed badge), and a cap (`OVERLAY_HOLD_CAP` in the
//! app, `hold_cap` here) ends a wedged decode's hold.
//!
//! This module owns that ladder as [`render_rung`], and the full-res ring's
//! victim choice as [`evict_fullres`]. It speaks in rungs, holds and
//! decisions only — never textures, properties or Slint (01-architecture.md:
//! if a piece of code can live in `fastcull-core`, it must). The app gathers
//! the plain-data inputs (texture lookups, the clock read, the zoom factor),
//! calls in, and does the property writes its answer names.
//!
//! **Why it lives here** (ui-grid.md's own recorded deferral, gate
//! 2026-08-09): every #46-class bug so far lived exactly in untestable
//! app-side state. `elapsed` is an INPUT rather than a clock read, which is
//! what makes the cap testable as a table instead of a stopwatch.

use std::time::Duration;

use crate::loupe::PREFETCH;

/// Full-res textures the UI keeps at once: the cursor plus the engine's
/// prefetch ring on both sides.
///
/// This was the bare literal `5` in the app, with `2·PREFETCH+1` written
/// only in a comment beside it — so a change to [`PREFETCH`] moved the
/// engine's ring and left the texture ring behind. Derived now, and pinned
/// by `the_ring_is_the_prefetch_ring`.
pub const FULLRES_RING: usize = 2 * PREFETCH + 1;

/// What the overlay should do this refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderDecision {
    /// Render the cursor's TOP rung: sharp, no cue pill.
    Sharp,
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
}

/// Why the overlay came down. The two *traced* reasons are the ones that
/// excuse a drop while the desire is still above fit — an unexcused drop
/// there is the M1 fit-flash the transit contract outlaws, which is why the
/// trace distinguishes them and the regression tests grep for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// The desire is at or below fit, or the loupe is not on screen: the
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
    /// over the full-res slot, terminality included).
    pub has_sharp: bool,
    /// A sub-top texture of the cursor's own image is in hand: its mid
    /// rung, or a warm sub-top texture the engine re-announced into the
    /// full-res slot (the pruned-and-revisited path).
    pub has_mid: bool,
    /// The cursor's own 320 px grid thumb is in hand.
    pub has_thumb: bool,
    /// The cursor image's decode has FAILED (the strip shows its badge).
    pub cursor_failed: bool,
    /// The desire is above fit AND the loupe is on screen — the overlay
    /// applies at all this refresh.
    pub overlay_wanted: bool,
    /// The overlay was up on the PREVIOUS refresh: there are previous
    /// pixels on screen that a hold would be keeping.
    pub overlay_was_up: bool,
    /// The hold recorded by an earlier refresh, if any.
    pub hold: Option<HoldState>,
    /// Longest one photograph may be misrepresented by a hold.
    pub hold_cap: Duration,
}

/// The render ladder, as one total function: every combination of inputs
/// yields a decision (`render_rung_is_total` sweeps them).
///
/// Order is the ladder's own, top rung first. Two rules are easy to state
/// backwards and are therefore spelled out here:
///
/// * the thumb RESCUE is skipped for a failed cursor image — a file whose
///   320 px thumb survived while every loupe rung is corrupt would sit at
///   1:1 behind a "loading" pill that can never complete, hiding the
///   strip's failed badge (validator finding, #46). One transient is
///   causally unavoidable and accepted: the first focus of a freshly dead
///   file renders its thumb for the milliseconds until the decode attempt
///   fails, because the failure does not exist as knowledge yet. Here that
///   is simply `cursor_failed: false` — the gate binds from the Failed
///   event on.
/// * the cap is PER CURSOR IMAGE: a hold stamped for a different image
///   does not cap this one, it re-starts (`Hold { start: true }`). So the
///   same stale pixels can exceed the cap in aggregate across a hold-arrow
///   run over consecutively cold frames — the bound is on how long any ONE
///   photograph can be misrepresented (recorded in ui-grid.md).
pub fn render_rung(i: &RungInputs) -> RenderDecision {
    if !i.overlay_wanted {
        // Leaving the ladder: at or below fit, or out of the loupe.
        return RenderDecision::Drop {
            reason: DropReason::BelowLadder,
        };
    }
    if i.has_sharp {
        return RenderDecision::Sharp;
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

/// Which slot of the full-res texture ring to give up, or `None` while the
/// ring is within [`FULLRES_RING`].
///
/// `held` is the image ids in slot order (most recently inserted last),
/// `view` the current view order. Eviction is by VIEW distance from the
/// cursor, not insertion age (issue #46): age is view-order-blind — the
/// provisional-order startup window legitimately decodes filename-order
/// neighbors, and once the capture sort lands those are strangers occupying
/// slots; age eviction then discarded exactly the view neighbor the next
/// tap needed while keeping a frame seven positions away (observed as an
/// 81 ms thumb blink on a warm frame).
///
/// Three rules the caller must not re-derive:
/// * the CURSOR's own texture is never the victim (it is what the user is
///   looking at; a prefetch evicting it was seen as back-arrow quality
///   degradation);
/// * an entry no longer in the view (or any entry when the cursor itself
///   is not in the view) is at maximum distance and goes first;
/// * on a TIE the LATER slot goes — the freshly inserted texture loses to
///   an equally distant older one, which is what keeps a back-and-forth
///   walk from thrashing the neighbor it just came from.
pub fn evict_fullres(held: &[usize], cursor: usize, view: &[usize]) -> Option<usize> {
    if held.len() <= FULLRES_RING {
        return None;
    }
    let pos_of = |id: usize| view.iter().position(|v| *v == id);
    let cursor_pos = pos_of(cursor);
    Some(
        held.iter()
            .enumerate()
            .filter(|(_, id)| **id != cursor)
            .max_by_key(|(_, id)| match (cursor_pos, pos_of(**id)) {
                (Some(c), Some(p)) => p.abs_diff(c),
                _ => usize::MAX, // not in the view (or no view): first out
            })
            .map(|(slot, _)| slot)
            // Only reachable if every slot holds the cursor, which the
            // caller's dedupe makes impossible — stay total anyway.
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Duration = Duration::from_millis(250);

    /// The inputs, with everything absent and the overlay wanted: rows
    /// name only what they are about.
    fn cold() -> RungInputs {
        RungInputs {
            has_sharp: false,
            has_mid: false,
            has_thumb: false,
            cursor_failed: false,
            overlay_wanted: true,
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

    #[test]
    fn at_or_below_fit_nothing_on_the_ladder_applies() {
        // Every rung in hand, every hold state — off the ladder is off the
        // ladder, and the drop is the untraced kind.
        for has_sharp in [false, true] {
            for has_mid in [false, true] {
                for has_thumb in [false, true] {
                    for overlay_was_up in [false, true] {
                        let i = RungInputs {
                            has_sharp,
                            has_mid,
                            has_thumb,
                            overlay_wanted: false,
                            overlay_was_up,
                            hold: held_for_this_cursor(1),
                            ..cold()
                        };
                        assert_eq!(
                            render_rung(&i),
                            RenderDecision::Drop {
                                reason: DropReason::BelowLadder
                            },
                            "off the ladder: {i:?}"
                        );
                    }
                }
            }
        }
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
    /// nothing.
    fn the_old_app_ladder(i: &RungInputs) -> RenderDecision {
        // `let sharp = fullres.filter(is_top_rung)` — the Option the old
        // match scrutinised. `overlay` is `factor > 1.0 && at_loupe`.
        let sharp = i.has_sharp;
        let overlay = i.overlay_wanted;
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

    /// Every combination of the eight inputs the ladder reads: 2^6 boolean
    /// combinations × 5 hold states = 320 rows.
    fn every_input_combination() -> Vec<RungInputs> {
        let holds = [
            None,
            held_for_this_cursor(0),
            held_for_this_cursor(249),
            held_for_this_cursor(250),
            held_for_the_previous_image(10_000),
        ];
        let mut rows = Vec::new();
        for has_sharp in [false, true] {
            for has_mid in [false, true] {
                for has_thumb in [false, true] {
                    for cursor_failed in [false, true] {
                        for overlay_wanted in [false, true] {
                            for overlay_was_up in [false, true] {
                                for hold in holds {
                                    rows.push(RungInputs {
                                        has_sharp,
                                        has_mid,
                                        has_thumb,
                                        cursor_failed,
                                        overlay_wanted,
                                        overlay_was_up,
                                        hold,
                                        hold_cap: CAP,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        rows
    }

    #[test]
    fn render_rung_reproduces_the_old_app_ladder_on_every_input() {
        let rows = every_input_combination();
        assert_eq!(rows.len(), 320, "the sweep must be the full cross product");
        for i in &rows {
            assert_eq!(
                render_rung(i),
                the_old_app_ladder(i),
                "extraction changed behavior for {i:?}"
            );
        }
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
        ] {
            assert!(seen.contains(&expected), "no input produces {expected:?}");
        }
        assert_eq!(seen.len(), 9, "an unexpected decision appeared: {seen:?}");
    }

    #[test]
    fn the_overlay_never_drops_to_fit_with_pixels_of_the_cursor_in_hand() {
        // The transit contract as an invariant over the whole sweep,
        // stated independently of how the ladder is written: while the
        // desire is above fit and the cursor image is not known dead, a
        // rung of the CURSOR's own image always renders — never fit.
        for i in every_input_combination() {
            if !i.overlay_wanted || i.cursor_failed {
                continue;
            }
            if i.has_sharp || i.has_mid || i.has_thumb {
                assert!(
                    matches!(
                        render_rung(&i),
                        RenderDecision::Sharp | RenderDecision::Soft { .. }
                    ),
                    "fit-flash with pixels in hand: {i:?}"
                );
            }
        }
    }

    #[test]
    fn a_drop_above_fit_always_carries_an_excuse() {
        // The M1 fit-flash rule: an overlay that was UP coming down while
        // the desire is still above fit must name failure or the cap —
        // the excuse-less `(no rung in hand)` form is outlawed.
        for i in every_input_combination() {
            if !i.overlay_wanted || !i.overlay_was_up {
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
    // The full-res ring.
    // ---------------------------------------------------------------

    #[test]
    fn the_ring_is_the_prefetch_ring() {
        assert_eq!(FULLRES_RING, 2 * PREFETCH + 1);
        assert_eq!(FULLRES_RING, 5, "the literal the app used to carry");
    }

    #[test]
    fn a_ring_within_capacity_evicts_nothing() {
        let view: Vec<usize> = (0..20).collect();
        for n in 0..=FULLRES_RING {
            let held: Vec<usize> = (0..n).collect();
            assert_eq!(evict_fullres(&held, 0, &view), None, "held {n}");
        }
    }

    #[test]
    fn the_victim_is_the_farthest_in_view_order() {
        let view: Vec<usize> = (0..20).collect();
        // Cursor at 10; the farthest held id is 3 (distance 7), in slot 1.
        let held = [9, 3, 10, 11, 12, 8];
        assert_eq!(evict_fullres(&held, 10, &view), Some(1));
    }

    #[test]
    fn the_cursors_own_texture_is_never_the_victim() {
        // The cursor sits at one END of the ring, so it IS the farthest
        // entry by distance — and must still survive.
        let view: Vec<usize> = (0..20).collect();
        let held = [10, 9, 8, 7, 6, 5];
        assert_eq!(evict_fullres(&held, 10, &view), Some(5)); // id 5, not id 10
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
        assert_eq!(evict_fullres(&held, 9, &view), Some(2));
    }

    #[test]
    fn an_entry_no_longer_in_the_view_goes_first() {
        // A filter removed id 4 from the view: it is unreachable by any
        // arrow, so it outranks even the farthest live entry.
        let view = vec![0, 1, 2, 3, 5, 6, 7, 8, 9, 10];
        let held = [0, 4, 9, 10, 8, 7];
        assert_eq!(evict_fullres(&held, 7, &view), Some(1));
    }

    #[test]
    fn a_cursor_outside_the_view_makes_every_entry_maximal() {
        // Nothing has a distance, so the tie rule decides: the LAST
        // non-cursor slot.
        let view = vec![0, 1, 2, 3, 4, 5];
        let held = [0, 1, 2, 3, 4, 99];
        assert_eq!(evict_fullres(&held, 42, &view), Some(5));
        // ...and with the cursor among them it is still spared.
        let held = [0, 1, 2, 3, 4, 42];
        assert_eq!(evict_fullres(&held, 42, &view), Some(4));
    }

    #[test]
    fn ties_go_to_the_later_slot() {
        // ids 8 and 12 are both 2 away from the cursor at 10. The freshly
        // inserted one (the later slot) loses — a back-and-forth walk then
        // keeps the neighbour it just came from.
        let view: Vec<usize> = (0..20).collect();
        let held = [10, 9, 11, 8, 12, 13];
        // Distances: 9→1, 11→1, 8→2, 12→2, 13→3. Farthest is 13 (slot 5).
        assert_eq!(evict_fullres(&held, 10, &view), Some(5));
        // Remove it and the 8/12 tie decides: slot 4 (id 12), the later.
        let held = [10, 9, 11, 8, 12, 7];
        // 7 is 3 away, so it goes first...
        assert_eq!(evict_fullres(&held, 10, &view), Some(5));
        let held = [10, 9, 11, 8, 12, 11];
        // ...with the maximum shared by 8 (slot 3) and 12 (slot 4).
        assert_eq!(evict_fullres(&held, 10, &view), Some(4));
    }

    #[test]
    fn an_empty_view_evicts_the_last_non_cursor_slot() {
        // No view at all (a session being swapped): every entry is at
        // maximum distance, so the tie rule alone decides.
        let held = [1, 2, 3, 4, 5, 6];
        assert_eq!(evict_fullres(&held, 3, &[]), Some(5));
    }

    #[test]
    fn repeated_eviction_walks_the_ring_down_to_capacity() {
        // The caller's loop: evict until `None`. Pinning the SEQUENCE,
        // because that is what the app does and a single-shot victim can
        // be right while the walk is wrong.
        let view: Vec<usize> = (0..20).collect();
        let mut held = vec![10, 3, 11, 17, 9, 12, 8];
        let mut evicted = Vec::new();
        while let Some(victim) = evict_fullres(&held, 10, &view) {
            evicted.push(held.remove(victim));
        }
        assert_eq!(evicted, vec![17, 3]);
        assert_eq!(held, vec![10, 11, 9, 12, 8]);
        assert_eq!(held.len(), FULLRES_RING);
    }

    /// The rule stated in WORDS rather than in `max_by_key`: farthest by
    /// view distance wins, out-of-view is maximal, the cursor is spared,
    /// and a tie goes to the LATER slot.
    ///
    /// Written as an explicit scan on purpose. The app's version leaned on
    /// `max_by_key` returning the LAST maximum — documented std behavior,
    /// but nothing in the app ever said the tie rule mattered. Re-deriving
    /// it by hand here is what makes the sweep below a check rather than a
    /// mirror.
    fn farthest_by_hand(held: &[usize], cursor: usize, view: &[usize]) -> Option<usize> {
        if held.len() <= FULLRES_RING {
            return None;
        }
        let pos_of = |id: usize| view.iter().position(|v| *v == id);
        let mut best: Option<(usize, usize)> = None; // (distance, slot)
        for (slot, id) in held.iter().enumerate() {
            if *id == cursor {
                continue;
            }
            let distance = match (pos_of(cursor), pos_of(*id)) {
                (Some(c), Some(p)) => p.abs_diff(c),
                _ => usize::MAX,
            };
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
        // pool is small), and lengths on both sides of the ring capacity.
        let mut seed: u64 = 0x5DEE_CE66;
        let mut next = move |n: usize| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 16) as usize % n
        };
        let mut evicted_something = 0;
        let mut cursor_out_of_view = 0;
        for _ in 0..2_000 {
            let view: Vec<usize> = (0..12).filter(|_| next(4) > 0).collect();
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
            let victim = evict_fullres(&held, cursor, &view);
            assert_eq!(
                victim,
                farthest_by_hand(&held, cursor, &view),
                "held {held:?} cursor {cursor} view {view:?}"
            );
            if let Some(slot) = victim {
                evicted_something += 1;
                assert!(held.len() > FULLRES_RING, "evicted inside capacity");
                assert_ne!(held[slot], cursor, "the cursor was evicted");
            }
        }
        // Non-vacuity: the sweep really exercised both interesting shapes.
        assert!(
            evicted_something > 200,
            "only {evicted_something} evictions"
        );
        assert!(
            cursor_out_of_view > 100,
            "only {cursor_out_of_view} stray cursors"
        );
    }
}
