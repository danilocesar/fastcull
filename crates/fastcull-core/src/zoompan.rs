//! Loupe zoom ladder + pan anchoring math (`ui-grid.md` § Loupe zoom
//! ladder, user decisions 2026-07-25).
//!
//! All functions are pure: the app crate feeds them viewport/extent numbers
//! and binds the results to the Slint overlay. Factors are relative to the
//! fit view (`1.0` = fit); the 1:1 ceiling is `max` = native-pixel extent
//! divided by fit extent. Ladder stops are `1.5^n`, recomputed from 1.0
//! each time so `-` retraces the identical stops with no float drift.

/// Per-press zoom multiplier (user decision: "1.5x multipliers until the
/// image is 1:1" — not 2x).
pub const ZOOM_STEP: f32 = 1.5;

/// Next ladder stop up. A step that would exceed `max` lands EXACTLY at
/// `max` (a `+` that visibly does almost nothing reads as a broken key);
/// at or above `max` it stays put. `max <= 1` (small file: 1:1 fits the
/// screen) pins to fit — `+` does nothing, no flicker.
pub fn ladder_up(factor: f32, max: f32) -> f32 {
    if max <= 1.0 {
        return 1.0;
    }
    if factor >= max {
        return max;
    }
    (factor * ZOOM_STEP).min(max)
}

/// Next ladder stop down: the largest `1.5^n` strictly below `factor`
/// (retracing `+`'s stops exactly, including from a capped 1:1 that is not
/// itself a power), floored at fit.
pub fn ladder_down(factor: f32) -> f32 {
    let mut stop = 1.0f32;
    // Tolerance so retracing a stop we produced (`1.5^n`) steps below it
    // rather than returning it unchanged.
    while stop * ZOOM_STEP < factor * (1.0 - 1e-4) {
        stop *= ZOOM_STEP;
    }
    stop
}

/// One-axis pan offset (Flickable `viewport-x`/`-y`, i.e. `<= 0`) that
/// places `frac` (fractional image coordinate, 0..1) at the viewport
/// center, clamped so the image never detaches from the edges. An extent
/// smaller than the viewport centers (offset 0 — the Image element centers
/// itself within the viewport).
pub fn offset_centering(viewport: f32, extent: f32, frac: f32) -> f32 {
    // Totality guard (QE finding D3, 2026-07-30): `f32::clamp` PANICS when
    // its bounds are NaN or inverted, and a non-finite extent produces
    // exactly that. Nothing reachable feeds one today, but this module is
    // documented as pure math over caller-supplied geometry and the repo
    // forbids panicking paths in core — a degenerate number must degrade to
    // "centered", never take the process down.
    if !viewport.is_finite() || !extent.is_finite() || !frac.is_finite() {
        return 0.0;
    }
    if extent <= viewport {
        return 0.0;
    }
    (viewport / 2.0 - frac * extent).clamp(viewport - extent, 0.0)
}

/// Inverse of [`offset_centering`]: the fractional image coordinate at the
/// viewport center for the current offset. Extents at or below the
/// viewport are centered by construction.
pub fn frac_at_center(viewport: f32, extent: f32, offset: f32) -> f32 {
    if !viewport.is_finite() || !extent.is_finite() || !offset.is_finite() {
        return 0.5; // see offset_centering: degenerate geometry centers
    }
    if extent <= 0.0 || extent <= viewport {
        return 0.5;
    }
    ((viewport / 2.0 - offset) / extent).clamp(0.0, 1.0)
}

/// Fit scale for a `native`-sized image in a `viewport` (both logical px):
/// the factor-1.0 extent is `native * fit_scale`, and the 1:1 ceiling
/// passed to [`ladder_up`] is `1.0 / fit_scale` (device pixels on screen).
/// Degenerate sizes yield a scale of 1 (callers never divide by zero).
pub fn fit_scale(viewport_w: f32, viewport_h: f32, native_w: f32, native_h: f32) -> f32 {
    let all_finite = [viewport_w, viewport_h, native_w, native_h]
        .iter()
        .all(|v| v.is_finite());
    if !all_finite || native_w <= 0.0 || native_h <= 0.0 || viewport_w <= 0.0 || viewport_h <= 0.0 {
        return 1.0;
    }
    (viewport_w / native_w).min(viewport_h / native_h)
}

/// Map a click inside a cell to a fractional coordinate of the image the
/// cell contain-fits (`image-fit: contain`: centered, aspect preserved).
/// `(cx, cy)` are cell-local click coordinates; `aspect` = width/height.
/// Clicks in the letterbox bars clamp to the nearest image edge.
pub fn contain_click_frac(cell_w: f32, cell_h: f32, aspect: f32, cx: f32, cy: f32) -> (f32, f32) {
    if cell_w <= 0.0 || cell_h <= 0.0 || aspect <= 0.0 {
        return (0.5, 0.5);
    }
    let (iw, ih) = if cell_w / cell_h > aspect {
        (cell_h * aspect, cell_h) // height-limited: pillarboxed
    } else {
        (cell_w, cell_w / aspect) // width-limited: letterboxed
    };
    let ix = (cell_w - iw) / 2.0;
    let iy = (cell_h - ih) / 2.0;
    (
        ((cx - ix) / iw.max(1.0)).clamp(0.0, 1.0),
        ((cy - iy) / ih.max(1.0)).clamp(0.0, 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_climbs_by_1_5_and_lands_exactly_on_max() {
        // A1 on 4K: max ~2.25 = exactly two presses (persona arithmetic).
        let max = 2.25;
        let f1 = ladder_up(1.0, max);
        assert_eq!(f1, 1.5);
        let f2 = ladder_up(f1, max);
        assert_eq!(f2, 2.25, "second press lands exactly at 1:1");
        assert_eq!(ladder_up(f2, max), 2.25, "capped: + at 1:1 holds");
        // 1080p-ish: 4.5x ceiling, overshoot step clamps to it exactly.
        let max = 4.5;
        let f3 = ladder_up(ladder_up(ladder_up(1.0, max), max), max); // 3.375
        assert!((f3 - 3.375).abs() < 1e-6);
        assert_eq!(ladder_up(f3, max), 4.5, "overshoot lands at max, not 5.06");
    }

    #[test]
    fn ladder_down_retraces_identical_stops() {
        let max = 4.5;
        let mut f = 1.0;
        let mut ups = vec![f];
        loop {
            let next = ladder_up(f, max);
            if next == f {
                break;
            }
            f = next;
            ups.push(f);
        }
        // Walk back down: every stop must be re-visited exactly.
        for expect in ups.iter().rev().skip(1) {
            f = ladder_down(f);
            assert_eq!(f, *expect, "descent must retrace ascent");
        }
        assert_eq!(ladder_down(1.0), 1.0, "floor at fit");
    }

    #[test]
    fn small_file_pins_to_fit() {
        assert_eq!(ladder_up(1.0, 0.8), 1.0, "1:1 smaller than fit: no zoom");
        assert_eq!(ladder_up(1.0, 1.0), 1.0);
    }

    #[test]
    fn offset_roundtrips_frac_and_clamps() {
        let (vw, ext) = (1000.0, 3000.0);
        for frac in [0.0, 0.25, 0.5, 0.77, 1.0] {
            let off = offset_centering(vw, ext, frac);
            assert!((vw - ext..=0.0).contains(&off), "offset {off} out of range");
            let back = frac_at_center(vw, ext, off);
            // Edge fracs clamp (can't center 0.0 — the image edge pins),
            // interior fracs round-trip exactly.
            if (0.2..=0.8).contains(&frac) {
                assert!((back - frac).abs() < 1e-4, "{frac} -> {off} -> {back}");
            }
        }
        // Extent smaller than viewport: always centered, frac always 0.5.
        assert_eq!(offset_centering(1000.0, 500.0, 0.9), 0.0);
        assert_eq!(frac_at_center(1000.0, 500.0, 0.0), 0.5);
    }

    #[test]
    fn contain_click_maps_both_orientations_and_letterbox() {
        // Landscape 3:2 in a wider-than-image cell (pillarboxed): image is
        // 1212x808 centered in 1440x808 -> bars of 114 each side.
        let (fx, fy) = contain_click_frac(1440.0, 808.0, 1.5, 720.0, 404.0);
        assert!((fx - 0.5).abs() < 1e-4 && (fy - 0.5).abs() < 1e-4, "center");
        let (fx, _) = contain_click_frac(1440.0, 808.0, 1.5, 114.0, 404.0);
        assert!(fx.abs() < 1e-3, "left image edge = frac 0");
        let (fx, _) = contain_click_frac(1440.0, 808.0, 1.5, 10.0, 404.0);
        assert_eq!(fx, 0.0, "pillarbox bar clamps to the edge");
        // Portrait 2:3 in a landscape cell (mid-burst orientation flip).
        let (fx, fy) = contain_click_frac(1440.0, 808.0, 2.0 / 3.0, 720.0, 0.0);
        assert!((fx - 0.5).abs() < 1e-4 && fy == 0.0, "portrait top center");
        // Degenerate inputs never NaN.
        assert_eq!(contain_click_frac(0.0, 808.0, 1.5, 1.0, 1.0), (0.5, 0.5));
        assert_eq!(contain_click_frac(100.0, 100.0, 0.0, 1.0, 1.0), (0.5, 0.5));
    }

    /// Degenerate geometry must DEGRADE, never panic: `f32::clamp` asserts
    /// when its bounds are NaN or inverted, which a non-finite extent
    /// produces. QE's sweep over `pointer::step` found 7,992,116 panicking
    /// combinations before these guards and 0 after — but deleting any one
    /// of the three left every core target green (finding G1), so each is
    /// pinned here individually.
    #[test]
    fn non_finite_geometry_degrades_instead_of_panicking() {
        let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        for v in bad {
            // offset_centering: any non-finite argument centers.
            assert_eq!(offset_centering(v, 3000.0, 0.5), 0.0);
            assert_eq!(offset_centering(1000.0, v, 0.5), 0.0);
            assert_eq!(offset_centering(1000.0, 3000.0, v), 0.0);
            // frac_at_center: any non-finite argument is the image center.
            assert_eq!(frac_at_center(v, 3000.0, -500.0), 0.5);
            assert_eq!(frac_at_center(1000.0, v, -500.0), 0.5);
            assert_eq!(frac_at_center(1000.0, 3000.0, v), 0.5);
            // fit_scale: unusable numbers yield the identity scale, and
            // NEVER a NaN that would poison every extent downstream.
            for s in [
                fit_scale(v, 800.0, 4000.0, 3200.0),
                fit_scale(1000.0, v, 4000.0, 3200.0),
                fit_scale(1000.0, 800.0, v, 3200.0),
                fit_scale(1000.0, 800.0, 4000.0, v),
            ] {
                assert_eq!(s, 1.0, "fit_scale({v}) must degrade to 1.0");
            }
        }
    }

    #[test]
    fn fit_scale_picks_limiting_axis_and_survives_zeroes() {
        // 8640x5760 in a 3840x2160 viewport: height limits (2160/5760).
        let s = fit_scale(3840.0, 2160.0, 8640.0, 5760.0);
        assert!((s - 0.375).abs() < 1e-6);
        assert!((1.0 / s - 2.6667).abs() < 1e-3, "1:1 ceiling = 1/fit_scale");
        assert_eq!(fit_scale(0.0, 100.0, 10.0, 10.0), 1.0);
        assert_eq!(fit_scale(100.0, 100.0, 0.0, 10.0), 1.0);
    }
}
