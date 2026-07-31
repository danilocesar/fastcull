//! Pointer state machine (ui-grid.md § Mouse & pointer contract, issue
//! #11): what the mouse means at every zoom level, as one explicit
//! (state, input) → (state, action) table instead of `if`s scattered
//! through the app crate.
//!
//! Pure by contract (rule 5): no Slint types, no I/O. Geometry (viewport,
//! native size, 1:1 ceiling, current pan center) is passed per call; all
//! math delegates to [`crate::zoompan`]. The app crate's only job is to
//! normalize raw pointer events into [`PointerInput`] and apply the
//! returned [`Action`] — a gesture whose behavior cannot be read off the
//! spec's table is a bug in this machine, not in the bridge.
//!
//! Normalization contract (the bridge's side of the deal, per spec):
//! high-resolution wheels accumulate delta and emit ONE input per
//! notch-equivalent; a drag suppresses the click; a double-click requires
//! the second press within the drag/click movement threshold; wheel
//! events over the IPTC panel / filter bar / scrollbar are that widget's,
//! never routed here.

use crate::zoompan;

/// The zoom level IS the state. `columns == 1` is not a grid state — one
/// column is the loupe, i.e. [`ViewState::Fit`] or [`ViewState::Zoomed`].
/// The machine holds no other state: marks, cursor, filter and selection
/// are untouched by it (spec).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ViewState {
    /// Multi-image view, `columns ∈ {12, 8, 6, 4, 3, 2}`.
    Grid { columns: u8 },
    /// Single image, factor 1.0 — the whole image is on screen.
    Fit,
    /// Single image above fit; `factor == max` is 1:1.
    Zoomed { factor: f32 },
}

/// Normalized pointer input. Positions are view-area coordinates in
/// logical pixels; the machine converts them to fractional image
/// coordinates itself (spec: via the zoompan math).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointerInput {
    /// One notch-equivalent of wheel. `ctrl` is carried so the reserved
    /// rows stay explicit (grid: M2 deferral; loupe: modifier ignored).
    Wheel {
        up: bool,
        ctrl: bool,
        pos: (f32, f32),
    },
    Click {
        pos: (f32, f32),
    },
    DoubleClick {
        pos: (f32, f32),
    },
    /// Pointer moved `(dx, dy)` logical px while dragging.
    Drag {
        dx: f32,
        dy: f32,
    },
    /// Right / middle / thumb buttons — reserved, explicit no-op (spec:
    /// nobody grows a context menu into the culling grid by accident).
    OtherButton,
}

/// Per-call geometry. The machine never caches sizes — the bridge feeds
/// the current numbers on every event.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    /// The GRID AREA (excludes the IPTC panel), logical px.
    pub viewport_w: f32,
    pub viewport_h: f32,
    /// Native image size, logical px (the bridge applies its HiDPI rule).
    pub native_w: f32,
    pub native_h: f32,
    /// The 1:1 ceiling as a factor above fit. `None` = not yet known
    /// (full-res still decoding): the ladder climbs OPTIMISTICALLY and
    /// 1:1 desires pin as INFINITY — identical to the keyboard `+`/`Z`;
    /// the bridge's render clamp resolves them once the ceiling is known.
    pub max_factor: Option<f32>,
    /// Current fractional pan center (`(0.5, 0.5)` = image center).
    pub pan_center: (f32, f32),
    /// Where the FIT view actually renders: the N=1 grid cell's on-screen
    /// rect `(x, y, w, h)` in view coords — the fit view is a grid strip
    /// cell (3:2, scroll-dependent), NOT an image centered in the
    /// viewport (validator MAJOR on the first cut). `None` = centered in
    /// the viewport (degenerate fallback; tests).
    pub fit_cell: Option<(f32, f32, f32, f32)>,
}

/// On-screen rect of the contain-fitted image at fit: inside `fit_cell`
/// when provided, else centered in the viewport.
fn fit_frame(geo: &Geometry) -> (f32, f32, f32, f32) {
    let (cx, cy, cw, ch) = geo
        .fit_cell
        .unwrap_or((0.0, 0.0, geo.viewport_w, geo.viewport_h));
    let aspect = if geo.native_h > 0.0 {
        (geo.native_w / geo.native_h).max(1e-6)
    } else {
        1.0
    };
    let (iw, ih) = if cw / ch.max(1.0) > aspect {
        (ch * aspect, ch) // pillarboxed
    } else {
        (cw, cw / aspect) // letterboxed
    };
    (cx + (cw - iw) / 2.0, cy + (ch - ih) / 2.0, iw, ih)
}

/// What the bridge must do. Reserved combinations return
/// [`Action::Reserved`] — never a silent fallthrough (spec).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Explicit nothing (e.g. wheel-down at fit is clamped).
    None,
    /// Reserved gesture: no-op today, the string names why.
    Reserved(&'static str),
    /// Grid: scroll the view one notch (cursor unmoved — browsing).
    GridScroll { up: bool },
    /// Grid: the Flickable's native kinetic drag is KEPT — bridge does
    /// nothing (rubber-band select is the reserved future gesture).
    GridNativeDrag,
    /// Grid click: route to the existing cursor-contract path (move
    /// cursor, claim, collapse multi-selection; Ctrl/Shift variants are
    /// the bridge's cell-level concern).
    GridClick,
    /// Grid double-click: open that image in the loupe at fit (the first
    /// click already moved the cursor there).
    EnterLoupe,
    /// Set factor and pan center (fractional image coords). Factor 1.0
    /// means the fit view — the bridge drops the overlay.
    SetZoom { factor: f32, center: (f32, f32) },
    /// Re-center only; factor unchanged.
    Recenter { center: (f32, f32) },
}

/// Fractional image coordinate under a viewport point, for an image of
/// `extent` shown at pan `center` in `viewport` (one axis). Extents at or
/// below the viewport are centered by the Image element.
fn frac_under(viewport: f32, extent: f32, center: f32, p: f32) -> f32 {
    if extent <= 0.0 {
        return 0.5;
    }
    if extent <= viewport {
        return ((p - (viewport - extent) / 2.0) / extent).clamp(0.0, 1.0);
    }
    let offset = zoompan::offset_centering(viewport, extent, center);
    ((p - offset) / extent).clamp(0.0, 1.0)
}

/// Is the point inside the image rect (not in a letterbox bar)? Spec:
/// clicks and double-clicks in the bars produce no action at all.
fn inside_image(viewport: f32, extent: f32, center: f32, p: f32) -> bool {
    if extent <= viewport {
        let edge = (viewport - extent) / 2.0;
        p >= edge && p <= viewport - extent - edge + extent
    } else {
        let offset = zoompan::offset_centering(viewport, extent, center);
        p >= offset && p <= offset + extent
    }
}

/// Extents (logical px) at `factor` above fit.
fn extents(geo: &Geometry, factor: f32) -> (f32, f32) {
    let s = zoompan::fit_scale(geo.viewport_w, geo.viewport_h, geo.native_w, geo.native_h);
    (geo.native_w * s * factor, geo.native_h * s * factor)
}

/// Pan center that keeps the image point under `pos` fixed under the
/// pointer at the new extents (spec: "you wheel toward an eye without
/// clicking first"). Derivation: the unclamped offset placing frac `f`
/// at `p` is `p - f·e'`; `offset_centering`'s definition `o = v/2 - c·e'`
/// gives `c = f + (v/2 - p)/e'`. When the pan clamp makes the anchor
/// impossible (image edge) the clamp wins and the anchor drifts (spec).
fn anchor_center(geo: &Geometry, from_factor: f32, to_factor: f32, pos: (f32, f32)) -> (f32, f32) {
    let (ew, eh) = extents(geo, from_factor);
    let fx = frac_under(geo.viewport_w, ew, geo.pan_center.0, pos.0);
    let fy = frac_under(geo.viewport_h, eh, geo.pan_center.1, pos.1);
    let (ew2, eh2) = extents(geo, to_factor);
    (
        (fx + (geo.viewport_w / 2.0 - pos.0) / ew2.max(1.0)).clamp(0.0, 1.0),
        (fy + (geo.viewport_h / 2.0 - pos.1) / eh2.max(1.0)).clamp(0.0, 1.0),
    )
}

/// Ceiling used while the real 1:1 ceiling is still unknown (the full-res
/// rung is decoding). The wheel climbs OPTIMISTICALLY there, matching the
/// keyboard `+` — but not without bound: an unbounded ladder reaches ~1e38
/// in about 223 notches, at which point [`extents`] overflows to INFINITY
/// and the anchor math yields a NaN pan centre that then persists across
/// images (QE finding D4, 2026-07-30). Nothing real comes near this — a
/// 50 MP A1 frame on a 1440-wide viewport has a ceiling of 6.9 — so the cap
/// is invisible in practice and the render clamp still lands the view on the
/// true ceiling as soon as it is known.
const OPTIMISTIC_MAX: f32 = 64.0;

/// View-area position → fractional image coordinate at `factor` (both
/// axes).
pub fn view_to_frac(geo: &Geometry, factor: f32, pos: (f32, f32)) -> (f32, f32) {
    if factor <= 1.0 {
        // At fit the image renders in the fit frame (N=1 grid cell).
        let (fx0, fy0, fw, fh) = fit_frame(geo);
        return (
            ((pos.0 - fx0) / fw.max(1.0)).clamp(0.0, 1.0),
            ((pos.1 - fy0) / fh.max(1.0)).clamp(0.0, 1.0),
        );
    }
    let (ew, eh) = extents(geo, factor);
    (
        frac_under(geo.viewport_w, ew, geo.pan_center.0, pos.0),
        frac_under(geo.viewport_h, eh, geo.pan_center.1, pos.1),
    )
}

/// The transition table. Every (state, input) pair is handled explicitly;
/// reserved combinations return their named no-op.
pub fn step(state: ViewState, input: PointerInput, geo: &Geometry) -> (ViewState, Action) {
    use PointerInput::*;
    // A pinned-but-unresolved 1:1 desire (the bridge's INFINITY sentinel
    // while full-res decodes) has no finite extents: EVERY pointer
    // gesture is inert until the render clamp resolves it — anchor math
    // on infinite extents poisons the pan center with NaN (QE finding).
    if let ViewState::Zoomed { factor } = state {
        if !factor.is_finite() {
            return (state, Action::None);
        }
    }
    match (state, input) {
        // ------ Grid: the wheel browses, clicks are the cursor's ------
        (ViewState::Grid { .. }, Wheel { ctrl: true, .. }) => (
            state,
            Action::Reserved("grid Ctrl+wheel zoom — still the M2 deferral"),
        ),
        (ViewState::Grid { .. }, Wheel { up, .. }) => (state, Action::GridScroll { up }),
        (ViewState::Grid { .. }, Click { .. }) => (state, Action::GridClick),
        (ViewState::Grid { .. }, DoubleClick { .. }) => (ViewState::Fit, Action::EnterLoupe),
        (ViewState::Grid { .. }, Drag { .. }) => (state, Action::GridNativeDrag),

        // ------ Fit: wheel-up enters the ladder, wheel-down clamps ------
        (ViewState::Fit, Wheel { up: true, pos, .. }) => {
            // Modifier deliberately ignored in the loupe (reserved row =
            // the plain-wheel row applies). An UNKNOWN ceiling climbs
            // optimistically — identical to the keyboard `+` (the render
            // clamp lands it at 1:1 when the ceiling becomes known).
            let max = geo.max_factor.unwrap_or(OPTIMISTIC_MAX);
            let f = zoompan::ladder_up(1.0, max);
            if f <= 1.0 {
                return (state, Action::None); // small file: pinned to fit
            }
            // Anchor: image frac from the RENDERED fit frame (the N=1
            // grid cell), projected so that point stays under the
            // pointer at the new (overlay) extents.
            let (fx, fy) = view_to_frac(geo, 1.0, pos);
            let (ew2, eh2) = extents(geo, f);
            let center = (
                (fx + (geo.viewport_w / 2.0 - pos.0) / ew2.max(1.0)).clamp(0.0, 1.0),
                (fy + (geo.viewport_h / 2.0 - pos.1) / eh2.max(1.0)).clamp(0.0, 1.0),
            );
            (
                ViewState::Zoomed { factor: f },
                Action::SetZoom { factor: f, center },
            )
        }
        (ViewState::Fit, Wheel { up: false, .. }) => (state, Action::None), // never falls out of the loupe
        (ViewState::Fit, Click { .. }) => (state, Action::None), // whole image on screen; stores nothing
        (ViewState::Fit, DoubleClick { pos }) => {
            // Unknown ceiling: the 1:1 desire is pinned as INFINITY, the
            // bridge's render clamp resolves it (keyboard `Z` semantics).
            let max = geo.max_factor.unwrap_or(f32::INFINITY);
            if max <= 1.0 {
                return (state, Action::None); // 1:1 fits the screen already
            }
            // The RENDERED fit frame decides letterbox rejection and the
            // clicked image point (the N=1 grid cell, not the viewport).
            let (fx0, fy0, fw, fh) = fit_frame(geo);
            if pos.0 < fx0 || pos.0 > fx0 + fw || pos.1 < fy0 || pos.1 > fy0 + fh {
                return (state, Action::None); // letterbox bars: dead
            }
            let (fx, fy) = view_to_frac(geo, 1.0, pos);
            (
                ViewState::Zoomed { factor: max },
                Action::SetZoom {
                    factor: max,
                    center: (fx, fy),
                },
            )
        }
        (ViewState::Fit, Drag { .. }) => (state, Action::None), // nothing off-screen: no pan axis

        // ------ Zoomed: wheel walks the ladder, click re-centers, drag pans ------
        (ViewState::Zoomed { factor }, Wheel { up: true, pos, .. }) => {
            let max = geo.max_factor.unwrap_or(OPTIMISTIC_MAX);
            let f = zoompan::ladder_up(factor, max);
            if (f - factor).abs() < 1e-6 {
                return (state, Action::None); // capped exactly at 1:1
            }
            let center = anchor_center(geo, factor, f, pos);
            (
                ViewState::Zoomed { factor: f },
                Action::SetZoom { factor: f, center },
            )
        }
        (ViewState::Zoomed { factor }, Wheel { up: false, pos, .. }) => {
            let f = zoompan::ladder_down(factor);
            if f <= 1.0 {
                // Landing on fit forgets the pan spot (spec: a stale pan
                // from three images ago is a trap).
                return (
                    ViewState::Fit,
                    Action::SetZoom {
                        factor: 1.0,
                        center: (0.5, 0.5),
                    },
                );
            }
            let center = anchor_center(geo, factor, f, pos);
            (
                ViewState::Zoomed { factor: f },
                Action::SetZoom { factor: f, center },
            )
        }
        (ViewState::Zoomed { factor }, Click { pos }) => {
            let (ew, eh) = extents(geo, factor);
            if !inside_image(geo.viewport_w, ew, geo.pan_center.0, pos.0)
                || !inside_image(geo.viewport_h, eh, geo.pan_center.1, pos.1)
            {
                return (state, Action::None);
            }
            let fx = frac_under(geo.viewport_w, ew, geo.pan_center.0, pos.0);
            let fy = frac_under(geo.viewport_h, eh, geo.pan_center.1, pos.1);
            (state, Action::Recenter { center: (fx, fy) })
        }
        (ViewState::Zoomed { factor }, DoubleClick { pos }) => {
            let (ew, eh) = extents(geo, factor);
            if !inside_image(geo.viewport_w, ew, geo.pan_center.0, pos.0)
                || !inside_image(geo.viewport_h, eh, geo.pan_center.1, pos.1)
            {
                return (state, Action::None);
            }
            let fx = frac_under(geo.viewport_w, ew, geo.pan_center.0, pos.0);
            let fy = frac_under(geo.viewport_h, eh, geo.pan_center.1, pos.1);
            let max = geo.max_factor.unwrap_or(f32::INFINITY);
            if factor >= max - 1e-6 {
                // Already at 1:1: re-center only.
                return (state, Action::Recenter { center: (fx, fy) });
            }
            (
                ViewState::Zoomed { factor: max },
                Action::SetZoom {
                    factor: max,
                    center: (fx, fy),
                },
            )
        }
        (ViewState::Zoomed { factor }, Drag { dx, dy }) => {
            // Pan 1:1 with pointer motion, clamped so the image never
            // detaches from the viewport edges: fold the delta through
            // the offset math (drag right = offset toward 0).
            let (ew, eh) = extents(geo, factor);
            let ox = zoompan::offset_centering(geo.viewport_w, ew, geo.pan_center.0) + dx;
            let oy = zoompan::offset_centering(geo.viewport_h, eh, geo.pan_center.1) + dy;
            let cx = zoompan::frac_at_center(
                geo.viewport_w,
                ew,
                ox.clamp((geo.viewport_w - ew).min(0.0), 0.0),
            );
            let cy = zoompan::frac_at_center(
                geo.viewport_h,
                eh,
                oy.clamp((geo.viewport_h - eh).min(0.0), 0.0),
            );
            (state, Action::Recenter { center: (cx, cy) })
        }

        // ------ Reserved buttons: dead everywhere, on purpose ------
        (_, OtherButton) => (
            state,
            Action::Reserved("right/middle/thumb buttons reserved — explicit no-op"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1000x800 viewport, 4000x3200 native → fit_scale 0.25, ceiling 4.0.
    fn geo() -> Geometry {
        Geometry {
            viewport_w: 1000.0,
            viewport_h: 800.0,
            native_w: 4000.0,
            native_h: 3200.0,
            max_factor: Some(4.0),
            pan_center: (0.5, 0.5),
            fit_cell: None,
        }
    }

    const CENTER: (f32, f32) = (500.0, 400.0);

    /// The spec's transition table, enumerated: every (state, input) pair
    /// asserts the resulting state + action, including the reserved
    /// no-ops (spec acceptance criterion).
    #[test]
    fn transition_table_every_pair() {
        let g = geo();
        let grid = ViewState::Grid { columns: 8 };
        let fit = ViewState::Fit;
        let zoomed = ViewState::Zoomed { factor: 1.5 };
        let wheel = |up, ctrl| PointerInput::Wheel {
            up,
            ctrl,
            pos: CENTER,
        };

        // Grid row.
        assert_eq!(
            step(grid, wheel(true, false), &g),
            (grid, Action::GridScroll { up: true })
        );
        assert_eq!(
            step(grid, wheel(false, false), &g),
            (grid, Action::GridScroll { up: false })
        );
        assert!(matches!(step(grid, wheel(true, true), &g), (s, Action::Reserved(_)) if s == grid));
        assert!(
            matches!(step(grid, wheel(false, true), &g), (s, Action::Reserved(_)) if s == grid)
        );
        assert_eq!(
            step(grid, PointerInput::Click { pos: CENTER }, &g),
            (grid, Action::GridClick)
        );
        assert_eq!(
            step(grid, PointerInput::DoubleClick { pos: CENTER }, &g),
            (ViewState::Fit, Action::EnterLoupe)
        );
        assert_eq!(
            step(grid, PointerInput::Drag { dx: 3.0, dy: 3.0 }, &g),
            (grid, Action::GridNativeDrag)
        );
        assert!(
            matches!(step(grid, PointerInput::OtherButton, &g), (s, Action::Reserved(_)) if s == grid)
        );

        // Fit row.
        assert_eq!(
            step(fit, wheel(true, false), &g),
            (
                ViewState::Zoomed { factor: 1.5 },
                Action::SetZoom {
                    factor: 1.5,
                    center: (0.5, 0.5)
                }
            ),
            "wheel-up at fit from the center: first ladder stop, center anchor"
        );
        assert_eq!(
            step(fit, wheel(false, false), &g),
            (fit, Action::None),
            "clamped, never exits"
        );
        // Ctrl ignored in the loupe: same as plain wheel.
        assert_eq!(
            step(fit, wheel(true, true), &g),
            step(fit, wheel(true, false), &g)
        );
        assert_eq!(
            step(fit, PointerInput::Click { pos: CENTER }, &g),
            (fit, Action::None)
        );
        assert_eq!(
            step(fit, PointerInput::DoubleClick { pos: CENTER }, &g),
            (
                ViewState::Zoomed { factor: 4.0 },
                Action::SetZoom {
                    factor: 4.0,
                    center: (0.5, 0.5)
                }
            ),
            "double-click at fit: 1:1 with the clicked point centered"
        );
        assert_eq!(
            step(fit, PointerInput::Drag { dx: 3.0, dy: 0.0 }, &g),
            (fit, Action::None)
        );
        assert!(
            matches!(step(fit, PointerInput::OtherButton, &g), (s, Action::Reserved(_)) if s == fit)
        );

        // Zoomed row.
        assert_eq!(
            step(zoomed, wheel(true, false), &g),
            (
                ViewState::Zoomed { factor: 2.25 },
                Action::SetZoom {
                    factor: 2.25,
                    center: (0.5, 0.5)
                }
            )
        );
        assert_eq!(
            step(zoomed, wheel(false, false), &g),
            (
                ViewState::Fit,
                Action::SetZoom {
                    factor: 1.0,
                    center: (0.5, 0.5)
                }
            ),
            "one stop below 1.5 is fit; pan spot forgotten"
        );
        assert_eq!(
            step(zoomed, wheel(true, true), &g),
            step(zoomed, wheel(true, false), &g)
        );
        assert_eq!(
            step(zoomed, PointerInput::Click { pos: CENTER }, &g),
            (zoomed, Action::Recenter { center: (0.5, 0.5) }),
            "click re-centers, factor unchanged"
        );
        assert_eq!(
            step(zoomed, PointerInput::DoubleClick { pos: CENTER }, &g),
            (
                ViewState::Zoomed { factor: 4.0 },
                Action::SetZoom {
                    factor: 4.0,
                    center: (0.5, 0.5)
                }
            )
        );
        assert!(matches!(
            step(zoomed, PointerInput::Drag { dx: -10.0, dy: 0.0 }, &g),
            (s, Action::Recenter { .. }) if s == zoomed
        ));
        assert!(
            matches!(step(zoomed, PointerInput::OtherButton, &g), (s, Action::Reserved(_)) if s == zoomed)
        );
    }

    /// Wheel anchor is the POINTER, not the center: the image point under
    /// the cursor stays under the cursor as the factor changes.
    #[test]
    fn wheel_anchors_the_pointer_point() {
        let g = geo();
        // Pointer at (750, 400): image frac at fit = 0.75 horizontally
        // (extent 1000 fills the viewport width).
        let pos = (750.0, 400.0);
        let (state, action) = step(
            ViewState::Fit,
            PointerInput::Wheel {
                up: true,
                ctrl: false,
                pos,
            },
            &g,
        );
        let ViewState::Zoomed { factor } = state else {
            panic!("must zoom")
        };
        let Action::SetZoom { center, .. } = action else {
            panic!("must set zoom")
        };
        // At the new extent, the anchored frac must sit at x=750 again:
        // offset = offset_centering(1000, 1500, cx); 750 = offset + 0.75·1500
        let (ew, _) = extents(&g, factor);
        let off = zoompan::offset_centering(g.viewport_w, ew, center.0);
        let back = off + 0.75 * ew;
        assert!(
            (back - pos.0).abs() < 0.5,
            "anchored point moved: {back} vs {}",
            pos.0
        );
    }

    /// One notch = one ladder stop; a step that would exceed 1:1 lands
    /// exactly at 1:1; a further notch is inert (the identical 1.5^n
    /// stops as +/- by construction — same zoompan functions).
    #[test]
    fn wheel_walks_the_ladder_and_caps_exactly() {
        let g = geo();
        let mut state = ViewState::Fit;
        let wheel_up = PointerInput::Wheel {
            up: true,
            ctrl: false,
            pos: CENTER,
        };
        let mut factors = Vec::new();
        for _ in 0..6 {
            let (next, _) = step(state, wheel_up, &g);
            state = next;
            if let ViewState::Zoomed { factor } = state {
                factors.push(factor);
            }
        }
        assert_eq!(
            factors,
            vec![1.5, 2.25, 3.375, 4.0, 4.0, 4.0],
            "caps EXACTLY at max"
        );
        // And back down: retraces stops, lands on Fit with the pan reset.
        let wheel_down = PointerInput::Wheel {
            up: false,
            ctrl: false,
            pos: CENTER,
        };
        let mut downs = Vec::new();
        loop {
            let (next, action) = step(state, wheel_down, &g);
            state = next;
            match state {
                ViewState::Zoomed { factor } => downs.push(factor),
                ViewState::Fit => {
                    assert_eq!(
                        action,
                        Action::SetZoom {
                            factor: 1.0,
                            center: (0.5, 0.5)
                        }
                    );
                    break;
                }
                ViewState::Grid { .. } => unreachable!("wheel never exits the loupe"),
            }
        }
        assert_eq!(downs, vec![3.375, 2.25, 1.5], "descent retraces ascent");
        // Wheel-down at fit stays inert forever.
        assert_eq!(step(state, wheel_down, &g).1, Action::None);
    }

    /// Clicks and double-clicks in the letterbox bars are dead (spec:
    /// a double-click on black must not slam to 1:1 on a frame edge).
    #[test]
    fn letterbox_clicks_are_ignored() {
        // Portrait image in the landscape viewport: fat pillarbox bars.
        let g = Geometry {
            native_w: 2000.0,
            native_h: 4000.0,
            ..geo()
        };
        // fit extent: scale = min(1000/2000, 800/4000) = 0.2 → 400x800:
        // bars at x < 300 and x > 700.
        let bar = (50.0, 400.0);
        assert_eq!(
            step(ViewState::Fit, PointerInput::DoubleClick { pos: bar }, &g),
            (ViewState::Fit, Action::None)
        );
        let z = ViewState::Zoomed { factor: 1.5 };
        // At 1.5x the extent is 600x1200: x=50 still in the bar.
        assert_eq!(
            step(z, PointerInput::Click { pos: bar }, &g),
            (z, Action::None)
        );
        assert_eq!(
            step(z, PointerInput::DoubleClick { pos: bar }, &g),
            (z, Action::None)
        );
        // Inside the image everything works.
        let inside = (500.0, 400.0);
        assert!(matches!(
            step(z, PointerInput::Click { pos: inside }, &g).1,
            Action::Recenter { .. }
        ));
    }

    /// Small file (1:1 at or below fit): wheel-up and double-click at fit
    /// do nothing — clamped, no flicker.
    #[test]
    fn small_file_pins_to_fit() {
        let g = Geometry {
            max_factor: Some(0.8),
            ..geo()
        };
        let up = PointerInput::Wheel {
            up: true,
            ctrl: false,
            pos: CENTER,
        };
        assert_eq!(step(ViewState::Fit, up, &g), (ViewState::Fit, Action::None));
        assert_eq!(
            step(
                ViewState::Fit,
                PointerInput::DoubleClick { pos: CENTER },
                &g
            ),
            (ViewState::Fit, Action::None)
        );
        // Unknown ceiling (full-res still decoding): climb OPTIMISTICALLY,
        // same as the keyboard `+` — the bridge's render clamp lands it
        // at 1:1 once the ceiling is known.
        let g = Geometry {
            max_factor: None,
            ..geo()
        };
        let (state, action) = step(ViewState::Fit, up, &g);
        assert_eq!(state, ViewState::Zoomed { factor: 1.5 });
        assert!(matches!(action, Action::SetZoom { factor, .. } if factor == 1.5));
        // Unknown-ceiling double-click pins the 1:1 desire as INFINITY
        // (keyboard `Z` semantics).
        let (state, action) = step(
            ViewState::Fit,
            PointerInput::DoubleClick { pos: CENTER },
            &g,
        );
        assert!(matches!(state, ViewState::Zoomed { factor } if factor.is_infinite()));
        assert!(matches!(action, Action::SetZoom { factor, .. } if factor.is_infinite()));
    }

    /// Everything above uses `pan_center = (0.5, 0.5)` and `pos = CENTER`,
    /// where the pointer anchor and the CENTRE anchor coincide — so those
    /// assertions cannot tell the two apart. QE proved it by mutation
    /// (2026-07-30): making `anchor_center` return `geo.pan_center`
    /// unchanged, making a wheel-down landing on fit KEEP the pan, and
    /// making the drag ignore `dy` all left the suite green. These three
    /// use an off-centre pan and an off-centre pointer, where the mutants
    /// and the truth diverge.
    #[test]
    fn zoomed_wheel_anchors_the_pointer_off_centre() {
        let g = Geometry {
            pan_center: (0.30, 0.70),
            ..geo()
        };
        let from = 1.5;
        let pos = (200.0, 150.0);
        let (state, action) = step(
            ViewState::Zoomed { factor: from },
            PointerInput::Wheel {
                up: true,
                ctrl: false,
                pos,
            },
            &g,
        );
        assert_eq!(state, ViewState::Zoomed { factor: 2.25 });
        let Action::SetZoom { factor, center } = action else {
            panic!("must zoom")
        };
        // The mutant returns the input pan center untouched.
        assert!(
            (center.0 - g.pan_center.0).abs() > 1e-3 || (center.1 - g.pan_center.1).abs() > 1e-3,
            "anchor must depend on the pointer, got {center:?}"
        );
        // And it is the POINTER anchor: the image point under the cursor
        // before the step is still under the cursor after it.
        let (ew0, eh0) = extents(&g, from);
        let fx = frac_under(g.viewport_w, ew0, g.pan_center.0, pos.0);
        let fy = frac_under(g.viewport_h, eh0, g.pan_center.1, pos.1);
        let (ew1, eh1) = extents(&g, factor);
        let back_x = zoompan::offset_centering(g.viewport_w, ew1, center.0) + fx * ew1;
        let back_y = zoompan::offset_centering(g.viewport_h, eh1, center.1) + fy * eh1;
        assert!(
            (back_x - pos.0).abs() < 0.5,
            "x anchor: {back_x} vs {}",
            pos.0
        );
        assert!(
            (back_y - pos.1).abs() < 0.5,
            "y anchor: {back_y} vs {}",
            pos.1
        );
    }

    /// Wheel-down landing on fit FORGETS the pan spot — asserted from an
    /// off-centre pan, so `(0.5, 0.5)` is a real reset and not an echo of
    /// the input (spec: a stale pan from three images ago is a trap).
    #[test]
    fn wheel_down_to_fit_forgets_an_off_centre_pan() {
        let g = Geometry {
            pan_center: (0.30, 0.70),
            ..geo()
        };
        let (state, action) = step(
            ViewState::Zoomed { factor: 1.5 },
            PointerInput::Wheel {
                up: false,
                ctrl: false,
                pos: (900.0, 100.0),
            },
            &g,
        );
        assert_eq!(state, ViewState::Fit);
        assert_eq!(
            action,
            Action::SetZoom {
                factor: 1.0,
                center: (0.5, 0.5)
            },
            "the carried pan must be discarded, not carried to fit"
        );
    }

    /// The drag pans on BOTH axes: a vertical-only drag must move the
    /// vertical centre (and only it).
    #[test]
    fn drag_pans_the_vertical_axis_too() {
        let g = geo();
        let z = ViewState::Zoomed { factor: 4.0 }; // extent 4000x3200
        let (_, action) = step(z, PointerInput::Drag { dx: 0.0, dy: 160.0 }, &g);
        let Action::Recenter { center } = action else {
            panic!("drag must recenter")
        };
        // offset_y = 400 - 0.5*3200 = -1200; +160 -> -1040;
        // frac = (400 + 1040) / 3200 = 0.45.
        assert!(
            (center.1 - 0.45).abs() < 1e-4,
            "dy must pan vertically, got {}",
            center.1
        );
        assert!((center.0 - 0.5).abs() < 1e-4, "dx=0 leaves x alone");
    }

    /// An unknown 1:1 ceiling climbs optimistically but NOT without bound:
    /// an uncapped ladder overflows `extents` and poisons the pan centre
    /// with NaN, which then persists across images (QE D4).
    #[test]
    fn optimistic_climb_is_bounded_and_never_nans() {
        let g = Geometry {
            max_factor: None,
            ..geo()
        };
        let up = PointerInput::Wheel {
            up: true,
            ctrl: false,
            pos: (742.0, 311.0),
        };
        let mut state = ViewState::Fit;
        for _ in 0..400 {
            let (next, action) = step(state, up, &g);
            state = next;
            if let Action::SetZoom { factor, center } = action {
                assert!(factor.is_finite(), "factor went non-finite");
                assert!(
                    center.0.is_finite() && center.1.is_finite(),
                    "NaN pan center at factor {factor}"
                );
                assert!((0.0..=1.0).contains(&center.0) && (0.0..=1.0).contains(&center.1));
            }
        }
        let ViewState::Zoomed { factor } = state else {
            panic!("must have climbed")
        };
        assert_eq!(factor, OPTIMISTIC_MAX, "climb is capped while unknown");
    }

    /// view_to_frac matches the machine's own conversion.
    #[test]
    fn view_to_frac_matches_the_machine() {
        let g = geo();
        // At fit the extent fills the 1000px width: x=750 -> frac 0.75.
        let (fx, fy) = view_to_frac(&g, 1.0, (750.0, 400.0));
        assert!((fx - 0.75).abs() < 1e-4);
        assert!((fy - 0.5).abs() < 1e-4);
    }

    /// A pinned-but-unresolved 1:1 desire (INFINITY while full-res
    /// decodes) makes EVERY pointer gesture inert — no NaN pan centers
    /// (QE defect on the first cut).
    #[test]
    fn infinite_factor_is_inert_never_nan() {
        let g = Geometry {
            max_factor: None,
            ..geo()
        };
        let z = ViewState::Zoomed {
            factor: f32::INFINITY,
        };
        let inputs = [
            PointerInput::Wheel {
                up: true,
                ctrl: false,
                pos: CENTER,
            },
            PointerInput::Wheel {
                up: false,
                ctrl: false,
                pos: CENTER,
            },
            PointerInput::Click { pos: CENTER },
            PointerInput::DoubleClick { pos: CENTER },
            PointerInput::Drag { dx: 10.0, dy: 5.0 },
        ];
        for input in inputs {
            let (state, action) = step(z, input, &g);
            assert_eq!(action, Action::None, "{input:?} must be inert");
            assert_eq!(state, z, "{input:?} must not change state");
        }
    }

    /// The two table cells the first QE pass had to probe by hand:
    /// double-click at 1:1 re-centers only; Ctrl+wheel-DOWN in the loupe
    /// equals the plain row (the modifier is ignored, both directions).
    #[test]
    fn at_max_double_click_recenters_and_ctrl_wheel_down_parity() {
        let g = geo();
        let at_max = ViewState::Zoomed { factor: 4.0 };
        let (state, action) = step(at_max, PointerInput::DoubleClick { pos: CENTER }, &g);
        assert_eq!(state, at_max, "already at 1:1: factor unchanged");
        assert!(matches!(action, Action::Recenter { .. }));
        let down = |ctrl| PointerInput::Wheel {
            up: false,
            ctrl,
            pos: CENTER,
        };
        assert_eq!(
            step(ViewState::Fit, down(true), &g),
            step(ViewState::Fit, down(false), &g)
        );
        let z = ViewState::Zoomed { factor: 2.25 };
        assert_eq!(step(z, down(true), &g), step(z, down(false), &g));
    }

    /// The fit view renders in the N=1 grid CELL (scroll-dependent, 3:2),
    /// not centered in the viewport: frac mapping and letterbox rejection
    /// follow the cell (validator MAJOR on the first cut).
    #[test]
    fn fit_cell_geometry_drives_fit_mapping() {
        // Cell at (6, 46), 988x658 (3:2-ish); landscape 4000x3200 image
        // (aspect 1.25) is PILLARBOXED in it: image w = 658*1.25 = 822.5,
        // bars of ~82.75 each side; image rect x in [88.75, 911.25].
        let g = Geometry {
            fit_cell: Some((6.0, 46.0, 988.0, 658.0)),
            ..geo()
        };
        // Double-click on the image's horizontal center, top edge:
        let pos = (6.0 + 988.0 / 2.0, 46.0);
        let (state, action) = step(ViewState::Fit, PointerInput::DoubleClick { pos }, &g);
        assert!(matches!(state, ViewState::Zoomed { factor } if factor == 4.0));
        let Action::SetZoom { center, .. } = action else {
            panic!("must zoom")
        };
        assert!(
            (center.0 - 0.5).abs() < 1e-3,
            "clicked x center: {center:?}"
        );
        assert!(center.1.abs() < 1e-3, "clicked top edge: {center:?}");
        // Double-click in the pillarbox bar: dead.
        let bar = (6.0 + 40.0, 46.0 + 329.0);
        assert_eq!(
            step(ViewState::Fit, PointerInput::DoubleClick { pos: bar }, &g),
            (ViewState::Fit, Action::None)
        );
        // Wheel-up anchored at the image's right edge: the anchored frac
        // comes from the CELL frame (x=911.25 -> frac 1.0), and the
        // projected center keeps it under the pointer at the new extents.
        let edge = (6.0 + (988.0 + 822.5) / 2.0 - 0.01, 46.0 + 329.0);
        let (_, action) = step(
            ViewState::Fit,
            PointerInput::Wheel {
                up: true,
                ctrl: false,
                pos: edge,
            },
            &g,
        );
        let Action::SetZoom { factor, center } = action else {
            panic!("must zoom")
        };
        let (ew, _) = extents(&g, factor);
        let off = zoompan::offset_centering(g.viewport_w, ew, center.0);
        let back = off + 1.0 * ew; // frac 1.0 on screen after the zoom
        assert!(
            (back - edge.0).abs() < 1.0 || off == g.viewport_w - ew,
            "anchor holds or the edge clamp wins: back {back} vs {}",
            edge.0
        );
    }

    /// Drag pans 1:1 with pointer motion and clamps at the image edges —
    /// the image never detaches from the viewport.
    #[test]
    fn drag_pans_and_clamps() {
        let g = geo();
        let z = ViewState::Zoomed { factor: 4.0 }; // extent 4000x3200
                                                   // Centered: offset_x = 500 - 0.5*4000 = -1500. Drag +100 → -1400:
                                                   // frac = (500 + 1400) / 4000 = 0.475.
        let (state, action) = step(z, PointerInput::Drag { dx: 100.0, dy: 0.0 }, &g);
        assert_eq!(state, z, "drag never changes the factor");
        let Action::Recenter { center } = action else {
            panic!("drag must recenter")
        };
        assert!((center.0 - 0.475).abs() < 1e-4, "1:1 with pointer motion");
        assert!((center.1 - 0.5).abs() < 1e-4);
        // A huge drag clamps at the edge instead of detaching.
        let (_, action) = step(z, PointerInput::Drag { dx: 1e6, dy: 0.0 }, &g);
        let Action::Recenter { center } = action else {
            panic!()
        };
        let off = zoompan::offset_centering(g.viewport_w, 4000.0, center.0);
        assert_eq!(off, 0.0, "clamped: image left edge pinned to viewport");
    }

    /// The machine never touches state it doesn't own: every action is
    /// zoom/pan/routing — there is no mark, cursor, filter or selection
    /// variant to emit at all (type-level guarantee, asserted here for
    /// the record).
    #[test]
    fn grid_states_pass_through_unchanged() {
        let g = geo();
        for cols in [12u8, 8, 6, 4, 3, 2] {
            let s = ViewState::Grid { columns: cols };
            let (next, _) = step(s, PointerInput::Click { pos: CENTER }, &g);
            assert_eq!(next, s, "click never changes the grid zoom");
            let (next, _) = step(
                s,
                PointerInput::Wheel {
                    up: true,
                    ctrl: false,
                    pos: CENTER,
                },
                &g,
            );
            assert_eq!(next, s, "wheel scrolls, never zooms the grid");
        }
    }
}
