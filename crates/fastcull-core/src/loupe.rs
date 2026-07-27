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
//! and prefetches ±PREFETCH neighbors; a byte-budget LRU (default 2 GiB)
//! evicts the least recently focused images, never the focused one.

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
    failed: std::collections::HashSet<usize>,
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
    /// have an asset sufficient for that display (ladder rule) or are
    /// queued. Returns the best cached image immediately (which may be a
    /// lower rung — a better one arrives as an event once cooked).
    pub fn focus(&self, index: usize, display_long: u32) -> Option<FullImage> {
        let count = self.shared.paths.len();
        if count == 0 || index >= count {
            return None;
        }
        let stamp = self.shared.stamp.fetch_add(1, Ordering::Relaxed) + 1;
        let mut state = lock(&self.shared);
        note_focus(&mut state, index, display_long, std::time::Instant::now());
        // Prefetch ring: farthest neighbors first, focused index last (back
        // of the queue = popped first by workers).
        let lo = index.saturating_sub(PREFETCH);
        let hi = (index + PREFETCH).min(count - 1);
        let mut wanted: Vec<usize> = (lo..=hi).filter(|i| *i != index).collect();
        wanted.sort_by_key(|i| std::cmp::Reverse(i.abs_diff(index)));
        wanted.push(index);
        for i in wanted {
            if !sufficient_cached(&mut state, i, display_long, stamp) && !state.failed.contains(&i)
            {
                if state.in_flight.contains(&i) {
                    let e = state.deferred.entry(i).or_insert(0);
                    *e = (*e).max(display_long);
                } else {
                    state.queue.retain(|(q, _, _)| *q != i);
                    state.queue.push((i, display_long, true));
                }
            }
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
            if !sufficient_cached(&mut state, i, display_long, stamp) && !state.failed.contains(&i)
            {
                if state.in_flight.contains(&i) {
                    let e = state.deferred.entry(i).or_insert(0);
                    *e = (*e).max(display_long);
                } else {
                    if state.queue.iter().any(|(q, _, _)| *q == i) {
                        continue; // already scheduled by focus/prefetch
                    }
                    // Front of the vec = popped last: focused work stays first.
                    state.queue.insert(0, (i, display_long, false));
                    queued_any = true;
                }
            }
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

/// Land-time revival of a deferred upgrade (an in-flight index whose wanted
/// rung grew mid-decode). Revived ONLY while the index is still inside the
/// focused prefetch ring: a stale upgrade — the cursor moved on while the
/// flight decoded — re-queued at top priority captured BOTH workers for
/// multi-second full-res decodes and starved the current frame's ladder
/// (Windows CI 2026-07-27: three screenshot tests hit the 60 s shutter cap
/// exactly this way). Dropping a stale upgrade loses nothing: focus()
/// re-requests it the moment the user returns. The focused index re-queues
/// at the back (popped next); a ring neighbor goes to the front so it can
/// never outrank the focused frame's own pending work.
fn revive_deferred(state: &mut LoupeState, index: usize, target: u32, stamp: u64) -> bool {
    let in_ring = state.focused.is_some_and(|f| index.abs_diff(f) <= PREFETCH);
    if !in_ring || state.failed.contains(&index) || sufficient_cached(state, index, target, stamp) {
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
fn note_focus(state: &mut LoupeState, index: usize, display_long: u32, now: std::time::Instant) {
    if state.focused != Some(index) {
        state.focused = Some(index);
        state.focused_at = Some(now);
        state.focused_target = display_long;
    } else if display_long > state.focused_target {
        state.focused_at = Some(now);
        state.focused_target = display_long;
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
                None => return Slot::Wait,
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
            decode_ladder(shared, index, display_long, current_long)
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
            if revive_deferred(&mut state, index, target, stamp) {
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
fn decode_ladder(
    shared: &Shared,
    index: usize,
    display_long: u32,
    current_long: u32,
) -> Result<(), String> {
    let path = &shared.paths[index];
    let mut file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let previews = find_embedded_jpegs(&mut file).map_err(|e| format!("parse: {e}"))?;

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
        return Err("no usable embedded preview".into());
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
        Err("no decodable preview".into())
    }
}

fn note_best(shared: &Shared, index: usize, long: u32) {
    let mut state = lock(shared);
    let entry = state.best_long.entry(index).or_insert(0);
    *entry = (*entry).max(long);
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
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&bytes, options);
    let rgb = decoder.decode().map_err(|e| format!("decode: {e}"))?;
    let (w, h) = decoder.dimensions().ok_or("no dimensions")?;
    let w = u32::try_from(w).map_err(|_| "width overflow")?;
    let h = u32::try_from(h).map_err(|_| "height overflow")?;
    // Soft-rotate to display orientation (spec: every rung).
    let (rgb, w, h) = crate::raw::apply_orientation(rgb, w, h, orientation);
    Ok(FullImage {
        rgb: Arc::new(rgb),
        width: w,
        height: h,
    })
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

    /// State whose focus has already HELD past the debounce (the
    /// settled case) at a MAX target; tests for fresh/transient/
    /// escalating focuses override `focused_at`/`focused_target`.
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
            !revive_deferred(&mut state, 0, u32::MAX, 1),
            "index 0 is outside the ring of focus 4"
        );
        assert!(state.queue.is_empty(), "nothing may be re-queued");
        // Exact ring boundary: distance PREFETCH is IN, one past is OUT.
        assert!(revive_deferred(&mut state, 4 - PREFETCH, u32::MAX, 1));
        assert!(!revive_deferred(&mut state, 4 - PREFETCH - 1, u32::MAX, 1));
        // No focus at all (loupe never opened): equally dropped.
        state.focused = None;
        assert!(!revive_deferred(&mut state, 0, u32::MAX, 2));
    }

    #[test]
    fn focused_deferred_upgrade_revives_at_top_priority() {
        let mut state = stable_focus_state(4);
        state.queue.push((6, 1000, true));
        assert!(revive_deferred(&mut state, 4, u32::MAX, 1));
        // Workers pop from the back: the focused frame goes next.
        assert_eq!(state.queue.last(), Some(&(4, u32::MAX, true)));
    }

    #[test]
    fn ring_neighbor_deferred_upgrade_never_outranks_the_focused_frame() {
        let mut state = stable_focus_state(4);
        state.queue.push((4, u32::MAX, true)); // the cursor's own pending work
        assert!(revive_deferred(&mut state, 5, u32::MAX, 1));
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
        assert!(!revive_deferred(&mut state, 4, u32::MAX, 1));
        // A cached asset that already tops out (best_long known) is enough.
        let mut state = stable_focus_state(4);
        let img = FullImage {
            rgb: Arc::new(vec![0; 3]),
            width: 100,
            height: 100,
        };
        state.cache.insert(4, (img, 0));
        state.best_long.insert(4, 100);
        assert!(!revive_deferred(&mut state, 4, u32::MAX, 1));
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
}
