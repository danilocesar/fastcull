//! Loupe asset engine: full-resolution embedded-JPEG decodes for the
//! 1-column view and 1:1 zoom (`specs/modules/raw-pipeline.md` FullRes asset).
//!
//! Asset ladder (user decision, raw-pipeline.md): each image climbs
//! mid-preview (1616×1080, ~5 ms) → full-res (8640×5760, ~140 ms), and a
//! rung is only cooked when the display exceeds the current asset by more
//! than `UPSCALE_THRESHOLD` (1.25×). Every rung is published as its own
//! Ready event so the UI swaps quality in place without blocking.
//!
//! `focus(index, display_long)` schedules the focused image at top priority
//! and prefetches ±PREFETCH neighbors — in VIEW order, the order arrows
//! actually travel (`set_view`; issue #46): an id-space ring on a
//! capture-sorted multi-body folder warmed frames no arrow could reach
//! while every real neighbor stayed cold. The ring is capped to the frames
//! the memory budget holds (`ring_within_budget`; brief 012), sized from the
//! headers the workers parse. A byte-budget LRU (default 2 GiB) evicts the
//! least recently focused images, never the focused one, and a ring member
//! it evicts is not asked for again until the focus moves
//! (`LoupeState::evicted_under_focus`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use crate::raw::{find_embedded_jpegs, read_jpeg, EmbeddedJpeg, EmbeddedPreviews};

/// Neighbors prefetched on each side of the focused image.
pub const PREFETCH: usize = 2;
/// Default decoded-pixels budget (bytes of RGB kept in the LRU) — the
/// loupe memory setting's default (settings.md).
pub const DEFAULT_BUDGET_BYTES: usize = 2 * 1024 * 1024 * 1024;
/// The budget's floor: room for at least one decoded A1 full-res frame
/// (149 MB). The loupe memory setting is clamped to it (settings.md), and
/// `start` enforces it whatever it is handed.
pub const BUDGET_FLOOR_BYTES: usize = 200 * 1024 * 1024;
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

/// A decoded full-resolution image, shared with the UI without copying.
#[derive(Debug, Clone)]
pub struct FullImage {
    pub rgb: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub enum LoupeEvent {
    Ready {
        index: usize,
        image: FullImage,
        /// True when this is the file's BEST possible rung (its native
        /// resolution): single-rung sources (bare JPEGs, issue #8) have a
        /// terminal rung at or below mid-class size, and the app needs
        /// the signal to learn the zoom ceiling from it.
        terminal: bool,
    },
    Failed {
        index: usize,
        reason: String,
    },
}

/// The ring the engine's budget allows for the frame sizes it has learned
/// (`LoupeEngine::ring_report`): what the app's `loupe ring` trace mark
/// prints (test-harness.md; brief 012 D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingReport {
    /// The budget the engine adopted, floored (`LoupeEngine::budget`).
    pub budget: usize,
    /// The largest full-res frame whose header was parsed this session, in
    /// decoded bytes (width × height × 3).
    pub frame_bytes: usize,
    /// The largest mid preview parsed, likewise; 0 if none was.
    pub mid_bytes: usize,
    /// The ±PREFETCH window the budget allows for `frame_bytes` — what a
    /// settled 1:1 focus asks for — as `(behind, ahead)` of the direction
    /// of travel.
    pub rest: (usize, usize),
    /// The 2/8 transit window the budget allows for `mid_bytes`.
    pub transit: (usize, usize),
}

#[derive(Default)]
struct LoupeState {
    /// Pending (index, display-long-edge), most urgent last (workers pop
    /// from the back); one entry per index — the LATEST target wins (it
    /// reflects current intent; an escalation dropped while in flight
    /// self-heals via the Ready→refresh loop).
    /// Third field: true = focused/prefetch origin (survives want-culling).
    queue: Vec<(usize, u32, bool)>,
    /// Best rung a file can ever provide (long edge), learned when its
    /// ladder tops out: an asset at this size is sufficient for ANY display
    /// — without this memo, 1:1 (u32::MAX target) re-parsed files forever
    /// (validator MAJOR finding).
    best_long: HashMap<usize, u32>,
    in_flight: Vec<usize>,
    /// Upgrade targets requested while the index was in flight at a smaller
    /// target: re-queued when the flight lands (QE defect — the upgrade was
    /// silently dropped, 1:1 never arrived without the app's refresh loop).
    deferred: HashMap<usize, u32>,
    /// LRU cache: index -> (image, last-focus stamp).
    cache: HashMap<usize, (FullImage, u64)>,
    cached_bytes: usize,
    /// Indexes that failed to decode: never re-queued (a corrupt file must
    /// not be re-attempted on every focus — validator finding).
    failed: HashSet<usize>,
    /// Frames the budget evicted while the focus rested where it is now:
    /// `focus()` does not ask for them again until the focus moves to
    /// another frame or its target escalates — the two moments
    /// `note_focus` already treats as new work, and where this is cleared.
    ///
    /// Without it a budget smaller than the ±PREFETCH window (five decoded
    /// A1 frames, ~746 MB) never went quiet: each landing evicted a ring
    /// member, and the app's re-focus — `presenter::refresh` calls
    /// `focus()` on every landing — queued that member again, which
    /// evicted the next, for as long as the cursor rested (QE 2026-10-01,
    /// brief 008 D20; raw-pipeline.md's ring budget rule). The fixed
    /// 2 GiB budget hid it until the loupe memory became a setting.
    ///
    /// The invariant: a frame evicted while the focus rests on F at
    /// target T is never re-requested by `focus(F, T')` for any T' ≤ T.
    /// A small budget therefore never loops while the user rests. Since
    /// the ring fits the budget (`ring_within_budget`; brief 012) the
    /// window itself fits, so a member is evicted under a settled focus
    /// only at the margin — a transit's mids share the budget. The focused
    /// frame itself is never evicted (see `focused`), so this never holds
    /// it; grid wants (`want()`, mid rungs) are not held back either —
    /// they are the visible cells' own requests.
    evicted_under_focus: HashSet<usize>,
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
    /// The display target of the last focus() call for `focused` —
    /// escalation detection for the debounce clock.
    focused_target: u32,
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
    desired_long: u32,
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
    /// What one decoded frame of THIS folder takes, per rung, in bytes
    /// (width × height × 3): the largest full-res and the largest mid
    /// preview whose headers the workers have parsed this session, 0 until
    /// the first parse (raw-pipeline.md, "The ring fits the budget"; brief
    /// 012). Learned, never assumed: an A1 frame is 149 MB and another
    /// body's is not (M11), and a folder mixing bodies is sized by its
    /// biggest frame. `note_frame_dims` writes them; the ring's cap reads
    /// them through `rung_bytes_for`.
    frame_bytes_full: usize,
    frame_bytes_mid: usize,
    /// Long edge of the SMALLEST mid preview with a full rung above it in
    /// its own file, 0 until one is parsed: a request above what that mid
    /// serves sends at least one frame of this folder to its full rung, so
    /// the window is sized by the full (`rung_bytes_for`). The smallest,
    /// not the largest, because sizing a mixed folder by a mid some of its
    /// frames climb past would ask for full frames the budget cannot keep.
    /// A one-rung file (a bare JPEG) is left out: it costs its one rung at
    /// any request — already in both maxima — and a small one would
    /// otherwise size a held arrow's ring of mids by the full frames.
    mid_long: u32,
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
            budget: budget.max(BUDGET_FLOOR_BYTES),
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

    /// The budget this engine ADOPTED, in bytes: what `start` was handed,
    /// floored at [`BUDGET_FLOOR_BYTES`]. The app's `loupe engine started
    /// budget` trace mark reads it from here, never from the figure it
    /// passed in, so the mark proves what the engine holds (QE 2026-10-01,
    /// brief 008 D23: a mark from the caller's own local stayed true with
    /// the engine started on the default).
    pub fn budget(&self) -> usize {
        self.shared.budget
    }

    /// The ring this engine's budget allows for the frame sizes it has
    /// learned so far — `None` until a worker has parsed a header, because
    /// no size is known before then and none is assumed (M11; a report
    /// built from an assumed size would read the same at every budget, a
    /// mark that cannot go red — brief 012 D5). The app prints it as its
    /// `loupe ring` mark on change: the proof the budget reached the RING,
    /// as `budget()` proves it reached the engine. Takes the engine's lock
    /// briefly and reads no file, so the UI thread may call it.
    pub fn ring_report(&self) -> Option<RingReport> {
        let state = lock(&self.shared);
        if state.frame_bytes_full == 0 {
            return None;
        }
        let budget = self.shared.budget;
        Some(RingReport {
            budget,
            frame_bytes: state.frame_bytes_full,
            mid_bytes: state.frame_bytes_mid,
            rest: ring_within_budget(budget, state.frame_bytes_full, PREFETCH, PREFETCH),
            transit: ring_within_budget(
                budget,
                state.frame_bytes_mid,
                TRANSIT_BEHIND,
                TRANSIT_AHEAD,
            ),
        })
    }

    /// The user is looking at `index` on a display whose longest edge is
    /// `display_long` physical pixels: ensure it and its ±PREFETCH neighbors
    /// — in VIEW order (see `set_view`), as many of them as the budget holds
    /// (`ring_within_budget`) — have an asset sufficient for that display
    /// (ladder rule) or are queued. Returns the best cached image
    /// immediately (which may be a lower rung — a better one arrives as an
    /// event once cooked).
    pub fn focus(&self, index: usize, display_long: u32) -> Option<FullImage> {
        let count = self.shared.paths.len();
        if count == 0 || index >= count {
            return None;
        }
        let stamp = self.shared.stamp.fetch_add(1, Ordering::Relaxed) + 1;
        let now = std::time::Instant::now();
        let mut state = lock(&self.shared);
        note_focus(&mut state, index, display_long, now);
        let transit = in_transit(&state, now);
        // TRANSIT vs SETTLED (user requirement 2026-08-01). Moving: ask only
        // for the mid rung, across a wide ring leaning the way we travel —
        // ~5 MB and ~5 ms each, so the workers keep up with a held key.
        // Stopped: ask for what the app actually wants, over the tight ring,
        // which is the pre-existing behaviour and is what keeps tap-stepping
        // through a burst sharp.
        //
        // The ring is planned in VIEW-POSITION space and mapped back to
        // image ids at request time (issue #46): arrows travel the view. A
        // focused id with no view position (filtered out mid-flight) gets
        // no neighbors — its neighbors are unknowable, and guessing in id
        // space is the bug this replaced.
        //
        // The window is capped to what the budget holds, measured in the
        // bytes one decoded frame of the rung this request climbs to takes
        // in this folder (brief 012) — 0, and uncapped, until a worker has
        // parsed a header. Read from state the workers filled: no I/O here.
        let request = plan_request(transit, display_long);
        let rung_bytes = rung_bytes_for(&state, request);
        let (request, wanted) = match state.pos_of(index) {
            Some(fpos) => {
                let (request, lo, hi) = focus_plan(
                    transit,
                    state.travel_forward,
                    fpos,
                    display_long,
                    state.ring_len(count),
                    self.shared.budget,
                    rung_bytes,
                );
                (request, ring_ids(&state, fpos, lo, hi))
            }
            None => (request, Vec::new()),
        };
        // Farthest neighbors first, focused index last (back of the queue
        // = popped first by workers).
        let mut wanted: Vec<usize> = wanted.into_iter().filter(|i| *i < count).collect();
        wanted.push(index);
        for i in wanted {
            schedule(&mut state, i, request, stamp, Origin::Focus);
        }
        let hit = state.cache.get(&index).map(|(img, _)| img.clone());
        drop(state);
        self.shared.wakeup.notify_all();
        hit
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
        state.queue.retain(|(_, _, focus_origin)| *focus_origin);
        let mut queued_any = false;
        for i in indexes {
            if i >= count {
                continue;
            }
            queued_any |= schedule(&mut state, i, display_long, stamp, Origin::Grid);
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

/// Ladder rule: does this asset serve a display of `display_long` pixels?
fn serves(img: &FullImage, display_long: u32) -> bool {
    let asset_long = img.width.max(img.height) as f32;
    asset_long * UPSCALE_THRESHOLD >= display_long as f32
}

/// Cached-and-sufficient check (refreshing the LRU stamp): an asset counts
/// as sufficient when it serves the display OR it already is the best rung
/// this file can provide (terminal-rung memo).
fn sufficient_cached(state: &mut LoupeState, index: usize, display_long: u32, stamp: u64) -> bool {
    let best = state.best_long.get(&index).copied();
    if let Some((img, s)) = state.cache.get_mut(&index) {
        *s = stamp;
        serves(img, display_long) || best.is_some_and(|b| img.width.max(img.height) >= b)
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
fn cached_serves(state: &LoupeState, index: usize, display_long: u32) -> bool {
    let best = state.best_long.get(&index).copied();
    state.cache.get(&index).is_some_and(|(img, _)| {
        serves(img, display_long) || best.is_some_and(|b| img.width.max(img.height) >= b)
    })
}

/// Land-time revival of a deferred upgrade (an in-flight index whose wanted
/// rung grew mid-decode). Revived ONLY while the index is still inside the
/// focused prefetch ring — the settled window the budget allows for the
/// rung `target` climbs to (±PREFETCH at the default; brief 012): reviving
/// a neighbour outside it decodes a frame the budget cannot keep. A stale
/// upgrade — the cursor moved on while the flight decoded — re-queued at
/// top priority captured BOTH workers for multi-second full-res decodes and
/// starved the current frame's ladder (Windows CI 2026-07-27: three
/// screenshot tests hit the 60 s shutter cap exactly this way). Dropping a
/// stale upgrade loses nothing: focus() re-requests it the moment the user
/// returns. The focused index re-queues at the back (popped next); a ring
/// neighbor goes to the front so it can never outrank the focused frame's
/// own pending work.
fn revive_deferred(
    state: &mut LoupeState,
    budget: usize,
    index: usize,
    target: u32,
    stamp: u64,
) -> bool {
    // Ring membership in VIEW positions (issue #46), like the ring itself:
    // an id 2 away can be a view-order stranger, and a view neighbor can
    // be any id at all. No position (filtered out) = not in the ring.
    let window = ring_within_budget(budget, rung_bytes_for(state, target), PREFETCH, PREFETCH);
    let in_ring = state
        .focused
        .is_some_and(|f| match (state.pos_of(index), state.pos_of(f)) {
            (Some(pos), Some(fpos)) => in_reach(state.travel_forward, fpos, window, pos),
            _ => false,
        });
    // A frame the budget evicted under this focus stays out until the
    // focus moves, a revival included (see `evicted_under_focus`).
    if !in_ring
        || state.failed.contains(&index)
        || state.evicted_under_focus.contains(&index)
        || sufficient_cached(state, index, target, stamp)
    {
        return false;
    }
    state.queue.retain(|(q, _, _)| *q != index);
    if state.focused == Some(index) {
        state.queue.push((index, target, true));
    } else {
        state.queue.insert(0, (index, target, true));
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
fn schedule(state: &mut LoupeState, index: usize, target: u32, stamp: u64, origin: Origin) -> bool {
    if sufficient_cached(state, index, target, stamp) || state.failed.contains(&index) {
        return false;
    }
    // AFTER `sufficient_cached`, whose stamp refresh a cached member still
    // gets: a ring member the budget evicted under this very focus is not
    // asked for again until the focus moves — asking re-decodes it, which
    // evicts the next member, which the next landing's re-focus asks for…
    // (see `evicted_under_focus`).
    if origin == Origin::Focus && state.evicted_under_focus.contains(&index) {
        return false;
    }
    if state.in_flight.contains(&index) {
        let e = state.deferred.entry(index).or_insert(0);
        *e = (*e).max(target);
        return false;
    }
    match origin {
        Origin::Focus => {
            state.queue.retain(|(q, _, _)| *q != index);
            state.queue.push((index, target, true));
        }
        Origin::Grid => {
            if state.queue.iter().any(|(q, _, _)| *q == index) {
                return false; // already scheduled by focus/prefetch
            }
            // Front of the vec = popped last: focused work stays first.
            state.queue.insert(0, (index, target, false));
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
///
/// The same two moments end the budget's hold on what it evicted
/// (`evicted_under_focus`): a step, or a bigger target, is new work, and
/// the window it asks for is asked for whole.
fn note_focus(state: &mut LoupeState, index: usize, display_long: u32, now: std::time::Instant) {
    // The app's real intent, before any transit capping, so the settle
    // knows what to climb to.
    state.desired_long = display_long;
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
        state.focused_target = display_long;
        state.evicted_under_focus.clear();
    } else if display_long > state.focused_target {
        state.focused_at = Some(now);
        state.focused_target = display_long;
        state.evicted_under_focus.clear();
    }
}

/// What a worker should do next.
#[derive(Debug, PartialEq)]
enum Slot {
    /// Decode this (index, display_long).
    Job(usize, u32),
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
/// for the ~5 MB mid rung — the whole ring costs less than ONE 149 MB
/// full-res frame.
const TRANSIT_AHEAD: usize = 8;
const TRANSIT_BEHIND: usize = 2;

/// The window the budget lets the engine ask for — the one site of the
/// rule (raw-pipeline.md, "The ring fits the budget"; brief 012, issue
/// #99).
///
/// `behind` and `ahead` are in TRAVEL coordinates: `ahead` is the side the
/// user is moving toward. The budget holds `max(1, ⌊budget ÷ rung_bytes⌋)`
/// decoded frames of the rung being asked for. The focused frame takes the
/// first slot whatever the budget — a loupe that cannot show the frame the
/// user is looking at is broken, not frugal — and the rest go to the
/// neighbours nearest the focus, the travel side first at equal distance
/// (that is where the next arrow lands), so the far side shrinks first.
/// The result is never wider than the window it was given.
///
/// `rung_bytes == 0` means no header has been parsed yet, so no size is
/// known: the window comes back uncapped (brief 012 D4 — there is no size
/// to assume, M11). Without this cap every step asked for the whole window
/// and the byte LRU evicted what did not fit: at 0.5 GB two or three
/// full-res decodes per step keeping two, at the 200 MB floor four keeping
/// none but the focused frame (brief 012's before table).
pub fn ring_within_budget(
    budget: usize,
    rung_bytes: usize,
    behind: usize,
    ahead: usize,
) -> (usize, usize) {
    if rung_bytes == 0 {
        return (behind, ahead);
    }
    // The slots left once the focused frame has its own.
    let mut room = (budget / rung_bytes).max(1) - 1;
    let (mut kept_behind, mut kept_ahead) = (0, 0);
    let mut distance = 1;
    while room > 0 && (distance <= ahead || distance <= behind) {
        if distance <= ahead {
            kept_ahead = distance;
            room -= 1;
        }
        if distance <= behind && room > 0 {
            kept_behind = distance;
            room -= 1;
        }
        distance += 1;
    }
    (kept_behind, kept_ahead)
}

/// Is the user MOVING between frames (held key, `[`/`]`, a Y/N
/// auto-advance chain) rather than looking at one?
///
/// While true the engine asks only for the mid rung, however far above fit
/// the view is (user requirement 2026-08-01: "while I'm holding a key I
/// don't need the image to be as good as possible, I need it to move fast;
/// when I release the key, then I want quality to be high").
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
/// Moving: the mid rung only, over a wide ring leaning the way we travel
/// — ~5 MB and ~5 ms each, so the workers keep up with a held key, and
/// the lean is what puts frames in cache BEFORE the finger reaches them.
/// Stopped: what the app actually wants over the tight ring, which is the
/// pre-existing behaviour and is what keeps tap-stepping through a burst
/// sharp.
///
/// Returns `(request, lo, hi)`: `lo..=hi` is the window capped to what
/// `budget` holds of frames `rung_bytes` each (`ring_within_budget`; 0 =
/// no size known yet, uncapped), clamped to `count`. When the cap does not
/// bind — the default budget — the window is exactly the pre-brief-012 one.
///
/// Since issue #46 the coordinates are VIEW POSITIONS, not image ids —
/// the caller (`focus`) maps positions back to ids via `ring_ids` at
/// request time. The policy in here is unchanged.
fn focus_plan(
    transit: bool,
    forward: bool,
    index: usize,
    display_long: u32,
    count: usize,
    budget: usize,
    rung_bytes: usize,
) -> (u32, usize, usize) {
    let (behind, ahead) = uncapped_window(transit);
    // Capped in TRAVEL coordinates, before the mapping to view positions:
    // "the travel side first" is the side the user moves toward, whichever
    // way that is (raw-pipeline.md, "The ring fits the budget").
    let window = ring_within_budget(budget, rung_bytes, behind, ahead);
    let (below, above) = view_reach(forward, window);
    (
        plan_request(transit, display_long),
        index.saturating_sub(below),
        (index + above).min(count - 1),
    )
}

/// The UNCAPPED window `(behind, ahead)` in travel coordinates: the wide
/// lean of a held arrow, the tight ±PREFETCH ring once settled — what the
/// budget then caps (`ring_within_budget`).
fn uncapped_window(transit: bool) -> (usize, usize) {
    if transit {
        (TRANSIT_BEHIND, TRANSIT_AHEAD)
    } else {
        (PREFETCH, PREFETCH)
    }
}

/// A travel-coordinate window as view positions: how far `(below, above)`
/// the focused position it reaches.
///
/// A reversal must re-lean immediately: arrowing back through a burst you
/// just flew over is the commonest correction there is, and a ring still
/// leaning forward would prefetch behind you.
fn view_reach(forward: bool, (behind, ahead): (usize, usize)) -> (usize, usize) {
    if forward {
        (behind, ahead)
    } else {
        (ahead, behind)
    }
}

/// Is view position `pos` inside the travel-coordinate `window` around the
/// focused position `fpos`? The focused position itself always is. One
/// definition for the two places that test membership rather than plan —
/// the deferred revival and the first parse's cull — so neither can drift
/// from the window `focus_plan` asks for.
fn in_reach(forward: bool, fpos: usize, window: (usize, usize), pos: usize) -> bool {
    let (below, above) = view_reach(forward, window);
    if pos >= fpos {
        pos - fpos <= above
    } else {
        fpos - pos <= below
    }
}

/// The bytes one decoded frame of the rung `request` climbs to takes in
/// this folder — the measure the ring is capped by (brief 012 D3): the mid
/// preview while the mid serves the request (a held arrow; fit on a ≤2K
/// display), the full frame above that (1:1; fit on a 4K display). A
/// transit ring of ~5 MB mids measured in 149 MB frames would shrink to the
/// focused frame at the floor for no memory reason, losing the look-ahead
/// a held arrow exists for. 0 while no header has been parsed: no size,
/// no cap. The mid's reach is the `serves` rule (`UPSCALE_THRESHOLD`).
fn rung_bytes_for(state: &LoupeState, request: u32) -> usize {
    if state.mid_long > 0 && serves_dims(state.mid_long, state.mid_long, request) {
        state.frame_bytes_mid
    } else {
        state.frame_bytes_full
    }
}

/// What to ask the decoder for: the mid while moving, the app's real
/// target when stopped. Split from `focus_plan` so a focus with no view
/// position (no ring) still requests the right rung.
fn plan_request(transit: bool, display_long: u32) -> u32 {
    if transit {
        transit_request(display_long)
    } else {
        display_long
    }
}

/// What a moving frame asks the decoder for.
///
/// `MID_RUNG_TARGET`, not `MID_RUNG_MAX_LONG`: the latter (2048) is the
/// ceiling of what COUNTS as mid class, but `serves` allows only a 1.25x
/// upscale, so a 1616 mid covers 2020 px — 28 short of 2048. Asking for
/// 2048 quietly sent every transit frame up to full-res anyway, and the
/// whole change measured as no improvement at all until the arithmetic was
/// checked. `transit_request_is_served_by_the_mid_rung` pins both halves.
fn transit_request(display_long: u32) -> u32 {
    display_long.min(MID_RUNG_TARGET)
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
            match state.queue.iter().rposition(|(q, _, _)| *q == f) {
                Some(pos) => pos,
                None => {
                    // SETTLE GUARANTEE. Transit deliberately asked only for
                    // the mid, so once the user stops, SOMETHING has to ask
                    // for the real target — and it cannot be the app, whose
                    // refresh loop goes quiet exactly when nothing is
                    // decoding. This lane already wakes on a timer, so it is
                    // the one place that can promise it: settled, focused
                    // frame short of what the app wants, nothing queued for
                    // it -> queue it here.
                    let settled = state
                        .last_index_change
                        .is_some_and(|t| now.saturating_duration_since(t) >= SETTLE_DEBOUNCE);
                    let want = state.desired_long;
                    if settled
                        && want > 0
                        && !state.failed.contains(&f)
                        && !state.in_flight.contains(&f)
                        && !cached_serves(state, f, want)
                    {
                        state.queue.push((f, want, true));
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
        let (index, display_long, _) = state.queue.remove(pos);
        if let Some((img, _)) = state.cache.get(&index) {
            let best = state.best_long.get(&index).copied();
            if serves(img, display_long) || best.is_some_and(|b| img.width.max(img.height) >= b) {
                continue; // upgraded or topped out meanwhile
            }
        }
        state.in_flight.push(index);
        return Slot::Job(index, display_long);
    }
}

fn worker(shared: &Shared, focus_reserved: bool) {
    loop {
        let (index, display_long) = {
            let mut state = lock(shared);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                match next_job(&mut state, focus_reserved, std::time::Instant::now()) {
                    Slot::Job(index, display_long) => break (index, display_long),
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
            decode_ladder(shared, index, display_long, current_long, focus_reserved)
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
        if let Some(target) = state.deferred.remove(&index) {
            let stamp = shared.stamp.load(Ordering::Relaxed);
            if revive_deferred(&mut state, shared.budget, index, target, stamp) {
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

/// Decode rungs for `index` until one serves `display_long`, publishing each
/// improvement over `current_long` to the cache + event channel.
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
fn decode_ladder(
    shared: &Shared,
    index: usize,
    display_long: u32,
    current_long: u32,
    reserved_lane: bool,
) -> Result<(), String> {
    let path = &shared.paths[index];
    let mut file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let previews = find_embedded_jpegs(&mut file).map_err(|e| format!("parse: {e}"))?;
    // HERE, before a pixel is decoded: the session's first parse sizes the
    // loupe's ring and culls the queued prefetch the cap excludes (brief
    // 012 D4) — about a millisecond into the focused frame's own decode.
    note_frame_dims(shared, &previews, std::time::Instant::now());

    let orientation = previews.orientation;
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
    let mut achieved = current_long;
    for rung in &rungs {
        let rung_long = rung.width.max(rung.height);
        if rung_long <= achieved {
            continue; // already have this rung or better
        }
        if reserved_lane && lock(shared).focused != Some(index) {
            // The focus moved: free the lane at the rung boundary (see
            // the fn doc — the reserved worker serves the focus, only
            // ever the focus). Logged so the next stall-shaped CI
            // failure is diagnosable in one read (validator finding:
            // silent abandons force timing inference).
            eprintln!("fastcull: loupe lane abandoned idx {index} at {achieved} (focus moved)");
            return Ok(());
        }
        match decode_jpeg_rung(&mut file, rung, orientation) {
            Ok(image) => {
                publish(shared, index, image, rung_long >= top_long);
                achieved = rung_long;
                if serves_dims(rung.width, rung.height, display_long) {
                    return Ok(());
                }
            }
            Err(reason) => {
                // A broken HIGHER rung must not fail an image that already
                // has a good lower rung (validator MAJOR: valid mid +
                // truncated full-res would badge Failed AND show an image).
                // Memoize what we achieved so the ladder quiesces.
                if achieved > 0 {
                    note_best(shared, index, achieved);
                    return Ok(());
                }
                return Err(reason);
            }
        }
    }
    // Ladder topped out below the display target: memoize the terminal rung
    // so this file is never re-parsed for an unreachable target.
    if achieved > 0 {
        note_best(shared, index, achieved);
        Ok(())
    } else {
        Err(crate::raw::NO_DECODABLE_PREVIEW.into())
    }
}

fn note_best(shared: &Shared, index: usize, long: u32) {
    let mut state = lock(shared);
    let entry = state.best_long.entry(index).or_insert(0);
    *entry = (*entry).max(long);
}

/// Learn this folder's frame sizes from a header a worker just parsed —
/// before a pixel of it is decoded — and, when they change, drop the
/// queued prefetch the capped ring now excludes (raw-pipeline.md, "The
/// ring fits the budget"; brief 012 D4). WORKER thread; holds the lock
/// briefly, never across I/O.
fn note_frame_dims(shared: &Shared, previews: &EmbeddedPreviews, now: std::time::Instant) {
    let mut state = lock(shared);
    if learn_frame_dims(&mut state, previews) {
        cull_outside_ring(&mut state, shared.budget, now);
    }
}

/// Fold one file's preview sizes into the session's (see
/// `LoupeState::frame_bytes_full` and `mid_long`); true when anything
/// changed. The rungs are the ones `decode_ladder` climbs: `grid_source`
/// (the mid) and `fullres`, two rungs when they differ.
fn learn_frame_dims(state: &mut LoupeState, previews: &EmbeddedPreviews) -> bool {
    let before = (
        state.frame_bytes_full,
        state.frame_bytes_mid,
        state.mid_long,
    );
    let full = previews
        .fullres()
        .and_then(|j| decoded_bytes(j).map(|bytes| (j, bytes)));
    let mid = previews
        .grid_source()
        .and_then(|j| decoded_bytes(j).map(|bytes| (j, bytes)));
    if let Some((_, bytes)) = full {
        state.frame_bytes_full = state.frame_bytes_full.max(bytes);
    }
    if let Some((_, bytes)) = mid {
        state.frame_bytes_mid = state.frame_bytes_mid.max(bytes);
    }
    if let (Some((mid, _)), Some((full, _))) = (mid, full) {
        if mid != full {
            let long = mid.width.max(mid.height);
            state.mid_long = match state.mid_long {
                0 => long,
                seen => seen.min(long),
            };
        }
    }
    before
        != (
            state.frame_bytes_full,
            state.frame_bytes_mid,
            state.mid_long,
        )
}

/// Bytes of one decoded RGB frame of this preview, or `None` for a size
/// that teaches nothing: a header claiming more pixels than
/// `decode_oriented` will ever decode (`plausible_decoded_dims`, issue #31)
/// would otherwise shrink every ring to the focused frame for a frame that
/// is refused — a harmless complaint must not cost the folder its prefetch
/// (M11) — and a zero side is no frame at all.
fn decoded_bytes(jpeg: &EmbeddedJpeg) -> Option<usize> {
    let width = usize::try_from(jpeg.width).ok()?;
    let height = usize::try_from(jpeg.height).ok()?;
    if !crate::raw::plausible_decoded_dims(width, height) {
        return None;
    }
    width
        .checked_mul(height)?
        .checked_mul(3)
        .filter(|bytes| *bytes > 0)
}

/// Remove the queued prefetch the capped ring excludes — the first parse's
/// cull (raw-pipeline.md, "The ring fits the budget"; brief 012 D4).
/// WORKER thread, under the lock.
///
/// Before a session's first header is parsed `focus()` knows no size and
/// queues the uncapped window; once a size is learned, or grows, the
/// focus-origin entries outside the window the budget allows around the
/// current focus are dropped. Kept: grid wants (the visible cells' own
/// requests), the focused frame and its capped window. A decode already
/// running completes — a decode is never interrupted — and nothing is
/// replanned: the cold focus already queued a superset. When the cap does
/// not bind (the default budget) the queue is left exactly as it was.
fn cull_outside_ring(state: &mut LoupeState, budget: usize, now: std::time::Instant) {
    let Some(focused) = state.focused else {
        return;
    };
    // No view position: `focus()` gave this focus no ring at all.
    let Some(fpos) = state.pos_of(focused) else {
        return;
    };
    let transit = in_transit(state, now);
    let uncapped = uncapped_window(transit);
    let rung_bytes = rung_bytes_for(state, plan_request(transit, state.desired_long));
    let window = ring_within_budget(budget, rung_bytes, uncapped.0, uncapped.1);
    if window == uncapped {
        return;
    }
    let forward = state.travel_forward;
    // Taken out so the filter can read the view mapping beside it.
    let mut queue = std::mem::take(&mut state.queue);
    queue.retain(|&(q, _, focus_origin)| {
        !focus_origin
            || state
                .pos_of(q)
                .is_some_and(|pos| in_reach(forward, fpos, window, pos))
    });
    state.queue = queue;
}

fn serves_dims(w: u32, h: u32, display_long: u32) -> bool {
    w.max(h) as f32 * UPSCALE_THRESHOLD >= display_long as f32
}

fn publish(shared: &Shared, index: usize, image: FullImage, terminal: bool) {
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
        })
        .ok();
}

fn decode_jpeg_rung(
    file: &mut std::fs::File,
    rung: &crate::raw::EmbeddedJpeg,
    orientation: u16,
) -> Result<FullImage, String> {
    let bytes = read_jpeg(file, rung).map_err(|e| format!("read: {e}"))?;
    let (rgb, w, h) = decode_oriented(&bytes, orientation)?;
    Ok(FullImage {
        rgb: Arc::new(rgb),
        width: w,
        height: h,
    })
}

/// Decode a JPEG stream and apply its EXIF orientation — THE full-res hot
/// path, public so `perf_budgets` measures the code that actually ships
/// instead of a re-implementation of it (the old test replicated
/// `decode()` + rotate and therefore could not see pipeline-level wins or
/// regressions in this path).
pub fn decode_oriented(bytes: &[u8], orientation: u16) -> Result<(Vec<u8>, u32, u32), String> {
    // Per-side limits are lifted (the default is 16384, which would reject
    // legitimate stitched panoramas served as bare JPEGs); SOF sides are u16
    // so the real bound is the PIXEL-COUNT cap below (issue #31), checked
    // before anything is allocated from the header's claim.
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
    // The A1 full-res JPEG is baseline with ZERO restart markers (verified
    // by parsing them — probe 2026-08-02), so the Huffman decode is
    // strictly serial: one core for ~220 ms while the rest idle. Two
    // things reclaim that dead time on a 50 MP frame (measured, medians):
    //
    // - `decode_into` a pre-faulted buffer instead of `decode()`:
    //   247-252 ms → 215-227 ms. `decode()` allocates internally and
    //   pays ~40 ms of first-touch page faults inside the decode.
    // - The transpose's 149 MB output buffer is allocated AND pre-faulted
    //   on a spare thread WHILE the decode runs, so the rotate that
    //   follows starts with hot pages ([`crate::raw::Scratch`]).
    //
    // Neither changes peak memory: the same two buffers exist either way;
    // only WHEN the page faults are paid moves — off the critical path.
    decoder
        .decode_headers()
        .map_err(|e| format!("decode: {e}"))?;
    let (w, h) = decoder.dimensions().ok_or("no dimensions")?;
    // Issue #31: the header's dimension claim sizes the decode buffer, the
    // prefault pass, and the transpose Scratch below — all BEFORE zune sees
    // one byte of scan data, and a truncated scan decodes as "success"
    // (zero-filled). Reject implausible claims and unterminated streams
    // here, while nothing has been allocated from them.
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
        let decoded = decoder
            .decode_into(&mut rgb)
            .map_err(|e| format!("decode: {e}"));
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
        // Held out until the focus moves: re-requesting it under this
        // focus is the loop (see `evicted_under_focus`).
        state.evicted_under_focus.insert(victim);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(schedule(&mut st, 7, 1616, 1, Origin::Grid));
        assert!(schedule(&mut st, 3, 8640, 2, Origin::Focus));
        assert_eq!(st.queue, vec![(7, 1616, false), (3, 8640, true)]);

        // A grid want for an already-queued index yields (no duplicate,
        // no downgrade of the focus entry's target).
        assert!(!schedule(&mut st, 3, 1616, 3, Origin::Grid));
        assert_eq!(st.queue, vec![(7, 1616, false), (3, 8640, true)]);

        // A focus request for an already-queued index REPLACES it.
        assert!(schedule(&mut st, 7, 8640, 4, Origin::Focus));
        assert_eq!(st.queue, vec![(3, 8640, true), (7, 8640, true)]);

        // In flight: nothing is queued, the target is deferred, and the
        // merge keeps the LARGEST target regardless of arrival order.
        st.in_flight.push(5);
        assert!(!schedule(&mut st, 5, 8640, 5, Origin::Focus));
        assert!(!schedule(&mut st, 5, 1616, 6, Origin::Grid));
        assert_eq!(st.deferred.get(&5), Some(&8640));
        assert_eq!(st.queue.len(), 2, "an in-flight index is never queued");

        // Failed indexes are never scheduled again.
        st.failed.insert(9);
        assert!(!schedule(&mut st, 9, 1616, 7, Origin::Focus));
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
        note_focus(&mut st, 0, u32::MAX, t0);
        assert!(!in_transit(&st, t0), "the first focus is never transit");

        // Held key: changes one repeat interval apart.
        let mut t = t0;
        for i in 1..=5 {
            t += Duration::from_millis(120);
            note_focus(&mut st, i, u32::MAX, t);
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
        note_focus(&mut st, 0, u32::MAX, t);
        for i in 1..=4 {
            t += Duration::from_millis(400);
            note_focus(&mut st, i, u32::MAX, t);
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
        state.desired_long = 8640;
        state.last_index_change = Some(now - SETTLE_DEBOUNCE);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: MID_RUNG_TARGET,
                    height: 1080,
                },
                0,
            ),
        );
        assert!(state.queue.is_empty(), "transit queued nothing sharp");
        assert_eq!(
            next_job(&mut state, true, now),
            Slot::Job(4, 8640),
            "a settled frame short of the app's target must climb"
        );

        // Still MOVING: the guarantee must not fire mid-hold, or every
        // frame of a held arrow starts a full-res decode and transit is
        // pointless.
        let mut state = stable_focus_state(4);
        state.desired_long = 8640;
        state.last_index_change = Some(now);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: MID_RUNG_TARGET,
                    height: 1080,
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
        state.desired_long = 8640;
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
        state.desired_long = 8640;
        state.last_index_change = Some(now - SETTLE_DEBOUNCE);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: 8640,
                    height: 5760,
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
        state.desired_long = 8640;
        state.last_index_change = Some(now - SETTLE_DEBOUNCE);
        state.cache.insert(
            4,
            (
                FullImage {
                    rgb: std::sync::Arc::new(vec![0u8; 3]),
                    width: 8640,
                    height: 5760,
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
        let (_, lo, hi) = focus_plan(true, true, 500, u32::MAX, count, DEFAULT_BUDGET_BYTES, 0);
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
        let (_, lo, hi) = focus_plan(true, false, 499, u32::MAX, count, DEFAULT_BUDGET_BYTES, 0);
        assert_eq!(
            (499 - lo, hi - 499),
            (TRANSIT_AHEAD, TRANSIT_BEHIND),
            "arrowing back must re-lean backward immediately"
        );
        // Settled: the tight symmetric ring, and the app's REAL target.
        let (req, lo, hi) = focus_plan(false, true, 500, 8640, count, DEFAULT_BUDGET_BYTES, 0);
        assert_eq!((500 - lo, hi - 500), (PREFETCH, PREFETCH));
        assert_eq!(req, 8640, "a settled frame must ask for full quality");
        assert!(
            focus_plan(true, true, 500, 8640, count, DEFAULT_BUDGET_BYTES, 0).0 < req,
            "transit must ask for LESS than settled, or it is not transit"
        );
        // Edges clamp rather than wrap or panic.
        let (_, lo, hi) = focus_plan(true, true, 0, u32::MAX, 3, DEFAULT_BUDGET_BYTES, 0);
        assert_eq!((lo, hi), (0, 2), "ring clamps at the start of the folder");
        let (_, lo, hi) = focus_plan(true, true, 2, u32::MAX, 3, DEFAULT_BUDGET_BYTES, 0);
        assert_eq!((lo, hi), (0, 2), "ring clamps at the end of the folder");
    }

    /// The transit request must be a rung the MID actually serves.
    ///
    /// This is the bug the first implementation shipped with: it asked for
    /// `MID_RUNG_MAX_LONG` (2048), but `serves` allows only a 1.25x upscale,
    /// so a 1616 mid covers 2020 px — 28 short. Every transit frame quietly
    /// climbed to full-res anyway, and the change measured as no improvement
    /// at all until the arithmetic was checked.
    #[test]
    fn transit_request_is_served_by_the_mid_rung() {
        let mid = FullImage {
            rgb: std::sync::Arc::new(vec![0u8; 3]),
            width: 1616,
            height: 1080,
        };
        // What focus() asks for while moving, at 1:1 on a full A1 frame.
        let request = transit_request(8640);
        assert!(
            serves(&mid, request),
            "the mid rung must satisfy the transit request, or transit still \
             climbs to full-res: mid 1616 covers {} px, asked for {request}",
            (1616.0 * UPSCALE_THRESHOLD) as u32
        );
        // The old value is exactly the trap: keep it documented as failing.
        assert!(
            !serves(&mid, MID_RUNG_MAX_LONG),
            "MID_RUNG_MAX_LONG is NOT served by a 1616 mid — that was the bug"
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
            u32::MAX,
            state.ring_len(10),
            DEFAULT_BUDGET_BYTES,
            0,
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
        note_focus(&mut st, 9, u32::MAX, t0);
        note_focus(&mut st, 1, u32::MAX, t0 + Duration::from_millis(120));
        assert!(
            st.travel_forward,
            "pos 3 -> pos 4 is forward travel although the id fell 9 -> 1"
        );
        // Backward in the view despite a rising id: id 1 (pos 4) -> id 6
        // (pos 2).
        note_focus(&mut st, 6, u32::MAX, t0 + Duration::from_millis(240));
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
            revive_deferred(&mut state, DEFAULT_BUDGET_BYTES, 1, u32::MAX, 1),
            "id 1 is the focused frame's direct VIEW neighbor (pos 4)"
        );
        state.queue.clear();
        assert!(
            !revive_deferred(&mut state, DEFAULT_BUDGET_BYTES, 8, u32::MAX, 1),
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
            focused_target: u32::MAX,
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
            !revive_deferred(&mut state, DEFAULT_BUDGET_BYTES, 0, u32::MAX, 1),
            "index 0 is outside the ring of focus 4"
        );
        assert!(state.queue.is_empty(), "nothing may be re-queued");
        // Exact ring boundary: distance PREFETCH is IN, one past is OUT.
        assert!(revive_deferred(
            &mut state,
            DEFAULT_BUDGET_BYTES,
            4 - PREFETCH,
            u32::MAX,
            1
        ));
        assert!(!revive_deferred(
            &mut state,
            DEFAULT_BUDGET_BYTES,
            4 - PREFETCH - 1,
            u32::MAX,
            1
        ));
        // No focus at all (loupe never opened): equally dropped.
        state.focused = None;
        assert!(!revive_deferred(
            &mut state,
            DEFAULT_BUDGET_BYTES,
            0,
            u32::MAX,
            2
        ));
    }

    #[test]
    fn focused_deferred_upgrade_revives_at_top_priority() {
        let mut state = stable_focus_state(4);
        state.queue.push((6, 1000, true));
        assert!(revive_deferred(
            &mut state,
            DEFAULT_BUDGET_BYTES,
            4,
            u32::MAX,
            1
        ));
        // Workers pop from the back: the focused frame goes next.
        assert_eq!(state.queue.last(), Some(&(4, u32::MAX, true)));
    }

    #[test]
    fn ring_neighbor_deferred_upgrade_never_outranks_the_focused_frame() {
        let mut state = stable_focus_state(4);
        state.queue.push((4, u32::MAX, true)); // the cursor's own pending work
        assert!(revive_deferred(
            &mut state,
            DEFAULT_BUDGET_BYTES,
            5,
            u32::MAX,
            1
        ));
        assert_eq!(
            state.queue.last(),
            Some(&(4, u32::MAX, true)),
            "the focused frame stays first in line"
        );
        assert_eq!(state.queue.first(), Some(&(5, u32::MAX, true)));
    }

    #[test]
    fn failed_or_sufficient_deferred_upgrades_stay_dead() {
        let mut state = stable_focus_state(4);
        state.failed.insert(4);
        assert!(!revive_deferred(
            &mut state,
            DEFAULT_BUDGET_BYTES,
            4,
            u32::MAX,
            1
        ));
        // A cached asset that already tops out (best_long known) is enough.
        let mut state = stable_focus_state(4);
        let img = FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 100,
            height: 100,
        };
        state.cache.insert(4, (img, 0));
        state.best_long.insert(4, 100);
        assert!(!revive_deferred(
            &mut state,
            DEFAULT_BUDGET_BYTES,
            4,
            u32::MAX,
            1
        ));
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
        state.focused_target = 1900; // resting at a fit-sized target
        note_focus(&mut state, 0, u32::MAX, now); // the pin escalates
        state.queue.push((0, u32::MAX, true));
        match next_job(&mut state, true, now) {
            Slot::WaitFor(_) => {}
            other => panic!("escalated climb taken without debounce: {other:?}"),
        }
        // Render-cadence re-focus at the SAME target must not keep
        // re-arming (the clock would never expire).
        note_focus(
            &mut state,
            0,
            u32::MAX,
            now + std::time::Duration::from_millis(100),
        );
        assert_eq!(
            next_job(&mut state, true, now + FOCUS_DEBOUNCE),
            Slot::Job(0, u32::MAX)
        );
        // A smaller target (zoom out) never re-arms either.
        let mut state = stable_focus_state(3);
        note_focus(&mut state, 3, 1000, now);
        state.queue.push((3, 1000, true));
        assert_eq!(next_job(&mut state, true, now), Slot::Job(3, 1000));
    }

    /// The second starvation shape (Windows CI 2026-07-27): every
    /// worker was captured by legitimate climbs before the cursor
    /// settled — the reserved worker must take the STABLE focused
    /// frame's job, and nothing else.
    #[test]
    fn reserved_worker_takes_only_the_stable_focused_job() {
        let now = std::time::Instant::now();
        let mut state = stable_focus_state(4);
        state.queue.push((2, u32::MAX, true));
        state.queue.push((4, u32::MAX, true));
        state.queue.push((5, u32::MAX, true)); // more urgent than 4's entry
        assert_eq!(next_job(&mut state, true, now), Slot::Job(4, u32::MAX));
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
        assert_eq!(next_job(&mut state, false, now), Slot::Job(5, u32::MAX));
    }

    /// The capture-bait case that FAILED validation on the debounce-less
    /// version: a fresh focus (startup rest, transit touch) must never
    /// bind the reserved lane to a multi-second climb.
    #[test]
    fn reserved_worker_debounces_a_fresh_focus() {
        let now = std::time::Instant::now();
        let mut state = stable_focus_state(2);
        state.focused_at = Some(now); // focus just changed (transit touch)
        state.queue.push((2, u32::MAX, true));
        match next_job(&mut state, true, now) {
            Slot::WaitFor(d) => assert!(d <= FOCUS_DEBOUNCE, "timed wait bounded"),
            other => panic!("fresh focus must not be taken: {other:?}"),
        }
        assert_eq!(state.queue.len(), 1, "entry left for the backlog workers");
        // Once the focus has held, the reserved worker commits.
        assert_eq!(
            next_job(&mut state, true, now + FOCUS_DEBOUNCE),
            Slot::Job(2, u32::MAX)
        );
    }

    #[test]
    fn reserved_worker_waits_without_a_focus() {
        let now = std::time::Instant::now();
        let mut state = LoupeState::default();
        state.queue.push((0, u32::MAX, true));
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
        };
        state.cache.insert(0, (img, 0));
        state.best_long.insert(0, 100); // topped out
        state.queue.push((0, u32::MAX, true));
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
    #[test]
    fn decode_oriented_rejects_implausible_header_dimensions() {
        let mut jpeg = crate::raw::jpeg_hostile::encoded(64, 64);
        crate::raw::jpeg_hostile::patch_sof_dims(&mut jpeg, 30000, 30000);
        let hostile = crate::raw::jpeg_hostile::truncate_scan(&jpeg, 16);
        assert!(hostile.len() < 1024, "the attack fits in under a KB");
        for orientation in [1u16, 6] {
            // 6 = transpose: the Scratch prefault thread must not run either.
            let err = decode_oriented(&hostile, orientation)
                .expect_err("a 900 MP header claim must never allocate");
            assert!(
                err.contains("implausible"),
                "the reason must name the cause: {err}"
            );
        }
        // The same claim with an intact EOI is still implausible: the cap,
        // not the truncation check, is what bounds the allocation.
        let mut with_eoi = crate::raw::jpeg_hostile::encoded(64, 64);
        crate::raw::jpeg_hostile::patch_sof_dims(&mut with_eoi, 30000, 30000);
        assert!(decode_oriented(&with_eoi, 1)
            .expect_err("hostile dims with a valid EOI")
            .contains("implausible"));
    }

    /// Issue #31, half two: zune-jpeg 0.4 zero-fills a truncated scan and
    /// reports SUCCESS (its overread counter stops growing once it starts
    /// zero-filling, so even strict mode cannot see it) — the loupe showed
    /// a blank frame instead of the Failed badge. THIS TEST FAILS ON
    /// PRE-FIX CODE (it returns Ok there).
    #[test]
    fn decode_oriented_rejects_a_truncated_scan() {
        let intact = crate::raw::jpeg_hostile::encoded(64, 64);
        let truncated = crate::raw::jpeg_hostile::truncate_scan(&intact, 16);
        let err = decode_oriented(&truncated, 1).expect_err("truncated scan must fail");
        assert!(err.contains("truncated"), "reason names the cause: {err}");
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
            paths: vec![path],
            events: tx,
            shutdown: AtomicBool::new(false),
            stamp: AtomicU64::new(0),
            budget: DEFAULT_BUDGET_BYTES,
        };
        // Ask for far more than the mid can serve, so the ladder MUST try
        // the truncated full rung.
        let outcome = decode_ladder(&shared, 0, 8640, 0, false);
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
    }

    /// `budget()` is what the engine adopted: the figure it was handed,
    /// floored. No paths, so no worker ever decodes anything.
    #[test]
    fn the_engine_reports_the_budget_it_adopted() {
        assert_eq!(LoupeEngine::start(vec![], 1).0.budget(), BUDGET_FLOOR_BYTES);
        assert_eq!(LoupeEngine::start(vec![], 3 << 30).0.budget(), 3 << 30);
    }

    /// `ring_report()` (brief 012 D5, the `loupe ring` mark's source):
    /// nothing before a size is learned — no size is assumed — then the
    /// adopted budget, the sizes learned and the two windows the budget
    /// allows for them. No paths, so no worker decodes anything; the sizes
    /// are written as a parse would write them (A1-shaped fixtures).
    #[test]
    fn the_ring_report_waits_for_a_size_and_reports_the_ring_it_allows() {
        let (engine, _rx) = LoupeEngine::start(vec![], 512 << 20);
        assert_eq!(engine.ring_report(), None, "no header parsed: no report");
        {
            let mut state = lock(&engine.shared);
            state.frame_bytes_full = 8640 * 5760 * 3;
            state.frame_bytes_mid = 1616 * 1080 * 3;
        }
        assert_eq!(
            engine.ring_report(),
            Some(RingReport {
                budget: 512 << 20,
                frame_bytes: 149_299_200,
                mid_bytes: 5_235_840,
                rest: (1, 1),
                transit: (TRANSIT_BEHIND, TRANSIT_AHEAD),
            })
        );
        // The budget reported is the one ADOPTED: a figure under the floor
        // reports the floor, and the ring it allows there.
        let (floored, _rx) = LoupeEngine::start(vec![], 1);
        lock(&floored.shared).frame_bytes_full = 8640 * 5760 * 3;
        let report = floored.ring_report().expect("a size is known");
        assert_eq!((report.budget, report.rest), (BUDGET_FLOOR_BYTES, (0, 0)));
    }

    #[test]
    fn eviction_keeps_newest_and_at_least_one() {
        let mut state = LoupeState::default();
        for i in 0..4usize {
            let img = FullImage {
                rgb: Arc::new(vec![0; 100]),
                width: 10,
                height: 10,
            };
            state.cached_bytes += 100;
            state.cache.insert(i, (img, i as u64));
        }
        evict_to_budget(&mut state, 250);
        assert!(state.cache.len() <= 2 && state.cache.contains_key(&3));
        evict_to_budget(&mut state, 0);
        assert_eq!(state.cache.len(), 1, "never evicts the last image");
    }

    /// The budget's hold on what it evicted (`evicted_under_focus`;
    /// raw-pipeline.md, the ring's budget rule): a frame evicted while the
    /// focus rests on F is not queued again by a re-focus of F at the same
    /// or a smaller target — the app re-focuses on every landing, and
    /// queueing it there was the loop — nor revived as a deferred upgrade;
    /// a grid want is the visible cell's own request and is never held;
    /// and a step, or an escalation, asks for it again. Since the ring fits
    /// the budget (brief 012) an eviction under a settled focus happens
    /// only at the margin, so this test is where the hold itself is pinned;
    /// the engine-level proof with real decodes — quiet while idle, a step
    /// back re-fetching what the budget let go — is `tests/loupe.rs::
    /// a_budget_below_the_prefetch_window_goes_quiet_when_idle`.
    ///
    /// Mutants (2026-10-01): the hold check taken out of `schedule` → the
    /// re-focus queues 5 again and the first assertion goes red; the two
    /// clearings taken out of `note_focus` → the step to 7 still refuses 5
    /// and the step assertion goes red.
    #[test]
    fn a_frame_evicted_under_a_settled_focus_waits_for_the_next_step() {
        let full = |stamp: u64| {
            (
                FullImage {
                    rgb: Arc::new(vec![0; 100]),
                    width: 8640,
                    height: 5760,
                },
                stamp,
            )
        };
        let now = std::time::Instant::now();
        let mut state = LoupeState::default();
        note_focus(&mut state, 6, 8640, now);
        // The focused 6 and two neighbours; room for two, so 5 (the
        // oldest) goes.
        for (i, stamp) in [(5usize, 1u64), (8, 2), (6, 3)] {
            state.cached_bytes += 100;
            state.cache.insert(i, full(stamp));
        }
        evict_to_budget(&mut state, 250);
        assert!(!state.cache.contains_key(&5) && state.cache.contains_key(&6));

        // The app's re-focus of the same frame, at the same target and at
        // a smaller one: 5 is not asked for again.
        note_focus(&mut state, 6, 8640, now);
        assert!(
            !schedule(&mut state, 5, 8640, 4, Origin::Focus),
            "the re-focus queued the frame the budget had just evicted — the loop"
        );
        note_focus(&mut state, 6, 4000, now);
        assert!(!schedule(&mut state, 5, 4000, 5, Origin::Focus));
        assert!(
            !revive_deferred(&mut state, DEFAULT_BUDGET_BYTES, 5, 8640, 6),
            "a deferred upgrade revived the evicted frame under the same focus"
        );
        assert!(state.queue.is_empty());
        // A grid want is never held back.
        assert!(schedule(&mut state, 5, 1616, 7, Origin::Grid));
        state.queue.clear();

        // A step is new work: the new window is asked for whole.
        note_focus(&mut state, 7, 8640, now);
        assert!(
            schedule(&mut state, 5, 8640, 8, Origin::Focus),
            "after a step the frame evicted under the old focus is still held out"
        );
        state.queue.clear();

        // So is an escalation of the same frame's target.
        state.cached_bytes += 100;
        state.cache.insert(5, full(9));
        state.cached_bytes += 100;
        state.cache.insert(7, full(10));
        evict_to_budget(&mut state, 250); // 8 now oldest: out
        assert!(!state.cache.contains_key(&8));
        assert!(!schedule(&mut state, 8, 8640, 11, Origin::Focus));
        note_focus(&mut state, 7, u32::MAX, now);
        assert!(
            schedule(&mut state, 8, u32::MAX, 12, Origin::Focus),
            "an escalated target did not ask for the evicted frame again"
        );
    }

    /// The ring rule's table (raw-pipeline.md, "The ring fits the budget";
    /// brief 012 AC1). Windows are `(behind, ahead)` in travel coordinates.
    /// The bodies' frame sizes are fixtures here — the engine itself learns
    /// them from the headers it parses and holds no body constant (M11).
    #[test]
    fn the_ring_fits_the_budget() {
        /// budget, rung bytes, the window asked for, the window allowed, why.
        type Row = (usize, usize, (usize, usize), (usize, usize), &'static str);
        const GIB: usize = 1 << 30;
        const DEF: usize = DEFAULT_BUDGET_BYTES;
        const FLOOR: usize = BUDGET_FLOOR_BYTES;
        // Decoded RGB bytes of one frame: width × height × 3.
        const A1_FULL: usize = 8640 * 5760 * 3; // 149,299,200
        const A1_MID: usize = 1616 * 1080 * 3; // 5,235,840
        const MP24_FULL: usize = 6000 * 4000 * 3; // 72,000,000
        const MP100_FULL: usize = 11_648 * 8_736 * 3; // 305,270,784
        let rest = (PREFETCH, PREFETCH);
        let transit = (TRANSIT_BEHIND, TRANSIT_AHEAD);
        let rows: [Row; 19] = [
            // The default: both windows unchanged, with room (14 A1 frames).
            (DEF, A1_FULL, rest, rest, "default, 1:1"),
            (DEF, A1_FULL, transit, transit, "default, 2/8 fulls"),
            (DEF, A1_MID, transit, transit, "default, 2/8 mids"),
            // 0.5 GB holds three A1 frames: the focused one and its nearest two.
            (512 << 20, A1_FULL, (2, 2), (1, 1), "0.5 GB, A1 at 1:1"),
            // The floor holds one: the focused frame alone — at the engine's
            // floor and at the setting's 0.2 GiB, 5 MB above it.
            (FLOOR, A1_FULL, (2, 2), (0, 0), "the 200 MiB floor"),
            (214_748_365, A1_FULL, (2, 2), (0, 0), "0.2 GiB setting"),
            // Two frames, at both ends of the range: the travel-side
            // neighbour, never the one behind.
            (2 * A1_FULL, A1_FULL, (2, 2), (0, 1), "two frames exactly"),
            (3 * A1_FULL - 1, A1_FULL, (2, 2), (0, 1), "byte short of 3"),
            // Room for six neighbours of a 2/8 window: nearest first, the far
            // side shrinks first.
            (2 * GIB, MP100_FULL, (2, 8), (2, 4), "2 GiB, 100 MP, 2/8"),
            // Other bodies: a 24 MP frame fits more per GB, a 100 MP one fewer.
            (GIB, MP24_FULL, (2, 2), (2, 2), "1 GiB, 24 MP"),
            (GIB, MP100_FULL, (2, 2), (1, 1), "1 GiB, 100 MP"),
            // No header parsed yet: no size, no cap.
            (512 << 20, 0, (2, 2), (2, 2), "unknown size, settled"),
            (FLOOR, 0, (2, 8), (2, 8), "unknown size, transit"),
            // A budget below one frame still asks for the focused frame.
            (A1_FULL - 1, A1_FULL, (2, 2), (0, 0), "byte short of 1"),
            (1, A1_FULL, (2, 8), (0, 0), "one byte"),
            // A transit of A1 mids is never narrowed above 58 MB (eleven
            // mids): not at the floor, not at exactly eleven, and the
            // boundary bites one byte below.
            (FLOOR, A1_MID, (2, 8), (2, 8), "mids at the floor"),
            (11 * A1_MID, A1_MID, (2, 8), (2, 8), "exactly 11 mids"),
            (11 * A1_MID - 1, A1_MID, (2, 8), (2, 7), "short of 11 mids"),
            // Never wider than asked, however large the budget.
            (usize::MAX, 1, (2, 2), (2, 2), "a budget of everything"),
        ];
        // Every row is checked before anything fails, so a red run names
        // every row the rule broke, not only the first.
        let wrong: Vec<String> = rows
            .iter()
            .filter_map(|&(budget, rung_bytes, (behind, ahead), want, why)| {
                let got = ring_within_budget(budget, rung_bytes, behind, ahead);
                (got != want).then(|| {
                    format!(
                        "{why}: budget {budget}, rung {rung_bytes} bytes, window \
                         {behind}/{ahead} -> {}/{}, expected {}/{}",
                        got.0, got.1, want.0, want.1
                    )
                })
            })
            .collect();
        assert!(
            wrong.is_empty(),
            "the ring does not fit the budget as raw-pipeline.md states it:\n{}",
            wrong.join("\n")
        );
    }

    /// An engine's shared half for the tests that drive worker-side code
    /// (`decode_ladder`, `note_frame_dims`) without spawning workers.
    fn test_shared(paths: Vec<PathBuf>, state: LoupeState, budget: usize) -> Shared {
        let (events, _) = std::sync::mpsc::channel();
        Shared {
            state: Mutex::new(state),
            wakeup: Condvar::new(),
            paths,
            events,
            shutdown: AtomicBool::new(false),
            stamp: AtomicU64::new(0),
            budget,
        }
    }

    /// The folder's frame sizes come from the headers the workers parse —
    /// through the shipped `decode_ladder`, before a pixel of the frame is
    /// decoded — never from a body constant (raw-pipeline.md, "The ring
    /// fits the budget"; brief 012 AC3, M11): the largest full and mid seen
    /// govern; the mid's reach is that of the smallest mid with a full rung
    /// above it; a file that fails to parse, a header claiming an
    /// undecodable size and a one-rung file teach nothing more.
    #[test]
    fn frame_bytes_come_from_the_headers_parsed() {
        use crate::raw::jpeg_hostile::encoded;
        use crate::raw::tiff_testutil::{tiny_jpeg, TiffBuilder};
        // The A1's layout at synthetic sizes: the mid preview in IFD0, the
        // full rung in the next IFD (sized from its SOF, as on the A1).
        let tiff = |mid: &[u8], full: &[u8]| {
            let mut b = TiffBuilder::new(true);
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
        };
        let dir = crate::testutil::scratch_dir("ring-sizes");
        let files: [(&str, Vec<u8>); 7] = [
            ("a.arw", tiff(&encoded(640, 400), &tiny_jpeg(2000, 1500))),
            ("b.arw", tiff(&encoded(640, 400), &tiny_jpeg(3000, 2000))),
            // Smaller than a and b, still above the grid source's ~2.1 MP line.
            ("c.arw", tiff(&encoded(640, 400), &tiny_jpeg(1800, 1200))),
            ("d.arw", tiff(&encoded(1000, 700), &tiny_jpeg(2400, 1600))),
            ("junk.arw", b"junk".to_vec()),
            // 900 MP claimed: over MAX_DECODED_PIXELS, refused at decode.
            (
                "claim.arw",
                tiff(&encoded(640, 400), &tiny_jpeg(30000, 30000)),
            ),
            ("solo.jpg", encoded(380, 260)),
        ];
        let paths = files
            .iter()
            .map(|(name, bytes)| {
                let path = dir.join(name);
                std::fs::write(&path, bytes).unwrap();
                path
            })
            .collect();
        let shared = test_shared(paths, LoupeState::default(), DEFAULT_BUDGET_BYTES);
        let sizes = || {
            let state = lock(&shared);
            (
                state.frame_bytes_full,
                state.frame_bytes_mid,
                state.mid_long,
            )
        };
        let rung = |request: u32| rung_bytes_for(&lock(&shared), request);
        // A request every mid here serves: each ladder parses, decodes its
        // mid (or its only rung) and stops — the full rungs are header-only
        // fixtures and are never decoded.
        let ladder = |index: usize| decode_ladder(&shared, index, 640, 0, false);
        assert_eq!(sizes(), (0, 0, 0), "nothing is known before a parse");
        assert_eq!(rung(u32::MAX), 0, "no size, no cap");

        ladder(0).expect("a decodes");
        assert_eq!(
            sizes(),
            (2000 * 1500 * 3, 640 * 400 * 3, 640),
            "the first parse teaches the full and the mid"
        );
        assert_eq!(rung(u32::MAX), 9_000_000, "1:1 climbs to the full");
        assert_eq!(rung(800), 768_000, "the mid serves 1.25x its long edge");
        assert_eq!(rung(801), 9_000_000, "above that, the full");

        ladder(1).expect("b decodes");
        assert_eq!(
            sizes(),
            (18_000_000, 768_000, 640),
            "a larger full raises it"
        );
        ladder(2).expect("c decodes");
        assert_eq!(
            sizes(),
            (18_000_000, 768_000, 640),
            "a smaller full leaves the largest governing"
        );
        ladder(3).expect("d decodes");
        assert_eq!(
            sizes(),
            (18_000_000, 2_100_000, 640),
            "a larger mid raises the mid's bytes, and the mid's reach stays \
             that of the smallest mid with a full above it"
        );
        assert_eq!(
            rung(800),
            2_100_000,
            "every mid serves 800: the largest mid"
        );
        assert_eq!(
            rung(801),
            18_000_000,
            "a's 640 mid does not: the largest full"
        );

        assert!(ladder(4).is_err(), "junk must fail to parse");
        assert_eq!(
            sizes(),
            (18_000_000, 2_100_000, 640),
            "a file that fails to parse teaches nothing"
        );
        ladder(5).expect("the claim's mid decodes");
        assert_eq!(
            sizes(),
            (18_000_000, 2_100_000, 640),
            "a header claiming 900 MP teaches nothing (it is refused at decode)"
        );
        ladder(6).expect("the bare JPEG decodes");
        assert_eq!(
            sizes(),
            (18_000_000, 2_100_000, 640),
            "a one-rung 380 px file leaves the mid's reach alone"
        );
        assert_eq!(
            rung(800),
            2_100_000,
            "a held arrow's ring is still sized by mids"
        );
    }

    /// The deferred-upgrade revival respects the capped ring (raw-pipeline.md,
    /// the deferred-upgrade sentence; brief 012 R3, AC2): a neighbour outside
    /// the settled window the budget allows is dropped, never revived —
    /// reviving it decodes a frame the budget cannot keep. At the default the
    /// window is ±PREFETCH, as before brief 012.
    #[test]
    fn deferred_revival_respects_the_capped_ring() {
        // An A1-sized full frame (a fixture: the engine learns it from the
        // header), the focus on 4 at 1:1.
        let focused_on_4 = |forward: bool| LoupeState {
            frame_bytes_full: 8640 * 5760 * 3,
            travel_forward: forward,
            ..stable_focus_state(4)
        };
        // 0.5 GB holds three such frames: 4 and its nearest two.
        let half_gb = 512 << 20;
        let mut state = focused_on_4(true);
        assert!(revive_deferred(&mut state, half_gb, 5, u32::MAX, 1));
        assert!(revive_deferred(&mut state, half_gb, 3, u32::MAX, 1));
        state.queue.clear();
        assert!(
            !revive_deferred(&mut state, half_gb, 6, u32::MAX, 1),
            "6 is two away, outside the 1/1 window 0.5 GB allows — it must be \
             dropped, not revived"
        );
        assert!(!revive_deferred(&mut state, half_gb, 2, u32::MAX, 1));
        assert!(
            state.queue.is_empty(),
            "nothing outside the window re-queued"
        );
        assert!(
            revive_deferred(&mut state, DEFAULT_BUDGET_BYTES, 6, u32::MAX, 1),
            "at the default the window is ±PREFETCH and 6 revives"
        );
        // Exactly two frames: the travel-side neighbour only, whichever way
        // the user is moving.
        let two_frames = 2 * 8640 * 5760 * 3;
        let mut state = focused_on_4(true);
        assert!(revive_deferred(&mut state, two_frames, 5, u32::MAX, 1));
        assert!(
            !revive_deferred(&mut state, two_frames, 3, u32::MAX, 1),
            "moving forward, 3 is behind: outside a 0/1 window"
        );
        let mut state = focused_on_4(false);
        assert!(revive_deferred(&mut state, two_frames, 3, u32::MAX, 1));
        assert!(
            !revive_deferred(&mut state, two_frames, 5, u32::MAX, 1),
            "moving backward, 5 is behind: outside a 0/1 window"
        );
    }

    /// The first parse culls what the cap excludes (raw-pipeline.md, "The
    /// ring fits the budget"; brief 012 D4, AC2): before any header is
    /// parsed a cold focus queues the uncapped window, and the parse that
    /// teaches the frame size removes the focus-origin entries outside the
    /// capped window — only those. The grid want and the window stay, and
    /// at the default budget, where the cap does not bind, the queue is left
    /// exactly as it was, a stale entry of an earlier focus included.
    #[test]
    fn the_first_parse_culls_what_the_cap_excludes() {
        // An A1-shaped header (a fixture: the engine learns sizes from it).
        let previews = EmbeddedPreviews {
            candidates: vec![
                EmbeddedJpeg {
                    offset: 4096,
                    len: 1000,
                    width: 8640,
                    height: 5760,
                },
                EmbeddedJpeg {
                    offset: 2048,
                    len: 1000,
                    width: 1616,
                    height: 1080,
                },
            ],
            whole_file: false,
            orientation: 1,
        };
        // What a cold settled focus on 6 at 1:1 queues before any size is
        // known — the uncapped ±2 window, farthest first, 6 last (popped
        // first) — behind a grid want (20, at the front) and a prefetch
        // entry left by an earlier focus (12).
        let queued = vec![
            (20, 1616, false),
            (12, u32::MAX, true),
            (4, u32::MAX, true),
            (8, u32::MAX, true),
            (5, u32::MAX, true),
            (7, u32::MAX, true),
            (6, u32::MAX, true),
        ];
        let cold_focus_on_6 = || LoupeState {
            desired_long: u32::MAX,
            travel_forward: true,
            queue: queued.clone(),
            ..stable_focus_state(6)
        };
        let now = std::time::Instant::now();

        // 0.5 GB holds three A1 frames: the window is 5..=7.
        let tight = test_shared(Vec::new(), cold_focus_on_6(), 512 << 20);
        note_frame_dims(&tight, &previews, now);
        assert_eq!(
            lock(&tight).queue,
            vec![
                (20, 1616, false),
                (5, u32::MAX, true),
                (7, u32::MAX, true),
                (6, u32::MAX, true),
            ],
            "the first parse must drop the queued prefetch outside the capped \
             window (4, 8, and the stale 12) and nothing else"
        );

        // The default holds the whole window: nothing moves.
        let roomy = test_shared(Vec::new(), cold_focus_on_6(), DEFAULT_BUDGET_BYTES);
        note_frame_dims(&roomy, &previews, now);
        assert_eq!(
            lock(&roomy).queue,
            queued,
            "at the default budget the cap does not bind and the queue must be \
             left exactly as it was"
        );
    }
}
