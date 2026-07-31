//! Grid layout math for the zoomable thumbnail view (`specs/modules/ui-grid.md`).
//!
//! Pure functions — no UI types — so the windowed-model computation that
//! drives Slint's virtualization is fully unit-testable in core. The app
//! crate feeds in viewport geometry and gets back which cells exist, where
//! they sit, and how the cursor moves.
//!
//! Coordinates are f32 logical pixels, origin at the top of the full
//! (virtual) grid; the UI subtracts its scroll offset.

/// Zoom ladder: number of columns per step; index 6 (1 column) is the loupe
/// per the spec's one-axis zoom model.
pub const ZOOM_COLUMNS: [usize; 7] = [12, 8, 6, 4, 3, 2, 1];

/// 3:2 landscape cells (Sony A1 native aspect). Portrait images letterbox
/// inside the cell.
pub const CELL_ASPECT: f32 = 1.5;

/// Gap between cells in logical pixels.
pub const CELL_GAP: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLayout {
    pub columns: usize,
    pub cell_width: f32,
    pub cell_height: f32,
    /// Total virtual height of the grid for `item_count` items.
    pub total_height: f32,
}

impl GridLayout {
    /// Layout for a zoom step within a viewport of `viewport_width` x
    /// `viewport_height` (the GRID area — below the filter bar, left of the
    /// IPTC panel).
    ///
    /// The height only ever matters at ONE COLUMN, where the cell is the
    /// loupe's fit view: a 3:2 cell of the full grid width is taller than
    /// any normal viewport (1428x952 in a 794 px-high area on a 1440x900
    /// window), and `scroll_to_reveal` top-aligns what it cannot fit, so
    /// the bottom 17-23% of every frame was simply not on screen — with
    /// nothing to say so. That contradicts the two things the zoom model
    /// and the pointer contract assert about `Fit`: "the whole image is on
    /// screen" and "nothing is off-screen, so there is no pan axis"
    /// (ui-grid.md). Since issue #11 gave the wheel to zoom and made drag
    /// inert at fit, the hidden band was not reachable by any input either.
    ///
    /// So at one column the cell is bounded by the viewport and the image
    /// contain-fits inside it with pillarbox bars — a real fit. Multi-column
    /// grids are untouched: their cells are far shorter than the viewport,
    /// and bounding them would shrink the comparison pair at N=2 for
    /// nothing (persona review 2026-07-30).
    pub fn new(
        zoom_step: usize,
        viewport_width: f32,
        viewport_height: f32,
        item_count: usize,
    ) -> Self {
        let columns = ZOOM_COLUMNS[zoom_step.min(ZOOM_COLUMNS.len() - 1)];
        let cell_width = (viewport_width - CELL_GAP * (columns as f32 + 1.0)) / columns as f32;
        let cell_width = cell_width.max(1.0);
        let mut cell_height = cell_width / CELL_ASPECT;
        if columns == 1 {
            // A pre-layout refresh sees a zero or negative height (issue #4):
            // leave the cell alone there rather than collapsing it to 1 px.
            let avail = viewport_height - 2.0 * CELL_GAP;
            if avail > 1.0 {
                cell_height = cell_height.min(avail);
            }
        }
        let rows = item_count.div_ceil(columns.max(1));
        let total_height = rows as f32 * (cell_height + CELL_GAP) + CELL_GAP;
        Self {
            columns,
            cell_width,
            cell_height,
            total_height,
        }
    }

    pub fn row_of(&self, index: usize) -> usize {
        index / self.columns
    }

    /// Top-left position of a cell in virtual-grid coordinates.
    pub fn position(&self, index: usize) -> (f32, f32) {
        let row = self.row_of(index);
        let col = index % self.columns;
        (
            CELL_GAP + col as f32 * (self.cell_width + CELL_GAP),
            CELL_GAP + row as f32 * (self.cell_height + CELL_GAP),
        )
    }

    /// Indexes whose cells intersect the viewport `[scroll_y, scroll_y +
    /// viewport_height)`, expanded by `margin_rows` on each side (the
    /// windowed model: only these cells exist UI-side).
    pub fn visible_range(
        &self,
        item_count: usize,
        scroll_y: f32,
        viewport_height: f32,
        margin_rows: usize,
    ) -> std::ops::Range<usize> {
        if item_count == 0 {
            return 0..0;
        }
        let row_pitch = self.cell_height + CELL_GAP;
        let first_row = ((scroll_y - CELL_GAP) / row_pitch).floor().max(0.0) as usize;
        let last_row = ((scroll_y + viewport_height) / row_pitch).ceil() as usize;
        let first_row = first_row.saturating_sub(margin_rows);
        let last_row = last_row + margin_rows;
        let start = (first_row * self.columns).min(item_count);
        let end = ((last_row + 1) * self.columns).min(item_count);
        start..end
    }

    /// Scroll offset that keeps `index`'s row fully visible, moving as
    /// little as possible from `scroll_y`. A cell taller than the viewport
    /// (1-column loupe on a 16:9 window) is top-aligned and stable on
    /// repeated calls — the top/bottom rules would otherwise oscillate
    /// (QE finding: ~260 px flip per keypress).
    pub fn scroll_to_reveal(&self, index: usize, scroll_y: f32, viewport_height: f32) -> f32 {
        let (_, top) = self.position(index);
        let bottom = top + self.cell_height;
        if self.cell_height + 2.0 * CELL_GAP > viewport_height {
            return (top - CELL_GAP).max(0.0);
        }
        if top - CELL_GAP < scroll_y {
            (top - CELL_GAP).max(0.0)
        } else if bottom + CELL_GAP > scroll_y + viewport_height {
            (bottom + CELL_GAP - viewport_height).max(0.0)
        } else {
            scroll_y
        }
    }

    /// Is `index`'s cell showing any part of itself in the viewport
    /// `[scroll_y, scroll_y + viewport_height)`?
    ///
    /// One definition, because three copies of it had accumulated in the app
    /// crate and it is not glue: it decides whether the load-settled re-sort
    /// restores a cursor the user was watching or leaves a browsing user's
    /// scroll alone (`scroll_after_resort`), and whether a relayout re-anchors
    /// (rule 5 — the semantics live in core).
    pub fn is_visible(&self, index: usize, scroll_y: f32, viewport_height: f32) -> bool {
        let (_, top) = self.position(index);
        top < scroll_y + viewport_height && top + self.cell_height > scroll_y
    }
}

/// Scroll offset after the ONE-SHOT re-sort that fires when a folder finishes
/// loading (issue #25): the view flips from the provisional filename order
/// into the user's sort, which is the only mutation that reorders every cell
/// at once, so the old pixel offset points at unrelated content.
///
/// `cursor_was_visible` must describe the cell BEFORE the flip — the flip
/// changes the cursor's position, so asking afterwards answers a different
/// question. When it is false the offset is returned untouched: wheel and
/// scrollbar browsing do not claim the cursor, and the cursor contract says
/// an off-screen cursor stays off-screen until the next arrow key, so hauling
/// the viewport back would be the very "the view moved with no input" defect
/// the provisional order exists to remove (validator FAIL, 2026-07-31 — the
/// unguarded version snapped a user browsing at 20,000 px to 0).
///
/// Pixels are preserved for that browsing user, not content: the grid under
/// the viewport has re-sorted, so they are looking at a different set of
/// photographs at the same offset. That is the accepted trade — a viewport
/// that stays put is recoverable by looking, one that teleports is not.
pub fn scroll_after_resort(
    layout: &GridLayout,
    cursor_pos: usize,
    scroll_y: f32,
    viewport_height: f32,
    cursor_was_visible: bool,
) -> f32 {
    if !cursor_was_visible {
        return scroll_y;
    }
    layout.scroll_to_reveal(cursor_pos, scroll_y, viewport_height)
}

/// Cursor movement over the item list in grid terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

/// New cursor index for a navigation key. `rows_per_page` is the number of
/// fully visible rows (≥1). Clamps at the ends; never leaves `0..item_count`.
pub fn navigate(
    cursor: usize,
    item_count: usize,
    columns: usize,
    rows_per_page: usize,
    nav: Nav,
) -> usize {
    if item_count == 0 {
        return 0;
    }
    let last = item_count - 1;
    let columns = columns.max(1);
    let page = columns * rows_per_page.max(1);
    let target = match nav {
        Nav::Left => cursor.saturating_sub(1),
        Nav::Right => cursor + 1,
        Nav::Up => cursor.saturating_sub(columns),
        Nav::Down => {
            // Moving down from the last (possibly partial) row stays put
            // rather than jumping to End: predictable during fast culling.
            if cursor + columns > last {
                cursor
            } else {
                cursor + columns
            }
        }
        Nav::PageUp => cursor.saturating_sub(page),
        Nav::PageDown => (cursor + page).min(last),
        Nav::Home => 0,
        Nav::End => last,
    };
    target.min(last)
}

/// Zoom step change, clamped to the ladder. Positive `delta` zooms in
/// (fewer columns).
pub fn zoom_step(current: usize, delta: i32) -> usize {
    let max = ZOOM_COLUMNS.len() as i32 - 1;
    (current as i32 + delta).clamp(0, max) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_fills_viewport_width() {
        let l = GridLayout::new(1, 1600.0, 900.0, 100); // 8 columns
        assert_eq!(l.columns, 8);
        let expected = (1600.0 - CELL_GAP * 9.0) / 8.0;
        assert!((l.cell_width - expected).abs() < 0.01);
        assert!((l.cell_height - expected / 1.5).abs() < 0.01);
    }

    #[test]
    fn single_column_is_loupe_sized() {
        let l = GridLayout::new(6, 1600.0, 900.0, 10);
        assert_eq!(l.columns, 1);
        assert!(l.cell_width > 1500.0);
    }

    /// The loupe fit view shows the WHOLE frame (ui-grid.md: `Fit` = "the
    /// whole image is on screen"). Before this, the N=1 cell was a 3:2 box
    /// of the full grid width — taller than any normal viewport — and
    /// `scroll_to_reveal` top-aligned it, hiding the bottom 17-23% of every
    /// photograph with no way to reach it after issue #11 took the wheel
    /// and the drag.
    #[test]
    fn single_column_cell_never_exceeds_the_viewport() {
        // Real geometry: 1440x900 window leaves a 1440x794 grid area.
        let l = GridLayout::new(6, 1440.0, 794.0, 3);
        assert_eq!(l.columns, 1);
        assert_eq!(l.cell_width, 1428.0, "still full width");
        assert!(
            l.cell_height <= 794.0 - 2.0 * CELL_GAP,
            "cell must fit the viewport, got {}",
            l.cell_height
        );
        // The whole cell is reachable: revealing it leaves nothing below
        // the fold, at any index.
        for idx in 0..3 {
            let scroll = l.scroll_to_reveal(idx, 0.0, 794.0);
            let (_, top) = l.position(idx);
            assert!(
                top + l.cell_height <= scroll + 794.0 + 0.01,
                "cell {idx} bottom below the fold"
            );
        }
        // A 3:2 frame therefore contain-fits with PILLARBOX bars, and the
        // full image height is on screen.
        let image_h = (l.cell_width / CELL_ASPECT).min(l.cell_height);
        assert!((image_h - l.cell_height).abs() < 0.01, "height-limited");
        let image_w = image_h * CELL_ASPECT;
        assert!(image_w < l.cell_width, "bars at the sides, not a crop");

        // Fullscreen 1080p, where the old crop was worst (23.4%).
        let l = GridLayout::new(6, 1920.0, 974.0, 3);
        assert!(l.cell_height <= 974.0 - 2.0 * CELL_GAP);
    }

    /// Issue #25's load-settled re-sort: the viewport follows a cursor the
    /// user was LOOKING at, and leaves a browsing user's offset alone.
    ///
    /// This lives in core precisely because the app-level version of it
    /// shipped into review with the guard missing, and the only evidence
    /// offered was a screenshot test that could not run the mid-load window
    /// in the debug profile CI uses (validator, 2026-07-31). The decision is
    /// pure arithmetic; it does not need a 400-file folder to check.
    #[test]
    fn resort_reveals_a_watched_cursor_and_spares_a_browsing_one() {
        // 1440-wide grid, 800 tall, 8 columns, 400 items.
        let l = GridLayout::new(1, 1440.0, 800.0, 400);
        assert_eq!(l.columns, 8);
        let pitch = l.cell_height + CELL_GAP;
        let far = 40.0 * pitch; // browsed a long way down

        // Cursor off-screen because the user scrolled past it: untouched.
        assert_eq!(
            scroll_after_resort(&l, 0, far, 800.0, false),
            far,
            "a browsing user's viewport must not be hauled back"
        );
        // ...at every offset, and for any cursor position.
        for scroll in [0.0, 1.0, 250.0, far, 1e6] {
            for pos in [0, 7, 199, 399] {
                assert_eq!(scroll_after_resort(&l, pos, scroll, 800.0, false), scroll);
            }
        }

        // Cursor the user WAS looking at: revealed, moving as little as
        // possible (identical to scroll_to_reveal by construction).
        let watched = scroll_after_resort(&l, 300, far, 800.0, true);
        assert_eq!(watched, l.scroll_to_reveal(300, far, 800.0));
        let (_, top) = l.position(300);
        assert!(
            top >= watched - 0.01 && top + l.cell_height <= watched + 800.0 + 0.01,
            "cursor cell must end up fully on screen: top {top}, scroll {watched}"
        );
        // A cursor already in view moves nothing.
        assert_eq!(scroll_after_resort(&l, 0, 0.0, 800.0, true), 0.0);
    }

    /// `is_visible` is the single definition the re-anchor decisions consult.
    #[test]
    fn is_visible_covers_partial_overlap_and_both_edges() {
        let l = GridLayout::new(1, 1440.0, 800.0, 400); // 8 columns
        let pitch = l.cell_height + CELL_GAP;
        // Row 0 at scroll 0: plainly visible.
        assert!(l.is_visible(0, 0.0, 800.0));
        // Scrolled just past its bottom edge: gone.
        let (_, top0) = l.position(0);
        assert!(!l.is_visible(0, top0 + l.cell_height, 800.0));
        // Straddling the fold counts as visible — a partly-shown cell is a
        // cell the user can see, which is what the re-anchor cares about.
        assert!(l.is_visible(0, top0 + l.cell_height - 1.0, 800.0));
        // A row far below the viewport is not.
        assert!(!l.is_visible(8 * 40, 0.0, 800.0));
        // ...and becomes visible once scrolled to.
        assert!(l.is_visible(8 * 40, 40.0 * pitch, 800.0));
    }

    /// Multi-column grids are deliberately NOT viewport-bounded: their
    /// cells are far shorter than the viewport anyway, and capping N=2
    /// would shrink the side-by-side comparison pair for nothing.
    #[test]
    fn multi_column_cells_keep_the_3_2_aspect() {
        for step in 0..=5 {
            let l = GridLayout::new(step, 1440.0, 200.0, 100);
            assert!(l.columns > 1);
            assert!(
                (l.cell_height - l.cell_width / CELL_ASPECT).abs() < 0.01,
                "{} columns must stay 3:2 even in a short viewport",
                l.columns
            );
        }
    }

    #[test]
    fn total_height_counts_partial_rows() {
        let l = GridLayout::new(1, 1600.0, 900.0, 9); // 8 cols -> 2 rows (8 + 1)
        let row_pitch = l.cell_height + CELL_GAP;
        assert!((l.total_height - (2.0 * row_pitch + CELL_GAP)).abs() < 0.01);
    }

    #[test]
    fn visible_range_windows_with_margin() {
        let l = GridLayout::new(1, 1600.0, 900.0, 2000); // 8 columns
        let row_pitch = l.cell_height + CELL_GAP;
        // Viewport showing rows ~4..8
        let range = l.visible_range(2000, 4.0 * row_pitch, 4.0 * row_pitch, 1);
        assert!(range.start <= 3 * 8, "margin row above included");
        assert!(range.end >= 9 * 8, "margin row below included");
        assert!(range.len() < 120, "window stays small: {}", range.len());
    }

    #[test]
    fn visible_range_edges() {
        let l = GridLayout::new(0, 1200.0, 800.0, 5); // 12 columns, 5 items: 1 row
        assert_eq!(l.visible_range(5, 0.0, 800.0, 2), 0..5);
        assert_eq!(l.visible_range(0, 0.0, 800.0, 2), 0..0);
        // Scrolled far past the end clamps to item count.
        let r = l.visible_range(5, 1e6, 800.0, 2);
        assert!(r.start <= 5 && r.end == 5);
    }

    #[test]
    fn navigate_clamps_and_stays_in_partial_rows() {
        // 10 items, 4 columns: rows [0..4),[4..8),[8..10)
        assert_eq!(navigate(0, 10, 4, 2, Nav::Left), 0);
        assert_eq!(navigate(9, 10, 4, 2, Nav::Right), 9);
        assert_eq!(navigate(1, 10, 4, 2, Nav::Down), 5);
        assert_eq!(navigate(5, 10, 4, 2, Nav::Down), 9);
        assert_eq!(navigate(7, 10, 4, 2, Nav::Down), 7); // would pass End: stay
        assert_eq!(navigate(9, 10, 4, 2, Nav::Up), 5);
        assert_eq!(navigate(9, 10, 4, 2, Nav::Home), 0);
        assert_eq!(navigate(0, 10, 4, 2, Nav::End), 9);
        assert_eq!(navigate(0, 10, 4, 2, Nav::PageDown), 8);
        assert_eq!(navigate(8, 10, 4, 2, Nav::PageUp), 0);
        assert_eq!(navigate(0, 0, 4, 2, Nav::Down), 0); // empty folder
    }

    #[test]
    fn navigate_single_column_acts_like_filmstrip() {
        assert_eq!(navigate(3, 10, 1, 5, Nav::Down), 4);
        assert_eq!(navigate(3, 10, 1, 5, Nav::Right), 4);
        assert_eq!(navigate(3, 10, 1, 5, Nav::Up), 2);
    }

    #[test]
    fn zoom_ladder_clamps() {
        assert_eq!(zoom_step(0, -1), 0);
        assert_eq!(zoom_step(0, 1), 1);
        assert_eq!(zoom_step(6, 1), 6);
        assert_eq!(zoom_step(3, -3), 0);
    }

    /// Regression (QE defect D1): a cell taller than the viewport must
    /// top-align and stay put on repeated reveals, never oscillate.
    #[test]
    fn reveal_of_oversized_cell_is_stable() {
        // Pre-layout height (issue #4): the N=1 cap is skipped, so the
        // cell is the full 3:2 height and still overflows the viewport.
        let l = GridLayout::new(6, 1920.0, 0.0, 50); // 1 column, cell_h ~1272
        let viewport = 1020.0;
        assert!(l.cell_height > viewport);
        let s1 = l.scroll_to_reveal(5, 0.0, viewport);
        let s2 = l.scroll_to_reveal(5, s1, viewport);
        let s3 = l.scroll_to_reveal(5, s2, viewport);
        assert_eq!(s1, s2, "reveal must be idempotent");
        assert_eq!(s2, s3);
        let (_, top) = l.position(5);
        assert!((s1 - (top - CELL_GAP)).abs() < 0.01, "top-aligned");
    }

    /// N=1 (loupe) windowing: exactly the on-screen image ± margin exists.
    #[test]
    fn visible_range_at_single_column() {
        let l = GridLayout::new(6, 1920.0, 1020.0, 100);
        let row_pitch = l.cell_height + CELL_GAP;
        let r = l.visible_range(100, 10.0 * row_pitch, row_pitch, 1);
        assert!(r.contains(&10));
        // Viewport row + partial-overlap row + one margin row each side.
        assert!(r.len() <= 6, "tight window at N=1, got {r:?}");
    }

    #[test]
    fn scroll_to_reveal_moves_minimally() {
        let l = GridLayout::new(1, 1600.0, 900.0, 2000);
        let row_pitch = l.cell_height + CELL_GAP;
        let viewport = 3.0 * row_pitch;
        // Cell below the viewport: scroll down just enough.
        let idx_row10 = 10 * 8;
        let s = l.scroll_to_reveal(idx_row10, 0.0, viewport);
        let (_, top) = l.position(idx_row10);
        assert!(s + viewport >= top + l.cell_height);
        // Already visible: unchanged.
        assert_eq!(l.scroll_to_reveal(idx_row10, s, viewport), s);
        // Cell above: scroll up to its top.
        let s2 = l.scroll_to_reveal(0, s, viewport);
        assert_eq!(s2, 0.0);
    }
}
