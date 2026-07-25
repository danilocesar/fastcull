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
pub fn view(
    picks: &[PickState],
    names: &[String],
    capture_keys: &[Option<String>],
    query: &ViewQuery,
) -> Vec<usize> {
    let mut ids: Vec<usize> = (0..picks.len())
        .filter(|i| query.filter.matches(picks[*i]))
        .collect();
    match query.sort {
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
                    let v = view(&picks, &names, &keys, &q);
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
                    );
                    rev.reverse();
                    assert_eq!(v, rev, "{q:?} descending is exact reverse");
                }
            }
        }
        // Capture-time order: keyed images chronologically, keyless after,
        // by name.
        let q = ViewQuery::default();
        assert_eq!(view(&picks, &names, &keys, &q), vec![3, 0, 1, 4, 5, 2]);
        // Filename order.
        let q = ViewQuery {
            sort: SortKey::Filename,
            ..q
        };
        assert_eq!(view(&picks, &names, &keys, &q), vec![5, 4, 3, 2, 1, 0]);
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
        let mut v = view(&picks, &names, &keys, &q);
        assert_eq!(v, vec![4, 2]); // capture order: b.ARW then keyless d.ARW
        let mut cursor = Some(v[0]);

        // Mark image 4 as picked: it leaves the view; cursor slides to 2.
        picks[4] = Picked;
        let old_pos = v.iter().position(|i| Some(*i) == cursor).unwrap();
        v = view(&picks, &names, &keys, &q);
        cursor = cursor_after_removal(&v, old_pos);
        assert_eq!(v, vec![2]);
        assert_eq!(cursor, Some(2));

        // Mark image 2 rejected: view empties, cursor none — inbox zero.
        picks[2] = Rejected;
        let old_pos = v.iter().position(|i| Some(*i) == cursor).unwrap();
        v = view(&picks, &names, &keys, &q);
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
