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

/// A decoded full-resolution image, shared with the UI without copying.
#[derive(Debug, Clone)]
pub struct FullImage {
    pub rgb: Arc<Vec<u8>>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub enum LoupeEvent {
    Ready { index: usize, image: FullImage },
    Failed { index: usize, reason: String },
}

#[derive(Default)]
struct LoupeState {
    /// Pending (index, display-long-edge), most urgent last (workers pop
    /// from the back); one entry per index keeping the largest target.
    queue: Vec<(usize, u32)>,
    in_flight: Vec<usize>,
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
        let workers = (0..2)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || worker(&shared))
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
        state.focused = Some(index);
        // Prefetch ring: farthest neighbors first, focused index last (back
        // of the queue = popped first by workers).
        let lo = index.saturating_sub(PREFETCH);
        let hi = (index + PREFETCH).min(count - 1);
        let mut wanted: Vec<usize> = (lo..=hi).filter(|i| *i != index).collect();
        wanted.sort_by_key(|i| std::cmp::Reverse(i.abs_diff(index)));
        wanted.push(index);
        for i in wanted {
            let sufficient = if let Some((img, s)) = state.cache.get_mut(&i) {
                *s = stamp;
                serves(img, display_long)
            } else {
                false
            };
            if !sufficient && !state.in_flight.contains(&i) && !state.failed.contains(&i) {
                state.queue.retain(|(q, _)| *q != i);
                state.queue.push((i, display_long));
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

fn worker(shared: &Shared) {
    loop {
        let (index, display_long) = {
            let mut state = lock(shared);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some((index, display_long)) = state.queue.pop() {
                    if let Some((img, _)) = state.cache.get(&index) {
                        if serves(img, display_long) {
                            continue; // upgraded meanwhile
                        }
                    }
                    state.in_flight.push(index);
                    break (index, display_long);
                }
                state = shared
                    .wakeup
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        if let Err(reason) = outcome {
            state.failed.insert(index);
            drop(state);
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

    let mut published = false;
    for rung in &rungs {
        let rung_long = rung.width.max(rung.height);
        if rung_long <= current_long {
            continue; // already have this rung or better
        }
        let image = decode_jpeg_rung(&mut file, rung)?;
        publish(shared, index, image);
        published = true;
        if serves_dims(rung.width, rung.height, display_long) {
            return Ok(());
        }
    }
    // Nothing better than the cache existed; that is fine, not an error —
    // but a file where NO rung decoded is a failure.
    if published || current_long > 0 {
        Ok(())
    } else {
        Err("no decodable preview".into())
    }
}

fn serves_dims(w: u32, h: u32, display_long: u32) -> bool {
    w.max(h) as f32 * UPSCALE_THRESHOLD >= display_long as f32
}

fn publish(shared: &Shared, index: usize, image: FullImage) {
    let mut state = lock(shared);
    let stamp = shared.stamp.load(Ordering::Relaxed);
    if let Some((old, _)) = state.cache.remove(&index) {
        state.cached_bytes -= old.rgb.len();
    }
    state.cached_bytes += image.rgb.len();
    state.cache.insert(index, (image.clone(), stamp));
    evict_to_budget(&mut state, shared.budget);
    drop(state);
    shared.events.send(LoupeEvent::Ready { index, image }).ok();
}

fn decode_jpeg_rung(
    file: &mut std::fs::File,
    rung: &crate::raw::EmbeddedJpeg,
) -> Result<FullImage, String> {
    let bytes = read_jpeg(file, rung).map_err(|e| format!("read: {e}"))?;
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB)
        .set_max_width(usize::MAX)
        .set_max_height(usize::MAX);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&bytes, options);
    let rgb = decoder.decode().map_err(|e| format!("decode: {e}"))?;
    let (w, h) = decoder.dimensions().ok_or("no dimensions")?;
    Ok(FullImage {
        rgb: Arc::new(rgb),
        width: u32::try_from(w).map_err(|_| "width overflow")?,
        height: u32::try_from(h).map_err(|_| "height overflow")?,
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
