//! Multi-selection over the filtered view (`ui-grid.md`: Shift+arrows
//! extend, Ctrl+A selects the filtered set, Ctrl/Shift-click join in the
//! panel step). Selected images are IDs (session-stable); ranges are
//! resolved against VIEW positions at extension time, so a filter change
//! never re-interprets an old range.
//!
//! The cursor is always implicitly part of the batch the IPTC panel acts
//! on: an empty selection means "the cursor image" (Photo Mechanic
//! convention — no dead panel on a bare cursor).

use std::collections::HashSet;

#[derive(Default)]
pub struct Selection {
    /// Committed selections: Ctrl-toggles, select-all, and folded spans.
    base: HashSet<usize>,
    /// The CURRENT Shift-span (anchor..cursor): each extension REPLACES it
    /// (validator finding: a grow-only span made shrinking impossible —
    /// Shift+Right x3 then Shift+Left left the abandoned tail selected).
    /// Folded into `base` when the anchor resets (plain navigation).
    span: HashSet<usize>,
    /// Range anchor (image id): where Shift-extension started. Plain
    /// navigation moves the cursor and RESETS the anchor; Shift+arrows
    /// keep it and re-span anchor..cursor.
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

    /// Ctrl+A: exactly the current filtered view.
    pub fn select_all(&mut self, view: &[usize]) {
        self.base = view.iter().copied().collect();
        self.span.clear();
        self.anchor = None;
    }

    /// Shift+arrow / Shift+click: span anchor..cursor over view positions.
    /// The anchor arms on the first extension (from the pre-move cursor)
    /// and persists across further extensions. Each extension REPLACES the
    /// current span (so it can shrink and flip) and unions with prior
    /// toggles/spans (PM behavior).
    pub fn extend_to(&mut self, view: &[usize], from: usize, to: usize) {
        let anchor = *self.anchor.get_or_insert(from);
        let (Some(a), Some(b)) = (
            view.iter().position(|id| *id == anchor),
            view.iter().position(|id| *id == to),
        ) else {
            return; // anchor or target filtered out: nothing to span
        };
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.span = view[lo..=hi].iter().copied().collect();
    }

    /// Plain (non-extending) cursor movement resets the range anchor; the
    /// selected set itself is untouched (arrows never deselect — the live
    /// span is committed into the base, Esc/click semantics come with the
    /// panel step).
    pub fn reset_anchor(&mut self) {
        self.base.extend(self.span.drain());
        self.anchor = None;
    }

    /// Ctrl+click toggle (commits any live span first — the toggle acts on
    /// the selection as the user sees it).
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

    /// Session swap / folder change: stale ids must never leak.
    pub fn reset(&mut self) {
        self.clear();
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

    #[test]
    fn toggle_and_anchor_reset() {
        let view = vec![1, 2, 3, 4];
        let mut sel = Selection::default();
        sel.toggle(2);
        sel.toggle(4);
        sel.toggle(2); // off again
        assert_eq!(sel.batch(&view, 1), vec![4]);
        // The anchor is the LAST interaction point (2, even though it was
        // toggled off — file-manager convention): the next span runs from
        // there, re-including it.
        sel.extend_to(&view, 4, 3);
        assert_eq!(sel.batch(&view, 3), vec![2, 3, 4]);
        sel.reset_anchor();
        sel.extend_to(&view, 1, 2);
        assert_eq!(sel.batch(&view, 2), vec![1, 2, 3, 4], "unions with prior");
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
}
