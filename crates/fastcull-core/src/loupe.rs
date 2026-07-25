//! Loupe asset engine: full-resolution embedded-JPEG decodes for the
//! 1-column view and 1:1 zoom (`specs/modules/raw-pipeline.md` FullRes asset).
//!
//! Design (recorded): one asset per image — the fully decoded full-res RGB
//! (A1: 8640×5760 ≈ 150 MB) — displayed GPU-scaled for fit and native for
//! 1:1. The spec's separate DCT-scaled FitPreview is folded into this asset
//! for M4: zune-jpeg has no DCT scaling and turbojpeg needs system packages;
//! one decode serves both uses. Revisit if fit-quality or memory demands it.
//!
//! `focus(index)` decodes that image at top priority and prefetches ±PREFETCH
//! neighbors; a byte-budget LRU (default 2 GiB) evicts the least recently
//! focused images. Decodes ~150 ms each; two workers keep a fast arrow-key
//! advance ahead of the user.

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
    /// Pending indexes, most urgent last (workers pop from the back).
    queue: Vec<usize>,
    in_flight: Vec<usize>,
    /// LRU cache: index -> (image, last-focus stamp).
    cache: HashMap<usize, (FullImage, u64)>,
    cached_bytes: usize,
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

    /// The user is looking at `index`: ensure it and its ±PREFETCH neighbors
    /// are decoded or queued (focused image most urgent). Returns the image
    /// immediately when already cached (its LRU stamp is refreshed).
    pub fn focus(&self, index: usize) -> Option<FullImage> {
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
            let cached = if let Some((_, s)) = state.cache.get_mut(&i) {
                *s = stamp;
                true
            } else {
                false
            };
            if !cached && !state.in_flight.contains(&i) {
                state.queue.retain(|q| *q != i);
                state.queue.push(i);
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

fn worker(shared: &Shared) {
    loop {
        let index = {
            let mut state = lock(shared);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(index) = state.queue.pop() {
                    if state.cache.contains_key(&index) {
                        continue; // decoded meanwhile
                    }
                    state.in_flight.push(index);
                    break index;
                }
                state = shared
                    .wakeup
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_fullres(&shared.paths[index])
        }))
        .unwrap_or_else(|_| Err("internal error (panic) decoding full image".into()));

        let mut state = lock(shared);
        state.in_flight.retain(|i| *i != index);
        match outcome {
            Ok(image) => {
                let stamp = shared.stamp.load(Ordering::Relaxed);
                state.cached_bytes += image.rgb.len();
                state.cache.insert(index, (image.clone(), stamp));
                evict_to_budget(&mut state, shared.budget);
                drop(state);
                shared.events.send(LoupeEvent::Ready { index, image }).ok();
            }
            Err(reason) => {
                drop(state);
                shared
                    .events
                    .send(LoupeEvent::Failed { index, reason })
                    .ok();
            }
        }
    }
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

fn decode_fullres(path: &std::path::Path) -> Result<FullImage, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let previews = find_embedded_jpegs(&mut file).map_err(|e| format!("parse: {e}"))?;
    let source = previews.fullres().ok_or("no full-size preview")?.clone();
    let bytes = read_jpeg(&mut file, &source).map_err(|e| format!("read: {e}"))?;
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
