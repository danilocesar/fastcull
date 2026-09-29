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
//! Priority (pop order): Full > Wrap > Thumb > Mid, and ahead of all four a
//! Thumb for a frame inside the fill window. The full-res buffer
//! fill is the sharpness-on-stop contract's tail (~300 ms budget,
//! ui-grid.md) and the full-res ring's textures at 1:1, so it never queues
//! behind a page of thumbnails; Wrap (the engine's own mid-rung and
//! screen-rung textures, copied at native size) feeds the transit hold, so it
//! beats thumbs; thumbs beat Mid downscales because a placeholder is worse
//! than a soft cell (01-architecture.md, the kitchen). The one thumb that
//! goes first is the loupe's rescue rung: a frame's thumb is what the loupe
//! shows when the cursor reaches the frame before any loupe rung, and queued
//! behind the full-res ring's 149 MB fills it reached the screen after the
//! cursor did — a 1:1 hold then kept the previous frame's pixels for a few
//! frames, a stutter — while a 320 px thumb delays a fill by about a
//! millisecond (Manager ruling 2026-09-28, brief 008 step-6 review F1). The
//! window is the full-res texture window around the cursor that the fill
//! order carries ([`FillOrder`]); with the cursor out of the view there is
//! none, and no thumb goes first. Among Full fills the
//! order is core's `transit::next_fill` over the fill order the app sets at
//! every refresh at the loupe ([`FillOrder`]): the cursor's fill first, then
//! the nearest by view distance, ties toward the lean — the order the
//! engine decodes them in, so the member a tap reaches first is never cooked
//! last (brief 008; the kitchen popped the LATEST fill first while the ring
//! was ±2, which under a ring fifteen deep cooks the farthest member first).
//!
//! Staleness: Full requests dedupe per index and never cancel one another
//! (replace-latest starved one of two frames whose events shared a pump
//! drain); a queued Full for a frame outside the full-res texture window is
//! culled when the app sets the next fill order, so a revisit never waits
//! behind copies of frames already passed (brief 008, the redesign's G4);
//! Mid requests are culled to the visible set on every submission wave;
//! Thumb and Wrap jobs are never culled — their sources were MOVED or
//! cheaply cloned on submit, and a landed texture for a scrolled-away cell
//! is still adopted (paid-for work stays paid for, the pruned-and-revisited
//! rule). Wraps dedupe per index AND kind, so a queued mid wrap never
//! swallows the screen rung's.
//!
//! Sessions: `retarget()` bumps a generation and empties the queue; late
//! `Done`s from the previous session carry the old generation and are
//! dropped at drain. Indexes from a dead session must never touch the new
//! session's texture maps.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use fastcull_core::loupe::{FullImage, RingWindow, RungKind};

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
    /// engine's own mid-rung and screen-rung events, and terminal small
    /// files whose native size IS the top rung (issue #8 — downscaling those
    /// would lower the zoom ceiling). `kind` is the rung the engine decoded,
    /// which decides the texture ring it lands in. Deduped per index AND
    /// kind, never replaced: a transit hold produces one of these per ring
    /// member and every one matters, and a mid wrap queued for a frame must
    /// not swallow its screen rung's.
    Wrap {
        index: usize,
        image: FullImage,
        terminal: bool,
        kind: RungKind,
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
        kind: RungKind,
    },
}

/// Which kind of work an index has pending — queue AND in-flight, because
/// submitters drop their source bytes/handles on submit and must not
/// resubmit while the worker is mid-cook. A wrap is pending per rung kind:
/// the mid's and the screen rung's copies of one frame are two jobs.
#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Thumb,
    Full,
    Mid,
    Wrap(RungKind),
}

fn kind_of(job: &Job) -> (Kind, usize) {
    match job {
        Job::Thumb { index, .. } => (Kind::Thumb, *index),
        Job::Full { index, .. } => (Kind::Full, *index),
        Job::Mid { index, .. } => (Kind::Mid, *index),
        Job::Wrap { index, kind, .. } => (Kind::Wrap(*kind), *index),
    }
}

/// The order the kitchen cooks its queued full-res fills in, as the app
/// knows it at a refresh at the loupe: the cursor, the view order and the
/// full-res texture window (`LoupeEngine::texture_windows().full`, leaned by
/// the engine's travel latch). A snapshot, so the worker reads it without
/// touching app state; the view is shared, never copied per pop.
pub struct FillOrder {
    pub cursor: usize,
    pub view: Arc<[usize]>,
    pub window: RingWindow,
}

impl FillOrder {
    /// Is a full-res fill for `index` outside the window — a frame the
    /// cursor has left, whose copy no revisit should wait behind? Never the
    /// cursor's own; a frame out of the view is outside (no arrow reaches
    /// it); with the cursor itself out of the view there is no window to
    /// judge by, and nothing is culled — the next refresh brings one.
    fn outside(&self, index: usize) -> bool {
        if index == self.cursor {
            return false;
        }
        let pos_of = |id: usize| self.view.iter().position(|v| *v == id);
        match (pos_of(self.cursor), pos_of(index)) {
            (Some(cursor), Some(pos)) => !self.window.contains(cursor, pos),
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// The frames INSIDE the window, for [`pick`]'s rescue clause: the ids
    /// at the view positions the window reaches around the cursor
    /// (`RingWindow::span`), the cursor's own included — none when the
    /// cursor is out of the view, where there is no window to be inside.
    /// Never `!outside`: `outside` answers no for every frame once the
    /// cursor has left the view, which would put every queued thumb first.
    /// A slice of the snapshot, found once per pop, so a queued thumb is
    /// checked against the window's few frames rather than by a walk of the
    /// whole view per thumb — pops happen under the queue lock the UI
    /// thread's submissions take, and the order stays set in the grid, where
    /// a page queues a hundred thumbs.
    fn inside(&self) -> &[usize] {
        match self.view.iter().position(|v| *v == self.cursor) {
            Some(cursor) => &self.view[self.window.span(cursor, self.view.len())],
            None => &[],
        }
    }
}

struct Shared {
    queue: Mutex<Vec<(u64, Job)>>,
    /// What the worker is cooking right now (generation, kind, index).
    in_flight: Mutex<Option<(u64, Kind, usize)>>,
    done: Mutex<Vec<(u64, Done)>>,
    /// The latest fill order the app set ([`Kitchen::set_fill_order`]).
    /// A LEAF lock: taken alone, or nested INSIDE the queue lock (the
    /// worker reads it at every pop), and never held while any other lock
    /// is taken — so it cannot invert against the worker's queue-then-leaf
    /// order. `None` before the first order and after a session swap.
    fill_order: Mutex<Option<Arc<FillOrder>>>,
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
            fill_order: Mutex::new(None),
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

    /// Queue a native-size copy (the engine's mid and screen rungs, and
    /// terminal small files), deduped per index and `kind`.
    pub fn submit_wrap(&self, index: usize, image: FullImage, terminal: bool, kind: RungKind) {
        if self.pending(Kind::Wrap(kind), index) {
            return;
        }
        let generation = self.shared.generation.load(Ordering::SeqCst);
        lock(&self.shared.queue).push((
            generation,
            Job::Wrap {
                index,
                image,
                terminal,
                kind,
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
    /// suite). The fill order gets the cursor's frame cooked first
    /// ([`pick`]); a fill for a frame the cursor has left is culled by the
    /// next [`set_fill_order`](Self::set_fill_order).
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

    /// Set the order queued full-res fills are cooked in, and CULL the
    /// queued fills for frames outside its window (never the cursor's) —
    /// 01-architecture.md's staleness rule for the kitchen: a queued Full
    /// fill for a frame outside the full-res texture window is culled at the
    /// next submission wave, which is the app's next refresh at the loupe.
    /// Returns the culled indexes in queue order: each fill that never
    /// happens ends its decode's time-to-screen measurement unmeasured, which
    /// the caller reports to the engine (`LoupeEngine::note_dropped`; Manager
    /// ruling Q-K) — `#[must_use]` so a caller that drops the list is a
    /// `-D warnings` error rather than a silently censored switch rule.
    ///
    /// LOCK ORDER: the order is stored under its leaf lock alone, which is
    /// released, and only THEN is the queue lock taken to cull — the two are
    /// never held together in that order, so this cannot invert against the
    /// worker, which reads the leaf inside its queue-lock section.
    #[must_use]
    pub fn set_fill_order(&self, order: FillOrder) -> Vec<usize> {
        let order = Arc::new(order);
        *lock(&self.shared.fill_order) = Some(Arc::clone(&order));
        let generation = self.shared.generation.load(Ordering::SeqCst);
        let mut culled = Vec::new();
        lock(&self.shared.queue).retain(|(g, job)| match job {
            Job::Full { index, .. } if *g == generation && order.outside(*index) => {
                culled.push(*index);
                false
            }
            _ => true,
        });
        self.shared.wake.notify_all();
        culled
    }

    /// New session: bump the generation, drop every queued job and every
    /// undrained completion. Late `Done`s from the worker's current flight
    /// carry the old generation and die at drain. The fill order goes too:
    /// it names the dead session's cursor and view.
    pub fn retarget(&self) {
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        // The leaf lock alone (see `Shared::fill_order`).
        *lock(&self.shared.fill_order) = None;
        let dropped_queued = {
            let mut q = lock(&self.shared.queue);
            let n = q.len();
            q.clear();
            n
        };
        let dropped_done = {
            let mut d = lock(&self.shared.done);
            let n = d.len();
            d.clear();
            n
        };
        // Evidence channel for the session-swap drive test (issue #34): a
        // dropped-queued count > 0 is the proof the swap really happened
        // MID-FLIGHT — without it the test cannot tell "the fence held"
        // from "there was nothing to fence" (the vacuous-test trap).
        if std::env::var_os("FASTCULL_TRACE").is_some() {
            eprintln!("kitchen: retarget dropped {dropped_queued} queued, {dropped_done} done");
        }
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

/// Which queued job to cook next: first a Thumb for a frame inside `order`'s
/// window ([`FillOrder::inside`]), oldest first — the loupe's rescue rung,
/// ahead of every Full fill, the cursor's own included, and so of every Wrap
/// (01-architecture.md, the kitchen; Manager ruling 2026-09-28, brief 008
/// step-6 review F1); then Full, in `order`'s order — core's
/// `transit::next_fill`, the cursor's fill first, then by view distance,
/// ties toward the lean — or first queued first before the app has set an
/// order; then Wrap (transit swaps, first queued first, both kinds) > Thumb
/// (oldest first — visibility order) > Mid. With no order set no thumb goes
/// first. Pure so the priority contract is unit-tested.
fn pick(q: &[(u64, Job)], order: Option<&FillOrder>) -> Option<usize> {
    if let Some(order) = order {
        let inside = order.inside();
        let rescue = q
            .iter()
            .position(|(_, j)| matches!(j, Job::Thumb { index, .. } if inside.contains(index)));
        if rescue.is_some() {
            return rescue;
        }
    }
    let fulls: Vec<(usize, usize)> = q
        .iter()
        .enumerate()
        .filter_map(|(slot, (_, j))| match j {
            Job::Full { index, .. } => Some((slot, *index)),
            _ => None,
        })
        .collect();
    let full = match order {
        Some(order) => {
            let queued: Vec<usize> = fulls.iter().map(|(_, index)| *index).collect();
            fastcull_core::transit::next_fill(&queued, order.cursor, &order.view, order.window)
                .map(|k| fulls[k].0)
        }
        None => fulls.first().map(|(slot, _)| *slot),
    };
    full.or_else(|| q.iter().position(|(_, j)| matches!(j, Job::Wrap { .. })))
        .or_else(|| q.iter().position(|(_, j)| matches!(j, Job::Thumb { .. })))
        .or_else(|| q.iter().position(|(_, j)| matches!(j, Job::Mid { .. })))
}

fn worker(shared: &Shared) {
    loop {
        let (generation, job) = {
            let mut q = lock(&shared.queue);
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                // Priority pop: a Thumb inside the fill window (the loupe's
                // rescue rung, which a 1:1 hold must find in hand) > Full
                // (the sharpness-on-stop tail and the full-res ring, in the
                // fill order) > Wrap (the transit hold's mid and screen-rung
                // swaps) > Thumb (oldest first — visibility order) > Mid.
                // The fill order is read HERE, inside the queue-lock section
                // — the queue lock, then the leaf, released at once — the one
                // nesting the leaf allows.
                let order = lock(&shared.fill_order).clone();
                if let Some(pos) = pick(&q, order.as_deref()) {
                    let picked = q.remove(pos);
                    // in_flight is written while the queue lock is still
                    // held, so `pending()` can never observe the gap
                    // between pop and cook (validator finding: a resubmit
                    // slipped through it and duplicated a 149 MB cook).
                    // Nested-lock safety: pending() takes the two locks
                    // SEQUENTIALLY, never both at once.
                    let (k, i) = kind_of(&picked.1);
                    *lock(&shared.in_flight) = Some((picked.0, k, i));
                    // Traced while the queue lock is STILL HELD, so a
                    // `cooking` line can never appear after a `retarget`
                    // line in stderr unless the pop really followed the
                    // retarget: retarget clears the queue under this same
                    // lock, and the swap test's no-cooking-after-retarget
                    // assertion depends on that ordering (validator F3 —
                    // printed after unlock, a descheduled worker could
                    // interleave the two lines and fail the test falsely).
                    // The cost, trace-only: a stderr reader that stops
                    // draining blocks this print, so the UI thread's next
                    // submit waits on the queue lock behind it (measured,
                    // brief 008 D2 diagnosis, E1b) — a harness must drain.
                    if std::env::var_os("FASTCULL_TRACE").is_some() {
                        eprintln!(
                            "kitchen: cooking {:?} idx {i}",
                            match k {
                                Kind::Thumb => "thumb",
                                Kind::Full => "full",
                                Kind::Mid => "mid",
                                Kind::Wrap(_) => "wrap",
                            }
                        );
                    }
                    break picked;
                }
                q = shared
                    .wake
                    .wait(q)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        // FASTCULL_KITCHEN_COOK_MS=N: hold every cook for N ms first — a
        // harness pacing knob (ui-grid.md debug facilities, issue #34; same
        // family as FASTCULL_MAX_READERS). The session-swap drive test needs
        // the queue to still be mid-flight at a SCRIPTED moment in both
        // build profiles; a release build otherwise drains a screenful of
        // thumbs in tens of milliseconds and the swap becomes timing
        // roulette. The job still flows queue → cook → done → drain
        // unchanged — the knob slows the real path, it does not fork it.
        static COOK_HOLD: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        let hold = *COOK_HOLD.get_or_init(|| {
            let ms = std::env::var("FASTCULL_KITCHEN_COOK_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if ms > 0 {
                // Said out loud, unconditionally: a leftover value in some
                // environment makes the whole app mysteriously slow, and a
                // knob that ships in release builds must be diagnosable
                // from a bug report's stderr (validator risk note).
                eprintln!("fastcull: FASTCULL_KITCHEN_COOK_MS={ms} — every texture cook is held");
            }
            ms
        });
        if hold > 0 {
            std::thread::sleep(std::time::Duration::from_millis(hold));
        }
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
            kind,
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
                kind,
            })
        }
    }
}

/// Full-res → mid-rung buffer (the old UI-side `adopt_texture`'s sizing
/// math). Sources at or below mid size are copied as-is.
///
/// The reported long edge is WHAT THE BUFFER HOLDS, not the source's —
/// `ViewAssets::note_held`'s contract compares the ×1.25 ladder against
/// the held rung, and reporting the 8640 source for a 1616 downscale
/// silently disabled upgrades (validator finding, 2026-08-02).
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
    Some((buf, dst_w.max(dst_h)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn img(w: u32, h: u32, seed: u8) -> FullImage {
        FullImage {
            rgb: Arc::new(
                (0..w as usize * h as usize * 3)
                    .map(|i| seed.wrapping_add(i as u8))
                    .collect(),
            ),
            width: w,
            height: h,
            kind: fastcull_core::loupe::RungKind::Full,
        }
    }

    /// A kitchen with NO worker thread: the queue can be inspected and
    /// asserted on without a cook racing the assertions. Returns the
    /// shared state too, since that is where the queue lives.
    fn paused() -> (Kitchen, Arc<Shared>) {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Vec::new()),
            in_flight: Mutex::new(None),
            done: Mutex::new(Vec::new()),
            fill_order: Mutex::new(None),
            wake: Condvar::new(),
            shutdown: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            notify: Box::new(|| {}),
        });
        let k = Kitchen {
            shared: Arc::clone(&shared),
            worker: None,
        };
        (k, shared)
    }

    /// A fill order over an identity view of 40 frames, leaning forward over
    /// the ring's 2 behind / 15 ahead, around `cursor`.
    fn order_at(cursor: usize) -> FillOrder {
        FillOrder {
            cursor,
            view: (0..40).collect(),
            window: RingWindow::leaning(2, 15, true),
        }
    }

    /// The priority contract: Full (in the fill order) > Wrap > Thumb
    /// (oldest) > Mid. This ordering is what keeps the sharpness-on-stop
    /// tail ahead of a page of thumbnails — pure function so it cannot rot
    /// untested. Amended by brief 008, which replaced "latest first" among
    /// the fills with the fill order (ui-grid.md, the kitchen box): the pair
    /// is read with an order whose cursor is 4. Amended again by its step-6
    /// review, F1: the thumbs are 30 and 31, outside that order's window
    /// (positions 2 to 19), where they were 2 and 3, inside it — the rescue
    /// clause puts a thumb inside the window ahead of every fill
    /// (`a_thumb_inside_the_fill_window_pops_before_any_full_fill`), and this
    /// test's promise is the order of the jobs the clause does not reach.
    #[test]
    fn pick_orders_full_wrap_thumb_mid() {
        let mut q: Vec<(u64, Job)> = vec![
            (
                0,
                Job::Mid {
                    index: 1,
                    image: img(1, 1, 0),
                },
            ),
            (
                0,
                Job::Thumb {
                    index: 30,
                    jpeg: vec![],
                },
            ),
            (
                0,
                Job::Thumb {
                    index: 31,
                    jpeg: vec![],
                },
            ),
            (
                0,
                Job::Full {
                    index: 4,
                    image: img(1, 1, 0),
                },
            ),
            (
                0,
                Job::Wrap {
                    index: 5,
                    image: img(1, 1, 0),
                    terminal: false,
                    kind: RungKind::Mid,
                },
            ),
            (
                0,
                Job::Full {
                    index: 6,
                    image: img(1, 1, 0),
                },
            ),
        ];
        // Full first, in the fill order: the cursor's (idx 4) before its
        // neighbour's (idx 6).
        let order = order_at(4);
        let p = pick(&q, Some(&order)).unwrap();
        assert!(matches!(q[p].1, Job::Full { index: 4, .. }));
        q.remove(p);
        let p = pick(&q, Some(&order)).unwrap();
        assert!(matches!(q[p].1, Job::Full { index: 6, .. }));
        q.remove(p);
        // Then Wrap, then thumbs OLDEST first, then Mid.
        let p = pick(&q, Some(&order)).unwrap();
        assert!(matches!(q[p].1, Job::Wrap { index: 5, .. }));
        q.remove(p);
        let p = pick(&q, Some(&order)).unwrap();
        assert!(matches!(q[p].1, Job::Thumb { index: 30, .. }));
        q.remove(p);
        let p = pick(&q, Some(&order)).unwrap();
        assert!(matches!(q[p].1, Job::Thumb { index: 31, .. }));
        q.remove(p);
        let p = pick(&q, Some(&order)).unwrap();
        assert!(matches!(q[p].1, Job::Mid { index: 1, .. }));
        q.remove(p);
        assert!(pick(&q, Some(&order)).is_none());
    }

    /// End-to-end through a live worker: a Wrap job produces a buffer with
    /// the source's exact dimensions and bytes.
    #[test]
    fn wrap_cooks_byte_identical() {
        let k = Kitchen::start(Box::new(|| {}));
        let source = img(3, 2, 7);
        k.submit_wrap(9, source.clone(), false, RungKind::Mid);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let done = k.drain();
            if let Some(Done::Wrap { index, buf, .. }) = done.into_iter().next() {
                assert_eq!(index, 9);
                assert_eq!((buf.width(), buf.height()), (3, 2));
                let bytes: &[u8] = buf.as_bytes();
                assert_eq!(bytes, source.rgb.as_slice());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "wrap never cooked");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A downscaled mid reports the long edge of what the BUFFER holds,
    /// never the source's (the ladder-upgrade check compares against the
    /// held rung — validator finding).
    #[test]
    fn mid_reports_held_long_not_source_long() {
        let k = Kitchen::start(Box::new(|| {}));
        // 3232x2154 source: above MID_RUNG_TARGET, downscales to 1616-long.
        k.submit_mid(4, img(3232, 2154, 3));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if let Some(Done::Mid { held_long, buf, .. }) = k.drain().into_iter().next() {
                assert_eq!(buf.width().max(buf.height()), held_long);
                assert!(held_long <= fastcull_core::loupe::MID_RUNG_TARGET);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "mid never cooked");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// retarget() orphans everything: queued jobs are dropped and even a
    /// completion racing the bump dies at drain (generation filter). A
    /// dead session's indexes must never reach the new session's maps.
    #[test]
    fn retarget_orphans_queued_and_racing_work() {
        let k = Kitchen::start(Box::new(|| {}));
        // Jobs big enough that cooking OUTLASTS the retarget below — QE
        // proved the original 16x16 wraps cooked before the fence was
        // tested, so deleting the generation filter stayed green (F1).
        for i in 0..24 {
            k.submit_full(i, img(2000, 1400, i as u8));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        k.retarget();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < deadline {
            assert!(
                k.drain().is_empty(),
                "a dead session's texture crossed the generation fence"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // And the new session works: fresh submissions still cook.
        k.submit_wrap(0, img(2, 2, 1), false, RungKind::Mid);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if !k.drain().is_empty() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "new generation dead");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Adoption is UNBUDGETED (persona condition): drain returns EVERY
    /// finished texture in one call — a future "optimization" rationing
    /// it would turn one-tick-later into a visible trickle-in, and QE
    /// proved the whole screenshot suite cannot see that mutation (F2).
    #[test]
    fn drain_returns_everything_at_once() {
        let (k, shared) = paused();
        for i in 0..40 {
            lock(&shared.done).push((
                0,
                Done::Thumb {
                    index: i,
                    buf: slint::SharedPixelBuffer::new(1, 1),
                },
            ));
        }
        assert_eq!(k.drain().len(), 40, "drain must never ration adoption");
        assert!(k.drain().is_empty());
    }

    /// Full jobs dedupe per index and NEVER cancel a neighbour. The
    /// original replace-latest design cancelled a ring member's queued
    /// fill when both full-res events shared a pump drain — the flaky
    /// 60 s shutter refusals — and QE proved no surviving test enforces
    /// the fixed contract (F3).
    #[test]
    fn full_jobs_coexist_per_index_and_dedupe() {
        let (k, shared) = paused();
        k.submit_full(0, img(1, 1, 0));
        k.submit_full(1, img(1, 1, 1));
        {
            let q = lock(&shared.queue);
            assert_eq!(q.len(), 2, "a second Full must not cancel the first");
            assert!(q
                .iter()
                .any(|(_, j)| matches!(j, Job::Full { index: 0, .. })));
            assert!(q
                .iter()
                .any(|(_, j)| matches!(j, Job::Full { index: 1, .. })));
        }
        // Same index again: deduped, not duplicated.
        k.submit_full(1, img(1, 1, 1));
        assert_eq!(lock(&shared.queue).len(), 2);
        // The cursor's fill pops first (amended by brief 008: this read
        // "latest-first pop still favours the newest Full").
        let q = lock(&shared.queue);
        let p = pick(&q, Some(&order_at(0))).unwrap();
        assert!(matches!(q[p].1, Job::Full { index: 0, .. }));
    }

    /// cull_mids drops only invisible MID jobs — Full/Wrap/Thumb are never
    /// culled (their sources were moved or the work is wanted regardless).
    #[test]
    fn cull_mids_is_mid_only_and_visibility_scoped() {
        // No worker racing us: fill the queue while holding it hostage is
        // not possible from outside, so use a paused shared directly.
        let (k, shared) = paused();
        k.submit_mid(1, img(1, 1, 0));
        k.submit_mid(2, img(1, 1, 0));
        k.submit_thumb(3, vec![1, 2, 3]);
        k.submit_wrap(4, img(1, 1, 0), false, RungKind::Mid);
        k.cull_mids(&[2]);
        let q = lock(&shared.queue);
        assert_eq!(q.len(), 3, "mid 1 culled; mid 2, thumb 3, wrap 4 stay");
        assert!(q
            .iter()
            .any(|(_, j)| matches!(j, Job::Mid { index: 2, .. })));
        assert!(q
            .iter()
            .any(|(_, j)| matches!(j, Job::Thumb { index: 3, .. })));
        assert!(q
            .iter()
            .any(|(_, j)| matches!(j, Job::Wrap { index: 4, .. })));
        drop(q);
        // Dedupe: resubmitting a queued index is a no-op.
        k.submit_mid(2, img(1, 1, 0));
        assert_eq!(lock(&shared.queue).len(), 3);
    }

    /// The kitchen drops stale full fills and keeps both wraps (ui-grid.md,
    /// the kitchen box): a mid wrap and a screen-rung wrap of one index both
    /// stay queued — a queued mid wrap must not swallow the rung's, which on
    /// a wide viewport is the texture the fit cell needs — while a second
    /// wrap of the same kind is deduped. Red when a wrap is keyed by its
    /// index alone.
    #[test]
    fn wrap_jobs_dedupe_per_kind_not_per_index() {
        let (k, shared) = paused();
        k.submit_wrap(5, img(1, 1, 0), false, RungKind::Mid);
        k.submit_wrap(5, img(1, 1, 0), false, RungKind::Screen);
        let kinds = |q: &[(u64, Job)]| -> Vec<RungKind> {
            q.iter()
                .filter_map(|(_, j)| match j {
                    Job::Wrap { index: 5, kind, .. } => Some(*kind),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            kinds(&lock(&shared.queue)),
            [RungKind::Mid, RungKind::Screen],
            "the screen rung's wrap was swallowed by the mid's"
        );
        k.submit_wrap(5, img(1, 1, 0), false, RungKind::Mid);
        assert_eq!(
            kinds(&lock(&shared.queue)),
            [RungKind::Mid, RungKind::Screen],
            "a second mid wrap of the same index is deduped"
        );
    }

    /// A queued full-res fill for a frame outside the full-res window is
    /// culled when the app sets the next fill order, never one inside it
    /// (01-architecture.md, the kitchen; brief 008, the redesign's G4), and
    /// the culled indexes come back in queue order, for the engine's
    /// `note_dropped` (Manager ruling Q-K). Cursor 5, leaning forward over
    /// 2 behind / 15 ahead: the window is 3..=20, so both edges are pinned —
    /// 2 and 21 go, 3 and 20 stay, and the cursor's own stays. Red with no
    /// cull (the queue and the list), and with the cull done but an empty
    /// list returned (the list).
    #[test]
    fn full_fills_outside_the_window_are_culled() {
        let (k, shared) = paused();
        for index in [2, 3, 5, 20, 21] {
            k.submit_full(index, img(1, 1, 0));
        }
        let culled = k.set_fill_order(order_at(5));
        let left: Vec<usize> = lock(&shared.queue)
            .iter()
            .filter_map(|(_, j)| match j {
                Job::Full { index, .. } => Some(*index),
                _ => None,
            })
            .collect();
        assert_eq!(left, [3, 5, 20], "the fills inside the window are kept");
        assert_eq!(culled, [2, 21], "the culled fills, reported in queue order");
    }

    /// The kitchen pops its full fills in core's `transit::next_fill` order
    /// over the order the app set (ui-grid.md, the kitchen box): queued 7, 3,
    /// 5, 6 with the cursor on 5 and the lean forward — the cursor's first,
    /// then 6, then 7 before 3 (equal distance, the travel direction first),
    /// whatever order they were queued in. Read as the worker reads it,
    /// through the kitchen's own stored order. Red with the latest-first pop
    /// restored.
    #[test]
    fn the_kitchen_pops_full_fills_in_next_fill_order() {
        let (k, shared) = paused();
        for index in [7, 3, 5, 6] {
            k.submit_full(index, img(1, 1, 0));
        }
        assert!(
            k.set_fill_order(order_at(5)).is_empty(),
            "the premise: every fill is inside the window"
        );
        let order = lock(&shared.fill_order).clone();
        let mut q = std::mem::take(&mut *lock(&shared.queue));
        let mut popped = Vec::new();
        while let Some(slot) = pick(&q, order.as_deref()) {
            if let (_, Job::Full { index, .. }) = q.remove(slot) {
                popped.push(index);
            }
        }
        assert_eq!(popped, [5, 6, 7, 3]);
    }

    /// The rescue clause (ui-grid.md, "A thumb inside the fill window pops
    /// before any full fill"; 01-architecture.md, the kitchen; Manager ruling
    /// 2026-09-28, brief 008 step-6 review F1). Cursor 10, leaning forward
    /// over 2 behind / 15 ahead, so the window is positions 8 to 25. Queued
    /// oldest first: thumbs just past both edges (7, 26) and one for a frame
    /// out of the view (99), the cursor's fill and a neighbour's (10, 12), a
    /// wrap (11), then thumbs AT both edges (25, 8). Popped as the worker pops
    /// them, through the kitchen's own stored order: the two inside first,
    /// oldest first, ahead of every fill — the cursor's own included — and
    /// the wrap; the other three keep their places behind them. With the
    /// order's cursor out of its view there is no window to be inside, and
    /// with no order set there is no clause: no thumb goes first. Red with
    /// the clause removed and with it applied to every thumb (the first
    /// queue), and with "inside" read as "not outside" (`!FillOrder::outside`,
    /// which says yes for every frame once the cursor has left the view: the
    /// cursor-away row).
    #[test]
    fn a_thumb_inside_the_fill_window_pops_before_any_full_fill() {
        #[derive(Debug, PartialEq)]
        enum Popped {
            Thumb(usize),
            Full(usize),
            Wrap(usize),
        }
        use Popped::{Full, Thumb, Wrap};
        let drain = |mut q: Vec<(u64, Job)>, order: Option<&FillOrder>| {
            let mut popped = Vec::new();
            while let Some(slot) = pick(&q, order) {
                popped.push(match q.remove(slot).1 {
                    Job::Thumb { index, .. } => Thumb(index),
                    Job::Full { index, .. } => Full(index),
                    Job::Wrap { index, .. } => Wrap(index),
                    Job::Mid { .. } => panic!("no mid was queued"),
                });
            }
            popped
        };
        let (k, shared) = paused();
        for index in [7, 26, 99] {
            k.submit_thumb(index, vec![]);
        }
        k.submit_full(10, img(1, 1, 0));
        k.submit_full(12, img(1, 1, 0));
        k.submit_wrap(11, img(1, 1, 0), false, RungKind::Screen);
        for index in [25, 8] {
            k.submit_thumb(index, vec![]);
        }
        assert!(
            k.set_fill_order(order_at(10)).is_empty(),
            "the premise: both fills are inside the window"
        );
        let order = lock(&shared.fill_order).clone();
        let q = std::mem::take(&mut *lock(&shared.queue));
        assert_eq!(
            drain(q, order.as_deref()),
            [
                Thumb(25),
                Thumb(8),
                Full(10),
                Full(12),
                Wrap(11),
                Thumb(7),
                Thumb(26),
                Thumb(99)
            ],
            "the thumbs inside the window, both edges, first; the rest in the \
             order the clause does not touch"
        );
        let queue = || -> Vec<(u64, Job)> {
            vec![
                (
                    0,
                    Job::Thumb {
                        index: 10,
                        jpeg: vec![],
                    },
                ),
                (
                    0,
                    Job::Full {
                        index: 12,
                        image: img(1, 1, 0),
                    },
                ),
                (
                    0,
                    Job::Wrap {
                        index: 11,
                        image: img(1, 1, 0),
                        terminal: false,
                        kind: RungKind::Screen,
                    },
                ),
                (
                    0,
                    Job::Thumb {
                        index: 20,
                        jpeg: vec![],
                    },
                ),
            ]
        };
        let away = FillOrder {
            cursor: 99,
            view: (0..40).collect(),
            window: RingWindow::leaning(2, 15, true),
        };
        assert_eq!(
            drain(queue(), Some(&away)),
            [Full(12), Wrap(11), Thumb(10), Thumb(20)],
            "the order's cursor out of its view: no window, so no thumb goes first"
        );
        assert_eq!(
            drain(queue(), None),
            [Full(12), Wrap(11), Thumb(10), Thumb(20)],
            "no order set: no thumb goes first"
        );
    }

    /// A session swap forgets the fill order: it names the dead session's
    /// cursor and view, and the new session's first fills must not be cooked
    /// by it. Red with `retarget`'s reset removed.
    #[test]
    fn retarget_forgets_the_fill_order() {
        let (k, shared) = paused();
        let _ = k.set_fill_order(order_at(5));
        assert!(
            lock(&shared.fill_order).is_some(),
            "the premise: an order was set"
        );
        k.retarget();
        assert!(
            lock(&shared.fill_order).is_none(),
            "a swap kept the dead session's fill order"
        );
    }
}
