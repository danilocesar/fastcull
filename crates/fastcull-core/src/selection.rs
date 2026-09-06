//! Multi-selection over the filtered view (`ui-grid.md`: Shift+arrows
//! extend, Ctrl+A selects the filtered set, Ctrl+Space and Ctrl/Shift-click
//! join). Selected images are IDs (session-stable); ranges are resolved
//! against VIEW positions at extension time, so a filter change never
//! re-interprets an old range.
//!
//! The cursor is always implicitly part of the batch the IPTC panel acts
//! on: an empty selection means "the cursor image" (Photo Mechanic
//! convention — no dead panel on a bare cursor).
//!
//! HOW LONG A SELECTION LASTS (the rule of 2026-09-06, user decision
//! "file manager style", brief 002 — `ui-grid.md`, "Selection" under
//! Visual language, has it in full): a PLAIN cursor move ends it
//! ([`Selection::collapse`]), a CTRL move keeps it and drops the anchor
//! ([`Selection::reset_anchor`]), and a Shift-span whose anchor arms on
//! that press IS the selection — the frames a Ctrl+click or Ctrl+Space
//! added included. Until that date a plain move kept the selection and
//! the next span was UNIONED with it, which is how a video export came to
//! hold the previous export's frames as well as the new ones (the user's
//! report of 2026-09-06). Which keys are which is the app's business
//! (`nav.rs`); this module owns what each does to the selection.

use std::collections::HashSet;

#[derive(Default)]
pub struct Selection {
    /// Committed selections: Ctrl-toggles, select-all, and folded spans.
    base: HashSet<usize>,
    /// The CURRENT Shift-span (anchor..cursor): each extension REPLACES it
    /// (validator finding: a grow-only span made shrinking impossible —
    /// Shift+Right x3 then Shift+Left left the abandoned tail selected).
    /// Folded into `base` when the anchor resets (Ctrl-navigation).
    span: HashSet<usize>,
    /// Range anchor (image id): where Shift-extension started. NONE means
    /// the next Shift gesture arms fresh — and a fresh span replaces the
    /// whole selection. Ctrl-navigation resets it; Shift+arrows keep it
    /// and re-span anchor..cursor.
    anchor: Option<usize>,
}

impl Selection {
    pub fn is_selected(&self, id: usize) -> bool {
        self.base.contains(&id) || self.span.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.base.is_empty() && self.span.is_empty()
    }

    pub fn len(&self) -> usize {
        self.base.union(&self.span).count()
    }

    pub fn clear(&mut self) {
        self.base.clear();
        self.span.clear();
        self.anchor = None;
    }

    /// Rule 1 of the selection rule (user decision 2026-09-06, brief 002,
    /// "file manager style"): every UNMODIFIED cursor move — an arrow,
    /// PgUp/PgDn/Home/End, `[`/`]` — and the `Y`/`N` mark advance end the
    /// selection. Nothing is left selected, so the cursor alone is the
    /// batch ([`Selection::batch`]), [`Selection::count_in_view`] is 0 and
    /// the status bar goes silent. It fires whether or not the cursor
    /// actually moved: a Right at the last frame is the same intent.
    ///
    /// The body is [`Selection::clear`] — this exists as a NAMED operation
    /// because the call sites are a key map, and a key map that reads
    /// `selection.clear()` says what the code does while this one says
    /// which rule it is obeying. `clear` stays what Esc, a plain click and
    /// G at a grid zoom call.
    pub fn collapse(&mut self) {
        self.clear();
    }

    /// Ctrl+A: exactly the current filtered view.
    pub fn select_all(&mut self, view: &[usize]) {
        self.base = view.iter().copied().collect();
        self.span.clear();
        self.anchor = None;
    }

    /// Shift+arrow / Shift+click: span anchor..cursor over view positions.
    /// The anchor arms on the first extension (from the pre-move cursor)
    /// and persists across further extensions. Each extension REPLACES the
    /// current span, so a span can shrink and flip.
    ///
    /// Rule 3 (user decision 2026-09-06, brief 002, answer 3): a span whose
    /// anchor arms on THIS call — after a plain click, after
    /// Ctrl-navigation, on an empty selection — IS the whole selection, so
    /// the base goes with it, Ctrl-added frames included. A span that
    /// CONTINUES a live anchor replaces only the span, so everything the
    /// user built around it survives. Until 2026-09-06 a fresh span was
    /// unioned with the base instead ("PM behavior", a claim the research
    /// of brief 002 found no product has), which is what carried an
    /// exported set into the next export.
    pub fn extend_to(&mut self, view: &[usize], from: usize, to: usize) {
        let fresh = self.anchor.is_none();
        let anchor = *self.anchor.get_or_insert(from);
        let (Some(a), Some(b)) = (
            view.iter().position(|id| *id == anchor),
            view.iter().position(|id| *id == to),
        ) else {
            return; // anchor or target filtered out: nothing to span
        };
        // AFTER the lookup, never before: a gesture that spans nothing
        // must change nothing (`a_span_that_resolves_to_nothing_leaves_a_
        // fresh_selection_untouched` pins the order).
        if fresh {
            self.base.clear();
        }
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.span = view[lo..=hi].iter().copied().collect();
    }

    /// Shift+`[` / Shift+`]` (burst-grouping.md, issue #55): span
    /// anchor..cursor exactly like [`Selection::extend_to`], then widen BOTH
    /// ends to whole groups — the anchor's group and the cursor's group are
    /// taken entirely, never half. `group_of` maps an image id to its group
    /// (None = a single, which is its own one-frame "group"). The result is
    /// a plain view-order RANGE like every other Shift gesture (persona
    /// 2026-08-28): with interleaved bodies or a non-capture sort the frames
    /// sitting between the two bursts come along, the way Shift+arrows
    /// would take them.
    ///
    /// The anchor is re-armed at its OWN group's far edge — first frame when
    /// the cursor is after it, last frame when before — so a Shift+arrow
    /// that follows spans frame-precisely from the burst's edge ("40 plus
    /// the first two frames of 41"; persona: one rule for what a Shift
    /// extension is, not two). Each press REPLACES the live span, so the
    /// opposite key drops a burst and flips past the anchor burst the way
    /// Shift+arrows flip.
    ///
    /// Rule 3 applies here exactly as it does to [`Selection::extend_to`]:
    /// a burst span whose anchor arms on this press is the whole selection
    /// (brief 002, 2026-09-06).
    pub fn extend_bursts(
        &mut self,
        view: &[usize],
        from: usize,
        to: usize,
        group_of: impl Fn(usize) -> Option<usize>,
    ) {
        let fresh = self.anchor.is_none();
        let anchor = *self.anchor.get_or_insert(from);
        let (Some(a), Some(b)) = (
            view.iter().position(|id| *id == anchor),
            view.iter().position(|id| *id == to),
        ) else {
            return; // anchor or target filtered out: nothing to span
        };
        // After the lookup, for the reason `extend_to` gives.
        if fresh {
            self.base.clear();
        }
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let lo = group_edge(view, lo, &group_of, false);
        let hi = group_edge(view, hi, &group_of, true);
        self.span = view[lo..=hi].iter().copied().collect();
        self.anchor = Some(if a <= b { view[lo] } else { view[hi] });
    }

    /// Ctrl+Shift+B (issue #55, the user's proposal): select every frame of
    /// the cursor's group that is IN THE VIEW — a single selects itself.
    /// Additive (a union, like Ctrl+click: a stale selection is cleared
    /// with Esc, never silently replaced) and idempotent (a second press
    /// changes nothing — a toggle on a 23-frame chord would empty the
    /// selection on a double-tap). The cursor does not move; the Shift
    /// anchor arms here so a following Shift+`]` extends from this burst.
    /// Members hidden by the filter stay unselected: what you see is what
    /// you stamp, and widening the filter later must not reveal a
    /// selection that was never on screen.
    pub fn select_group(
        &mut self,
        view: &[usize],
        cursor: usize,
        group_of: impl Fn(usize) -> Option<usize>,
    ) {
        if !view.contains(&cursor) {
            return; // the cursor is filtered out: nothing under it to select
        }
        self.base.extend(self.span.drain());
        match group_of(cursor) {
            Some(g) => self
                .base
                .extend(view.iter().copied().filter(|id| group_of(*id) == Some(g))),
            None => {
                self.base.insert(cursor);
            }
        }
        self.anchor = Some(cursor);
    }

    /// Rule 2 (brief 002, 2026-09-06): the cursor moved WITHOUT a selection
    /// gesture — Ctrl+arrows, Ctrl+PgUp/PgDn/Home/End, Ctrl+`[`/`]` — so
    /// the live span is committed into the base and the anchor drops. The
    /// selected set itself is untouched; the next Shift-span, arming
    /// fresh, therefore REPLACES it (see [`Selection::extend_to`]).
    ///
    /// This used to be what a PLAIN arrow called, which is why the comment
    /// here promised that an arrow could never drop a selection. Since
    /// 2026-09-06 a plain move calls [`Selection::collapse`] instead.
    pub fn reset_anchor(&mut self) {
        self.base.extend(self.span.drain());
        self.anchor = None;
    }

    /// Ctrl+click and Ctrl+Space (brief 002 R4): add or remove one image,
    /// committing any live span first — the toggle acts on the selection
    /// as the user sees it. The anchor arms here, so a Shift-span that
    /// follows CONTINUES from this image instead of replacing everything.
    pub fn toggle(&mut self, id: usize) {
        self.base.extend(self.span.drain());
        if !self.base.insert(id) {
            self.base.remove(&id);
        }
        self.anchor = Some(id);
    }

    /// The batch the IPTC panel acts on, in VIEW ORDER (the `{seq}`
    /// contract): the selection intersected with the view, or the cursor
    /// alone when nothing is selected. Selected-but-filtered-out images
    /// are NOT in the batch — what you see is what you stamp.
    pub fn batch(&self, view: &[usize], cursor: usize) -> Vec<usize> {
        let selected: Vec<usize> = view
            .iter()
            .copied()
            .filter(|id| self.is_selected(*id))
            .collect();
        if selected.is_empty() {
            view.contains(&cursor)
                .then_some(cursor)
                .into_iter()
                .collect()
        } else {
            selected
        }
    }

    /// How many images the selection covers WITHIN the view — the number the
    /// status bar reports (ui-grid.md "Selection count in the status bar").
    /// Lives here next to `batch` so the two can never drift: whenever
    /// the selection has at least one member IN THE VIEW this is exactly
    /// `batch(view, _).len()`, which a unit test pins. Returns 0 for an
    /// empty selection — where `batch` falls back to the cursor alone —
    /// because a bare cursor is not a selection and the status bar must
    /// stay silent for it.
    ///
    /// The qualifier matters, and the sentence used to lack it (QE
    /// finding 2026-08-28): a selection that is non-empty but entirely
    /// FILTERED OUT counts 0 here while `batch` still returns the cursor
    /// alone, i.e. 1. Reading the old wording as "these two are
    /// interchangeable" is how the video export's menu item and its
    /// dialog came to disagree about how many frames there were.
    pub fn count_in_view(&self, view: &[usize]) -> usize {
        // O(1) short-circuit before touching the view: "nothing selected" is
        // the common state, and rescanning a 50k-image view on every refresh
        // just to prove 0 is pure waste on the UI's hot path.
        if self.is_empty() {
            return 0;
        }
        view.iter().filter(|id| self.is_selected(**id)).count()
    }

    /// Session swap / folder change: stale ids must never leak.
    pub fn reset(&mut self) {
        self.clear();
    }
}

/// The first (or last) view position sharing `view[pos]`'s group — `pos`
/// itself for a single. Members need not be contiguous in the view
/// (interleaved bodies), so this is a scan over the view, the same way
/// `burst::next_boundary` finds a group's first visible frame.
fn group_edge(
    view: &[usize],
    pos: usize,
    group_of: &impl Fn(usize) -> Option<usize>,
    last: bool,
) -> usize {
    let Some(g) = group_of(view[pos]) else {
        return pos;
    };
    let same = |p: &usize| group_of(view[*p]) == Some(g);
    if last {
        (pos..view.len()).rev().find(same).unwrap_or(pos)
    } else {
        (0..=pos).find(same).unwrap_or(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_selection_batches_the_cursor() {
        let sel = Selection::default();
        assert_eq!(sel.batch(&[5, 3, 8], 3), vec![3]);
        assert!(sel.batch(&[5, 8], 3).is_empty(), "cursor filtered out");
    }

    #[test]
    fn shift_extension_spans_view_order_and_keeps_anchor() {
        let view = vec![10, 11, 12, 13, 14];
        let mut sel = Selection::default();
        // Cursor at 11, Shift+Right twice: 11..13.
        sel.extend_to(&view, 11, 12);
        sel.extend_to(&view, 11, 13);
        assert_eq!(sel.batch(&view, 13), vec![11, 12, 13]);
        // Shift back above the anchor: the span FLIPS — the abandoned
        // tail (12, 13) deselects (validator finding: grow-only spans
        // could never shrink).
        sel.extend_to(&view, 11, 10);
        assert_eq!(sel.batch(&view, 10), vec![10, 11]);
        // Shrink within the same direction too.
        sel.extend_to(&view, 11, 13);
        sel.extend_to(&view, 11, 12);
        assert_eq!(sel.batch(&view, 12), vec![11, 12]);
    }

    #[test]
    fn select_all_is_exactly_the_view_and_batch_is_view_ordered() {
        let view = vec![9, 4, 7]; // sorted view order, arbitrary ids
        let mut sel = Selection::default();
        sel.select_all(&view);
        assert_eq!(
            sel.batch(&view, 4),
            vec![9, 4, 7],
            "view order, not id order"
        );
        // Filter narrows the view: batch narrows with it.
        assert_eq!(sel.batch(&[7, 9], 4), vec![7, 9]);
    }

    /// The rule of 2026-09-06 (brief 002, user decision "file manager
    /// style", answer 3), in one test, because it is two halves of one
    /// sentence: a span whose anchor arms on THIS gesture IS the whole
    /// selection, and a span that continues a live anchor replaces only
    /// itself.
    ///
    /// This is the rewrite of `toggle_and_anchor_reset`, which asserted
    /// the opposite of the last line — `[1, 2, 3, 4]`, "unions with
    /// prior" — and was the only place the union rule of commit 7ed0949
    /// was ever written down. That rule is what put a previous export's
    /// four frames into the next video (the user's report of 2026-09-06).
    #[test]
    fn a_fresh_span_replaces_the_whole_selection_a_continued_one_replaces_its_span() {
        let view = vec![1, 2, 3, 4];
        let mut sel = Selection::default();
        sel.toggle(2);
        sel.toggle(4);
        sel.toggle(2); // off again
        assert_eq!(sel.batch(&view, 1), vec![4]);
        // The anchor is the LAST interaction point (2, even though it was
        // toggled off — file-manager convention): the next span runs from
        // there, re-including it. The anchor is LIVE, so this span
        // continues it and the Ctrl-toggled 4 survives beside it.
        sel.extend_to(&view, 4, 3);
        assert_eq!(sel.batch(&view, 3), vec![2, 3, 4], "a continued span");
        // Ctrl-navigation (or a click) drops the anchor. The next span
        // therefore arms FRESH — and a fresh span is the whole selection,
        // Ctrl-added frames included.
        sel.reset_anchor();
        sel.extend_to(&view, 1, 2);
        assert_eq!(
            sel.batch(&view, 2),
            vec![1, 2],
            "a fresh span REPLACES what was selected before it"
        );
    }

    /// The same rule for the burst spans (Shift+`[` / Shift+`]`): a fresh
    /// one replaces, Ctrl+Shift+B's frames included. The CONTINUED case —
    /// `select_group` arms the anchor, so the Shift+`]` after it extends
    /// from that burst — is pinned at the end of
    /// `select_group_is_additive_idempotent_and_view_scoped` and must stay
    /// as it is.
    #[test]
    fn a_fresh_burst_span_replaces_ctrl_added_frames() {
        let view = ids(0..=12);
        let mut sel = Selection::default();
        sel.select_group(&view, 8, groups); // Ctrl+Shift+B on B
        assert_eq!(sel.batch(&view, 8), vec![7, 8, 9]);
        sel.reset_anchor(); // Ctrl+`]` to C
        sel.extend_bursts(&view, 10, 12, groups);
        assert_eq!(
            sel.batch(&view, 12),
            ids(10..=12),
            "a fresh burst span is the whole selection, B gone"
        );
    }

    /// Rule 1 (brief 002): a plain cursor move empties the selection — the
    /// cursor is then the batch and the status count is silent — and the
    /// span that follows starts fresh, so it cannot resurrect what the
    /// move dropped.
    #[test]
    fn plain_navigation_collapses_and_the_next_span_starts_fresh() {
        let view = ids(0..=9);
        let mut sel = Selection::default();
        sel.toggle(2);
        sel.extend_to(&view, 2, 4);
        assert_eq!(sel.count_in_view(&view), 3);
        sel.collapse(); // a plain Right / `]` / PgDn / Y-advance
        assert!(sel.is_empty(), "the selection is gone, not folded");
        assert_eq!(sel.count_in_view(&view), 0, "the count goes silent");
        assert_eq!(sel.batch(&view, 3), vec![3], "the cursor is the batch");
        sel.extend_to(&view, 6, 7);
        assert_eq!(
            sel.batch(&view, 7),
            vec![6, 7],
            "and the next span is fresh"
        );
    }

    /// The ORDER inside `extend_to`: the base is cleared only once the
    /// span is really going to be set. A Shift+arrow whose anchor or
    /// target is filtered out spans nothing, and "spans nothing" must mean
    /// "changes nothing" — clearing first would make a no-op gesture eat
    /// the selection.
    #[test]
    fn a_span_that_resolves_to_nothing_leaves_a_fresh_selection_untouched() {
        let view = vec![1usize, 2, 3];
        let mut sel = Selection::default();
        sel.toggle(5); // selected, and not in this view
        sel.reset_anchor();
        sel.extend_to(&view, 1, 9); // 9 is filtered out: no span
        assert!(sel.is_selected(5), "the no-op span dropped the selection");
        assert_eq!(sel.count_in_view(&[1, 2, 3, 5]), 1);
    }

    /// Ctrl+Space is `toggle`, and `toggle` stays ADDITIVE across the
    /// Ctrl-navigation between two presses: that is the whole point of the
    /// pair — the keyboard's way to build a discontiguous selection
    /// (brief 002 R4, AC4).
    #[test]
    fn ctrl_space_toggles_additively_across_ctrl_navigation() {
        let view = ids(0..=9);
        let mut sel = Selection::default();
        sel.toggle(2);
        sel.reset_anchor(); // Ctrl+Right
        sel.toggle(4);
        assert_eq!(sel.batch(&view, 4), vec![2, 4], "additive");
        sel.toggle(4);
        assert_eq!(sel.batch(&view, 2), vec![2], "and it takes away again");
    }

    #[test]
    fn reset_clears_everything() {
        let mut sel = Selection::default();
        sel.select_all(&[1, 2, 3]);
        sel.reset();
        assert!(sel.is_empty());
        assert_eq!(sel.len(), 0);
        assert_eq!(sel.batch(&[1, 2, 3], 2), vec![2]);
    }

    /// The status-bar count and the panel's batch must never disagree — the
    /// spec claims they "match `batch()` exactly", so pin it rather than
    /// trusting two copies of the same filter to stay in step.
    /// The ONE state where the count and the batch differ, pinned so the
    /// doc comment above cannot drift back into claiming otherwise: a
    /// selection whose every member is filtered out counts 0, while
    /// `batch` falls back to the cursor alone and returns 1 (QE finding
    /// 2026-08-28 — two call sites were written as if these agreed).
    #[test]
    fn a_selection_filtered_entirely_out_of_view_counts_zero_but_batches_the_cursor() {
        let view = vec![1usize, 2, 3];
        let mut sel = Selection::default();
        sel.toggle(99); // selected, and not in the view
        assert!(!sel.is_empty());
        assert_eq!(sel.count_in_view(&view), 0);
        assert_eq!(sel.batch(&view, 2), vec![2], "the cursor alone");
    }

    /// Issue #55 fixtures: a contiguous capture-sorted view — the everyday
    /// case. Ids double as view positions: single 0, burst A = 1..=5,
    /// single 6, burst B = 7..=9, burst C = 10..=12.
    fn groups(id: usize) -> Option<usize> {
        match id {
            1..=5 => Some(0),
            7..=9 => Some(1),
            10..=12 => Some(2),
            _ => None,
        }
    }

    fn ids(r: std::ops::RangeInclusive<usize>) -> Vec<usize> {
        r.collect()
    }

    #[test]
    fn shift_bracket_selects_whole_bursts_and_shrinks_by_burst() {
        let view = ids(0..=12);
        let mut sel = Selection::default();
        // On A's opener (where `]` leaves you), Shift+`]` lands on the
        // single at 6 and takes ALL of A plus that single: the heron in
        // one press.
        sel.extend_bursts(&view, 1, 6, groups);
        assert_eq!(sel.batch(&view, 6), ids(1..=6));
        // Again: B, whole.
        sel.extend_bursts(&view, 6, 7, groups);
        assert_eq!(sel.batch(&view, 7), ids(1..=9));
        // Shift+`[` from B's opener lands on the single: B drops whole —
        // never "A plus the first frame of B".
        sel.extend_bursts(&view, 7, 6, groups);
        assert_eq!(sel.batch(&view, 6), ids(1..=6));
        // Back on A's opener: just A.
        sel.extend_bursts(&view, 6, 1, groups);
        assert_eq!(sel.batch(&view, 1), ids(1..=5));
        // Past the anchor burst: flips, still whole (A plus single 0).
        sel.extend_bursts(&view, 1, 0, groups);
        assert_eq!(sel.batch(&view, 0), ids(0..=5));
    }

    #[test]
    fn shift_bracket_from_mid_burst_takes_the_whole_anchor_burst() {
        let view = ids(0..=12);
        let mut sel = Selection::default();
        // Frame 3 of A, Shift+`]` → the single at 6: all of A, not 3..5.
        sel.extend_bursts(&view, 3, 6, groups);
        assert_eq!(sel.batch(&view, 6), ids(1..=6));
        // Mid-B, Shift+`[` re-anchors on B's opener (as `[` does): just B
        // — the "select this burst and go to its opener" move.
        let mut sel = Selection::default();
        sel.extend_bursts(&view, 8, 7, groups);
        assert_eq!(sel.batch(&view, 7), vec![7, 8, 9]);
        // A second Shift+`[` crosses to the single: B plus 6.
        sel.extend_bursts(&view, 7, 6, groups);
        assert_eq!(sel.batch(&view, 6), vec![6, 7, 8, 9]);
        // At the end of the view `]` clamps: from == to selects the
        // burst under the cursor, whole.
        let mut sel = Selection::default();
        sel.extend_bursts(&view, 11, 12, groups);
        assert_eq!(sel.batch(&view, 12), vec![10, 11, 12]);
    }

    #[test]
    fn shift_arrows_after_a_burst_span_are_frame_precise_from_the_bursts_edge() {
        let view = ids(0..=12);
        let mut sel = Selection::default();
        sel.extend_bursts(&view, 3, 7, groups); // A whole + B whole, cursor 7
        assert_eq!(sel.batch(&view, 7), ids(1..=9));
        // Shift+Right: "A plus the first two of B" — anchor..cursor with the
        // anchor at A's FIRST frame, not at 3 where the gesture started.
        sel.extend_to(&view, 7, 8);
        assert_eq!(sel.batch(&view, 8), ids(1..=8));
        // Backwards the anchor sits at its burst's LAST frame.
        let mut sel = Selection::default();
        sel.extend_bursts(&view, 8, 6, groups); // B whole + single 6
        assert_eq!(sel.batch(&view, 6), vec![6, 7, 8, 9]);
        sel.extend_to(&view, 6, 5);
        assert_eq!(sel.batch(&view, 5), ids(5..=9));
    }

    #[test]
    fn interleaved_bodies_select_the_view_range_between_the_bursts() {
        // Body 1's burst (1, 3, 5) interleaved with body 2's (2, 4, 6).
        let view = ids(0..=7);
        let g = |id: usize| match id {
            1 | 3 | 5 => Some(0),
            2 | 4 | 6 => Some(1),
            _ => None,
        };
        let mut sel = Selection::default();
        sel.extend_bursts(&view, 3, 7, g);
        // Group 0 widens to its first member; everything between comes
        // along — a view range, as Shift+arrows would take it.
        assert_eq!(sel.batch(&view, 7), ids(1..=7));
        // A filtered-out anchor spans nothing (same rule as extend_to).
        let mut sel = Selection::default();
        sel.extend_bursts(&[0, 2, 4], 3, 4, g);
        assert!(sel.is_empty());
    }

    #[test]
    fn select_group_is_additive_idempotent_and_view_scoped() {
        let view = ids(0..=12);
        let mut sel = Selection::default();
        sel.select_group(&view, 8, groups);
        assert_eq!(sel.batch(&view, 8), vec![7, 8, 9]);
        sel.select_group(&view, 8, groups); // a double-tap changes nothing
        assert_eq!(sel.count_in_view(&view), 3);
        // A single selects itself; additive with the burst already held.
        sel.select_group(&view, 0, groups);
        assert_eq!(sel.batch(&view, 0), vec![0, 7, 8, 9]);
        // Filtered view: only the visible members (a Picked filter stamps
        // the keepers, never the hidden rejects).
        let filtered = vec![0, 1, 3, 5, 7, 9];
        let mut sel = Selection::default();
        sel.select_group(&filtered, 3, groups);
        assert_eq!(sel.batch(&filtered, 3), vec![1, 3, 5]);
        assert!(!sel.is_selected(2) && !sel.is_selected(4));
        // The cursor itself filtered out: nothing under it.
        let mut sel = Selection::default();
        sel.select_group(&filtered, 2, groups);
        assert!(sel.is_empty());
        // Then Shift+`]` extends FROM this burst (the anchor armed here).
        let mut sel = Selection::default();
        sel.select_group(&view, 8, groups);
        sel.extend_bursts(&view, 8, 10, groups);
        assert_eq!(sel.batch(&view, 10), ids(7..=12));
    }

    #[test]
    fn count_in_view_agrees_with_batch() {
        let view: Vec<usize> = (0..10).collect();
        let mut sel = Selection::default();
        // Empty selection: the count is silent-0 even though `batch` falls
        // back to the cursor alone. This asymmetry is deliberate.
        assert_eq!(sel.count_in_view(&view), 0);
        assert_eq!(sel.batch(&view, 3), vec![3]);
        // Non-empty: exactly the batch length, INCLUDING when a selected id
        // is filtered out of the view (99 below) — what you see is what you
        // stamp, and what the status bar counts.
        sel.toggle(2);
        sel.toggle(5);
        sel.toggle(99);
        assert_eq!(sel.count_in_view(&view), 2);
        assert_eq!(sel.count_in_view(&view), sel.batch(&view, 3).len());
        // A span counts too, not just committed toggles.
        sel.clear();
        sel.extend_to(&view, 1, 4);
        assert_eq!(sel.count_in_view(&view), sel.batch(&view, 1).len());
        assert!(sel.count_in_view(&view) >= 4);
    }
}
