//! Loupe asset engine: full-resolution embedded-JPEG decodes for the
//! 1-column view and 1:1 zoom (`specs/modules/raw-pipeline.md` FullRes asset).
//!
//! Asset ladder (user decision, raw-pipeline.md): each image climbs
//! mid-preview (1616×1080, ~5 ms) → the screen rung (the full JPEG decoded
//! at N/8 to the loupe's fit box, at fit on a wide viewport; brief 008) →
//! full-res (8640×5760, ~140 ms), and a rung is only cooked when the display
//! exceeds the current asset by more than `UPSCALE_THRESHOLD` (1.25×). Every
//! rung is published as its own Ready event so the UI swaps quality in place
//! without blocking.
//!
//! `focus(index, display_long)` and `focus_fit(index)` schedule the focused
//! image at top priority and its RING — `RING_BEHIND` behind and
//! `RING_AHEAD` ahead, leaning the way of travel (raw-pipeline.md, "The
//! ring") — in VIEW order, the order arrows actually travel (`set_view`;
//! issue #46): an id-space ring on a capture-sorted multi-body folder warmed
//! frames no arrow could reach while every real neighbor stayed cold. A
//! byte-budget LRU (2 GiB unless the caller sizes it from the machine,
//! `budget.rs`) evicts the least recently focused images, never the focused
//! one.

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use crate::raw::{find_embedded_jpegs, read_jpeg};

/// The settled ring on each side of the focused image for an engine with no
/// fit box (raw-pipeline.md, "An engine with no fit box") — the behaviour
/// before brief 008, which the older core tests pin — and the app's full-res
/// texture window while it supplies no box.
pub const PREFETCH: usize = 2;
/// THE ring (raw-pipeline.md, "The ring"; the user, 2026-09-26: "What if we
/// do 2 behind and 15 in front? I feel we are overly complicating things."):
/// frames behind and ahead of the cursor in VIEW order, leaning the way of
/// travel, at every factor, travelling and settled alike — fixed, never
/// derived from the machine. The folder's ends clamp it; above fit the
/// pixel cache may shorten its far end (`fullres_ring_ahead`).
pub const RING_BEHIND: usize = 2;
/// See [`RING_BEHIND`].
pub const RING_AHEAD: usize = 15;
/// The reference A1 frame, 8640 × 5760 × 3 bytes of RGB: what one full-res
/// frame costs in the pixel cache, and again as the app's texture copy.
pub const REF_FRAME_BYTES: u64 = 149_299_200;
/// Default decoded-pixels budget (bytes of RGB kept in the LRU): the pixel
/// cache's floor (raw-pipeline.md, "Memory"), which `LoupeEngine::start`
/// and the older core tests run with.
pub const DEFAULT_BUDGET_BYTES: usize = 2 * 1024 * 1024 * 1024;
/// Asset ladder rule (user decision): a loaded asset serves any display up
/// to 25% larger than itself; beyond that the next rung is cooked.
pub const UPSCALE_THRESHOLD: f32 = 1.25;
/// Assets at or below this long edge are "mid rung" class (grid-cell size).
pub const MID_RUNG_MAX_LONG: u32 = 2048;
/// Downscale target when adopting a full-res image for a grid cell.
pub const MID_RUNG_TARGET: u32 = 1616;

/// Is this decoded asset the file's TOP rung — the one the sharp 1:1 view
/// may use and the one the zoom ceiling is read from?
///
/// Two ways to qualify, and both matter:
/// * `long_edge` above `MID_RUNG_MAX_LONG` — a real full-res decode;
/// * `terminal` — the file has nothing better to give (bare JPEGs and
///   other single-rung sources, issue #8), so its native size IS the
///   ceiling however small it is.
///
/// This is the meaning of `MID_RUNG_MAX_LONG`, and it was spelled by hand
/// at five call sites; one of them shipped without the terminal half and
/// made every small-JPEG session refuse to sharpen (QE D2).
pub fn is_top_rung(long_edge: u32, terminal: bool) -> bool {
    long_edge > MID_RUNG_MAX_LONG || terminal
}

/// The loupe's N=1 cell in PHYSICAL pixels, both sides > 0: what "at fit"
/// asks the ladder to fill (raw-pipeline.md, "The screen rung": "The fit
/// box is the loupe's N=1 cell in physical pixels, which the app supplies
/// on every refresh at the loupe").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FitBox {
    pub width: u32,
    pub height: u32,
}

/// Which decode a loupe image is: the rung the decoder RAN, never a size
/// (raw-pipeline.md: "The kind is what the decoder RAN — a scale below 8/8
/// is `screen` — never a comparison with an IFD's size claim, which a file
/// can under-state"). A screen rung is never the top rung however large, so
/// the app routes by this before any size test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RungKind {
    /// The mid preview (the 1616-class embedded JPEG).
    Mid,
    /// The full JPEG decoded at N/8, N < 8.
    Screen,
    /// The full JPEG at full scale — a bare JPEG's one rung included.
    Full,
}

impl std::fmt::Display for RungKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RungKind::Mid => "mid",
            RungKind::Screen => "screen",
            RungKind::Full => "full",
        })
    }
}

/// The request state a decode carried (ui-grid.md, "Transit and settled";
/// raw-pipeline.md, "The request state travels with the decode"): an
/// instrument carried out of core on the `Ready` event, not behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestState {
    /// Asked while the user was moving (a held key, a Y/N chain).
    Transit,
    /// Asked at rest — and by grid wants, which have no travel.
    #[default]
    Settled,
}

impl std::fmt::Display for RequestState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RequestState::Transit => "transit",
            RequestState::Settled => "settled",
        })
    }
}

/// A decoded full-resolution image, shared with the UI without copying.
#[derive(Debug, Clone)]
pub struct FullImage {
    pub rgb: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
    /// The rung this decode is (see [`RungKind`]).
    pub kind: RungKind,
}

#[derive(Debug, Clone)]
pub enum LoupeEvent {
    Ready {
        index: usize,
        image: FullImage,
        /// True when this is the file's BEST possible rung (its native
        /// resolution): single-rung sources (bare JPEGs, issue #8) have a
        /// terminal rung at or below mid-class size, and the app needs
        /// the signal to learn the zoom ceiling from it. A screen rung is
        /// never terminal.
        terminal: bool,
        /// The request state the decode carried when it was queued (an
        /// instrument for ui-grid.md A5: under a transit capped at the mid
        /// no screen rung ever lands `transit`).
        state: RequestState,
    },
    Failed {
        index: usize,
        reason: String,
    },
}

/// The reference landscape A1 mid, 1616×1080: the yardstick of the "wide
/// viewport" predicate, [`mid_serves_box`] (raw-pipeline.md, "The idle
/// cook": "Wide is one predicate, decided per VIEWPORT").
pub const REFERENCE_MID: (u32, u32) = (1616, 1080);

/// What a request asks the ladder for: a display long edge (`Long`, the
/// app's real target — `u32::MAX` above fit, the top rung) or the fit box
/// (`Fit`, the cheapest rung that serves the loupe's N=1 cell).
///
/// Ordered — `schedule` merges a deferred target with `max`, and
/// `note_focus` re-arms the debounce on an escalation — by the long edge
/// first, then `Long` above `Fit` at an equal long edge, then the box, so
/// that `cmp` says `Equal` exactly when the two are `==`. NOT a derived
/// `Ord`: comparing boxes by their long edge alone would call
/// `Fit(3840×2160)` equal to `Fit(3840×1600)` while `==` calls them
/// different, and a merge would then keep whichever came first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Long(u32),
    Fit(FitBox),
}

impl Target {
    /// The sort key; see the type's doc.
    fn key(self) -> (u32, u8, u32, u32) {
        match self {
            Target::Long(l) => (l, 1, 0, 0),
            Target::Fit(b) => (b.width.max(b.height), 0, b.width, b.height),
        }
    }

    /// The long edge the target asks for (a box's longer side).
    fn long(self) -> u32 {
        self.key().0
    }
}

impl Ord for Target {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for Target {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Default for Target {
    /// Nothing asked yet: the `desired_long: 0` of the engine before brief
    /// 008.
    fn default() -> Self {
        Target::Long(0)
    }
}

/// One queued request. `focus_origin` survives the grid-want cull; `state`
/// is the request state of the focus that last scheduled or re-targeted it
/// (raw-pipeline.md, "The request state travels with the decode").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entry {
    pub index: usize,
    pub target: Target,
    pub focus_origin: bool,
    pub state: RequestState,
}

/// Width × height with the two swapped for an EXIF orientation of 5..=8
/// (the transposing ones): what the screen draws.
fn oriented(width: u32, height: u32, orientation: u16) -> (u32, u32) {
    if matches!(orientation, 5..=8) {
        (height, width)
    } else {
        (width, height)
    }
}

/// The 1.25 rule for a box — its ONE home; the app asks it too (ui-grid.md,
/// the quality rule). An image of `width`×`height` (ORIENTED, as the screen
/// draws it) serves the fit box when the screen shows it upscaled by at
/// most `UPSCALE_THRESHOLD`: min(bw/w, bh/h) ≤ 5/4, i.e. 4·bw ≤ 5·w or
/// 4·bh ≤ 5·h — exact integer arithmetic in u64, no float. A zero side never
/// serves.
pub fn serves_box(width: u32, height: u32, fit_box: FitBox) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    let (w, h) = (u64::from(width), u64::from(height));
    let (bw, bh) = (u64::from(fit_box.width), u64::from(fit_box.height));
    4 * bw <= 5 * w || 4 * bh <= 5 * h
}

/// The "wide viewport" predicate, decided per VIEWPORT (raw-pipeline.md, the
/// idle cook): true when the reference landscape A1 mid, 1616×1080, serves
/// the fit box — a viewport up to ~2K, where nothing on the decode path
/// changes; false on a wide one (QHD, 4K, 5K).
pub fn mid_serves_box(fit_box: FitBox) -> bool {
    serves_box(REFERENCE_MID.0, REFERENCE_MID.1, fit_box)
}

/// The frame already FITS the box × 1.25, so no rung is worth decoding and
/// the full is the target (raw-pipeline.md, "The factor rule": "A frame
/// whose full JPEG already fits within the box × 1.25 gets no rung"):
/// 4·w ≤ 5·bw and 4·h ≤ 5·bh. NOT `serves_box` of the full, which is true of
/// every frame LARGER than the box — the trap the first branch fell into.
pub fn fits_box(width: u32, height: u32, fit_box: FitBox) -> bool {
    let (w, h) = (u64::from(width), u64::from(height));
    let (bw, bh) = (u64::from(fit_box.width), u64::from(fit_box.height));
    4 * w <= 5 * bw && 4 * h <= 5 * bh
}

/// The box rule (raw-pipeline.md, "The factor rule"): the smallest N in
/// 1..=7 whose N/8 decode of the full JPEG, ORIENTED, serves the fit box —
/// "the smallest N that serves", never "the nearest" — or `None` when the
/// oriented full already fits the box × 1.25, or when no N below 8 serves
/// (N = 8 is the full). `full_width`×`full_height` are the stored, unrotated
/// sizes; orientation 5..=8 swaps them, and swaps the scaled sizes alike.
pub fn rung_factor(
    full_width: u32,
    full_height: u32,
    orientation: u16,
    fit_box: FitBox,
) -> Option<u8> {
    let (w, h) = oriented(full_width, full_height, orientation);
    if fits_box(w, h, fit_box) {
        return None;
    }
    (1..=7u8).find(|&n| {
        let (sw, sh) = scaled_dims(full_width, full_height, n);
        let (sw, sh) = oriented(sw, sh, orientation);
        serves_box(sw, sh, fit_box)
    })
}

/// What one frame at fit is served by (raw-pipeline.md, "The fit box": "the
/// ladder serves it with the cheapest rung: the mid when its ORIENTED size
/// serves the box, else the screen rung, else the full").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitRungChoice {
    Mid,
    /// The screen rung at this N/8.
    Screen(u8),
    Full,
}

/// The ladder's composition at fit for one frame: the mid when its ORIENTED
/// size serves the box (`mid` is the stored, unrotated size, `None` for a
/// bare JPEG), else [`rung_factor`]'s rung, else the full.
pub fn fit_rung(
    full_width: u32,
    full_height: u32,
    mid: Option<(u32, u32)>,
    orientation: u16,
    fit_box: FitBox,
) -> FitRungChoice {
    if let Some((mw, mh)) = mid {
        let (ow, oh) = oriented(mw, mh, orientation);
        if serves_box(ow, oh, fit_box) {
            return FitRungChoice::Mid;
        }
    }
    match rung_factor(full_width, full_height, orientation, fit_box) {
        Some(n) => FitRungChoice::Screen(n),
        None => FitRungChoice::Full,
    }
}

/// How far ahead the full-res ring reaches above fit (raw-pipeline.md,
/// "Above fit"): clamped at its far end so that the pixel cache's figure
/// holds its frames twice over — each frame's decoded pixels in the cache
/// and its texture copy outside it — and the kitchen's fill in flight
/// besides: ⌊(cache − `REF_FRAME_BYTES`) ÷ (2 × `REF_FRAME_BYTES`)⌋ frames in
/// all, the cursor and the `RING_BEHIND` always, the rest ahead up to
/// `RING_AHEAD` (Manager rulings 2026-09-26, brief 008 Q4 and Q-G): 3 ahead
/// at the 2 GiB floor, 10 on a 4 GiB cache, the whole 15 from 5,524,070,400 B.
/// A ring the cache cannot hold would be decoded and then evicted, and one
/// whose textures the RAM cannot hold would take the whole app past it. The
/// startup line prints it (`budget.rs`).
pub fn fullres_ring_ahead(cache_bytes: u64) -> usize {
    let frames = cache_bytes.saturating_sub(REF_FRAME_BYTES) / (2 * REF_FRAME_BYTES);
    let frames = usize::try_from(frames).unwrap_or(usize::MAX);
    RING_AHEAD.min(frames.saturating_sub(1 + RING_BEHIND))
}

/// A ring's extent in VIEW positions around the cursor, already leaned the
/// way of travel: `before` positions below the cursor and `after` above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingWindow {
    pub before: usize,
    pub after: usize,
}

impl RingWindow {
    /// `behind` frames behind the cursor and `ahead` in front of it, for
    /// travel forward (`before` = behind) or backward (`before` = ahead) —
    /// THE lean, in one place, so the engine's ring and the app's texture
    /// rings cannot lean differently.
    pub fn leaning(behind: usize, ahead: usize, forward: bool) -> Self {
        if forward {
            RingWindow {
                before: behind,
                after: ahead,
            }
        } else {
            RingWindow {
                before: ahead,
                after: behind,
            }
        }
    }

    /// `depth` positions on both sides: plain distance.
    pub const fn symmetric(depth: usize) -> Self {
        RingWindow {
            before: depth,
            after: depth,
        }
    }

    /// How many frames the window holds, the cursor's included.
    pub fn capacity(&self) -> usize {
        self.before + self.after + 1
    }

    /// Is view position `pos` inside the window around the cursor at view
    /// position `cursor_pos`?
    pub fn contains(&self, cursor_pos: usize, pos: usize) -> bool {
        pos.saturating_add(self.before) >= cursor_pos
            && pos <= cursor_pos.saturating_add(self.after)
    }

    /// The view positions the window reaches around the cursor at view
    /// position `cursor_pos`, in a view of `len` positions: the positions
    /// [`contains`](Self::contains) says yes to, clamped at both ends of the
    /// view, as one range — empty when the cursor is not in the view. One
    /// home for the clamp, so the app's rescue-thumb lead and the kitchen's
    /// fill window (ui-grid.md, "Virtualization"; 01-architecture.md, the
    /// kitchen) cannot reach different frames.
    pub fn span(&self, cursor_pos: usize, len: usize) -> std::ops::Range<usize> {
        if cursor_pos >= len {
            return 0..0;
        }
        let lo = cursor_pos.saturating_sub(self.before);
        let hi = cursor_pos.saturating_add(self.after).min(len - 1);
        lo..hi + 1
    }

    /// [`span`](Self::span) in the order the cursor meets its frames: the
    /// cursor's own first, then the nearest, at equal distance the one toward
    /// the window's lean first — `after > before` leans forward, and a
    /// symmetric window reads forward, as in `transit::next_fill` — which is
    /// the order the engine decodes its ring in (`ring_order`, reversed:
    /// raw-pipeline.md, "Order in the queue"). The order the app sends the
    /// rung window's thumbs to the kitchen in (ui-grid.md, "Virtualization":
    /// "the cursor's first"), so the frame an arrow reaches next is cooked
    /// first after a jump or a reversal.
    pub fn nearest_first(&self, cursor_pos: usize, len: usize) -> Vec<usize> {
        let span = self.span(cursor_pos, len);
        if span.is_empty() {
            return Vec::new();
        }
        let forward = self.after >= self.before;
        // Push order, farthest first, the cursor excluded — reversed below.
        let mut order = ring_order(cursor_pos, span.start, span.end - 1, forward);
        order.push(cursor_pos);
        order.reverse();
        order
    }
}

/// The windows of the app's two texture rings (ui-grid.md, "The render
/// ladder": `transit::evict_ring`), leaned by the engine's own travel latch,
/// which the app never re-derives: `rung` for the screen-rung ring, `full`
/// for the full-res ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextureWindows {
    pub rung: RingWindow,
    pub full: RingWindow,
}

/// The switch rule's state for the current hold above fit (raw-pipeline.md,
/// "Above fit: the full-res ring and the switch rule"): where the members
/// ahead stop asking for full-res and ask for the fit box, and where they
/// ask for full-res again. Both boundaries are VIEW positions, read in the
/// travel direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SwitchState {
    /// Rule 1's boundary: the member at this position and every member
    /// beyond it ask for the fit box. `None`: no step-down in force.
    down: Option<usize>,
    /// Rule 2's boundary, stored as the FAR END of the ring in force at the
    /// last step-up: the members up to it ask for the fit box, and those
    /// strictly beyond it — "from the first position beyond the ring's far
    /// end onward" — ask for full-res again. The far end rather than the
    /// first position beyond it, because on a backward hold whose ring
    /// reaches the folder's first frame no position lies beyond it, and a
    /// view position cannot be −1. `None`: no step-up this hold.
    up: Option<usize>,
    /// Rule 3: a step-down less than one ring past the last step-up's
    /// boundary keeps the hold on the fit box until it ends — no step-up.
    locked: bool,
}

/// How many view positions `pos` lies beyond `from` in the travel direction
/// — at least 1 — or `None` when it does not lie beyond it. Rule 1's
/// distance ahead (counted from 1: the first member ahead is 1), and the
/// switch rule's "beyond a boundary".
fn ahead_by(from: usize, pos: usize, forward: bool) -> Option<usize> {
    let distance = if forward {
        pos.checked_sub(from)
    } else {
        from.checked_sub(pos)
    };
    distance.filter(|d| *d > 0)
}

/// Is `pos` at `boundary` or beyond it in the travel direction?
fn at_or_beyond(pos: usize, boundary: usize, forward: bool) -> bool {
    pos == boundary || ahead_by(boundary, pos, forward).is_some()
}

/// During a hold above fit, does the member ahead at view position `pos`
/// ask for full-res, by the switch rule? Not at or beyond rule 1's
/// step-down boundary, and — after a step-up — only strictly beyond the
/// step-up's far end (rule 2); with neither in force, every member ahead.
fn switch_asks_full(switch: SwitchState, pos: usize, forward: bool) -> bool {
    switch
        .up
        .is_none_or(|far_end| ahead_by(far_end, pos, forward).is_some())
        && switch
            .down
            .is_none_or(|down| !at_or_beyond(pos, down, forward))
}

#[derive(Default)]
struct LoupeState {
    /// Pending requests, most urgent last (workers pop from the back); one
    /// entry per index — the LATEST target wins (it reflects current
    /// intent; an escalation dropped while in flight self-heals via the
    /// Ready→refresh loop). `focus_origin` entries survive want-culling.
    queue: Vec<Entry>,
    /// Best rung a file can ever provide (long edge), learned when its
    /// ladder tops out: an asset at this size is sufficient for ANY display
    /// — without this memo, 1:1 (u32::MAX target) re-parsed files forever
    /// (validator MAJOR finding).
    best_long: HashMap<usize, u32>,
    in_flight: Vec<usize>,
    /// Upgrade targets requested while the index was in flight at a smaller
    /// target: re-queued when the flight lands (QE defect — the upgrade was
    /// silently dropped, 1:1 never arrived without the app's refresh loop).
    /// The request state rides beside the target, so a revival keeps the
    /// state of the focus whose target was deferred, never the mode at
    /// revival (raw-pipeline.md, "The request state travels with the
    /// decode").
    deferred: HashMap<usize, (Target, RequestState)>,
    /// LRU cache: index -> (image, last-focus stamp).
    cache: HashMap<usize, (FullImage, u64)>,
    cached_bytes: usize,
    /// Indexes that failed to decode: never re-queued (a corrupt file must
    /// not be re-attempted on every focus — validator finding).
    failed: std::collections::HashSet<usize>,
    /// The embedded JPEGs — (index, offset in the file) — whose complaint
    /// line has been printed: at most once per session each, however often
    /// the ladder decodes them (raw-pipeline.md, "One line on stderr,
    /// once").
    noted: std::collections::HashSet<(usize, u64)>,
    /// The image the user is looking at: never evicted, even over-budget —
    /// evicting it after decode would strand the loupe forever (found by
    /// the tight-budget integration test).
    focused: Option<usize>,
    /// When the focus last became NEW WORK: reset when the focused index
    /// changes AND when the focused index's target escalates (see
    /// note_focus) — the reserved worker's debounce clock
    /// (FOCUS_DEBOUNCE: neither a transient transit focus nor a big
    /// climb freshly queued for a resting frame may capture the lane).
    focused_at: Option<std::time::Instant>,
    /// The target of the last focus for `focused` — escalation detection
    /// for the debounce clock, by the `Target` order.
    focused_target: Target,
    /// When the focused INDEX last changed. Unlike `focused_at`, a target
    /// escalation does not reset it — this is the TRANSIT clock.
    last_index_change: Option<std::time::Instant>,
    /// Direction of travel, latched at the last real index CHANGE.
    ///
    /// It cannot be re-derived per call from the previous focus: the app
    /// re-focuses the SAME index on every refresh, and refresh runs on
    /// every decode landing — of which transit produces one per ring
    /// member per frame. `index >= prev` is trivially true for those, so
    /// deriving it per call flipped the ring forward within milliseconds
    /// of every backward step, and a backward hold prefetched the frames
    /// the user was moving away from (validator + QE, 2026-08-01).
    travel_forward: bool,
    /// True when the last index change followed the previous one closely
    /// enough to be a held key rather than a deliberate tap. Decays via
    /// `in_transit`.
    moving: bool,
    /// What the APP asked for, before transit capping. Transit downgrades
    /// the REQUEST, so the settle must remember the real intent or a frame
    /// would stay soft forever once the user stops.
    desired: Target,
    /// The loupe's fit box, as the app last supplied it (`set_fit_box`);
    /// `None` before the first layout, off the loupe, and in every engine
    /// whose consumer never calls it — which then keeps the behaviour
    /// before brief 008 (raw-pipeline.md, "An engine with no fit box").
    fit_box: Option<FitBox>,
    /// How far ahead the full-res ring reaches above fit, as the pixel cache
    /// clamps it (`fullres_ring_ahead` of the engine's budget, set by
    /// `start_with` — its one home); `None`, the unit tests' default, is the
    /// whole ring, `RING_AHEAD`.
    fullres_clamp: Option<usize>,
    /// The switch rule's boundaries for the current hold above fit
    /// (raw-pipeline.md, "Above fit"). Reset only at an index change that is
    /// not part of a hold, or at a reversal (`note_focus`).
    switch: SwitchState,
    /// The full-res TIME-TO-SCREEN (rule 1): for the latest full-res decode
    /// whose fill the app reported complete, the time from the decode's start
    /// to that report (`note_adopted`). `None` until the first such report.
    time_to_screen: Option<std::time::Duration>,
    /// The open time-to-screen measurements: an index whose full-res decode
    /// ran with the engine's fit box in place from its start to its publish,
    /// and when that decode started. Ended ONLY by facts (Manager ruling
    /// Q-K): the app's report that the fill completed (`note_adopted`, which
    /// measures) or was culled (`note_dropped`, which does not), a newer
    /// decode of the index (which replaces it), and the box going
    /// (`set_fit_box(None)`, which clears them all). Never by the frame's
    /// position — a frame the cursor has passed is exactly the slow landing
    /// rule 1 must see.
    full_started: HashMap<usize, std::time::Instant>,
    /// How many times the fit box has gone (`set_fit_box(None)` over a box):
    /// a decode that began under an earlier count ran across a time with no
    /// box, so its full-res frame opens no measurement (`DecodeStart`).
    box_epoch: u64,
    /// The hold's KEY PERIOD (rule 1): the interval between the last two
    /// index changes, set at each index change before its clock moves on.
    key_period: Option<std::time::Duration>,
    /// How many backlog workers the engine runs (`start_with`: every decoder
    /// but the reserved lane) and how many are decoding now (`worker`): rule
    /// 2's "a backlog worker is free".
    backlog_workers: usize,
    backlog_busy: usize,
    /// The settled ring around the focused frame has been asked since the
    /// frame was reached — by a settled focus of the app's own, or by the
    /// reserved lane when the frame needed nothing (raw-pipeline.md, "The
    /// settled ring after a hold"; Manager ruling Q-I). Reset at every index
    /// change. A guard, not an optimisation: `schedule` reports a focus
    /// re-push as queued, so without it the lane would ask again at every
    /// wake and wake the backlog workers each time.
    settled_ring_asked: bool,
    /// The reserved lane queued the settled ring for the backlog workers
    /// while holding the state lock (`next_job` is pure over the state, so it
    /// cannot wake them itself): the worker loop takes this and wakes them.
    wake_backlog: bool,
    /// The app's VIEW order: `view_ids[pos]` = image id, `view_pos[id]` =
    /// position (usize::MAX = filtered out). The prefetch ring and the
    /// travel-direction latch walk THIS order, because arrows move over
    /// view positions (issue #46 M1): the old id-space ring, on a
    /// capture-sorted folder whose filenames interleave (two bodies, or
    /// repeated capture times), prefetched frames no arrow could reach
    /// while every real neighbor stayed cold — a deterministic fit-flash
    /// on every step. Empty = identity (id order): core-only consumers
    /// and engines whose app never calls `set_view` keep the old
    /// behavior exactly.
    view_ids: Vec<usize>,
    view_pos: Vec<usize>,
}

impl LoupeState {
    /// View position of an image id (identity when no view is set).
    fn pos_of(&self, id: usize) -> Option<usize> {
        if self.view_ids.is_empty() {
            return Some(id);
        }
        self.view_pos.get(id).copied().filter(|p| *p != usize::MAX)
    }

    /// Image id at a view position (identity when no view is set).
    fn id_at(&self, pos: usize) -> Option<usize> {
        if self.view_ids.is_empty() {
            Some(pos)
        } else {
            self.view_ids.get(pos).copied()
        }
    }

    /// Length of the space the ring is clamped to: the view when set,
    /// else the whole folder.
    fn ring_len(&self, count: usize) -> usize {
        if self.view_ids.is_empty() {
            count
        } else {
            self.view_ids.len()
        }
    }
}

/// Install a view order (the body of [`LoupeEngine::set_view`], pure so
/// the ring tests exercise the shipped mapping, not a re-implementation).
fn apply_view(state: &mut LoupeState, order: &[usize], count: usize) {
    state.view_ids = order.to_vec();
    state.view_pos = vec![usize::MAX; count];
    for (pos, id) in order.iter().enumerate() {
        if *id < count {
            state.view_pos[*id] = pos;
        }
    }
}

/// The ring's VIEW positions `lo..=hi` around the focused position `fpos`,
/// the focused one excluded (the caller pushes it last), in PUSH order:
/// farthest first — workers pop from the back, so the nearest is popped
/// soonest — and at equal distance the one in the travel direction LATER,
/// popped first (raw-pipeline.md, "Order in the queue"). Before brief 008 a
/// stable sort put the higher position later whatever the direction: right
/// on a forward hold, wrong on every backward one.
fn ring_order(fpos: usize, lo: usize, hi: usize, forward: bool) -> Vec<usize> {
    let mut ring: Vec<usize> = (lo..=hi).filter(|p| *p != fpos).collect();
    ring.sort_by_key(|&p| {
        let toward_travel = if forward { p > fpos } else { p < fpos };
        (std::cmp::Reverse(p.abs_diff(fpos)), toward_travel)
    });
    ring
}

struct Shared {
    state: Mutex<LoupeState>,
    wakeup: Condvar,
    paths: Vec<PathBuf>,
    events: Sender<LoupeEvent>,
    shutdown: AtomicBool,
    stamp: AtomicU64,
    budget: usize,
}

/// Handle; dropping stops the workers.
pub struct LoupeEngine {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl LoupeEngine {
    /// The engine with three decoders whatever the machine — the form the
    /// older core tests run on every seat (raw-pipeline.md, Contracts); the
    /// app starts it with the machine's own count, [`start_with`](Self::start_with).
    pub fn start(paths: Vec<PathBuf>, budget: usize) -> (Self, Receiver<LoupeEvent>) {
        Self::start_with(paths, budget, 3)
    }

    /// The engine with `decoders` workers (at least 2) and a pixel cache of
    /// `cache_bytes` (at least 200 MiB, room for one A1 frame): the app's
    /// form, both figures derived from the machine by `budget.rs`
    /// (raw-pipeline.md, "The decode workers" and "Memory").
    ///
    /// The LAST worker is the focus-reserved lane and the rest are backlog
    /// workers (see next_job/FOCUS_DEBOUNCE/note_focus): the reserved thread
    /// only commits to a focus whose pending work has HELD for the debounce,
    /// so neither transient transit focuses nor a climb freshly escalated on
    /// a resting frame capture it — the lane is free at the first settle
    /// after sub-debounce transits, and that frame's ladder starts within
    /// ~debounce even when every backlog worker is mid-flight on a
    /// multi-second decode. At least two, so one backlog worker still reads
    /// ahead beside the lane (`FASTCULL_DECODERS=1` reads as 2). Each thread
    /// is named — `fastcull-loupe-N`, the lane `fastcull-loupe-reserved` —
    /// and the name and the lane's role read the same `reserved` flag.
    pub fn start_with(
        paths: Vec<PathBuf>,
        cache_bytes: usize,
        decoders: usize,
    ) -> (Self, Receiver<LoupeEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let budget = cache_bytes.max(200 * 1024 * 1024); // room for at least one A1
        let decoders = decoders.max(2);
        let state = LoupeState {
            // The full-res ring's clamp follows the LRU's real size.
            fullres_clamp: Some(fullres_ring_ahead(
                u64::try_from(budget).unwrap_or(u64::MAX),
            )),
            // Every decoder but the reserved lane (rule 2's free worker).
            backlog_workers: decoders - 1,
            ..Default::default()
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            wakeup: Condvar::new(),
            paths,
            events: tx,
            shutdown: AtomicBool::new(false),
            stamp: AtomicU64::new(0),
            budget,
        });
        let workers = (0..decoders)
            .map(|n| {
                let reserved = n + 1 == decoders;
                let name = if reserved {
                    "fastcull-loupe-reserved".to_owned()
                } else {
                    format!("fastcull-loupe-{n}")
                };
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name(name)
                    .spawn(move || worker(&shared, reserved))
                    .expect("spawn a loupe worker")
            })
            .collect();
        (Self { shared, workers }, rx)
    }

    /// The leaned windows of the app's two texture rings (ui-grid.md, "The
    /// render ladder"): the screen-rung ring's is the ring, `RING_BEHIND` /
    /// `RING_AHEAD`; the full-res ring's is the full-res ring as the pixel
    /// cache clamps it, and, with no fit box, the settled ±`PREFETCH` ring —
    /// both leaned by the engine's own travel latch, read under the state
    /// lock, never re-derived by the app.
    pub fn texture_windows(&self) -> TextureWindows {
        windows_of(&lock(&self.shared))
    }

    /// The user is looking at `index` on a display whose longest edge is
    /// `display_long` physical pixels: ensure it and its ring — in VIEW
    /// order (see `set_view`) — have what the ring plan asks of each
    /// (raw-pipeline.md, "The ring") or are queued. Returns the best cached
    /// image immediately (which may be a lower rung — a better one arrives as
    /// an event once cooked).
    pub fn focus(&self, index: usize, display_long: u32) -> Option<FullImage> {
        self.focus_request(index, FocusRequest::Long(display_long))
    }

    /// [`focus`](Self::focus) at FIT: the request is the loupe's fit box
    /// (`set_fit_box`), served by the cheapest rung that serves it — the
    /// mid on viewports up to ~2K, the screen rung on wider ones
    /// (raw-pipeline.md, "The fit box"). With no box — before the app's
    /// first layout — the request is the mid, `MID_RUNG_TARGET`.
    pub fn focus_fit(&self, index: usize) -> Option<FullImage> {
        self.focus_request(index, FocusRequest::Fit)
    }

    /// The lock, the stamp, the clock read and the wake-up around the one
    /// pure body both focus forms share, [`focus_on`].
    fn focus_request(&self, index: usize, desired: FocusRequest) -> Option<FullImage> {
        let count = self.shared.paths.len();
        if count == 0 || index >= count {
            return None;
        }
        let stamp = self.shared.stamp.fetch_add(1, Ordering::Relaxed) + 1;
        let now = std::time::Instant::now();
        let mut state = lock(&self.shared);
        let hit = focus_on(&mut state, index, desired, count, stamp, now);
        drop(state);
        self.shared.wakeup.notify_all();
        hit
    }

    /// Supply the loupe's fit box — its N=1 cell in physical pixels — or
    /// `None` when there is none (before the first layout, off the loupe).
    /// The app calls this on every refresh at the loupe, the way it
    /// supplies the view order; a box with a zero side is stored as `None`.
    /// An engine whose consumer never calls it keeps the behaviour before
    /// brief 008 (raw-pipeline.md, "An engine with no fit box"). The box
    /// going ends every open time-to-screen measurement unmeasured
    /// ([`note_adopted`](Self::note_adopted)).
    pub fn set_fit_box(&self, fit_box: Option<FitBox>) {
        apply_fit_box(&mut lock(&self.shared), fit_box);
    }

    /// The app's report that a fill it made for `index` — a texture of rung
    /// `kind` — COMPLETED at the loupe, and whether its texture ring then
    /// kept it (`held`). For a full-res frame this is where the switch rule's
    /// time-to-screen ends: the moment the frame is ready to draw
    /// (raw-pipeline.md, "Above fit", rule 1; Manager ruling 2026-09-26,
    /// brief 008 Q-K). The app reports every completed fill at the loupe and
    /// carries no flag of its own; the engine decides what a report means —
    /// see [`adopted`].
    pub fn note_adopted(&self, index: usize, kind: RungKind, held: bool) {
        let now = std::time::Instant::now();
        adopted(&mut lock(&self.shared), index, kind, held, now);
    }

    /// The app's report that it CULLED `index`'s queued full-res fill — the
    /// kitchen dropped it as outside the full-res texture window (ui-grid.md,
    /// "The render ladder"). The fill never happened, so there is no moment
    /// its frame was ready to draw: the decode's time-to-screen measurement
    /// ends unmeasured (Manager ruling Q-K).
    pub fn note_dropped(&self, index: usize) {
        dropped(&mut lock(&self.shared), index);
    }

    /// How long the request state stays TRANSIT without another index
    /// change — the app's pill input (raw-pipeline.md, Contracts;
    /// `transit::cue_pill`): while [`in_transit`], `SETTLE_DEBOUNCE` minus
    /// the time since the last index change, so its `is_some()` is
    /// "travelling" and its value the instant travel ends, when a pill held
    /// lit by its minimum must clear; `None` at rest. The app has no
    /// definition of travelling of its own.
    pub fn travel_left(&self) -> Option<std::time::Duration> {
        travel_left_at(&lock(&self.shared), std::time::Instant::now())
    }

    /// Cached image without scheduling anything (e.g. re-render).
    pub fn peek(&self, index: usize) -> Option<FullImage> {
        lock(&self.shared).cache.get(&index).map(|(i, _)| i.clone())
    }

    /// Supply the app's current VIEW order (`order[pos]` = image id), the
    /// order arrows actually travel. The prefetch ring and the direction
    /// latch follow it from the next `focus()` on — the app calls this
    /// from every view recompute, so a filter or sort change re-keys the
    /// ring the same tick (issue #46: an id-space ring on an interleaved
    /// view prefetched frames no arrow could reach). The POLICY (ring
    /// widths, lean, transit capping) stays in this module; the caller
    /// supplies only the mapping it already owns. Never calling this
    /// keeps identity order — the pre-#46 behavior, exact.
    pub fn set_view(&self, order: &[usize]) {
        let count = self.shared.paths.len();
        let mut state = lock(&self.shared);
        apply_view(&mut state, order, count);
    }

    /// Grid-cell ladder (same 25% rule as focus): ensure every `index` has
    /// an asset serving `display_long`, at lower urgency than the focused
    /// image — used by intermediate zoom levels whose cells outgrow the
    /// 320 px thumb. Does not touch the focused index or prefetch ring.
    pub fn want(&self, indexes: impl IntoIterator<Item = usize>, display_long: u32) {
        let count = self.shared.paths.len();
        let stamp = self.shared.stamp.fetch_add(1, Ordering::Relaxed) + 1;
        let mut state = lock(&self.shared);
        // This call defines the CURRENT visible set: cull all stale grid
        // wants so scrolled-past cells never starve on-screen ones
        // (validator finding — the backlog ran before visible work).
        state.queue.retain(|e| e.focus_origin);
        let mut queued_any = false;
        for i in indexes {
            if i >= count {
                continue;
            }
            queued_any |= schedule(
                &mut state,
                i,
                Target::Long(display_long),
                stamp,
                Origin::Grid,
                RequestState::Settled,
            );
        }
        drop(state);
        if queued_any {
            self.shared.wakeup.notify_all();
        }
    }
}

impl Drop for LoupeEngine {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        drop(lock(&self.shared)); // serialize with check-then-wait (see pipeline)
        self.shared.wakeup.notify_all();
        for w in self.workers.drain(..) {
            w.join().ok();
        }
    }
}

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, LoupeState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The body of [`LoupeEngine::set_fit_box`], pure so the time-to-screen
/// tests drive it clock-free. The box going — the app left the loupe —
/// clears every open time-to-screen measurement (a full-res fill that
/// completes off the loupe is never adopted, so it would never end) and
/// moves the box's epoch on, so a decode running across it opens none
/// either (raw-pipeline.md, "Above fit", rule 1: "a measurement exists only
/// while the engine has a fit box — a decode started without one starts
/// none, and the box going ends every open one unmeasured").
fn apply_fit_box(state: &mut LoupeState, fit_box: Option<FitBox>) {
    let had_a_box = state.fit_box.is_some();
    state.fit_box = fit_box.filter(|b| b.width > 0 && b.height > 0);
    if state.fit_box.is_none() {
        state.full_started.clear();
        if had_a_box {
            state.box_epoch += 1;
        }
    }
}

/// When a decode began, and under which fit box: a full-res frame's
/// time-to-screen runs from `at`, and exists only while the engine has a
/// fit box — so it needs the box in place when the decode began
/// (`box_epoch` is `Some`) and still the same one, never gone in between,
/// when the frame is published (rule 1: "a decode started without one
/// starts none, and the box going ends every open one unmeasured").
#[derive(Debug, Clone, Copy)]
struct DecodeStart {
    at: std::time::Instant,
    /// The box's epoch when the decode began; `None` with no box then.
    box_epoch: Option<u64>,
}

impl DecodeStart {
    /// A decode beginning at `at`, read against the state it begins under.
    fn read(state: &LoupeState, at: std::time::Instant) -> Self {
        DecodeStart {
            at,
            box_epoch: state.fit_box.is_some().then_some(state.box_epoch),
        }
    }
}

/// A full-res frame of `index`, from a decode that began at `start`, has
/// just been published: open its time-to-screen measurement, which the
/// app's report that the frame's fill completed ends ([`adopted`]) — only
/// when the engine had its fit box from the decode's start to now: the
/// switch rule reads the measurement only above fit, off the loupe nothing
/// is adopted, and a time before the box existed is not the loupe's. A
/// newer decode of the same index replaces the stamp. Called under the
/// state lock by `publish`, for every `Full`-kind image, from whichever
/// decode produced it — the plain decode of the full, or a screen-rung
/// attempt the decoder ran at full scale (a lossless, CMYK or YCCK stream),
/// since only the image's kind tells the two apart.
fn note_full_started(state: &mut LoupeState, index: usize, start: DecodeStart) {
    if state.fit_box.is_some() && start.box_epoch == Some(state.box_epoch) {
        state.full_started.insert(index, start.at);
    }
}

/// The body of [`LoupeEngine::note_adopted`]: a full-res fill for `index`
/// completed at `now`. It ends the measurement its decode opened: the
/// time-to-screen becomes `now` minus that decode's start. Whether the ring
/// kept the texture does not matter — `held` false is the ring's own victim,
/// a frame that was ready to draw at that instant all the same (the ring
/// evicting it says where the cursor is, not how long the frame took), and
/// skipping victims would censor the slowest landings, the ones farthest
/// behind the cursor, which rule 1 exists to see (Manager ruling Q-K). Any
/// other kind, and a full-res fill whose measurement already ended — a
/// re-wrap of a cached frame, or one whose fill was culled — measures
/// nothing.
fn adopted(
    state: &mut LoupeState,
    index: usize,
    kind: RungKind,
    held: bool,
    now: std::time::Instant,
) {
    // Deliberately unread: a victim measures like a held texture (above).
    // The app reports the fact; what it means is the engine's (hard rule 5).
    let _ = held;
    if kind != RungKind::Full {
        return;
    }
    if let Some(started) = state.full_started.remove(&index) {
        state.time_to_screen = Some(now.saturating_duration_since(started));
    }
}

/// The body of [`LoupeEngine::note_dropped`]: `index`'s full-res fill was
/// culled, so its decode's measurement ends without a reading.
fn dropped(state: &mut LoupeState, index: usize) {
    state.full_started.remove(&index);
}

/// What a focus asks for before the engine resolves it: the app's display
/// target, or the fit box — resolved against the engine's own box under the
/// same lock that holds it, so a focus never pairs a box with a stale one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusRequest {
    Long(u32),
    Fit,
}

/// The one body behind [`LoupeEngine::focus`] and
/// [`LoupeEngine::focus_fit`], pure over the state and an explicit clock so
/// the unit tests drive it without workers: note the focus, decide TRANSIT
/// vs SETTLED, judge the switch rule's step-up (rule 2) against this
/// focus's ring, plan the ring (`plan_ring`), schedule it — farthest first,
/// the focused index last (the back of the queue, popped first) — re-plan
/// the queued full-res entries above what their positions now ask (an
/// engine with a fit box), cull the queue to the ring in force, and return
/// the cached image of `index`, whatever rung it is.
fn focus_on(
    state: &mut LoupeState,
    index: usize,
    desired: FocusRequest,
    count: usize,
    stamp: u64,
    now: std::time::Instant,
) -> Option<FullImage> {
    let desired = match desired {
        FocusRequest::Long(l) => Target::Long(l),
        FocusRequest::Fit => state
            .fit_box
            .map_or(Target::Long(MID_RUNG_TARGET), Target::Fit),
    };
    note_focus(state, index, desired, now);
    let req = if in_transit(state, now) {
        RequestState::Transit
    } else {
        RequestState::Settled
    };
    if req == RequestState::Settled {
        // A settled focus asks for the settled ring below, so the reserved
        // lane need not ask it again for this frame (raw-pipeline.md, "The
        // settled ring after a hold"; Manager ruling Q-I).
        state.settled_ring_asked = true;
    }
    // The ring is planned in VIEW-POSITION space and mapped back to image
    // ids at request time (issue #46): arrows travel the view. A focused id
    // with no view position (filtered out mid-flight) gets no ring and culls
    // nothing — its neighbours are unknowable, and guessing in id space is
    // the bug #46 replaced — and asks what the plan asks of a focused frame.
    let Some(fpos) = state.pos_of(index) else {
        let alone = plan_ring(&plan_inputs(state, 0, 1, now)).focused;
        schedule(state, index, alone, stamp, Origin::Focus, req);
        return state.cache.get(&index).map(|(img, _)| img.clone());
    };
    let len = state.ring_len(count);
    // RULE 2, STEP UP (raw-pipeline.md, "Above fit"): judged here, before
    // this focus schedules anything, against the ring in force it is about
    // to schedule. Its effect is on the plan below: the members up to the
    // ring's far end ask for the fit box, those beyond it for full-res.
    if let Some(far_end) = step_up_boundary(state, &plan_inputs(state, fpos, len, now), count) {
        state.switch.up = Some(far_end);
        state.switch.down = None;
    }
    let inputs = plan_inputs(state, fpos, len, now);
    let plan = plan_ring(&inputs);
    // Farthest members first, the focused index last (the back of the
    // queue, popped first by a backlog worker): a ring member never
    // outranks the focused frame's own pending work.
    for &(pos, target) in &plan.members {
        if let Some(id) = state.id_at(pos).filter(|id| *id < count) {
            schedule(state, id, target, stamp, Origin::Focus, req);
        }
    }
    schedule(state, index, plan.focused, stamp, Origin::Focus, req);
    // THE RE-PLAN (raw-pipeline.md, "Above fit"): with a fit box, every
    // position the plan asks something of is re-planned whatever the cache
    // holds — a QUEUED full-res entry above its ask becomes that ask, in
    // this focus's request state, or is dropped when the cache already
    // serves it. `schedule` leaves a request the cache serves untouched, so
    // without this a full-res entry an earlier ask queued would still be
    // popped: during a hold above fit, a decode of the frame the cursor is
    // on or has passed (the 2026-08-01 finding); at fit after `Z` to 1:1
    // and back, the settled full-res ring, which the next hold at fit
    // popped (brief 008 step 5, the senior developer's review). During a
    // hold above fit the members ahead that ask for full-res are no-ops
    // here: nothing is queued above the top rung. The targets come from
    // the plan, never a literal box, so the transit mutation moves them
    // too. An engine with no box keeps the behaviour before brief 008.
    if inputs.fit_box.is_some() {
        let asked: Vec<(usize, Target)> = plan
            .members
            .iter()
            .filter_map(|&(pos, target)| state.id_at(pos).map(|id| (id, target)))
            .chain(std::iter::once((index, plan.focused)))
            .collect();
        for (id, target) in asked {
            replan_queued(state, id, target, req);
        }
    }
    // THE CULL (raw-pipeline.md, "Culling"): queued — never in-flight —
    // focus-origin entries outside the ring in force are dropped, so a
    // reversal culls what leaned the wrong way and a stretched gap mid-hold
    // costs at most the decodes in flight. Grid wants keep their own cull.
    let (lo, hi) = (plan.lo, plan.hi);
    let mut queue = std::mem::take(&mut state.queue);
    queue.retain(|e| {
        !e.focus_origin
            || e.index == index
            || state.pos_of(e.index).is_some_and(|p| lo <= p && p <= hi)
    });
    state.queue = queue;
    state.cache.get(&index).map(|(img, _)| img.clone())
}

/// The re-plan for one index the plan asks `t` of: a QUEUED entry whose
/// target is a `Long` above `t` becomes `t` in the request state `req`, or
/// is dropped when the cache already serves `t` — the early return
/// `schedule` takes for a served request would have left it.
fn replan_queued(state: &mut LoupeState, index: usize, t: Target, req: RequestState) {
    let Some(pos) = state.queue.iter().position(|e| e.index == index) else {
        return;
    };
    let entry = state.queue[pos];
    if !matches!(entry.target, Target::Long(_)) || entry.target <= t {
        return;
    }
    if cached_serves(state, index, t) {
        state.queue.remove(pos);
    } else {
        state.queue[pos].target = t;
        state.queue[pos].state = req;
    }
}

/// Everything the ring's plan reads. `plan_inputs` is the ONE place the
/// engine's state becomes these, so the schedule, the cull and the revival
/// gate cannot drift from the plan or from each other.
#[derive(Debug, Clone, Copy)]
struct PlanInputs {
    /// A held key: TRANSIT (`in_transit`).
    transit: bool,
    /// The travel latch (`note_focus`), never re-derived per call.
    forward: bool,
    /// The focused frame's view position.
    fpos: usize,
    /// How many view positions there are.
    len: usize,
    /// What the app asked for: the fit box at fit, a long edge above it.
    desired: Target,
    fit_box: Option<FitBox>,
    /// How far ahead the full-res ring reaches (`fullres_ring_ahead`).
    fullres_ahead: usize,
    /// The switch rule's boundaries, which the hold row reads.
    switch: SwitchState,
}

/// The ring in force around the focused frame and what each of its
/// positions asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RingPlan {
    /// The ring in force, view positions, inclusive, clamped to the view.
    lo: usize,
    hi: usize,
    /// What the focused frame asks for.
    focused: Target,
    /// Every position in `lo..=hi` but the focused one, with what it asks
    /// for, in push order (`ring_order`).
    members: Vec<(usize, Target)>,
}

/// The engine's state as the plan's inputs, for the focused frame at view
/// position `fpos` of `len`.
fn plan_inputs(state: &LoupeState, fpos: usize, len: usize, now: std::time::Instant) -> PlanInputs {
    PlanInputs {
        transit: in_transit(state, now),
        forward: state.travel_forward,
        fpos,
        len,
        desired: state.desired,
        fit_box: state.fit_box,
        fullres_ahead: state.fullres_clamp.unwrap_or(RING_AHEAD),
        switch: state.switch,
    }
}

/// THE ring plan — raw-pipeline.md's request table ("The ring" and "Above
/// fit"), transcribed, with `T` the transit request (`transit_request`,
/// through which EVERY transit cell goes):
///
/// | inputs | window | focused | behind | ahead |
/// |---|---|---|---|---|
/// | no box, transit | leaning(2, 15) | T | T | T |
/// | no box, settled | ±PREFETCH | desired | desired | desired |
/// | box, at fit (desired `Fit`), settled | leaning(2, 15) | desired | desired | desired |
/// | box, at fit, transit | leaning(2, 15) | T | T | T |
/// | box, above fit (desired `Long`), settled | leaning(2, full-res ahead) | desired | desired | desired |
/// | box, above fit, transit (a hold) | leaning(2, full-res ahead) | T | T | desired, or T by the switch rule |
///
/// The positions beyond the full-res clamp are outside the ring in force
/// above fit: they ask for nothing. The folder's ends clamp every window.
/// During a hold above fit a member ahead asks for full-res only where the
/// switch rule lets it (`switch_asks_full`), and for `T` elsewhere.
fn plan_ring(i: &PlanInputs) -> RingPlan {
    let t = transit_request(i.desired, i.fit_box);
    let above_fit = i.fit_box.is_some() && matches!(i.desired, Target::Long(_));
    let window = match i.fit_box {
        None if !i.transit => RingWindow::symmetric(PREFETCH),
        None => RingWindow::leaning(RING_BEHIND, RING_AHEAD, i.forward),
        Some(_) if above_fit => RingWindow::leaning(RING_BEHIND, i.fullres_ahead, i.forward),
        Some(_) => RingWindow::leaning(RING_BEHIND, RING_AHEAD, i.forward),
    };
    let lo = i.fpos.saturating_sub(window.before);
    let hi = i
        .fpos
        .saturating_add(window.after)
        .min(i.len.saturating_sub(1));
    let focused = if i.transit { t } else { i.desired };
    let hold = i.transit && above_fit;
    let members = ring_order(i.fpos, lo, hi, i.forward)
        .into_iter()
        .map(|pos| {
            let ahead = if i.forward {
                pos > i.fpos
            } else {
                pos < i.fpos
            };
            let target =
                if !i.transit || (hold && ahead && switch_asks_full(i.switch, pos, i.forward)) {
                    i.desired
                } else {
                    t
                };
            (pos, target)
        })
        .collect();
    RingPlan {
        lo,
        hi,
        focused,
        members,
    }
}

/// RULE 2, STEP UP ONLY FROM A COMPLETE RING WITH A FREE DECODER
/// (raw-pipeline.md, "Above fit"): at a focus of a hold above fit that has
/// stepped down and is not locked (rule 3), the far end of the ring in force
/// this focus plans (`i`) — beyond which the members ask for full-res again,
/// one boundary a ring's length ahead of the cursor — when
/// - every member ahead of that ring but the farthest (the newest, which a
///   hold keeps renewing) holds its fit-box rung or better, or has its
///   decode in flight: the persona's "the rung ring ahead is complete", read
///   as nothing of it still waiting to start (Manager ruling Q-H) — during a
///   fast hold the members nearest the far end entered a key period or two
///   ago, and a rung decode takes several, so a rule that waited for their
///   rungs to LAND could never step up on a wide viewport;
/// - no ring work waits in the queue: no focus-origin entry inside that
///   ring. Entries outside it are not ring work — the previous focus's
///   entry for the position that just fell three behind is still queued at
///   this moment, since the cull runs after the schedule;
/// - a backlog worker is free: "the decoders are idle" read as spare
///   capacity, since a decode is in flight at nearly every instant of a hold.
///
/// `None`: no step-up at this focus.
fn step_up_boundary(state: &LoupeState, i: &PlanInputs, count: usize) -> Option<usize> {
    let hold_above_fit = i.transit && i.fit_box.is_some() && matches!(i.desired, Target::Long(_));
    if !hold_above_fit
        || i.switch.down.is_none()
        || i.switch.locked
        || state.backlog_busy >= state.backlog_workers
    {
        return None;
    }
    let plan = plan_ring(i);
    let t = transit_request(i.desired, i.fit_box);
    let far_end = if i.forward { plan.hi } else { plan.lo };
    let complete = (plan.lo..=plan.hi)
        .filter(|&pos| pos != far_end && ahead_by(i.fpos, pos, i.forward).is_some())
        .filter_map(|pos| state.id_at(pos).filter(|id| *id < count))
        .all(|id| cached_serves(state, id, t) || state.in_flight.contains(&id));
    let ring_work_waits = state.queue.iter().any(|e| {
        e.focus_origin
            && state
                .pos_of(e.index)
                .is_some_and(|p| plan.lo <= p && p <= plan.hi)
    });
    (complete && !ring_work_waits).then_some(far_end)
}

/// What view position `pos` asks for now, by the ring plan around the
/// focused frame: `None` when there is no focused frame in the view or
/// `pos` lies outside the ring in force — the revival gate.
fn position_asks(
    state: &LoupeState,
    pos: usize,
    count: usize,
    now: std::time::Instant,
) -> Option<Target> {
    let fpos = state.focused.and_then(|f| state.pos_of(f))?;
    let plan = plan_ring(&plan_inputs(state, fpos, state.ring_len(count), now));
    if pos == fpos {
        return Some(plan.focused);
    }
    plan.members
        .iter()
        .find(|(p, _)| *p == pos)
        .map(|(_, target)| *target)
}

/// The windows of the app's two texture rings, the body of
/// [`LoupeEngine::texture_windows`] (pure, so a test can model the app's
/// rings from the very function the app is handed).
fn windows_of(state: &LoupeState) -> TextureWindows {
    let forward = state.travel_forward;
    let full = if state.fit_box.is_some() {
        RingWindow::leaning(
            RING_BEHIND,
            state.fullres_clamp.unwrap_or(RING_AHEAD),
            forward,
        )
    } else {
        RingWindow::symmetric(PREFETCH)
    };
    TextureWindows {
        rung: RingWindow::leaning(RING_BEHIND, RING_AHEAD, forward),
        full,
    }
}

/// Ladder rule: does this asset serve a display of `display_long` pixels?
fn serves(img: &FullImage, display_long: u32) -> bool {
    let asset_long = img.width.max(img.height) as f32;
    asset_long * UPSCALE_THRESHOLD >= display_long as f32
}

/// Does this decoded image satisfy `target`? A `Long` target by the long
/// edge (`serves`), a `Fit` target by the image's own, already ORIENTED size
/// against the box (`serves_box`) — never the IFD's stored, unrotated
/// claim — or, either way, when it already is the best rung this file can
/// provide (`best`, the terminal-rung memo). The one check behind
/// `sufficient_cached`, `cached_serves`, `next_job`'s post-pop check and the
/// ladder's stop test.
fn served_by(img: &FullImage, target: Target, best: Option<u32>) -> bool {
    let reached = match target {
        Target::Long(l) => serves(img, l),
        Target::Fit(b) => serves_box(img.width, img.height, b),
    };
    reached || best.is_some_and(|b| img.width.max(img.height) >= b)
}

/// Cached-and-sufficient check (refreshing the LRU stamp): an asset counts
/// as sufficient when it serves the target OR it already is the best rung
/// this file can provide (terminal-rung memo).
fn sufficient_cached(state: &mut LoupeState, index: usize, target: Target, stamp: u64) -> bool {
    let best = state.best_long.get(&index).copied();
    if let Some((img, s)) = state.cache.get_mut(&index) {
        *s = stamp;
        served_by(img, target, best)
    } else {
        false
    }
}

/// `sufficient_cached` without the LRU write — for callers that are only
/// ASKING, not using the frame.
///
/// `sufficient_cached` refreshes the stamp because its callers (`focus`,
/// `want`) are declaring live interest. The settle guarantee is not: it
/// polls on a timer, and stamping there would mark the frame the user just
/// settled on as the OLDEST in the cache, making it the first eviction
/// victim the moment they arrow away — exactly backwards for arrowing back
/// to compare two frames of a burst.
fn cached_serves(state: &LoupeState, index: usize, target: Target) -> bool {
    let best = state.best_long.get(&index).copied();
    state
        .cache
        .get(&index)
        .is_some_and(|(img, _)| served_by(img, target, best))
}

/// Land-time revival of a deferred upgrade (an in-flight index whose wanted
/// rung grew mid-decode). Revived ONLY while the index is inside the RING IN
/// FORCE (raw-pipeline.md, "Revival"), the very ring the focus scheduled —
/// a gate still at ±`PREFETCH` would drop most of a 15-ahead ring's
/// upgrades (the trap raw-pipeline.md recorded) — and at no more than what
/// its position there asks for now, so during a hold above fit a frame the
/// cursor has reached or passed revives at the fit box, never at full-res.
/// A stale upgrade — the cursor moved on while the flight decoded —
/// re-queued at top priority once captured BOTH workers for multi-second
/// full-res decodes and starved the current frame's ladder (Windows CI
/// 2026-07-27: three screenshot tests hit the 60 s shutter cap exactly this
/// way). Dropping a stale upgrade loses nothing: focus() re-requests it the
/// moment the user returns. The focused index re-queues at the back (popped
/// next); a ring neighbor goes to the front so it can never outrank the
/// focused frame's own pending work. The revived entry carries `req`, the
/// request state stored beside the deferred target — never the mode at
/// revival (raw-pipeline.md, "The request state travels with the decode").
fn revive_deferred(
    state: &mut LoupeState,
    index: usize,
    target: Target,
    req: RequestState,
    stamp: u64,
    count: usize,
    now: std::time::Instant,
) -> bool {
    // Ring membership in VIEW positions (issue #46), like the ring itself:
    // an id 2 away can be a view-order stranger, and a view neighbor can
    // be any id at all. No position (filtered out) = not in the ring.
    let Some(asks) = state
        .pos_of(index)
        .and_then(|pos| position_asks(state, pos, count, now))
    else {
        return false;
    };
    let target = target.min(asks);
    if state.failed.contains(&index) || sufficient_cached(state, index, target, stamp) {
        return false;
    }
    state.queue.retain(|e| e.index != index);
    let entry = Entry {
        index,
        target,
        focus_origin: true,
        state: req,
    };
    if state.focused == Some(index) {
        state.queue.push(entry);
    } else {
        state.queue.insert(0, entry);
    }
    true
}

/// Where a scheduled request goes in the queue — the ONE thing
/// `focus()` and `want()` disagree about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Origin {
    /// Focus and its prefetch ring: the BACK of the queue (workers pop
    /// from the back, so this is served first), REPLACING any pending
    /// entry for the same index — the latest intent wins. Marked
    /// focus-origin, so the grid-want cull never drops it.
    Focus,
    /// Grid want (intermediate zoom cells): the FRONT of the queue, so
    /// focused work stays ahead of it, and it YIELDS to an entry the
    /// focus path already placed rather than displacing it.
    Grid,
}

/// Schedule `index` at `target`, or record why it needs no work.
///
/// The rule was spelled out twice — once in `focus`, once in `want` —
/// including the deferred-upgrade merge, which has a recorded QE defect
/// of its own (a dropped upgrade meant 1:1 never arrived without the
/// app's refresh loop, see `LoupeState::deferred`). It is now single-site:
/// sufficient-or-failed does nothing, in-flight merges the target into
/// the deferred map with `max` (a smaller later request must never undo a
/// bigger one), and anything else is queued at the origin's polarity.
///
/// Returns true only when an entry was actually queued — `want` wakes
/// workers on that, `focus` wakes them unconditionally.
fn schedule(
    state: &mut LoupeState,
    index: usize,
    target: Target,
    stamp: u64,
    origin: Origin,
    req: RequestState,
) -> bool {
    if sufficient_cached(state, index, target, stamp) || state.failed.contains(&index) {
        return false;
    }
    if state.in_flight.contains(&index) {
        // The request state changes only when the target GROWS: an equal
        // or smaller request later in a hold must not relabel the decode a
        // bigger one deferred (raw-pipeline.md, "The request state travels
        // with the decode").
        let e = state.deferred.entry(index).or_insert((target, req));
        if target > e.0 {
            *e = (target, req);
        }
        return false;
    }
    match origin {
        Origin::Focus => {
            state.queue.retain(|e| e.index != index);
            state.queue.push(Entry {
                index,
                target,
                focus_origin: true,
                state: req,
            });
        }
        Origin::Grid => {
            if state.queue.iter().any(|e| e.index == index) {
                return false; // already scheduled by focus/prefetch
            }
            // Front of the vec = popped last: focused work stays first.
            state.queue.insert(
                0,
                Entry {
                    index,
                    target,
                    focus_origin: false,
                    state: req,
                },
            );
        }
    }
    true
}

/// Track the focus for the reserved worker's debounce (see
/// FOCUS_DEBOUNCE). The clock resets when the focused INDEX changes and
/// ALSO when the focused index's target ESCALATES: the cursor rests on
/// the load frame long enough to pass the debounce, and when the 1:1
/// pin then queues that frame's full-res climb, a stable-focus-only
/// clock made the reserved worker instantly eligible — it raced the
/// backlog workers for the entry and, on winning, was captured for a
/// multi-second climb moments before the cursor left (QE defect: ~20%
/// capture rate in the CI shape, restoring the starvation). New big
/// work must survive the debounce regardless of how long the focus has
/// rested. A same-or-smaller target (render-cadence re-focus, zoom out)
/// never resets.
fn note_focus(state: &mut LoupeState, index: usize, desired: Target, now: std::time::Instant) {
    // The app's real intent, before any transit capping, so the settle
    // knows what to climb to.
    state.desired = desired;
    if state.focused != Some(index) {
        // A change hard on the heels of the previous one is a held key.
        let moving = state
            .last_index_change
            .is_some_and(|t| now.saturating_duration_since(t) <= TRANSIT_GAP);
        // THE KEY PERIOD (raw-pipeline.md, "Above fit", rule 1): the
        // interval between the last two index changes — read here, before
        // this change becomes the last one. Never from the last focus CALL:
        // the app re-focuses the same index on every refresh.
        state.key_period = state
            .last_index_change
            .map(|t| now.saturating_duration_since(t));
        state.moving = moving;
        state.last_index_change = Some(now);
        // Latch direction HERE, where a real change proves it — in VIEW
        // positions (issue #46): arrows travel the view, and comparing
        // ids on an interleaved view read a steady forward hold as
        // jumping around, flapping the ring's lean. No previous focus,
        // or one no longer in the view: forward (matching the pre-#46
        // first-focus default).
        let new_pos = state.pos_of(index);
        let prev_pos = state.focused.and_then(|p| state.pos_of(p));
        let forward = match (new_pos, prev_pos) {
            (Some(n), Some(p)) => n >= p,
            _ => true,
        };
        // THE HOLD'S END (raw-pipeline.md, "Above fit", rule 3's "until it
        // ends"): the switch rule starts afresh at an index change that is
        // not part of a hold — a stop, or keys slower than four a second
        // (`TRANSIT_GAP`) — and at a reversal. Nothing else resets it, and
        // in particular not the end of `in_transit`: that decays after
        // `SETTLE_DEBOUNCE` (150 ms), so keys 150–250 ms apart are a hold
        // with a settled window in every gap, and rule 3's lock must carry
        // across them.
        if !moving || forward != state.travel_forward {
            state.switch = SwitchState::default();
        }
        state.travel_forward = forward;
        // A new frame: nothing has asked for its settled ring yet (Q-I).
        state.settled_ring_asked = false;
    }
    if state.focused != Some(index) {
        state.focused = Some(index);
        state.focused_at = Some(now);
        state.focused_target = desired;
    } else if desired > state.focused_target {
        // By the `Target` order: fit box → the top rung re-arms (a Z at
        // fit); back down never does.
        state.focused_at = Some(now);
        state.focused_target = desired;
    }
}

/// What a worker should do next.
#[derive(Debug, PartialEq)]
enum Slot {
    /// Decode this index to this target, carrying the request state it was
    /// queued with.
    Job(usize, Target, RequestState),
    /// Nothing for this worker: wait for a queue notification.
    Wait,
    /// The reserved worker's debounce hasn't elapsed: wait at most this
    /// long (a timed wait — nothing will notify when time passes).
    WaitFor(std::time::Duration),
}

/// A fresh focus must HOLD this long before the reserved worker commits
/// to it. Without the debounce the reservation is capture-bait: the
/// cursor legitimately rests on frame 0 during startup and touches every
/// transit frame for ~60-150 ms, and any of those would bind the
/// reserved lane to a multi-second climb of a frame the user already
/// left (validator FAIL on the debounce-less version: all three workers
/// provably committed before the cursor settled). Normal workers have no
/// debounce, so with idle capacity a fresh focus still starts instantly
/// — the ~300 ms sharpness-on-stop contract only meets this delay when
/// every backlog worker is saturated, exactly when the lane matters.
const FOCUS_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(250);

/// Two successive frame changes closer together than this are a HELD key,
/// not deliberate taps. Measured against real hands: key autorepeat lands
/// around 120 ms, while tap-stepping through a burst to compare frames is
/// 350 ms to 2 s apart (persona, 2026-08-01). The two populations are far
/// apart, so the exact value is not delicate.
const TRANSIT_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// Quiet for this long after the last frame change means the user has
/// STOPPED, and quality becomes the goal again — this is what `in_transit`
/// decays on.
///
/// It is NOT, however, what the user feels. The settle guarantee runs in
/// the reserved lane, which `FOCUS_DEBOUNCE` (250 ms) gates first, and
/// `note_focus` sets `focused_at` and `last_index_change` from the same
/// index change — so the lane cannot act before 250 ms and its `settled`
/// check is always true by the time it is reached. QE measured ~215 ms of
/// overhead over a bare decode and confirmed, by poking a focus in at
/// T+150 ms and getting a FASTER result, that the engine had not yet acted.
/// The check stays as an explicit statement of intent, not as live logic.
///
/// An earlier version of this comment claimed 250 ms here would "stack
/// with the reserved lane's own debounce into most of a second". That is
/// wrong: both debounces are measured from the same origin, so they do not
/// add. The real floor is 250 ms, not ~500 ms.
const SETTLE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);

/// What a moving frame asks the decoder for: the fit box when the engine
/// has one — at fit and above it alike (raw-pipeline.md, "The ring": during
/// a hold the focused frame asks for the fit box, never full-res) — and,
/// with no box, the mid (raw-pipeline.md, "An engine with no fit box").
/// EVERY transit request the engine makes comes from this one function, so
/// one change to it moves them all.
///
/// With no box: `MID_RUNG_TARGET`, not `MID_RUNG_MAX_LONG`: the latter
/// (2048) is the ceiling of what COUNTS as mid class, but `serves` allows
/// only a 1.25x upscale, so a 1616 mid covers 2020 px — 28 short of 2048.
/// Asking for 2048 quietly sent every transit frame up to full-res anyway,
/// and the whole change measured as no improvement at all until the
/// arithmetic was checked. `transit_request_is_the_fit_box_and_never_the_full`
/// pins both halves, and the box.
fn transit_request(desired: Target, fit_box: Option<FitBox>) -> Target {
    match fit_box {
        Some(b) => Target::Fit(b),
        None => Target::Long(desired.long().min(MID_RUNG_TARGET)),
    }
}

/// Is the user MOVING between frames (held key, `[`/`]`, a Y/N
/// auto-advance chain) rather than looking at one?
///
/// While true the focused frame asks for no more than the fit box, however
/// far above fit the view is — the mid on an engine with no box (user
/// requirement 2026-08-01: "while I'm holding a key I don't need the image
/// to be as good as possible, I need it to move fast; when I release the
/// key, then I want quality to be high") — and the ring asks by the
/// request table (`plan_ring`).
///
/// This governs what is REQUESTED, never what is DISPLAYED. The renderer
/// always shows the best rung in cache, so flying back over frames whose
/// full-res is still resident shows them sharp — a rule that rendered the
/// mid with a sharp texture in hand would be worse than the bug it fixes
/// (persona).
fn in_transit(state: &LoupeState, now: std::time::Instant) -> bool {
    state.moving
        && state
            .last_index_change
            .is_some_and(|t| now.saturating_duration_since(t) < SETTLE_DEBOUNCE)
}

/// The body of [`LoupeEngine::travel_left`], pure over the state and an
/// explicit clock so it is tested without workers.
fn travel_left_at(state: &LoupeState, now: std::time::Instant) -> Option<std::time::Duration> {
    if !in_transit(state, now) {
        return None;
    }
    let since = now.saturating_duration_since(state.last_index_change?);
    Some(SETTLE_DEBOUNCE.saturating_sub(since))
}

/// Pick this worker's next job off the queue.
/// A focus-reserved worker takes ONLY the focused index's entry, and
/// only once the focus has been stable for FOCUS_DEBOUNCE: in-flight
/// decodes cannot be preempted, so a debounced reservation is the only
/// way the SETTLED frame's ladder is guaranteed to start promptly when
/// the backlog workers are already committed to multi-second climbs of
/// frames the cursor legitimately rested on moments ago (Windows CI
/// 2026-07-27, second starvation shape: every worker was captured
/// before the cursor settled, and the settled frame's full-res landed
/// past the screenshot shutter's 60 s cap).
/// Normal workers pop from the back (most urgent last), and the switch
/// rule's step-down is decided at that pop, before the decode starts
/// (`step_down_at_the_decode`). `stamp` and `count` are the engine's current
/// focus stamp and folder length, for the settled ring the reserved lane
/// may ask (`ask_the_settled_ring`), as `revive_deferred` takes them.
fn next_job(
    state: &mut LoupeState,
    focus_reserved: bool,
    stamp: u64,
    count: usize,
    now: std::time::Instant,
) -> Slot {
    loop {
        let pos = if focus_reserved {
            let Some(f) = state.focused else {
                return Slot::Wait;
            };
            let held = state
                .focused_at
                .map(|t| now.saturating_duration_since(t))
                .unwrap_or(FOCUS_DEBOUNCE);
            if held < FOCUS_DEBOUNCE {
                return Slot::WaitFor(FOCUS_DEBOUNCE - held);
            }
            match state.queue.iter().rposition(|e| e.index == f) {
                Some(pos) => pos,
                None => {
                    // SETTLE GUARANTEE. Transit deliberately asked for no
                    // more than the fit box (the mid, with no box), so once
                    // the user stops, SOMETHING has to ask for the real
                    // target — and it cannot be the app, whose
                    // refresh loop goes quiet exactly when nothing is
                    // decoding. This lane already wakes on a timer, so it is
                    // the one place that can promise it: settled, focused
                    // frame short of what the app wants, nothing queued for
                    // it -> queue it here.
                    let settled = state
                        .last_index_change
                        .is_some_and(|t| now.saturating_duration_since(t) >= SETTLE_DEBOUNCE);
                    let want = state.desired;
                    if settled
                        && want.long() > 0
                        && !state.failed.contains(&f)
                        && !state.in_flight.contains(&f)
                        && !cached_serves(state, f, want)
                    {
                        state.queue.push(Entry {
                            index: f,
                            target: want,
                            focus_origin: true,
                            state: RequestState::Settled,
                        });
                        state.queue.len() - 1
                    } else {
                        // THE SETTLED RING AFTER A HOLD (raw-pipeline.md;
                        // Manager ruling Q-I): the frame needed nothing, so
                        // nothing will land to refresh the app, whose next
                        // focus would have asked for the settled ring — the
                        // lane asks for it here, once per settle. Never
                        // together with the climb above: that climb's landing
                        // refreshes the app, whose settled focus asks it —
                        // SETTLED, then SETTLED-AND-IDLE (ui-grid.md).
                        if settled && settled_ring_due(state, f) {
                            ask_the_settled_ring(state, f, stamp, count, now);
                        }
                        if settled && idle_cook_due(state, f) {
                            // THE IDLE COOK (raw-pipeline.md, "The idle cook";
                            // brief 008 R9): the cursor's full-res behind a
                            // stop at fit on a wide viewport, queued and
                            // popped in this same call, so `Z` after the stop
                            // finds it cooked or cooking. It never re-arms the
                            // escalation clock and never passes through
                            // `revive_deferred`.
                            state.queue.push(Entry {
                                index: f,
                                target: Target::Long(u32::MAX),
                                focus_origin: true,
                                state: RequestState::Settled,
                            });
                            state.queue.len() - 1
                        } else {
                            return Slot::Wait;
                        }
                    }
                }
            }
        } else {
            match state.queue.len().checked_sub(1) {
                Some(pos) => pos,
                None => return Slot::Wait,
            }
        };
        let mut entry = state.queue.remove(pos);
        if !focus_reserved {
            step_down_at_the_decode(state, &mut entry, now);
        }
        if cached_serves(state, entry.index, entry.target) {
            continue; // upgraded or topped out meanwhile
        }
        state.in_flight.push(entry.index);
        return Slot::Job(entry.index, entry.target, entry.state);
    }
}

/// Is the idle cook due for the focused frame `f` (raw-pipeline.md, "The
/// idle cook")? Each clause, and what it keeps out:
/// - a fit box and the app's request IS that box — at fit, on the CURRENT
///   box: a stale box after leaving the loupe cooks nothing;
/// - the reference mid does not serve the box (`mid_serves_box`) — the
///   VIEWPORT is wide: 3/8 rungs left in the LRU by a shrink to a 1080p box
///   would otherwise cook for ever there;
/// - the cursor's cached image is a SCREEN rung — with only a mid, the
///   settle guarantee climbs first;
/// - the file's best is not already in hand, written as the served check
///   itself (`served_by` at the top rung, with the memo), so the lane cannot
///   manufacture a job the post-pop check then discards: that re-pushes it
///   on the next loop turn, for ever, under the state lock;
/// - nothing in flight, failed or queued for it.
fn idle_cook_due(state: &LoupeState, f: usize) -> bool {
    state
        .fit_box
        .is_some_and(|b| !mid_serves_box(b) && state.desired == Target::Fit(b))
        && state.cache.get(&f).is_some_and(|(img, _)| {
            img.kind == RungKind::Screen
                && !served_by(
                    img,
                    Target::Long(u32::MAX),
                    state.best_long.get(&f).copied(),
                )
        })
        && !state.in_flight.contains(&f)
        && !state.failed.contains(&f)
        && !state.queue.iter().any(|e| e.index == f)
}

/// Is the settled ring due from the reserved lane for the focused frame `f`
/// (raw-pipeline.md, "The settled ring after a hold"; Manager ruling Q-I)?
/// An engine with a fit box — one without keeps the behaviour before brief
/// 008 — whose focused frame's real target is already in hand, so the
/// settle guarantee has nothing to climb and nothing will land to refresh
/// the app; and the ring not yet asked since the frame was reached, by a
/// settled focus or by the lane itself (`settled_ring_asked`).
fn settled_ring_due(state: &LoupeState, f: usize) -> bool {
    state.fit_box.is_some() && !state.settled_ring_asked && cached_serves(state, f, state.desired)
}

/// The reserved lane asks for the settled ring around the focused frame `f`
/// itself: the members of `plan_ring`'s plan — settled by now, so its
/// settled row — scheduled in push order exactly as a focus schedules them,
/// never a second copy of the request table; once per settle
/// (`settled_ring_asked`). The lane holds the state lock, so it cannot wake
/// the backlog workers that will decode what it queued: it says so in
/// `wake_backlog`, which the worker loop takes.
fn ask_the_settled_ring(
    state: &mut LoupeState,
    f: usize,
    stamp: u64,
    count: usize,
    now: std::time::Instant,
) {
    state.settled_ring_asked = true;
    let Some(fpos) = state.pos_of(f) else {
        return; // no view position: no ring to ask (as `focus_on`)
    };
    let plan = plan_ring(&plan_inputs(state, fpos, state.ring_len(count), now));
    let mut queued = false;
    for &(pos, target) in &plan.members {
        if let Some(id) = state.id_at(pos).filter(|id| *id < count) {
            queued |= schedule(
                state,
                id,
                target,
                stamp,
                Origin::Focus,
                RequestState::Settled,
            );
        }
    }
    state.wake_backlog |= queued;
}

/// RULE 1, STEP DOWN ONCE, AT THE DECODE (raw-pipeline.md, "Above fit"): a
/// backlog worker has just popped `entry` and is about to start it. During
/// a hold above fit, when the entry asks full-res of a member AHEAD of the
/// cursor, the engine compares the time the cursor needs to reach that
/// member — its distance ahead in view positions, counted from 1, × the
/// key period — with the time-to-screen. When the cursor would arrive
/// first, that member and every member beyond it ask for the fit box: one
/// boundary, set here, before any of their full-res decodes starts, so the
/// frames the cursor meets step from full-res to the fit-box rung once. The
/// full-res entries still queued at or beyond it become fit-box entries in
/// the transit state, or are dropped when that rung is in hand; the popped
/// entry becomes one too (and is skipped when served); a decode already in
/// flight lands. A step-down less than one ring past the last step-up's
/// boundary locks the hold on the fit box (rule 3). With the time-to-screen
/// or the key period still unknown the member asks for full-res. An entry at
/// or beyond a boundary already in force is re-targeted without the timing
/// test — defensive: the focus's re-plan and the step-down's own conversion
/// should leave none.
fn step_down_at_the_decode(state: &mut LoupeState, entry: &mut Entry, now: std::time::Instant) {
    if state.fit_box.is_none()
        || !matches!(state.desired, Target::Long(_))
        || !in_transit(state, now)
    {
        return;
    }
    let t = transit_request(state.desired, state.fit_box);
    if !entry.focus_origin || !matches!(entry.target, Target::Long(_)) || entry.target <= t {
        return;
    }
    let forward = state.travel_forward;
    let (Some(fpos), Some(pos)) = (
        state.focused.and_then(|f| state.pos_of(f)),
        state.pos_of(entry.index),
    ) else {
        return;
    };
    let Some(distance) = ahead_by(fpos, pos, forward) else {
        return; // not a member ahead: the hold's re-plan owns those
    };
    let past_the_boundary = state
        .switch
        .down
        .is_some_and(|down| at_or_beyond(pos, down, forward));
    if !past_the_boundary {
        let steps = u32::try_from(distance).unwrap_or(u32::MAX);
        let cursor_first = match (state.key_period, state.time_to_screen) {
            (Some(key_period), Some(to_screen)) => key_period.saturating_mul(steps) < to_screen,
            _ => false,
        };
        if !cursor_first {
            return;
        }
        // The boundary: this member, nearer than any boundary in force.
        state.switch.down = Some(pos);
        // RULE 3: less than one ring beyond the last step-up's boundary —
        // the first position beyond its far end — locks the hold.
        if state.switch.up.is_some_and(|far_end| {
            ahead_by(far_end, pos, forward).is_none_or(|beyond| beyond <= RING_AHEAD)
        }) {
            state.switch.locked = true;
        }
        let at_or_past: Vec<usize> = state
            .queue
            .iter()
            .filter(|e| {
                e.focus_origin
                    && state
                        .pos_of(e.index)
                        .is_some_and(|p| at_or_beyond(p, pos, forward))
            })
            .map(|e| e.index)
            .collect();
        for index in at_or_past {
            replan_queued(state, index, t, RequestState::Transit);
        }
    }
    entry.target = t;
    entry.state = RequestState::Transit;
}

fn worker(shared: &Shared, focus_reserved: bool) {
    let count = shared.paths.len();
    loop {
        let (index, target, req) = {
            let mut state = lock(shared);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                let stamp = shared.stamp.load(Ordering::Relaxed);
                let now = std::time::Instant::now();
                let slot = next_job(&mut state, focus_reserved, stamp, count, now);
                // The reserved lane queued the settled ring under the lock it
                // still holds: wake the backlog workers to decode it. (The
                // lane itself is not waiting, so it never wakes itself.)
                if std::mem::take(&mut state.wake_backlog) {
                    shared.wakeup.notify_all();
                }
                match slot {
                    Slot::Job(index, target, req) => {
                        // Rule 2's "a backlog worker is free" counts these.
                        if !focus_reserved {
                            state.backlog_busy += 1;
                        }
                        break (index, target, req);
                    }
                    Slot::Wait => {
                        state = shared
                            .wakeup
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    Slot::WaitFor(d) => {
                        state = shared
                            .wakeup
                            .wait_timeout(state, d)
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .0;
                    }
                }
            }
        };

        // Climb the ladder: cheapest sufficient rung first (mid preview
        // ~5 ms), then the full-res rung (~140 ms) only if the display
        // needs it. Each rung is published as its own Ready so the UI
        // swaps quality in place without ever blocking.
        let current_long = {
            let state = lock(shared);
            state
                .cache
                .get(&index)
                .map(|(img, _)| img.width.max(img.height))
                .unwrap_or(0)
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_ladder(shared, index, target, current_long, focus_reserved, req)
        }))
        .unwrap_or_else(|_| Err("internal error (panic) decoding image".into()));

        let mut state = lock(shared);
        if !focus_reserved {
            state.backlog_busy = state.backlog_busy.saturating_sub(1);
        }
        state.in_flight.retain(|i| *i != index);
        // Record failure BEFORE draining deferred upgrades: the old order
        // re-queued a doomed index and emitted a duplicate Failed
        // (validator + QE finding, 300/300 repro).
        let failure = outcome.err();
        if failure.is_some() {
            state.failed.insert(index);
        }
        if let Some((target, req)) = state.deferred.remove(&index) {
            let stamp = shared.stamp.load(Ordering::Relaxed);
            let now = std::time::Instant::now();
            if revive_deferred(&mut state, index, target, req, stamp, count, now) {
                shared.wakeup.notify_all();
            }
        }
        drop(state);
        if let Some(reason) = failure {
            shared
                .events
                .send(LoupeEvent::Failed { index, reason })
                .ok();
        }
    }
}

/// Decode rungs for `index` until one serves `target`, publishing each
/// improvement over `current_long` to the cache + event channel, every
/// `Ready` carrying `req`, the request state the decode was queued with.
/// `reserved_lane`: this flight runs on the focus-reserved worker, which
/// exists ONLY to serve the focused frame — at every rung boundary it
/// re-checks that its index is still THE focus and abandons otherwise
/// (no note_best: the ladder didn't top out; the backlog workers own
/// the frame from then on, and focus() re-requests on return). This
/// closes the double-settle residual the reservation had accepted: a
/// stall-stretched transient hold can pass the debounce and commit the
/// lane to a frame the user leaves moments later — on the Windows CI
/// release-commit run, bunched drive timers held frame 3 for ~2 s, the
/// lane spent a ~30 s debug climb on it, and the settled frame 4
/// missed the shutter's 60 s cap. Backlog workers never abandon:
/// their in-flight neighbors are legitimate prefetch.
///
/// At fit (`target` is `Fit(box)`) the full JPEG is first decoded at the
/// screen rung's scale, `rung_factor`'s N/8 (raw-pipeline.md, "The screen
/// rung"), PLANNED from the size the stream declares in its own SOF — the
/// size the decoder scales — never from the IFD's claim, which
/// `find_embedded_jpegs` trusts for sizing a candidate and which a file can
/// over- or under-state (QE 2026-09-28, D2: planned from an over-claim, the
/// rung came back short of the box and the full followed — two decodes, and a
/// 149 MB full-res texture at fit, for every frame of the ring). Each rung's
/// bytes are read once and serve both of its decodes. What follows is read
/// off the KIND of the image that decode returned — the scale the decoder
/// ran — never off the IFD's size claim:
/// - it failed: as any failed rung — a good lower rung stays, memoized,
///   with no Failed; nothing does, and the image fails. No second attempt
///   at full scale: the same bytes would fail the same way;
/// - it is a `Full` (the decoder ran 8/8 — a lossless, CMYK or YCCK stream):
///   it IS the full rung, published as the plain decode would publish it,
///   which then does not run (it would decode the same JPEG twice);
/// - it is a `Screen` rung larger than what is in hand: published, never
///   terminal; the ladder stops if it serves, and otherwise falls through
///   to the plain decode;
/// - it is a `Screen` rung no larger: nothing published; falls through.
///
/// Planned from the stream, a rung serves by construction — `rung_factor`
/// picks it by the decoder's own arithmetic (`scaled_dims`) — so the
/// fall-through, and a `Full` that misses the box, are defence: should a
/// plan ever miss its stream, the ladder still converges on the full, never
/// on a screen rung it would have to memoize as the file's best.
///
/// A fall-through is a rung boundary like any other: the reserved lane
/// checks its focus again before the plain decode.
///
/// After EVERY publish the stop test reads the decoded, ORIENTED image,
/// never the IFD's stored, unrotated size: a portrait mid on a QHD box
/// serves oriented and fails unrotated.
fn decode_ladder(
    shared: &Shared,
    index: usize,
    target: Target,
    current_long: u32,
    reserved_lane: bool,
    req: RequestState,
) -> Result<(), String> {
    let path = &shared.paths[index];
    let mut file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let previews = find_embedded_jpegs(&mut file).map_err(|e| format!("parse: {e}"))?;

    let orientation = previews.orientation;
    // The top rung is the largest embedded JPEG whole OR CUT: a RAW cut
    // inside its full keeps that full as its best, so the mid below it is
    // never `terminal` — soft and cued where it does not serve — and the
    // full's read names the cut ("truncated") on the stderr line of a rung
    // that fails over a good lower one (raw-pipeline.md, "Hostile-input
    // bounds"; QE 2026-09-28, D1).
    let full = previews.loupe_top().cloned();
    let mut rungs: Vec<crate::raw::EmbeddedJpeg> = Vec::new();
    if let Some(mid) = previews.grid_source() {
        rungs.push(mid.clone());
    }
    if let Some(full) = &full {
        if rungs.last() != Some(full) {
            rungs.push(full.clone());
        }
    }
    if rungs.is_empty() {
        return Err(crate::raw::NO_USABLE_PREVIEW.into());
    }

    let top_long = rungs
        .last()
        .map(|r| r.width.max(r.height))
        .unwrap_or_default();
    // What is in hand, as the long edge actually DECODED — never a rung's
    // IFD claim, which a file can over-state: the memo below is set from
    // this, and a claim the stream never reaches left the cached image short
    // of its own memo for good, so the settle guarantee re-decoded the file
    // at every settle while the cursor rested on it (raw-pipeline.md, "The
    // screen rung"; the step-2 review, Manager ruling 2026-09-27, M11).
    let mut achieved = current_long;
    for rung in &rungs {
        let rung_long = rung.width.max(rung.height);
        if rung_long <= achieved {
            continue; // already have this rung or better
        }
        if lane_abandons(shared, index, reserved_lane, achieved) {
            return Ok(());
        }
        // The file's largest embedded JPEG is the full; a bare JPEG's one
        // candidate is both its grid source and its full.
        let candidate = if full.as_ref() == Some(rung) {
            RungKind::Full
        } else {
            RungKind::Mid
        };
        // The decode's start: a full-res frame's time-to-screen runs from
        // here — the read included — when the decoder ran full scale.
        let started = DecodeStart::read(&lock(shared), std::time::Instant::now());
        let bytes = match read_jpeg(&mut file, rung) {
            Ok(bytes) => bytes,
            Err(e) => {
                return keep_lower_rung(shared, index, achieved, candidate, format!("read: {e}"))
            }
        };
        // THE SCREEN RUNG: at fit, the full's N/8 decode first, planned from
        // the stream's own size.
        let mut screen_decoded = false;
        if let (RungKind::Full, Target::Fit(fit_box)) = (candidate, target) {
            let plan = planned_dims(&bytes)
                .and_then(|(w, h)| rung_factor(w, h, orientation, fit_box).map(|n| (w, h, n)));
            if let Some((width, height, n)) = plan {
                let (sw, sh) = scaled_dims(width, height, n);
                if sw.max(sh) > achieved {
                    screen_decoded = true;
                    match decode_rung(&bytes, orientation, n, candidate) {
                        Err(reason) => {
                            return keep_lower_rung(
                                shared,
                                index,
                                achieved,
                                RungKind::Screen,
                                reason,
                            )
                        }
                        Ok((image, note)) if image.kind == RungKind::Full => {
                            let serves = served_by(&image, target, None);
                            let long = image.width.max(image.height);
                            report_complaint(shared, index, rung, image.kind, note);
                            publish(shared, index, image, rung_long >= top_long, req, started);
                            achieved = long;
                            if serves {
                                return Ok(());
                            }
                            continue;
                        }
                        Ok((image, note)) => {
                            let long = image.width.max(image.height);
                            if long > achieved {
                                let serves = served_by(&image, target, None);
                                report_complaint(shared, index, rung, image.kind, note);
                                publish(shared, index, image, false, req, started);
                                achieved = long;
                                if serves {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }
        // A screen rung that did not serve falls through to the plain decode
        // of the same full, from the same bytes: a second decode in one
        // flight, so the reserved lane checks its focus again here, as between
        // any two rungs (raw-pipeline.md, "The loupe ladder": "The lane checks
        // only BETWEEN rungs, so a focus change during a rung's decode waits
        // out that rung — one decode"; the step-2 review's F3). Planned from
        // the stream a rung always serves (above); this is the defence for a
        // plan that misses it — before QE round 1's D2, every IFD that
        // over-claimed its stream.
        let started = if screen_decoded {
            #[cfg(test)]
            if let Some(hook) = AFTER_SCREEN_DECODE.with(std::cell::Cell::get) {
                hook(shared);
            }
            if lane_abandons(shared, index, reserved_lane, achieved) {
                return Ok(());
            }
            DecodeStart::read(&lock(shared), std::time::Instant::now())
        } else {
            started
        };
        match decode_rung(&bytes, orientation, 8, candidate) {
            Ok((image, note)) => {
                let serves = served_by(&image, target, None);
                let long = image.width.max(image.height);
                report_complaint(shared, index, rung, image.kind, note);
                publish(shared, index, image, rung_long >= top_long, req, started);
                achieved = long;
                if serves {
                    return Ok(());
                }
            }
            Err(reason) => return keep_lower_rung(shared, index, achieved, candidate, reason),
        }
    }
    // Ladder topped out below the display target: memoize the terminal rung
    // — the long edge DECODED, see `achieved` — so this file is never
    // re-parsed for an unreachable target.
    if achieved > 0 {
        note_best(shared, index, achieved);
        Ok(())
    } else {
        Err(crate::raw::NO_DECODABLE_PREVIEW.into())
    }
}

/// The reserved lane exists ONLY to serve the focused frame: at every rung
/// boundary its flight re-checks that its index is still THE focus, and
/// abandons when it is not — no `note_best`, since the ladder did not top
/// out; the backlog workers own the frame from then on, and `focus()`
/// re-requests it on return. Logged, so the next stall-shaped CI failure is
/// diagnosable in one read (validator finding: silent abandons force timing
/// inference). A backlog flight never abandons: its in-flight neighbours are
/// legitimate prefetch.
fn lane_abandons(shared: &Shared, index: usize, reserved_lane: bool, achieved: u32) -> bool {
    if reserved_lane && lock(shared).focused != Some(index) {
        eprintln!("fastcull: loupe lane abandoned idx {index} at {achieved} (focus moved)");
        return true;
    }
    false
}

#[cfg(test)]
thread_local! {
    /// Test-only, compiled out of a real build (as `fileops.rs`'s probe
    /// counter is): run by `decode_ladder` on its OWN thread right after a
    /// screen rung that did not serve, before the lane's second focus check
    /// — the one instant a unit test cannot otherwise reach, where a focus
    /// change must stop the reserved lane before the plain decode of the
    /// same full (the step-2 review's F3). A focus change from another
    /// thread would race the check it has to precede.
    static AFTER_SCREEN_DECODE: std::cell::Cell<Option<fn(&Shared)>> =
        const { std::cell::Cell::new(None) };

    /// Test-only, compiled out of a real build: the size `decode_ladder`
    /// plans the screen rung from, in place of the stream's own SOF. Planned
    /// from the stream, a rung always serves (QE round 1's D2), so the
    /// ladder's defence for a plan that misses its stream — the fall-through
    /// to the plain decode, the lane's focus check before it, a `Full` that
    /// misses the box — is reached only through a plan the stream cannot
    /// meet, which is what this supplies (an IFD's over-claim, the plan every
    /// such file got before D2).
    static PLANNED_DIMS: std::cell::Cell<Option<(u32, u32)>> =
        const { std::cell::Cell::new(None) };
}

/// The size the ladder plans a screen rung from: the one the STREAM declares
/// in its own SOF, which is what the decoder scales — never the IFD's claim,
/// which a file can over- or under-state (raw-pipeline.md, "The factor rule";
/// QE 2026-09-28, D2; M11). `None` for a stream with no SOF to size, which
/// then gets no rung: its plain decode names what is wrong with it.
fn planned_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    #[cfg(test)]
    if let Some(dims) = PLANNED_DIMS.with(std::cell::Cell::get) {
        return Some(dims);
    }
    crate::raw::sof_dimensions(bytes)
}

/// A rung failed. A broken HIGHER rung must not fail an image that already
/// has a good lower rung (validator MAJOR: valid mid + truncated full-res
/// would badge Failed AND show an image): the flight ends `Ok`, so the
/// worker emits no Failed, and what was achieved is memoized so the ladder
/// quiesces — and one line on stderr names the file, the rung that failed
/// and the decoder's reason, so a fault that shows no badge is still seen
/// (raw-pipeline.md, "All rejections"; brief 008, the step-1 review). The
/// memo stops the climb, so the line prints once. With nothing lower in
/// hand the rung's failure is the image's: the Failed badge says it.
fn keep_lower_rung(
    shared: &Shared,
    index: usize,
    achieved: u32,
    attempted: RungKind,
    reason: String,
) -> Result<(), String> {
    if achieved > 0 {
        note_best(shared, index, achieved);
        eprintln!(
            "fastcull: loupe {}: the {attempted} rung failed ({reason}); the lower rung stays",
            shared.paths[index].display()
        );
        return Ok(());
    }
    Err(reason)
}

/// A rung decoded past a complaint (`note`: a header gap skipped,
/// libjpeg-turbo's image kept, the second opinion taken) prints one line on
/// stderr naming the file, the rung and what the loupe did — at most once
/// per session for each of a file's embedded JPEGs, whichever rungs of it
/// decode how often (raw-pipeline.md, "One line on stderr, once"). The
/// memo is checked and set under the state lock; the line prints after the
/// lock is released.
fn report_complaint(
    shared: &Shared,
    index: usize,
    rung: &crate::raw::EmbeddedJpeg,
    kind: RungKind,
    note: Option<String>,
) {
    let Some(note) = note else {
        return;
    };
    let first = lock(shared).noted.insert((index, rung.offset));
    if first {
        eprintln!(
            "fastcull: loupe {}: the {kind} rung decoded past a complaint -- {note}",
            shared.paths[index].display()
        );
    }
}

fn note_best(shared: &Shared, index: usize, long: u32) {
    let mut state = lock(shared);
    let entry = state.best_long.entry(index).or_insert(0);
    *entry = (*entry).max(long);
}

/// Put a decoded rung in the index's one cache slot and send its `Ready`.
/// `started` is when its decode began: for a full-res frame it opens the
/// time-to-screen measurement (`note_full_started`) under the same lock as
/// the cache write, so the stamp exists before the app can hear of the
/// frame.
fn publish(
    shared: &Shared,
    index: usize,
    image: FullImage,
    terminal: bool,
    state_at_request: RequestState,
    started: DecodeStart,
) {
    let mut state = lock(shared);
    let stamp = shared.stamp.load(Ordering::Relaxed);
    if let Some((old, _)) = state.cache.remove(&index) {
        state.cached_bytes -= old.rgb.len();
    }
    state.cached_bytes += image.rgb.len();
    if image.kind == RungKind::Full {
        note_full_started(&mut state, index, started);
    }
    state.cache.insert(index, (image.clone(), stamp));
    evict_to_budget(&mut state, shared.budget);
    drop(state);
    shared
        .events
        .send(LoupeEvent::Ready {
            index,
            image,
            terminal,
            state: state_at_request,
        })
        .ok();
}

/// Decode one embedded JPEG's bytes at `numerator`/8. The image's kind is
/// the scale the decoder RAN — below 8 a screen rung, at 8 the `candidate`
/// the caller named (the mid, or the full) — never a comparison of the
/// decoded size with the IFD's claim. Beside the image, what the decode went
/// past, if anything (`Decoded::note`).
fn decode_rung(
    bytes: &[u8],
    orientation: u16,
    numerator: u8,
    candidate: RungKind,
) -> Result<(FullImage, Option<String>), String> {
    let decoded = decode_with(bytes, orientation, numerator)?;
    let kind = if decoded.ran < 8 {
        RungKind::Screen
    } else {
        candidate
    };
    let image = FullImage {
        rgb: Arc::new(decoded.rgb),
        width: decoded.width,
        height: decoded.height,
        kind,
    };
    Ok((image, decoded.note))
}

/// Decode a JPEG stream and apply its EXIF orientation — THE full-res hot
/// path, public so `perf_budgets` measures the code that actually ships
/// instead of a re-implementation of it (the old test replicated
/// `decode()` + rotate and therefore could not see pipeline-level wins or
/// regressions in this path).
///
/// The decoder is libjpeg-turbo through the safe `turbojpeg` crate (ADR
/// 0005, brief 008; zune-jpeg until 2026-09-26). Reading the header
/// parses markers and allocates no image buffer, so the two guards below
/// run before anything is sized from the stream's claims (issue #31,
/// raw-pipeline.md "Hostile-input bounds"), in this order: the header's
/// FULL dimensions against `MAX_DECODED_PIXELS`, then `scan_is_terminated`
/// on the bytes — both before any buffer is sized or any scan byte
/// decoded. A stream that passes both and still runs short (a valid EOI
/// over too little entropy data) makes libjpeg-turbo warn, and
/// `tj3Decompress8` returns -1 on any warning, which the crate turns into
/// `Err`: that message is in the damage class, so a `Failed` badge, never a
/// blank success. A progressive stream of more than 100 scans fails the
/// same way (the scan limit, set on every decompressor). A HARMLESS
/// complaint does not fail the decode (raw-pipeline.md, "The decoder's
/// complaints"): see `decode_with`. See [`decode_scaled_oriented`] for the
/// same decode at an N/8 scale; a lossless stream ignores the scale and
/// decodes full-size, and so does a CMYK or YCCK stream, which libjpeg-turbo
/// will not convert to RGB and zune-jpeg decodes instead (brief 008 R14).
pub fn decode_oriented(bytes: &[u8], orientation: u16) -> Result<(Vec<u8>, u32, u32), String> {
    decode_with(bytes, orientation, 8).map(|d| (d.rgb, d.width, d.height))
}

/// [`decode_oriented`] at a DCT scale of `numerator`/8: the screen rung
/// (raw-pipeline.md, "The screen rung"). libjpeg-turbo scales inside the
/// inverse DCT, so a 3/8 decode of the A1's full never builds the
/// 8640x5760 frame at all. `numerator` is 1..=8, 8 being full scale; any
/// other value is refused, because 9/8 and up would UPSCALE, which no rung
/// may do. The guards and their order are `decode_oriented`'s, applied to
/// the header's FULL dimensions before any factor is set. A lossless JPEG,
/// which libjpeg-turbo cannot scale, decodes full-size whatever
/// `numerator` asks (the Cargo.toml canary, item 4), and so does a CMYK or
/// YCCK stream, which zune-jpeg decodes (the canary, item 8), and a stream
/// zune-jpeg's second opinion decodes.
pub fn decode_scaled_oriented(
    bytes: &[u8],
    orientation: u16,
    numerator: u8,
) -> Result<(Vec<u8>, u32, u32), String> {
    decode_with(bytes, orientation, numerator).map(|d| (d.rgb, d.width, d.height))
}

/// The size a `numerator`/8 decode of a `width` x `height` JPEG comes out
/// at: TurboJPEG's `TJSCALED`, a ceiling division, in u64 so the result is
/// the decoder's own arithmetic (8640x5760 at 3/8 is 3240x2160; odd sizes
/// round up). Before orientation: a transposing orientation swaps the two.
pub fn scaled_dims(width: u32, height: u32, numerator: u8) -> (u32, u32) {
    let scale = |d: u32| {
        let scaled = (u64::from(d) * u64::from(numerator)).div_ceil(8);
        u32::try_from(scaled).unwrap_or(u32::MAX)
    };
    (scale(width), scale(height))
}

/// How the loupe reads one of libjpeg-turbo's complaints (raw-pipeline.md,
/// "The decoder's complaints"). The safe crate gives the loupe the complaint
/// as TEXT only — the first message of the decode, which libjpeg reports
/// and a fatal error replaces (the Cargo.toml canary, item 9) — so the
/// class is read off the vendored message texts (`jerror.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Complaint {
    /// A message only a damaged stream raises: refused, and no second
    /// decoder asked.
    Damage,
    /// A message a writer's quirk can raise, after which libjpeg-turbo's
    /// buffer is complete: its image is used, at the rung asked for.
    Kept,
    /// Everything else: zune-jpeg's second opinion, at full scale.
    SecondOpinion,
}

/// Sort one libjpeg-turbo message into its [`Complaint`] class, by `contains`
/// on the vendored texts — never on the "Corrupt JPEG data" prefix, which
/// three classes share (a marker hit mid-scan is damage, leftover bytes are
/// kept, a bad ICC chunk takes the second opinion). The damage class: the
/// data ran out (`JWRN_JPEG_EOF`, `JWRN_HIT_MARKER`), a code no table holds
/// (`JWRN_HUFF_BAD_CODE`, `JWRN_ARITH_BAD_CODE`), a restart marker out of
/// sequence (`JWRN_MUST_RESYNC`), and the scan limit's own message. The kept
/// class: the two scan-parameter warnings (`JWRN_NOT_SEQUENTIAL`,
/// `JWRN_BOGUS_PROGRESSION`) and bytes left over after a scan
/// (`JWRN_EXTRANEOUS_DATA`).
pub(crate) fn complaint_class(message: &str) -> Complaint {
    const DAMAGE: [&str; 6] = [
        "Premature end of JPEG file",
        "premature end of data segment",
        "bad Huffman code",
        "bad arithmetic code",
        "instead of RST",
        "Progressive JPEG image has more than",
    ];
    const KEPT: [&str; 3] = [
        "Invalid SOS parameters for sequential JPEG",
        "Inconsistent progression sequence",
        "extraneous bytes before marker",
    ];
    if DAMAGE.iter().any(|text| message.contains(text)) {
        Complaint::Damage
    } else if KEPT.iter().any(|text| message.contains(text)) {
        Complaint::Kept
    } else {
        Complaint::SecondOpinion
    }
}

/// One loupe decode: the oriented RGB, its size, the numerator the decoder
/// RAN — 8 for a lossless, CMYK or YCCK stream and for zune-jpeg's second
/// opinion, whatever was asked — and what the loupe decoded past, if
/// anything: a header gap skipped, libjpeg-turbo's image kept past a
/// complaint, the second opinion taken. The ladder prints the note once per
/// file; the public entry points drop it.
struct Decoded {
    rgb: Vec<u8>,
    width: u32,
    height: u32,
    ran: u8,
    note: Option<String>,
}

/// libjpeg-turbo's message without the safe crate's "TurboJPEG error: "
/// prefix, for a note or a combined reason a person reads.
fn bare(message: &str) -> &str {
    message.strip_prefix("TurboJPEG error: ").unwrap_or(message)
}

/// Two notes, either optional, joined with "; ".
fn join_notes(first: Option<String>, second: Option<String>) -> Option<String> {
    match (first, second) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    }
}

/// The one decode behind [`decode_oriented`] and [`decode_scaled_oriented`].
/// It reports the numerator the decoder RAN, so the ladder can tell a scaled
/// rung from the full by what the decoder did, never by comparing a size
/// with an IFD's claim (raw-pipeline.md, "The screen rung": the app never
/// infers top-rung-ness from a size).
///
/// The order is the spec's (raw-pipeline.md, "Hostile-input bounds" and
/// "The decoder's complaints") and each step depends on the ones before it:
/// the numerator's range; the header-gap pre-pass, so libjpeg-turbo reads
/// the header with no warning and zune-jpeg's strict mode never meets a gap;
/// the header read — whose complaint is refused when it is damage and
/// otherwise handed to the second opinion, since a header read that warned
/// leaves no header to decode from; the pixel cap on the header's FULL
/// dimensions; the byte check; the CMYK/YCCK route; the scale; the buffers;
/// the decode — whose complaint is refused, kept or handed on by its class.
fn decode_with(bytes: &[u8], orientation: u16, numerator: u8) -> Result<Decoded, String> {
    if !(1..=8).contains(&numerator) {
        return Err(format!("scaling numerator {numerator} out of 1..=8"));
    }
    // THE PRE-PASS (raw-pipeline.md, "Header gaps are skipped before any
    // decode"): bytes between two header segments that are not a marker are
    // dropped before either decoder sees the stream. `Cow::Borrowed` when
    // there are none, so an A1 stream is never copied.
    let (bytes, gap) = crate::raw::without_header_gaps(bytes);
    let bytes: &[u8] = &bytes;
    let gap_note = (gap > 0).then(|| format!("skipped {gap} bytes between header segments"));
    // One decompressor per call: its setup is small next to a decode, and
    // the loupe's workers then share no decoder state. A per-thread handle
    // would have to be measured to earn its place (brief 008).
    let mut dec = turbojpeg::Decompressor::new().map_err(|e| format!("decode: {e}"))?;
    // At most 100 progressive scans (raw-pipeline.md, "Progressive scans: at
    // most 100"; brief 008 R2): a scan is a pass over every block of the
    // components it covers, so a small crafted stream with thousands of scans
    // would hold a decoder far longer than any real photo. libjpeg-turbo's
    // default is no limit; 100 is the bound zune-jpeg 0.4's default gave the
    // loupe until brief 008, which the decoder swap dropped unnoticed (the
    // Cargo.toml canary, item 6). Over it the decode fails with the
    // library's own message. Set before anything else, so no decode on this
    // handle runs without it.
    dec.set_scan_limit(100)
        .map_err(|e| format!("decode: {e}"))?;
    let header = match dec.read_header(bytes) {
        Ok(header) => header,
        Err(e) => {
            // libjpeg-turbo's header read fails on ANY warning and then
            // gives no header (the canary, item 11): refuse damage; for
            // anything else — a JFIF revision it does not know, an ICC chunk
            // out of sequence, an unknown Adobe transform — ask zune-jpeg,
            // whose route checks the pixel cap and the byte check itself,
            // there being no libjpeg-turbo header to check them on.
            let message = e.to_string();
            if complaint_class(&message) == Complaint::Damage {
                return Err(format!("decode: {message}"));
            }
            drop(dec);
            return second_opinion(bytes, orientation, &message, gap_note);
        }
    };
    // Issue #31: the header's dimension claim sizes the decode buffer, the
    // prefault pass and the transpose Scratch below, and in a crafted file
    // every claim is the attacker's. Reject an implausible claim — on the
    // FULL dimensions, before any factor — and an unterminated stream here,
    // while nothing has been allocated from them. The byte check runs
    // second so that a hostile stream that is ALSO cut short is named for
    // its size ("implausible"), and it runs at all, although the decoder
    // would fail a cut-off scan by itself, because it spares the 80-115 ms
    // grey decode of the commonest field corruption (a cut-off copy) and
    // names the cause ("truncated"), which the decoder's message does not.
    if !crate::raw::plausible_decoded_dims(header.width, header.height) {
        return Err(format!(
            "implausible JPEG dimensions {}x{} (over {} pixels)",
            header.width,
            header.height,
            crate::raw::MAX_DECODED_PIXELS
        ));
    }
    if !crate::raw::scan_is_terminated(bytes) {
        return Err("truncated JPEG stream (scan reaches no end-of-image marker)".into());
    }
    // CMYK and YCCK (brief 008 R14; raw-pipeline.md, "The decoder"):
    // libjpeg-turbo will not convert either to RGB ("Unsupported color
    // conversion request", the Cargo.toml canary, item 8), so such a stream
    // -- a print-ready bare JPEG, issue #8 -- decodes through zune-jpeg as
    // every loupe stream did before the swap, at FULL scale whatever
    // `numerator` asked: no screen rung, and 8 reported as the numerator
    // run, so the ladder sees a full. The two guards above have run on this
    // stream's own SOF and bytes; the route checks both again on zune-jpeg's
    // header, as it must when it is the second opinion.
    if matches!(
        header.colorspace,
        turbojpeg::Colorspace::CMYK | turbojpeg::Colorspace::YCCK
    ) {
        let (rgb, width, height) = decode_through_zune(bytes, orientation)
            .map_err(|zune| format!("decode: zune-jpeg: {zune}"))?;
        return Ok(Decoded {
            rgb,
            width,
            height,
            ran: 8,
            note: gap_note,
        });
    }
    let numerator = if header.is_lossless { 8 } else { numerator };
    // `ScalingFactor::new` reduces by the gcd, so 8/8 is the crate's `ONE`
    // and a lossless stream never meets `CannotScaleLossless`.
    dec.set_scaling_factor(turbojpeg::ScalingFactor::new(usize::from(numerator), 8))
        .map_err(|e| format!("decode: {e}"))?;
    let full_w = u32::try_from(header.width).map_err(|_| "width overflow")?;
    let full_h = u32::try_from(header.height).map_err(|_| "height overflow")?;
    let (w, h) = scaled_dims(full_w, full_h, numerator);
    let n = (w as usize)
        .checked_mul(h as usize)
        .and_then(|px| px.checked_mul(3))
        .ok_or("dimension overflow")?;
    // The A1 full-res JPEG is baseline with ZERO restart markers (verified
    // by parsing them — probe 2026-08-02), so the Huffman decode is
    // strictly serial: one core for ~170 ms (~220 ms under zune-jpeg)
    // while the rest idle. Two things reclaim that dead time on a 50 MP
    // frame (raw-pipeline.md, Orientation):
    //
    // - The decode fills a pre-faulted buffer we own: `decompress` writes
    //   into `rgb`, whose first-touch page faults `prefault_parallel` pays
    //   from several threads first (measured 2026-09-26 during brief 008:
    //   alloc + prefault ~14 ms at full size, ~4 ms at a rung; under
    //   zune-jpeg, `decode_into` a pre-faulted buffer saved ~30 ms against
    //   `decode()`'s internal allocation).
    // - The transpose's output buffer is allocated AND pre-faulted on a
    //   spare thread WHILE the decode runs, so the rotate that follows
    //   starts with hot pages ([`crate::raw::Scratch`]).
    //
    // Neither changes peak memory: the same two buffers exist either way;
    // only WHEN the page faults are paid moves — off the critical path.
    // Pitch is 3w with no row padding: the buffer is byte for byte the
    // packed RGB the kitchen's `SharedPixelBuffer` fill takes.
    //
    // The buffer and the scratch come back WITH the decode's outcome, not
    // only on success: a kept complaint uses them as decoded.
    let needs_transpose = matches!(orientation, 5..=8);
    let (rgb, scratch, decoded) = std::thread::scope(|scope| {
        let scratch = needs_transpose.then(|| {
            // Output dims are swapped, but the byte count is what matters
            // and it is identical; build it while the decode runs.
            scope.spawn(|| crate::raw::Scratch::prefaulted(h, w))
        });
        let mut rgb = vec![0u8; n];
        crate::raw::orient::prefault_parallel(&mut rgb);
        let decoded = dec.decompress(
            bytes,
            turbojpeg::Image {
                pixels: &mut rgb[..],
                width: w as usize,
                pitch: w as usize * 3,
                height: h as usize,
                format: turbojpeg::PixelFormat::RGB,
            },
        );
        let scratch = scratch.map(|j| j.join().expect("prefault thread"));
        (rgb, scratch, decoded)
    });
    let note = match decoded {
        Ok(()) => None,
        Err(e) => {
            let message = e.to_string();
            match complaint_class(&message) {
                Complaint::Damage => return Err(format!("decode: {message}")),
                // `tj3Decompress8` writes every scanline before it returns
                // -1 for a warning, and a fatal error after one would have
                // replaced its text (the canary, item 10): the buffer is
                // complete, so it is used as decoded, at the rung asked for.
                Complaint::Kept => Some(format!(
                    "kept libjpeg-turbo's image past: {}",
                    bare(&message)
                )),
                Complaint::SecondOpinion => {
                    // Freed BEFORE zune-jpeg allocates (raw-pipeline.md, "The
                    // decoder's complaints"): libjpeg-turbo's buffer and the
                    // transpose scratch go first, so a decoder never holds
                    // more than its two full-size frames.
                    drop(rgb);
                    drop(scratch);
                    drop(dec);
                    return second_opinion(bytes, orientation, &message, gap_note);
                }
            }
        }
    };
    // Soft-rotate to display orientation (spec: every rung).
    let (rgb, width, height) = crate::raw::apply_orientation_with(rgb, w, h, orientation, scratch);
    Ok(Decoded {
        rgb,
        width,
        height,
        ran: numerator,
        note: join_notes(gap_note, note),
    })
}

/// zune-jpeg's second opinion on a stream libjpeg-turbo refused over a
/// complaint outside the damage and kept classes (raw-pipeline.md, "The
/// decoder's complaints"): the zune-jpeg route at full scale, its note
/// naming libjpeg-turbo's complaint; when zune-jpeg refuses too, the rung has
/// failed with both decoders' reasons.
fn second_opinion(
    bytes: &[u8],
    orientation: u16,
    libjpeg_message: &str,
    gap_note: Option<String>,
) -> Result<Decoded, String> {
    let libjpeg = bare(libjpeg_message);
    match decode_through_zune(bytes, orientation) {
        Ok((rgb, width, height)) => Ok(Decoded {
            rgb,
            width,
            height,
            ran: 8,
            note: join_notes(
                gap_note,
                Some(format!("libjpeg-turbo: {libjpeg}; decoded by zune-jpeg")),
            ),
        }),
        Err(zune) => Err(format!(
            "decode: libjpeg-turbo: {libjpeg}; zune-jpeg: {zune}"
        )),
    }
}

/// The zune-jpeg route (brief 008 R14; raw-pipeline.md, "The decoder" and
/// "The decoder's complaints"): the loupe's decode as it shipped before
/// libjpeg-turbo (the pre-swap `decode_oriented`, e1b488a^, verbatim), for
/// the streams libjpeg-turbo cannot give as RGB — CMYK and YCCK, which
/// zune-jpeg 0.4 converts — and as the second opinion. It decodes at FULL
/// size only (zune-jpeg has no DCT scaling), in strict mode with RGB out and
/// the per-side limits lifted (the default 16384 would reject a
/// panorama-wide bare JPEG), into a pre-faulted buffer with the transpose
/// scratch built while it runs, then rotates.
///
/// It checks the pixel cap on ZUNE-JPEG'S header and then the byte check
/// itself, before any buffer is sized — load-bearing for the second opinion,
/// which may come after a libjpeg-turbo header read that failed and left no
/// header to check; for CMYK and YCCK the caller has checked both already.
/// Its errors are zune-jpeg's messages, or the guards' own, bare: the
/// caller says which decoder spoke.
fn decode_through_zune(bytes: &[u8], orientation: u16) -> Result<(Vec<u8>, u32, u32), String> {
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
    decoder.decode_headers().map_err(|e| e.to_string())?;
    let (w, h) = decoder.dimensions().ok_or("no dimensions")?;
    // Issue #31, on zune-jpeg's own reading of the header: nothing is
    // allocated from a claim before these two pass (raw-pipeline.md,
    // "Hostile-input bounds"). zune-jpeg zero-fills a cut scan and calls it
    // a success, so the byte check is its only guard against one.
    if !crate::raw::plausible_decoded_dims(w, h) {
        return Err(format!(
            "implausible JPEG dimensions {w}x{h} (over {} pixels)",
            crate::raw::MAX_DECODED_PIXELS
        ));
    }
    if !crate::raw::scan_is_terminated(bytes) {
        return Err("truncated JPEG stream (scan reaches no end-of-image marker)".into());
    }
    let n = w
        .checked_mul(h)
        .and_then(|px| px.checked_mul(3))
        .ok_or("dimension overflow")?;
    let w = u32::try_from(w).map_err(|_| "width overflow")?;
    let h = u32::try_from(h).map_err(|_| "height overflow")?;
    let needs_transpose = matches!(orientation, 5..=8);
    let (rgb, scratch) = std::thread::scope(|scope| {
        let scratch = needs_transpose.then(|| {
            // Output dims are swapped, but the byte count is what matters
            // and it is identical; build it while the decode runs.
            scope.spawn(|| crate::raw::Scratch::prefaulted(h, w))
        });
        let mut rgb = vec![0u8; n];
        crate::raw::orient::prefault_parallel(&mut rgb);
        let decoded = decoder.decode_into(&mut rgb).map_err(|e| e.to_string());
        let scratch = scratch.map(|j| j.join().expect("prefault thread"));
        decoded.map(|()| (rgb, scratch))
    })?;
    // Soft-rotate to display orientation (spec: every rung).
    Ok(crate::raw::apply_orientation_with(
        rgb,
        w,
        h,
        orientation,
        scratch,
    ))
}

fn evict_to_budget(state: &mut LoupeState, budget: usize) {
    while state.cached_bytes > budget && state.cache.len() > 1 {
        let focused = state.focused;
        let Some((&victim, _)) = state
            .cache
            .iter()
            .filter(|(k, _)| Some(**k) != focused)
            .min_by_key(|(_, (_, s))| *s)
        else {
            return;
        };
        if let Some((img, _)) = state.cache.remove(&victim) {
            state.cached_bytes -= img.rgb.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A queued entry with a `Long` target and the settled state: what the
    /// tests written before brief 008 spelled `(index, long, focus_origin)`.
    fn long_entry(index: usize, long: u32, focus_origin: bool) -> Entry {
        Entry {
            index,
            target: Target::Long(long),
            focus_origin,
            state: RequestState::Settled,
        }
    }

    /// `schedule` at a `Long` target in the settled state: the call the
    /// tests written before brief 008 made with a bare long edge.
    fn schedule_long(
        state: &mut LoupeState,
        index: usize,
        long: u32,
        stamp: u64,
        origin: Origin,
    ) -> bool {
        schedule(
            state,
            index,
            Target::Long(long),
            stamp,
            origin,
            RequestState::Settled,
        )
    }

    /// `revive_deferred` at a `Long` target in the settled state, likewise,
    /// over a folder of 1000 (the view's own length when one is set).
    fn revive_long(state: &mut LoupeState, index: usize, long: u32, stamp: u64) -> bool {
        revive_deferred(
            state,
            index,
            Target::Long(long),
            RequestState::Settled,
            stamp,
            1000,
            std::time::Instant::now(),
        )
    }

    /// The ring's members as image ids in push order — `ring_order`, mapped
    /// through the view as `focus_on` maps them.
    fn ring_ids(
        state: &LoupeState,
        fpos: usize,
        lo: usize,
        hi: usize,
        forward: bool,
    ) -> Vec<usize> {
        ring_order(fpos, lo, hi, forward)
            .into_iter()
            .filter_map(|p| state.id_at(p))
            .collect()
    }

    /// The ring plan for the given inputs, over a folder of `len`, for the
    /// table rows below.
    fn plan(
        transit: bool,
        forward: bool,
        fpos: usize,
        len: usize,
        desired: Target,
        fit_box: Option<FitBox>,
        fullres_ahead: usize,
    ) -> RingPlan {
        plan_ring(&PlanInputs {
            transit,
            forward,
            fpos,
            len,
            desired,
            fit_box,
            fullres_ahead,
            switch: SwitchState::default(),
        })
    }

    /// The two scheduling polarities, pinned: focus work goes to the BACK
    /// (popped first) and replaces a pending entry for the same index;
    /// grid work goes to the FRONT and yields to whatever focus queued.
    /// And the in-flight case defers with a MAX merge — a later smaller
    /// target must never undo a bigger one (recorded QE defect).
    #[test]
    fn schedule_polarity_and_deferred_merge() {
        let mut st = LoupeState::default();
        // Grid want first, then focus: focus must end up behind it in the
        // vec (= popped first) and carry the focus-origin flag.
        assert!(schedule_long(&mut st, 7, 1616, 1, Origin::Grid));
        assert!(schedule_long(&mut st, 3, 8640, 2, Origin::Focus));
        assert_eq!(
            st.queue,
            vec![long_entry(7, 1616, false), long_entry(3, 8640, true)]
        );

        // A grid want for an already-queued index yields (no duplicate,
        // no downgrade of the focus entry's target).
        assert!(!schedule_long(&mut st, 3, 1616, 3, Origin::Grid));
        assert_eq!(
            st.queue,
            vec![long_entry(7, 1616, false), long_entry(3, 8640, true)]
        );

        // A focus request for an already-queued index REPLACES it.
        assert!(schedule_long(&mut st, 7, 8640, 4, Origin::Focus));
        assert_eq!(
            st.queue,
            vec![long_entry(3, 8640, true), long_entry(7, 8640, true)]
        );

        // In flight: nothing is queued, the target is deferred, and the
        // merge keeps the LARGEST target regardless of arrival order.
        st.in_flight.push(5);
        assert!(!schedule_long(&mut st, 5, 8640, 5, Origin::Focus));
        assert!(!schedule_long(&mut st, 5, 1616, 6, Origin::Grid));
        assert_eq!(
            st.deferred.get(&5),
            Some(&(Target::Long(8640), RequestState::Settled))
        );
        assert_eq!(st.queue.len(), 2, "an in-flight index is never queued");

        // Failed indexes are never scheduled again.
        st.failed.insert(9);
        assert!(!schedule_long(&mut st, 9, 1616, 7, Origin::Focus));
        assert_eq!(st.queue.len(), 2);
    }

    /// The boundary of the top-rung predicate, pinned: the mid-class
    /// ceiling itself is NOT top (`serves` allows only a 1.25x upscale
    /// above it), one pixel more is, and a terminal rung is top at any
    /// size (issue #8).
    #[test]
    fn top_rung_boundary() {
        assert!(!is_top_rung(MID_RUNG_MAX_LONG, false));
        assert!(is_top_rung(MID_RUNG_MAX_LONG + 1, false));
        assert!(is_top_rung(640, true));
    }

    /// State whose focus has already HELD past the debounce (the
    /// settled case) at a MAX target; tests for fresh/transient/
    /// escalating focuses override `focused_at`/`focused_target`.
    /// TRANSIT vs SETTLED (user requirement 2026-08-01). Held keys must be
    /// distinguished from deliberate taps, and the distinction must decay
    /// once the user stops.
    #[test]
    fn transit_tracks_held_keys_and_decays_on_release() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let mut st = LoupeState::default();

        // First ever focus is NOT transit: there is no previous change to
        // be close to. A folder must not open in scrub mode.
        note_focus(&mut st, 0, Target::Long(u32::MAX), t0);
        assert!(!in_transit(&st, t0), "the first focus is never transit");

        // Held key: changes one repeat interval apart.
        let mut t = t0;
        for i in 1..=5 {
            t += Duration::from_millis(120);
            note_focus(&mut st, i, Target::Long(u32::MAX), t);
            assert!(in_transit(&st, t), "a held key at 120 ms must be transit");
        }
        // ...and it decays once the key is released.
        assert!(
            in_transit(&st, t + SETTLE_DEBOUNCE - Duration::from_millis(1)),
            "still transit just before the settle"
        );
        assert!(
            !in_transit(&st, t + SETTLE_DEBOUNCE),
            "settled once the debounce elapses"
        );
        // Those two are written in terms of the constant, so they hold for
        // ANY value of it — including 5 s, which would strand the user on a
        // mid rung long after they stopped. Pin the value itself in absolute
        // terms, from both sides:
        assert!(
            in_transit(&st, t + Duration::from_millis(100)),
            "100 ms after the last key is still mid-hold at any normal repeat \
             rate; settling that eagerly would fire a sharp decode between \
             every two frames of a held arrow"
        );
        assert!(
            !in_transit(&st, t + Duration::from_millis(200)),
            "200 ms after release the user has stopped and is WAITING — the \
             settle is paid on every stop and is pure latency before the \
             sharp decode even starts"
        );

        // Deliberate tap-stepping through a burst is NOT transit, so each
        // tap asks for the sharp rung immediately.
        let mut st = LoupeState::default();
        let mut t = t0;
        note_focus(&mut st, 0, Target::Long(u32::MAX), t);
        for i in 1..=4 {
            t += Duration::from_millis(400);
            note_focus(&mut st, i, Target::Long(u32::MAX), t);
            assert!(!in_transit(&st, t), "a 400 ms tap must not be transit");
        }
    }

    /// Brief 008 (raw-pipeline.md, Contracts, `travel_left()`): the transit
    /// time that remains — `SETTLE_DEBOUNCE` minus the time since the last
    /// index change while in transit, `None` at rest. The pill's input: its
    /// `is_some()` is "travelling", and its value is when a pill held lit by
    /// its minimum must clear, so both are pinned in absolute terms.
    #[test]
    fn travel_left_is_the_transit_that_remains() {
        use std::time::{Duration, Instant};
        let ms = Duration::from_millis;
        let t0 = Instant::now();
        let mut st = LoupeState::default();
        note_focus(&mut st, 0, Target::Long(u32::MAX), t0);
        assert_eq!(
            travel_left_at(&st, t0),
            None,
            "the first focus is never travel"
        );
        // A held key: the next frame one repeat interval later.
        let t1 = t0 + ms(120);
        note_focus(&mut st, 1, Target::Long(u32::MAX), t1);
        assert_eq!(travel_left_at(&st, t1), Some(ms(150)));
        assert_eq!(travel_left_at(&st, t1 + ms(40)), Some(ms(110)));
        assert_eq!(travel_left_at(&st, t1 + ms(149)), Some(ms(1)));
        assert_eq!(
            travel_left_at(&st, t1 + ms(150)),
            None,
            "travel ends when the settle debounce has passed"
        );
        // A tap a second later is not travel.
        let t2 = t1 + ms(1000);
        note_focus(&mut st, 2, Target::Long(u32::MAX), t2);
        assert_eq!(travel_left_at(&st, t2), None, "a tap is not travel");
    }

    /// SETTLE GUARANTEE: after a transit, something must ask for the
    /// sharp rung — and it can only be this lane.
    ///
    /// Transit deliberately caps every request at the mid, so when the user
    /// stops there is no full-res request anywhere in the system. The app
    /// cannot issue one: its refresh loop is event-driven and goes quiet
    /// exactly when nothing is decoding. Without this branch the user holds
    /// an arrow, stops, and the frame stays soft forever — a strictly worse
    /// bug than the slow transit this whole change exists to fix.
    #[test]
    fn a_settled_frame_climbs_even_though_transit_only_asked_for_the_mid() {
        let now = std::time::Instant::now();
        // Transit left index 4 at the mid, and nothing queued for it.
        let mut state = stable_focus_state(4);
        state.desired = Target::Long(8640);
        state.last_index_change = Some(now - SETTLE_DEBOUNCE);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: MID_RUNG_TARGET,
                    height: 1080,
                    kind: RungKind::Mid,
                },
                0,
            ),
        );
        assert!(state.queue.is_empty(), "transit queued nothing sharp");
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Job(4, Target::Long(8640), RequestState::Settled),
            "a settled frame short of the app's target must climb"
        );

        // Still MOVING: the guarantee must not fire mid-hold, or every
        // frame of a held arrow starts a full-res decode and transit is
        // pointless.
        let mut state = stable_focus_state(4);
        state.desired = Target::Long(8640);
        state.last_index_change = Some(now);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: MID_RUNG_TARGET,
                    height: 1080,
                    kind: RungKind::Mid,
                },
                0,
            ),
        );
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "no sharp decode while the user is still moving"
        );

        // Already IN FLIGHT: releasing the key while the transit mid is
        // still decoding is the common case, and queueing the sharp job
        // anyway burns a second worker on a duplicate and ~149 MB of
        // transient for an A1 (QE finding, 2026-08-01).
        let mut state = stable_focus_state(4);
        state.desired = Target::Long(8640);
        state.last_index_change = Some(now - SETTLE_DEBOUNCE);
        state.in_flight.push(4);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "the settle must not duplicate a job already in flight"
        );
        assert!(
            state.queue.is_empty(),
            "and must not leave a duplicate queued either: {:?}",
            state.queue
        );

        // Already sharp: the lane must not re-queue it forever (a spin).
        let mut state = stable_focus_state(4);
        state.desired = Target::Long(8640);
        state.last_index_change = Some(now - SETTLE_DEBOUNCE);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: 8640,
                    height: 5760,
                    kind: RungKind::Full,
                },
                0,
            ),
        );
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "a frame that already serves the target must not be re-queued"
        );
    }

    /// The settle guarantee POLLS; it must not touch the LRU order.
    ///
    /// It runs on the reserved lane's timer, so it fires repeatedly while
    /// the user simply looks at a photo. Refreshing the stamp there (the
    /// first version passed `stamp: 0` to `sufficient_cached`, which
    /// WRITES) marked the settled frame as the oldest entry in the cache —
    /// so the frame the user had just been studying became the first thing
    /// evicted the moment they arrowed away, which is the exact opposite of
    /// what arrowing back to compare a burst needs.
    #[test]
    fn the_settle_guarantee_does_not_disturb_the_lru_order() {
        let now = std::time::Instant::now();
        let mut state = stable_focus_state(4);
        state.desired = Target::Long(8640);
        state.last_index_change = Some(now - SETTLE_DEBOUNCE);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: 8640,
                    height: 5760,
                    kind: RungKind::Full,
                },
                77,
            ),
        );
        // Already sharp: the guarantee looks, decides there is nothing to
        // do, and must leave the stamp exactly as it found it.
        assert_eq!(next_job(&mut state, true, 0, 1000, now), Slot::Wait);
        assert_eq!(
            state.cache.get(&4).map(|(_, s)| *s),
            Some(77),
            "the settle poll must not restamp the frame it merely inspected"
        );
    }

    /// Brief 008 A1 (raw-pipeline.md, "The ring"): the transit ring leans the
    /// way the user is travelling, 2 behind and 15 ahead, and a reversal
    /// re-leans it on the very next frame. Its depths moved from the old
    /// `TRANSIT_BEHIND` / `TRANSIT_AHEAD`, 2 / 8, to `RING_BEHIND` /
    /// `RING_AHEAD`, the promise kept — red on the old shape by construction.
    /// This engine has no fit box, so settled it keeps ±`PREFETCH` and the
    /// app's real target, and in transit it asks for the mid.
    ///
    /// Untested, a symmetric ring survives every other assertion here: it
    /// still requests the mid, still keeps up on the frame you are ON. What
    /// it loses is the whole point of the look-ahead — the frames arriving
    /// BEFORE the finger gets to them.
    #[test]
    fn transit_ring_leans_in_the_direction_of_travel() {
        let top = Target::Long(u32::MAX);
        let span = |p: &RingPlan| (p.lo, p.hi);
        // Moving forward: far more ahead than behind.
        let forward = plan(true, true, 500, 1000, top, None, RING_AHEAD);
        assert_eq!(
            span(&forward),
            (498, 515),
            "a forward ring leans forward, 2 behind / 15 ahead"
        );
        assert_eq!(forward.members.len(), 17, "every position but the focused");
        // Reversed on the very next frame: the lean flips with it.
        assert_eq!(
            span(&plan(true, false, 500, 1000, top, None, RING_AHEAD)),
            (485, 502),
            "arrowing back must re-lean backward immediately"
        );
        // Settled: the tight symmetric ring, and the app's REAL target.
        let desired = Target::Long(8640);
        let settled = plan(false, true, 500, 1000, desired, None, RING_AHEAD);
        assert_eq!(span(&settled), (498, 502));
        assert_eq!(
            settled.focused, desired,
            "a settled frame must ask for full quality"
        );
        assert!(settled.members.iter().all(|(_, t)| *t == desired));
        let moving = plan(true, true, 500, 1000, desired, None, RING_AHEAD);
        assert!(
            moving.focused < settled.focused,
            "transit must ask for LESS than settled, or it is not transit"
        );
        assert!(
            moving
                .members
                .iter()
                .all(|(_, t)| *t == Target::Long(MID_RUNG_TARGET)),
            "with no box, transit asks the mid of the whole ring: {:?}",
            moving.members
        );
        // Edges clamp rather than wrap or panic.
        assert_eq!(
            span(&plan(true, true, 0, 1000, top, None, RING_AHEAD)),
            (0, 15),
            "the ring clamps at the start of the folder"
        );
        assert_eq!(
            span(&plan(true, true, 999, 1000, top, None, RING_AHEAD)),
            (997, 999),
            "the ring clamps at the end of the folder"
        );
        // A folder shorter than the ring is the whole folder.
        assert_eq!(
            span(&plan(true, true, 2, 10, top, None, RING_AHEAD)),
            (0, 9)
        );
        assert_eq!(
            span(&plan(true, true, 3, 10, top, None, RING_AHEAD)),
            (1, 9),
            "position 0 is three behind"
        );
    }

    /// Brief 008 A1 (raw-pipeline.md, "The ring", its request table): at fit
    /// the whole ring — 2 behind / 15 ahead, leaned — and the focused frame
    /// ask for the fit box, travelling and settled alike, so a stop at fit
    /// asks nothing the hold had not already asked; on a box the mid serves
    /// the shape is the same (the ladder, not the plan, picks the mid there).
    /// Red when an engine with a box keeps the old settled ±2 (the settled
    /// rows), and when a transit is capped at the mid whatever the box — the
    /// one mutation of `transit_request`, A7's first — on the transit rows.
    #[test]
    fn the_ring_at_fit_asks_the_fit_box_travelling_and_settled() {
        for fit_box in [
            FitBox {
                width: 3840,
                height: 2160,
            },
            FitBox {
                width: 1920,
                height: 1080,
            },
        ] {
            let fit = Target::Fit(fit_box);
            for transit in [true, false] {
                for (forward, span) in [(true, (498, 515)), (false, (485, 502))] {
                    let row = format!(
                        "{}x{} box, transit {transit}, forward {forward}",
                        fit_box.width, fit_box.height
                    );
                    let p = plan(transit, forward, 500, 1000, fit, Some(fit_box), RING_AHEAD);
                    assert_eq!((p.lo, p.hi), span, "{row}");
                    assert_eq!(p.focused, fit, "{row}: the focused frame");
                    assert_eq!(p.members.len(), 17, "{row}");
                    assert!(
                        p.members.iter().all(|(_, t)| *t == fit),
                        "{row}: every member asks the fit box: {:?}",
                        p.members
                    );
                }
            }
        }
    }

    /// Brief 008 A1 (raw-pipeline.md, "Above fit"): above fit the ring in
    /// force is the FULL-RES ring, clamped at its far end so the pixel
    /// cache's figure holds each frame twice — its pixels and its texture
    /// copy — and the kitchen's fill besides: 3 ahead at the 2 GiB floor, 10
    /// on 4 GiB, 15 from 5,524,070,400 B (Manager rulings 2026-09-26, brief
    /// 008 Q4 and Q-G). Settled, every member asks full-res; during a hold the
    /// focused frame and the two behind ask the fit box and the members
    /// ahead full-res; and the positions beyond the clamp ask for nothing.
    /// Red under the clamp that counted the pixels alone (11, 15, 15) and
    /// under the one that left out the fill (4, 11, 15), and when the
    /// positions beyond the clamp ask the fit box.
    #[test]
    fn the_full_res_ring_is_clamped_by_the_cache() {
        const GIB: u64 = 1 << 30;
        for (cache, ahead) in [
            (2 * GIB, 3),
            (4 * GIB, 10),
            (5_524_070_399, 14),
            (5_524_070_400, 15),
            (8 * GIB, 15),
            (10 * GIB, 15),
            (200 << 20, 0),
        ] {
            assert_eq!(fullres_ring_ahead(cache), ahead, "a {cache} B cache");
        }
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let top = Target::Long(u32::MAX);
        for ahead in [3usize, 10, 15] {
            let ring: Vec<usize> = (498..=500 + ahead).filter(|p| *p != 500).collect();
            let settled = plan(false, true, 500, 1000, top, Some(uhd), ahead);
            assert_eq!(
                (settled.lo, settled.hi),
                (498, 500 + ahead),
                "settled, {ahead} ahead"
            );
            assert_eq!(settled.focused, top, "settled: the top rung");
            assert!(
                settled.members.iter().all(|(_, t)| *t == top),
                "settled, {ahead} ahead: every member full-res: {:?}",
                settled.members
            );
            let hold = plan(true, true, 500, 1000, top, Some(uhd), ahead);
            assert_eq!(
                hold.focused,
                Target::Fit(uhd),
                "a hold: the focused frame asks the fit box"
            );
            let mut asked: Vec<usize> = hold.members.iter().map(|(p, _)| *p).collect();
            asked.sort_unstable();
            assert_eq!(
                asked, ring,
                "a hold, {ahead} ahead: the positions beyond the clamp ask for nothing"
            );
            for (pos, target) in &hold.members {
                let want = if *pos < 500 { Target::Fit(uhd) } else { top };
                assert_eq!(*target, want, "a hold, {ahead} ahead: position {pos}");
            }
        }
    }

    /// raw-pipeline.md, "Order in the queue": farthest first, and at equal
    /// distance the member in the travel direction is pushed later, so it is
    /// popped first — both ways. Before brief 008 a stable sort pushed the
    /// higher position later whatever the direction, right forward and wrong
    /// on every backward hold (red then on the backward rows).
    #[test]
    fn ring_ties_break_toward_the_travel_direction() {
        let state = LoupeState::default(); // no view: identity
        let at = |ids: &[usize], id: usize| {
            ids.iter()
                .position(|i| *i == id)
                .unwrap_or_else(|| panic!("{id} is not in {ids:?}"))
        };
        let backward = ring_ids(&state, 10, 4, 16, false);
        let forward = ring_ids(&state, 10, 4, 16, true);
        for d in 1..=6 {
            assert!(
                at(&backward, 10 - d) > at(&backward, 10 + d),
                "backward, distance {d}: {} must be popped before {}: {backward:?}",
                10 - d,
                10 + d
            );
            assert!(
                at(&forward, 10 + d) > at(&forward, 10 - d),
                "forward, distance {d}: {} must be popped before {}: {forward:?}",
                10 + d,
                10 - d
            );
        }
        assert_eq!(
            (backward.first(), backward.last()),
            (Some(&16), Some(&9)),
            "farthest first, the nearest in the travel direction last"
        );
    }

    /// raw-pipeline.md, "Culling": a focus drops the QUEUED focus-origin
    /// entries whose view position lies outside the ring in force, and leaves
    /// grid entries and in-flight decodes alone — so a reversal culls what
    /// leaned the wrong way. The clamped row: above fit, the ring in force is
    /// the full-res ring as the cache clamps it, so a `Z` from fit drops the
    /// fit-box entries beyond its far end — red when the engine ignores its
    /// own clamp.
    #[test]
    fn a_focus_culls_queued_entries_outside_the_ring_in_force() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let held = t0 + Duration::from_millis(40);
        let count = 1000;
        let queued = |state: &LoupeState, i: usize| {
            state.queue.iter().find(|e| e.index == i).map(|e| e.target)
        };
        let planted = |index, target, focus_origin| Entry {
            index,
            target,
            focus_origin,
            state: RequestState::Settled,
        };
        // A settled focus at 99 (the first focus is never transit): its ±2.
        let mut state = LoupeState::default();
        focus_on(&mut state, 99, FocusRequest::Long(8640), count, 1, t0);
        for i in 97..=101 {
            assert_eq!(queued(&state, i), Some(Target::Long(8640)), "settled {i}");
        }
        state
            .queue
            .insert(0, planted(117, Target::Long(8640), true));
        state.queue.insert(0, planted(96, Target::Long(8640), true));
        state
            .queue
            .insert(0, planted(140, Target::Long(1616), false));
        state.in_flight.push(120);
        // The key is held: 100 is 40 ms later, a transit ring 98..=115.
        focus_on(&mut state, 100, FocusRequest::Long(8640), count, 2, held);
        assert!(in_transit(&state, held), "the premise: a held key");
        for gone in [117, 96, 97] {
            assert_eq!(
                queued(&state, gone),
                None,
                "{gone} lies outside the ring in force, 98..=115: {:?}",
                state.queue
            );
        }
        for i in 98..=115 {
            assert_eq!(
                queued(&state, i),
                Some(Target::Long(MID_RUNG_TARGET)),
                "transit member {i}"
            );
        }
        assert_eq!(
            queued(&state, 140),
            Some(Target::Long(1616)),
            "a grid want keeps its own cull"
        );
        assert!(state.in_flight.contains(&120), "an in-flight decode lands");
        assert_eq!(queued(&state, 120), None, "and nothing is queued for it");

        // Clamped: a fit box and a clamp of 4 ahead. Settled at fit the ring
        // is 98..=115 at the box; `Z` (the same index, above fit, still
        // settled) makes the full-res ring 98..=104 the ring in force.
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let mut state = LoupeState {
            fit_box: Some(uhd),
            fullres_clamp: Some(4),
            ..Default::default()
        };
        focus_on(&mut state, 100, FocusRequest::Fit, count, 1, t0);
        for i in 98..=115 {
            assert_eq!(queued(&state, i), Some(Target::Fit(uhd)), "at fit {i}");
        }
        focus_on(
            &mut state,
            100,
            FocusRequest::Long(u32::MAX),
            count,
            2,
            held,
        );
        assert!(!in_transit(&state, held), "the premise: no index change");
        for i in 98..=104 {
            assert_eq!(
                queued(&state, i),
                Some(Target::Long(u32::MAX)),
                "the full-res ring {i}"
            );
        }
        for i in 105..=115 {
            assert_eq!(
                queued(&state, i),
                None,
                "{i} lies beyond the full-res ring's far end, 104"
            );
        }
    }

    /// Brief 008 A7 (raw-pipeline.md, "Revival"): a deferred upgrade revives
    /// only inside the RING IN FORCE — the ring the focus scheduled, 15 ahead
    /// at fit — and at no more than what its position asks for now, so during
    /// a hold above fit a frame the cursor is on or has passed revives at the
    /// fit box. Red with the gate left at ±`PREFETCH` (the trap
    /// raw-pipeline.md recorded: +7 dropped), with the revival at the stored
    /// target (the hold rows read full-res), and when the engine ignores its
    /// own clamp (105 revived).
    #[test]
    fn revival_gates_on_the_ring_in_force() {
        let now = std::time::Instant::now();
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let fit = Target::Fit(uhd);
        let top = Target::Long(u32::MAX);
        let ring_state = |fit_box, desired, transit: bool, clamp| {
            let mut state = stable_focus_state(100);
            state.fit_box = fit_box;
            state.desired = desired;
            state.fullres_clamp = clamp;
            state.travel_forward = true;
            state.moving = transit;
            state.last_index_change = Some(if transit {
                now
            } else {
                now - SETTLE_DEBOUNCE * 2
            });
            state
        };
        // What the queue holds for `index` after its revival, if revived.
        let revived = |state: &mut LoupeState, index, target, req| {
            revive_deferred(state, index, target, req, 1, 1000, now)
                .then(|| state.queue.iter().find(|e| e.index == index))
                .flatten()
                .map(|e| e.target)
        };
        for transit in [true, false] {
            for (index, want) in [
                (107, Some(fit)),
                (115, Some(fit)),
                (116, None),
                (98, Some(fit)),
                (97, None),
            ] {
                let mut state = ring_state(Some(uhd), fit, transit, None);
                assert_eq!(
                    revived(&mut state, index, fit, RequestState::Transit),
                    want,
                    "at fit, transit {transit}: {index}"
                );
            }
        }
        let long = Target::Long(8640);
        for (index, want) in [(102, Some(long)), (103, None)] {
            let mut state = ring_state(None, long, false, None);
            assert_eq!(
                revived(&mut state, index, long, RequestState::Settled),
                want,
                "no box, settled: {index}"
            );
        }
        for (index, want) in [(100, Some(fit)), (99, Some(fit)), (101, Some(top))] {
            let mut state = ring_state(Some(uhd), top, true, None);
            assert_eq!(
                revived(&mut state, index, top, RequestState::Transit),
                want,
                "a hold above fit: {index}"
            );
        }
        for transit in [false, true] {
            for (index, want) in [(104, Some(top)), (105, None)] {
                let mut state = ring_state(Some(uhd), top, transit, Some(4));
                assert_eq!(
                    revived(&mut state, index, top, RequestState::Settled),
                    want,
                    "above fit with a clamp of 4, transit {transit}: {index}"
                );
            }
        }
    }

    /// Brief 008 A13, its step-3 rows (raw-pipeline.md, "Above fit"): during
    /// a hold the focused frame and the members behind ask for the fit box,
    /// and at every focus of the hold the engine re-plans them WHATEVER the
    /// cache holds — a queued full-res entry for them is replaced by the
    /// fit-box request, or dropped when that rung is already in hand (the
    /// early return `schedule` takes for a served request would have left it,
    /// to be popped: a full-res decode for the frame the cursor is on). An
    /// in-flight decode lands; the settle afterwards asks for the top rung.
    #[test]
    fn a_hold_above_fit_asks_the_fit_box_for_the_focused_frame() {
        use RequestState::{Settled, Transit};
        let t0 = std::time::Instant::now();
        let hold = t0 + std::time::Duration::from_millis(40);
        let count = 1000;
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let fit = Target::Fit(uhd);
        let top = Target::Long(u32::MAX);
        let queued = |state: &LoupeState, i: usize| {
            state
                .queue
                .iter()
                .find(|e| e.index == i)
                .map(|e| (e.target, e.state))
        };
        let settled_at_100 = || {
            let mut state = LoupeState {
                fit_box: Some(uhd),
                ..Default::default()
            };
            focus_on(&mut state, 100, FocusRequest::Long(u32::MAX), count, 1, t0);
            state
        };
        let mut state = settled_at_100();
        for i in 98..=115 {
            assert_eq!(queued(&state, i), Some((top, Settled)), "settled {i}");
        }
        focus_on(
            &mut state,
            101,
            FocusRequest::Long(u32::MAX),
            count,
            2,
            hold,
        );
        assert!(in_transit(&state, hold), "the premise: a hold");
        for i in [99, 100, 101] {
            assert_eq!(
                queued(&state, i),
                Some((fit, Transit)),
                "{i}: the focused frame and the members behind ask the fit box"
            );
        }
        for i in 102..=116 {
            assert_eq!(queued(&state, i), Some((top, Transit)), "{i}: ahead");
        }

        // 101's fit-box rung is in hand while its full-res is still queued.
        let mut state = settled_at_100();
        let rung = FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 3240,
            height: 2160,
            kind: RungKind::Screen,
        };
        state.cache.insert(101, (rung, 0));
        focus_on(
            &mut state,
            101,
            FocusRequest::Long(u32::MAX),
            count,
            2,
            hold,
        );
        assert_eq!(
            queued(&state, 101),
            None,
            "the queued full-res for the frame the cursor is on is dropped: \
             its fit-box rung is in hand"
        );
        // Once the user stops, the reserved lane asks for the top rung.
        let later = hold + FOCUS_DEBOUNCE * 2;
        assert_eq!(
            next_job(&mut state, true, 0, 1000, later),
            Slot::Job(101, top, Settled),
            "the settle climbs the frame the hold left at its fit-box rung"
        );

        // 101 in flight at full-res: it lands; nothing is queued for it.
        let mut state = settled_at_100();
        state.queue.retain(|e| e.index != 101);
        state.in_flight.push(101);
        focus_on(
            &mut state,
            101,
            FocusRequest::Long(u32::MAX),
            count,
            2,
            hold,
        );
        assert!(state.in_flight.contains(&101), "an in-flight decode lands");
        assert_eq!(queued(&state, 101), None, "and nothing is queued for it");
    }

    /// `Z` to 1:1 and back to fit leaves no full-res decode queued at fit
    /// (raw-pipeline.md, "Above fit": the re-plan runs at every focus of an
    /// engine with a fit box; senior-developer review of brief 008 step 5).
    /// A 1:1 rest queues the full-res ring. Back at fit every position asks
    /// for the fit box, and a request the cache already serves never reaches
    /// `schedule`'s queue — so with the re-plan confined to a hold above fit,
    /// the full-res entry of every member whose screen rung was in hand stayed
    /// queued, and the next hold at fit popped them: full-res decodes of the
    /// frames the hold was on or had reached, each a 149 MB texture fill at
    /// fit (the 2026-08-01 shape, at fit). Red with the re-plan confined to a
    /// hold above fit, at the "back at fit" assertion (all eighteen still
    /// queued).
    #[test]
    fn z_and_back_to_fit_leaves_no_full_res_queued() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let count = 1000;
        let top = Target::Long(u32::MAX);
        let queued_full = |state: &LoupeState| -> Vec<usize> {
            let mut ids: Vec<usize> = state
                .queue
                .iter()
                .filter(|e| e.target == top)
                .map(|e| e.index)
                .collect();
            ids.sort_unstable();
            ids
        };
        // At fit on a 4K viewport, resting on 100, with the fit ring's
        // screen rungs in hand for 90..=130.
        let mut state = LoupeState {
            fit_box: Some(UHD),
            backlog_workers: 3,
            ..Default::default()
        };
        for i in 90..=130 {
            state.cache.insert(i, (screen_rung(), 0));
        }
        focus_on(&mut state, 100, FocusRequest::Fit, count, 1, t0);
        assert_eq!(
            queued_full(&state),
            Vec::<usize>::new(),
            "the premise: at fit nothing asks for full-res"
        );
        // `Z`: the 1:1 rest queues its full-res ring, 98..=115.
        let t1 = t0 + Duration::from_secs(2);
        focus_on(&mut state, 100, FocusRequest::Long(u32::MAX), count, 2, t1);
        assert_eq!(
            queued_full(&state),
            (98..=115).collect::<Vec<_>>(),
            "the premise: the 1:1 rest queued its full-res ring"
        );
        // `Z` back to fit a second later, before any worker popped.
        let t2 = t1 + Duration::from_secs(1);
        focus_on(&mut state, 100, FocusRequest::Fit, count, 3, t2);
        assert_eq!(
            queued_full(&state),
            Vec::<usize>::new(),
            "back at fit, no position asks for full-res"
        );
        // A three-key hold at fit, then a backlog worker pops: nothing
        // full-res.
        let h0 = t2 + Duration::from_secs(1);
        for (k, i) in [101usize, 102, 103].into_iter().enumerate() {
            let at = h0 + Duration::from_millis(40 * k as u64);
            focus_on(&mut state, i, FocusRequest::Fit, count, 4 + k as u64, at);
        }
        let popped = next_job(&mut state, false, 7, count, h0 + Duration::from_millis(90));
        assert!(
            !matches!(popped, Slot::Job(_, target, _) if target == top),
            "a backlog worker started a full-res decode at fit during a hold: {popped:?}"
        );
    }

    /// The 3840x2160 fit box the switch-rule tests hold above.
    const UHD: FitBox = FitBox {
        width: 3840,
        height: 2160,
    };

    /// A 3240x2160 screen rung: the 3/8 decode of a landscape A1 frame, which
    /// serves the 3840x2160 box.
    fn screen_rung() -> FullImage {
        FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 3240,
            height: 2160,
            kind: RungKind::Screen,
        }
    }

    /// A full-res A1 frame, 8640x5760 (its pixels a stand-in).
    fn full_frame() -> FullImage {
        FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 8640,
            height: 5760,
            kind: RungKind::Full,
        }
    }

    /// The state of a hold above fit whose last key landed on `cursor` at
    /// `key`, as `focus_on` leaves it: a 3840x2160 box, the app asking for the
    /// top rung, identity view, travelling forward, the whole fifteen ahead
    /// (no clamp), three backlog workers, all idle.
    fn holding_at(cursor: usize, key: std::time::Instant) -> LoupeState {
        LoupeState {
            fit_box: Some(UHD),
            desired: Target::Long(u32::MAX),
            focused: Some(cursor),
            focused_at: Some(key),
            focused_target: Target::Long(u32::MAX),
            last_index_change: Some(key),
            moving: true,
            travel_forward: true,
            backlog_workers: 3,
            ..Default::default()
        }
    }

    /// Brief 008 A13, rule 1 (raw-pipeline.md, "Above fit": "Step down once,
    /// at the decode"). A backlog worker is about to start a member's
    /// full-res decode during a hold above fit: when the cursor would reach
    /// the member — its distance ahead, COUNTED FROM 1, × the key period —
    /// before a full-res frame reaches the screen (the time-to-screen), that
    /// member and every member beyond it ask for the fit box. The popped
    /// entry decodes the fit box instead, the full-res entries queued beyond
    /// it become fit-box ones or are dropped where that rung is in hand, and
    /// a decode already in flight lands. When the frame can land first, and
    /// while the time-to-screen or the key period is unknown, the member
    /// decodes full-res and nothing is converted. A boundary in force holds at
    /// every later pop: a member beyond it decodes the fit box whatever its
    /// own timing says. Red when distances count from 0 (the 60 ms row then
    /// steps down: 1 × 40 < 60), and when a member beyond the boundary is
    /// judged by its own timing (108 then decodes full-res).
    #[test]
    fn the_switch_rule_steps_down_before_a_frame_it_cannot_land() {
        use std::time::Duration;
        use RequestState::Transit;
        let now = std::time::Instant::now();
        let fit = Target::Fit(UHD);
        let top = Target::Long(u32::MAX);
        let queued = |state: &LoupeState, i: usize| {
            state
                .queue
                .iter()
                .find(|e| e.index == i)
                .map(|e| (e.target, e.state))
        };
        // The cursor on 100 with 101 already sharp; the members 102..=115
        // queued for full-res, farthest first (102 at the back, popped
        // first), except 104, whose full-res decode is in flight; 110's
        // fit-box rung is in hand.
        let hold = |time_to_screen: Option<Duration>| {
            let mut state = holding_at(100, now);
            state.key_period = Some(Duration::from_millis(40));
            state.time_to_screen = time_to_screen;
            state.cache.insert(101, (full_frame(), 0));
            state.cache.insert(110, (screen_rung(), 0));
            state.in_flight.push(104);
            for index in (102..=115).rev().filter(|i| *i != 104) {
                state.queue.push(Entry {
                    index,
                    target: top,
                    focus_origin: true,
                    state: Transit,
                });
            }
            state
        };
        let beyond = || (103..=115).filter(|i| ![104, 110].contains(i));

        // 102 is two ahead: the cursor is there in 2 × 40 = 80 ms, before a
        // full-res frame's 100 ms to the screen.
        let mut state = hold(Some(Duration::from_millis(100)));
        assert_eq!(
            next_job(&mut state, false, 0, 1000, now),
            Slot::Job(102, fit, Transit),
            "the member the cursor reaches first decodes the fit box"
        );
        assert_eq!(state.switch.down, Some(102), "the boundary is that member");
        for i in beyond() {
            assert_eq!(
                queued(&state, i),
                Some((fit, Transit)),
                "{i}, beyond the boundary: the fit box"
            );
        }
        assert_eq!(
            queued(&state, 110),
            None,
            "110's fit-box rung is in hand: its queued full-res is dropped"
        );
        assert!(
            state.in_flight.contains(&104) && queued(&state, 104).is_none(),
            "104's full-res decode in flight lands, and nothing is queued for it"
        );

        // A full-res frame 60 ms from the screen lands before the cursor's
        // 80 ms of travel — the first member ahead is 1, not 0.
        let mut state = hold(Some(Duration::from_millis(60)));
        assert_eq!(
            next_job(&mut state, false, 0, 1000, now),
            Slot::Job(102, top, Transit),
            "a member whose full-res lands in time decodes full-res"
        );
        assert_eq!(state.switch, SwitchState::default(), "no step-down");
        for i in beyond().chain([110]) {
            assert_eq!(queued(&state, i), Some((top, Transit)), "{i}: untouched");
        }

        // Unknown time-to-screen (no full-res fill completed yet), or unknown
        // key period: full-res, nothing converted.
        let mut state = hold(None);
        assert_eq!(
            next_job(&mut state, false, 0, 1000, now),
            Slot::Job(102, top, Transit),
            "no time-to-screen yet"
        );
        assert_eq!(state.switch, SwitchState::default());
        let mut state = hold(Some(Duration::from_millis(100)));
        state.key_period = None;
        assert_eq!(
            next_job(&mut state, false, 0, 1000, now),
            Slot::Job(102, top, Transit),
            "no key period yet"
        );
        assert_eq!(state.switch, SwitchState::default());

        // ONE BOUNDARY: once set, it holds at every later pop. A full-res
        // entry for 108 — 8 ahead, 320 ms of travel against a 100 ms
        // time-to-screen, so on its own timing it would land in time — lies
        // beyond the boundary at 102: it decodes the fit box, and the boundary
        // does not move. Judged per pop instead, the members beyond a boundary
        // would ask full-res again whenever their own timing allowed — sharp
        // and soft by turns, the pumping rule 1's single step exists to stop.
        let mut state = holding_at(100, now);
        state.switch.down = Some(102);
        state.key_period = Some(Duration::from_millis(40));
        state.time_to_screen = Some(Duration::from_millis(100));
        state.queue.push(Entry {
            index: 108,
            target: top,
            focus_origin: true,
            state: Transit,
        });
        assert_eq!(
            next_job(&mut state, false, 0, 1000, now),
            Slot::Job(108, fit, Transit),
            "108 is beyond the boundary at 102: the fit box"
        );
        assert_eq!(
            state.switch.down,
            Some(102),
            "the boundary stays where it was"
        );
    }

    /// Brief 008 A13, rule 2 (raw-pipeline.md, "Above fit": "Step up only
    /// from a complete ring with a free decoder"), judged at a focus of the
    /// hold before it schedules anything, against the ring in force it is
    /// about to schedule. A hold stepped down at 102 moves on to 105 (its
    /// ring 103..=120): with every member ahead but the farthest, 120,
    /// holding its fit-box rung, nothing queued inside the ring and one of
    /// the three backlog workers busy elsewhere, the members beyond the far
    /// end ask for full-res again — 121 at the next focus. "Complete" reads
    /// as nothing of the ring still waiting to start (Manager ruling Q-H), so
    /// a member whose rung decode is in flight counts; a stale entry for the
    /// position that just fell three behind is not ring work. No step-up
    /// with a member's rung missing and nothing in flight for it — and that
    /// focus, still stepped down, asks the fit box of the member beyond the
    /// boundary — with ring work queued, with every backlog worker busy, or
    /// for a hold that never stepped down. Red when the step-up waits for
    /// nothing in flight anywhere, when it ignores the free worker, when every
    /// queued entry counts as ring work, when an in-flight member counts as
    /// missing (the literal clause Q-H replaced), when the plan's hold row
    /// ignores the step-down boundary (110 then asks full-res), and when a
    /// hold that never stepped down steps up.
    #[test]
    fn the_switch_rule_steps_up_only_from_a_complete_ring_with_a_free_decoder() {
        use std::time::Duration;
        use RequestState::Transit;
        let t = std::time::Instant::now();
        let key = t + Duration::from_millis(40);
        let fit = Target::Fit(UHD);
        let top = Target::Long(u32::MAX);
        let stepped_down = SwitchState {
            down: Some(102),
            up: None,
            locked: false,
        };
        let stepped_up = SwitchState {
            down: None,
            up: Some(120),
            locked: false,
        };
        let queued = |state: &LoupeState, i: usize| {
            state
                .queue
                .iter()
                .find(|e| e.index == i)
                .map(|e| (e.target, e.state))
        };
        // The cursor on 104, stepped down at 102; the rungs of 106..=119 in
        // hand; one backlog worker busy on a decode outside the ring.
        let hold_at_104 = || {
            let mut state = holding_at(104, t);
            state.switch = stepped_down;
            state.in_flight.push(90);
            state.backlog_busy = 1;
            for i in 106..=119 {
                state.cache.insert(i, (screen_rung(), 0));
            }
            state
        };
        let key_to_105 = |state: &mut LoupeState| {
            focus_on(state, 105, FocusRequest::Long(u32::MAX), 1000, 2, key);
            state.switch
        };

        let mut state = hold_at_104();
        assert_eq!(
            key_to_105(&mut state),
            stepped_up,
            "a complete ring with a free decoder steps up beyond its far end, 120"
        );
        assert_eq!(
            queued(&state, 120),
            Some((fit, Transit)),
            "up to the far end the members still ask for the fit box"
        );
        focus_on(
            &mut state,
            106,
            FocusRequest::Long(u32::MAX),
            1000,
            3,
            key + Duration::from_millis(40),
        );
        assert_eq!(
            queued(&state, 121),
            Some((top, Transit)),
            "the member entering beyond the far end asks for full-res again"
        );

        let mut state = hold_at_104();
        state.queue.push(Entry {
            index: 102,
            target: top,
            focus_origin: true,
            state: Transit,
        });
        assert_eq!(
            key_to_105(&mut state),
            stepped_up,
            "the previous focus's entry for 102, now three behind, is not ring work"
        );

        let mut state = hold_at_104();
        state.cache.remove(&119);
        state.in_flight.push(119);
        state.backlog_busy = 2;
        assert_eq!(
            key_to_105(&mut state),
            stepped_up,
            "119's rung decode is in flight: nothing of the ring waits to start"
        );

        let mut state = hold_at_104();
        state.cache.remove(&110);
        assert_eq!(
            key_to_105(&mut state),
            stepped_down,
            "110 has no rung and no decode in flight"
        );
        // Rule 1's one boundary, at a FOCUS: the hold is still stepped down at
        // 102, so this focus asks the fit box of 110 too, not only the pop
        // that set the boundary — or every focus of the hold would ask
        // full-res again of the members beyond it.
        assert_eq!(
            queued(&state, 110),
            Some((fit, Transit)),
            "stepped down at 102: 110, beyond the boundary, asks for the fit box"
        );

        let mut state = hold_at_104();
        state.queue.push(Entry {
            index: 112,
            target: fit,
            focus_origin: true,
            state: Transit,
        });
        assert_eq!(
            key_to_105(&mut state),
            stepped_down,
            "ring work waits in the queue"
        );

        let mut state = hold_at_104();
        state.backlog_busy = 3;
        assert_eq!(
            key_to_105(&mut state),
            stepped_down,
            "every backlog worker is busy"
        );

        // Only a hold that stepped DOWN steps up: one whose decoders have kept
        // up has no boundary to lift, whatever its ring and its workers say.
        // Stepping it "up" would move the full-res boundary a ring ahead, and
        // the member entering at the far end, 120, would ask the fit box.
        let mut state = hold_at_104();
        state.switch = SwitchState::default();
        assert_eq!(
            key_to_105(&mut state),
            SwitchState::default(),
            "no step-down, so no step-up"
        );
        assert_eq!(
            queued(&state, 120),
            Some((top, Transit)),
            "120, entering at the far end, asks for full-res"
        );
    }

    /// Brief 008 A13, rule 3 (raw-pipeline.md, "Above fit": "No pumping").
    /// After a step-up whose far end is 120 (full-res again from 121), a
    /// member 15 ahead of the cursor on 110 — 125, whose full-res would take
    /// 700 ms to the screen against the cursor's 15 × 40 = 600 — steps the
    /// hold down 4 positions past the step-up's boundary, fewer than one ring
    /// (15): the hold stays on the fit box until it ends, so a later focus
    /// that meets every step-up condition does not step up. The hold ends at
    /// a stop or at keys slower than four a second (`TRANSIT_GAP`), never at
    /// the end of `in_transit`: keys 200 ms apart are a hold whose every gap
    /// has a settled window (a refresh there runs settled), and the lock
    /// carries across it; a key 300 ms after the last ends the hold. Red with
    /// the lock removed, and with the state reset whenever a focus is not in
    /// transit (the previous plan's rule).
    #[test]
    fn a_quick_second_step_down_holds_the_rung_until_the_hold_ends() {
        use std::time::Duration;
        use RequestState::Transit;
        let t = std::time::Instant::now();
        let ms = |n: u64| t + Duration::from_millis(n);
        let top = Target::Long(u32::MAX);
        let locked = SwitchState {
            down: Some(125),
            up: Some(120),
            locked: true,
        };
        let mut state = holding_at(110, t);
        state.switch.up = Some(120);
        state.key_period = Some(Duration::from_millis(40));
        state.time_to_screen = Some(Duration::from_millis(700));
        state.queue.push(Entry {
            index: 125,
            target: top,
            focus_origin: true,
            state: Transit,
        });
        assert_eq!(
            next_job(&mut state, false, 0, 1000, t),
            Slot::Job(125, Target::Fit(UHD), Transit),
            "the cursor reaches 125 in 600 ms, before its full-res's 700"
        );
        assert_eq!(
            state.switch, locked,
            "125 is 4 past the step-up's boundary, 121: the hold locks"
        );

        // A later key meets every step-up condition — the members ahead but
        // the farthest hold their rungs or have them in flight (125), nothing
        // is queued, a backlog worker is free — and the lock keeps the rung.
        state.backlog_busy = 1; // 125's decode, as the worker loop counts it
        for i in 112..=124 {
            state.cache.insert(i, (screen_rung(), 0));
        }
        focus_on(
            &mut state,
            111,
            FocusRequest::Long(u32::MAX),
            1000,
            2,
            ms(40),
        );
        assert!(in_transit(&state, ms(40)), "the premise: a hold");
        assert_eq!(state.switch, locked, "locked: no step-up");

        // THE BAND: a refresh 170 ms after the last key runs settled, and the
        // next key comes 200 ms after the last — still a hold.
        focus_on(
            &mut state,
            111,
            FocusRequest::Long(u32::MAX),
            1000,
            3,
            ms(210),
        );
        assert!(
            !in_transit(&state, ms(210)),
            "the premise: settled between the keys"
        );
        focus_on(
            &mut state,
            112,
            FocusRequest::Long(u32::MAX),
            1000,
            4,
            ms(240),
        );
        assert!(state.moving, "the premise: a key 200 ms after the last");
        assert_eq!(
            state.switch, locked,
            "a settled window inside the hold does not end it"
        );

        // A key 300 ms after the last: the hold has ended.
        focus_on(
            &mut state,
            113,
            FocusRequest::Long(u32::MAX),
            1000,
            5,
            ms(540),
        );
        assert!(!state.moving, "the premise: a key 300 ms after the last");
        assert_eq!(
            state.switch,
            SwitchState::default(),
            "a stop, or keys slower than four a second, starts the rule afresh"
        );
    }

    /// Brief 008 A13, rule 3 at its edge (raw-pipeline.md, "Above fit": "When
    /// a step-down's boundary falls less than one ring (`RING_AHEAD` frames)
    /// beyond the last step-up's boundary"). The step-up's boundary is the
    /// first position beyond the ring's far end, while `SwitchState::up`
    /// stores the far end itself (a backward ring that reaches the folder's
    /// first frame has no position beyond it), so the lock's comparison is
    /// one off the spec's words by construction — the place an off-by-one
    /// would hide. After a step-up whose far end is 120 (full-res again from
    /// 121): a step-down at 135, 14 past 121, locks; one at 136, a whole ring
    /// (15) past it, does not. Red with the lock counted strictly less than a
    /// ring from the far end (135 then does not lock) and with it counted one
    /// further (136 then locks).
    #[test]
    fn the_lock_is_counted_from_the_first_position_beyond_the_far_end() {
        use std::time::Duration;
        let t = std::time::Instant::now();
        // The cursor on `cursor`, a full-res entry for `member`, 15 ahead,
        // popped: 15 × 40 ms of travel against a 10 s time-to-screen steps
        // the hold down at `member`.
        for (cursor, member, locks) in [(120, 135, true), (121, 136, false)] {
            let mut state = holding_at(cursor, t);
            state.switch.up = Some(120);
            state.key_period = Some(Duration::from_millis(40));
            state.time_to_screen = Some(Duration::from_secs(10));
            state.queue.push(Entry {
                index: member,
                target: Target::Long(u32::MAX),
                focus_origin: true,
                state: RequestState::Transit,
            });
            assert_eq!(
                next_job(&mut state, false, 0, 1000, t),
                Slot::Job(member, Target::Fit(UHD), RequestState::Transit),
                "the premise: {member} steps the hold down"
            );
            assert_eq!(state.switch.down, Some(member));
            assert_eq!(
                state.switch.locked,
                locks,
                "a step-down at {member}, {} past the step-up's boundary 121",
                member - 121
            );
        }
    }

    /// Brief 008 A13 (raw-pipeline.md, "Above fit": "A reversal starts the
    /// rule afresh"): a stepped-down, locked hold that reverses has its
    /// switch state reset at that index change — the ring now leans the
    /// other way — while a key in the same direction keeps it. Red with the
    /// reset on the latch's flip removed.
    #[test]
    fn a_reversal_starts_the_switch_rule_afresh() {
        let t = std::time::Instant::now();
        let key = t + std::time::Duration::from_millis(40);
        let locked = SwitchState {
            down: Some(125),
            up: Some(120),
            locked: true,
        };
        let at_110 = || {
            let mut state = holding_at(110, t);
            state.switch = locked;
            state
        };
        let mut state = at_110();
        note_focus(&mut state, 109, Target::Long(u32::MAX), key);
        assert!(
            state.moving && !state.travel_forward,
            "the premise: a held key, reversed"
        );
        assert_eq!(
            state.switch,
            SwitchState::default(),
            "a reversal starts the rule afresh"
        );
        let mut state = at_110();
        note_focus(&mut state, 111, Target::Long(u32::MAX), key);
        assert_eq!(state.switch, locked, "the lock holds within a hold");
    }

    /// Brief 008 A13 (raw-pipeline.md, "Above fit": rule 1 decides during a
    /// hold; "Settled and tapping, the whole full-res ring asks for
    /// full-res"). A hold that stepped down at 103 has stopped on 100, which
    /// is sharp. Nothing resets its switch state until the next index change,
    /// and its 40 ms key period against a 700 ms time-to-screen would step
    /// down every member on their timing — but the user is no longer
    /// travelling. The reserved lane asks for the settled ring (Manager ruling
    /// Q-I), and the backlog workers' pops of it decode full-res, nearest
    /// first, the boundary untouched: so a tap forward after the hold lands
    /// on a sharp frame (the persona's MUST-HAVE 3a). Red with rule 1's
    /// "during a hold" clause removed: the first pop, 101, steps the ring down
    /// at 101 and the members whose rung is in hand are dropped.
    #[test]
    fn a_stop_ends_the_step_down() {
        use std::time::Duration;
        let now = std::time::Instant::now();
        let top = Target::Long(u32::MAX);
        let mut state = holding_at(100, now - Duration::from_millis(300));
        state.switch = SwitchState {
            down: Some(103),
            up: None,
            locked: false,
        };
        state.key_period = Some(Duration::from_millis(40));
        state.time_to_screen = Some(Duration::from_millis(700));
        state.cache.insert(100, (full_frame(), 0));
        state.best_long.insert(100, 8640);
        for i in (98..=99).chain(101..=115) {
            state.cache.insert(i, (screen_rung(), 0));
        }
        assert_eq!(next_job(&mut state, true, 7, 1000, now), Slot::Wait);
        assert!(
            std::mem::take(&mut state.wake_backlog),
            "the premise: the lane asked for the settled ring"
        );
        let switch = state.switch;
        for expected in [101, 99, 102, 98, 103] {
            assert_eq!(
                next_job(&mut state, false, 7, 1000, now),
                Slot::Job(expected, top, RequestState::Settled),
                "{expected}: the stop's settled ring decodes at full-res"
            );
        }
        assert_eq!(state.switch, switch, "a pop at rest moves no boundary");
    }

    /// Brief 008 A13 (raw-pipeline.md, "Above fit", rule 1): the key period
    /// is the interval between the last two INDEX CHANGES — never between
    /// focus calls, since the app re-focuses the same index on every refresh,
    /// and never from the debounce clock `focused_at`, which a same-index
    /// escalation (`Z`) re-arms. Red when it is never written (rule 1 could
    /// then never fire), and when it is read off `focused_at`.
    #[test]
    fn the_key_period_is_the_interval_between_index_changes() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut state = LoupeState {
            fit_box: Some(UHD),
            ..Default::default()
        };
        note_focus(&mut state, 0, Target::Fit(UHD), t0);
        assert_eq!(state.key_period, None, "nothing before the first change");
        // A refresh of the same frame 20 ms later — `Z`, which escalates the
        // target and so re-arms the debounce clock.
        note_focus(&mut state, 0, Target::Long(u32::MAX), ms(20));
        assert_eq!(
            state.focused_at,
            Some(ms(20)),
            "the premise: the refresh moved the debounce clock"
        );
        note_focus(&mut state, 1, Target::Long(u32::MAX), ms(40));
        assert_eq!(
            state.key_period,
            Some(Duration::from_millis(40)),
            "from index change to index change"
        );
        note_focus(&mut state, 1, Target::Long(u32::MAX), ms(50));
        assert_eq!(
            state.key_period,
            Some(Duration::from_millis(40)),
            "a refresh leaves it"
        );
        note_focus(&mut state, 2, Target::Long(u32::MAX), ms(90));
        assert_eq!(state.key_period, Some(Duration::from_millis(50)));
    }

    /// Brief 008 A13 (raw-pipeline.md, "Above fit", rule 1; Manager ruling
    /// Q-K): the time-to-screen runs from a full-res decode's start to the
    /// app's report that its fill completed — whether the ring then held the
    /// texture or it was at once that ring's victim (it was ready to draw
    /// either way), and a frame the cursor has passed included. Only facts
    /// end a measurement: a completed fill (it measures), a culled fill and
    /// the box going (they do not); a re-wrap of a cached frame and another
    /// kind measure nothing. And a measurement exists only while the engine
    /// has a fit box: a decode published with no box, one STARTED with no
    /// box, and one during which the box went, open none. Red when an
    /// adoption leaves its stamp (a re-wrap re-measures), when a victim
    /// measures nothing, when `note_dropped` does nothing, when the box going
    /// keeps the stamps, when a stamp is taken without a box, when only the
    /// box at the publish is read (the decode's start ignored), and when the
    /// stamps are culled as their frame leaves the ring (fix round 2's cull,
    /// which censored the slow landings rule 1 exists to see).
    #[test]
    fn note_adopted_measures_a_full_res_frame_from_its_decode_start() {
        use std::time::Duration;
        use RungKind::{Full, Screen};
        let t0 = std::time::Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let took = |n: u64| Some(Duration::from_millis(n));
        /// A full-res decode of `index` that began at `at`, published now,
        /// under the box the state has had all along.
        fn published(state: &mut LoupeState, index: usize, at: std::time::Instant) {
            let start = DecodeStart::read(state, at);
            note_full_started(state, index, start);
        }
        let mut state = LoupeState {
            fit_box: Some(UHD),
            ..Default::default()
        };
        published(&mut state, 7, t0);
        adopted(&mut state, 7, Full, true, ms(250));
        assert_eq!(state.time_to_screen, took(250), "decode start to the fill");
        adopted(&mut state, 7, Full, true, ms(900));
        assert_eq!(
            state.time_to_screen,
            took(250),
            "a re-wrap of the cached frame measures nothing"
        );
        published(&mut state, 8, ms(1000));
        adopted(&mut state, 8, Screen, true, ms(1100));
        assert_eq!(
            state.time_to_screen,
            took(250),
            "another kind measures nothing"
        );

        // THE VICTIM: a fill its ring evicted at once still measures.
        published(&mut state, 9, ms(2000));
        adopted(&mut state, 9, Full, false, ms(2330));
        assert_eq!(
            state.time_to_screen,
            took(330),
            "a victim was ready to draw too"
        );

        // THE DROP: a culled fill ends its measurement, and a later re-wrap
        // from the cache measures nothing.
        published(&mut state, 90, ms(3000));
        dropped(&mut state, 90);
        adopted(&mut state, 90, Full, true, ms(6000));
        assert_eq!(
            state.time_to_screen,
            took(330),
            "a culled fill measures nothing"
        );

        // THE BOX GOING clears every open measurement.
        published(&mut state, 101, ms(7000));
        apply_fit_box(&mut state, None);
        adopted(&mut state, 101, Full, true, ms(7400));
        assert_eq!(
            state.time_to_screen,
            took(330),
            "a fill completing off the loupe measures nothing"
        );

        // NO BOX: a full-res frame decoded and published without one opens
        // nothing, and the box arriving afterwards does not measure it.
        published(&mut state, 102, ms(8000));
        apply_fit_box(&mut state, Some(UHD));
        adopted(&mut state, 102, Full, true, ms(8500));
        assert_eq!(
            state.time_to_screen,
            took(330),
            "a decode published with no box measures nothing"
        );

        // STARTED WITHOUT A BOX: a decode that began off the loupe and is
        // published after the box arrived — its time includes a time with no
        // box, which is not the loupe's.
        apply_fit_box(&mut state, None);
        let start = DecodeStart::read(&state, ms(9000));
        apply_fit_box(&mut state, Some(UHD));
        note_full_started(&mut state, 103, start);
        adopted(&mut state, 103, Full, true, ms(9400));
        assert_eq!(
            state.time_to_screen,
            took(330),
            "a decode started with no box measures nothing"
        );

        // THE BOX WENT DURING THE DECODE, and came back before its publish.
        let start = DecodeStart::read(&state, ms(10_000));
        apply_fit_box(&mut state, None);
        apply_fit_box(&mut state, Some(UHD));
        note_full_started(&mut state, 104, start);
        adopted(&mut state, 104, Full, true, ms(10_400));
        assert_eq!(
            state.time_to_screen,
            took(330),
            "a decode the box's going interrupted measures nothing"
        );

        // THE PASSED FRAME: 101's full-res started at t0 with the cursor on
        // 100; the hold moves on to 104 — 101 is three behind, outside the
        // ring in force — before its fill completes. It measures all the same:
        // the slow landing is the one rule 1 must see.
        let mut state = LoupeState {
            fit_box: Some(UHD),
            ..Default::default()
        };
        focus_on(&mut state, 100, FocusRequest::Long(u32::MAX), 1000, 1, t0);
        published(&mut state, 101, t0);
        for (stamp, index) in (2..).zip(101..=104) {
            let key = ms(40 * (index as u64 - 100));
            focus_on(
                &mut state,
                index,
                FocusRequest::Long(u32::MAX),
                1000,
                stamp,
                key,
            );
        }
        assert!(
            in_transit(&state, ms(160)) && state.focused == Some(104),
            "the premise: a hold, the cursor on 104, 101 outside its ring 102..=119"
        );
        adopted(&mut state, 101, Full, true, ms(400));
        assert_eq!(
            state.time_to_screen,
            took(400),
            "a frame the cursor has passed measures"
        );
    }

    /// Brief 008, Manager ruling Q-I (raw-pipeline.md, "The settled ring
    /// after a hold"): the app refreshes — and so asks for the settled ring
    /// with its next focus — only when something lands. A hold above fit
    /// that stops on a frame already sharp lands nothing, so the reserved
    /// lane, finding nothing to climb, asks for the settled ring itself:
    /// full-res for every member, in the ring's push order, and it wakes the
    /// backlog workers to decode it; on its next wake it asks nothing. When
    /// the frame still needs its climb the lane queues the climb and no
    /// member (the climb's landing refreshes the app, whose settled focus
    /// asks the ring); when a settled focus of the app's own has asked the
    /// ring, the lane asks nothing; an engine with no fit box keeps the
    /// behaviour before brief 008. Red with the lane's ask removed, with the
    /// ring asked beside the climb, and with the once-per-settle guard
    /// removed (the second wake asks again — at engine level, a lane that
    /// wakes the backlog at every wake).
    #[test]
    fn a_settle_with_nothing_to_climb_asks_for_the_settled_ring() {
        use RequestState::Settled;
        let now = std::time::Instant::now();
        let top = Target::Long(u32::MAX);
        // The state a stepped-down hold leaves 300 ms after its last key, on
        // 100: 100 sharp, the ring's members holding only their fit-box rungs,
        // nothing queued or in flight.
        let stopped = || {
            let mut state = holding_at(100, now - std::time::Duration::from_millis(300));
            // Sharp: its full-res in hand, and memoized as the file's best,
            // as the ladder memoizes a climb to the top rung that tops out.
            state.cache.insert(100, (full_frame(), 0));
            state.best_long.insert(100, 8640);
            for i in (98..=99).chain(101..=115) {
                state.cache.insert(i, (screen_rung(), 0));
            }
            state
        };
        let ring_in_push_order: Vec<Entry> = (103..=115)
            .rev()
            .chain([98, 102, 99, 101])
            .map(|index| Entry {
                index,
                target: top,
                focus_origin: true,
                state: Settled,
            })
            .collect();

        let mut state = stopped();
        assert_eq!(
            next_job(&mut state, true, 7, 1000, now),
            Slot::Wait,
            "nothing for the lane itself: 100 is sharp, and above fit there is no idle cook"
        );
        assert!(
            std::mem::take(&mut state.wake_backlog),
            "the lane asked for the ring and wakes the backlog to decode it"
        );
        assert_eq!(
            state.queue, ring_in_push_order,
            "the settled ring, full-res, farthest first"
        );
        assert_eq!(next_job(&mut state, true, 7, 1000, now), Slot::Wait);
        assert!(
            !state.wake_backlog,
            "once per settle: the next wake asks nothing"
        );
        assert_eq!(state.queue, ring_in_push_order);

        let mut state = stopped();
        state.cache.insert(100, (screen_rung(), 0));
        state.best_long.remove(&100);
        assert_eq!(
            next_job(&mut state, true, 7, 1000, now),
            Slot::Job(100, top, Settled),
            "100 needs its climb"
        );
        assert!(
            !state.wake_backlog && state.queue.is_empty(),
            "no member asked beside the climb: {:?}",
            state.queue
        );

        let mut state = stopped();
        state.fit_box = None;
        assert_eq!(next_job(&mut state, true, 7, 1000, now), Slot::Wait);
        assert!(
            !state.wake_backlog && state.queue.is_empty(),
            "no fit box: the behaviour before brief 008"
        );

        let mut state = stopped();
        focus_on(&mut state, 100, FocusRequest::Long(u32::MAX), 1000, 7, now);
        assert_eq!(
            state.queue, ring_in_push_order,
            "the premise: the app's settled focus asked for the ring"
        );
        assert_eq!(next_job(&mut state, true, 7, 1000, now), Slot::Wait);
        assert!(
            !state.wake_backlog,
            "a settled focus of the app's own asked it: the lane asks nothing"
        );
        assert_eq!(state.queue, ring_in_push_order);
    }

    /// Brief 008, Manager ruling Q-I (raw-pipeline.md, "The settled ring after
    /// a hold": the lane asks "once per settle"): the guard that keeps the lane
    /// from asking twice is cleared at every index change, so each settle
    /// asks its own ring — never once per session. After the lane has asked
    /// 100's ring, 101 and 102 turn sharp; a new hold's first key lands on 101
    /// (a settled focus, which asks 101's ring itself) and a held key on 102;
    /// the stop on 102, sharp already, lands nothing, so the lane asks 102's
    /// ring. Red with `note_focus`'s clearing of the guard removed: 101's
    /// settled focus set it, the key onto 102 leaves it set, and 102's ring is
    /// never asked — a tap forward after that hold would land soft.
    #[test]
    fn each_settle_after_a_hold_asks_its_own_ring() {
        use std::time::Duration;
        let now = std::time::Instant::now();
        let ms = |n: u64| now + Duration::from_millis(n);
        let top = FocusRequest::Long(u32::MAX);
        let mut state = holding_at(100, now - Duration::from_millis(300));
        state.cache.insert(100, (full_frame(), 0));
        state.best_long.insert(100, 8640);
        for i in (98..=99).chain(101..=115) {
            state.cache.insert(i, (screen_rung(), 0));
        }
        assert_eq!(next_job(&mut state, true, 7, 1000, now), Slot::Wait);
        assert!(
            std::mem::take(&mut state.wake_backlog),
            "the premise: the lane asked 100's ring"
        );
        for i in [101, 102] {
            state.cache.insert(i, (full_frame(), 0));
            state.best_long.insert(i, 8640);
        }
        focus_on(&mut state, 101, top, 1000, 8, ms(40));
        focus_on(&mut state, 102, top, 1000, 9, ms(80));
        assert!(
            in_transit(&state, ms(80)),
            "the premise: a held key onto 102"
        );
        assert_eq!(next_job(&mut state, true, 9, 1000, ms(380)), Slot::Wait);
        assert!(
            state.wake_backlog,
            "the settle on 102 asks 102's ring: {:?}",
            state.queue.iter().map(|e| e.index).collect::<Vec<_>>()
        );
    }

    /// Brief 008, Manager ruling Q-I and rule 2's free worker (raw-pipeline.md,
    /// "The settled ring after a hold"; "Above fit", rule 2), on the engine's
    /// real `worker` threads over twenty synthetic RAWs: the two duties the
    /// worker loop carries for the switch rule, which no clock-free row can
    /// see.
    /// - A hold above fit has stopped on frame 5, already sharp, its ring
    ///   holding only screen rungs: nothing lands, so the reserved lane asks
    ///   for the settled ring — under the state lock, so it cannot wake anyone
    ///   itself — and the worker loop wakes the backlog workers, which wait
    ///   with no timeout. Every member then decodes full-res with no further
    ///   focus from the app.
    /// - Each backlog flight counts its worker busy while it decodes (rule 2's
    ///   "a backlog worker is free") and gives it back when it ends.
    /// - The reserved lane's own flight is never counted as a backlog
    ///   worker's (raw-pipeline.md, "The decode workers": the decoders are
    ///   the backlog workers AND the lane). The second phase takes frame 5's
    ///   full away, so the lane's settle guarantee climbs it, and reads the
    ///   count while that flight runs and after it ends: both must be 0.
    ///
    /// Red with the loop's wake removed (no member decodes before the 30 s
    /// deadline: the backlog workers sleep until an event that never comes),
    /// with the busy count's decrement removed (16 after 16 flights, and rule
    /// 2 never steps up again), with its increment removed (never busy), and
    /// with the increment's `!focus_reserved` guard removed while the
    /// decrement keeps its own — every lane flight then adds a busy worker
    /// that is never given back, so after as many lane flights as there are
    /// backlog workers rule 2 finds no free worker for the rest of the session
    /// (`(1, 1)` against `(0, 0)` on the second phase). With both guards
    /// removed the lane is counted for exactly its own flight, which the
    /// second phase's sampled reading also sees (`(0, 1)`). With only the
    /// decrement's guard removed the count reads one low while a backlog
    /// flight overlaps a lane flight and never drifts: benign, and green here.
    /// The 200 ms sleep only orders the backlog workers' first wait before the
    /// lane asks: on a correct build it cannot make the test fail, and under an
    /// extreme stall it could only let the missing wake read green. The busy
    /// count is read under the state lock every millisecond while sixteen
    /// decodes run on two workers, so a correct build reads it busy unless
    /// this thread is kept off the CPU for all sixteen.
    #[test]
    fn the_lane_wakes_the_backlog_and_every_flight_frees_its_worker() {
        use std::time::{Duration, Instant};
        let dir = crate::testutil::scratch_dir("lane-wakes-backlog");
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let paths: Vec<PathBuf> = (0..20)
            .map(|i| {
                let path = dir.join(format!("f{i:02}.arw"));
                std::fs::write(&path, raw_with_full(&full)).unwrap();
                path
            })
            .collect();
        let (shared, rx) = shared_over(paths);
        let shared = Arc::new(shared);
        let image = |width, height, kind| FullImage {
            rgb: Arc::new(vec![0; 3]),
            width,
            height,
            kind,
        };
        {
            let mut state = lock(&shared);
            *state = holding_at(5, Instant::now() - Duration::from_millis(300));
            state.fit_box = Some(FitBox {
                width: 800,
                height: 600,
            });
            state.backlog_workers = 2;
            // 5 is sharp: its full in hand, memoized as the file's best.
            state
                .cache
                .insert(5, (image(2000, 1500, RungKind::Full), 0));
            state.best_long.insert(5, 2000);
            // The members hold the screen rungs the hold asked for.
            for i in (3..=4).chain(6..=19) {
                state
                    .cache
                    .insert(i, (image(800, 600, RungKind::Screen), 0));
            }
            state.cached_bytes = state.cache.values().map(|(i, _)| i.rgb.len()).sum();
        }
        let mut threads = Vec::new();
        for _ in 0..2 {
            let shared = Arc::clone(&shared);
            threads.push(std::thread::spawn(move || worker(&shared, false)));
        }
        // The backlog workers find nothing and wait, untimed, before the lane
        // starts: only the lane's wake can bring them to the ring it asks.
        std::thread::sleep(Duration::from_millis(200));
        let lane = Arc::clone(&shared);
        threads.push(std::thread::spawn(move || worker(&lane, true)));

        let members: std::collections::BTreeSet<usize> = (3..=4).chain(6..=19).collect();
        let mut landed = std::collections::BTreeSet::new();
        let mut busy_seen = 0;
        let deadline = Instant::now() + Duration::from_secs(30);
        while landed != members {
            busy_seen = busy_seen.max(lock(&shared).backlog_busy);
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left.min(Duration::from_millis(1))) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !left.is_zero() => continue,
                Ok(LoupeEvent::Ready { index, image, .. }) if image.kind == RungKind::Full => {
                    landed.insert(index);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // A flight frees its worker and its in-flight slot under one lock, so
        // once nothing is in flight every flight has ended.
        let settle = Instant::now() + Duration::from_secs(30);
        while !lock(&shared).in_flight.is_empty() && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(10));
        }
        let busy = lock(&shared).backlog_busy;
        // The reserved lane's own flight is not a backlog worker's: 5 loses
        // its full, so the lane's settle guarantee climbs it, and rule 2's
        // count must not move — a lane flight counted and never given back
        // would leave rule 2 with no free worker after as many stops as there
        // are backlog workers.
        {
            let mut state = lock(&shared);
            if let Some((img, _)) = state.cache.remove(&5) {
                state.cached_bytes -= img.rgb.len();
            }
            state.best_long.remove(&5);
        }
        shared.wakeup.notify_all();
        let mut lane_busy_seen = 0;
        let mut lane_landed = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !lane_landed {
            lane_busy_seen = lane_busy_seen.max(lock(&shared).backlog_busy);
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left.min(Duration::from_millis(1))) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !left.is_zero() => continue,
                Ok(LoupeEvent::Ready {
                    index: 5, image, ..
                }) if image.kind == RungKind::Full => {
                    lane_landed = true;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let settle = Instant::now() + Duration::from_secs(30);
        while !lock(&shared).in_flight.is_empty() && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(10));
        }
        let lane_busy = lock(&shared).backlog_busy;
        // Stop the workers as `LoupeEngine`'s drop does: taking the lock
        // between the flag and the wake-up means no worker is between its
        // check of the flag and its wait, where the wake-up would be lost.
        shared.shutdown.store(true, Ordering::SeqCst);
        drop(lock(&shared));
        shared.wakeup.notify_all();
        for thread in threads {
            thread.join().unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(
            landed, members,
            "the lane's settled ring reached the backlog workers: every member decoded full-res"
        );
        assert_eq!(busy, 0, "every backlog flight gave its worker back");
        assert!(
            busy_seen > 0,
            "a backlog flight counts its worker busy while it decodes"
        );
        assert!(lane_landed, "the premise: the lane climbed 5 to full-res");
        assert_eq!(
            (lane_busy, lane_busy_seen),
            (0, 0),
            "the reserved lane's flight never counts as a backlog worker's"
        );
    }

    /// Brief 008 A13 (raw-pipeline.md, "Above fit"): a simulated 800-key hold
    /// at 1:1 never starts a full-res decode for the frame the cursor is on
    /// or one it has passed (the 2026-08-01 finding, ui-grid.md History:
    /// such decodes swamped a hold before transit existed).
    ///
    /// Everything that decides is the engine's own code — `focus_on` at every
    /// key, `next_job` at every pop (the switch rule's step-down included),
    /// `revive_deferred` at every landing, and the time-to-screen's stamp and
    /// its two reports — and the app is modelled with the real functions it
    /// will run: a kitchen of ONE worker that cooks a full-res fill in 60 ms
    /// in `transit::next_fill`'s order, over the full-res window the engine
    /// hands the app (`windows_of`, the body of `texture_windows`); at every
    /// key the fills outside that window are culled and each reported
    /// (`note_dropped`); a completed fill enters a full-res texture ring
    /// through `transit::evict_ring`, and is reported (`note_adopted`) held or
    /// its ring's own victim — so a frame that lands behind the cursor is
    /// measured exactly as the app will measure it. Only the clock (a
    /// 1 ms virtual step), the three backlog workers' loop (a pop does
    /// `backlog_busy += 1`, a flight's end `-= 1`, as `worker` does) and the
    /// decode times — mid 5 ms, screen rung 120 ms, full-res 200 ms, the
    /// laptop's order of magnitude — are simulated; decodes land in the cache
    /// as `publish` lands them (the full-res stamped from its decode's start),
    /// with stand-in pixels, so the LRU never evicts.
    ///
    /// From a rest: the cursor on 0, sharp, the fifteen ahead holding the
    /// fit-box rungs of an earlier pass at fit, and a settled focus above fit
    /// that queued their full-res; then 800 keys 40 ms apart. Asserted at
    /// every full-res job: the frame is strictly ahead of the cursor at that
    /// instant. And, before anything is printed, that the run measured a
    /// time-to-screen and stepped down at least once: with 200 ms decodes
    /// against 40 ms keys on three workers the rule must step down on a
    /// correct build, so a run that never does is a harness that stopped
    /// measuring. The counts it prints — full-res starts (those started
    /// before the first measurement: the spec's unjudged decodes), step-downs,
    /// step-ups, locks, fills completed and culled — are a record for brief
    /// 008, not a gate. Red with the focused frame asking the top rung during
    /// a hold, with the hold's re-plan skipping a frame whose fit-box rung is
    /// cached, and with the time-to-screen stamps culled as their frame
    /// leaves the ring (then it never measures).
    #[test]
    fn a_hold_above_fit_never_starts_a_full_res_decode_the_cursor_has_reached() {
        use std::collections::VecDeque;
        use std::time::Duration;
        const COUNT: usize = 1000;
        const KEYS: usize = 800;
        let key = Duration::from_millis(40);
        let (mid, rung, full, fill) = (
            Duration::from_millis(5),
            Duration::from_millis(120),
            Duration::from_millis(200),
            Duration::from_millis(60),
        );
        /// One backlog worker's decode: what it will publish, and when.
        struct Flight {
            index: usize,
            landings: VecDeque<(std::time::Instant, FullImage)>,
            full_started: DecodeStart,
        }
        #[derive(Debug, Default)]
        struct Counts {
            full_starts: usize,
            unjudged: usize,
            step_downs: usize,
            step_ups: usize,
            locks: usize,
            held: usize,
            victims: usize,
            culled: usize,
        }
        let t0 = std::time::Instant::now();
        let view: Vec<usize> = (0..COUNT).collect();
        let mut state = LoupeState {
            fit_box: Some(UHD),
            backlog_workers: 3,
            ..Default::default()
        };
        state.cache.insert(0, (full_frame(), 0));
        state.best_long.insert(0, 8640);
        for i in 1..=15 {
            state.cache.insert(i, (screen_rung(), 0));
        }
        state.cached_bytes = state.cache.values().map(|(image, _)| image.rgb.len()).sum();
        let mut stamp = 1;
        focus_on(
            &mut state,
            0,
            FocusRequest::Long(u32::MAX),
            COUNT,
            stamp,
            t0,
        );
        assert_eq!(
            state.queue.len(),
            15,
            "the premise: the rest queued the fifteen ahead for full-res"
        );

        let mut counts = Counts::default();
        let mut flights: Vec<Option<Flight>> = (0..3).map(|_| None).collect();
        let mut fills: Vec<usize> = Vec::new();
        let mut cooking: Option<(usize, std::time::Instant)> = None;
        let mut textures: Vec<usize> = Vec::new();
        let mut next_key = 1;
        let end = t0 + key * (KEYS as u32 + 1);
        let mut now = t0;
        while now <= end {
            let elapsed = now.duration_since(t0).as_millis();
            // The decodes land, as `publish` lands them, and each flight's
            // end is what `worker` does once the ladder returns.
            for slot in &mut flights {
                let Some(flight) = slot.as_mut() else {
                    continue;
                };
                while flight.landings.front().is_some_and(|(at, _)| *at <= now) {
                    let (_, image) = flight.landings.pop_front().expect("a landing");
                    let is_full = image.kind == RungKind::Full;
                    if let Some((old, _)) = state.cache.remove(&flight.index) {
                        state.cached_bytes -= old.rgb.len();
                    }
                    state.cached_bytes += image.rgb.len();
                    if is_full {
                        note_full_started(&mut state, flight.index, flight.full_started);
                        // A climb to the top rung tops out at the full: the
                        // ladder memoizes it as the file's best (`note_best`).
                        state.best_long.insert(flight.index, 8640);
                        // The app queues a full-res fill for every full landing.
                        fills.push(flight.index);
                    }
                    state.cache.insert(flight.index, (image, stamp));
                }
                if flight.landings.is_empty() {
                    let index = flight.index;
                    *slot = None;
                    state.backlog_busy -= 1;
                    state.in_flight.retain(|i| *i != index);
                    if let Some((target, req)) = state.deferred.remove(&index) {
                        revive_deferred(&mut state, index, target, req, stamp, COUNT, now);
                    }
                }
            }
            // The kitchen's fill completes: into the full-res texture ring,
            // and reported held or as the ring's own victim.
            if let Some((index, done)) = cooking {
                if done <= now {
                    cooking = None;
                    let cursor = state.focused.expect("a focus");
                    let window = windows_of(&state).full;
                    textures.retain(|i| *i != index);
                    textures.push(index);
                    while let Some(victim) =
                        crate::transit::evict_ring(&textures, cursor, &view, window)
                    {
                        textures.remove(victim);
                    }
                    let held = textures.contains(&index);
                    if held {
                        counts.held += 1;
                    } else {
                        counts.victims += 1;
                    }
                    adopted(&mut state, index, RungKind::Full, held, now);
                }
            }
            // A key: the engine's focus, then the app's refresh, which culls
            // the queued fills outside the full-res window and reports each.
            if next_key <= KEYS && now >= t0 + key * next_key as u32 {
                stamp += 1;
                let up_before = state.switch.up;
                focus_on(
                    &mut state,
                    next_key,
                    FocusRequest::Long(u32::MAX),
                    COUNT,
                    stamp,
                    now,
                );
                if state.switch.up.is_some() && state.switch.up != up_before {
                    counts.step_ups += 1;
                }
                let window = windows_of(&state).full;
                let cursor = next_key;
                fills.retain(|&i| {
                    let keep = i == cursor || window.contains(cursor, i);
                    if !keep {
                        dropped(&mut state, i);
                        counts.culled += 1;
                    }
                    keep
                });
                next_key += 1;
            }
            // The kitchen starts its next fill, the one the cursor meets first.
            if cooking.is_none() {
                let cursor = state.focused.expect("a focus");
                let window = windows_of(&state).full;
                if let Some(slot) = crate::transit::next_fill(&fills, cursor, &view, window) {
                    cooking = Some((fills.remove(slot), now + fill));
                }
            }
            // Idle backlog workers pop.
            for slot in flights.iter_mut().filter(|f| f.is_none()) {
                let before = state.switch;
                let job = next_job(&mut state, false, stamp, COUNT, now);
                if state.switch.down.is_some() && state.switch.down != before.down {
                    counts.step_downs += 1;
                }
                if state.switch.locked && !before.locked {
                    counts.locks += 1;
                }
                let Slot::Job(index, target, _) = job else {
                    break;
                };
                state.backlog_busy += 1;
                let cursor = state.focused.expect("a focus");
                if matches!(target, Target::Long(_)) {
                    assert!(
                        index > cursor,
                        "at {elapsed} ms a full-res decode started for {index} with the cursor \
                         on {cursor}: {counts:?}"
                    );
                    counts.full_starts += 1;
                    if state.time_to_screen.is_none() {
                        counts.unjudged += 1;
                    }
                }
                // The ladder: the mid first when nothing is in hand, then the
                // rung the target asks for.
                let have = state
                    .cache
                    .get(&index)
                    .map_or(0, |(image, _)| image.width.max(image.height));
                let mut at = now;
                let mut landings = VecDeque::new();
                if have < 1616 {
                    at += mid;
                    landings.push_back((
                        at,
                        FullImage {
                            rgb: Arc::new(vec![0; 3]),
                            width: 1616,
                            height: 1080,
                            kind: RungKind::Mid,
                        },
                    ));
                }
                let full_started = DecodeStart::read(&state, at);
                match target {
                    Target::Long(_) => {
                        at += full;
                        landings.push_back((at, full_frame()));
                    }
                    Target::Fit(_) => {
                        at += rung;
                        landings.push_back((at, screen_rung()));
                    }
                }
                *slot = Some(Flight {
                    index,
                    landings,
                    full_started,
                });
            }
            now += Duration::from_millis(1);
        }
        assert!(
            state.time_to_screen.is_some() && counts.step_downs > 0,
            "the simulation never measured a time-to-screen or never stepped down — with \
             200 ms decodes against 40 ms keys on three workers a correct build must: {counts:?}"
        );
        println!(
            "MEASURED a13 simulation: {KEYS} keys at 40 ms, 3 backlog workers (full-res 200 ms, \
             rung 120 ms, mid 5 ms), one kitchen worker (60 ms a fill): full-res starts {} ({} \
             before the first measurement), step-downs {}, step-ups {}, locks {}, fills completed \
             {} (held {}, victims {}), fills culled {}, time-to-screen at the end {:?}",
            counts.full_starts,
            counts.unjudged,
            counts.step_downs,
            counts.step_ups,
            counts.locks,
            counts.held + counts.victims,
            counts.held,
            counts.victims,
            counts.culled,
            state.time_to_screen
        );
    }

    /// Brief 008 R9 (raw-pipeline.md, "The idle cook"): settled at fit on a
    /// WIDE viewport with the cursor's screen rung in hand, the reserved
    /// lane's next job is the cursor's full-res, so `Z` after a stop finds it
    /// cooked or cooking. None while moving, in flight, without a box, on a
    /// box the reference mid serves (a 3/8 rung left over from a 4K box, on a
    /// 1080p one), for a request at a box other than the engine's current one,
    /// or once the file's best is in hand; with only a mid the settle
    /// guarantee climbs first, and a queued entry is taken as it is.
    #[test]
    fn the_reserved_lane_cooks_the_cursors_full_at_fit_on_a_wide_viewport() {
        use RequestState::{Settled, Transit};
        let now = std::time::Instant::now();
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let hd = FitBox {
            width: 1920,
            height: 1080,
        };
        let top = Target::Long(u32::MAX);
        let image = |width, height, kind| FullImage {
            rgb: Arc::new(vec![0; 3]),
            width,
            height,
            kind,
        };
        let rung = image(3240, 2160, RungKind::Screen);
        let at_rest = |fit_box: Option<FitBox>, desired, cached: &FullImage| {
            let mut state = stable_focus_state(4);
            state.fit_box = fit_box;
            state.desired = desired;
            state.last_index_change = Some(now - SETTLE_DEBOUNCE * 2);
            state.cache.insert(4, (cached.clone(), 0));
            state
        };
        let mut state = at_rest(Some(uhd), Target::Fit(uhd), &rung);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Job(4, top, Settled),
            "the cursor's full-res behind a stop at fit on 4K"
        );
        let mut state = at_rest(Some(uhd), Target::Fit(uhd), &rung);
        state.last_index_change = Some(now);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "moving"
        );
        let mut state = at_rest(Some(uhd), Target::Fit(uhd), &rung);
        state.in_flight.push(4);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "in flight"
        );
        let mut state = at_rest(None, Target::Fit(uhd), &rung);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "no box"
        );
        let mut state = at_rest(
            Some(uhd),
            Target::Fit(uhd),
            &image(1616, 1080, RungKind::Mid),
        );
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Job(4, Target::Fit(uhd), Settled),
            "only a mid: the settle guarantee climbs to the box first"
        );
        let mut state = at_rest(Some(hd), Target::Fit(hd), &rung);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "a viewport the reference mid serves never cooks, whatever rung is cached"
        );
        // A stale request: the engine's box is 4K, but the app last asked for
        // a 1080p box (a resize before the next focus, or a box that arrived
        // after a box-less `focus_fit`). The rung serves that request, so the
        // settle guarantee has nothing to climb; the cook is for the CURRENT
        // box only (the step-3 review's F2: the `desired` clause had no red).
        let mut state = at_rest(Some(uhd), Target::Fit(hd), &rung);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "a request at a box the engine no longer has cooks nothing"
        );
        let mut state = at_rest(Some(uhd), Target::Fit(uhd), &rung);
        state.best_long.insert(4, 3240);
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Wait,
            "the file's best is in hand"
        );
        let mut state = at_rest(Some(uhd), Target::Fit(uhd), &rung);
        state.queue.push(Entry {
            index: 4,
            target: Target::Long(8640),
            focus_origin: true,
            state: Transit,
        });
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Job(4, Target::Long(8640), Transit),
            "a queued entry is taken as it is, no cook"
        );
    }

    /// Brief 008 A4 (raw-pipeline.md, "The decode workers"): the engine
    /// spawns as many workers as it is given, at least two, the LAST of them
    /// the focus-reserved lane — named so, from the same flag that makes it
    /// the lane; `start` keeps three whatever the machine. Red with a fixed
    /// three, and with the lane's flag read off a fixed index.
    #[test]
    fn start_with_spawns_the_decoders_and_reserves_the_last() {
        let names = |engine: &LoupeEngine| -> Vec<String> {
            engine
                .workers
                .iter()
                .map(|w| w.thread().name().unwrap_or_default().to_owned())
                .collect()
        };
        let (engine, _events) = LoupeEngine::start_with(Vec::new(), 1, 5);
        assert_eq!(
            names(&engine),
            [
                "fastcull-loupe-0",
                "fastcull-loupe-1",
                "fastcull-loupe-2",
                "fastcull-loupe-3",
                "fastcull-loupe-reserved"
            ]
        );
        drop(engine);
        let (engine, _events) = LoupeEngine::start(Vec::new(), 1);
        assert_eq!(
            names(&engine),
            [
                "fastcull-loupe-0",
                "fastcull-loupe-1",
                "fastcull-loupe-reserved"
            ]
        );
        drop(engine);
        let (engine, _events) = LoupeEngine::start_with(Vec::new(), 1, 1);
        assert_eq!(
            names(&engine),
            ["fastcull-loupe-0", "fastcull-loupe-reserved"],
            "at least one backlog worker beside the lane"
        );
    }

    /// Brief 008 (ui-grid.md, "The render ladder"; raw-pipeline.md, "The
    /// ring"): the engine hands the app the windows of its two texture rings,
    /// leaned by the engine's own latch — the screen-rung ring's is the ring,
    /// the full-res ring's the full-res ring as the cache clamps it, and with
    /// no fit box the settled ±2 ring. Red when the latch is ignored (the
    /// backward row) and when the clamp is (the 2 GiB row).
    #[test]
    fn the_engine_hands_the_app_its_leaned_windows() {
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let window = |before, after| RingWindow { before, after };
        let (engine, _events) = LoupeEngine::start_with(Vec::new(), 8 << 30, 3);
        engine.set_fit_box(Some(uhd));
        lock(&engine.shared).travel_forward = true;
        assert_eq!(
            engine.texture_windows(),
            TextureWindows {
                rung: window(2, 15),
                full: window(2, 15),
            },
            "forward"
        );
        lock(&engine.shared).travel_forward = false;
        assert_eq!(
            engine.texture_windows(),
            TextureWindows {
                rung: window(15, 2),
                full: window(15, 2),
            },
            "backward"
        );
        drop(engine);
        let (engine, _events) = LoupeEngine::start_with(Vec::new(), 2 << 30, 3);
        engine.set_fit_box(Some(uhd));
        lock(&engine.shared).travel_forward = true;
        assert_eq!(
            engine.texture_windows().full,
            window(2, 3),
            "a 2 GiB cache's full-res ring: 3 ahead"
        );
        engine.set_fit_box(None);
        assert_eq!(
            engine.texture_windows().full,
            RingWindow::symmetric(PREFETCH),
            "no box: the settled ±2 ring, the app's texture ring before brief 008"
        );
    }

    /// Brief 008 step-6 review F1 (ui-grid.md, "Virtualization";
    /// 01-architecture.md, the kitchen): a window's `span` is exactly the
    /// positions `contains` says yes to, clamped at both ends of the view —
    /// swept over every cursor and position of a 40-frame view for a forward,
    /// a backward and a symmetric window, and empty when the cursor is not in
    /// the view — and `nearest_first` meets them in the engine's decode
    /// order: the cursor first, then the nearest, at equal distance the one
    /// toward the lean first. Red when the far end is not clamped (the
    /// far-end row), when the near end is not (the sweep), when the lean is
    /// ignored (the backward tie row) and when the cursor is not first (the
    /// sweep).
    #[test]
    fn a_ring_window_spans_its_positions_and_meets_them_nearest_first() {
        let forward = RingWindow::leaning(2, 15, true);
        let backward = RingWindow::leaning(2, 15, false);
        let even = RingWindow::symmetric(2);
        for window in [forward, backward, even] {
            for cursor in 0..40 {
                let span = window.span(cursor, 40);
                for pos in 0..40 {
                    assert_eq!(
                        span.contains(&pos),
                        window.contains(cursor, pos),
                        "{window:?} around {cursor}: position {pos}"
                    );
                }
                let mut met = window.nearest_first(cursor, 40);
                assert_eq!(met.first(), Some(&cursor), "{window:?}: the cursor first");
                met.sort_unstable();
                assert_eq!(
                    met,
                    span.collect::<Vec<_>>(),
                    "{window:?}: the span, once each"
                );
            }
            assert!(window.span(40, 40).is_empty(), "the cursor out of the view");
            assert!(window.span(0, 0).is_empty(), "an empty view");
            assert!(window.nearest_first(40, 40).is_empty());
        }
        assert_eq!(forward.span(0, 40), 0..16, "the near end clamps");
        assert_eq!(forward.span(39, 40), 37..40, "the far end clamps");
        assert_eq!(forward.span(2, 5), 0..5, "a view shorter than the window");
        let run = |from: usize, to: usize| -> Vec<usize> {
            if from <= to {
                (from..=to).collect()
            } else {
                (to..=from).rev().collect()
            }
        };
        let mut want = vec![10, 11, 9, 12, 8];
        want.extend(run(13, 25));
        assert_eq!(
            forward.nearest_first(10, 40),
            want,
            "forward: the tie ahead first"
        );
        let mut want = vec![30, 29, 31, 28, 32];
        want.extend(run(27, 15));
        assert_eq!(
            backward.nearest_first(30, 40),
            want,
            "backward: the tie behind first"
        );
        assert_eq!(
            even.nearest_first(5, 10),
            [5, 6, 4, 7, 3],
            "a symmetric window reads forward"
        );
        assert_eq!(
            forward.nearest_first(39, 40),
            [39, 38, 37],
            "clamped at the far end"
        );
    }

    /// What a moving frame asks for: the fit box when the engine has one,
    /// and the mid when it has none — never the full (brief 008; renamed
    /// from `transit_request_is_served_by_the_mid_rung`, whose promise, the
    /// mid for a box-less engine, it keeps in its first two rows).
    ///
    /// Box-less, the request must be a rung the MID actually serves. This is
    /// the bug the first implementation shipped with: it asked for
    /// `MID_RUNG_MAX_LONG` (2048), but `serves` allows only a 1.25x upscale,
    /// so a 1616 mid covers 2020 px — 28 short. Every transit frame quietly
    /// climbed to full-res anyway, and the change measured as no improvement
    /// at all until the arithmetic was checked.
    ///
    /// With a 3840x2160 box the request is that box, which the ladder serves
    /// with the 3/8 screen rung of an A1 frame, never the full. A transit
    /// capped at the mid whatever the box (A7's first mutant: the box
    /// ignored) is red here, clock-free: a held arrow on 4K would go back to
    /// the 2x-upscaled mid the brief exists to replace (issue #60).
    #[test]
    fn transit_request_is_the_fit_box_and_never_the_full() {
        let mid = FullImage {
            rgb: std::sync::Arc::new(vec![0u8; 3]),
            width: 1616,
            height: 1080,
            kind: RungKind::Mid,
        };
        // What focus() asks for while moving, at 1:1 on a full A1 frame.
        let request = transit_request(Target::Long(8640), None);
        assert!(
            serves(&mid, request.long()),
            "the mid rung must satisfy the transit request, or transit still \
             climbs to full-res: mid 1616 covers {} px, asked for {request:?}",
            (1616.0 * UPSCALE_THRESHOLD) as u32
        );
        // The old value is exactly the trap: keep it documented as failing.
        assert!(
            !serves(&mid, MID_RUNG_MAX_LONG),
            "MID_RUNG_MAX_LONG is NOT served by a 1616 mid — that was the bug"
        );
        // With the loupe's fit box on a 4K viewport, at fit and above it.
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        for desired in [Target::Long(u32::MAX), Target::Fit(uhd)] {
            assert_eq!(
                transit_request(desired, Some(uhd)),
                Target::Fit(uhd),
                "a moving frame on a 4K box asks for the box (desired {desired:?}), \
                 not the mid a 4K screen shows upscaled 2x"
            );
        }
        assert_eq!(
            fit_rung(8640, 5760, Some((1616, 1080)), 1, uhd),
            FitRungChoice::Screen(3),
            "the 4K box is served by the A1's 3/8 rung, never its full"
        );
    }

    /// Brief 008 A2 (raw-pipeline.md, "The factor rule"): the rung for a
    /// frame at fit is the BOX rule over (fit box, frame, mid, orientation) —
    /// the mid when its ORIENTED size serves the box, else the smallest N/8
    /// of the full whose ORIENTED output serves it, else the full. Every row
    /// carries its arithmetic: "serves" is 4·box ≤ 5·image on either side,
    /// "fits" is 4·image ≤ 5·box on both. The A1 is 8640x5760 with a
    /// 1616x1080 mid; N/8 of it is 1080x720, 2160x1440, 3240x2160,
    /// 4320x2880 for N = 1..=4.
    ///
    /// What the rows pin: a long-edge rule, blind to the box's short side,
    /// asks too much of every portrait frame and of a letterboxed box (the
    /// o6, o8, 5K-o8 and 3000x1700 rows); a mid read unrotated serves the
    /// wrong portrait boxes (the QHD-o8, 2100x1400 and 1400x2100 rows); and
    /// "no rung" read as "the full serves the box" — true of any frame
    /// larger than the box — sends every Screen row to the full.
    #[test]
    fn rung_factor_follows_the_viewport_and_the_frame() {
        use FitRungChoice::{Full, Mid, Screen};
        let bx = |width, height| FitBox { width, height };
        const A1: (u32, u32) = (8640, 5760);
        const A1_MID: Option<(u32, u32)> = Some((1616, 1080));
        // (box, full, mid, orientation, fit_rung, rung_factor), each row's
        // arithmetic above it.
        let rows = [
            // 4K, landscape: the mid (15360 > 8080, 8640 > 5400); 2/8
            // 2160x1440 (15360 > 10800, 8640 > 7200); 3/8 3240x2160 serves
            // (15360 <= 16200).
            (bx(3840, 2160), A1, A1_MID, 1, Screen(3), Some(3)),
            // 4K, portrait o6: the mid 1080x1616 (15360 > 5400, 8640 >
            // 8080); 1/8 720x1080 (8640 > 5400); 2/8 1440x2160 serves
            // (8640 <= 10800).
            (bx(3840, 2160), A1, A1_MID, 6, Screen(2), Some(2)),
            // 4K, portrait o8: as o6.
            (bx(3840, 2160), A1, A1_MID, 8, Screen(2), Some(2)),
            // QHD, landscape: the mid (10240 > 8080, 5760 > 5400); 1/8
            // 1080x720 (10240 > 5400, 5760 > 3600); 2/8 2160x1440 serves
            // (10240 <= 10800).
            (bx(2560, 1440), A1, A1_MID, 1, Screen(2), Some(2)),
            // QHD, portrait: the mid 1080x1616 oriented serves (5760 <=
            // 8080); without it, 2/8 1440x2160 (5760 <= 10800).
            (bx(2560, 1440), A1, A1_MID, 8, Mid, Some(2)),
            // 1080p, landscape: the mid serves (7680 <= 8080).
            (bx(1920, 1080), A1, A1_MID, 1, Mid, Some(2)),
            // 1080p, portrait: the mid 1080x1616 serves (4320 <= 8080);
            // without it, 1/8 720x1080 (4320 <= 5400).
            (bx(1920, 1080), A1, A1_MID, 8, Mid, Some(1)),
            // 5K, landscape: 3/8 3240x2160 (20480 > 16200, 11520 > 10800);
            // 4/8 4320x2880 serves (20480 <= 21600).
            (bx(5120, 2880), A1, A1_MID, 1, Screen(4), Some(4)),
            // 5K, portrait: 2/8 1440x2160 (20480 > 7200, 11520 > 10800);
            // 3/8 2160x3240 serves (11520 <= 16200).
            (bx(5120, 2880), A1, A1_MID, 8, Screen(3), Some(3)),
            // 4K, a 3000x2000 frame: its 1616x1077 mid does not serve
            // (15360 > 8080, 8640 > 5385), and the full fits the box x 1.25
            // (12000 <= 19200, 8000 <= 10800): no rung.
            (
                bx(3840, 2160),
                (3000, 2000),
                Some((1616, 1077)),
                1,
                Full,
                None,
            ),
            // 4K, a bare A1 (no mid): the same rule, 3/8.
            (bx(3840, 2160), A1, None, 1, Screen(3), Some(3)),
            // 4K, a bare 380x260: it fits the box (1520 <= 19200, 1040 <=
            // 10800): no rung.
            (bx(3840, 2160), (380, 260), None, 1, Full, None),
            // A box between factors: 2/8 (12000 > 10800, 8000 > 7200) does
            // not serve, so the next one up, 3/8 (12000 <= 16200) — never
            // the nearest.
            (bx(3000, 2000), A1, A1_MID, 1, Screen(3), Some(3)),
            // A letterboxed box: 2/8 serves by the SHORT side (6800 <= 7200)
            // though its long side does not (12000 > 10800).
            (bx(3000, 1700), A1, A1_MID, 1, Screen(2), Some(2)),
            // The mid ORIENTED, 1080x1616, serves (5600 <= 8080); unrotated
            // it would not (8400 > 8080, 5600 > 5400).
            (bx(2100, 1400), A1, A1_MID, 8, Mid, Some(2)),
            // The oriented mid 1080x1616 does not serve (5600 > 5400, 8400 >
            // 8080) — unrotated it would (5600 <= 8080); 2/8, oriented
            // 1440x2160, does (5600 <= 7200).
            (bx(1400, 2100), A1, A1_MID, 8, Screen(2), Some(2)),
            // The 4K window's real cell: 2/8 (15360 > 10800, 8400 > 7200);
            // 3/8 serves (15360 <= 16200).
            (bx(3840, 2100), A1, A1_MID, 1, Screen(3), Some(3)),
            // The default window's real cell: the mid serves (5760 <= 8080);
            // without it, 1/8 1080x720 (3360 <= 3600).
            (bx(1440, 840), A1, A1_MID, 1, Mid, Some(1)),
        ];
        for (row, (fit_box, (fw, fh), mid, orientation, want, factor)) in
            rows.into_iter().enumerate()
        {
            let why = format!("row {row}: {fw}x{fh} o{orientation} on {fit_box:?}");
            assert_eq!(
                fit_rung(fw, fh, mid, orientation, fit_box),
                want,
                "fit_rung, {why}"
            );
            assert_eq!(
                rung_factor(fw, fh, orientation, fit_box),
                factor,
                "rung_factor, {why}"
            );
            let oriented_scaled = |n: u8| {
                let (w, h) = scaled_dims(fw, fh, n);
                if matches!(orientation, 5..=8) {
                    (h, w)
                } else {
                    (w, h)
                }
            };
            match factor {
                // The factor serves, and it is the SMALLEST that does.
                Some(n) => {
                    let (w, h) = oriented_scaled(n);
                    assert!(serves_box(w, h, fit_box), "{n}/8 serves: {why}");
                    for smaller in 1..n {
                        let (w, h) = oriented_scaled(smaller);
                        assert!(!serves_box(w, h, fit_box), "{smaller}/8 does not: {why}");
                    }
                }
                // No rung: the oriented full fits the box x 1.25.
                None => {
                    let (w, h) = oriented_scaled(8);
                    assert!(fits_box(w, h, fit_box), "the full fits: {why}");
                }
            }
        }
        // The primitives on the same boxes.
        assert!(
            !serves_box(1616, 1080, bx(2560, 1440)),
            "the landscape mid on QHD"
        );
        assert!(
            serves_box(1080, 1616, bx(2560, 1440)),
            "the portrait mid on QHD"
        );
        assert!(fits_box(3000, 2000, bx(3840, 2160)));
        assert!(!fits_box(8640, 5760, bx(3840, 2160)));
        assert!(
            !serves_box(0, 1080, bx(1, 1)),
            "an image with a zero side never serves"
        );
        // The "wide viewport" predicate: the reference mid serves 1080p and a
        // portrait-turned QHD-class screen, not QHD or 4K.
        assert!(mid_serves_box(bx(1920, 1080)));
        assert!(mid_serves_box(bx(1440, 2260)));
        assert!(!mid_serves_box(bx(2560, 1440)));
        assert!(!mid_serves_box(bx(3840, 2160)));
    }

    /// The request state rides with the request (raw-pipeline.md, "The
    /// request state travels with the decode"): a focus that re-schedules a
    /// queued index replaces its state with its target; an in-flight index's
    /// deferred target merges with `max`, and its state changes only when
    /// the target GROWS; a revived entry keeps the state stored beside the
    /// deferred target, never the mode at revival. And the `Target` order
    /// those merges run on says `Equal` exactly when the two are `==`.
    #[test]
    fn the_request_state_travels_with_the_decode() {
        use RequestState::{Settled, Transit};
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let fit = Target::Fit(uhd);
        let mut st = LoupeState::default();
        assert!(schedule(&mut st, 5, fit, 1, Origin::Focus, Settled));
        assert!(schedule(&mut st, 5, fit, 2, Origin::Focus, Transit));
        assert_eq!(
            st.queue,
            vec![Entry {
                index: 5,
                target: fit,
                focus_origin: true,
                state: Transit,
            }],
            "a transit focus replaces the queued settled entry, state and all"
        );

        st.in_flight.push(7);
        assert!(!schedule(&mut st, 7, fit, 3, Origin::Focus, Settled));
        assert!(!schedule(&mut st, 7, fit, 4, Origin::Focus, Transit));
        assert_eq!(
            st.deferred.get(&7),
            Some(&(fit, Settled)),
            "an equal target keeps the state it was deferred with"
        );
        assert!(!schedule(
            &mut st,
            7,
            Target::Long(u32::MAX),
            5,
            Origin::Focus,
            Transit
        ));
        assert_eq!(
            st.deferred.get(&7),
            Some(&(Target::Long(u32::MAX), Transit)),
            "a grown target brings its state"
        );
        assert!(!schedule(&mut st, 7, fit, 6, Origin::Focus, Settled));
        assert_eq!(
            st.deferred.get(&7),
            Some(&(Target::Long(u32::MAX), Transit)),
            "a smaller one changes neither"
        );

        // The revival: the engine is settled (no held key), the deferred
        // state is transit — the entry keeps transit.
        let mut state = stable_focus_state(4);
        let now = std::time::Instant::now();
        assert!(!in_transit(&state, now));
        assert!(revive_deferred(&mut state, 5, fit, Transit, 1, 1000, now));
        assert_eq!(
            state.queue.first(),
            Some(&Entry {
                index: 5,
                target: fit,
                focus_origin: true,
                state: Transit,
            }),
            "a revived entry keeps the deferred state, not the mode at revival"
        );

        // The order.
        let letterbox = Target::Fit(FitBox {
            width: 3840,
            height: 1600,
        });
        assert!(
            fit > letterbox,
            "the taller box of the same width is the bigger ask"
        );
        assert_ne!(fit, letterbox);
        assert_ne!(
            fit.cmp(&letterbox),
            CmpOrdering::Equal,
            "two different boxes of one long edge must not compare Equal"
        );
        assert!(
            Target::Long(3840) > fit,
            "Long above Fit at an equal long edge"
        );
        assert!(Target::Long(2000) < fit);
        assert!(Target::Long(u32::MAX) > fit, "the top rung above any box");
    }

    /// Brief 008 R3: the factor follows the viewport, so a cached rung that
    /// no longer serves the box is re-requested at the new one, and shown
    /// meanwhile (the app cues it): a 2160x1440 rung, cut for a QHD box,
    /// under a 3840x2160 box is upscaled min(3840/2160, 2160/1440) = 1.5x.
    /// Under a 1920x1080 box the same rung is a downscale and serves.
    #[test]
    fn a_cached_rung_that_no_longer_serves_the_box_is_re_requested() {
        let rung = FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 2160,
            height: 1440,
            kind: RungKind::Screen,
        };
        let now = std::time::Instant::now();

        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        let mut state = LoupeState {
            fit_box: Some(uhd),
            ..Default::default()
        };
        state.cache.insert(5, (rung.clone(), 0));
        let hit = focus_on(&mut state, 5, FocusRequest::Fit, 10, 1, now);
        assert_eq!(
            hit.map(|i| (i.width, i.height, i.kind)),
            Some((2160, 1440, RungKind::Screen)),
            "the cached rung is what the loupe shows meanwhile"
        );
        assert_eq!(
            state.queue.iter().find(|e| e.index == 5),
            Some(&Entry {
                index: 5,
                target: Target::Fit(uhd),
                focus_origin: true,
                state: RequestState::Settled,
            }),
            "a 1.5x-upscaled rung must be re-requested at the 4K box"
        );

        let mut state = LoupeState {
            fit_box: Some(FitBox {
                width: 1920,
                height: 1080,
            }),
            ..Default::default()
        };
        state.cache.insert(5, (rung, 0));
        focus_on(&mut state, 5, FocusRequest::Fit, 10, 1, now);
        assert!(
            state.queue.iter().all(|e| e.index != 5),
            "a rung that serves the box by downscaling is enough: {:?}",
            state.queue
        );
    }

    /// The prefetch ring walks the VIEW order, not id order (issue #46).
    ///
    /// The M1 shape: a capture-time sort over a folder cycling three file
    /// classes interleaves ids in the view, so the old ±PREFETCH ring in
    /// id space warmed frames no arrow could reach while every actual
    /// arrow neighbor stayed cold — a deterministic fit-flash per step.
    #[test]
    fn the_prefetch_ring_walks_view_order_not_id_order() {
        let view = [0usize, 3, 6, 9, 1, 4, 7, 2, 5, 8];
        let mut state = LoupeState::default();
        apply_view(&mut state, &view, 10);
        // Focused on id 9 = view position 3: the settled ±PREFETCH ring
        // covers positions 1..=5, i.e. ids {3, 6, 1, 4} — and none of id
        // 9's id-space neighbors (7, 8), which are view strangers.
        let fpos = state.pos_of(9).expect("id 9 is in the view");
        assert_eq!(fpos, 3);
        let settled = plan(
            false,
            true,
            fpos,
            state.ring_len(10),
            Target::Long(u32::MAX),
            None,
            RING_AHEAD,
        );
        let ids = ring_ids(&state, fpos, settled.lo, settled.hi, true);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![1, 3, 4, 6],
            "the ring must hold the VIEW neighbors of id 9, not its id \
             neighbors: got {ids:?}"
        );
        // Farthest first: the two nearest view neighbors (ids 6 and 1,
        // positions 2 and 4) must sit at the BACK, where workers pop.
        assert_eq!(
            {
                let mut tail = ids[2..].to_vec();
                tail.sort_unstable();
                tail
            },
            vec![1, 6],
            "nearest view neighbors must be popped first: got {ids:?}"
        );
        // No view installed: identity — the pre-#46 id-space behavior,
        // which keeps core-only consumers and the older tests exact.
        let state = LoupeState::default();
        let mut ids = ring_ids(&state, 9, 7, 11, true);
        ids.sort_unstable();
        assert_eq!(ids, vec![7, 8, 10, 11], "no view = identity order");
    }

    /// The travel-direction latch compares VIEW positions (issue #46): on
    /// an interleaved view a steady forward hold FALLS in id half the
    /// time, and an id comparison flaps the transit ring's lean.
    #[test]
    fn travel_direction_is_latched_in_view_positions() {
        use std::time::{Duration, Instant};
        let view = [0usize, 3, 6, 9, 1, 4, 7, 2, 5, 8];
        let t0 = Instant::now();
        let mut st = LoupeState::default();
        apply_view(&mut st, &view, 10);
        // Forward in the VIEW: id 9 (pos 3) -> id 1 (pos 4).
        note_focus(&mut st, 9, Target::Long(u32::MAX), t0);
        note_focus(
            &mut st,
            1,
            Target::Long(u32::MAX),
            t0 + Duration::from_millis(120),
        );
        assert!(
            st.travel_forward,
            "pos 3 -> pos 4 is forward travel although the id fell 9 -> 1"
        );
        // Backward in the view despite a rising id: id 1 (pos 4) -> id 6
        // (pos 2).
        note_focus(
            &mut st,
            6,
            Target::Long(u32::MAX),
            t0 + Duration::from_millis(240),
        );
        assert!(
            !st.travel_forward,
            "pos 4 -> pos 2 is backward although the id rose 1 -> 6"
        );
    }

    /// Deferred-upgrade revival uses the same view-order ring (issue #46):
    /// an id 8 away can be the direct view neighbor, and the id next door
    /// can be a view stranger.
    #[test]
    fn deferred_revival_ring_follows_view_order() {
        let view = [0usize, 3, 6, 9, 1, 4, 7, 2, 5, 8];
        let mut state = stable_focus_state(9); // view position 3
        apply_view(&mut state, &view, 10);
        assert!(
            revive_long(&mut state, 1, u32::MAX, 1),
            "id 1 is the focused frame's direct VIEW neighbor (pos 4)"
        );
        state.queue.clear();
        assert!(
            !revive_long(&mut state, 8, u32::MAX, 1),
            "id 8 neighbors 9 in id space but sits at view pos 9 — a \
             stranger the ring must not revive"
        );
        assert!(state.queue.is_empty(), "nothing may be re-queued");
    }

    fn stable_focus_state(index: usize) -> LoupeState {
        LoupeState {
            focused: Some(index),
            // 2x: the caller's `now` predates this call by nanoseconds —
            // exactly one debounce would leave held marginally short.
            focused_at: Some(std::time::Instant::now() - FOCUS_DEBOUNCE * 2),
            focused_target: Target::Long(u32::MAX),
            // What the focus asked for, as `note_focus` sets it at every
            // focus before any decode can land (brief 008 step 3: a fixture
            // completion, no promise changed — the revival now asks the ring
            // plan what the position wants, which is `desired`, and the
            // default `Long(0)` is a state the real engine never reaches).
            desired: Target::Long(u32::MAX),
            ..Default::default()
        }
    }

    /// The Windows CI starvation (2026-07-27): indexes 0/1 were in flight
    /// at the mid rung when the 1:1 pin upgraded their deferred target to
    /// full-res; by the time those flights landed the cursor was at 4 —
    /// yet the old code re-queued them at TOP priority and both workers
    /// spent ~30 s (debug decode) on frames nobody was looking at.
    #[test]
    fn stale_deferred_upgrade_is_dropped_not_revived() {
        let mut state = stable_focus_state(4);
        assert!(
            !revive_long(&mut state, 0, u32::MAX, 1),
            "index 0 is outside the ring of focus 4"
        );
        assert!(state.queue.is_empty(), "nothing may be re-queued");
        // Exact ring boundary: distance PREFETCH is IN, one past is OUT.
        assert!(revive_long(&mut state, 4 - PREFETCH, u32::MAX, 1));
        assert!(!revive_long(&mut state, 4 - PREFETCH - 1, u32::MAX, 1));
        // No focus at all (loupe never opened): equally dropped.
        state.focused = None;
        assert!(!revive_long(&mut state, 0, u32::MAX, 2));
    }

    #[test]
    fn focused_deferred_upgrade_revives_at_top_priority() {
        let mut state = stable_focus_state(4);
        state.queue.push(long_entry(6, 1000, true));
        assert!(revive_long(&mut state, 4, u32::MAX, 1));
        // Workers pop from the back: the focused frame goes next.
        assert_eq!(state.queue.last(), Some(&long_entry(4, u32::MAX, true)));
    }

    #[test]
    fn ring_neighbor_deferred_upgrade_never_outranks_the_focused_frame() {
        let mut state = stable_focus_state(4);
        state.queue.push(long_entry(4, u32::MAX, true)); // the cursor's own pending work
        assert!(revive_long(&mut state, 5, u32::MAX, 1));
        assert_eq!(
            state.queue.last(),
            Some(&long_entry(4, u32::MAX, true)),
            "the focused frame stays first in line"
        );
        assert_eq!(state.queue.first(), Some(&long_entry(5, u32::MAX, true)));
    }

    #[test]
    fn failed_or_sufficient_deferred_upgrades_stay_dead() {
        let mut state = stable_focus_state(4);
        state.failed.insert(4);
        assert!(!revive_long(&mut state, 4, u32::MAX, 1));
        // A cached asset that already tops out (best_long known) is enough.
        let mut state = stable_focus_state(4);
        let img = FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 100,
            height: 100,
            kind: RungKind::Full,
        };
        state.cache.insert(4, (img, 0));
        state.best_long.insert(4, 100);
        assert!(!revive_long(&mut state, 4, u32::MAX, 1));
    }

    /// QE defect (the settled-then-left capture, ~20% in the CI shape):
    /// the cursor rests past the debounce on the load frame, THEN the
    /// 1:1 pin queues that frame's full-res climb — the escalation must
    /// re-arm the debounce, or the reserved lane races the backlog
    /// workers for a climb the cursor is about to leave.
    #[test]
    fn target_escalation_rearms_the_debounce() {
        let now = std::time::Instant::now();
        let mut state = stable_focus_state(0);
        state.focused_target = Target::Long(1900); // resting at a fit-sized target
        note_focus(&mut state, 0, Target::Long(u32::MAX), now); // the pin escalates
        state.queue.push(long_entry(0, u32::MAX, true));
        match next_job(&mut state, true, 0, 1000, now) {
            Slot::WaitFor(_) => {}
            other => panic!("escalated climb taken without debounce: {other:?}"),
        }
        // Render-cadence re-focus at the SAME target must not keep
        // re-arming (the clock would never expire).
        note_focus(
            &mut state,
            0,
            Target::Long(u32::MAX),
            now + std::time::Duration::from_millis(100),
        );
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now + FOCUS_DEBOUNCE),
            Slot::Job(0, Target::Long(u32::MAX), RequestState::Settled)
        );
        // A smaller target (zoom out) never re-arms either.
        let mut state = stable_focus_state(3);
        note_focus(&mut state, 3, Target::Long(1000), now);
        state.queue.push(long_entry(3, 1000, true));
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Job(3, Target::Long(1000), RequestState::Settled)
        );
    }

    /// The second starvation shape (Windows CI 2026-07-27): every
    /// worker was captured by legitimate climbs before the cursor
    /// settled — the reserved worker must take the STABLE focused
    /// frame's job, and nothing else.
    #[test]
    fn reserved_worker_takes_only_the_stable_focused_job() {
        let now = std::time::Instant::now();
        let mut state = stable_focus_state(4);
        state.queue.push(long_entry(2, u32::MAX, true));
        state.queue.push(long_entry(4, u32::MAX, true));
        state.queue.push(long_entry(5, u32::MAX, true)); // more urgent than 4's entry
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now),
            Slot::Job(4, Target::Long(u32::MAX), RequestState::Settled)
        );
        assert!(state.in_flight.contains(&4));
        // The focused entry is gone: the reserved worker now waits even
        // though backlog remains.
        assert_eq!(next_job(&mut state, true, 0, 1000, now), Slot::Wait);
        assert_eq!(
            state.queue.len(),
            2,
            "backlog untouched by the reserved worker"
        );
        // A normal worker still pops from the back.
        assert_eq!(
            next_job(&mut state, false, 0, 1000, now),
            Slot::Job(5, Target::Long(u32::MAX), RequestState::Settled)
        );
    }

    /// The capture-bait case that FAILED validation on the debounce-less
    /// version: a fresh focus (startup rest, transit touch) must never
    /// bind the reserved lane to a multi-second climb.
    #[test]
    fn reserved_worker_debounces_a_fresh_focus() {
        let now = std::time::Instant::now();
        let mut state = stable_focus_state(2);
        state.focused_at = Some(now); // focus just changed (transit touch)
        state.queue.push(long_entry(2, u32::MAX, true));
        match next_job(&mut state, true, 0, 1000, now) {
            Slot::WaitFor(d) => assert!(d <= FOCUS_DEBOUNCE, "timed wait bounded"),
            other => panic!("fresh focus must not be taken: {other:?}"),
        }
        assert_eq!(state.queue.len(), 1, "entry left for the backlog workers");
        // Once the focus has held, the reserved worker commits.
        assert_eq!(
            next_job(&mut state, true, 0, 1000, now + FOCUS_DEBOUNCE),
            Slot::Job(2, Target::Long(u32::MAX), RequestState::Settled)
        );
    }

    #[test]
    fn reserved_worker_waits_without_a_focus() {
        let now = std::time::Instant::now();
        let mut state = LoupeState::default();
        state.queue.push(long_entry(0, u32::MAX, true));
        assert_eq!(next_job(&mut state, true, 0, 1000, now), Slot::Wait);
        assert_eq!(state.queue.len(), 1);
    }

    #[test]
    fn next_job_skips_entries_already_served() {
        let now = std::time::Instant::now();
        let mut state = stable_focus_state(0);
        let img = FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 100,
            height: 100,
            kind: RungKind::Full,
        };
        state.cache.insert(0, (img, 0));
        state.best_long.insert(0, 100); // topped out
        state.queue.push(long_entry(0, u32::MAX, true));
        assert_eq!(
            next_job(&mut state, false, 0, 1000, now),
            Slot::Wait,
            "served entry consumed, no job"
        );
        assert!(state.queue.is_empty());
        assert!(state.in_flight.is_empty());
    }

    /// Issue #31, half one: `decode_oriented` sizes its decode buffer, the
    /// prefault pass, and the transpose Scratch from the HEADER's dimension
    /// claim before any scan data is validated. A sub-KB hostile stream
    /// (real headers, SOF patched to 30000x30000, truncated after SOS —
    /// the issue's 653-byte repro shape) committed 5.29 GB on the old
    /// code and "decoded" successfully. It must be rejected before any
    /// allocation. THIS TEST FAILS ON PRE-FIX CODE (it returns Ok there).
    ///
    /// Since brief 008 (2026-09-26) the loupe also decodes SCALED (the
    /// screen rung), and the cap applies to the header's FULL dimensions
    /// there too, before any factor is chosen (raw-pipeline.md A8): at 3/8
    /// the claim is 11250x11250, 127 MP, UNDER the cap, so a check on the
    /// scaled size would pass it and size a ~380 MB buffer from a sub-KB
    /// stream.
    #[test]
    fn decode_oriented_rejects_implausible_header_dimensions() {
        let mut jpeg = crate::raw::jpeg_hostile::encoded(64, 64);
        crate::raw::jpeg_hostile::patch_sof_dims(&mut jpeg, 30000, 30000);
        let hostile = crate::raw::jpeg_hostile::truncate_scan(&jpeg, 16);
        assert!(hostile.len() < 1024, "the attack fits in under a KB");
        for orientation in [1u16, 6] {
            // 6 = transpose: the Scratch prefault thread must not run either.
            let full = decode_oriented(&hostile, orientation)
                .expect_err("a 900 MP header claim must never allocate");
            let scaled = decode_scaled_oriented(&hostile, orientation, 3)
                .expect_err("a 900 MP header claim must never allocate, scaled or not");
            for err in [full, scaled] {
                assert!(
                    err.contains("implausible"),
                    "the reason must name the cause: {err}"
                );
            }
        }
        // The same claim with an intact EOI is still implausible: the cap,
        // not the truncation check, is what bounds the allocation.
        let mut with_eoi = crate::raw::jpeg_hostile::encoded(64, 64);
        crate::raw::jpeg_hostile::patch_sof_dims(&mut with_eoi, 30000, 30000);
        assert!(decode_oriented(&with_eoi, 1)
            .expect_err("hostile dims with a valid EOI")
            .contains("implausible"));
        assert!(decode_scaled_oriented(&with_eoi, 1, 3)
            .expect_err("hostile dims with a valid EOI, scaled")
            .contains("implausible"));
    }

    /// Issue #31, half two: zune-jpeg 0.4 zero-fills a truncated scan and
    /// reports SUCCESS (its overread counter stops growing once it starts
    /// zero-filling, so even strict mode cannot see it) — the loupe showed
    /// a blank frame instead of the Failed badge. THIS TEST FAILED ON THE
    /// PRE-#31 CODE (it returned Ok there).
    ///
    /// Since brief 008 the loupe decodes with libjpeg-turbo, which fails
    /// such a stream by itself ("Premature end of JPEG file"), so what
    /// this test pins now is OUR byte check (`scan_is_terminated`), which
    /// runs first, spares the grey decode and names the cause: remove it
    /// and the reason no longer says "truncated" — red on both entry
    /// points (raw-pipeline.md, "Truncation on the loupe path").
    #[test]
    fn decode_oriented_rejects_a_truncated_scan() {
        let intact = crate::raw::jpeg_hostile::encoded(64, 64);
        let truncated = crate::raw::jpeg_hostile::truncate_scan(&intact, 16);
        let err = decode_oriented(&truncated, 1).expect_err("truncated scan must fail");
        assert!(err.contains("truncated"), "reason names the cause: {err}");
        let err = decode_scaled_oriented(&truncated, 1, 3)
            .expect_err("truncated scan must fail at 3/8 too");
        assert!(err.contains("truncated"), "reason names the cause: {err}");
    }

    /// THIS IS THE RESIDUAL GAP OF ISSUE #31, CLOSED ON THE LOUPE PATH BY
    /// THE LIBRARY'S RETURN CONTRACT. When this fails with an `Ok`, the
    /// `turbojpeg` crate or libjpeg-turbo changed that contract — re-read
    /// canary 1 beside the dependency in `Cargo.toml`; do not quiet it.
    ///
    /// The stream: plausible dimensions, a scan cut 16 bytes in, and a
    /// valid EOI appended. It PASSES the byte check (asserted below: that
    /// is the point), and zune-jpeg decoded it as a mostly-blank success.
    /// libjpeg-turbo's Huffman decoder warns `JWRN_HIT_MARKER` and feeds
    /// zero bits, and `tj3Decompress8` returns -1 because a warning was
    /// emitted, so the decode is an `Err` over a grey-bottomed buffer —
    /// through the full decode and the scaled one, both orientations
    /// (raw-pipeline.md A8, "Truncation on the loupe path").
    #[test]
    fn a_short_scan_with_a_valid_eoi_fails_on_the_loupe_path() {
        let intact = crate::raw::jpeg_hostile::encoded(64, 64);
        let mut short = crate::raw::jpeg_hostile::truncate_scan(&intact, 16);
        short.extend_from_slice(&[0xFF, 0xD9]);
        assert!(
            crate::raw::scan_is_terminated(&short),
            "the byte check must PASS this stream, or the test proves nothing about the decoder"
        );
        let outcomes = [
            ("full, o1", decode_oriented(&short, 1)),
            ("3/8, o1", decode_scaled_oriented(&short, 1, 3)),
            ("3/8, o6", decode_scaled_oriented(&short, 6, 3)),
        ];
        for (shape, outcome) in outcomes {
            match outcome {
                Ok((_, w, h)) => panic!(
                    "{shape}: a short scan decoded as a {w}x{h} success — a blank frame \
                     where the Failed badge belongs"
                ),
                Err(err) => assert!(
                    err.contains("premature end of data segment"),
                    "{shape}: the decoder's own reason must reach the badge: {err}"
                ),
            }
        }
    }

    /// Issue #31 gate finding: the commonest field corruption is a
    /// partially copied RAW — the mid preview sits early in the file and
    /// survives, the full-res is cut off. The ladder must show the good
    /// mid and must NOT fail the image (decode_ladder's achieved>0
    /// branch): a Failed badge next to a visibly displayed image is the
    /// exact contradiction the validator flagged when this branch was
    /// first added.
    #[test]
    fn truncated_full_rung_keeps_the_good_mid_and_no_failed_badge() {
        // A TIFF container holding an intact mid-class preview and a
        // truncated full-res rung (SOF headers intact, scan cut off).
        let mid = crate::raw::jpeg_hostile::encoded(640, 400);
        let full_intact = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let full = crate::raw::jpeg_hostile::truncate_scan(&full_intact, 64);
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let mid_off = b.add_blob(&mid);
        let full_off = b.add_blob(&full);
        let second = b.add_ifd(
            &[(0x0201, 4, 1, full_off), (0x0202, 4, 1, full.len() as u32)],
            0,
        );
        let ifd0 = b.add_ifd(
            &[(0x0201, 4, 1, mid_off), (0x0202, 4, 1, mid.len() as u32)],
            second,
        );
        b.set_ifd0(ifd0);
        let dir = crate::testutil::scratch_dir("ladder31");
        let path = dir.join("mid_ok_full_cut.arw");
        std::fs::write(&path, &b.bytes).unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Shared {
            state: Mutex::new(LoupeState::default()),
            wakeup: Condvar::new(),
            paths: vec![path.clone()],
            events: tx,
            shutdown: AtomicBool::new(false),
            stamp: AtomicU64::new(0),
            budget: DEFAULT_BUDGET_BYTES,
        };
        // Ask for far more than the mid can serve, so the ladder MUST try
        // the truncated full rung.
        let outcome = decode_ladder(
            &shared,
            0,
            Target::Long(8640),
            0,
            false,
            RequestState::Settled,
        );
        assert_eq!(
            outcome,
            Ok(()),
            "a good mid must survive a truncated full rung — Ok means the \
             worker emits no Failed event"
        );
        match rx.try_recv() {
            Ok(LoupeEvent::Ready { image, .. }) => {
                assert_eq!((image.width, image.height), (640, 400), "the mid is shown");
            }
            other => panic!("expected the mid rung's Ready event, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "no second event: no Failed, no phantom rung"
        );
        assert_eq!(
            lock(&shared).best_long.get(&0).copied(),
            Some(640),
            "the ladder memoizes the achieved rung so it quiesces"
        );

        // The same file at FIT (brief 008): the mid does not serve a
        // 1000x700 box (min(1000/640, 700/400) = 1.5625), so the ladder
        // tries the full's screen rung, 3/8 -- and the SCALED decode of the
        // cut full is refused by the byte check. It must end exactly as the
        // plain decode's failure does: the good mid stays, no Failed, the
        // mid memoized; never the scaled failure passed on as the image's.
        let fit_box = FitBox {
            width: 1000,
            height: 700,
        };
        assert_eq!(
            rung_factor(2000, 1500, 1, fit_box),
            Some(3),
            "the premise: the ladder tries the 3/8 rung here"
        );
        let (shared, rx) = shared_over(vec![path]);
        let outcome = decode_ladder(
            &shared,
            0,
            Target::Fit(fit_box),
            0,
            false,
            RequestState::Settled,
        );
        assert_eq!(
            outcome,
            Ok(()),
            "a good mid must survive a truncated full's screen rung too"
        );
        match rx.try_recv() {
            Ok(LoupeEvent::Ready { image, .. }) => {
                assert_eq!(
                    (image.width, image.height, image.kind),
                    (640, 400, RungKind::Mid),
                    "the mid is shown"
                );
            }
            other => panic!("expected the mid rung's Ready event, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "no second event at fit either: no Failed, no phantom rung"
        );
        assert_eq!(
            lock(&shared).best_long.get(&0).copied(),
            Some(640),
            "the ladder memoizes the mid at fit too"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A whole RAW laid out as an A1 is — its IFD tables first, then the
    /// intact 640x400 mid, whose IFD gives no size (an A1's IFD0 gives none),
    /// then `full` — with the offsets of the mid and of the full. With
    /// `dims_in_ifd` the full's IFD gives its size, as an A1's IFD2 does;
    /// without it the walker sizes the full from its SOF, as it does for a
    /// body whose IFD gives none (M11).
    fn raw_laid_out_as_an_a1(full: &[u8], dims_in_ifd: bool) -> (Vec<u8>, usize, usize) {
        let mid = crate::raw::jpeg_hostile::encoded(640, 400);
        let ifd_len = |entries: u32| 2 + 12 * entries + 4;
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let ifd0 = b.bytes.len() as u32;
        let second = ifd0 + ifd_len(2);
        // The mid follows the second IFD, whose length is its own entry
        // count: two pointer entries, and the size pair when it has one.
        let mid_off = second + ifd_len(if dims_in_ifd { 4 } else { 2 });
        let full_off = mid_off + mid.len() as u32;
        assert_eq!(
            b.add_ifd(
                &[(0x0201, 4, 1, mid_off), (0x0202, 4, 1, mid.len() as u32)],
                second
            ),
            ifd0
        );
        let mut entries = Vec::new();
        if dims_in_ifd {
            let (w, h) = crate::raw::sof_dimensions(full).expect("a full with a SOF");
            entries.extend([(0x0100, 3, 1, w), (0x0101, 3, 1, h)]);
        }
        entries.extend([(0x0201, 4, 1, full_off), (0x0202, 4, 1, full.len() as u32)]);
        assert_eq!(b.add_ifd(&entries, 0), second);
        assert_eq!(b.add_blob(&mid), mid_off);
        assert_eq!(b.add_blob(full), full_off);
        b.set_ifd0(ifd0);
        (b.bytes, mid_off as usize, full_off as usize)
    }

    /// A RAW cut by an interrupted copy, laid out as an A1 is
    /// ([`raw_laid_out_as_an_a1`], the full's size in its IFD or not), cut
    /// `keep` bytes into the full.
    fn raw_cut_inside_its_full(full: &[u8], keep: usize, dims_in_ifd: bool) -> Vec<u8> {
        let (mut bytes, _, full_off) = raw_laid_out_as_an_a1(full, dims_in_ifd);
        bytes.truncate(full_off + keep);
        bytes
    }

    /// A RAW cut by an interrupted copy INSIDE ITS MID, laid out as an A1 is
    /// ([`raw_laid_out_as_an_a1`], the full's size in its IFD): cut `keep`
    /// bytes into the mid, so the full's pointer lies past the file's end.
    fn raw_cut_inside_its_mid(full: &[u8], keep: usize) -> Vec<u8> {
        let (mut bytes, mid_off, _) = raw_laid_out_as_an_a1(full, true);
        bytes.truncate(mid_off + keep);
        bytes
    }

    /// QE round 1 of brief 008, D1 (raw-pipeline.md, "Hostile-input bounds"
    /// and "All rejections"): a RAW cut inside its full — an interrupted
    /// copy, the commonest field corruption — keeps the full as its top rung,
    /// so the good mid below it is published and is NEVER the file's best
    /// (`terminal`): at fit it does not serve a box it would upscale, and the
    /// app shows it cued; above fit it is soft under the pill and the zoom
    /// reaches past it. The full's read fails as truncated over the good
    /// mid: no Failed, the mid memoized so the ladder quiesces. At fit (a box
    /// the mid does not serve, where the full's screen rung is tried) and at
    /// 1:1. Red on the walker that dropped a JPEG the file ends inside: the
    /// mid was the file's only rung, published terminal — its best — which
    /// the app showed uncued at fit and would not zoom past.
    ///
    /// Two shapes of the file (QE round 2 of brief 008, T6): the full's size
    /// in its IFD, as the A1 carries it, and none there, as another body's
    /// IFD may give none (M11) — the walker then sizes the cut full from the
    /// SOF the file still holds, and the ladder must reach the same end: the
    /// mid is never such a file's best either. Red on that second shape alone
    /// with the walker dropping a cut JPEG its IFD does not size.
    #[test]
    fn a_raw_cut_inside_its_full_never_makes_the_mid_its_best() {
        let dir = crate::testutil::scratch_dir("cut-full");
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let fit_box = FitBox {
            width: 1000,
            height: 700,
        };
        for dims_in_ifd in [true, false] {
            let shape = if dims_in_ifd {
                "the full's size in its IFD"
            } else {
                "no size in the full's IFD (sized from its SOF)"
            };
            let path = dir.join(if dims_in_ifd {
                "cut_full.arw"
            } else {
                "cut_full_sized_from_its_sof.arw"
            });
            std::fs::write(
                &path,
                raw_cut_inside_its_full(&full, full.len() / 2, dims_in_ifd),
            )
            .unwrap();
            let whole = find_embedded_jpegs(&mut std::fs::File::open(&path).unwrap())
                .unwrap()
                .fullres()
                .map(|c| (c.width, c.height));
            assert_eq!(
                whole,
                Some((640, 400)),
                "{shape}: the premise: the file holds its mid whole and its full cut"
            );
            for target in [Target::Fit(fit_box), Target::Long(u32::MAX)] {
                let (shared, rx) = shared_over(vec![path.clone()]);
                assert_eq!(
                    decode_ladder(&shared, 0, target, 0, false, RequestState::Settled),
                    Ok(()),
                    "{shape}, {target:?}: the mid is good, so the cut full fails nothing"
                );
                match rx.try_recv() {
                    Ok(LoupeEvent::Ready {
                        image, terminal, ..
                    }) => {
                        assert_eq!(
                            (image.width, image.height, image.kind),
                            (640, 400, RungKind::Mid),
                            "{shape}, {target:?}: the mid is shown"
                        );
                        assert!(
                            !terminal,
                            "{shape}, {target:?}: the mid of a file whose full was cut is \
                             never its best"
                        );
                    }
                    other => {
                        panic!("{shape}, {target:?}: expected the mid's Ready event, got {other:?}")
                    }
                }
                assert!(
                    rx.try_recv().is_err(),
                    "{shape}, {target:?}: nothing else — no Failed, no phantom rung"
                );
                assert_eq!(
                    lock(&shared).best_long.get(&0).copied(),
                    Some(640),
                    "{shape}, {target:?}: the mid memoized, so the ladder quiesces"
                );
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QE round 2 of brief 008, T7 (raw-pipeline.md, "Hostile-input bounds",
    /// "Truncation, a RAW cut inside an embedded JPEG"): a RAW cut inside its
    /// MID holds no JPEG whole — the full's pointer lies past the file's end
    /// — so the loupe's top rung is the cut mid itself (`loupe_top`: the
    /// largest embedded JPEG whole or cut, its `(None, c)` arm), `read_jpeg`
    /// refuses it as truncated, and with nothing lower in hand the rung's
    /// failure is the image's: the ladder fails naming the cut and publishes
    /// nothing. With the mid's SOF cut away too (10 bytes in, no size in its
    /// IFD) nothing of it can be sized, the walker keeps nothing, and the
    /// failure is `NO_USABLE_PREVIEW`. At a fit box the mid would not serve
    /// and at 1:1.
    ///
    /// Core's `Failed` event contract only: what the app shows for this file
    /// is the grid pipeline's badge, whose tooltip reads the pipeline's own
    /// reason, "no usable embedded preview" — the pump drops a loupe
    /// `Failed`'s reason (pump.rs).
    ///
    /// Red with `loupe_top`'s `(None, c) => c` arm reading `(None, _) =>
    /// None` (row 1 then fails "no usable embedded preview"; no other core
    /// test sees that arm), and with `read_jpeg`'s truncation check removed
    /// (row 1 then fails "read: I/O error reading RAW file", naming no
    /// cause — a mutant shared with the walker and the stderr tests).
    #[test]
    fn a_raw_cut_inside_its_mid_fails_naming_the_cut() {
        let dir = crate::testutil::scratch_dir("cut-mid");
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let mid_len = crate::raw::jpeg_hostile::encoded(640, 400).len();
        let fit_box = FitBox {
            width: 1000,
            height: 700,
        };
        let targets = [Target::Fit(fit_box), Target::Long(u32::MAX)];

        // Row 1: cut inside the mid's scan, half of the mid kept — its SOF
        // with it, so the walker sizes the cut mid.
        let path = dir.join("cut_in_mid.arw");
        std::fs::write(&path, raw_cut_inside_its_mid(&full, mid_len / 2)).unwrap();
        let previews = find_embedded_jpegs(&mut std::fs::File::open(&path).unwrap()).unwrap();
        assert!(
            previews.fullres().is_none(),
            "the premise: the file holds no JPEG whole: {previews:?}"
        );
        assert_eq!(
            previews.loupe_top().map(|c| (c.width, c.height)),
            Some((640, 400)),
            "the premise: the loupe's top is the cut mid"
        );
        for target in targets {
            let (shared, rx) = shared_over(vec![path.clone()]);
            let outcome = decode_ladder(&shared, 0, target, 0, false, RequestState::Settled);
            let reason = match outcome {
                Err(reason) => reason,
                Ok(()) => panic!("{target:?}: a file with no JPEG whole failed nothing"),
            };
            assert!(
                reason.contains("truncated"),
                "{target:?}: the failure names the cut: {reason}"
            );
            assert!(rx.try_recv().is_err(), "{target:?}: nothing is published");
            assert_eq!(
                lock(&shared).best_long.get(&0),
                None,
                "{target:?}: nothing is memoized"
            );
        }

        // Row 2: cut 10 bytes into the mid — its SOF gone, and no size in its
        // IFD — so nothing of the file can be sized.
        let path = dir.join("cut_before_the_mids_sof.arw");
        std::fs::write(&path, raw_cut_inside_its_mid(&full, 10)).unwrap();
        for target in targets {
            let (shared, rx) = shared_over(vec![path.clone()]);
            assert_eq!(
                decode_ladder(&shared, 0, target, 0, false, RequestState::Settled),
                Err(crate::raw::NO_USABLE_PREVIEW.to_string()),
                "{target:?}: the mid's SOF cut away leaves nothing to size"
            );
            assert!(rx.try_recv().is_err(), "{target:?}: nothing is published");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Brief 008 (raw-pipeline.md, "The screen rung"): a rung's kind is the
    /// scale the decoder RAN, never a comparison with the IFD's size claim,
    /// which `find_embedded_jpegs` trusts over the SOF. Here the one IFD
    /// under-claims its intact 2000x1500 stream as 400x300; the box rule on
    /// the stream picks 2/8 for a 500x375 box, and the 2/8 decode comes out
    /// 500x375 — LONGER than the claim. It is still a screen rung: never
    /// terminal, never memoized as the file's best. Read the kind off the
    /// claim ("decoded at least as long as declared: the full") and the
    /// rung ships as a terminal full, the zoom ceiling read from it.
    ///
    /// Re-fixtured by QE round 1's D2 (2026-09-28), the promise kept: it
    /// under-claimed 500x375 for a 200x150 box, where the factor planned from
    /// the claim, 3/8, gave a 750x563 rung; planned from the stream that box
    /// takes 1/8, 250x188, which is SHORTER than that claim — and a kind read
    /// off the claim would call it a screen rung too, so the row would no
    /// longer catch it. A 400x300 claim (still over the 100,000-pixel floor a
    /// candidate needs) and a 500x375 box put the decoded rung past the claim
    /// again, on the ladder's main path.
    #[test]
    fn the_rung_kind_comes_from_the_decode_not_the_ifd_claim() {
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let off = b.add_blob(&full);
        let ifd0 = b.add_ifd(
            &[
                (0x0100, 3, 1, 400),
                (0x0101, 3, 1, 300),
                (0x0201, 4, 1, off),
                (0x0202, 4, 1, full.len() as u32),
            ],
            0,
        );
        b.set_ifd0(ifd0);
        let dir = crate::testutil::scratch_dir("kind-claim");
        let path = dir.join("under_claimed.arw");
        std::fs::write(&path, &b.bytes).unwrap();
        let fit_box = FitBox {
            width: 500,
            height: 375,
        };
        assert_eq!(
            rung_factor(2000, 1500, 1, fit_box),
            Some(2),
            "the premise: the stream asks for the 2/8 rung"
        );
        assert!(
            scaled_dims(2000, 1500, 2).0 > 400,
            "the premise: the rung decodes longer than the IFD claims"
        );

        let (shared, rx) = shared_over(vec![path]);
        assert_eq!(
            decode_ladder(
                &shared,
                0,
                Target::Fit(fit_box),
                0,
                false,
                RequestState::Settled
            ),
            Ok(())
        );
        match rx.try_recv() {
            Ok(LoupeEvent::Ready {
                image, terminal, ..
            }) => {
                assert_eq!(
                    (image.width, image.height, image.kind),
                    (500, 375, RungKind::Screen),
                    "a 2/8 decode is a screen rung whatever the IFD claimed"
                );
                assert!(!terminal, "a screen rung is never the file's best");
            }
            other => panic!("expected the screen rung's Ready event, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "it serves the box: one rung");
        assert_eq!(
            lock(&shared).best_long.get(&0),
            None,
            "a screen rung is never memoized as the file's best"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A RAW whose second IFD CLAIMS a 4000x3000 full over `full`, an intact
    /// 2000x1500 stream, behind an intact 640x400 mid: `find_embedded_jpegs`
    /// trusts an IFD's size over the SOF, so the ladder plans for a full it
    /// can never decode (M11: another body's writer, or a damaged IFD).
    fn raw_over_claiming_its_full(full: &[u8]) -> Vec<u8> {
        raw_claiming_its_full(full, 4000, 3000)
    }

    /// [`raw_over_claiming_its_full`] with the second IFD claiming
    /// `claim_w`x`claim_h` for `full`, over or under what the stream holds.
    fn raw_claiming_its_full(full: &[u8], claim_w: u32, claim_h: u32) -> Vec<u8> {
        let mid = crate::raw::jpeg_hostile::encoded(640, 400);
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let mid_off = b.add_blob(&mid);
        let full_off = b.add_blob(full);
        let second = b.add_ifd(
            &[
                (0x0100, 3, 1, claim_w),
                (0x0101, 3, 1, claim_h),
                (0x0201, 4, 1, full_off),
                (0x0202, 4, 1, full.len() as u32),
            ],
            0,
        );
        let ifd0 = b.add_ifd(
            &[(0x0201, 4, 1, mid_off), (0x0202, 4, 1, mid.len() as u32)],
            second,
        );
        b.set_ifd0(ifd0);
        b.bytes
    }

    /// Every image a ladder flight published, in order, as (width, height,
    /// kind); a `Failed` event fails the test.
    fn published(rx: &std::sync::mpsc::Receiver<LoupeEvent>) -> Vec<(u32, u32, RungKind)> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                LoupeEvent::Ready { image, .. } => {
                    out.push((image.width, image.height, image.kind));
                }
                LoupeEvent::Failed { reason, .. } => panic!("an unexpected Failed: {reason}"),
            }
        }
        out
    }

    /// The ladder memoizes the long edge it DECODED as the file's best, never
    /// an IFD's claim (raw-pipeline.md, "The screen rung"; the step-2 review;
    /// Manager ruling 2026-09-27; M11). An IFD that over-claims its full —
    /// 4000x3000 over a 2000x1500 stream — made the ladder memoize 4000,
    /// which the cached 2000x1500 frame can never reach: `cached_serves`
    /// stayed false, and the reserved lane's settle guarantee queued the same
    /// decode again at every settle for as long as the cursor rested on the
    /// frame (the review measured 3 re-decodes over 3 settle passes at 1:1,
    /// 4 at a 4K fit box). Red on the old memo: `best_long` reads 4000, and
    /// the lane returns a job where it must wait.
    ///
    /// The memo is set in two places, and each has its row. The plain decode
    /// of the full (the baseline stream, at 1:1 and at fit). And, at fit, the
    /// screen branch's arm for a decode the decoder ran at FULL scale whatever
    /// it was asked — a CMYK, YCCK, lossless or second-opinion stream — which
    /// here is the same over-claiming IFD over a CMYK full: its 5/8 attempt
    /// comes out 2000x1500 and IS the full, so the plain decode never runs
    /// (the step-3 review's F1: with only the baseline rows, that arm's memo
    /// could revert to the claim with the suite green).
    ///
    /// Changed by QE round 1's D2 (2026-09-28), the promise kept: the screen
    /// rung is planned from the stream's own SOF, so the baseline row at the
    /// 4K box — where the 2000x1500 stream already fits the box × 1.25 and
    /// takes no rung — decodes the full once; it read the claim's 5/8 rung
    /// first, 1250x938, then the full: the double decode D2 names. And a
    /// `Full` that misses the box after a screen attempt is now the ladder's
    /// defence for a plan that misses its stream, so the CMYK row plans from
    /// the IFD's claim through the test seam `PLANNED_DIMS` — the plan every
    /// such file got before D2 — to keep reaching that arm's memo.
    #[test]
    fn the_ladder_memoizes_the_decoded_size_not_the_ifd_claim() {
        use RungKind::{Full, Mid};
        /// Clears the seam whatever happens, so no later test on this
        /// thread inherits it.
        struct ClearPlan;
        impl Drop for ClearPlan {
            fn drop(&mut self) {
                PLANNED_DIMS.with(|plan| plan.set(None));
            }
        }
        let _clear = ClearPlan;
        let dir = crate::testutil::scratch_dir("over-claim-memo");
        let baseline = dir.join("over_claimed.arw");
        std::fs::write(
            &baseline,
            raw_over_claiming_its_full(&crate::raw::jpeg_hostile::encoded(2000, 1500)),
        )
        .unwrap();
        let cmyk = dir.join("over_claimed_cmyk.arw");
        std::fs::write(
            &cmyk,
            raw_over_claiming_its_full(&crate::raw::jpeg_hostile::encoded_as(
                2000,
                1500,
                jpeg_encoder::ColorType::Cmyk,
            )),
        )
        .unwrap();
        for path in [&baseline, &cmyk] {
            let mut file = std::fs::File::open(path).unwrap();
            let claim = find_embedded_jpegs(&mut file)
                .unwrap()
                .fullres()
                .map(|f| (f.width, f.height));
            assert_eq!(
                claim,
                Some((4000, 3000)),
                "the premise: the IFD's claim sizes the full of {}",
                path.display()
            );
        }
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        // At the 4K box the STREAM, 2000x1500, already fits the box × 1.25:
        // no rung, and the full is decoded once. Planned from the claim, the
        // CMYK row asks 5/8, which a CMYK stream decodes at full scale: the
        // screen branch's `Full` arm, short of the box.
        assert_eq!(rung_factor(2000, 1500, 1, uhd), None);
        assert_eq!(rung_factor(4000, 3000, 1, uhd), Some(5));
        let now = std::time::Instant::now();
        for (path, target, planned, rungs) in [
            (
                &baseline,
                Target::Long(u32::MAX),
                None,
                vec![(640, 400, Mid), (2000, 1500, Full)],
            ),
            (
                &baseline,
                Target::Fit(uhd),
                None,
                vec![(640, 400, Mid), (2000, 1500, Full)],
            ),
            (
                &cmyk,
                Target::Fit(uhd),
                Some((4000, 3000)),
                vec![(640, 400, Mid), (2000, 1500, Full)],
            ),
        ] {
            let row = format!(
                "{} at {target:?}, planned from {planned:?}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            PLANNED_DIMS.with(|plan| plan.set(planned));
            let (shared, rx) = shared_over(vec![path.clone()]);
            assert_eq!(
                decode_ladder(&shared, 0, target, 0, false, RequestState::Settled),
                Ok(()),
                "{row}"
            );
            assert_eq!(published(&rx), rungs, "{row}: what the stream holds");
            let mut state = lock(&shared);
            assert_eq!(
                state.best_long.get(&0).copied(),
                Some(2000),
                "{row}: the memo is the decoded 2000, not the IFD's 4000"
            );
            // The cursor at rest on this frame, settled and past the debounce.
            state.focused = Some(0);
            state.focused_at = Some(now - FOCUS_DEBOUNCE * 2);
            state.last_index_change = Some(now - SETTLE_DEBOUNCE * 2);
            state.desired = target;
            assert_eq!(
                next_job(&mut state, true, 0, 1000, now),
                Slot::Wait,
                "{row}: a file whose ladder topped out is not decoded again at every settle"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QE round 1's D2 (2026-09-28; raw-pipeline.md, "The factor rule"): the
    /// screen rung is planned from the size the STREAM declares in its own
    /// SOF, the size the decoder scales, never from the IFD's claim. Over a
    /// 2000x1500 stream, an IFD claiming 4000x3000 planned 2/8 for a 1000x700
    /// box, which the stream decodes to 500x375 — short of the box and no
    /// longer than the mid in hand — so the ladder went on to the full: a
    /// full-res decode at fit, and in the app a 149 MB texture, for every such
    /// frame of the ring. One claiming 1000x750 fitted the box on paper, so
    /// the ladder asked for the full outright (its 750,000 claimed pixels also
    /// make it the file's grid source, so its ladder starts at the full).
    /// Planned from the stream, both take the 3/8 rung, 750x563 — the
    /// smallest that serves the box — in one decode of the full, and a screen
    /// rung is never memoized. Red on the claim's plan: each publishes the
    /// 2000x1500 full instead of the rung.
    #[test]
    fn the_screen_rung_is_planned_from_the_stream_not_the_ifd_claim() {
        use RungKind::{Mid, Screen};
        let dir = crate::testutil::scratch_dir("plan-from-stream");
        let stream = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let fit_box = FitBox {
            width: 1000,
            height: 700,
        };
        assert_eq!(
            rung_factor(2000, 1500, 1, fit_box),
            Some(3),
            "the premise: the stream asks for the 3/8 rung"
        );
        assert_eq!(
            rung_factor(4000, 3000, 1, fit_box),
            Some(2),
            "the premise: the over-claim asks for 2/8, which the stream decodes short of the box"
        );
        assert_eq!(
            rung_factor(1000, 750, 1, fit_box),
            None,
            "the premise: the under-claim fits the box on paper"
        );
        for (name, (claim_w, claim_h), rungs) in [
            (
                "over_claimed.arw",
                (4000, 3000),
                vec![(640, 400, Mid), (750, 563, Screen)],
            ),
            ("under_claimed.arw", (1000, 750), vec![(750, 563, Screen)]),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, raw_claiming_its_full(&stream, claim_w, claim_h)).unwrap();
            let (shared, rx) = shared_over(vec![path]);
            assert_eq!(
                decode_ladder(
                    &shared,
                    0,
                    Target::Fit(fit_box),
                    0,
                    false,
                    RequestState::Settled
                ),
                Ok(()),
                "{name}"
            );
            assert_eq!(
                published(&rx),
                rungs,
                "{name}: one decode of the full, at the rung that serves the box"
            );
            assert_eq!(
                lock(&shared).best_long.get(&0),
                None,
                "{name}: a screen rung is never memoized as the file's best"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The step-2 review's F3 (raw-pipeline.md, "The loupe ladder": the lane
    /// "checks only BETWEEN rungs, so a focus change during a rung's decode
    /// waits out that rung — one decode"). A screen rung that does not serve
    /// — the over-claimed full's 5/8 decode comes out 1250x938 for a
    /// 3840x2160 box — falls through to the plain decode of the same full, a
    /// SECOND decode in one flight, so the reserved lane checks its focus
    /// between the two. The focus moves at the one instant only a test hook
    /// reaches, right after the screen decode (`AFTER_SCREEN_DECODE`). Red
    /// with that check removed: the lane decodes the full of a frame the user
    /// has left.
    ///
    /// Changed by QE round 1's D2 (2026-09-28), the rows and the promise
    /// kept: the screen rung is planned from the stream's own SOF, where this
    /// file's rung came from its IFD's 4000x3000 claim, and a plan from the
    /// stream always serves — the stream here, 2000x1500, fits the 4K box and
    /// takes no rung at all. The fall-through stays as the ladder's defence
    /// for a plan that misses its stream, and the test seam `PLANNED_DIMS`
    /// plans from the claim again, the one way left to reach it.
    #[test]
    fn the_reserved_lane_abandons_between_the_screen_rung_and_the_full() {
        use RungKind::{Full, Mid, Screen};
        /// Clears the hook and the planning seam whatever happens, so no
        /// later test on this thread inherits either.
        struct ClearHook;
        impl Drop for ClearHook {
            fn drop(&mut self) {
                AFTER_SCREEN_DECODE.with(|hook| hook.set(None));
                PLANNED_DIMS.with(|plan| plan.set(None));
            }
        }
        let _clear = ClearHook;
        PLANNED_DIMS.with(|plan| plan.set(Some((4000, 3000))));
        let dir = crate::testutil::scratch_dir("lane-f3");
        let path = dir.join("over_claimed.arw");
        std::fs::write(
            &path,
            raw_over_claiming_its_full(&crate::raw::jpeg_hostile::encoded(2000, 1500)),
        )
        .unwrap();
        let uhd = Target::Fit(FitBox {
            width: 3840,
            height: 2160,
        });
        let move_focus: fn(&Shared) = |shared| lock(shared).focused = Some(1);
        let keep_focus: fn(&Shared) = |_| {};
        let mid_and_rung = vec![(640, 400, Mid), (1250, 938, Screen)];
        let every_rung = vec![(640, 400, Mid), (1250, 938, Screen), (2000, 1500, Full)];
        for (row, reserved, hook, rungs) in [
            ("the lane, focus moved", true, move_focus, mid_and_rung),
            ("the lane, focus kept", true, keep_focus, every_rung.clone()),
            (
                "a backlog flight, focus moved",
                false,
                move_focus,
                every_rung,
            ),
        ] {
            AFTER_SCREEN_DECODE.with(|slot| slot.set(Some(hook)));
            let (shared, rx) = shared_over(vec![path.clone()]);
            lock(&shared).focused = Some(0);
            assert_eq!(
                decode_ladder(&shared, 0, uhd, 0, reserved, RequestState::Settled),
                Ok(()),
                "{row}"
            );
            assert_eq!(published(&rx), rungs, "{row}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Brief 008: the ladder's stop test reads the decoded, ORIENTED image,
    /// never the IFD's stored, unrotated size. A 640x400 mid in a portrait
    /// file (orientation 8) is 400x640 on screen, and serves a 1000x700 box:
    /// min(1000/400, 700/640) = 1.09. Read unrotated it is min(1000/640,
    /// 700/400) = 1.5625, which does not, and the ladder climbs on to decode
    /// the full's screen rung for nothing.
    #[test]
    fn a_portrait_mid_that_serves_the_box_stops_the_ladder() {
        let mid = crate::raw::jpeg_hostile::encoded(640, 400);
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let mid_off = b.add_blob(&mid);
        let full_off = b.add_blob(&full);
        let second = b.add_ifd(
            &[(0x0201, 4, 1, full_off), (0x0202, 4, 1, full.len() as u32)],
            0,
        );
        // IFD0 carries the orientation, as an A1's does (the TIFF walker
        // keeps the first one it meets).
        let ifd0 = b.add_ifd(
            &[
                (0x0112, 3, 1, 8),
                (0x0201, 4, 1, mid_off),
                (0x0202, 4, 1, mid.len() as u32),
            ],
            second,
        );
        b.set_ifd0(ifd0);
        let dir = crate::testutil::scratch_dir("portrait-mid");
        let path = dir.join("portrait.arw");
        std::fs::write(&path, &b.bytes).unwrap();

        let (shared, rx) = shared_over(vec![path]);
        let fit_box = FitBox {
            width: 1000,
            height: 700,
        };
        assert_eq!(
            decode_ladder(
                &shared,
                0,
                Target::Fit(fit_box),
                0,
                false,
                RequestState::Settled
            ),
            Ok(())
        );
        match rx.try_recv() {
            Ok(LoupeEvent::Ready { image, .. }) => {
                assert_eq!(
                    (image.width, image.height, image.kind),
                    (400, 640, RungKind::Mid),
                    "the mid, rotated to portrait"
                );
            }
            other => panic!("expected the mid's Ready event, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "the oriented mid serves the box: nothing more is decoded"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The checks must not reject what they exist to protect: an intact
    /// stream decodes exactly as before, on both the plain and the
    /// transpose orientation path.
    #[test]
    fn decode_oriented_still_accepts_intact_streams() {
        let jpeg = crate::raw::jpeg_hostile::encoded(64, 48);
        let (rgb, w, h) = decode_oriented(&jpeg, 1).expect("intact stream decodes");
        assert_eq!((w, h), (64, 48));
        assert_eq!(rgb.len(), 64 * 48 * 3);
        let (_, w, h) = decode_oriented(&jpeg, 6).expect("transpose path decodes");
        assert_eq!((w, h), (48, 64), "orientation 6 swaps the sides");
        // The scaled entry point (the screen rung, brief 008): 4/8 halves
        // each side, and the rotate still applies to what was decoded.
        let (rgb, w, h) = decode_scaled_oriented(&jpeg, 1, 4).expect("a 4/8 decode");
        assert_eq!((w, h), (32, 24));
        assert_eq!(rgb.len(), 32 * 24 * 3);
        let (_, w, h) = decode_scaled_oriented(&jpeg, 6, 4).expect("a 4/8 transpose decode");
        assert_eq!((w, h), (24, 32), "orientation 6 swaps the scaled sides");
    }

    /// A rung is never larger than the full JPEG: libjpeg-turbo would
    /// happily decode 9/8 up to 2/1 (upscaling inside the IDCT), so the
    /// numerator's range is ours to guard. Delete the guard and 9 decodes
    /// a 72x54 image from this 64x48 stream.
    #[test]
    fn decode_scaled_oriented_refuses_a_numerator_outside_1_to_8() {
        let jpeg = crate::raw::jpeg_hostile::encoded(64, 48);
        for numerator in [9u8, 16, 0] {
            match decode_scaled_oriented(&jpeg, 1, numerator) {
                // Not `expect_err`: its message would print the whole buffer.
                Ok((_, w, h)) => panic!(
                    "numerator {numerator}: decoded a {w}x{h} image from a 64x48 stream — \
                     no rung may come from outside 1/8..=8/8"
                ),
                Err(err) => assert!(
                    err.contains("out of 1..=8"),
                    "numerator {numerator}: the reason names the range: {err}"
                ),
            }
        }
    }

    /// `scaled_dims` is TurboJPEG's `TJSCALED` (a ceiling division): the
    /// A1's full at 3/8 is the 4K fit, at 2/8 the QHD one, and odd sizes
    /// round UP, exactly as the decoder sizes its output.
    #[test]
    fn scaled_dims_is_the_decoders_ceiling_division() {
        assert_eq!(scaled_dims(8640, 5760, 3), (3240, 2160));
        assert_eq!(scaled_dims(8640, 5760, 2), (2160, 1440));
        assert_eq!(scaled_dims(8640, 5760, 4), (4320, 2880));
        assert_eq!(scaled_dims(8640, 5760, 8), (8640, 5760));
        assert_eq!(scaled_dims(2000, 1500, 3), (750, 563), "562.5 rounds up");
        assert_eq!(
            scaled_dims(1, 1, 1),
            (1, 1),
            "1/8 of one pixel is one pixel"
        );
    }

    /// libjpeg-turbo cannot DCT-scale a LOSSLESS stream (the Cargo.toml
    /// canary, item 4), so `decode_with` runs such a stream at full scale
    /// whatever the rung asked and reports 8 as the numerator it RAN — the
    /// ladder then sees a full, never a screen rung (raw-pipeline.md, "The
    /// screen rung"). Delete the lossless branch and the 3/8 decode is
    /// `Err("... lossless JPEG image cannot be scaled ...")`: a Failed badge
    /// on a lossless bare JPEG. The crate's own compressor encodes the
    /// fixture (senior-developer review 2026-09-26, F2: the step-1 record
    /// said no lossless fixture could be encoded here).
    #[test]
    fn a_lossless_stream_decodes_full_scale_through_the_scaled_entry_point() {
        let img = turbojpeg::Image::mandelbrot(64, 48, turbojpeg::PixelFormat::RGB);
        let mut c = turbojpeg::Compressor::new().unwrap();
        c.set_lossless(true).unwrap();
        let j = c.compress_to_vec(img.as_deref()).unwrap();
        assert!(turbojpeg::read_header(&j).unwrap().is_lossless);
        for (o, want) in [(1u16, (64, 48)), (6, (48, 64))] {
            let d = decode_with(&j, o, 3).expect("lossless at 3/8");
            let (w, h, ran) = (d.width, d.height, d.ran);
            assert_eq!(
                ((w, h), ran),
                (want, 8),
                "a lossless stream runs full scale"
            );
        }
    }

    /// Brief 008 R2, the bound the decoder swap dropped unnoticed: zune-jpeg
    /// 0.4's default refused a progressive stream of more than 100 scans,
    /// and libjpeg-turbo's scan limit defaults to none. A scan is a pass
    /// over every block of the components it covers, so a small crafted
    /// stream with thousands of them would hold a loupe decoder far longer
    /// than any real photo (raw-pipeline.md, "Progressive scans: at most
    /// 100"). Remove `set_scan_limit(100)` from `decode_with` and the
    /// 101-scan stream decodes, at both entry points: this test is red. The
    /// 100-scan stream decoding shows the limit is not lower, and that the
    /// generated progression is valid (a warning would fail it too).
    #[test]
    fn a_progressive_stream_over_100_scans_fails_on_the_loupe_path() {
        let sos_markers = |j: &[u8]| j.windows(2).filter(|w| w == &[0xFF, 0xDA]).count();
        let at_limit = crate::raw::jpeg_hostile::progressive(100);
        let over = crate::raw::jpeg_hostile::progressive(101);
        assert_eq!(
            (sos_markers(&at_limit), sos_markers(&over)),
            (100, 101),
            "the fixtures carry the scans they claim"
        );
        let (_, w, h) = decode_oriented(&at_limit, 1).expect("100 scans decode: the limit is 100");
        assert_eq!((w, h), (8, 8));
        let (_, w, h) =
            decode_scaled_oriented(&at_limit, 1, 3).expect("100 scans decode at 3/8 as well");
        assert_eq!((w, h), (3, 3));
        let outcomes = [
            ("full", decode_oriented(&over, 1)),
            ("3/8", decode_scaled_oriented(&over, 1, 3)),
        ];
        for (entry, outcome) in outcomes {
            match outcome {
                // Not `expect_err`: its message would print the whole buffer.
                Ok((_, w, h)) => panic!(
                    "{entry}: a 101-scan progressive stream decoded as a {w}x{h} success — \
                     the loupe's scan limit is gone"
                ),
                Err(err) => assert!(
                    err.contains("more than 100 scans"),
                    "{entry}: the library's reason must reach the badge: {err}"
                ),
            }
        }
    }

    /// A `Shared` over `paths` with no worker threads, for tests that drive
    /// `decode_ladder` directly and read what it published.
    fn shared_over(paths: Vec<PathBuf>) -> (Shared, std::sync::mpsc::Receiver<LoupeEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Shared {
            state: Mutex::new(LoupeState::default()),
            wakeup: Condvar::new(),
            paths,
            events: tx,
            shutdown: AtomicBool::new(false),
            stamp: AtomicU64::new(0),
            budget: DEFAULT_BUDGET_BYTES,
        };
        (shared, rx)
    }

    /// Brief 008 R14 (raw-pipeline.md A14). libjpeg-turbo refuses CMYK and
    /// YCCK for RGB output ("Unsupported color conversion request", the
    /// Cargo.toml canary, item 8), so from the decoder swap on a
    /// print-ready bare JPEG showed a Failed badge in the loupe where
    /// zune-jpeg had decoded it. Such a stream now takes the zune-jpeg
    /// route: pixels, at full scale whatever rung was asked, with 8 reported
    /// as the numerator run. Delete the route and every decode row below
    /// fails with the library's "Unsupported color conversion request" --
    /// the step-1 code's red.
    ///
    /// The route keeps both hostile-input bounds: a cut CMYK scan is
    /// refused as "truncated" (zune-jpeg alone zero-fills it into a
    /// success), and a CMYK header claiming 30000x30000 as "implausible"
    /// before any buffer exists (zune-jpeg alone would size ~2.7 GB from
    /// it). It keeps them twice over — the call site checks both on
    /// libjpeg-turbo's header, and the route again on zune-jpeg's, since it
    /// is also the second opinion, where no libjpeg-turbo header exists — so
    /// a guard removed from ONE place leaves these rows green, and removed
    /// from both places turns its row red (raw-pipeline.md A14, corrected in
    /// step 2c; the step-2 review's F2: this comment still said "move either
    /// guard after the route and its row is red", true of step 2a only).
    #[test]
    fn cmyk_and_ycck_streams_decode_on_the_loupe_path() {
        use jpeg_encoder::ColorType;
        for (color, colorspace) in [
            (ColorType::Cmyk, turbojpeg::Colorspace::CMYK),
            (ColorType::CmykAsYcck, turbojpeg::Colorspace::YCCK),
        ] {
            let jpeg = crate::raw::jpeg_hostile::encoded_as(64, 48, color);
            assert_eq!(
                turbojpeg::read_header(&jpeg)
                    .expect("the fixture's header reads")
                    .colorspace,
                colorspace,
                "the fixture must be a stream libjpeg-turbo reads as {colorspace:?}"
            );
            for (orientation, want) in [(1u16, (64u32, 48u32)), (6, (48, 64))] {
                let outcomes = [
                    ("full", decode_oriented(&jpeg, orientation)),
                    ("3/8", decode_scaled_oriented(&jpeg, orientation, 3)),
                ];
                for (entry, outcome) in outcomes {
                    match outcome {
                        Ok((rgb, w, h)) => {
                            assert_eq!(
                                (w, h),
                                want,
                                "{colorspace:?} {entry}, orientation {orientation}: \
                                 decoded at full scale and oriented"
                            );
                            assert_eq!(rgb.len(), 64 * 48 * 3, "{colorspace:?} {entry}: RGB");
                        }
                        Err(err) => panic!(
                            "{colorspace:?} {entry}, orientation {orientation}: {err} -- a \
                             Failed badge on a stream zune-jpeg decodes"
                        ),
                    }
                }
                let ran = decode_with(&jpeg, orientation, 3)
                    .map(|d| d.ran)
                    .unwrap_or_else(|err| panic!("{colorspace:?} at 3/8: {err}"));
                assert_eq!(
                    ran, 8,
                    "{colorspace:?}: the route runs full scale, so the ladder sees a full"
                );
            }
            let cut = crate::raw::jpeg_hostile::truncate_scan(&jpeg, 16);
            let mut hostile = jpeg.clone();
            crate::raw::jpeg_hostile::patch_sof_dims(&mut hostile, 30000, 30000);
            let bounds = [
                ("cut, full", decode_oriented(&cut, 1), "truncated"),
                ("cut, 3/8", decode_scaled_oriented(&cut, 1, 3), "truncated"),
                (
                    "30000x30000, full",
                    decode_oriented(&hostile, 1),
                    "implausible",
                ),
                (
                    "30000x30000, 3/8",
                    decode_scaled_oriented(&hostile, 1, 3),
                    "implausible",
                ),
            ];
            for (row, outcome, cause) in bounds {
                match outcome {
                    // Not `expect_err`: its message would print the buffer.
                    Ok((_, w, h)) => panic!(
                        "{colorspace:?} {row}: decoded a {w}x{h} success -- the route \
                         lost the bound that says \"{cause}\""
                    ),
                    Err(err) => assert!(
                        err.contains(cause),
                        "{colorspace:?} {row}: the reason must say \"{cause}\": {err}"
                    ),
                }
            }
        }

        // Through the ladder: a bare CMYK file at the app's 1:1 target shows
        // its one rung, terminal, and the flight emits no Failed.
        let dir = crate::testutil::scratch_dir("cmyk-ladder");
        let path = dir.join("print.jpg");
        std::fs::write(
            &path,
            crate::raw::jpeg_hostile::encoded_as(2000, 1500, ColorType::Cmyk),
        )
        .unwrap();
        let (shared, rx) = shared_over(vec![path]);
        assert_eq!(
            decode_ladder(
                &shared,
                0,
                Target::Long(u32::MAX),
                0,
                false,
                RequestState::Settled
            ),
            Ok(()),
            "a CMYK bare JPEG must not fail its only rung"
        );
        match rx.try_recv() {
            Ok(LoupeEvent::Ready {
                image, terminal, ..
            }) => {
                assert_eq!((image.width, image.height), (2000, 1500));
                assert!(terminal, "a bare JPEG's only rung is its best");
            }
            other => panic!("expected the CMYK rung's Ready event, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "one rung, one event, no Failed");

        // At fit on a box its 2/8 would serve (brief 008): the rung attempt
        // runs the route at full scale, so what comes back is the full, and
        // the ladder publishes it once, terminal -- the plain decode of the
        // same JPEG does not run a second time.
        let fit_box = FitBox {
            width: 500,
            height: 400,
        };
        assert_eq!(
            rung_factor(2000, 1500, 1, fit_box),
            Some(2),
            "the premise: the ladder tries a screen rung here"
        );
        let (shared, rx) = shared_over(vec![dir.join("print.jpg")]);
        assert_eq!(
            decode_ladder(
                &shared,
                0,
                Target::Fit(fit_box),
                0,
                false,
                RequestState::Settled
            ),
            Ok(())
        );
        match rx.try_recv() {
            Ok(LoupeEvent::Ready {
                image, terminal, ..
            }) => {
                assert_eq!(
                    (image.width, image.height, image.kind),
                    (2000, 1500, RungKind::Full),
                    "the route ran 8/8: the full, never a screen rung"
                );
                assert!(terminal, "a bare JPEG's full is its best");
            }
            other => panic!("expected the CMYK full's Ready event, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "one decode of the full, one event, no Failed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Brief 008, other cameras (raw-pipeline.md, "The decoder's
    /// complaints"): every warning text of the vendored message table, the
    /// scan limit's message and an unknown text, each with and without the
    /// safe crate's "TurboJPEG error: " prefix, sorted into the three
    /// classes. The leftover-bytes and bad-ICC texts share the damage texts'
    /// "Corrupt JPEG data" prefix: a class read off the prefix would refuse
    /// them.
    #[test]
    fn libjpeg_turbo_messages_sort_into_three_classes() {
        use Complaint::{Damage, Kept, SecondOpinion};
        let table = [
            ("Premature end of JPEG file", Damage),
            ("Corrupt JPEG data: premature end of data segment", Damage),
            ("Corrupt JPEG data: bad Huffman code", Damage),
            ("Corrupt JPEG data: bad arithmetic code", Damage),
            (
                "Corrupt JPEG data: found marker 0xd5 instead of RST1",
                Damage,
            ),
            (
                "Corrupt JPEG data: found marker 0xd9 instead of RST3",
                Damage,
            ),
            ("Progressive JPEG image has more than 100 scans", Damage),
            ("Invalid SOS parameters for sequential JPEG", Kept),
            (
                "Inconsistent progression sequence for component 0 coefficient 1",
                Kept,
            ),
            (
                "Corrupt JPEG data: 59 extraneous bytes before marker 0xd9",
                Kept,
            ),
            (
                "Corrupt JPEG data: 3 extraneous bytes before marker 0xd0",
                Kept,
            ),
            (
                "Corrupt JPEG data: 3 extraneous bytes before marker 0xda",
                Kept,
            ),
            ("Warning: unknown JFIF revision number 2.01", SecondOpinion),
            ("Corrupt JPEG data: bad ICC marker", SecondOpinion),
            ("Unknown Adobe color transform code 5", SecondOpinion),
            ("Application transferred too many scanlines", SecondOpinion),
            ("Bogus Huffman table definition", SecondOpinion),
            ("Unsupported color conversion request", SecondOpinion),
            (
                "Invalid JPEG file structure: two SOI markers",
                SecondOpinion,
            ),
            ("something unheard of", SecondOpinion),
        ];
        for (text, class) in table {
            assert_eq!(complaint_class(text), class, "{text:?}");
            let prefixed = format!("TurboJPEG error: {text}");
            assert_eq!(complaint_class(&prefixed), class, "{prefixed:?}");
        }
    }

    /// One harmless row's outcome through `decode_with` at `numerator`, and
    /// through both public entry points: `Ok` from all three, and the
    /// `Decoded` to read.
    fn decoded_past(stream: &[u8], numerator: u8, row: &str) -> Decoded {
        if let Err(err) = decode_oriented(stream, 1) {
            panic!("{row}: decode_oriented refused a harmless stream: {err}");
        }
        if let Err(err) = decode_scaled_oriented(stream, 1, numerator) {
            panic!("{row}: decode_scaled_oriented refused a harmless stream: {err}");
        }
        decode_with(stream, 1, numerator)
            .unwrap_or_else(|err| panic!("{row}: decode_with refused a harmless stream: {err}"))
    }

    /// Brief 008, other cameras (raw-pipeline.md, "The decoder's
    /// complaints"; CLAUDE.md M11): a harmless complaint never refuses a
    /// frame. Each row is a stream some writer produces, which libjpeg-turbo
    /// complains about (or not), decoded through `decode_with` and both
    /// public entry points; every row at 3/8 unless it says otherwise, and
    /// "byte-identical" means equal to the intact base's decode at the same
    /// scale. Before the relaxation rows (a)-(e) and (g)-(i) were refused;
    /// (f) and (j) pin what it must not touch.
    #[test]
    fn harmless_complaints_decode_on_the_loupe_path() {
        use crate::raw::jpeg_hostile::{
            before_eoi, find, find_nth, insert, mandelbrot_baseline, mandelbrot_progressive,
            mandelbrot_restart, progressive_six_scans,
        };
        let base = mandelbrot_baseline();
        let base_3 = decode_with(&base, 1, 3).expect("the base decodes").rgb;
        let same_pixels = |d: &Decoded, reference: &[u8], row: &str| {
            assert_eq!(
                (d.width, d.height, d.ran),
                (384, 288, 3),
                "{row}: decoded at the rung asked for"
            );
            assert!(
                d.rgb == reference,
                "{row}: the image must be byte-identical to the intact stream's"
            );
        };
        let note = |d: &Decoded| d.note.clone().unwrap_or_default();

        // (a) Three non-FF bytes before the first DQT: the pre-pass drops
        // them, and libjpeg-turbo decodes at the rung.
        let dqt = find(&base, &[0xFF, 0xDB]).expect("a DQT");
        let d = decoded_past(&insert(&base, dqt, &[0x01, 0x02, 0x03]), 3, "(a)");
        same_pixels(&d, &base_3, "(a)");
        assert!(
            note(&d).contains("3 bytes"),
            "(a) names the gap: {:?}",
            d.note
        );

        // (b) The SOS's Se byte 0 (`JWRN_NOT_SEQUENTIAL`: "there are some
        // baseline files out there with all zeroes in these bytes").
        let sos = find(&base, &[0xFF, 0xDA]).expect("an SOS");
        let sos_len = usize::from(u16::from_be_bytes([base[sos + 2], base[sos + 3]]));
        let se = sos + 2 + sos_len - 2; // Ss, Se, Ah/Al close the header
        assert_eq!(base[se], 63, "the premise: Se is 63 in a baseline scan");
        let mut not_sequential = base.clone();
        not_sequential[se] = 0;
        let d = decoded_past(&not_sequential, 3, "(b)");
        same_pixels(&d, &base_3, "(b)");
        assert!(
            note(&d).contains("Invalid SOS parameters"),
            "(b): {:?}",
            d.note
        );

        // (c) A progressive stream whose first AC band's refinement comes
        // before its first scan (`JWRN_BOGUS_PROGRESSION`); its twin in
        // order has nothing to say.
        let d = decoded_past(&progressive_six_scans(true), 3, "(c)");
        assert_eq!((d.width, d.height), (3, 3), "(c) at 3/8");
        assert!(
            note(&d).contains("Inconsistent progression"),
            "(c): {:?}",
            d.note
        );
        let d = decoded_past(&progressive_six_scans(false), 3, "(c) twin");
        assert_eq!(d.note, None, "(c) the twin in order is clean");

        // (d) A JFIF APP0 of major version 2: libjpeg-turbo's header read
        // fails, zune-jpeg's second opinion decodes it — full scale.
        let jfif = find(&base, b"JFIF\0").expect("the base's JFIF APP0");
        let mut jfif2 = base.clone();
        jfif2[jfif + 5] = 2;
        let d = decoded_past(&jfif2, 3, "(d)");
        assert_eq!(
            (d.width, d.height, d.ran),
            (1024, 768, 8),
            "(d) the second opinion runs full scale"
        );
        assert!(
            note(&d).contains("JFIF") && note(&d).contains("zune-jpeg"),
            "(d): {:?}",
            d.note
        );

        // (e) An ICC_PROFILE chunk numbered 0, after SOI: a header complaint
        // (TurboJPEG saves APP2), so as (d).
        let mut icc = b"\xFF\xE2\x00\x14ICC_PROFILE\x00\x00\x01".to_vec();
        icc.extend_from_slice(&[0; 4]);
        let d = decoded_past(&insert(&base, 2, &icc), 3, "(e)");
        assert_eq!((d.width, d.height, d.ran), (1024, 768, 8), "(e)");
        assert!(note(&d).contains("bad ICC marker"), "(e): {:?}", d.note);

        // (f) One byte, and three, of junk before EOI: the Huffman decoder's
        // bit buffer read them ahead and drops them at the scan's end
        // uncounted (the canary, item 12) — no message at all.
        for junk in [1usize, 3] {
            let row = format!("(f) {junk} junk bytes before EOI");
            let d = decoded_past(&before_eoi(&base, &vec![0x11; junk]), 3, &row);
            same_pixels(&d, &base_3, &row);
            assert_eq!(d.note, None, "{row}: nothing to say");
        }

        // (g) 64 junk bytes before EOI: `JWRN_EXTRANEOUS_DATA` from the
        // decode — a writer's padding — and libjpeg-turbo's image is kept.
        let d = decoded_past(&before_eoi(&base, &[0x11; 64]), 3, "(g)");
        same_pixels(&d, &base_3, "(g)");
        assert!(note(&d).contains("extraneous bytes"), "(g): {:?}", d.note);

        // (h) Three bytes before the restart base's first RST0.
        let restart = mandelbrot_restart();
        let restart_3 = decode_with(&restart, 1, 3)
            .expect("the restart base decodes")
            .rgb;
        let rst0 = find(&restart, &[0xFF, 0xD0]).expect("an RST0");
        let d = decoded_past(&insert(&restart, rst0, &[0x01, 0x02, 0x03]), 3, "(h)");
        same_pixels(&d, &restart_3, "(h)");
        assert!(note(&d).contains("extraneous bytes"), "(h): {:?}", d.note);

        // (i) Three bytes between a progressive stream's DHT and its next
        // SOS — a later scan's header, past the pre-pass, which stops at the
        // first SOS.
        let progressive = mandelbrot_progressive();
        let progressive_3 = decode_with(&progressive, 1, 3)
            .expect("the progressive base decodes")
            .rgb;
        let sos2 = find_nth(&progressive, &[0xFF, 0xDA], 1).expect("a second SOS");
        let dht_before = (0..sos2).any(|at| {
            progressive[at..].starts_with(&[0xFF, 0xC4])
                && at
                    + 2
                    + usize::from(u16::from_be_bytes([
                        progressive[at + 2],
                        progressive[at + 3],
                    ]))
                    == sos2
        });
        assert!(
            dht_before,
            "(i) the premise: a DHT ends where the second SOS begins"
        );
        let d = decoded_past(&insert(&progressive, sos2, &[0x01, 0x02, 0x03]), 3, "(i)");
        same_pixels(&d, &progressive_3, "(i)");
        assert!(note(&d).contains("extraneous bytes"), "(i): {:?}", d.note);

        // (j) An Adobe APP14 of transform 5 on the three-component base, its
        // JFIF APP0 kept: libjpeg-turbo takes JFIF's colours and says
        // nothing.
        let adobe5 = insert(
            &base,
            2,
            b"\xFF\xEE\x00\x0EAdobe\x00\x64\x00\x00\x00\x00\x05",
        );
        let d = decoded_past(&adobe5, 3, "(j)");
        same_pixels(&d, &base_3, "(j)");
        assert_eq!(d.note, None, "(j) nothing to say");
    }

    /// Brief 008, other cameras: the damage class — the messages only a
    /// damaged stream raises — is refused with libjpeg-turbo's own message
    /// and NO second opinion, through both entry points. For (a), (d1) and
    /// (d2) zune-jpeg alone would call the stream a success (the premise
    /// asserted below), so a second opinion taken there would show; (b),
    /// (c) and (f) are told apart by the text alone, zune-jpeg refusing
    /// them in words of its own. (d2) decodes intact in both decoders and is
    /// refused all the same: the class is the message's, not the pixels'.
    #[test]
    fn the_damage_class_is_refused_without_a_second_opinion() {
        use crate::raw::jpeg_hostile::{
            encoded, find, find_nth, insert, mandelbrot_progressive, mandelbrot_restart,
            progressive, truncate_scan,
        };
        let refused = |stream: &[u8], text: &str, row: &str| {
            for (entry, outcome) in [
                ("full", decode_oriented(stream, 1)),
                ("3/8", decode_scaled_oriented(stream, 1, 3)),
            ] {
                match outcome {
                    Ok((_, w, h)) => {
                        panic!("{row} {entry}: a damaged stream decoded as a {w}x{h} success")
                    }
                    Err(err) => {
                        assert!(
                            err.contains(text),
                            "{row} {entry}: libjpeg-turbo's own \"{text}\" must be the reason: {err}"
                        );
                        assert!(
                            !err.contains("zune"),
                            "{row} {entry}: no second opinion for damage: {err}"
                        );
                    }
                }
            }
        };
        let zune_alone_decodes = |stream: &[u8], row: &str| {
            if let Err(err) = decode_through_zune(stream, 1) {
                panic!("{row}: the premise is that zune-jpeg alone decodes this stream: {err}");
            }
        };

        // (a) The A8 short scan: cut 16 bytes in, a valid EOI appended.
        let mut short = truncate_scan(&encoded(64, 64), 16);
        short.extend_from_slice(&[0xFF, 0xD9]);
        zune_alone_decodes(&short, "(a)");
        refused(&short, "premature end of data segment", "(a)");

        // (b) `JWRN_JPEG_EOF` past the byte check: a DQT redefining table 3,
        // two adjacent entries 0xFF 0xD9, before the progressive base's
        // third SOS; the stream cut 200 bytes after its sixth SOS header.
        let base = mandelbrot_progressive();
        let sos3 = find_nth(&base, &[0xFF, 0xDA], 2).expect("a third SOS");
        let mut dqt = vec![0xFF, 0xDB, 0x00, 0x43, 0x03];
        let mut table = [1u8; 64];
        table[10] = 0xFF;
        table[11] = 0xD9;
        dqt.extend_from_slice(&table);
        let with_dqt = insert(&base, sos3, &dqt);
        let sos6 = find_nth(&with_dqt, &[0xFF, 0xDA], 5).expect("a sixth SOS");
        let sos6_end =
            sos6 + 2 + usize::from(u16::from_be_bytes([with_dqt[sos6 + 2], with_dqt[sos6 + 3]]));
        let cut = with_dqt[..sos6_end + 200].to_vec();
        assert!(
            crate::raw::scan_is_terminated(&cut),
            "(b) the premise: the table's FF D9 passes the byte check"
        );
        refused(&cut, "Premature end of JPEG file", "(b)");

        // (c) An all-ones run — 16 FF 00 pairs — 40 bytes into the first scan
        // of the progressive base: no Huffman code is all ones.
        let first_sos = find(&base, &[0xFF, 0xDA]).expect("an SOS");
        let first_scan = first_sos
            + 2
            + usize::from(u16::from_be_bytes([
                base[first_sos + 2],
                base[first_sos + 3],
            ]));
        let all_ones = insert(&base, first_scan + 40, &[0xFF, 0x00].repeat(16));
        refused(&all_ones, "bad Huffman code", "(c)");

        // (d1) The restart base's second RST renumbered one ahead (RST1 ->
        // RST2): libjpeg-turbo zero-fills an interval. (d2) Four ahead
        // (RST1 -> RST5): every block decodes intact, and the stream is
        // refused all the same.
        let restart = mandelbrot_restart();
        let rst1 = find(&restart, &[0xFF, 0xD1]).expect("an RST1");
        for (to, row) in [(0xD2u8, "(d1)"), (0xD5, "(d2)")] {
            let mut renumbered = restart.clone();
            renumbered[rst1 + 1] = to;
            zune_alone_decodes(&renumbered, row);
            refused(&renumbered, "instead of RST", row);
        }

        // (f) The 101-scan progressive stream: the scan limit is
        // libjpeg-turbo's refusal, never zune-jpeg's "Too many scans".
        let over = progressive(101);
        refused(&over, "more than 100 scans", "(f)");
        for outcome in [
            decode_oriented(&over, 1),
            decode_scaled_oriented(&over, 1, 3),
        ] {
            if let Err(err) = outcome {
                assert!(!err.contains("Too many scans"), "(f): {err}");
            }
        }
    }

    /// Brief 008, other cameras: the second opinion keeps the bounds. After
    /// a header read that failed there is no libjpeg-turbo header to check,
    /// so the zune-jpeg route checks the pixel cap and the byte check on its
    /// own reading of the stream, and keeps zune-jpeg's own scan limit; when
    /// zune-jpeg refuses too, both decoders' reasons are named.
    #[test]
    fn the_second_opinion_keeps_the_bounds() {
        use crate::raw::jpeg_hostile::{
            find, insert, mandelbrot_baseline, patch_sof_dims, progressive, truncate_scan,
        };
        let refused = |stream: &[u8], texts: &[&str], row: &str| {
            for (entry, outcome) in [
                ("full", decode_oriented(stream, 1)),
                ("3/8", decode_scaled_oriented(stream, 1, 3)),
            ] {
                match outcome {
                    Ok((_, w, h)) => panic!("{row} {entry}: decoded a {w}x{h} success"),
                    Err(err) => {
                        for text in texts {
                            assert!(err.contains(text), "{row} {entry}: \"{text}\" in {err}");
                        }
                    }
                }
            }
        };
        let base = mandelbrot_baseline();
        let jfif = find(&base, b"JFIF\0").expect("the base's JFIF APP0");
        let mut jfif2 = base.clone();
        jfif2[jfif + 5] = 2;

        // (a) Claiming 30000x30000 behind the JFIF complaint: the route's own
        // pixel cap, before any buffer.
        let mut hostile = jfif2.clone();
        patch_sof_dims(&mut hostile, 30000, 30000);
        refused(&hostile, &["implausible"], "(a)");

        // (b) Cut before EOI behind the JFIF complaint: the route's own byte
        // check (zune-jpeg would zero-fill the rest into a success).
        refused(&truncate_scan(&jfif2, 64), &["truncated"], "(b)");

        // (c) 101 scans behind a JFIF APP0 of revision 2: libjpeg-turbo
        // refuses the JFIF revision before any scan, so the limit that fires
        // is zune-jpeg's own.
        let jfif2_app0 = b"\xFF\xE0\x00\x10JFIF\x00\x02\x01\x00\x00\x01\x00\x01\x00\x00";
        refused(
            &insert(&progressive(101), 2, jfif2_app0),
            &["Too many scans", "JFIF"],
            "(c)",
        );

        // (d) Both refuse: the base without its JFIF APP0, an Adobe APP14 of
        // transform 5 after SOI.
        let app0_len = usize::from(u16::from_be_bytes([base[jfif - 2], base[jfif - 1]]));
        let app0 = jfif - 4;
        assert_eq!(
            &base[app0..app0 + 2],
            &[0xFF, 0xE0],
            "the premise: the APP0"
        );
        let mut no_jfif = base[..app0].to_vec();
        no_jfif.extend_from_slice(&base[app0 + 2 + app0_len..]);
        let adobe5 = insert(
            &no_jfif,
            2,
            b"\xFF\xEE\x00\x0EAdobe\x00\x64\x00\x00\x00\x00\x05",
        );
        refused(
            &adobe5,
            &[
                "Unknown Adobe color transform code 5",
                "Unknown Adobe colorspace 5",
            ],
            "(d)",
        );
    }

    /// The environment variable that turns [`stderr_child`] on, holding
    /// "<scenario>|<directory>".
    const STDERR_CHILD_VAR: &str = "FASTCULL_LOUPE_STDERR_CHILD";

    /// The child half of the two stderr tests: runs only in the child process
    /// they start from this same test binary, and returns at once in any
    /// other run (the `tests/xmp_crash.rs` pattern, no `#[ignore]`). It
    /// climbs each scenario's files through `decode_ladder` at the targets
    /// the scenario names — the top rung, as a 1:1 view asks, and the fit
    /// box, where the SCREEN rung is decoded (the "-fit" scenarios; the
    /// step-2 review's F1: the screen rung's branches print through their own
    /// call sites) — so whatever it prints on stderr is what the app would.
    #[test]
    fn stderr_child() {
        let Some(spec) = std::env::var_os(STDERR_CHILD_VAR) else {
            return; // not the child: nothing to do
        };
        let spec = spec.to_string_lossy().into_owned();
        let (scenario, dir) = spec.split_once('|').expect("<scenario>|<directory>");
        let dir = PathBuf::from(dir);
        let top = Target::Long(u32::MAX);
        let fit = |width, height| Target::Fit(FitBox { width, height });
        // (file, the targets it is climbed at, in order)
        let plan: Vec<(&str, Vec<Target>)> = match scenario {
            "damaged" => vec![
                ("mid_ok_full_cut.arw", vec![top]),
                ("mid_ok_full_ok.arw", vec![top]),
            ],
            "damaged-fit" => vec![
                ("mid_ok_full_cut.arw", vec![fit(1000, 700)]),
                ("mid_ok_full_ok.arw", vec![fit(1000, 700)]),
            ],
            // QE round 1's D1: the FILE cut inside its full.
            "cut-short" => vec![
                ("file_cut_in_full.arw", vec![top]),
                ("mid_ok_full_ok.arw", vec![top]),
            ],
            "cut-short-fit" => vec![
                ("file_cut_in_full.arw", vec![fit(1000, 700)]),
                ("mid_ok_full_ok.arw", vec![fit(1000, 700)]),
            ],
            "complaint" => ["full_jfif2.arw", "gap.jpg", "pad.jpg", "intact.jpg"]
                .into_iter()
                .map(|f| (f, vec![top, top]))
                .collect(),
            "complaint-fit" => vec![
                ("full_jfif2.arw", vec![fit(1000, 700), fit(1000, 700)]),
                ("pad.jpg", vec![fit(300, 200), fit(300, 200), top]),
                ("both.arw", vec![top, top]),
            ],
            other => panic!("unknown scenario {other}"),
        };
        let (shared, _events) = shared_over(plan.iter().map(|(f, _)| dir.join(f)).collect());
        for (index, (_, targets)) in plan.iter().enumerate() {
            for target in targets {
                // Nothing in hand on any climb, as before: every climb
                // decodes again, so the once-memo is what keeps it to one
                // line.
                let _ = decode_ladder(&shared, index, *target, 0, false, RequestState::Settled);
            }
        }
        if scenario == "complaint" {
            // The grid thumb decodes the gapped and the padded JPEG too, and
            // must print nothing: the complaint line is the loupe's.
            for file in ["gap.jpg", "pad.jpg"] {
                let spec = crate::pipeline::JobSpec {
                    path: dir.join(file),
                    size: 0,
                    mtime: None,
                };
                crate::pipeline::make_grid_thumb(&spec)
                    .unwrap_or_else(|err| panic!("the grid thumb of {file}: {err}"));
            }
        }
    }

    /// Run [`stderr_child`] in a child process of this test binary over
    /// `dir` and return what it printed on stderr.
    fn stderr_of_child(scenario: &str, dir: &std::path::Path) -> String {
        let exe = std::env::current_exe().expect("the test binary");
        let out = std::process::Command::new(exe)
            .args([
                "loupe::tests::stderr_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(STDERR_CHILD_VAR, format!("{scenario}|{}", dir.display()))
            .output()
            .expect("the child runs");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "the child failed:\n{stderr}");
        stderr
    }

    /// A TIFF container, as a RAW is: IFD0 an intact 640x400 mid, the second
    /// IFD `full` (`truncated_full_rung_keeps_the_good_mid_and_no_failed_badge`'s
    /// layout).
    fn raw_with_full(full: &[u8]) -> Vec<u8> {
        raw_with(&crate::raw::jpeg_hostile::encoded(640, 400), full)
    }

    /// A TIFF container with IFD0 the given `mid` and the second IFD `full`.
    fn raw_with(mid: &[u8], full: &[u8]) -> Vec<u8> {
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let mid_off = b.add_blob(mid);
        let full_off = b.add_blob(full);
        let second = b.add_ifd(
            &[(0x0201, 4, 1, full_off), (0x0202, 4, 1, full.len() as u32)],
            0,
        );
        let ifd0 = b.add_ifd(
            &[(0x0201, 4, 1, mid_off), (0x0202, 4, 1, mid.len() as u32)],
            second,
        );
        b.set_ifd0(ifd0);
        b.bytes
    }

    /// raw-pipeline.md, "All rejections" (brief 008, the step-1 review): a
    /// higher rung that fails while a good lower one is in hand fails
    /// nothing — no Failed badge, the lower rung shown — so ONE stderr line
    /// names the file, the rung that failed and the decoder's reason, or a
    /// fault that shows no badge would go unseen. Read from a child
    /// process's stderr over a RAW whose full is cut before EOI and a control
    /// whose full is intact, climbed at the top rung and, in a second child,
    /// at a fit box whose screen rung is the one that fails (the step-2
    /// review's F1: that failure reaches the line through its own call site).
    /// (One climb each: in the app the memo stops the second, which
    /// `truncated_full_rung_keeps_the_good_mid_and_no_failed_badge` pins.)
    #[test]
    fn a_rung_that_fails_over_a_good_lower_one_is_named_on_stderr() {
        let dir = crate::testutil::scratch_dir("stderr-damaged");
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        std::fs::write(
            dir.join("mid_ok_full_cut.arw"),
            raw_with_full(&crate::raw::jpeg_hostile::truncate_scan(&full, 64)),
        )
        .unwrap();
        std::fs::write(dir.join("mid_ok_full_ok.arw"), raw_with_full(&full)).unwrap();

        let stderr = stderr_of_child("damaged", &dir);
        let lines: Vec<&str> = stderr
            .lines()
            .filter(|l| l.starts_with("fastcull: loupe "))
            .collect();
        let named: Vec<&&str> = lines
            .iter()
            .filter(|l| l.contains("mid_ok_full_cut.arw"))
            .collect();
        assert_eq!(named.len(), 1, "one line for the damaged rung:\n{stderr}");
        assert!(
            named[0].contains("the full rung") && named[0].contains("truncated"),
            "it names the rung and the reason: {}",
            named[0]
        );
        assert!(
            lines.iter().all(|l| !l.contains("mid_ok_full_ok.arw")),
            "a clean climb prints nothing:\n{stderr}"
        );
        // At FIT on a 1000x700 box the mid does not serve and the 3/8 SCREEN
        // rung is what fails over it: its failure reaches the line through
        // the screen branch's own arm of `decode_ladder`, not the plain
        // decode's.
        let stderr = stderr_of_child("damaged-fit", &dir);
        let named: Vec<&str> = stderr
            .lines()
            .filter(|l| l.starts_with("fastcull: loupe ") && l.contains("mid_ok_full_cut.arw"))
            .collect();
        assert_eq!(
            named.len(),
            1,
            "one line for the damaged screen rung:\n{stderr}"
        );
        assert!(
            named[0].contains("the screen rung") && named[0].contains("truncated"),
            "it names the screen rung and the reason: {}",
            named[0]
        );
        assert!(
            stderr.lines().all(|l| !l.contains("mid_ok_full_ok.arw")),
            "a clean climb at fit prints nothing:\n{stderr}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// QE round 1 of brief 008, D1 (raw-pipeline.md, "Hostile-input bounds"
    /// and "All rejections"; docs/faq.md): a RAW whose FILE was cut inside its
    /// full — an interrupted copy — names the cut on stderr, one line with
    /// the file, the full rung and "truncated", climbed at the top rung and,
    /// in a second child, at a fit box the mid does not serve, where the
    /// full's read fails before any scale is chosen; a control RAW whose full
    /// is whole prints nothing. Red on the walker that dropped a JPEG the file
    /// ends inside: the full was never tried, and no line was printed.
    #[test]
    fn a_raw_cut_inside_its_full_is_named_on_stderr() {
        let dir = crate::testutil::scratch_dir("stderr-cut-short");
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        // The A1's shape: the full's size in its IFD.
        std::fs::write(
            dir.join("file_cut_in_full.arw"),
            raw_cut_inside_its_full(&full, full.len() / 2, true),
        )
        .unwrap();
        std::fs::write(dir.join("mid_ok_full_ok.arw"), raw_with_full(&full)).unwrap();
        for scenario in ["cut-short", "cut-short-fit"] {
            let stderr = stderr_of_child(scenario, &dir);
            let named: Vec<&str> = stderr
                .lines()
                .filter(|l| l.starts_with("fastcull: loupe ") && l.contains("file_cut_in_full.arw"))
                .collect();
            assert_eq!(
                named.len(),
                1,
                "{scenario}: one line for the file cut inside its full:\n{stderr}"
            );
            assert!(
                named[0].contains("the full rung") && named[0].contains("truncated"),
                "{scenario}: it names the rung and the cause: {}",
                named[0]
            );
            assert!(
                stderr.lines().all(|l| !l.contains("mid_ok_full_ok.arw")),
                "{scenario}: a whole file prints nothing:\n{stderr}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// raw-pipeline.md, "One line on stderr, once" (brief 008, other
    /// cameras; M11): a rung decoded past a complaint prints one line naming
    /// the file, the rung and what the loupe did — at most once per session
    /// for each embedded JPEG. In one child: a RAW whose full says JFIF
    /// revision 2 (the second opinion), a bare JPEG with three bytes before
    /// its DQT (a gap skipped) and one with 64 junk bytes before EOI (the
    /// image kept), each climbed TWICE, print one line each; an intact JPEG
    /// prints nothing; and the grid thumb's decode of the gapped and the
    /// padded JPEG in the same child adds no line. A second child climbs at
    /// fit, where the screen rung's two branches print through their own
    /// call sites (the step-2 review's F1), and pins the memo's key: one
    /// line per embedded JPEG of a file, not per file.
    #[test]
    fn a_harmless_complaint_is_named_on_stderr_once() {
        use crate::raw::jpeg_hostile::{before_eoi, encoded, find, insert, mandelbrot_baseline};
        let dir = crate::testutil::scratch_dir("stderr-complaint");
        // `jpeg_encoder` writes a JFIF APP0 (measured); its major version to 2.
        let mut jfif2 = encoded(2000, 1500);
        let jfif = find(&jfif2, b"JFIF\0").expect("jpeg_encoder's JFIF APP0");
        jfif2[jfif + 5] = 2;
        std::fs::write(dir.join("full_jfif2.arw"), raw_with_full(&jfif2)).unwrap();
        let base = mandelbrot_baseline();
        let dqt = find(&base, &[0xFF, 0xDB]).expect("a DQT");
        std::fs::write(dir.join("gap.jpg"), insert(&base, dqt, &[0x01, 0x02, 0x03])).unwrap();
        std::fs::write(dir.join("pad.jpg"), before_eoi(&base, &[0x11; 64])).unwrap();
        std::fs::write(dir.join("intact.jpg"), &base).unwrap();

        let stderr = stderr_of_child("complaint", &dir);
        for (file, words) in [
            ("full_jfif2.arw", ["the full rung", "JFIF"]),
            ("gap.jpg", ["the full rung", "3 bytes"]),
            ("pad.jpg", ["the full rung", "extraneous"]),
        ] {
            let named: Vec<&str> = stderr.lines().filter(|l| l.contains(file)).collect();
            assert_eq!(
                named.len(),
                1,
                "exactly one line for {file} over two climbs and a grid thumb:\n{stderr}"
            );
            assert!(named[0].starts_with("fastcull: loupe "), "{}", named[0]);
            for word in words {
                assert!(
                    named[0].contains(word),
                    "{file}: \"{word}\" in {}",
                    named[0]
                );
            }
        }
        assert!(
            stderr.lines().all(|l| !l.contains("intact.jpg")),
            "an intact file prints nothing:\n{stderr}"
        );
        // At FIT: the screen rung kept past a complaint (pad.jpg at 2/8,
        // `decode_ladder`'s outcome (c)) and the second opinion's full asked
        // for a screen rung (full_jfif2.arw on a 1000x700 box, outcome (b))
        // print their line from the screen branch — once across the fit and
        // the 1:1 climbs of one JPEG. A RAW whose mid AND full both complain
        // prints one line for EACH embedded JPEG: the memo is keyed by the
        // JPEG's offset in the file, not by the file.
        let mut mid2 = encoded(640, 400);
        let at = find(&mid2, b"JFIF\0").expect("the mid's JFIF APP0");
        mid2[at + 5] = 2;
        std::fs::write(dir.join("both.arw"), raw_with(&mid2, &jfif2)).unwrap();
        let stderr = stderr_of_child("complaint-fit", &dir);
        for (file, words) in [
            ("full_jfif2.arw", ["the full rung", "JFIF"]),
            ("pad.jpg", ["the screen rung", "extraneous"]),
        ] {
            let named: Vec<&str> = stderr.lines().filter(|l| l.contains(file)).collect();
            assert_eq!(
                named.len(),
                1,
                "exactly one line for {file} over its climbs at fit (and 1:1):\n{stderr}"
            );
            for word in words {
                assert!(
                    named[0].contains(word),
                    "{file}: \"{word}\" in {}",
                    named[0]
                );
            }
        }
        let both: Vec<&str> = stderr.lines().filter(|l| l.contains("both.arw")).collect();
        assert_eq!(both.len(), 2, "one line per embedded JPEG:\n{stderr}");
        assert!(both.iter().any(|l| l.contains("the mid rung")), "{both:?}");
        assert!(both.iter().any(|l| l.contains("the full rung")), "{both:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn eviction_keeps_newest_and_at_least_one() {
        let mut state = LoupeState::default();
        for i in 0..4usize {
            let img = FullImage {
                rgb: Arc::new(vec![0; 100]),
                width: 10,
                height: 10,
                kind: RungKind::Full,
            };
            state.cached_bytes += 100;
            state.cache.insert(i, (img, i as u64));
        }
        evict_to_budget(&mut state, 250);
        assert!(state.cache.len() <= 2 && state.cache.contains_key(&3));
        evict_to_budget(&mut state, 0);
        assert_eq!(state.cache.len(), 1, "never evicts the last image");
    }
}
