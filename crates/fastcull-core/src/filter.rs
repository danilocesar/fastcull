//! Filter & sort predicates over the session (`specs/modules/ui-grid.md`,
//! M5 decisions 2026-07-25): single-choice pick filters, capture-time or
//! filename sort, live counts, and the cursor rules that make the
//! inbox-zero loop (filter Unmarked, Y/N until empty) work exactly.
//!
//! Pure functions over parallel slices — the app owns the state, this
//! module owns the semantics, and every rule is unit-tested here.

use crate::catalog::PickState;

/// Single-choice filter chips (user decision: combinations dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickFilter {
    #[default]
    All,
    Picked,
    Rejected,
    Unmarked,
}

impl PickFilter {
    pub fn matches(self, pick: PickState) -> bool {
        match self {
            PickFilter::All => true,
            PickFilter::Picked => pick == PickState::Picked,
            PickFilter::Rejected => pick == PickState::Rejected,
            PickFilter::Unmarked => pick == PickState::Unmarked,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Capture time (EXIF sort key, lexicographic == chronological);
    /// images without one sort after those with one, by name. Default:
    /// keeps bursts adjacent.
    #[default]
    CaptureTime,
    Filename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewQuery {
    pub filter: PickFilter,
    pub sort: SortKey,
    pub ascending: bool,
}

impl Default for ViewQuery {
    fn default() -> Self {
        Self {
            filter: PickFilter::All,
            sort: SortKey::CaptureTime,
            ascending: true,
        }
    }
}

/// Live chip counts (status for "am I done?" — persona: the progress bar of
/// the evening).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub all: usize,
    pub picked: usize,
    pub rejected: usize,
    pub unmarked: usize,
}

pub fn counts(picks: &[PickState]) -> Counts {
    let mut c = Counts {
        all: picks.len(),
        ..Default::default()
    };
    for p in picks {
        match p {
            PickState::Picked => c.picked += 1,
            PickState::Rejected => c.rejected += 1,
            PickState::Unmarked => c.unmarked += 1,
        }
    }
    c
}

/// Compute the display view: image indexes passing `filter`, ordered by
/// `sort`/`ascending`. `capture_keys[i]` is `ExifSummary::sort_key()` (None
/// until metadata loads or when absent); `names[i]` breaks ties and orders
/// keyless images.
///
/// `metadata_complete` says whether every image's metadata job has finished.
/// **While it is false the view is ordered by FILENAME regardless of
/// `query.sort`** (issue #25). The capture sort puts keyed images before
/// still-keyless ones, so applying it mid-load makes the order churn on
/// every arrival — and EXIF is read inside the per-file thumbnail job, so
/// "mid-load" is the WHOLE load (measured: ~15 s for 3,000 files locally,
/// longer off a card), not a startup blink. Two things rode that churn:
/// navigation resolved against an order that no longer existed a frame
/// later (a single `right` landed 870 frames from the intended second
/// image), and — worse — the untouched cursor is re-pinned to the view HEAD
/// on every refresh while marks write to that same cursor, so the head
/// changing identity between a photographer's decision and their keypress
/// lands `Y`/`N` on a frame they never looked at. Reproduced with no input
/// at all: the cursor moved from image 0 to image 2000 mid-load.
///
/// Filename order is available from the directory scan (~13 ms for 3,000
/// files), is stable under the user's hands, and for a single card in
/// shooting order IS capture order — so the eventual re-sort is invisible
/// in the common case. When the last job lands, the real sort applies once;
/// a claimed cursor keeps its image and the viewport re-anchors on it.
pub fn view(
    picks: &[PickState],
    names: &[String],
    capture_keys: &[Option<String>],
    query: &ViewQuery,
    metadata_complete: bool,
) -> Vec<usize> {
    let mut ids: Vec<usize> = (0..picks.len())
        .filter(|i| query.filter.matches(picks[*i]))
        .collect();
    // Provisional order while the metadata is still streaming (see above).
    let sort = if metadata_complete {
        query.sort
    } else {
        SortKey::Filename
    };
    match sort {
        SortKey::Filename => ids.sort_by(|a, b| names[*a].cmp(&names[*b])),
        SortKey::CaptureTime => ids.sort_by(|a, b| match (&capture_keys[*a], &capture_keys[*b]) {
            (Some(ka), Some(kb)) => ka.cmp(kb).then_with(|| names[*a].cmp(&names[*b])),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => names[*a].cmp(&names[*b]),
        }),
    }
    if !query.ascending {
        ids.reverse();
    }
    ids
}

/// The view in the user's TRUE sort, never issue #25's provisional filename
/// order — for the two consumers that must not see it:
///
/// - **burst grouping**, because a burst is a fact about capture times and
///   grouping by filename would invent groups;
/// - **Copy Picks**, because `{seq}` is baked into permanent filenames, the
///   one irreversible artifact this app produces.
///
/// A named function rather than a bare `true` at the call sites: that flag
/// reads as "metadata is complete", which is exactly what it is NOT where
/// these two call it — they are asserting "ignore the provisional rule".
/// Two mutation survivors (QE G3, G4) lived in that ambiguity.
pub fn view_true_sort(
    picks: &[PickState],
    names: &[String],
    capture_keys: &[Option<String>],
    query: &ViewQuery,
) -> Vec<usize> {
    view(picks, names, capture_keys, query, true)
}

/// Cursor rule for LIVE removal (spec, blocking gap closed pre-M5): after
/// the image at `old_pos` in `old_view` left the view, the cursor goes to
/// the image now at the same position (the "next" one slid in), else the
/// previous, else none. Returns the new cursor IMAGE ID.
pub fn cursor_after_removal(new_view: &[usize], old_pos: usize) -> Option<usize> {
    if new_view.is_empty() {
        return None;
    }
    Some(new_view[old_pos.min(new_view.len() - 1)])
}

/// Cursor rule after MARKING the cursor image (spec, persona gap G1
/// 2026-07-25): net movement is exactly one image, always. If the mark
/// removed the image from the filtered view, the removal rule IS the
/// advance; auto-advance applies only when the image stays in view.
/// `old_pos` is the cursor's position in the pre-mark view.
pub fn cursor_after_mark(
    marked_id: usize,
    old_pos: usize,
    new_view: &[usize],
    auto_advance: bool,
) -> Option<usize> {
    match new_view.iter().position(|id| *id == marked_id) {
        // Image left the view: the slide-in IS the advance.
        None => cursor_after_removal(new_view, old_pos),
        // Image stayed: advance one (clamped at the end) or stay put.
        Some(pos) if auto_advance => Some(new_view[(pos + 1).min(new_view.len() - 1)]),
        Some(pos) => Some(new_view[pos]),
    }
}

/// Cursor after ANY view recompute: the one place the untouched-cursor rule
/// and issue #25's load-settled re-sort are reconciled.
///
/// Issue #4 says that before the user's first interaction the cursor is "the
/// first image of the view", not a pinned id — so a filter change, a sort
/// change or streaming membership updates snap it to the new head, and a
/// folder never opens with the cursor stranded mid-grid.
///
/// **Once the folder has finished loading, ENGINE events stop moving it**
/// (user decision 2026-07-31: "during the loading phase, whatever is
/// currently selected stays selected, and stays visible on the screen").
///
/// The rule is deliberately a STATE, not an edge. A first attempt made the
/// keep fire only on the load-settled transition, which held the cursor for
/// exactly one refresh: `cursor_touched` was still false afterwards, so the
/// next background decode or sidecar write re-applied the head-follow rule
/// and snapped the photograph away again — with no input, after loading,
/// which is the whole defect class issue #25 exists to remove (validator
/// FAIL, 2026-07-31, reproduced live). So the discriminator is not "is this
/// the flip" but **"did the USER ask for a different view?"**:
///
/// - `user_changed_query` — a filter chip or the sort control. Pre-touch
///   these still snap to the new view's first image, exactly as issue #4
///   specifies; the user asked to see a different set, so showing them the
///   start of it is right.
/// - Everything else is the ENGINE talking: streaming metadata, the
///   load-settled re-sort, a decode landing, a sidecar arriving. While the
///   folder is still loading these keep the head (the order is provisional
///   and stable, so the head IS the cursor); once it has loaded they must
///   leave the photograph alone.
///
/// The cost is accepted knowingly: on a folder whose filename order runs
/// contrary to capture order, an untouched cursor that started at the top
/// ends up mid-grid once the real order lands — the stranding issue #4 was
/// written to prevent — and the viewport scrolls to keep it in view.
pub fn cursor_after_recompute(
    old_view: &[usize],
    old_cursor: Option<usize>,
    new_view: &[usize],
    cursor_touched: bool,
    metadata_complete: bool,
    user_changed_query: bool,
) -> Option<usize> {
    let follow_head = !cursor_touched && (user_changed_query || !metadata_complete);
    if follow_head {
        return new_view.first().copied();
    }
    cursor_after_filter_change(old_view, old_cursor, new_view)
}

/// Cursor rule for a FILTER change (spec): keep the cursor image if it
/// survived; else the nearest survivor from the old view (scanning outward
/// from the old position); else the first image of the new view.
pub fn cursor_after_filter_change(
    old_view: &[usize],
    old_cursor: Option<usize>,
    new_view: &[usize],
) -> Option<usize> {
    if new_view.is_empty() {
        return None;
    }
    let Some(cursor_id) = old_cursor else {
        return new_view.first().copied();
    };
    if new_view.contains(&cursor_id) {
        return Some(cursor_id);
    }
    if let Some(pos) = old_view.iter().position(|id| *id == cursor_id) {
        for dist in 1..=old_view.len() {
            for candidate in [pos.checked_sub(dist), pos.checked_add(dist)]
                .into_iter()
                .flatten()
            {
                if let Some(id) = old_view.get(candidate) {
                    if new_view.contains(id) {
                        return Some(*id);
                    }
                }
            }
        }
    }
    new_view.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use PickState::*;

    fn fixture() -> (Vec<PickState>, Vec<String>, Vec<Option<String>>) {
        // 6 images; capture order deliberately differs from name order.
        let picks = vec![Picked, Rejected, Unmarked, Picked, Unmarked, Rejected];
        let names: Vec<String> = ["f.ARW", "e.ARW", "d.ARW", "c.ARW", "b.ARW", "a.ARW"]
            .map(String::from)
            .to_vec();
        let keys: Vec<Option<String>> = vec![
            Some("2026:07:22 10:00:01.000".into()),
            Some("2026:07:22 10:00:02.000".into()),
            None, // metadata not loaded yet
            Some("2026:07:22 09:59:59.000".into()),
            Some("2026:07:22 10:00:03.000".into()),
            None,
        ];
        (picks, names, keys)
    }

    #[test]
    fn counts_are_live_totals() {
        let (picks, ..) = fixture();
        let c = counts(&picks);
        assert_eq!((c.all, c.picked, c.rejected, c.unmarked), (6, 2, 2, 2));
    }

    #[test]
    fn every_filter_sort_combination() {
        let (picks, names, keys) = fixture();
        for (filter, expected_ids) in [
            (PickFilter::All, vec![0, 1, 2, 3, 4, 5]),
            (PickFilter::Picked, vec![0, 3]),
            (PickFilter::Rejected, vec![1, 5]),
            (PickFilter::Unmarked, vec![2, 4]),
        ] {
            for sort in [SortKey::CaptureTime, SortKey::Filename] {
                for ascending in [true, false] {
                    let q = ViewQuery {
                        filter,
                        sort,
                        ascending,
                    };
                    let v = view(&picks, &names, &keys, &q, true);
                    let mut sorted_ids = v.clone();
                    sorted_ids.sort_unstable();
                    assert_eq!(sorted_ids, expected_ids, "{q:?} membership");
                    let mut rev = view(
                        &picks,
                        &names,
                        &keys,
                        &ViewQuery {
                            ascending: !ascending,
                            ..q
                        },
                        true,
                    );
                    rev.reverse();
                    assert_eq!(v, rev, "{q:?} descending is exact reverse");
                }
            }
        }
        // Capture-time order: keyed images chronologically, keyless after,
        // by name.
        let q = ViewQuery::default();
        assert_eq!(
            view(&picks, &names, &keys, &q, true),
            vec![3, 0, 1, 4, 5, 2]
        );
        // Filename order.
        let q = ViewQuery {
            sort: SortKey::Filename,
            ..q
        };
        assert_eq!(
            view(&picks, &names, &keys, &q, true),
            vec![5, 4, 3, 2, 1, 0]
        );
    }

    /// Issue #25: while metadata streams, the order must not move under the
    /// user's hands. Feed the capture keys in one at a time and assert the
    /// view is IDENTICAL at every step — then that the real sort applies
    /// once, when the last job lands.
    ///
    /// Before this, each arrival re-sorted: a keyed image jumps ahead of
    /// every still-keyless one, so the HEAD changes identity repeatedly for
    /// the whole load. Navigation resolved against an order that no longer
    /// existed a frame later, and marks — which write to the untouched
    /// cursor, itself re-pinned to that head — could land on a frame the
    /// photographer never looked at.
    #[test]
    fn provisional_order_is_stable_while_metadata_streams() {
        let (picks, names, keys) = fixture();
        let q = ViewQuery::default(); // CaptureTime — the default sort
        let unknown: Vec<Option<String>> = vec![None; keys.len()];

        // Nothing known yet: filename order, despite the capture-time query.
        let baseline = view(&picks, &names, &unknown, &q, false);
        let by_name = view(
            &picks,
            &names,
            &unknown,
            &ViewQuery {
                sort: SortKey::Filename,
                ..q
            },
            true,
        );
        assert_eq!(baseline, by_name, "provisional order must be by filename");

        // Keys landing one at a time must not reorder anything.
        let mut streaming = unknown.clone();
        for (i, key) in keys.iter().enumerate() {
            streaming[i] = key.clone();
            assert_eq!(
                view(&picks, &names, &streaming, &q, false),
                baseline,
                "the view moved while metadata was still streaming (after key {i})"
            );
        }

        // The last job lands: the real sort applies, exactly once.
        let settled = view(&picks, &names, &keys, &q, true);
        assert_eq!(settled, vec![3, 0, 1, 4, 5, 2], "capture order at the end");
        assert_ne!(
            settled, baseline,
            "fixture must actually reorder, or the stability check above is vacuous"
        );
    }

    /// The load-settled re-sort must not move the user's photograph, even
    /// before their first keypress (user decision 2026-07-31). Every OTHER
    /// recompute keeps issue #4's rule that an untouched cursor is "the
    /// first image of the view".
    #[test]
    fn engine_events_stop_moving_an_untouched_cursor_once_loaded() {
        // Provisional (filename) order, then the settled capture order.
        let loading = vec![0, 1, 2, 3, 4, 5];
        let settled = vec![3, 0, 1, 4, 5, 2];
        let cursor = Some(0); // the head of the provisional order
                              // (old_view, old_cursor, new_view, touched, complete, user_query)
        let after = |touched, complete, user_query| {
            cursor_after_recompute(&loading, cursor, &settled, touched, complete, user_query)
        };

        // THE FLIP, and EVERY engine recompute after it: the image is kept,
        // never re-pinned to the new head (image 3). This must be a STATE,
        // not a one-shot edge — the edge version held for a single refresh
        // and the next decode event snapped the photograph away.
        assert_eq!(
            after(false, true, false),
            Some(0),
            "an engine event after loading must not move the photograph"
        );
        // A touched cursor is kept too, as it always was.
        assert_eq!(after(true, true, false), Some(0));

        // WHILE LOADING an engine recompute still follows the head — the
        // provisional order is stable, so the head IS the cursor, and a
        // folder opens at its first image (issue #4).
        assert_eq!(after(false, false, false), Some(3));

        // The USER asking for a different view still snaps pre-touch, loaded
        // or not: they asked to see a different set, so show them its start.
        assert_eq!(after(false, true, true), Some(3), "filter/sort chip");
        assert_eq!(after(false, false, true), Some(3));
        // ...but never once they have claimed the cursor.
        assert_eq!(after(true, true, true), Some(0));

        // An empty result is honest in every combination.
        for touched in [true, false] {
            for complete in [true, false] {
                for user_query in [true, false] {
                    assert_eq!(
                        cursor_after_recompute(
                            &loading,
                            cursor,
                            &[],
                            touched,
                            complete,
                            user_query
                        ),
                        None
                    );
                }
            }
        }
        // A cursor that did not survive falls back rather than pointing at
        // an image no longer in the view.
        let without = vec![3, 1, 4, 5, 2];
        assert_eq!(
            cursor_after_recompute(&loading, cursor, &without, false, true, false),
            Some(1),
            "nearest survivor, not a dangling id"
        );
    }

    /// `view_true_sort` ignores the provisional order even mid-load — the
    /// property burst grouping and Copy Picks depend on.
    #[test]
    fn view_true_sort_never_uses_the_provisional_order() {
        let (picks, names, keys) = fixture();
        let q = ViewQuery::default(); // CaptureTime
        assert_eq!(
            view_true_sort(&picks, &names, &keys, &q),
            view(&picks, &names, &keys, &q, true),
            "must equal the settled order"
        );
        assert_ne!(
            view_true_sort(&picks, &names, &keys, &q),
            view(&picks, &names, &keys, &q, false),
            "must DIFFER from the provisional order, or this proves nothing"
        );
    }

    /// The provisional order overrides the SORT only — never membership.
    /// A filter chip must keep filtering while the folder loads.
    #[test]
    fn provisional_order_still_respects_the_filter_and_direction() {
        let (picks, names, keys) = fixture();
        for filter in [
            PickFilter::All,
            PickFilter::Picked,
            PickFilter::Rejected,
            PickFilter::Unmarked,
        ] {
            let q = ViewQuery {
                filter,
                ..Default::default()
            };
            let loading = view(&picks, &names, &keys, &q, false);
            let mut members = loading.clone();
            members.sort_unstable();
            let mut expected = view(&picks, &names, &keys, &q, true);
            expected.sort_unstable();
            assert_eq!(members, expected, "{filter:?} membership must not change");
            // Descending still reverses the provisional order.
            let desc = view(
                &picks,
                &names,
                &keys,
                &ViewQuery {
                    ascending: false,
                    ..q
                },
                false,
            );
            let mut rev = loading.clone();
            rev.reverse();
            assert_eq!(desc, rev, "{filter:?} descending while loading");
        }
    }

    /// The inbox-zero loop: filter Unmarked, mark everything, view empties,
    /// cursor always lands on the next unmarked image.
    #[test]
    fn inbox_zero_loop_cursor_flow() {
        let (mut picks, names, keys) = fixture();
        let q = ViewQuery {
            filter: PickFilter::Unmarked,
            ..Default::default()
        };
        let mut v = view(&picks, &names, &keys, &q, true);
        assert_eq!(v, vec![4, 2]); // capture order: b.ARW then keyless d.ARW
        let mut cursor = Some(v[0]);

        // Mark image 4 as picked: it leaves the view; cursor slides to 2.
        picks[4] = Picked;
        let old_pos = v.iter().position(|i| Some(*i) == cursor).unwrap();
        v = view(&picks, &names, &keys, &q, true);
        cursor = cursor_after_removal(&v, old_pos);
        assert_eq!(v, vec![2]);
        assert_eq!(cursor, Some(2));

        // Mark image 2 rejected: view empties, cursor none — inbox zero.
        picks[2] = Rejected;
        let old_pos = v.iter().position(|i| Some(*i) == cursor).unwrap();
        v = view(&picks, &names, &keys, &q, true);
        cursor = cursor_after_removal(&v, old_pos);
        assert!(v.is_empty());
        assert_eq!(cursor, None);
    }

    #[test]
    fn removal_mid_view_slides_to_next_then_previous_at_end() {
        // View of ids [10, 20, 30]; removing the middle slides 30 into pos 1.
        assert_eq!(cursor_after_removal(&[10, 30], 1), Some(30));
        // Removing the LAST falls back to the new last (previous image).
        assert_eq!(cursor_after_removal(&[10, 20], 2), Some(20));
        assert_eq!(cursor_after_removal(&[], 0), None);
    }

    /// Persona gap G1: marking must move the cursor exactly one image —
    /// the removal rule and auto-advance must never compose (double-skip).
    #[test]
    fn mark_advances_exactly_one_image() {
        // Filter=Unmarked view [10, 20, 30]; marking 10 removes it: the
        // new view's slide-in (20 at pos 0) IS the advance.
        assert_eq!(cursor_after_mark(10, 0, &[20, 30], true), Some(20));
        // Filter=All: image stays in view; auto-advance moves one right.
        assert_eq!(cursor_after_mark(10, 0, &[10, 20, 30], true), Some(20));
        // At the end of the view: clamp, don't wrap.
        assert_eq!(cursor_after_mark(30, 2, &[10, 20, 30], true), Some(30));
        // Auto-advance off (U / future config): stay on the image.
        assert_eq!(cursor_after_mark(10, 0, &[10, 20, 30], false), Some(10));
        // Removing the LAST image of the view: previous slides under.
        assert_eq!(cursor_after_mark(30, 2, &[10, 20], true), Some(20));
        // View emptied: inbox zero.
        assert_eq!(cursor_after_mark(10, 0, &[], true), None);
    }

    #[test]
    fn filter_change_keeps_or_finds_nearest_cursor() {
        let old = vec![1, 2, 3, 4, 5];
        // Survivor keeps the cursor.
        assert_eq!(cursor_after_filter_change(&old, Some(3), &[3, 5]), Some(3));
        // Nearest survivor outward from old position (2 and 4 gone -> 1).
        assert_eq!(
            cursor_after_filter_change(&old, Some(3), &[1, 5]),
            Some(1),
            "distance 2 below beats distance 2 above by scan order"
        );
        // Nothing near: first of new view.
        assert_eq!(cursor_after_filter_change(&old, Some(9), &[7, 8]), Some(7));
        assert_eq!(cursor_after_filter_change(&old, None, &[7]), Some(7));
        assert_eq!(cursor_after_filter_change(&old, Some(1), &[]), None);
    }
}
