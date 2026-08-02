//! The texture kitchen: every pixels→texture conversion, off the UI thread
//! (01-architecture.md § Threading model; user decision 2026-08-02: "no
//! decoding should be done on the UI thread").
//!
//! The UI thread used to decode thumbnail JPEGs (~32 per refresh), copy
//! 149 MB of full-res RGB into slint buffers, and downscale full-res to
//! mid — bounded per refresh, but the bounds only capped the stall (one
//! 23 ms refresh measured at 1:1 walking; ~0.93 s of UI-thread decoding
//! over a 5k import, 2026-07-27 investigation). This worker owns all of
//! it. The UI thread's only remaining texture duty is wrapping a finished
//! [`slint::SharedPixelBuffer`] into a [`slint::Image`] — O(1), because
//! the buffer is atomically refcounted; `Image` itself is not `Send`,
//! which is exactly why the WRAP is the one step that must stay put.
//!
//! Latency: a finished texture does not wait for the 33 ms pump — the
//! worker nudges the event loop (`notify`, wired to
//! `invoke_from_event_loop` → a window callback), so adoption happens as
//! soon as the UI thread is idle. The spec's accepted one-tick cost is
//! the worst case, not the design point.
//!
//! Priority (pop order): Full > Wrap > Thumb > Mid. The full-res buffer
//! fill is the sharpness-on-stop contract's tail (~300 ms budget,
//! ui-grid.md), so it never queues behind a page of thumbnails; Wrap (the
//! engine's own mid-rung textures) feeds the transit hold, so it beats
//! thumbs; thumbs beat Mid downscales because a placeholder is worse than
//! a soft cell. Staleness: Full requests dedupe per index and are popped
//! LATEST-FIRST, so the focused frame cooks soonest without cancelling a
//! ring neighbour's queued fill (replace-latest starved one of two frames
//! whose events shared a pump drain); Mid requests are culled to the
//! visible set on every
//! submission wave; Thumb and Wrap jobs are never culled — their sources
//! were MOVED or cheaply cloned on submit, and a landed texture for a
//! scrolled-away cell is still adopted (paid-for work stays paid for,
//! the pruned-and-revisited rule).
//!
//! Sessions: `retarget()` bumps a generation and empties the queue; late
//! `Done`s from the previous session carry the old generation and are
//! dropped at drain. Indexes from a dead session must never touch the new
//! session's texture maps.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use fastcull_core::loupe::FullImage;

/// Work for the kitchen. Each variant carries everything the conversion
/// needs, so the worker never touches app state.
pub enum Job {
    /// Decode an encoded thumbnail into a texture buffer.
    Thumb { index: usize, jpeg: Vec<u8> },
    /// Fill a full-size texture buffer from decoded RGB (the 149 MB copy).
    /// Terminal small files never come this way — they are `Wrap` jobs.
    Full { index: usize, image: FullImage },
    /// Downscale full-res to the mid rung and fill its buffer.
    Mid { index: usize, image: FullImage },
    /// Copy a decoded image at its NATIVE size (no downscale): the loupe
    /// engine's own mid-rung events, and terminal small files whose native
    /// size IS the top rung (issue #8 — downscaling those would lower the
    /// zoom ceiling). Deduped per index, never replaced: a transit hold
    /// produces one of these per ring member and every one matters.
    Wrap {
        index: usize,
        image: FullImage,
        terminal: bool,
    },
}

/// A finished texture buffer, ready for the O(1) UI-thread wrap.
pub enum Done {
    Thumb {
        index: usize,
        buf: slint::SharedPixelBuffer<slint::Rgb8Pixel>,
    },
    Full {
        index: usize,
        buf: slint::SharedPixelBuffer<slint::Rgb8Pixel>,
    },
    Mid {
        index: usize,
        buf: slint::SharedPixelBuffer<slint::Rgb8Pixel>,
        /// Long edge of the SOURCE the mid was cooked from, for
        /// `ViewAssets::note_held` (the 25% ladder bookkeeping).
        held_long: u32,
    },
    Wrap {
        index: usize,
        buf: slint::SharedPixelBuffer<slint::Rgb8Pixel>,
        terminal: bool,
    },
}

/// Which kind of work an index has pending — queue AND in-flight, because
/// submitters drop their source bytes/handles on submit and must not
/// resubmit while the worker is mid-cook.
#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Thumb,
    Full,
    Mid,
    Wrap,
}

fn kind_of(job: &Job) -> (Kind, usize) {
    match job {
        Job::Thumb { index, .. } => (Kind::Thumb, *index),
        Job::Full { index, .. } => (Kind::Full, *index),
        Job::Mid { index, .. } => (Kind::Mid, *index),
        Job::Wrap { index, .. } => (Kind::Wrap, *index),
    }
}

struct Shared {
    queue: Mutex<Vec<(u64, Job)>>,
    /// What the worker is cooking right now (generation, kind, index).
    in_flight: Mutex<Option<(u64, Kind, usize)>>,
    done: Mutex<Vec<(u64, Done)>>,
    wake: Condvar,
    shutdown: AtomicBool,
    generation: AtomicU64,
    /// Nudges the UI event loop after a completion (Send closure wired to
    /// `slint::invoke_from_event_loop` by the constructor's caller).
    notify: Box<dyn Fn() + Send + Sync>,
}

/// Handle; dropping joins the worker.
pub struct Kitchen {
    shared: Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Kitchen {
    pub fn start(notify: Box<dyn Fn() + Send + Sync>) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Vec::new()),
            in_flight: Mutex::new(None),
            done: Mutex::new(Vec::new()),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            notify,
        });
        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || worker(&shared))
        };
        Self {
            shared,
            worker: Some(worker),
        }
    }

    fn pending(&self, kind: Kind, index: usize) -> bool {
        let generation = self.shared.generation.load(Ordering::SeqCst);
        if *lock(&self.shared.in_flight) == Some((generation, kind, index)) {
            return true;
        }
        lock(&self.shared.queue)
            .iter()
            .any(|(g, j)| *g == generation && kind_of(j) == (kind, index))
    }

    /// Queue a thumbnail decode unless one is already pending for `index`.
    pub fn submit_thumb(&self, index: usize, jpeg: Vec<u8>) {
        if self.pending(Kind::Thumb, index) {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        lock(&self.shared.queue).push((generation, Job::Thumb { index, jpeg }));
        self.shared.wake.notify_all();
    }

    /// Queue a native-size copy (engine mid rung / terminal small file).
    pub fn submit_wrap(&self, index: usize, image: FullImage, terminal: bool) {
        if self.pending(Kind::Wrap, index) {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        lock(&self.shared.queue).push((
            generation,
            Job::Wrap {
                index,
                image,
                terminal,
            },
        ));
        self.shared.wake.notify_all();
    }

    /// Queue the full-res buffer fill, deduped per index. Deliberately NOT
    /// replace-latest: an earlier design cancelled any queued Full when a
    /// new one arrived, and a 2-file --start-11 session delivers BOTH ring
    /// members' full-res events in one pump drain — the second submission
    /// cancelled the first, and the warm-hit recovery could ping-pong the
    /// same way under an alternating cursor, leaving one frame's texture
    /// starved forever (flaky 60 s shutter refusals in the screenshot
    /// suite). The latest-first pop order already gets the focused frame
    /// cooked soonest; a superseded fill costs one wasted 149 MB copy that
    /// still lands usefully in the texture cache.
    pub fn submit_full(&self, index: usize, image: FullImage) {
        if self.pending(Kind::Full, index) {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        lock(&self.shared.queue).push((generation, Job::Full { index, image }));
        self.shared.wake.notify_all();
    }

    /// Queue a mid downscale unless one is pending for `index`.
    pub fn submit_mid(&self, index: usize, image: FullImage) {
        if self.pending(Kind::Mid, index) {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        lock(&self.shared.queue).push((generation, Job::Mid { index, image }));
        self.shared.wake.notify_all();
    }

    /// Drop queued MID jobs for cells no longer visible (spec: prep
    /// requests for scrolled-past cells are culled at submission waves).
    pub fn cull_mids(&self, visible: &[usize]) {
        let mut q = lock(&self.shared.queue);
        q.retain(|(_, j)| match j {
            Job::Mid { index, .. } => visible.contains(index),
            _ => true,
        });
    }

    /// New session: bump the generation, drop every queued job and every
    /// undrained completion. Late `Done`s from the worker's current flight
    /// carry the old generation and die at drain.
    pub fn retarget(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        lock(&self.shared.queue).clear();
        lock(&self.shared.done).clear();
    }

    /// Everything finished since the last drain, current session only.
    pub fn drain(&self) -> Vec<Done> {
        let generation = self.shared.generation.load(Ordering::SeqCst);
        lock(&self.shared.done)
            .drain(..)
            .filter(|(g, _)| *g == generation)
            .map(|(_, d)| d)
            .collect()
    }
}

impl Drop for Kitchen {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        if let Some(w) = self.worker.take() {
            w.join().ok();
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn worker(shared: &Shared) {
    loop {
        let (generation, job) = {
            let mut q = lock(&shared.queue);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                // Priority pop: Full (the sharpness-on-stop tail, latest
                // wins) > Wrap (the transit hold's mid swaps) > Thumb
                // (oldest first — visibility order) > Mid.
                if let Some(pos) = q
                    .iter()
                    .rposition(|(_, j)| matches!(j, Job::Full { .. }))
                    .or_else(|| q.iter().position(|(_, j)| matches!(j, Job::Wrap { .. })))
                    .or_else(|| q.iter().position(|(_, j)| matches!(j, Job::Thumb { .. })))
                    .or_else(|| q.iter().position(|(_, j)| matches!(j, Job::Mid { .. })))
                {
                    break q.remove(pos);
                }
                q = shared
                    .wake
                    .wait(q)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        if std::env::var_os("FASTCULL_TRACE").is_some() {
            let (k, i) = kind_of(&job);
            eprintln!(
                "kitchen: cooking {:?} idx {i}",
                match k {
                    Kind::Thumb => "thumb",
                    Kind::Full => "full",
                    Kind::Mid => "mid",
                    Kind::Wrap => "wrap",
                }
            );
        }
        *lock(&shared.in_flight) = Some({
            let (k, i) = kind_of(&job);
            (generation, k, i)
        });
        let done = cook(job);
        *lock(&shared.in_flight) = None;
        if let Some(done) = done {
            lock(&shared.done).push((generation, done));
            (shared.notify)();
        }
    }
}

/// The actual pixel work. A failed thumb decode returns None — the cell
/// stays a placeholder, same as the old UI-side decode's silent skip (the
/// SQLite cache row was decodable when stored; a corrupt row heals on the
/// next session's re-extract).
fn cook(job: Job) -> Option<Done> {
    match job {
        Job::Thumb { index, jpeg } => {
            let options = zune_jpeg::zune_core::options::DecoderOptions::default()
                .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
            let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&jpeg, options);
            let pixels = decoder.decode().ok()?;
            let (w, h) = decoder.dimensions()?;
            let buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                &pixels, w as u32, h as u32,
            );
            Some(Done::Thumb { index, buf })
        }
        Job::Full { index, image } => {
            let buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                &image.rgb,
                image.width,
                image.height,
            );
            Some(Done::Full { index, buf })
        }
        Job::Mid { index, image } => {
            let (buf, held_long) = downscale_to_mid(&image)?;
            Some(Done::Mid {
                index,
                buf,
                held_long,
            })
        }
        Job::Wrap {
            index,
            image,
            terminal,
        } => {
            let buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
                &image.rgb,
                image.width,
                image.height,
            );
            Some(Done::Wrap {
                index,
                buf,
                terminal,
            })
        }
    }
}

/// Full-res → mid-rung buffer (the old UI-side `adopt_texture`, verbatim
/// math). Sources at or below mid size are copied as-is.
fn downscale_to_mid(
    image: &FullImage,
) -> Option<(slint::SharedPixelBuffer<slint::Rgb8Pixel>, u32)> {
    use fastcull_core::loupe::MID_RUNG_TARGET;
    let long = image.width.max(image.height);
    if long <= MID_RUNG_TARGET {
        let buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(
            &image.rgb,
            image.width,
            image.height,
        );
        return Some((buf, long));
    }
    let t = u64::from(MID_RUNG_TARGET);
    let (dst_w, dst_h) = if image.width >= image.height {
        (
            MID_RUNG_TARGET,
            (u64::from(image.height) * t / u64::from(image.width)).max(1) as u32,
        )
    } else {
        (
            (u64::from(image.width) * t / u64::from(image.height)).max(1) as u32,
            MID_RUNG_TARGET,
        )
    };
    // Borrowed source: no 150 MB clone of the full-res pixels (validator,
    // carried over from the UI-side implementation).
    let src = fast_image_resize::images::ImageRef::new(
        image.width,
        image.height,
        image.rgb.as_ref(),
        fast_image_resize::PixelType::U8x3,
    )
    .ok()?;
    let mut dst =
        fast_image_resize::images::Image::new(dst_w, dst_h, fast_image_resize::PixelType::U8x3);
    fast_image_resize::Resizer::new()
        .resize(&src, &mut dst, None)
        .ok()?;
    let buf =
        slint::SharedPixelBuffer::<slint::Rgb8Pixel>::clone_from_slice(dst.buffer(), dst_w, dst_h);
    Some((buf, long))
}
