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
//! `focus(index, display_long)` schedules the focused image at top priority
//! and prefetches ±PREFETCH neighbors — in VIEW order, the order arrows
//! actually travel (`set_view`; issue #46): an id-space ring on a
//! capture-sorted multi-body folder warmed frames no arrow could reach
//! while every real neighbor stayed cold. A byte-budget LRU (default
//! 2 GiB) evicts the least recently focused images, never the focused one.

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use crate::raw::{find_embedded_jpegs, read_jpeg};

/// Neighbors prefetched on each side of the focused image.
pub const PREFETCH: usize = 2;
/// Default decoded-pixels budget (bytes of RGB kept in the LRU).
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

/// Ring members as image ids for view positions `lo..=hi` around the
/// focused position `fpos`: farthest first (workers pop from the back, so
/// the nearest neighbor is popped soonest), the focused position excluded
/// (its id is pushed last by the caller). Positions that map to no id
/// (stale view) are skipped rather than guessed.
fn ring_ids(state: &LoupeState, fpos: usize, lo: usize, hi: usize) -> Vec<usize> {
    let mut ring: Vec<(usize, usize)> = (lo..=hi)
        .filter(|p| *p != fpos)
        .filter_map(|p| state.id_at(p).map(|id| (p, id)))
        .collect();
    ring.sort_by_key(|(p, _)| std::cmp::Reverse(p.abs_diff(fpos)));
    ring.into_iter().map(|(_, id)| id).collect()
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
    pub fn start(paths: Vec<PathBuf>, budget: usize) -> (Self, Receiver<LoupeEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(LoupeState::default()),
            wakeup: Condvar::new(),
            paths,
            events: tx,
            shutdown: AtomicBool::new(false),
            stamp: AtomicU64::new(0),
            budget: budget.max(200 * 1024 * 1024), // room for at least one A1
        });
        // Two backlog workers plus ONE focus-reserved worker (see
        // next_job/FOCUS_DEBOUNCE/note_focus): the reserved thread only
        // commits to a focus whose pending work has HELD for the
        // debounce, so neither transient transit focuses nor a climb
        // freshly escalated on a resting frame capture it — the lane is
        // free at the first settle after sub-debounce transits, and
        // that frame's ladder starts within ~debounce even when both
        // backlog workers are mid-flight on multi-second decodes.
        // Worst-case transient memory grows by one concurrent decode
        // (~150 MB for an A1 full-res) — bounded and short-lived.
        let workers = (0..3)
            .map(|n| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || worker(&shared, n == 2))
            })
            .collect();
        (Self { shared, workers }, rx)
    }

    /// The user is looking at `index` on a display whose longest edge is
    /// `display_long` physical pixels: ensure it and its ±PREFETCH neighbors
    /// — in VIEW order (see `set_view`) — have an asset sufficient for that
    /// display (ladder rule) or are queued. Returns the best cached image
    /// immediately (which may be a lower rung — a better one arrives as an
    /// event once cooked).
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
    /// brief 008 (raw-pipeline.md, "An engine with no fit box").
    pub fn set_fit_box(&self, fit_box: Option<FitBox>) {
        let fit_box = fit_box.filter(|b| b.width > 0 && b.height > 0);
        lock(&self.shared).fit_box = fit_box;
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
/// the unit tests drive it without workers: note the focus, decide
/// TRANSIT vs SETTLED, plan the ring, schedule it — farthest first, the
/// focused index last (the back of the queue, popped first) — and return
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
    let transit = in_transit(state, now);
    let req = if transit {
        RequestState::Transit
    } else {
        RequestState::Settled
    };
    // TRANSIT vs SETTLED (user requirement 2026-08-01). Moving: ask only for
    // what a moving frame needs (`transit_request`), across a wide ring
    // leaning the way we travel, so the workers keep up with a held key.
    // Stopped: ask for what the app actually wants, over the tight ring,
    // which is the pre-existing behaviour and is what keeps tap-stepping
    // through a burst sharp.
    //
    // The ring is planned in VIEW-POSITION space and mapped back to image
    // ids at request time (issue #46): arrows travel the view. A focused id
    // with no view position (filtered out mid-flight) gets no neighbors —
    // its neighbors are unknowable, and guessing in id space is the bug
    // this replaced.
    let (request, wanted) = match state.pos_of(index) {
        Some(fpos) => {
            let (request, lo, hi) = focus_plan(
                transit,
                state.travel_forward,
                fpos,
                desired,
                state.fit_box,
                state.ring_len(count),
            );
            (request, ring_ids(state, fpos, lo, hi))
        }
        None => (plan_request(transit, desired, state.fit_box), Vec::new()),
    };
    // Farthest neighbors first, focused index last (back of the queue =
    // popped first by workers).
    let mut wanted: Vec<usize> = wanted.into_iter().filter(|i| *i < count).collect();
    wanted.push(index);
    for i in wanted {
        schedule(state, i, request, stamp, Origin::Focus, req);
    }
    state.cache.get(&index).map(|(img, _)| img.clone())
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
/// rung grew mid-decode). Revived ONLY while the index is still inside the
/// focused prefetch ring: a stale upgrade — the cursor moved on while the
/// flight decoded — re-queued at top priority captured BOTH workers for
/// multi-second full-res decodes and starved the current frame's ladder
/// (Windows CI 2026-07-27: three screenshot tests hit the 60 s shutter cap
/// exactly this way). Dropping a stale upgrade loses nothing: focus()
/// re-requests it the moment the user returns. The focused index re-queues
/// at the back (popped next); a ring neighbor goes to the front so it can
/// never outrank the focused frame's own pending work. The revived entry
/// carries `req`, the request state stored beside the deferred target —
/// never the mode at revival (raw-pipeline.md, "The request state travels
/// with the decode").
fn revive_deferred(
    state: &mut LoupeState,
    index: usize,
    target: Target,
    req: RequestState,
    stamp: u64,
) -> bool {
    // Ring membership in VIEW positions (issue #46), like the ring itself:
    // an id 2 away can be a view-order stranger, and a view neighbor can
    // be any id at all. No position (filtered out) = not in the ring.
    let in_ring = state
        .focused
        .is_some_and(|f| match (state.pos_of(index), state.pos_of(f)) {
            (Some(a), Some(b)) => a.abs_diff(b) <= PREFETCH,
            _ => false,
        });
    if !in_ring || state.failed.contains(&index) || sufficient_cached(state, index, target, stamp) {
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
        state.moving = state
            .last_index_change
            .is_some_and(|t| now.saturating_duration_since(t) <= TRANSIT_GAP);
        state.last_index_change = Some(now);
        // Latch direction HERE, where a real change proves it — in VIEW
        // positions (issue #46): arrows travel the view, and comparing
        // ids on an interleaved view read a steady forward hold as
        // jumping around, flapping the ring's lean. No previous focus,
        // or one no longer in the view: forward (matching the pre-#46
        // first-focus default).
        let new_pos = state.pos_of(index);
        let prev_pos = state.focused.and_then(|p| state.pos_of(p));
        state.travel_forward = match (new_pos, prev_pos) {
            (Some(n), Some(p)) => n >= p,
            _ => true,
        };
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

/// Transit prefetch is DIRECTIONAL: reading ten frames behind while the
/// user flies forward is waste, so the ring leans the way they travel and
/// flips when they reverse. Wide is affordable only because transit asks
/// for no more than the fit box — at most a screen rung, ~5 MB for a mid and
/// ~21 MB for a 4K rung, each far less than ONE 149 MB full-res frame.
const TRANSIT_AHEAD: usize = 8;
const TRANSIT_BEHIND: usize = 2;

/// Is the user MOVING between frames (held key, `[`/`]`, a Y/N
/// auto-advance chain) rather than looking at one?
///
/// While true the engine asks for no more than the fit box, however far
/// above fit the view is — the mid on an engine with no box (user
/// requirement 2026-08-01: "while I'm holding a key I don't need the image
/// to be as good as possible, I need it to move fast; when I release the
/// key, then I want quality to be high").
///
/// This governs what is REQUESTED, never what is DISPLAYED. The renderer
/// always shows the best rung in cache, so flying back over frames whose
/// full-res is still resident shows them sharp — a rule that rendered the
/// mid with a sharp texture in hand would be worse than the bug it fixes
/// (persona).
/// What `focus` should ask for, and over which ring: the TRANSIT vs
/// SETTLED decision (user requirement 2026-08-01), pure so it can be
/// tested without workers.
///
/// Moving: `transit_request` — the fit box, or the mid with no box — over a
/// wide ring leaning the way we travel, so the workers keep up with a held
/// key, and the lean is what puts frames in cache BEFORE the finger reaches
/// them. Stopped: what the app actually wants over the tight ring, which is
/// the pre-existing behaviour and is what keeps tap-stepping through a
/// burst sharp.
///
/// Returns `(request, lo, hi)` with `lo..=hi` already clamped to `count`.
///
/// Since issue #46 the coordinates are VIEW POSITIONS, not image ids —
/// the caller (`focus_on`) maps positions back to ids via `ring_ids` at
/// request time. The policy in here is unchanged.
fn focus_plan(
    transit: bool,
    forward: bool,
    index: usize,
    desired: Target,
    fit_box: Option<FitBox>,
    count: usize,
) -> (Target, usize, usize) {
    if transit {
        // A reversal must re-lean immediately: arrowing back through a
        // burst you just flew over is the commonest correction there is,
        // and a ring still leaning forward would prefetch behind you.
        let (back, ahead) = if forward {
            (TRANSIT_BEHIND, TRANSIT_AHEAD)
        } else {
            (TRANSIT_AHEAD, TRANSIT_BEHIND)
        };
        (
            plan_request(transit, desired, fit_box),
            index.saturating_sub(back),
            (index + ahead).min(count - 1),
        )
    } else {
        (
            desired,
            index.saturating_sub(PREFETCH),
            (index + PREFETCH).min(count - 1),
        )
    }
}

/// What to ask the decoder for: `transit_request` while moving, the app's
/// real target when stopped. Split from `focus_plan` so a focus with no
/// view position (no ring) still requests the right rung.
fn plan_request(transit: bool, desired: Target, fit_box: Option<FitBox>) -> Target {
    if transit {
        transit_request(desired, fit_box)
    } else {
        desired
    }
}

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

fn in_transit(state: &LoupeState, now: std::time::Instant) -> bool {
    state.moving
        && state
            .last_index_change
            .is_some_and(|t| now.saturating_duration_since(t) < SETTLE_DEBOUNCE)
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
/// Normal workers pop from the back (most urgent last).
fn next_job(state: &mut LoupeState, focus_reserved: bool, now: std::time::Instant) -> Slot {
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
                        return Slot::Wait;
                    }
                }
            }
        } else {
            match state.queue.len().checked_sub(1) {
                Some(pos) => pos,
                None => return Slot::Wait,
            }
        };
        let entry = state.queue.remove(pos);
        if cached_serves(state, entry.index, entry.target) {
            continue; // upgraded or topped out meanwhile
        }
        state.in_flight.push(entry.index);
        return Slot::Job(entry.index, entry.target, entry.state);
    }
}

fn worker(shared: &Shared, focus_reserved: bool) {
    loop {
        let (index, target, req) = {
            let mut state = lock(shared);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                match next_job(&mut state, focus_reserved, std::time::Instant::now()) {
                    Slot::Job(index, target, req) => break (index, target, req),
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
            if revive_deferred(&mut state, index, target, req, stamp) {
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
/// rung"), and what follows is read off the KIND of the image that decode
/// returned — the scale the decoder ran — never off the IFD's size claim,
/// which `find_embedded_jpegs` trusts over the SOF:
/// - it failed: as any failed rung — a good lower rung stays, memoized,
///   with no Failed; nothing does, and the image fails. No second attempt
///   at full scale: the same bytes would fail the same way;
/// - it is a `Full` (the decoder ran 8/8 — a lossless, CMYK or YCCK stream):
///   it IS the full rung, published as the plain decode would publish it,
///   which then does not run (it would decode the same JPEG twice);
/// - it is a `Screen` rung larger than what is in hand: published, never
///   terminal; the ladder stops if it serves, and otherwise falls through
///   to the plain decode (an IFD that over-claims its stream);
/// - it is a `Screen` rung no larger: nothing published; falls through.
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
    let full = previews.fullres().cloned();
    let mut rungs: Vec<crate::raw::EmbeddedJpeg> = Vec::new();
    if let Some(mid) = previews.grid_source() {
        rungs.push(mid.clone());
    }
    if let Some(full) = previews.fullres() {
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
        // THE SCREEN RUNG: at fit, the full's N/8 decode first.
        let mut screen_decoded = false;
        if let (RungKind::Full, Target::Fit(fit_box)) = (candidate, target) {
            if let Some(n) = rung_factor(rung.width, rung.height, orientation, fit_box) {
                let (sw, sh) = scaled_dims(rung.width, rung.height, n);
                if sw.max(sh) > achieved {
                    screen_decoded = true;
                    match decode_jpeg_rung(&mut file, rung, orientation, n, candidate) {
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
                            publish(shared, index, image, rung_long >= top_long, req);
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
                                publish(shared, index, image, false, req);
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
        // A screen rung that did not serve — an IFD that over-claims its
        // stream — falls through to the plain decode of the same full: a
        // second decode in one flight, so the reserved lane checks its focus
        // again here, as between any two rungs (raw-pipeline.md, "The loupe
        // ladder": "The lane checks only BETWEEN rungs, so a focus change
        // during a rung's decode waits out that rung — one decode"; the
        // step-2 review's F3).
        if screen_decoded {
            #[cfg(test)]
            if let Some(hook) = AFTER_SCREEN_DECODE.with(std::cell::Cell::get) {
                hook(shared);
            }
            if lane_abandons(shared, index, reserved_lane, achieved) {
                return Ok(());
            }
        }
        match decode_jpeg_rung(&mut file, rung, orientation, 8, candidate) {
            Ok((image, note)) => {
                let serves = served_by(&image, target, None);
                let long = image.width.max(image.height);
                report_complaint(shared, index, rung, image.kind, note);
                publish(shared, index, image, rung_long >= top_long, req);
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

fn publish(
    shared: &Shared,
    index: usize,
    image: FullImage,
    terminal: bool,
    state_at_request: RequestState,
) {
    let mut state = lock(shared);
    let stamp = shared.stamp.load(Ordering::Relaxed);
    if let Some((old, _)) = state.cache.remove(&index) {
        state.cached_bytes -= old.rgb.len();
    }
    state.cached_bytes += image.rgb.len();
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

/// Read one embedded JPEG and decode it at `numerator`/8. The image's kind
/// is the scale the decoder RAN — below 8 a screen rung, at 8 the
/// `candidate` the caller named (the mid, or the full) — never a
/// comparison of the decoded size with the IFD's claim. Beside the image,
/// what the decode went past, if anything (`Decoded::note`).
fn decode_jpeg_rung(
    file: &mut std::fs::File,
    rung: &crate::raw::EmbeddedJpeg,
    orientation: u16,
    numerator: u8,
    candidate: RungKind,
) -> Result<(FullImage, Option<String>), String> {
    let bytes = read_jpeg(file, rung).map_err(|e| format!("read: {e}"))?;
    let decoded = decode_with(&bytes, orientation, numerator)?;
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

    /// `revive_deferred` at a `Long` target in the settled state, likewise.
    fn revive_long(state: &mut LoupeState, index: usize, long: u32, stamp: u64) -> bool {
        revive_deferred(
            state,
            index,
            Target::Long(long),
            RequestState::Settled,
            stamp,
        )
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
            next_job(&mut state, true, now),
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
            next_job(&mut state, true, now),
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
            next_job(&mut state, true, now),
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
            next_job(&mut state, true, now),
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
        assert_eq!(next_job(&mut state, true, now), Slot::Wait);
        assert_eq!(
            state.cache.get(&4).map(|(_, s)| *s),
            Some(77),
            "the settle poll must not restamp the frame it merely inspected"
        );
    }

    /// The transit ring leans the way the user is travelling, and a
    /// reversal re-leans it on the very next frame.
    ///
    /// Untested, a symmetric ring survives every other assertion here: it
    /// still requests the mid, still keeps up on the frame you are ON. What
    /// it loses is the whole point of the look-ahead — the frames arriving
    /// BEFORE the finger gets to them.
    #[test]
    fn transit_ring_leans_in_the_direction_of_travel() {
        let count = 1000;
        // Moving forward: far more ahead than behind.
        let (_, lo, hi) = focus_plan(true, true, 500, Target::Long(u32::MAX), None, count);
        assert_eq!(
            (hi - 500, 500 - lo),
            (TRANSIT_AHEAD, TRANSIT_BEHIND),
            "a forward ring must lean forward"
        );
        assert!(
            hi - 500 > 500 - lo,
            "a symmetric transit ring prefetches frames the user is moving \
             AWAY from: ahead {} vs behind {}",
            hi - 500,
            500 - lo
        );
        // Reversed on the very next frame: the lean flips with it.
        let (_, lo, hi) = focus_plan(true, false, 499, Target::Long(u32::MAX), None, count);
        assert_eq!(
            (499 - lo, hi - 499),
            (TRANSIT_AHEAD, TRANSIT_BEHIND),
            "arrowing back must re-lean backward immediately"
        );
        // Settled: the tight symmetric ring, and the app's REAL target.
        let (req, lo, hi) = focus_plan(false, true, 500, Target::Long(8640), None, count);
        assert_eq!((500 - lo, hi - 500), (PREFETCH, PREFETCH));
        assert_eq!(
            req,
            Target::Long(8640),
            "a settled frame must ask for full quality"
        );
        assert!(
            focus_plan(true, true, 500, Target::Long(8640), None, count).0 < req,
            "transit must ask for LESS than settled, or it is not transit"
        );
        // Edges clamp rather than wrap or panic.
        let (_, lo, hi) = focus_plan(true, true, 0, Target::Long(u32::MAX), None, 3);
        assert_eq!((lo, hi), (0, 2), "ring clamps at the start of the folder");
        let (_, lo, hi) = focus_plan(true, true, 2, Target::Long(u32::MAX), None, 3);
        assert_eq!((lo, hi), (0, 2), "ring clamps at the end of the folder");
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
        assert!(!in_transit(&state, std::time::Instant::now()));
        assert!(revive_deferred(&mut state, 5, fit, Transit, 1));
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
        let (_, lo, hi) = focus_plan(
            false,
            true,
            fpos,
            Target::Long(u32::MAX),
            None,
            state.ring_len(10),
        );
        let ids = ring_ids(&state, fpos, lo, hi);
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
        let mut ids = ring_ids(&state, 9, 7, 11);
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
        match next_job(&mut state, true, now) {
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
            next_job(&mut state, true, now + FOCUS_DEBOUNCE),
            Slot::Job(0, Target::Long(u32::MAX), RequestState::Settled)
        );
        // A smaller target (zoom out) never re-arms either.
        let mut state = stable_focus_state(3);
        note_focus(&mut state, 3, Target::Long(1000), now);
        state.queue.push(long_entry(3, 1000, true));
        assert_eq!(
            next_job(&mut state, true, now),
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
            next_job(&mut state, true, now),
            Slot::Job(4, Target::Long(u32::MAX), RequestState::Settled)
        );
        assert!(state.in_flight.contains(&4));
        // The focused entry is gone: the reserved worker now waits even
        // though backlog remains.
        assert_eq!(next_job(&mut state, true, now), Slot::Wait);
        assert_eq!(
            state.queue.len(),
            2,
            "backlog untouched by the reserved worker"
        );
        // A normal worker still pops from the back.
        assert_eq!(
            next_job(&mut state, false, now),
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
        match next_job(&mut state, true, now) {
            Slot::WaitFor(d) => assert!(d <= FOCUS_DEBOUNCE, "timed wait bounded"),
            other => panic!("fresh focus must not be taken: {other:?}"),
        }
        assert_eq!(state.queue.len(), 1, "entry left for the backlog workers");
        // Once the focus has held, the reserved worker commits.
        assert_eq!(
            next_job(&mut state, true, now + FOCUS_DEBOUNCE),
            Slot::Job(2, Target::Long(u32::MAX), RequestState::Settled)
        );
    }

    #[test]
    fn reserved_worker_waits_without_a_focus() {
        let now = std::time::Instant::now();
        let mut state = LoupeState::default();
        state.queue.push(long_entry(0, u32::MAX, true));
        assert_eq!(next_job(&mut state, true, now), Slot::Wait);
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
            next_job(&mut state, false, now),
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

    /// Brief 008 (raw-pipeline.md, "The screen rung"): a rung's kind is the
    /// scale the decoder RAN, never a comparison with the IFD's size claim,
    /// which `find_embedded_jpegs` trusts over the SOF. Here the one IFD
    /// under-claims its intact 2000x1500 stream as 500x375; the box rule on
    /// the claim picks 3/8 for a 200x150 box, and the 3/8 decode comes out
    /// 750x563 — LARGER than the claim. It is still a screen rung: never
    /// terminal, never memoized as the file's best. Read the kind off the
    /// claim ("decoded at least as long as declared: the full") and the
    /// rung ships as a terminal full, the zoom ceiling read from it.
    #[test]
    fn the_rung_kind_comes_from_the_decode_not_the_ifd_claim() {
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let off = b.add_blob(&full);
        let ifd0 = b.add_ifd(
            &[
                (0x0100, 3, 1, 500),
                (0x0101, 3, 1, 375),
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
            width: 200,
            height: 150,
        };
        assert_eq!(
            rung_factor(500, 375, 1, fit_box),
            Some(3),
            "the premise: the claim asks for the 3/8 rung"
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
                    (750, 563, RungKind::Screen),
                    "a 3/8 decode is a screen rung whatever the IFD claimed"
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

    /// A RAW whose second IFD CLAIMS a 4000x3000 full over an intact
    /// 2000x1500 stream, behind an intact 640x400 mid: `find_embedded_jpegs`
    /// trusts an IFD's size over the SOF, so the ladder plans for a full it
    /// can never decode (M11: another body's writer, or a damaged IFD).
    fn raw_over_claiming_its_full() -> Vec<u8> {
        let mid = crate::raw::jpeg_hostile::encoded(640, 400);
        let full = crate::raw::jpeg_hostile::encoded(2000, 1500);
        let mut b = crate::raw::tiff_testutil::TiffBuilder::new(true);
        let mid_off = b.add_blob(&mid);
        let full_off = b.add_blob(&full);
        let second = b.add_ifd(
            &[
                (0x0100, 3, 1, 4000),
                (0x0101, 3, 1, 3000),
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
    #[test]
    fn the_ladder_memoizes_the_decoded_size_not_the_ifd_claim() {
        use RungKind::{Full, Mid, Screen};
        let dir = crate::testutil::scratch_dir("over-claim-memo");
        let path = dir.join("over_claimed.arw");
        std::fs::write(&path, raw_over_claiming_its_full()).unwrap();
        let claim = {
            let mut file = std::fs::File::open(&path).unwrap();
            find_embedded_jpegs(&mut file)
                .unwrap()
                .fullres()
                .map(|f| (f.width, f.height))
        };
        assert_eq!(
            claim,
            Some((4000, 3000)),
            "the premise: the IFD's claim sizes the full"
        );
        let uhd = FitBox {
            width: 3840,
            height: 2160,
        };
        // At the 4K box the claim asks 5/8, which the real stream decodes to
        // 1250x938: short of the box, so the ladder goes on to the full.
        assert_eq!(rung_factor(4000, 3000, 1, uhd), Some(5));
        let now = std::time::Instant::now();
        for (target, rungs) in [
            (
                Target::Long(u32::MAX),
                vec![(640, 400, Mid), (2000, 1500, Full)],
            ),
            (
                Target::Fit(uhd),
                vec![(640, 400, Mid), (1250, 938, Screen), (2000, 1500, Full)],
            ),
        ] {
            let (shared, rx) = shared_over(vec![path.clone()]);
            assert_eq!(
                decode_ladder(&shared, 0, target, 0, false, RequestState::Settled),
                Ok(())
            );
            assert_eq!(published(&rx), rungs, "{target:?}: what the stream holds");
            let mut state = lock(&shared);
            assert_eq!(
                state.best_long.get(&0).copied(),
                Some(2000),
                "{target:?}: the memo is the decoded 2000, not the IFD's 4000"
            );
            // The cursor at rest on this frame, settled and past the debounce.
            state.focused = Some(0);
            state.focused_at = Some(now - FOCUS_DEBOUNCE * 2);
            state.last_index_change = Some(now - SETTLE_DEBOUNCE * 2);
            state.desired = target;
            assert_eq!(
                next_job(&mut state, true, now),
                Slot::Wait,
                "{target:?}: a file whose ladder topped out is not decoded again at every settle"
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
    #[test]
    fn the_reserved_lane_abandons_between_the_screen_rung_and_the_full() {
        use RungKind::{Full, Mid, Screen};
        /// Clears the hook whatever happens, so no later test on this
        /// thread inherits it.
        struct ClearHook;
        impl Drop for ClearHook {
            fn drop(&mut self) {
                AFTER_SCREEN_DECODE.with(|hook| hook.set(None));
            }
        }
        let _clear = ClearHook;
        let dir = crate::testutil::scratch_dir("lane-f3");
        let path = dir.join("over_claimed.arw");
        std::fs::write(&path, raw_over_claiming_its_full()).unwrap();
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
