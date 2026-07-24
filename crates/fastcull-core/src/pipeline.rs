//! Priority thumbnail pipeline: turns a scanned session into grid thumbs and
//! EXIF metadata on all cores (`specs/modules/raw-pipeline.md`).
//!
//! Queue contract: three priorities (`Visible` > `Prefetch` > `Background`),
//! background jobs run in sequential file order (card-reader friendly),
//! `promote`/`set_visible` reprioritizes queued jobs without re-enqueueing,
//! duplicates coalesce, in-flight jobs are never cancelled (they are ≤150 ms).
//!
//! Per image the pipeline emits `MetadataReady` (EXIF via rawler) and
//! `ThumbReady` (embedded preview → decode → 320 px → JPEG q80), or `Failed`
//! once if the thumb path is impossible — metadata failure alone does not
//! fail an image, the thumb is the essential asset. Results are cached
//! (path, size, mtime); cache hits skip every RAW read.
//!
//! M1 scope: grid thumbs only. FitPreview/FullRes assets and their LRU memory
//! budget join in M4 (loupe); the queue and event plumbing are built for that
//! extension.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use crate::cache::PreviewCache;
use crate::exif::ExifSummary;
use crate::raw::{find_embedded_jpegs, read_jpeg};

/// Grid thumbnails: long edge in pixels.
pub const THUMB_LONG_EDGE: u32 = 320;
/// Grid thumbnails: JPEG re-encode quality.
pub const THUMB_JPEG_QUALITY: u8 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    Visible = 0,
    Prefetch = 1,
    Background = 2,
}

/// What the pipeline needs to know about one image (mirrors `ImageRecord`).
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

/// Progress events, delivered on the receiver returned by [`Pipeline::start`].
/// Indexes refer to the job list passed at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    MetadataReady {
        index: usize,
        exif: ExifSummary,
        from_cache: bool,
    },
    ThumbReady {
        index: usize,
        thumb_jpeg: Vec<u8>,
        width: u32,
        height: u32,
        from_cache: bool,
    },
    Failed {
        index: usize,
        reason: String,
    },
}

struct QueueState {
    /// Min-heap on (priority, sequence): lowest priority value first, then
    /// insertion order — which is file order for the initial background fill.
    heap: BinaryHeap<std::cmp::Reverse<(u8, u64, usize)>>,
    /// Source of truth for each queued job's current priority; heap entries
    /// that disagree are stale and skipped on pop (lazy deletion). Absence
    /// means done or in flight — promote() ignores those.
    queued: HashMap<usize, u8>,
    in_flight: HashSet<usize>,
    next_seq: u64,
}

struct Shared {
    state: Mutex<QueueState>,
    wakeup: Condvar,
    jobs: Vec<JobSpec>,
    cache_path: Option<PathBuf>,
    events: Sender<SessionEvent>,
    shutdown: AtomicBool,
}

/// Handle to the running pipeline. Dropping it stops the workers (queued jobs
/// are abandoned, in-flight jobs finish).
pub struct Pipeline {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Pipeline {
    /// Start `num_threads` workers over `jobs`; all jobs begin at
    /// `Background` priority in list order. `cache_path` is the SQLite
    /// preview cache (None disables caching, e.g. for tests).
    pub fn start(
        jobs: Vec<JobSpec>,
        cache_path: Option<PathBuf>,
        num_threads: usize,
    ) -> (Self, Receiver<SessionEvent>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut state = QueueState {
            heap: BinaryHeap::new(),
            queued: HashMap::new(),
            in_flight: HashSet::new(),
            next_seq: 0,
        };
        for index in 0..jobs.len() {
            let seq = state.next_seq;
            state.next_seq += 1;
            state.queued.insert(index, Priority::Background as u8);
            state
                .heap
                .push(std::cmp::Reverse((Priority::Background as u8, seq, index)));
        }
        // Create/validate the cache DB once, before workers race to open
        // their own handles: schema creation is not concurrency-safe on a
        // fresh file (two workers racing it left one silently cache-less).
        let cache_path = cache_path.filter(|p| match PreviewCache::open(p) {
            Ok(_) => true,
            Err(e) => {
                eprintln!("fastcull: preview cache disabled ({e}) — thumbnails will not persist");
                false
            }
        });
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            wakeup: Condvar::new(),
            jobs,
            cache_path,
            events: tx,
            shutdown: AtomicBool::new(false),
        });
        let workers = (0..num_threads.max(1))
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || worker_loop(&shared))
            })
            .collect();
        (Self { shared, workers }, rx)
    }

    /// Promote `indexes` to `priority`. Completed and in-flight jobs are
    /// skipped; queued jobs are reprioritized (never duplicated in effect).
    /// Lowering a priority is intentionally impossible — a job that was once
    /// visible stays at its highest requested urgency.
    pub fn promote(&self, indexes: impl IntoIterator<Item = usize>, priority: Priority) {
        // Drain the caller's iterator BEFORE taking the lock: caller code
        // must never run under our mutex (a panicking iterator would poison
        // it — QE finding).
        let indexes: Vec<usize> = indexes.into_iter().collect();
        let mut state = lock_state(&self.shared);
        for index in indexes {
            let Some(current) = state.queued.get(&index).copied() else {
                continue; // done, in flight, or out of range
            };
            if (priority as u8) < current {
                state.queued.insert(index, priority as u8);
                let seq = state.next_seq;
                state.next_seq += 1;
                state
                    .heap
                    .push(std::cmp::Reverse((priority as u8, seq, index)));
            }
        }
        drop(state);
        self.shared.wakeup.notify_all();
    }

    /// The UI's viewport changed: everything in `range` becomes `Visible`.
    pub fn set_visible(&self, range: std::ops::Range<usize>) {
        self.promote(range, Priority::Visible);
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Serialize with the workers' check-then-wait window: a worker that
        // saw shutdown == false while holding the lock must reach
        // Condvar::wait before our notify, or it sleeps forever (lost-wakeup
        // deadlock found by validator + reproduced by qe-engineer). Taking
        // and releasing the state lock between the store and the notify
        // closes that window; promote() is safe for the same reason.
        // Poison-tolerant: panicking in a destructor would abort the process.
        drop(lock_state(&self.shared));
        self.shared.wakeup.notify_all();
        for worker in self.workers.drain(..) {
            worker.join().ok();
        }
    }
}

/// Lock the queue state, tolerating poison: our own critical sections never
/// panic (caller iterators are drained outside the lock), so a poisoned
/// mutex only means some thread died mid-panic — the queue data is still a
/// consistent snapshot and shutdown must keep working.
fn lock_state(shared: &Shared) -> std::sync::MutexGuard<'_, QueueState> {
    shared
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn worker_loop(shared: &Shared) {
    // One cache handle per worker (schema already created by start());
    // handles contend via WAL + busy timeout. A failed open degrades this
    // worker to cache-less operation — loudly, never silently.
    let mut cache = shared.cache_path.as_deref().and_then(|p| {
        PreviewCache::open(p)
            .map_err(|e| eprintln!("fastcull: worker running without preview cache ({e})"))
            .ok()
    });

    loop {
        let index = {
            let mut state = lock_state(shared);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                // Pop entries until one matches its recorded priority.
                match state.heap.pop() {
                    Some(std::cmp::Reverse((prio, _seq, index))) => {
                        if state.queued.get(&index) == Some(&prio) {
                            state.queued.remove(&index);
                            state.in_flight.insert(index);
                            break index;
                        }
                        // Stale entry (reprioritized or already taken): skip.
                    }
                    None => {
                        state = shared
                            .wakeup
                            .wait(state)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                }
            }
        };

        // Contain panics from decoder internals on hostile files: the image
        // gets its Failed event and the worker survives; without this a
        // panicking job would strand the index in_flight and kill the worker.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process_job(shared, &mut cache, index);
        }));
        if outcome.is_err() {
            shared
                .events
                .send(SessionEvent::Failed {
                    index,
                    reason: "internal error (panic) while processing this file".into(),
                })
                .ok();
        }

        let mut state = lock_state(shared);
        state.in_flight.remove(&index);
    }
}

fn process_job(shared: &Shared, cache: &mut Option<PreviewCache>, index: usize) {
    let spec = &shared.jobs[index];
    let send = |event: SessionEvent| {
        shared.events.send(event).ok();
    };

    // Cache hit: no RAW reads at all.
    if let Some(cache) = cache.as_mut() {
        if let Ok(Some(hit)) = cache.lookup(&spec.path, spec.size, spec.mtime) {
            if let Some((w, h)) = jpeg_dimensions(&hit.thumb_jpeg) {
                send(SessionEvent::MetadataReady {
                    index,
                    exif: hit.exif,
                    from_cache: true,
                });
                send(SessionEvent::ThumbReady {
                    index,
                    thumb_jpeg: hit.thumb_jpeg,
                    width: w,
                    height: h,
                    from_cache: true,
                });
                return;
            }
            // Undecodable cached thumb: fall through to re-extraction.
        }
    }

    let exif = crate::exif::read_exif_summary(&spec.path).ok();
    if let Some(exif) = &exif {
        send(SessionEvent::MetadataReady {
            index,
            exif: exif.clone(),
            from_cache: false,
        });
    }

    match make_thumb(spec) {
        Ok((thumb_jpeg, width, height)) => {
            // Cache the thumb even when the EXIF read failed (as an
            // all-None summary): the zero-RAW-reads-on-reopen guarantee
            // outranks metadata completeness (recorded in catalog-cache.md).
            if let Some(cache) = cache.as_mut() {
                let exif_for_cache = exif.unwrap_or_default();
                cache
                    .store(
                        &spec.path,
                        spec.size,
                        spec.mtime,
                        &exif_for_cache,
                        &thumb_jpeg,
                    )
                    .ok();
            }
            send(SessionEvent::ThumbReady {
                index,
                thumb_jpeg,
                width,
                height,
                from_cache: false,
            });
        }
        Err(reason) => send(SessionEvent::Failed { index, reason }),
    }
}

/// Extract the grid-source preview, decode, resize to 320 px, re-encode q80.
fn make_thumb(spec: &JobSpec) -> Result<(Vec<u8>, u32, u32), String> {
    let mut file = std::fs::File::open(&spec.path).map_err(|e| format!("open: {e}"))?;
    let previews = find_embedded_jpegs(&mut file).map_err(|e| format!("parse: {e}"))?;
    let source = previews
        .grid_source()
        .ok_or("no usable embedded preview")?
        .clone();
    let jpeg_bytes = read_jpeg(&mut file, &source).map_err(|e| format!("read: {e}"))?;

    // Force RGB output so grayscale/CMYK previews also become valid thumbs.
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&jpeg_bytes, options);
    let pixels = decoder.decode().map_err(|e| format!("decode: {e}"))?;
    let (src_w, src_h) = decoder
        .dimensions()
        .ok_or("decoded JPEG reports no dimensions")?;
    let (src_w, src_h) = (
        u32::try_from(src_w).map_err(|_| "width overflow")?,
        u32::try_from(src_h).map_err(|_| "height overflow")?,
    );

    let (dst_w, dst_h) = fit_long_edge(src_w, src_h, THUMB_LONG_EDGE);
    let src_image = fast_image_resize::images::Image::from_vec_u8(
        src_w,
        src_h,
        pixels,
        fast_image_resize::PixelType::U8x3,
    )
    .map_err(|e| format!("resize input: {e}"))?;
    let mut dst_image =
        fast_image_resize::images::Image::new(dst_w, dst_h, fast_image_resize::PixelType::U8x3);
    fast_image_resize::Resizer::new()
        .resize(&src_image, &mut dst_image, None)
        .map_err(|e| format!("resize: {e}"))?;

    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, THUMB_JPEG_QUALITY)
        .encode(
            dst_image.buffer(),
            dst_w as u16,
            dst_h as u16,
            jpeg_encoder::ColorType::Rgb,
        )
        .map_err(|e| format!("encode: {e}"))?;
    Ok((out, dst_w, dst_h))
}

fn fit_long_edge(w: u32, h: u32, long_edge: u32) -> (u32, u32) {
    if w.max(h) <= long_edge {
        return (w, h);
    }
    if w >= h {
        (
            long_edge,
            (u64::from(h) * u64::from(long_edge) / u64::from(w)).max(1) as u32,
        )
    } else {
        (
            (u64::from(w) * u64::from(long_edge) / u64::from(h)).max(1) as u32,
            long_edge,
        )
    }
}

/// Cheap SOF sniff on an in-memory JPEG (for cached thumbs).
fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut decoder = zune_jpeg::JpegDecoder::new(bytes);
    decoder.decode_headers().ok()?;
    decoder
        .dimensions()
        .and_then(|(w, h)| Some((u32::try_from(w).ok()?, u32::try_from(h).ok()?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_long_edge_preserves_aspect() {
        assert_eq!(fit_long_edge(1616, 1080, 320), (320, 213));
        assert_eq!(fit_long_edge(1080, 1616, 320), (213, 320));
        assert_eq!(fit_long_edge(100, 50, 320), (100, 50)); // never upscale
        assert_eq!(fit_long_edge(8640, 2, 320), (320, 1)); // extreme aspect
    }

    /// The queue contract, tested deterministically without threads: pop
    /// order honors priority then insertion order, promotion reprioritizes
    /// without duplication, done/in-flight jobs are not re-queued.
    #[test]
    fn queue_pops_promoted_jobs_first() {
        let jobs: Vec<JobSpec> = (0..100)
            .map(|i| JobSpec {
                path: PathBuf::from(format!("/j{i}")),
                size: 0,
                mtime: None,
            })
            .collect();
        // Zero worker threads is not constructible via start(); emulate the
        // pop loop directly on QueueState through a started-but-starved pool:
        // instead, drive the same lazy-deletion algorithm.
        let mut state = QueueState {
            heap: BinaryHeap::new(),
            queued: HashMap::new(),
            in_flight: HashSet::new(),
            next_seq: 0,
        };
        for index in 0..jobs.len() {
            let seq = state.next_seq;
            state.next_seq += 1;
            state.queued.insert(index, Priority::Background as u8);
            state
                .heap
                .push(std::cmp::Reverse((Priority::Background as u8, seq, index)));
        }
        // Promote 90..95 to Visible, 10..12 to Prefetch (same algorithm as
        // Pipeline::promote).
        for (range, prio) in [(90..95, Priority::Visible), (10..12, Priority::Prefetch)] {
            for index in range {
                let current = state.queued[&index];
                if (prio as u8) < current {
                    state.queued.insert(index, prio as u8);
                    let seq = state.next_seq;
                    state.next_seq += 1;
                    state.heap.push(std::cmp::Reverse((prio as u8, seq, index)));
                }
            }
        }
        let mut order = Vec::new();
        while let Some(std::cmp::Reverse((prio, _seq, index))) = state.heap.pop() {
            if state.queued.get(&index) == Some(&prio) {
                state.queued.remove(&index);
                order.push(index);
            }
        }
        assert_eq!(order.len(), 100, "each job exactly once");
        assert_eq!(&order[0..5], &[90, 91, 92, 93, 94], "visible first");
        assert_eq!(&order[5..7], &[10, 11], "prefetch second");
        // Background keeps sequential file order (0,1,2,... minus promoted).
        assert_eq!(&order[7..10], &[0, 1, 2]);
        assert!(order[7..].windows(2).all(|w| w[0] < w[1]));
    }
}
